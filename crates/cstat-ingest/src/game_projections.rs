//! Materialize the pre-game projection for every completed game into
//! `game_projections` (migration 053), so `GET /api/teams/{id}` reads a row
//! per schedule game instead of running the model per game per page view.
//!
//! # Why this exists
//!
//! The team-detail page projected each completed game live. Every one of those
//! routes through the point-in-time feature path, whose first step
//! (`compute_pit_campom`) is a full-season GROUP BY over
//! `torvik_player_game_stats`; neutral-site games run it twice, once per team
//! ordering, for order-invariance. A 40-game schedule cost 54 rebuilds and 846
//! database round-trips, held the endpoint to ~3 requests/second, and starved
//! every other route of pooled connections while it ran (#266).
//!
//! # Why it is a batch, not a loop
//!
//! The expensive step is keyed on the **cutoff date**, not the game: every
//! game played on date D shares one point-in-time cohort. Sweeping by date
//! turns ~5,600 rebuilds into ~150. The other two feature fetches — team
//! season stats and rolling form — are season aggregates that don't vary by
//! date at all, so they're read once per team and reused for every game that
//! team played. What is left is one roster aggregate per (team, date), which
//! is genuinely per-matchup work.
//!
//! # Why it rewrites the whole season every night
//!
//! "A completed game's projection never changes" is *nearly* true and would
//! make this append-only, but only the CamPom channel of the pit feature
//! vector is actually point-in-time. Team stats, roster aggregates, and
//! rolling form all come from season-aggregate tables the nightly rewrites, so
//! a November game's projection still drifts in March. The sweep therefore
//! recomputes the full season and prunes anything it didn't refresh.
//!
//! # Parity
//!
//! Feature assembly (`cstat_core::features::assemble_features`), neutral
//! symmetrisation (`combine_neutral`), and the preseason blend
//! (`summarize_with_preseason`) are the same functions the live request path
//! calls. A precomputed row therefore equals what `predict_projection` would
//! have returned for the same matchup on the same inputs — checked directly by
//! `tests/game_projection_parity.rs`.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use cstat_core::features::{self, PitByPlayer, RollingForm, RosterAgg, TeamStats};
use cstat_core::inference::Predictor;
use cstat_core::pit_campom::compute_pit_campom;
use cstat_core::projection::{
    Attribution, BlendClock, Explained, ProjectionSummary, Venue, combine_neutral,
    predict_from_features, preseason_leg_margin, summarize_with_preseason,
};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{info, warn};
use uuid::Uuid;

/// How many per-team feature fetches to have in flight at once.
///
/// These are short indexed reads, but they run against a pool the API shares
/// when the nightly executes on the same database, so the sweep stays a modest
/// fraction of it rather than the whole thing. The dominant cost is the
/// once-per-date point-in-time rebuild, which is serial by construction, so
/// pushing this higher buys little.
const FETCH_CONCURRENCY: usize = 8;

/// One completed game to project.
#[derive(Debug, Clone, sqlx::FromRow)]
struct GameRow {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    is_neutral_site: bool,
    is_conference: Option<bool>,
}

/// What one season's sweep did, for the ledger line and the run summary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SweepReport {
    /// Rows written (inserted or updated).
    pub written: usize,
    /// Distinct cutoff dates swept — i.e. point-in-time rebuilds performed.
    pub dates: usize,
    /// Completed games that could not be projected (a team with no
    /// `team_season_stats` row yet, or a game on the first representable date,
    /// which has no "day before").
    pub skipped: usize,
    /// Rows deleted because the game they describe is no longer a completed
    /// game with those teams, venue and conference flag.
    pub pruned: usize,
    /// Per-team feature fetches that did not resolve. Non-zero means the sweep
    /// covered less than it should have; the affected games keep the previous
    /// run's rows rather than losing them (see the prune).
    pub fetch_failures: usize,
}

