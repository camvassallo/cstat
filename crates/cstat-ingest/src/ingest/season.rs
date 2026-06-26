use crate::NatStatClient;
use crate::client::NatStatError;
use crate::team_id_by_code_and_season;
use crate::torvik::TorkvikClient;
use cstat_core::compute::{ComputeReport, compute_all};
use sqlx::PgPool;
use tracing::{info, warn};

/// Orchestrates full-season data ingestion.
pub struct SeasonIngester<'a> {
    client: &'a NatStatClient,
    pool: &'a PgPool,
    season: i32,
}

/// Knobs for `bootstrap_season` — by default it runs every step.
#[derive(Debug, Clone, Copy)]
pub struct BootstrapOptions {
    /// When true, fetch Barttorvik player season stats after the NatStat steps.
    pub torvik: bool,
    /// When true, run the cstat-core compute pipeline at the end so derived
    /// stats (four factors, percentiles, archetypes-input columns, …) are
    /// fresh.
    pub compute: bool,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            torvik: true,
            compute: true,
        }
    }
}

impl<'a> SeasonIngester<'a> {
    pub fn new(client: &'a NatStatClient, pool: &'a PgPool, season: i32) -> Self {
        Self {
            client,
            pool,
            season,
        }
    }

    /// Run the NatStat-only portion of season ingestion in dependency order:
    /// 1. Teams (reference data, needed for foreign keys)
    /// 2. Games (results, needs team IDs)
    /// 3. Player performances (box scores — also auto-creates player records)
    /// 4. Team details (TCR, record, conference)
    /// 5. Team performances (team-level box scores for four factors)
    /// 6. ELO ratings (real ratings from /elo endpoint)
    /// 7. Game forecasts (per-game ELO snapshots, win exp, betting lines from /forecasts)
    pub async fn ingest_full_season(&self) -> Result<IngestReport, NatStatError> {
        let mut report = IngestReport::default();

        info!(season = self.season, "starting full season ingestion");

        info!("step 1/7: ingesting teams");
        report.teams = super::teams::ingest_teams(self.client, self.pool, self.season).await?;

        info!("step 2/7: ingesting games");
        report.games = super::games::ingest_games(self.client, self.pool, self.season).await?;

        info!("step 3/7: ingesting player performances");
        report.player_performances =
            super::games::ingest_player_performances(self.client, self.pool, self.season).await?;

        info!("step 4/7: ingesting team details");
        report.team_details =
            super::teams::ingest_team_details(self.client, self.pool, self.season).await?;

        info!("step 5/7: ingesting team performances");
        report.team_performances =
            super::games::ingest_all_team_performances(self.client, self.pool, self.season).await?;

        info!("step 6/7: ingesting ELO ratings");
        report.elo_ratings =
            super::elo::ingest_elo_ratings(self.client, self.pool, self.season).await?;

        info!("step 7/7: ingesting game forecasts");
        report.game_forecasts =
            super::elo::ingest_game_forecasts(self.client, self.pool, self.season).await?;

        info!(
            season = self.season,
            teams = report.teams,
            games = report.games,
            player_performances = report.player_performances,
            team_details = report.team_details,
            team_performances = report.team_performances,
            elo_ratings = report.elo_ratings,
            game_forecasts = report.game_forecasts,
            "season ingestion complete"
        );

        Ok(report)
    }

    /// Bootstrap a brand-new season end-to-end: NatStat ingest, Barttorvik
    /// ingest, and the compute pipeline. This is the single command for
    /// "add a new season" — its output, plus running the archetype trainer,
    /// is everything needed before the new year shows up in the UI.
    pub async fn bootstrap_season(
        &self,
        opts: BootstrapOptions,
    ) -> Result<BootstrapReport, NatStatError> {
        let ingest = self.ingest_full_season().await?;

        let torvik = if opts.torvik {
            let torvik_client = TorkvikClient::new();
            // Torvik failures shouldn't kill a season bootstrap — NatStat
            // data is the load-bearing part and Torvik can be re-run later
            // with `cstat-ingest torvik --year YYYY`. Log and continue.
            match super::torvik::ingest_torvik_player_stats(&torvik_client, self.pool, self.season)
                .await
            {
                Ok((upserted, matched)) => Some(TorvikReport { upserted, matched }),
                Err(e) => {
                    warn!(season = self.season, error = %e, "Torvik ingest failed; continuing");
                    None
                }
            }
        } else {
            None
        };

        let compute = if opts.compute {
            Some(
                compute_all(self.pool, self.season)
                    .await
                    .map_err(NatStatError::Database)?,
            )
        } else {
            None
        };

        Ok(BootstrapReport {
            ingest,
            torvik,
            compute,
        })
    }

