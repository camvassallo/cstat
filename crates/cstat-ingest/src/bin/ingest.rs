use anyhow::Result;
use clap::{Parser, Subcommand};
use cstat_core::Database;
use cstat_ingest::NatStatClient;
use cstat_ingest::ingest::SeasonIngester;
use tracing::info;

#[derive(Parser)]
#[command(name = "cstat-ingest", about = "NatStat data ingestion CLI for cstat")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ingest a full season (teams, players, games, box scores, team details).
    Season {
        /// Season year (e.g., 2026 for 2025-2026 season)
        #[arg(short, long, default_value = "2026")]
        year: i32,
    },

    /// Ingest only teams for a season.
    Teams {
        #[arg(short, long, default_value = "2026")]
        year: i32,
    },

    /// Ingest only players for a season.
    Players {
        #[arg(short, long, default_value = "2026")]
        year: i32,
    },

    /// Ingest everything for a single team: roster, details (TCR), and player performances.
    Team {
        /// Team code (e.g., DUKE, UNC, KU)
        code: String,

        #[arg(short, long, default_value = "2026")]
        year: i32,
    },

    /// Ingest games (and optionally box scores) for a date range.
    Games {
        #[arg(short, long, default_value = "2026")]
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
        #[arg(short, long, default_value = "2026")]
        year: i32,

        /// Start date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD). If omitted, fetches full season.
        #[arg(long)]
        to: Option<String>,
    },

    /// Incremental update: fetch recent games and performances.
    Update {
        #[arg(short, long, default_value = "2026")]
        year: i32,

        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        from: String,

        /// End date (YYYY-MM-DD)
        #[arg(long)]
        to: String,
    },

    /// Run compute pipeline: derive season stats, schedules, percentiles from raw data.
    Compute {
        #[arg(short, long, default_value = "2026")]
        year: i32,
    },

    /// Show rate limit status.
    Status,

    /// Clean up expired cache entries.
    CleanCache,

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

    let client = NatStatClient::new(db.pool.clone(), api_key, 500);

    match cli.command {
        Commands::Season { year } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_full_season().await?;
            println!("{report}");
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
            let code = code.to_uppercase();
            println!("Ingesting full data for {code} ({year})...");

            // 1. Team roster (players with metadata)
            let roster =
                cstat_ingest::ingest::players::ingest_team_roster(&client, &db.pool, year, &code)
                    .await?;
            println!("  Roster: {roster} players");

            // 2. Team details (TCR, ELO, W/L)
            let team_row: Option<(uuid::Uuid,)> =
                sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                    .bind(&code)
                    .bind(year)
                    .fetch_optional(&db.pool)
                    .await?;
            if let Some((team_id,)) = team_row {
                cstat_ingest::ingest::teams::ingest_single_team_details(
                    &client, &db.pool, year, &team_id, &code,
                )
                .await?;
                println!("  Team details: OK");
            }

            // 3. Player performances (box scores)
            let perfs = cstat_ingest::ingest::games::ingest_player_performances_by_team(
                &client, &db.pool, year, &code,
            )
            .await?;
            println!("  Player performances: {perfs} box scores");

            // 4. Team performances (team-level box scores)
            let team_perfs = cstat_ingest::ingest::games::ingest_team_performances(
                &client, &db.pool, year, &code,
            )
            .await?;
            println!("  Team performances: {team_perfs} game stats");

            println!("Done! {code} fully ingested.");
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

        Commands::Compute { year } => {
            let report = cstat_core::compute::compute_all(&db.pool, year).await?;
            println!("{report}");
        }

        Commands::Update { year, from, to } => {
            let ingester = SeasonIngester::new(&client, &db.pool, year);
            let report = ingester.ingest_recent(&from, &to).await?;
            println!("{report}");
        }

        Commands::Status => {
            let remaining = client.rate_limit_remaining().await;
            println!("Local rate limit tokens: {remaining}/500");
        }

        Commands::CleanCache => {
            let removed = client.cleanup_cache().await?;
            println!("Removed {removed} expired cache entries");
        }

        Commands::Explore { endpoint, range } => {
            let response = client.get(&endpoint, range.as_deref(), None, None).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }

    Ok(())
}