/// Every completed game in the season, in date order.
///
/// "Completed" is the same predicate the API uses for the pre-game label: both
/// scores populated. Games missing a team reference can't be projected at all.
async fn fetch_completed_games(pool: &PgPool, season: i32) -> Result<Vec<GameRow>> {
    let rows = sqlx::query_as::<_, GameRow>(
        r#"
        SELECT id, game_date, home_team_id, away_team_id, is_neutral_site, is_conference
        FROM games
        WHERE season = $1
          AND home_score IS NOT NULL
          AND away_score IS NOT NULL
          AND home_team_id IS NOT NULL
          AND away_team_id IS NOT NULL
        ORDER BY game_date, id
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .context("fetching completed games")?;
    Ok(rows)
}

/// Projected AdjEM per team for the season — the preseason leg of the blend,
/// read once instead of twice per game.
async fn fetch_preseason_adj_em(pool: &PgPool, season: i32) -> Result<HashMap<Uuid, f32>> {
    let rows: Vec<(Uuid, Option<f32>)> = sqlx::query_as(
        "SELECT team_id, projected_adj_em FROM team_preseason_projection WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .context("fetching preseason projections")?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, v)| v.map(|x| (id, x)))
        .collect())
}

/// Run `f` over `items` with at most [`FETCH_CONCURRENCY`] in flight.
///
/// Returns the entries that resolved plus a count of the ones that FAILED —
/// which is deliberately not the same as the ones that are missing. A team
/// absent from the map has every game it played skipped either way, but "this
/// team has no stats row yet" is an ordinary state and "the pool timed out" is
/// not, and only the second should raise the count the run summary reports.
async fn fetch_map<T, F, Fut>(
    pool: &PgPool,
    what: &'static str,
    items: &[Uuid],
    f: F,
) -> (HashMap<Uuid, T>, usize)
where
    T: Send + 'static,
    F: Fn(PgPool, Uuid) -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>> + Send + 'static,
{
    // Absent and Failed are kept apart all the way to the caller. Collapsing
    // them into one `None` is what makes a counter meaningless: a team with no
    // stats row yet is an ordinary state, and if it inflates the same number
    // that a pool timeout does, the number can no longer be read as "something
    // went wrong tonight".
    enum Outcome<T> {
        Found(Uuid, T),
        Absent,
        Failed,
    }

    let sem = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
    let mut set: JoinSet<Outcome<T>> = JoinSet::new();
    for &id in items {
        // Building the future is not running it — the permit below is what
        // gates the query.
        let fut = f(pool.clone(), id);
        let sem = Arc::clone(&sem);
        set.spawn(async move {
            let Ok(_permit) = sem.acquire_owned().await else {
                // Only reachable if the semaphore were closed, which it never
                // is; treat it as a failure rather than as absence.
                return Outcome::Failed;
            };
            match fut.await {
                Ok(v) => Outcome::Found(id, v),
                // `RowNotFound` is the ordinary "this team has nothing to
                // aggregate yet" case and stays at debug; anything else is an
                // actual failure and is worth a line in the cron log.
                Err(sqlx::Error::RowNotFound) => {
                    tracing::debug!(what, team_id = %id, "no row; skipping team");
                    Outcome::Absent
                }
                Err(e) => {
                    warn!(what, team_id = %id, error = %e, "feature fetch failed");
                    Outcome::Failed
                }
            }
        });
    }
    let mut out = HashMap::new();
    let mut failed = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Outcome::Found(id, v)) => {
                out.insert(id, v);
            }
            Ok(Outcome::Absent) => {}
            Ok(Outcome::Failed) => failed += 1,
            // A `JoinError` is a panic in the fetch task, which is not
            // supposed to happen and must not be silent either.
            Err(e) => {
                warn!(what, error = %e, "feature fetch task panicked");
                failed += 1;
            }
        }
    }
    (out, failed)
}

/// The season-invariant half of the feature inputs: one row per team, reused
/// across every date that team played on.
struct SeasonCache {
    team_stats: HashMap<Uuid, TeamStats>,
    form: HashMap<Uuid, RollingForm>,
    preseason_adj_em: HashMap<Uuid, f32>,
}

impl SeasonCache {
    async fn load(pool: &PgPool, season: i32, teams: &[Uuid]) -> Result<(Self, usize)> {
        let (team_stats, stats_failed) =
            fetch_map(pool, "team_season_stats", teams, |p, id| async move {
                features::get_team_stats(&p, id, season).await
            })
            .await;
        let (form, form_failed) = fetch_map(pool, "rolling_form", teams, |p, id| async move {
            features::get_rolling_form(&p, id, season).await
        })
        .await;
        let preseason_adj_em = fetch_preseason_adj_em(pool, season).await?;
        Ok((
            Self {
                team_stats,
                form,
                preseason_adj_em,
            },
            stats_failed + form_failed,
        ))
    }

    /// The preseason margin (home perspective) for a matchup, or `None` when
    /// either team lacks a projection row. Mirrors
    /// `projection::fetch_preseason_margin`, off the cached map.
    fn preseason_margin(&self, home: Uuid, away: Uuid, venue: Venue) -> Option<f32> {
        let h = self.preseason_adj_em.get(&home)?;
        let a = self.preseason_adj_em.get(&away)?;
        Some(preseason_leg_margin(h - a, venue))
    }
}

/// Build one team's half of the feature inputs for a given cutoff date.
///
/// Returns `None` when any part is missing, which drops the game rather than
/// projecting it from a partial vector.
fn parts<'a>(
    cache: &'a SeasonCache,
    rosters: &'a HashMap<Uuid, RosterAgg>,
    team: Uuid,
) -> Option<(&'a TeamStats, &'a RosterAgg, &'a RollingForm)> {
    Some((
        cache.team_stats.get(&team)?,
        rosters.get(&team)?,
        cache.form.get(&team)?,
    ))
}

