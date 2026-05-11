use anyhow::Result;
use clap::{Parser, Subcommand};
use cstat_core::Database;
use cstat_ingest::current_natstat_season;
use cstat_ingest::ingest::{BootstrapOptions, SeasonIngester};
use cstat_ingest::{NatStatClient, TorkvikClient};
use tracing::info;

/// CLI default for `--year`. Resolved at parse time so the binary picks up
/// the current NCAA basketball season automatically as the calendar rolls
/// over (Nov+ → next year).
fn default_season() -> i32 {
    current_natstat_season()
}

#[derive(Parser)]
#[command(name = "cstat-ingest", about = "NatStat data ingestion CLI for cstat")]
struct Cli {
    /// Clear all cached API responses before running (forces fresh fetches).
    #[arg(long, global = true)]
    no_cache: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bootstrap a season end-to-end: NatStat ingest + Torvik + compute.
    /// One command for "add a new season". Use `--no-torvik` / `--no-compute`
    /// to skip parts (e.g. when you'll batch them yourself).
    Season {
        /// Season year (e.g. 2026 for the 2025-26 season). Defaults to the
        /// current NCAA basketball season.
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Skip the Barttorvik player-stats step.
        #[arg(long)]
        no_torvik: bool,

        /// Skip the compute pipeline.
        #[arg(long)]
        no_compute: bool,
    },

