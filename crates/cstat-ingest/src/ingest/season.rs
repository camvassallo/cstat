use crate::NatStatClient;
use crate::client::NatStatError;
use sqlx::PgPool;
use tracing::info;

/// Orchestrates full-season data ingestion.
pub struct SeasonIngester<'a> {
    client: &'a NatStatClient,
    pool: &'a PgPool,
    season: i32,
}

impl<'a> SeasonIngester<'a> {
    pub fn new(client: &'a NatStatClient, pool: &'a PgPool, season: i32) -> Self {
        Self {
            client,
            pool,
            season,
        }
    }

    /// Run full season ingestion in the correct order:
    /// 1. Teams (reference data, needed for foreign keys)
    /// 2. Players (reference data, needs team_id)
    /// 3. Games (results, needs team IDs)
    /// 4. Player performances (box scores, needs player_id + game_id)
    /// 5. Team details (ELO, advanced stats)
    pub async fn ingest_full_season(&self) -> Result<IngestReport, NatStatError> {
        let mut report = IngestReport::default();

        info!(season = self.season, "starting full season ingestion");

        // Step 1: Teams
        info!("step 1/5: ingesting teams");
        report.teams = super::teams::ingest_teams(self.client, self.pool, self.season).await?;

        // Step 2: Players
        info!("step 2/5: ingesting players");
        report.players =
            super::players::ingest_players(self.client, self.pool, self.season).await?;

        // Step 3: Games
        info!("step 3/5: ingesting games");
        report.games = super::games::ingest_games(self.client, self.pool, self.season).await?;

        // Step 4: Player performances
        info!("step 4/5: ingesting player performances");
        report.player_performances =
            super::games::ingest_player_performances(self.client, self.pool, self.season).await?;

        // Step 5: Team details (ELO, etc.)
        info!("step 5/5: ingesting team details");
        report.team_details =
            super::teams::ingest_team_details(self.client, self.pool, self.season).await?;

        info!(
            season = self.season,
            teams = report.teams,
            players = report.players,
            games = report.games,
            player_performances = report.player_performances,
            team_details = report.team_details,
            "season ingestion complete"
        );

        Ok(report)
    }

    /// Incremental update: only fetch recent games and performances.
    pub async fn ingest_recent(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<IngestReport, NatStatError> {
        let mut report = IngestReport::default();

        info!(
            season = self.season,
            start_date, end_date, "starting incremental ingestion"
        );

        // Games for the date range
        report.games = super::games::ingest_games_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        // Player performances for the date range
        report.player_performances = super::games::ingest_player_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        info!(
            season = self.season,
            games = report.games,
            player_performances = report.player_performances,
            "incremental ingestion complete"
        );

        Ok(report)
    }
}

/// Summary of an ingestion run.
#[derive(Debug, Default)]
pub struct IngestReport {
    pub teams: u64,
    pub players: u64,
    pub games: u64,
    pub player_performances: u64,
    pub team_details: u64,
}

impl std::fmt::Display for IngestReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ingested: {} teams, {} players, {} games, {} player performances, {} team details",
            self.teams, self.players, self.games, self.player_performances, self.team_details
        )
    }
}