/// The model leg for one game from already-fetched parts, in the home team's
/// frame — the prediction before any preseason blend.
///
/// Neutral-site games are predicted in both orderings and averaged, exactly as
/// `predict_with_venue` does, so a precomputed neutral projection is
/// order-invariant for the same reason the live one is.
fn explain_game(
    predictor: &Predictor,
    cache: &SeasonCache,
    rosters: &HashMap<Uuid, RosterAgg>,
    game: &GameRow,
    is_pit: bool,
) -> Option<Explained> {
    let (home_ts, home_roster, home_form) = parts(cache, rosters, game.home_team_id)?;
    let (away_ts, away_roster, away_form) = parts(cache, rosters, game.away_team_id)?;
    let is_neutral = game.is_neutral_site;
    let is_conference = game.is_conference.unwrap_or(false);

    let build = |h_ts, h_roster: &RosterAgg, h_form, a_ts, a_roster: &RosterAgg, a_form| {
        features::assemble_features(
            h_ts,
            a_ts,
            h_roster.clone(),
            a_roster.clone(),
            h_form,
            a_form,
            is_neutral,
            is_conference,
        )
    };

    let fwd_features = build(
        home_ts,
        home_roster,
        home_form,
        away_ts,
        away_roster,
        away_form,
    );
    // Attribution is skipped throughout: `game_projections` stores margin,
    // win probability and scores, none of which TreeSHAP touches.
    let fwd = predict_from_features(predictor, &fwd_features, is_pit, Attribution::Skip).ok()?;

    let explained: Explained = if is_neutral {
        let rev_features = build(
            away_ts,
            away_roster,
            away_form,
            home_ts,
            home_roster,
            home_form,
        );
        let rev =
            predict_from_features(predictor, &rev_features, is_pit, Attribution::Skip).ok()?;
        combine_neutral(fwd, rev, is_pit)
    } else {
        fwd
    };
    Some(explained)
}

/// Project one game: the model leg from [`explain_game`], then the same
/// preseason blend the live path applies.
fn project_game(
    cache: &SeasonCache,
    game: &GameRow,
    explained: &Explained,
    as_of_date: NaiveDate,
    season: i32,
) -> ProjectionSummary {
    blend_game(cache, game, explained, BlendClock::AsOf(as_of_date), season)
}

/// The preseason blend for one game's model leg, on `clock`.
fn blend_game(
    cache: &SeasonCache,
    game: &GameRow,
    explained: &Explained,
    clock: BlendClock,
    season: i32,
) -> ProjectionSummary {
    let venue = if game.is_neutral_site {
        Venue::Neutral
    } else {
        Venue::Home
    };
    let pre_margin = if clock.blend_weight(season) > 0.0 {
        cache.preseason_margin(game.home_team_id, game.away_team_id, venue)
    } else {
        None
    };
    summarize_with_preseason(clock, season, pre_margin, explained)
}

/// One row's worth of output, accumulated per date and written in one
/// statement.
struct OutRow {
    game_id: Uuid,
    game_date: NaiveDate,
    as_of_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    is_neutral: bool,
    is_conference: bool,
    summary: ProjectionSummary,
}