    /// Ingest only teams for a season.
    Teams {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest only players for a season.
    Players {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest everything for a single team: roster, details (TCR), and player performances.
    Team {
        /// Team code (e.g. DUKE, UNC, KU)
        code: String,

        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest games (and optionally box scores) for a date range.
    Games {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        to: Option<String>,
    },

    /// Ingest player performances (box scores) for a date range.
    Perfs {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        to: Option<String>,
    },

    /// Incremental update: fetch recent games and performances, then run
    /// compute so derived stats stay fresh. Use `--no-compute` to skip the
    /// post-step (e.g. when batching several updates).
    Update {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        from: String,

        /// End date (YYYY-MM-DD)
        #[arg(long)]
        to: String,

        /// Skip the compute pipeline at the end.
        #[arg(long)]
        no_compute: bool,
    },

    /// Ingest ELO ratings from /elo endpoint.
    Elo {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Ingest per-game forecasts (ELO snapshots, win exp, betting lines) from /forecasts.
    Forecasts {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Run compute pipeline: derive season stats, schedules, percentiles from raw data.
    Compute {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,
    },

    /// Show rate limit status.
    Status,

    /// Clean up expired cache entries.
    CleanCache,

    /// Ingest Barttorvik player season stats (advanced metrics, shot zones, bio).
    Torvik {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Also backfill missing rebounds from Torvik game-level data.
        #[arg(long)]
        rebounds: bool,
    },

    /// Compare CamPom composites in torvik_player_stats against an external reference CSV.
    /// Pass condition: max abs diff < 0.01 across every CamPom intermediate and final.
    CampomParity {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Path to the baseline CSV. Defaults to docs/campom_2026_baseline.csv.
        #[arg(long, default_value = "docs/campom_2026_baseline.csv")]
        baseline: std::path::PathBuf,
    },

    /// Ingest the 247Sports transfer portal for a class year (matches 247's `year=`
    /// query param, i.e. the spring portal-cycle calendar year, NOT cstat-season).
    /// Requires TFS_247_JWT env var (capture from DevTools; ~6 hour expiry).
    Transfers {
        #[arg(short, long, default_value_t = default_season())]
        year: i32,

        /// Skip pages whose lastUpdated predates our DB cursor.
        /// Default is a full refresh (every page touched).
        #[arg(long)]
        incremental: bool,

        /// Load from a local snapshot file instead of hitting the live API.
        /// Useful for the initial seed and for reproducible local dev.
        #[arg(long)]
        bootstrap_from: Option<std::path::PathBuf>,

        /// After ingest, resolve cstat_player_id joins by name + source-team match.
        #[arg(long, default_value_t = true)]
        resolve_players: bool,
    },

    /// Fetch a raw API endpoint and dump the JSON (for exploration).
    Explore {
        /// Endpoint (e.g., "teams", "players", "playerperfs")
        endpoint: String,

        /// Range params (e.g., "2026,DUKE")
        #[arg(short, long)]
        range: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cstat_ingest=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let api_key = std::env::var("NATSTAT_API_KEY").expect("NATSTAT_API_KEY must be set");

    let db = Database::connect(&database_url).await?;
    db.migrate().await?;
    info!("connected to database");

    let client = NatStatClient::new(db.pool.clone(), api_key, 1500);

    if cli.no_cache {
        let cleared = client.clear_all_cache().await?;
        info!(cleared, "cleared API cache");
        println!("Cleared {cleared} cached API responses");
    }

    match cli.command {
        Commands::Season {
            year,
            no_torvik,
            no_compute,
        } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester
                .bootstrap_season(BootstrapOptions {
                    torvik: !no_torvik,
                    compute: !no_compute,
                })
                .await?;
            print!("{report}");
        }

        Commands::Teams { year } => {
            let count = cstat_ingest::ingest::teams::ingest_teams(&client, &db.pool, year).await?;
            println!("Ingested {count} teams for {year}");
        }

        Commands::Players { year } => {
            let count =
                cstat_ingest::ingest::players::ingest_all_rosters(&client, &db.pool, year).await?;
            println!("Ingested {count} players for {year}");
        }

        Commands::Team { code, year } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_team(&code).await?;
            print!("{report}");
        }

        Commands::Games { year, from, to } => {
            let count = match (from, to) {
                (Some(f), Some(t)) => {
                    cstat_ingest::ingest::games::ingest_games_by_date_range(
                        &client, &db.pool, year, &f, &t,
                    )
                    .await?
                }
                _ => cstat_ingest::ingest::games::ingest_games(&client, &db.pool, year).await?,
            };
            println!("Ingested {count} games for {year}");
        }

        Commands::Perfs { year, from, to } => {
            let count = match (from, to) {
                (Some(f), Some(t)) => {
                    cstat_ingest::ingest::games::ingest_player_performances_by_date_range(
                        &client, &db.pool, year, &f, &t,
                    )
                    .await?
                }
                _ => {
                    cstat_ingest::ingest::games::ingest_player_performances(&client, &db.pool, year)
                        .await?
                }
            };
            println!("Ingested {count} player performances for {year}");
        }

        Commands::Elo { year } => {
            let count =
                cstat_ingest::ingest::elo::ingest_elo_ratings(&client, &db.pool, year).await?;
            println!("Ingested {count} ELO ratings for {year}");
        }

        Commands::Forecasts { year } => {
            let count =
                cstat_ingest::ingest::elo::ingest_game_forecasts(&client, &db.pool, year).await?;
            println!("Ingested {count} game forecasts for {year}");
        }

        Commands::Compute { year } => {
            let report = cstat_core::compute::compute_all(&db.pool, year).await?;
            println!("{report}");
        }

        Commands::Update {
            year,
            from,
            to,
            no_compute,
        } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_recent(&from, &to, !no_compute).await?;
            print!("{report}");
        }

        Commands::Status => {
            let remaining = client.rate_limit_remaining().await;
            println!("Local rate limit tokens: {remaining}/1500");
        }

        Commands::CleanCache => {
            let removed = client.cleanup_cache().await?;
            println!("Removed {removed} expired cache entries");
        }

        Commands::Torvik { year, rebounds } => {
            let torvik = TorkvikClient::new();
            let (upserted, matched) =
                cstat_ingest::ingest::torvik::ingest_torvik_player_stats(&torvik, &db.pool, year)
                    .await?;
            println!(
                "Torvik player stats: {upserted} upserted, {matched} matched to cstat players"
            );

            if rebounds {
                let updated = cstat_ingest::ingest::torvik::backfill_rebounds_from_torvik(
                    &torvik, &db.pool, year,
                )
                .await?;
                println!("Rebound backfill: {updated} game rows updated");
            }
        }

        Commands::CampomParity { year, baseline } => {
            let report = cstat_ingest::campom_parity::run(&db.pool, year, &baseline).await?;
            report.print();
            if !report.passed() {
                std::process::exit(1);
            }
        }

        Commands::Transfers {
            year,
            incremental,
            bootstrap_from,
            resolve_players,
        } => {
            let report = if let Some(path) = bootstrap_from {
                info!("bootstrapping transfers from {}", path.display());
                cstat_ingest::ingest::transfers::bootstrap_from_snapshot(&db.pool, year, &path)
                    .await?
            } else {
                let tfs = cstat_ingest::TfsClient::from_env()?;
                cstat_ingest::ingest::transfers::ingest_live(&tfs, &db.pool, year, incremental)
                    .await?
            };
            println!(
                "transfers {}: {} upserted across {} page(s)",
                report.year, report.upserts, report.total_pages
            );

            if resolve_players {
                let n =
                    cstat_ingest::ingest::transfers::resolve_cstat_joins(&db.pool, year).await?;
                println!("cstat_player_id resolved on {n} row(s)");
            }
        }

        Commands::Explore { endpoint, range } => {
            let response = client.get(&endpoint, range.as_deref(), None, None).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }

    Ok(())
}
