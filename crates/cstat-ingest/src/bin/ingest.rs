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

    /// Show rate limit status.
    Status,

    /// Clean up expired cache entries.
    CleanCache,
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
                cstat_ingest::ingest::players::ingest_players(&client, &db.pool, year).await?;
            println!("Ingested {count} players for {year}");
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
    }

    Ok(())
}