/// Upsert a date's rows in a single statement. `UNNEST` over parallel arrays
/// rather than a row per `INSERT`: the whole point of this module is to stop
/// paying per-game round-trips.
async fn write_rows(pool: &PgPool, season: i32, rows: &[OutRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let game_ids: Vec<Uuid> = rows.iter().map(|r| r.game_id).collect();
    let game_dates: Vec<NaiveDate> = rows.iter().map(|r| r.game_date).collect();
    let as_of_dates: Vec<NaiveDate> = rows.iter().map(|r| r.as_of_date).collect();
    let home_ids: Vec<Uuid> = rows.iter().map(|r| r.home_team_id).collect();
    let away_ids: Vec<Uuid> = rows.iter().map(|r| r.away_team_id).collect();
    let neutrals: Vec<bool> = rows.iter().map(|r| r.is_neutral).collect();
    let conferences: Vec<bool> = rows.iter().map(|r| r.is_conference).collect();
    let margins: Vec<f64> = rows.iter().map(|r| r.summary.margin as f64).collect();
    let win_probs: Vec<f64> = rows.iter().map(|r| r.summary.home_win_prob).collect();
    let home_scores: Vec<i32> = rows.iter().map(|r| r.summary.home_score).collect();
    let away_scores: Vec<i32> = rows.iter().map(|r| r.summary.away_score).collect();

    let result = sqlx::query(
        r#"
        INSERT INTO game_projections
            (game_id, season, game_date, as_of_date, home_team_id, away_team_id,
             is_neutral, is_conference, projected_margin, home_win_prob,
             projected_home_score, projected_away_score, computed_at)
        SELECT g, $2, gd, aod, h, a, n, c, m, p, hs, aws, now()
        FROM UNNEST(
            $1::uuid[], $3::date[], $4::date[], $5::uuid[], $6::uuid[],
            $7::bool[], $8::bool[], $9::float8[], $10::float8[], $11::int[], $12::int[]
        ) AS t(g, gd, aod, h, a, n, c, m, p, hs, aws)
        ON CONFLICT (game_id) DO UPDATE SET
            season               = EXCLUDED.season,
            game_date            = EXCLUDED.game_date,
            as_of_date           = EXCLUDED.as_of_date,
            home_team_id         = EXCLUDED.home_team_id,
            away_team_id         = EXCLUDED.away_team_id,
            is_neutral           = EXCLUDED.is_neutral,
            is_conference        = EXCLUDED.is_conference,
            projected_margin     = EXCLUDED.projected_margin,
            home_win_prob        = EXCLUDED.home_win_prob,
            projected_home_score = EXCLUDED.projected_home_score,
            projected_away_score = EXCLUDED.projected_away_score,
            computed_at          = now()
        "#,
    )
    .bind(&game_ids)
    .bind(season)
    .bind(&game_dates)
    .bind(&as_of_dates)
    .bind(&home_ids)
    .bind(&away_ids)
    .bind(&neutrals)
    .bind(&conferences)
    .bind(&margins)
    .bind(&win_probs)
    .bind(&home_scores)
    .bind(&away_scores)
    .execute(pool)
    .await
    .context("writing game_projections rows")?;
    Ok(result.rows_affected() as usize)
}

/// The model leg for every game sharing one point-in-time cutoff, in `day`
/// order (`None` where a game cannot be projected), plus the count of failed
/// roster fetches. The once-per-date rebuild this module exists to batch.
async fn explain_cutoff(
    pool: &PgPool,
    predictor: &Predictor,
    cache: &SeasonCache,
    season: i32,
    cutoff: NaiveDate,
    day: &[&GameRow],
) -> Result<(Vec<Option<Explained>>, usize)> {
    let pit: Arc<PitByPlayer> = Arc::new(
        compute_pit_campom(pool, season, cutoff)
            .await
            .with_context(|| format!("point-in-time CamPom for {season} as of {cutoff}"))?,
    );

    let day_teams: Vec<Uuid> = day
        .iter()
        .flat_map(|g| [g.home_team_id, g.away_team_id])
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let (rosters, roster_failed) = fetch_map(pool, "roster_agg_pit", &day_teams, |p, id| {
        let pit = Arc::clone(&pit);
        async move { features::get_roster_agg_pit(&p, id, season, &pit).await }
    })
    .await;

    let legs = day
        .iter()
        .map(|g| explain_game(predictor, cache, &rosters, g, true))
        .collect();
    Ok((legs, roster_failed))
}

/// Every distinct team in `games`.
fn teams_of(games: &[GameRow]) -> Vec<Uuid> {
    games
        .iter()
        .flat_map(|g| [g.home_team_id, g.away_team_id])
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Group games by point-in-time cutoff (`game_date − 1`), in date order, plus
/// the count of games with no cutoff at all.
///
/// Group by cutoff, not by game date: the cutoff is what the point-in-time
/// cohort is keyed on, and it is the one thing worth paying for once. A game
/// on the first representable date has no "day before" and is dropped,
/// matching the API's rule that such a game is not labelled pre-game.
///
/// Shared by the sweep and [`model_legs`] so the calibration tool scores the
/// same games against the same cohorts the nightly writes.
fn group_by_cutoff(games: &[GameRow]) -> (BTreeMap<NaiveDate, Vec<&GameRow>>, usize) {
    let mut by_cutoff: BTreeMap<NaiveDate, Vec<&GameRow>> = BTreeMap::new();
    let mut no_cutoff = 0;
    for g in games {
        match g.game_date.pred_opt() {
            Some(cutoff) => by_cutoff.entry(cutoff).or_default().push(g),
            None => no_cutoff += 1,
        }
    }
    (by_cutoff, no_cutoff)
}

/// The model leg for each projectable game, and what could not be projected.
#[derive(Debug, Default)]
pub struct ModelLegs {
    /// Un-blended model margin (home perspective), keyed by game id.
    pub margins: HashMap<Uuid, f32>,
    /// Completed games in the window with no margin — absent data (a team
    /// with no stats row) or a failed fetch.
    pub skipped: usize,
    /// Feature fetches that FAILED rather than found nothing. Non-zero means
    /// the sample is short for a reason unrelated to the data, and a caller
    /// scoring the legs should say so.
    pub fetch_failures: usize,
}

/// The un-blended model margin for every completed game played on or before
/// `through`.
///
/// For `measure-blend-accuracy`, which has to hold the two legs of the blend
/// apart to fit the mix — the sweep itself only ever stores the blended
/// result. Same cohort, same features, same neutral symmetrisation as the
/// rows the nightly writes, so the tool calibrates against the leg that is
/// actually served on the completed-game path. Nothing is written.
pub async fn model_legs(
    pool: &PgPool,
    predictor: &Predictor,
    season: i32,
    through: NaiveDate,
) -> Result<ModelLegs> {
    let games: Vec<GameRow> = fetch_completed_games(pool, season)
        .await?
        .into_iter()
        .filter(|g| g.game_date <= through)
        .collect();
    let (cache, stats_failed) = SeasonCache::load(pool, season, &teams_of(&games)).await?;
    let (by_cutoff, no_cutoff) = group_by_cutoff(&games);

    let mut out = ModelLegs {
        margins: HashMap::with_capacity(games.len()),
        skipped: no_cutoff,
        fetch_failures: stats_failed,
    };
    for (&cutoff, day) in &by_cutoff {
        let (legs, roster_failed) =
            explain_cutoff(pool, predictor, &cache, season, cutoff, day).await?;
        out.fetch_failures += roster_failed;
        for (g, leg) in day.iter().zip(legs) {
            match leg {
                Some(explained) => {
                    out.margins
                        .insert(g.id, explained.prediction.predicted_margin);
                }
                None => out.skipped += 1,
            }
        }
    }
    Ok(out)
}

/// Sweep one season: recompute and persist every completed game's pre-game
/// projection, then prune rows this run no longer produces.
pub async fn run_season(pool: &PgPool, predictor: &Predictor, season: i32) -> Result<SweepReport> {
    let games = fetch_completed_games(pool, season).await?;
    let mut report = SweepReport::default();
    if games.is_empty() {
        info!(season, "no completed games; nothing to project");
        return Ok(report);
    }

    let (cache, fetch_failures) = SeasonCache::load(pool, season, &teams_of(&games)).await?;
    report.fetch_failures = fetch_failures;

    let (by_cutoff, no_cutoff) = group_by_cutoff(&games);
    report.skipped += no_cutoff;
    report.dates = by_cutoff.len();

    for (&cutoff, day) in &by_cutoff {
        let (legs, roster_failed) =
            explain_cutoff(pool, predictor, &cache, season, cutoff, day).await?;
        report.fetch_failures += roster_failed;

        let mut out = Vec::with_capacity(day.len());
        for (g, leg) in day.iter().zip(&legs) {
            match leg {
                Some(explained) => out.push(OutRow {
                    game_id: g.id,
                    game_date: g.game_date,
                    as_of_date: cutoff,
                    home_team_id: g.home_team_id,
                    away_team_id: g.away_team_id,
                    is_neutral: g.is_neutral_site,
                    is_conference: g.is_conference.unwrap_or(false),
                    summary: project_game(&cache, g, explained, cutoff, season),
                }),
                None => report.skipped += 1,
            }
        }
        report.written += write_rows(pool, season, &out).await?;
    }

    // Prune rows that no longer DESCRIBE a completed game as it now stands —
    // not rows this particular run failed to refresh.
    //
    // The difference matters. A timestamp prune ("delete what I didn't rewrite
    // tonight") turns any transient error into data loss: one pool timeout
    // while fetching a team's season stats drops that team from the cache,
    // skips all ~35 of its games, and then deletes 35 correct rows, sending
    // its page back to the slow live path for a day. Keyed on the row's own
    // content instead, a skipped game simply keeps last night's projection,
    // which is exactly what the fallback would have recomputed anyway.
    //
    // The predicate deletes precisely the rows that have stopped being true:
    // a game whose scores were nulled by a correction (postponed after the
    // fact), and a game whose teams, venue or conference flag were rewritten
    // under it — which is what those four columns are stored for. A deleted
    // game needs no clause here; the `game_id` foreign key cascades.
    let pruned = sqlx::query(
        r#"
        DELETE FROM game_projections gp
        WHERE gp.season = $1
          AND NOT EXISTS (
              SELECT 1 FROM games g
              WHERE g.id = gp.game_id
                AND g.season = gp.season
                AND g.home_score IS NOT NULL
                AND g.away_score IS NOT NULL
                AND g.home_team_id = gp.home_team_id
                AND g.away_team_id = gp.away_team_id
                AND g.is_neutral_site = gp.is_neutral
                AND COALESCE(g.is_conference, false) = gp.is_conference
          )
        "#,
    )
    .bind(season)
    .execute(pool)
    .await
    .context("pruning stale game_projections rows")?;
    report.pruned = pruned.rows_affected() as usize;

    info!(
        season,
        written = report.written,
        dates = report.dates,
        skipped = report.skipped,
        pruned = report.pruned,
        fetch_failures = report.fetch_failures,
        "materialized completed-game projections"
    );
    if report.skipped > 0 {
        warn!(
            season,
            skipped = report.skipped,
            fetch_failures = report.fetch_failures,
            "some completed games could not be projected; they keep the previous sweep's rows, \
             or fall back to a live projection if they never had one. A non-zero fetch_failures \
             means the cause was a failed query rather than absent data — see the warnings above"
        );
    }
    Ok(report)
}

/// Sweep each season in `seasons`, accumulating one report.
pub async fn run(pool: &PgPool, predictor: &Predictor, seasons: &[i32]) -> Result<SweepReport> {
    let mut total = SweepReport::default();
    for &season in seasons {
        let r = run_season(pool, predictor, season).await?;
        total.written += r.written;
        total.dates += r.dates;
        total.skipped += r.skipped;
        total.pruned += r.pruned;
        total.fetch_failures += r.fetch_failures;
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Upcoming games (#429)
// ---------------------------------------------------------------------------

/// What one season's upcoming-game sweep did.
#[derive(Debug, Default, Clone, Copy)]
pub struct UpcomingReport {
    /// Rows written (inserted or updated).
    pub written: usize,
    /// Upcoming games that could not be projected (a feature part missing).
    pub skipped: usize,
    /// Rows deleted: games now covered by `game_projections`, or whose teams,
    /// venue or conference flag changed under them.
    pub pruned: usize,
    /// Per-team feature fetches that FAILED rather than found nothing.
    pub fetch_failures: usize,
}

/// Every unplayed game in the season whose two teams have both played.
///
/// "Both played" is the line between the model and the preseason projection,
/// the same one the Schedule tab draws: a team with no completed game has no
/// season stats worth feeding the model — at most the blank row the `/teams`
/// ingest writes, which would yield an all-default vector and a confident
/// garbage margin. The API answers those games from the preseason anchors.
async fn fetch_upcoming_games(pool: &PgPool, season: i32) -> Result<Vec<GameRow>> {
    let rows = sqlx::query_as::<_, GameRow>(
        r#"
        WITH played AS (
            SELECT home_team_id AS team_id FROM games
             WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL
            UNION
            SELECT away_team_id FROM games
             WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL
        )
        SELECT g.id, g.game_date, g.home_team_id, g.away_team_id,
               g.is_neutral_site, g.is_conference
        FROM games g
        JOIN played ph ON ph.team_id = g.home_team_id
        JOIN played pa ON pa.team_id = g.away_team_id
        WHERE g.season = $1
          AND (g.home_score IS NULL OR g.away_score IS NULL)
        ORDER BY g.game_date, g.id
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .context("fetching upcoming games")?;
    Ok(rows)
}

/// Project every upcoming game of `season` and persist it to
/// `upcoming_game_projections`, with the preseason blend weighted for
/// `blend_date` (the nightly's today).
///
/// Cheap where the completed-game sweep is not: an upcoming game reads only
/// season-aggregate features, so every part is fetched once per TEAM — three
/// reads for each of ~360 teams — and each game is then a vector assembly and
/// an ONNX run. The result equals the live `predict_projection` on the same
/// inputs (same assembly, same neutral symmetrisation, same blend), and those
/// inputs only move when the nightly recomputes them.
pub async fn run_upcoming_season(
    pool: &PgPool,
    predictor: &Predictor,
    season: i32,
    blend_date: NaiveDate,
) -> Result<UpcomingReport> {
    let games = fetch_upcoming_games(pool, season).await?;
    let mut report = UpcomingReport::default();

    if !games.is_empty() {
        let teams = teams_of(&games);
        let (cache, stats_failed) = SeasonCache::load(pool, season, &teams).await?;
        let (rosters, roster_failed) = fetch_map(pool, "roster_agg", &teams, |p, id| async move {
            features::get_roster_agg(&p, id, season).await
        })
        .await;
        report.fetch_failures = stats_failed + roster_failed;

        let clock = BlendClock::Live(blend_date);
        let mut out = Vec::with_capacity(games.len());
        for g in &games {
            match explain_game(predictor, &cache, &rosters, g, false) {
                Some(explained) => out.push(OutRow {
                    game_id: g.id,
                    game_date: g.game_date,
                    as_of_date: blend_date,
                    home_team_id: g.home_team_id,
                    away_team_id: g.away_team_id,
                    is_neutral: g.is_neutral_site,
                    is_conference: g.is_conference.unwrap_or(false),
                    summary: blend_game(&cache, g, &explained, clock, season),
                }),
                None => report.skipped += 1,
            }
        }
        report.written = write_upcoming_rows(pool, season, &out).await?;
    }

    // Content-keyed, like the completed-game prune: a game skipped tonight by
    // a transient failure keeps last night's row. A row goes when the game is
    // gone from what it describes, or once `game_projections` holds the game —
    // until then a game played since this run still reads its row here, which
    // is its pre-game projection.
    let pruned = sqlx::query(
        r#"
        DELETE FROM upcoming_game_projections u
        WHERE u.season = $1
          AND (
              EXISTS (SELECT 1 FROM game_projections gp WHERE gp.game_id = u.game_id)
              OR NOT EXISTS (
                  SELECT 1 FROM games g
                  WHERE g.id = u.game_id
                    AND g.season = u.season
                    AND g.home_team_id = u.home_team_id
                    AND g.away_team_id = u.away_team_id
                    AND g.is_neutral_site = u.is_neutral
                    AND COALESCE(g.is_conference, false) = u.is_conference
              )
          )
        "#,
    )
    .bind(season)
    .execute(pool)
    .await
    .context("pruning stale upcoming_game_projections rows")?;
    report.pruned = pruned.rows_affected() as usize;

    info!(
        season,
        written = report.written,
        skipped = report.skipped,
        pruned = report.pruned,
        fetch_failures = report.fetch_failures,
        "materialized upcoming-game projections"
    );
    Ok(report)
}

/// Upsert upcoming rows in one statement. `as_of_date` on [`OutRow`] carries
/// the blend date here.
async fn write_upcoming_rows(pool: &PgPool, season: i32, rows: &[OutRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let game_ids: Vec<Uuid> = rows.iter().map(|r| r.game_id).collect();
    let game_dates: Vec<NaiveDate> = rows.iter().map(|r| r.game_date).collect();
    let blend_dates: Vec<NaiveDate> = rows.iter().map(|r| r.as_of_date).collect();
    let home_ids: Vec<Uuid> = rows.iter().map(|r| r.home_team_id).collect();
    let away_ids: Vec<Uuid> = rows.iter().map(|r| r.away_team_id).collect();
    let neutrals: Vec<bool> = rows.iter().map(|r| r.is_neutral).collect();
    let conferences: Vec<bool> = rows.iter().map(|r| r.is_conference).collect();
    let margins: Vec<f64> = rows.iter().map(|r| r.summary.margin as f64).collect();
    let win_probs: Vec<f64> = rows.iter().map(|r| r.summary.home_win_prob).collect();
    let home_scores: Vec<i32> = rows.iter().map(|r| r.summary.home_score).collect();
    let away_scores: Vec<i32> = rows.iter().map(|r| r.summary.away_score).collect();

    let result = sqlx::query(
        r#"
        INSERT INTO upcoming_game_projections
            (game_id, season, game_date, blend_date, home_team_id, away_team_id,
             is_neutral, is_conference, projected_margin, home_win_prob,
             projected_home_score, projected_away_score, computed_at)
        SELECT g, $2, gd, bd, h, a, n, c, m, p, hs, aws, now()
        FROM UNNEST(
            $1::uuid[], $3::date[], $4::date[], $5::uuid[], $6::uuid[],
            $7::bool[], $8::bool[], $9::float8[], $10::float8[], $11::int[], $12::int[]
        ) AS t(g, gd, bd, h, a, n, c, m, p, hs, aws)
        ON CONFLICT (game_id) DO UPDATE SET
            season               = EXCLUDED.season,
            game_date            = EXCLUDED.game_date,
            blend_date           = EXCLUDED.blend_date,
            home_team_id         = EXCLUDED.home_team_id,
            away_team_id         = EXCLUDED.away_team_id,
            is_neutral           = EXCLUDED.is_neutral,
            is_conference        = EXCLUDED.is_conference,
            projected_margin     = EXCLUDED.projected_margin,
            home_win_prob        = EXCLUDED.home_win_prob,
            projected_home_score = EXCLUDED.projected_home_score,
            projected_away_score = EXCLUDED.projected_away_score,
            computed_at          = now()
        "#,
    )
    .bind(&game_ids)
    .bind(season)
    .bind(&game_dates)
    .bind(&blend_dates)
    .bind(&home_ids)
    .bind(&away_ids)
    .bind(&neutrals)
    .bind(&conferences)
    .bind(&margins)
    .bind(&win_probs)
    .bind(&home_scores)
    .bind(&away_scores)
    .execute(pool)
    .await
    .context("writing upcoming_game_projections rows")?;
    Ok(result.rows_affected() as usize)
}

// ---------------------------------------------------------------------------
// Frozen pregame record (migration 063)
// ---------------------------------------------------------------------------

/// What one freeze did.
#[derive(Debug, Default, Clone, Copy)]
pub struct FreezeReport {
    /// Games frozen from their `upcoming_game_projections` row.
    pub model: usize,
    /// Games frozen at the preseason projection (a team has not played).
    pub preseason: usize,
    /// Untipped games with neither: both teams have played but the upcoming
    /// sweep has no row for the game, so the page projects it live and there
    /// is nothing stored to freeze.
    pub missing: usize,
    /// Frozen rows deleted because an unplayed game's teams or venue changed.
    pub pruned: usize,
}

#[derive(sqlx::FromRow)]
struct UntippedGame {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    is_neutral_site: bool,
    both_played: bool,
    projected_margin: Option<f64>,
    home_win_prob: Option<f64>,
    projected_home_score: Option<i32>,
    projected_away_score: Option<i32>,
}

/// Copy the projection of every game of `season` that has not tipped by
/// `now_eastern` into `pregame_projections`, overwriting the previous night's.
///
/// Run after the upcoming sweep. A game stops being refreshed once it tips, so
/// its row holds the last projection made before it: the record the Schedule
/// tab's prediction stats are graded on. "Tipped" reads the same tip instant
/// the Schedule tab files a game under (the broadcast listing's confirmed time,
/// else the schedule feed's); a game with no known time counts as untipped for
/// all of its date, which the morning nightly is comfortably inside.
///
/// The basis mirrors what the Schedule tab shows: the stored model projection,
/// or the preseason projection when either team has not played. Both-played
/// games without a stored row are counted, not guessed at.
pub async fn freeze_pregame(
    pool: &PgPool,
    season: i32,
    now_eastern: chrono::NaiveDateTime,
) -> Result<FreezeReport> {
    let games = sqlx::query_as::<_, UntippedGame>(
        r#"
        WITH played AS (
            SELECT home_team_id AS team_id FROM games
             WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL
            UNION
            SELECT away_team_id FROM games
             WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL
        ),
        untipped AS (
            SELECT g.id, g.game_date, g.home_team_id, g.away_team_id, g.is_neutral_site,
                   CASE WHEN gb.tip_time IS NOT NULL
                            THEN COALESCE(gb.tip_date, g.game_date) + gb.tip_time
                        WHEN g.tip_time IS NOT NULL
                            THEN g.game_date + g.tip_time
                   END AS tip_at
            FROM games g
            LEFT JOIN game_broadcasts gb ON gb.game_id = g.id
            WHERE g.season = $1
              AND (g.home_score IS NULL OR g.away_score IS NULL)
              AND g.home_team_id IS NOT NULL AND g.away_team_id IS NOT NULL
              AND g.game_date IS NOT NULL
        )
        SELECT t.id, t.game_date, t.home_team_id, t.away_team_id, t.is_neutral_site,
               (EXISTS (SELECT 1 FROM played p WHERE p.team_id = t.home_team_id)
                AND EXISTS (SELECT 1 FROM played p WHERE p.team_id = t.away_team_id))
                   AS both_played,
               u.projected_margin, u.home_win_prob,
               u.projected_home_score, u.projected_away_score
        FROM untipped t
        LEFT JOIN upcoming_game_projections u
               ON u.game_id = t.id
              AND u.home_team_id = t.home_team_id
              AND u.away_team_id = t.away_team_id
              AND u.is_neutral = t.is_neutral_site
        WHERE CASE WHEN t.tip_at IS NOT NULL THEN t.tip_at > $2
                   ELSE t.game_date >= $2::date END
        "#,
    )
    .bind(season)
    .bind(now_eastern)
    .fetch_all(pool)
    .await
    .context("fetching untipped games")?;

    let anchors = fetch_preseason_adj_em(pool, season).await?;
    let mut report = FreezeReport::default();

    let mut ids = Vec::new();
    let mut dates = Vec::new();
    let mut homes = Vec::new();
    let mut aways = Vec::new();
    let mut neutrals = Vec::new();
    let mut bases = Vec::new();
    let mut margins = Vec::new();
    let mut win_probs = Vec::new();
    let mut home_scores: Vec<Option<i32>> = Vec::new();
    let mut away_scores: Vec<Option<i32>> = Vec::new();

    for g in &games {
        let row = match (g.projected_margin, g.home_win_prob) {
            (Some(m), Some(p)) => {
                report.model += 1;
                Some((
                    "model",
                    m,
                    p,
                    g.projected_home_score,
                    g.projected_away_score,
                ))
            }
            _ if !g.both_played => {
                // The Schedule tab's own arithmetic, on the same anchors.
                match (anchors.get(&g.home_team_id), anchors.get(&g.away_team_id)) {
                    (Some(&h), Some(&a)) => {
                        let venue = if g.is_neutral_site {
                            Venue::Neutral
                        } else {
                            Venue::Home
                        };
                        let m = cstat_core::projection::preseason_only_margin(h - a, venue);
                        report.preseason += 1;
                        Some((
                            "preseason",
                            m as f64,
                            cstat_core::projection::preseason_only_win_prob(m),
                            None,
                            None,
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((basis, margin, win_prob, hs, aws)) = row else {
            report.missing += 1;
            continue;
        };
        ids.push(g.id);
        dates.push(g.game_date);
        homes.push(g.home_team_id);
        aways.push(g.away_team_id);
        neutrals.push(g.is_neutral_site);
        bases.push(basis.to_string());
        margins.push(margin);
        win_probs.push(win_prob);
        home_scores.push(hs);
        away_scores.push(aws);
    }

    if !ids.is_empty() {
        sqlx::query(
            r#"
            INSERT INTO pregame_projections
                (game_id, season, game_date, home_team_id, away_team_id, is_neutral,
                 basis, projected_margin, home_win_prob,
                 projected_home_score, projected_away_score, captured_at)
            SELECT g, $2, d, h, a, n, b, m, p, hs, aws, now()
            FROM UNNEST(
                $1::uuid[], $3::date[], $4::uuid[], $5::uuid[], $6::bool[],
                $7::text[], $8::float8[], $9::float8[], $10::int[], $11::int[]
            ) AS t(g, d, h, a, n, b, m, p, hs, aws)
            ON CONFLICT (game_id) DO UPDATE SET
                season               = EXCLUDED.season,
                game_date            = EXCLUDED.game_date,
                home_team_id         = EXCLUDED.home_team_id,
                away_team_id         = EXCLUDED.away_team_id,
                is_neutral           = EXCLUDED.is_neutral,
                basis                = EXCLUDED.basis,
                projected_margin     = EXCLUDED.projected_margin,
                home_win_prob        = EXCLUDED.home_win_prob,
                projected_home_score = EXCLUDED.projected_home_score,
                projected_away_score = EXCLUDED.projected_away_score,
                captured_at          = now()
            "#,
        )
        .bind(&ids)
        .bind(season)
        .bind(&dates)
        .bind(&homes)
        .bind(&aways)
        .bind(&neutrals)
        .bind(&bases)
        .bind(&margins)
        .bind(&win_probs)
        .bind(&home_scores)
        .bind(&away_scores)
        .execute(pool)
        .await
        .context("writing pregame_projections rows")?;
    }

    // A frozen row describes a matchup; if the schedule feed has since moved
    // an unplayed game to other teams or another venue, it no longer describes
    // it. Played games are left alone: `correct_swapped_games` relabels home
    // and away on a played game whose box rows arrived crossed, and deleting
    // the row then would lose a record nothing can rebuild. A reader matches
    // the row to the game by team, not by side.
    let pruned = sqlx::query(
        r#"
        DELETE FROM pregame_projections pp
        USING games g
        WHERE pp.season = $1
          AND g.id = pp.game_id
          AND (g.home_score IS NULL OR g.away_score IS NULL)
          AND (g.home_team_id IS DISTINCT FROM pp.home_team_id
               OR g.away_team_id IS DISTINCT FROM pp.away_team_id
               OR g.is_neutral_site IS DISTINCT FROM pp.is_neutral)
        "#,
    )
    .bind(season)
    .execute(pool)
    .await
    .context("pruning stale pregame_projections rows")?;
    report.pruned = pruned.rows_affected() as usize;

    info!(
        season,
        model = report.model,
        preseason = report.preseason,
        missing = report.missing,
        pruned = report.pruned,
        "froze pregame projections"
    );
    Ok(report)
}
