//! `GET /api/schedule?date=YYYY-MM-DD` — one day's slate for the Schedule tab
//! (issue #429): every game with its tip time, venue, team ranks, a projected
//! score and win probability, and a pregame watchability score.
//!
//! A day can hold ~150 games, so the page reads materialized projections
//! rather than running the model per game per view (the #266 lesson):
//! - **Completed** games: the row frozen in `pregame_projections` before the
//!   game tipped, which never moves again. Without one: a team's first game
//!   gets the preseason projection it was shown before tipoff, since nothing
//!   preceded it (#449). Any other game gets the nightly's `game_projections`
//!   row, recomputed from aggregates that keep moving, or, if it finished
//!   since the last nightly, its `upcoming_game_projections` row, written
//!   before tipoff. With none of these (a historical season nobody back-filled)
//!   it is left unprojected.
//! - **Upcoming, both teams have played**: the nightly's
//!   `upcoming_game_projections` row. A game the nightly has not seen yet (added
//!   to the schedule since) is projected live, up to [`LIVE_FALLBACK_CAP`] per
//!   request, so a failed nightly degrades to a partly unprojected page rather
//!   than to ~150 live model runs per view.
//! - **Upcoming, either team has not played**: the preseason projection, a
//!   subtraction on two stored anchors. The model has no season stats for an
//!   unplayed team, so it cannot answer.
//!
//! `GET /api/schedule/record?date=YYYY-MM-DD` grades the projections: the
//! night of `date`, the seven days through it, and the season through it. It
//! reads only `pregame_projections`, the projection each game was frozen at
//! before tipoff, never the recomputed rows above (`cstat_core::record`).
//!
//! `GET /api/schedule/live?date=YYYY-MM-DD` is the slate's running scores, kept
//! apart so the slate itself stays cacheable for minutes while this one turns
//! over every few seconds. Read from the public scoreboard the broadcasts step
//! matches (`cstat_ingest::live`) and never stored: a game's result, and every
//! season aggregate built on it, still waits for the nightly to see it final
//! in NatStat.

use axum::{
    Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::get,
};
use chrono::NaiveDate;
use cstat_core::projection::{self, ProjectionSummary, Venue};
use cstat_core::queries::{self, FrozenProjection, ScheduleGame, StoredGameProjection};
use cstat_core::watchability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::AppState;
use crate::routes::predict::predict_projection;
use crate::routes::teams::{PlayedProjection, is_debut, played_projection, stored_in_team_frame};

/// Max live projections in flight for one slate. Same budget, and the same
/// reasoning, as `teams::SCHEDULE_PROJECTION_CONCURRENCY`: each projection
/// peaks at several connections against a 25-connection pool.
const SLATE_PROJECTION_CONCURRENCY: usize = 6;

/// Most upcoming games one request projects live. Normally only a handful of
/// games lack a materialized row (scheduled since the last nightly); this
/// bounds the night the nightly fails, when every game on the slate would
/// otherwise go live. The same ceiling the score ticker uses.
const LIVE_FALLBACK_CAP: usize = 24;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/schedule", get(schedule))
        .route("/api/schedule/live", get(schedule_live))
        .route("/api/schedule/record", get(schedule_record))
}

/// Edge and browser lifetime of a live response. Under the server-side reuse
/// (`cstat_ingest::live::LIVE_TTL_SECS`), so a poll never waits long behind
/// a cached copy.
const LIVE_CACHE_CONTROL: &str = "public, max-age=10";

