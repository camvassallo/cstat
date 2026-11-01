//! `GET /api/schedule?date=YYYY-MM-DD` — one day's slate for the Schedule tab
//! (issue #429): every game with its tip time, venue, team ranks, a projected
//! score and win probability, and a pregame watchability score.
//!
//! A day can hold ~150 games, so the page reads materialized projections
//! rather than running the model per game per view (the #266 lesson):
//! - **Completed** games: the nightly's `game_projections` row, the honest
//!   pre-game number. A game finished since the last nightly still has its
//!   `upcoming_game_projections` row, written before tipoff from inputs that
//!   did not include it, which is the same claim. A completed game with
//!   neither (a historical season nobody back-filled) is left unprojected.
//! - **Upcoming, both teams have played**: the nightly's
//!   `upcoming_game_projections` row. A game the nightly has not seen yet (added
//!   to the schedule since) is projected live, up to [`LIVE_FALLBACK_CAP`] per
//!   request, so a failed nightly degrades to a partly unprojected page rather
//!   than to ~150 live model runs per view.
//! - **Upcoming, either team has not played**: the preseason projection, a
//!   subtraction on two stored anchors. The model has no season stats for an
//!   unplayed team, so it cannot answer.

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
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
    Router::new().route("/api/schedule", get(schedule))
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
    // Game dates are Eastern, so "today's games" is today in Eastern: from 8pm
    // ET the UTC date is already tomorrow.
    let date = params.date.unwrap_or_else(cstat_ingest::today_eastern);
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
    let mut anchors: HashMap<Uuid, f32> = HashMap::new();
    for t in &rating_inputs {
        played.insert(t.team_id, t.has_played);
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
        if is_played {
            continue;
        }

        let both_played = played.get(&home_id).copied().unwrap_or(false)
            && played.get(&away_id).copied().unwrap_or(false);
        if !both_played {
            // Same arithmetic the team page and `/api/predict` serve, on the
            // same anchors, so the three cannot disagree. No score: a
            // strength difference carries no tempo.
            if let (Some(&h), Some(&a)) = (anchors.get(&home_id), anchors.get(&away_id)) {
                let venue = if g.is_neutral_site {
                    Venue::Neutral
                } else {
                    Venue::Home
                };
                let margin = projection::preseason_only_margin(h - a, venue);
                predictions[idx] = Some(Prediction {
                    margin: margin as f64,
                    home_win_prob: projection::preseason_only_win_prob(margin),
                    home_score: None,
                    away_score: None,
                    basis: Basis::Preseason,
                });
            }
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
