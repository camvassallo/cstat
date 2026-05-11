//! 2027-and-beyond roster projections. Floor/ceiling bands per team
//! built from the prior-season roster minus departures plus incoming
//! portal commits — see `cstat_core::roster_projection` for the
//! composition logic and v1 honesty caveats.

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::roster_features::build_roster_features;
use cstat_core::roster_projection::{
    DraftScenario, ProjectedRoster, compose_all_projections, load_draft_entrants,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/projections/{year}", get(projection_list))
}

/// Minimum (returning + arrivals) count we'll score. Below this, the
/// projection is dominated by the few qualifying players' rate stats
/// and the model produces wildly optimistic AdjEM (the v1 spot-check
/// surfaced this — see ROADMAP §5b's "thin roster" caveat). Smaller
/// rosters return null predictions and a `too_thin = true` flag so the
/// UI can render them honestly without hiding the row.
const MIN_QUALIFYING_FOR_PROJECTION: usize = 7;

/// Shape returned to the frontend. One row per team; ranked by midpoint
/// AdjEM by default (route returns them sorted so JSON-fetch consumers
/// see a sensible default order). `floor` and `ceiling` are the model
/// predictions under the two NBA-draft scenarios — see
/// `cstat_core::roster_projection::DraftScenario`. All three prediction
/// fields are `null` when the roster fails the qualifying-size gate.
#[derive(Serialize)]
struct ProjectedTeam {
    team_id: Uuid,
    team_name: String,
    team_full_name: String,
    /// Headline AdjEM at the optimistic bound (all declared draft
    /// entrants withdraw and return). `None` for too-thin rosters.
    ceiling_adj_em: Option<f32>,
    /// Headline AdjEM at the conservative bound (all declared draft
    /// entrants are gone). `None` for too-thin rosters.
    floor_adj_em: Option<f32>,
    /// `(ceiling + floor) / 2`. A summary number for sortable display;
    /// `None` for too-thin rosters.
    midpoint_adj_em: Option<f32>,
    /// Count of qualifying returning players (excludes Sr, outbound
    /// portal, firm draft departures, and uncertain draft cohort).
    returning_count: usize,
    /// Count of incoming portal arrivals committed to this team.
    arrivals_count: usize,
    /// Count of players in the uncertain bucket — the spread
    /// (ceiling - floor) is roughly proportional to this.
    uncertain_count: usize,
    /// Count of recorded departures (Sr + outbound + firm draft-gone).
    departures_count: usize,
    /// True when `returning + arrivals < MIN_QUALIFYING_FOR_PROJECTION`.
    /// Frontend should render an explanation chip rather than the
    /// (null) prediction columns when this is set.
    too_thin: bool,
    /// The team's `adj_efficiency_margin` from the base season
    /// (= year - 1; what we'd casually call "last year's AdjEM").
    /// Used as the shrinkage anchor and as the reference point for
    /// the UI's "Δ vs last" column. `None` when the team has no
    /// base-season row (rare; new D-I program, or team that didn't
    /// finish enough games to compute AdjEM).
    baseline_adj_em: Option<f32>,
}

/// Weight given to the base-season AdjEM when shrinking the model's
/// projection toward last year's number. `0.5` means "halfway between
/// what the model says about the projected roster and what the team
/// actually was a year ago" — a Bayesian-style prior that next-season
/// AdjEM correlates strongly with current-season AdjEM (true
/// empirically; teams don't usually move by more than ±15 AdjEM
/// year-over-year). Without shrinkage the v1 spot-check produced
/// implausible swings — Auburn 41.7 → 14.4 (−27 in one year), West
/// Virginia 20.4 → −22 — driven by the model not seeing freshmen /
/// growth. Half-weight on last year tames those swings to about half
/// magnitude. When freshmen + growth lands (Phase 5c), this weight
/// can be reduced.
const BASELINE_SHRINK_WEIGHT: f32 = 0.5;

/// Apply the shrinkage anchor: returns the convex combination of the
/// raw model output and the baseline AdjEM. Returns the raw value
/// unchanged when no baseline is available (e.g. new D-I program).
fn shrink(model_value: f32, baseline: Option<f32>) -> f32 {
    match baseline {
        Some(b) => BASELINE_SHRINK_WEIGHT * b + (1.0 - BASELINE_SHRINK_WEIGHT) * model_value,
        None => model_value,
    }
}