/// How long a live request waits on the scoreboard before giving up. Under
/// the client's own 30s timeout: a scoreboard would rather skip one refresh
/// than hold the request.
const LIVE_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Running scores for the night of `date`, keyed by `game_id`. Only the
/// current night (and, before the 6 AM rollover, the one before) is fetched;
/// any other day, or a night whose every game already has a stored result,
/// answers with no games and costs no NatStat call.
async fn schedule_live(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ScheduleParams>,
) -> Response {
    let today = cstat_ingest::schedule_today();
    let date = params.date.unwrap_or(today);
    let empty = || {
        (
            [(header::CACHE_CONTROL, LIVE_CACHE_CONTROL)],
            Json(json!({ "date": date, "games": {} })),
        )
            .into_response()
    };
    if date > today || today.pred_opt().is_some_and(|y| date < y) {
        return empty();
    }
    // Our own failure is a 500 like any other route's.
    let fail = |what: &str, e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("live scores: {what}: {e}") })),
        )
            .into_response()
    };
    // The feed's is not: a NatStat blip during a slate would otherwise reach
    // #errors-api once a minute through the 5xx tap, for a display the page
    // already degrades on (it keeps the last scores and tries again). Logged,
    // answered with no games, and not cached.
    let feed_down = |what: &str, e: String| {
        tracing::warn!(%date, error = %e, "live scores: {what}");
        (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "date": date, "games": {}, "unavailable": true })),
        )
            .into_response()
    };

    let season = cstat_ingest::season_for_date(date);
    let pool = &state.db.pool;
    let games = match queries::get_schedule_games(pool, season, date).await {
        Ok(g) => g,
        Err(e) => return fail("schedule query failed", e.to_string()),
    };
    // A game with a stored result is final for good; only the rest need the
    // feed. The scoreboard files a game under the Eastern day it tips, and an
    // after-midnight tip on this night is filed under the next one, so each
    // listing day is fetched.
    let pending: Vec<&ScheduleGame> = games
        .iter()
        .filter(|g| g.home_score.is_none() || g.away_score.is_none())
        .collect();
    if pending.is_empty() {
        return empty();
    }
    let ids: Vec<Uuid> = pending.iter().map(|g| g.game_id).collect();
    let keys = match queries::get_live_listing_keys(pool, &ids).await {
        Ok(k) => k,
        Err(e) => return fail("listing query failed", e.to_string()),
    };
    let mut dates: Vec<NaiveDate> = keys.iter().map(|k| k.listing_date).collect();
    dates.sort();
    dates.dedup();

    let mut by_event: HashMap<String, cstat_ingest::live::LiveGame> = HashMap::new();
    for d in dates {
        let fetched = tokio::time::timeout(
            LIVE_FETCH_TIMEOUT,
            cstat_ingest::live::fetch_day(pool, &state.scoreboard, d),
        )
        .await;
        match fetched {
            Ok(Ok(day)) => by_event.extend(day),
            Ok(Err(e)) => return feed_down("feed request failed", format!("{e:#}")),
            Err(_) => return feed_down("feed timed out", format!("{LIVE_FETCH_TIMEOUT:?}")),
        }
    }

    let live: serde_json::Map<String, Value> = keys
        .into_iter()
        .filter_map(|k| {
            let line = by_event.get(&k.source_event_id)?.clone();
            let line = line.oriented(k.source_home_is_away?);
            Some((k.game_id.to_string(), serde_json::to_value(line).ok()?))
        })
        .collect();
    (
        [(header::CACHE_CONTROL, LIVE_CACHE_CONTROL)],
        Json(json!({ "date": date, "games": live })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ScheduleParams {
    date: Option<NaiveDate>,
}

/// Days in the short window, ending on the requested date.
const RECORD_RECENT_DAYS: i64 = 7;

async fn schedule_record(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ScheduleParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let through = params.date.unwrap_or_else(cstat_ingest::schedule_today);
    let season = cstat_ingest::season_for_date(through);
    let pool = &state.db.pool;
    let fail = |e: sqlx::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("record query failed: {e}") })),
        )
    };
    // The night's games as the slate files them, so the day's record covers
    // exactly the games the page lists (a tip after midnight Eastern belongs
    // to the night before).
    let (rows, night) = tokio::try_join!(
        queries::get_pregame_results(pool, season, through + chrono::Duration::days(1)),
        queries::get_schedule_games(pool, season, through),
    )
    .map_err(fail)?;
    let night: std::collections::HashSet<Uuid> = night.iter().map(|g| g.game_id).collect();

    let recent_from = through - chrono::Duration::days(RECORD_RECENT_DAYS - 1);
    let grade = |keep: &dyn Fn(&queries::PregameResultRow) -> bool| {
        let games: Vec<_> = rows
            .iter()
            .filter(|r| keep(r))
            .map(|r| r.graded())
            .collect();
        cstat_core::record::grade(&games)
    };
    let day = grade(&|r| night.contains(&r.game_id));
    // `night` on the two windows ending at `through`: an after-midnight tip
    // on that night is dated the day after.
    let recent = grade(&|r| {
        (r.game_date >= recent_from && r.game_date <= through) || night.contains(&r.game_id)
    });
    let to_date = grade(&|r| r.game_date <= through || night.contains(&r.game_id));

    Ok(Json(json!({
        "season": season,
        "through": through,
        "recent_from": recent_from,
        "first_graded": rows.first().map(|r| r.game_date),
        "day": day,
        "recent": recent,
        "season_to_date": to_date,
    })))
}

/// Where a projection came from. Serialized for the frontend's tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Basis {
    /// Completed game, projected before tipoff.
    Pregame,
    /// Upcoming game, current model.
    Model,
    /// A game with a team that had not played: preseason projection, no score
    /// (it carries no tempo).
    Preseason,
}

