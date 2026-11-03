//! `GET /api/schedule?date=YYYY-MM-DD` — one day's slate for the Schedule tab
//! (issue #429): every game with its tip time, venue, team ranks, a projected
//! score and win probability, and a pregame watchability score.
//!
//! A day can hold ~150 games, so the page reads materialized projections
//! rather than running the model per game per view (the #266 lesson):
//! - **Completed** games: the nightly's `game_projections` row, the honest
//!   pre-game number. A game finished since the last nightly still has its
//!   `upcoming_game_projections` row, written before tipoff from inputs that
//!   did not include it, which is the same claim. A team's first game has
//!   neither, since nothing preceded it to project from; it gets the
//!   preseason projection it was shown before tipoff. Any other completed game
//!   with no row (a historical season nobody back-filled) is left unprojected.
//! - **Upcoming, both teams have played**: the nightly's
//!   `upcoming_game_projections` row. A game the nightly has not seen yet (added
//!   to the schedule since) is projected live, up to [`LIVE_FALLBACK_CAP`] per
//!   request, so a failed nightly degrades to a partly unprojected page rather
//!   than to ~150 live model runs per view.
//! - **Upcoming, either team has not played**: the preseason projection, a
//!   subtraction on two stored anchors. The model has no season stats for an
//!   unplayed team, so it cannot answer.
//!
//! `GET /api/schedule/live?date=YYYY-MM-DD` is the slate's running scores, kept
//! apart so the slate itself stays cacheable for minutes while this one turns
//! over every half minute. Read straight from NatStat (`cstat_ingest::live`)
//! and never stored: a game's result, and every season aggregate built on it,
//! still waits for the nightly to see it final.

use axum::{
    Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::get,
};
use chrono::NaiveDate;
use cstat_core::projection::{self, ProjectionSummary, Venue};
use cstat_core::queries::{self, ScheduleGame, StoredGameProjection};
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
use crate::routes::teams::stored_in_team_frame;

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
}

/// Edge and browser lifetime of a live response. Half the server-side reuse
/// (`cstat_ingest::live::LIVE_TTL_SECS`), so a poll never waits a full extra
/// minute behind a cached copy.
const LIVE_CACHE_CONTROL: &str = "public, max-age=30";

/// How long a live request waits on NatStat before giving up. The client
/// retries 5xx with backoff for most of a minute, which would hold the
/// request to its 30s timeout; a scoreboard would rather skip one refresh.
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
    let fail = |what: &str, e: String| {
        tracing::warn!(%date, error = %e, "live scores: {what}");
        (
            StatusCode::BAD_GATEWAY,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "error": format!("live scores unavailable: {what}") })),
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
    // feed. NatStat dates by calendar day, and an after-midnight tip on this
    // night is dated the next one, so each pending date is fetched.
    let pending: Vec<&ScheduleGame> = games
        .iter()
        .filter(|g| g.home_score.is_none() || g.away_score.is_none())
        .collect();
    if pending.is_empty() {
        return empty();
    }
    let ids: Vec<Uuid> = pending.iter().map(|g| g.game_id).collect();
    let natstat_ids = match queries::get_game_natstat_ids(pool, &ids).await {
        Ok(m) => m,
        Err(e) => return fail("game id query failed", e.to_string()),
    };
    let mut dates: Vec<NaiveDate> = pending.iter().map(|g| g.game_date).collect();
    dates.sort();
    dates.dedup();

    let mut by_natstat: HashMap<String, cstat_ingest::live::LiveGame> = HashMap::new();
    for d in dates {
        let fetched = tokio::time::timeout(
            LIVE_FETCH_TIMEOUT,
            cstat_ingest::live::fetch_day(&state.natstat, d),
        )
        .await;
        match fetched {
            Ok(Ok(day)) => {
                by_natstat.extend(day.into_iter().map(|g| (g.natstat_id.clone(), g)));
            }
            Ok(Err(e)) => return fail("feed request failed", e.to_string()),
            Err(_) => return fail("feed timed out", format!("{LIVE_FETCH_TIMEOUT:?}")),
        }
    }

    let live: serde_json::Map<String, Value> = pending
        .iter()
        .filter_map(|g| {
            let line = by_natstat.get(natstat_ids.get(&g.game_id)?)?;
            Some((g.game_id.to_string(), serde_json::to_value(line).ok()?))
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

/// Where a projection came from. Serialized for the frontend's tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Basis {
    /// Completed game, projected before tipoff.
    Pregame,
    /// Upcoming game, current model.
    Model,
    /// Upcoming game with a team that has not played: preseason projection,
    /// no score (it carries no tempo).
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

    let (games, stored, upcoming, rating_inputs, (prev_date, next_date)) = tokio::try_join!(
        queries::get_schedule_games(pool, season, date),
        queries::get_date_game_projections(pool, season, date),
        queries::get_date_upcoming_projections(pool, season, date),
        queries::get_team_rating_inputs(pool, season),
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
    let mut anchors: HashMap<Uuid, f32> = HashMap::new();
    for t in &rating_inputs {
        played.insert(t.team_id, t.has_played);
        if let Some(d) = t.first_played {
            first_played.insert(t.team_id, d);
        }
        if let Some(p) = t.preseason_adj_em {
            anchors.insert(t.team_id, p);
        }
        if let Some(r) = watchability::team_rating(
            t.adj_em,
            t.preseason_adj_em.map(f64::from),
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

        if let Some(p) = stored.get(&g.game_id).filter(|_| is_played) {
            predictions[idx] = Some(Prediction::from_stored(p, home_id, Basis::Pregame));
            continue;
        }
        if let Some(p) = upcoming.get(&g.game_id) {
            let basis = if is_played {
                Basis::Pregame
            } else {
                Basis::Model
            };
            predictions[idx] = Some(Prediction::from_stored(p, home_id, basis));
            continue;
        }
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
            // A team's first game has no stored projection: the nightly's
            // sweep projects from the games before it, and there were none.
            // Before tipoff this page showed the preseason projection, so
            // that is the pre-game number, and it keeps the game's
            // watchability rather than dropping both at the final whistle.
            let debut = |id: Uuid| first_played.get(&id).is_none_or(|&d| d >= g.game_date);
            if debut(home_id) || debut(away_id) {
                predictions[idx] = preseason();
            }
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