async fn projection_list(
    State(state): State<Arc<AppState>>,
    Path(year): Path<i32>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !(2025..=2030).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "year out of range — projections supported for 2025–2030",
            })),
        ));
    }
    // `year` is the target season (the upcoming one we're projecting
    // *for*). The base season we compose from is the prior cstat
    // season; the portal class that fills it is keyed by `year - 1`
    // (the spring-of-year-1 portal cycle that moves players into
    // the target season).
    let base_season = year - 1;

    // Load the declared/gone NBA draft entrants for the matching
    // spring cycle. Missing-file failures are logged and the projection
    // proceeds with an empty cohort (every player who isn't a Sr or in
    // the portal returns) — partial coverage is better than 500.
    let entrants_path =
        PathBuf::from("data/draft").join(format!("{}_early_entrants.json", base_season));
    let entrants = match load_draft_entrants(&entrants_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %entrants_path.display(),
                error = %e,
                "draft entrants file unavailable; projecting without draft cohort",
            );
            vec![]
        }
    };

    let projections = compose_all_projections(&state.db.pool, base_season, &entrants)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("compose_all_projections failed: {e}") })),
            )
        })?;

    // Fetch the base-season AdjEM per team in one batch. Keyed by
    // team_id; missing entries (new D-I, no AdjEM) fall through to
    // `None` and skip shrinkage for that team.
    let baseline_map = fetch_baseline_adj_em(&state.db.pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("baseline fetch failed: {e}") })),
            )
        })?;

    let mut rows: Vec<ProjectedTeam> = Vec::with_capacity(projections.len());
    for p in &projections {
        let baseline = baseline_map.get(&p.team_id).copied();
        let Some(row) = predict_team(p, &state.predictor, baseline) else {
            continue;
        };
        rows.push(row);
    }

    // Rank by midpoint desc so the response is pre-sorted. Too-thin
    // rows (None midpoint) sort to the bottom regardless of direction;
    // negative-spread rows (declared cohort acts as a model net-drag)
    // keep their natural midpoint position.
    rows.sort_by(|a, b| match (a.midpoint_adj_em, b.midpoint_adj_em) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
    });

    Ok(Json(json!({
        "year": year,
        "base_season": base_season,
        "teams": rows,
        "total": projections.len(),
    })))
}

/// Run the model on a single projected roster (floor + ceiling) and
/// pack the result into a `ProjectedTeam`. Returns `None` for rosters
/// whose materialization is empty under both scenarios — those teams
/// can't be sensibly scored (no qualified players, e.g. a brand-new
/// D-I program in mid-transition). Errors from the ONNX session are
/// logged and treated as "skip this team" rather than 500-ing the
/// whole response.
fn predict_team(
    p: &ProjectedRoster,
    predictor: &cstat_core::inference::Predictor,
    baseline: Option<f32>,
) -> Option<ProjectedTeam> {
    let qualifying = p.returning.len() + p.arrivals.len();
    // Every team produces a row — too-thin rosters get null predictions
    // and a `too_thin = true` flag instead of being silently dropped.
    // This keeps "what happened to X?" auditable from the response.
    let base = |floor: Option<f32>, ceiling: Option<f32>, too_thin: bool| ProjectedTeam {
        team_id: p.team_id,
        team_name: p.team_name.clone(),
        team_full_name: p.team_full_name.clone(),
        ceiling_adj_em: ceiling,
        floor_adj_em: floor,
        midpoint_adj_em: floor.zip(ceiling).map(|(f, c)| (f + c) / 2.0),
        returning_count: p.returning.len(),
        arrivals_count: p.arrivals.len(),
        uncertain_count: p.uncertain.len(),
        departures_count: p.departures.len(),
        too_thin,
        baseline_adj_em: baseline,
    };

    if qualifying < MIN_QUALIFYING_FOR_PROJECTION {
        // Below the gate — the model can't honestly project a 1-6
        // qualifying-player roster (no freshmen / recruits modeled, so
        // the rate-stat aggregates over-weight the few starters). Surface
        // the row with metadata so the UI can show "—" and a tooltip.
        return Some(base(None, None, true));
    }

    let floor_roster = p.for_scenario(DraftScenario::Floor);
    let ceiling_roster = p.for_scenario(DraftScenario::Ceiling);
    let floor_feats = build_roster_features(&floor_roster);
    let ceiling_feats = build_roster_features(&ceiling_roster);

    let floor_raw = match predictor.predict_adj_em(&floor_feats) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(team = %p.team_name, error = ?e, "floor predict failed");
            return None;
        }
    };
    let ceiling_raw = match predictor.predict_adj_em(&ceiling_feats) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(team = %p.team_name, error = ?e, "ceiling predict failed");
            return None;
        }
    };

    // Apply baseline shrinkage to both bounds. The band shrinks in
    // width by `(1 - BASELINE_SHRINK_WEIGHT)` but stays internally
    // consistent (ceiling ≥ floor preserved for non-anomaly teams;
    // negative-spread anomalies stay negative-spread).
    Some(base(
        Some(shrink(floor_raw, baseline)),
        Some(shrink(ceiling_raw, baseline)),
        false,
    ))
}

/// Pull `team_season_stats.adj_efficiency_margin` for every team in
/// the base season. Keyed by team_id for O(1) lookup from
/// `predict_team`. Teams with NULL AdjEM (insufficient games / not
/// finished computing) get filtered out — they fall through to "no
/// baseline" and the route serves the raw model output.
async fn fetch_baseline_adj_em(
    pool: &sqlx::PgPool,
    base_season: i32,
) -> Result<std::collections::HashMap<Uuid, f32>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        team_id: Uuid,
        adj_efficiency_margin: f64,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT team_id, adj_efficiency_margin
        FROM team_season_stats
        WHERE season = $1 AND adj_efficiency_margin IS NOT NULL
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.team_id, r.adj_efficiency_margin as f32))
        .collect())
}