#[derive(Clone, Copy, Debug)]
struct Prediction {
    /// Home perspective.
    margin: f64,
    home_win_prob: f64,
    home_score: Option<i32>,
    away_score: Option<i32>,
    basis: Basis,
}

impl Prediction {
    fn from_summary(p: ProjectionSummary, basis: Basis) -> Self {
        Prediction {
            margin: p.margin as f64,
            home_win_prob: p.home_win_prob,
            home_score: Some(p.home_score),
            away_score: Some(p.away_score),
            basis,
        }
    }

    /// A stored row, put in this game's home frame. The row carries its own
    /// home team, and the flip keys off it rather than assuming the two match.
    fn from_stored(p: &StoredGameProjection, home_id: Uuid, basis: Basis) -> Self {
        let (margin, home_win_prob, home_score, away_score) =
            stored_in_team_frame(p, p.home_team_id == home_id);
        Prediction {
            margin,
            home_win_prob,
            home_score: Some(home_score),
            away_score: Some(away_score),
            basis,
        }
    }

    /// A frozen row, put in this game's home frame the same way.
    fn from_frozen(p: &FrozenProjection, home_id: Uuid) -> Self {
        let (margin, home_win_prob, home_score, away_score) = p.in_frame(p.home_team_id == home_id);
        Prediction {
            margin,
            home_win_prob,
            home_score,
            away_score,
            basis: if p.is_preseason {
                Basis::Preseason
            } else {
                Basis::Pregame
            },
        }
    }
}

#[derive(Serialize)]
struct SlateGame {
    #[serde(flatten)]
    game: ScheduleGame,
    home_rank: Option<usize>,
    away_rank: Option<usize>,
    /// Home perspective: positive means the home side is favored.
    predicted_margin: Option<f64>,
    home_win_probability: Option<f64>,
    predicted_home_score: Option<i32>,
    predicted_away_score: Option<i32>,
    prediction_basis: Option<Basis>,
    watchability: Option<f64>,
}

/// Rank every rated team, 1 = best. Ties broken by id so the order is stable
/// across requests.
fn rank_teams(ratings: &HashMap<Uuid, f64>) -> HashMap<Uuid, usize> {
    let mut sorted: Vec<(&Uuid, &f64)> = ratings.iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i + 1))
        .collect()
}