    /// Incremental update: refresh recent games and performances. Optionally
    /// re-runs the compute pipeline so derived stats stay in sync (default
    /// for the CLI; opt-out lets a caller batch several updates first).
    pub async fn ingest_recent(
        &self,
        start_date: &str,
        end_date: &str,
        run_compute: bool,
    ) -> Result<UpdateReport, NatStatError> {
        let mut ingest = IngestReport::default();

        info!(
            season = self.season,
            start_date, end_date, "starting incremental ingestion"
        );

        ingest.games = super::games::ingest_games_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        ingest.player_performances = super::games::ingest_player_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        // Team-level box scores must be ingested too — `team_game_stats` feeds
        // four-factors / AdjEM / W-L derivation. Omitting this is why games
        // brought in by the incremental path historically carried player box
        // scores but no team box scores (issue #148).
        ingest.team_performances = super::games::ingest_team_performances_by_date_range(
            self.client,
            self.pool,
            self.season,
            start_date,
            end_date,
        )
        .await?;

        info!(
            season = self.season,
            games = ingest.games,
            player_performances = ingest.player_performances,
            team_performances = ingest.team_performances,
            "incremental ingestion complete"
        );

        let compute = if run_compute {
            Some(
                compute_all(self.pool, self.season)
                    .await
                    .map_err(NatStatError::Database)?,
            )
        } else {
            None
        };

        Ok(UpdateReport { ingest, compute })
    }

    /// Ingest everything needed for a single team: roster (player metadata),
    /// team details (TCR/ELO/W-L), per-player box scores, and per-team box
    /// scores. Lives on `SeasonIngester` (rather than the bin) so the same
    /// orchestration is reachable from tests and from any other caller.
    pub async fn ingest_team(&self, team_code: &str) -> Result<TeamReport, NatStatError> {
        let code = team_code.to_uppercase();
        info!(season = self.season, code = %code, "ingesting full team data");

        let roster =
            super::players::ingest_team_roster(self.client, self.pool, self.season, &code).await?;

        let team_details = match team_id_by_code_and_season(self.pool, Some(&code), self.season)
            .await?
        {
            Some(team_id) => {
                super::teams::ingest_single_team_details(
                    self.client,
                    self.pool,
                    self.season,
                    &team_id,
                    &code,
                )
                .await?
            }
            None => {
                warn!(season = self.season, code = %code, "team not in DB; skipping team details");
                false
            }
        };

        let player_performances = super::games::ingest_player_performances_by_team(
            self.client,
            self.pool,
            self.season,
            &code,
        )
        .await?;

        let team_performances =
            super::games::ingest_team_performances(self.client, self.pool, self.season, &code)
                .await?;

        Ok(TeamReport {
            code,
            roster,
            team_details,
            player_performances,
            team_performances,
        })
    }
}

/// Summary of an ingestion run.
#[derive(Debug, Default)]
pub struct IngestReport {
    pub teams: u64,
    pub games: u64,
    pub player_performances: u64,
    pub team_details: u64,
    pub team_performances: u64,
    pub elo_ratings: u64,
    pub game_forecasts: u64,
}

impl std::fmt::Display for IngestReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ingested: {} teams, {} games, {} player perfs, {} team details, {} team perfs, {} ELO ratings, {} game forecasts",
            self.teams,
            self.games,
            self.player_performances,
            self.team_details,
            self.team_performances,
            self.elo_ratings,
            self.game_forecasts
        )
    }
}

#[derive(Debug, Default)]
pub struct TorvikReport {
    pub upserted: u64,
    pub matched: u64,
}

/// Aggregate report from `bootstrap_season`.
#[derive(Debug)]
pub struct BootstrapReport {
    pub ingest: IngestReport,
    pub torvik: Option<TorvikReport>,
    pub compute: Option<ComputeReport>,
}

impl std::fmt::Display for BootstrapReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.ingest)?;
        if let Some(t) = &self.torvik {
            writeln!(
                f,
                "Torvik: {} upserted, {} matched to cstat players",
                t.upserted, t.matched
            )?;
        }
        if let Some(c) = &self.compute {
            writeln!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Aggregate report for the `update` command — incremental ingest plus an
/// optional compute pass.
#[derive(Debug)]
pub struct UpdateReport {
    pub ingest: IngestReport,
    pub compute: Option<ComputeReport>,
}

impl std::fmt::Display for UpdateReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.ingest)?;
        if let Some(c) = &self.compute {
            writeln!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Per-team report from `ingest_team`.
#[derive(Debug)]
pub struct TeamReport {
    pub code: String,
    pub roster: u64,
    pub team_details: bool,
    pub player_performances: u64,
    pub team_performances: u64,
}

impl std::fmt::Display for TeamReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{}: {} roster, team details {}, {} player perfs, {} team perfs",
            self.code,
            self.roster,
            if self.team_details { "OK" } else { "skipped" },
            self.player_performances,
            self.team_performances,
        )
    }
}