fn round_to(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

async fn schedule(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ScheduleParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Game dates are Eastern, so "today's games" is today in Eastern (from 8pm
    // ET the UTC date is already tomorrow), rolling over at 6am rather than
    // midnight so a late West Coast or Hawaii tip stays on the default view.
    let date = params.date.unwrap_or_else(cstat_ingest::schedule_today);
    let season = cstat_ingest::season_for_date(date);
    let pool = &state.db.pool;

    let (games, frozen, stored, upcoming, rating_inputs, anchors, (prev_date, next_date)) =
        tokio::try_join!(
            queries::get_schedule_games(pool, season, date),
            queries::get_date_frozen_projections(pool, season, date),
            queries::get_date_game_projections(pool, season, date),
            queries::get_date_upcoming_projections(pool, season, date),
            queries::get_team_rating_inputs(pool, season),
            projection::fetch_preseason_adj_em_map(pool, season),
            queries::get_adjacent_game_dates(pool, date),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("schedule query failed: {e}") })),
            )
        })?;

    // Ratings are today's, whatever day is being viewed: the page answers
    // "how good are these teams", and the honest pre-game number for a past
    // game is already in its projection.
    // The same clock the projections' preseason blend reads.
    let preseason_weight = projection::live_blend_weight(cstat_ingest::today_utc(), season) as f64;
    let mut ratings: HashMap<Uuid, f64> = HashMap::with_capacity(rating_inputs.len());
    let mut played: HashMap<Uuid, bool> = HashMap::with_capacity(rating_inputs.len());
    let mut first_played: HashMap<Uuid, NaiveDate> = HashMap::new();
    for t in &rating_inputs {
        played.insert(t.team_id, t.has_played);
        if let Some(d) = t.first_played {
            first_played.insert(t.team_id, d);
        }
        if let Some(r) = watchability::team_rating(
            t.adj_em,
            anchors.get(&t.team_id).copied().map(f64::from),
            t.has_played,
            preseason_weight,
        ) {
            ratings.insert(t.team_id, r);
        }
    }
    let ranks = rank_teams(&ratings);

    let mut predictions: Vec<Option<Prediction>> = vec![None; games.len()];
    let sem = Arc::new(Semaphore::new(SLATE_PROJECTION_CONCURRENCY));
    let mut tasks: JoinSet<(usize, Option<ProjectionSummary>)> = JoinSet::new();

    let mut live_budget = LIVE_FALLBACK_CAP;
    for (idx, g) in games.iter().enumerate() {
        let (Some(home_id), Some(away_id)) = (g.home_team_id, g.away_team_id) else {
            continue;
        };
        let is_played = g.home_score.is_some() && g.away_score.is_some();

        // Same arithmetic the team page and `/api/predict` serve, on the same
        // anchors, so the three cannot disagree. No score: a strength
        // difference carries no tempo.
        let preseason = || {
            let (&h, &a) = (anchors.get(&home_id)?, anchors.get(&away_id)?);
            let venue = if g.is_neutral_site {
                Venue::Neutral
            } else {
                Venue::Home
            };
            let margin = projection::preseason_only_margin(h - a, venue);
            Some(Prediction {
                margin: margin as f64,
                home_win_prob: projection::preseason_only_win_prob(margin),
                home_score: None,
                away_score: None,
                basis: Basis::Preseason,
            })
        };

        if is_played {
            let frozen_row = frozen.get(&g.game_id);
            let debut = is_debut(&first_played, home_id, g.game_date)
                || is_debut(&first_played, away_id, g.game_date);
            predictions[idx] = match played_projection(frozen_row.is_some(), debut) {
                PlayedProjection::Frozen => frozen_row.map(|p| Prediction::from_frozen(p, home_id)),
                // Keeps the game's watchability too, rather than dropping
                // both at the final whistle.
                PlayedProjection::Preseason => preseason(),
                PlayedProjection::Recomputed => stored
                    .get(&g.game_id)
                    .or_else(|| upcoming.get(&g.game_id))
                    .map(|p| Prediction::from_stored(p, home_id, Basis::Pregame)),
            };
            continue;
        }

        if let Some(p) = upcoming.get(&g.game_id) {
            predictions[idx] = Some(Prediction::from_stored(p, home_id, Basis::Model));
            continue;
        }

        let both_played = played.get(&home_id).copied().unwrap_or(false)
            && played.get(&away_id).copied().unwrap_or(false);
        if !both_played {
            predictions[idx] = preseason();
            continue;
        }

        if live_budget == 0 {
            continue;
        }
        live_budget -= 1;
        let is_neutral = g.is_neutral_site;
        let is_conference = g.is_conference.unwrap_or(false);
        let state = Arc::clone(&state);
        let sem = Arc::clone(&sem);
        tasks.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let proj = predict_projection(
                &state,
                home_id,
                away_id,
                season,
                is_neutral,
                is_conference,
                // An unplayed game's only honest cutoff is "now".
                None,
            )
            .await
            .ok();
            (idx, proj)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        // A failed or panicked projection leaves just that row unprojected.
        if let Ok((idx, Some(proj))) = joined {
            predictions[idx] = Some(Prediction::from_summary(proj, Basis::Model));
        }
    }

    let slate: Vec<SlateGame> = games
        .into_iter()
        .zip(predictions)
        .map(|(game, pred)| {
            let rating = |id: Option<Uuid>| id.and_then(|i| ratings.get(&i).copied());
            let rank = |id: Option<Uuid>| id.and_then(|i| ranks.get(&i).copied());
            let watch = match (pred, rating(game.home_team_id), rating(game.away_team_id)) {
                (Some(p), Some(h), Some(a)) => {
                    Some(watchability::watchability(h, a, p.home_win_prob))
                }
                _ => None,
            };
            SlateGame {
                home_rank: rank(game.home_team_id),
                away_rank: rank(game.away_team_id),
                predicted_margin: pred.map(|p| round_to(p.margin, 1)),
                home_win_probability: pred.map(|p| round_to(p.home_win_prob, 3)),
                predicted_home_score: pred.and_then(|p| p.home_score),
                predicted_away_score: pred.and_then(|p| p.away_score),
                prediction_basis: pred.map(|p| p.basis),
                watchability: watch,
                game,
            }
        })
        .collect();

    Ok(Json(json!({
        "date": date,
        "season": season,
        "prev_date": prev_date,
        "next_date": next_date,
        "ranked_teams": ranks.len(),
        "games": slate,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_dense_from_one_best_first() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let ratings = HashMap::from([(a, 5.0), (b, 30.0), (c, -10.0)]);
        let ranks = rank_teams(&ratings);
        assert_eq!(ranks[&b], 1);
        assert_eq!(ranks[&a], 2);
        assert_eq!(ranks[&c], 3);
    }
}
