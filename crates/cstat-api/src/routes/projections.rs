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
use cstat_core::inference::Predictor;
use cstat_core::roster_features::build_roster_features;
use cstat_core::roster_projection::{
    DraftScenario, FreshmanTier, ProjectedRoster, compose_all_projections, load_draft_entrants,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/projections/{year}", get(projection_list))
        .route(
            "/api/projections/{year}/teams/{team_id}",
            get(projection_team_detail),
        )
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
    /// Count of incoming HS recruits committed to this team. Each
    /// recruit is synthesized from a tier-mean freshman profile (see
    /// `FreshmanTier` in `roster_projection.rs`).
    recruits_count: usize,
    /// Per-tier breakdown of the recruit class, e.g. `{"t1": 1, "t2": 2}`.
    /// Surfaced separately so the UI can render "1× elite · 2× top-100"
    /// without re-counting client-side.
    recruits_by_tier: serde_json::Value,
    /// Up to the top 5 recruits by composite_rank for UI display. Each
    /// entry is `{name, composite_rank, star_rating, tier}` from
    /// `RecruitMeta`.
    top_recruits: Vec<serde_json::Value>,
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
    predictor: &Predictor,
    baseline: Option<f32>,
) -> Option<ProjectedTeam> {
    // Recruits count toward the qualifying-size gate: a returners-thin
    // team with a strong freshman class (e.g. Duke with 4 incoming
    // 5-stars) is no longer "too thin to project". The roster model
    // sees them via build_roster_features just like returners.
    let qualifying = p.returning.len() + p.arrivals.len() + p.recruits.len();

    // Per-tier counts for the UI breakdown chip.
    let mut tier_counts: [u32; 4] = [0; 4];
    for (_, meta) in &p.recruits {
        let idx = match meta.tier {
            FreshmanTier::T1 => 0,
            FreshmanTier::T2 => 1,
            FreshmanTier::T3 => 2,
            FreshmanTier::T4 => 3,
        };
        tier_counts[idx] += 1;
    }
    let recruits_by_tier = json!({
        "t1": tier_counts[0],
        "t2": tier_counts[1],
        "t3": tier_counts[2],
        "t4": tier_counts[3],
    });

    // Top 5 recruits by composite_rank (NULL ranks last). Cloned so the
    // closure capture is move-friendly.
    let mut sorted_recruits: Vec<_> = p.recruits.iter().map(|(_, m)| m.clone()).collect();
    sorted_recruits.sort_by(|a, b| match (a.composite_rank, b.composite_rank) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let top_recruits: Vec<serde_json::Value> = sorted_recruits
        .into_iter()
        .take(5)
        .map(|m| {
            json!({
                "name": m.name,
                "composite_rank": m.composite_rank,
                "star_rating": m.star_rating,
                "tier": m.tier,
            })
        })
        .collect();

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
        recruits_count: p.recruits.len(),
        recruits_by_tier: recruits_by_tier.clone(),
        top_recruits: top_recruits.clone(),
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

/// Single-team projection detail. Mirrors the list route's composition
/// pipeline but enriches each roster row with the player's `name` (and
/// stats already on `PlayerRow`) so the frontend can render a per-team
/// projected-roster view (Returning / Arrivals / Recruits / Departures)
/// without a second round-trip.
///
/// `team_id` may belong to any season (UUIDs are season-scoped, but we
/// resolve cross-season via `natstat_id` the same way `team_detail`
/// does). The projection is composed off the `base_season = year - 1`
/// teams row that matches the requested team_id's natstat_id.
async fn projection_team_detail(
    State(state): State<Arc<AppState>>,
    Path((year, team_id)): Path<(i32, Uuid)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !(2025..=2030).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "year out of range — projections supported for 2025–2030",
            })),
        ));
    }
    let base_season = year - 1;
    let pool = &state.db.pool;

    // Resolve the cross-season UUID first: if the URL carries last-
    // season's UUID for a team that exists in base_season, we'd 404
    // without the natstat_id lookup. Same helper team_detail uses.
    let resolved_id =
        match cstat_core::queries::resolve_team_id_for_season(pool, team_id, base_season)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("team resolution failed: {e}") })),
                )
            })? {
            Some(id) => id,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "error": format!("team not found in base_season {base_season}")
                    })),
                ));
            }
        };

    // Compose all projections for the year, then pick the requested team.
    // Composition is fast (one season's worth of fetches in 3 queries);
    // single-team filtering after the fact is simpler than carving out a
    // single-team code path that risks drifting from the list route.
    let entrants_path =
        PathBuf::from("data/draft").join(format!("{}_early_entrants.json", base_season));
    let entrants = load_draft_entrants(&entrants_path).unwrap_or_default();
    let projections = compose_all_projections(pool, base_season, &entrants)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("compose_all_projections failed: {e}") })),
            )
        })?;
    let Some(projection) = projections.into_iter().find(|p| p.team_id == resolved_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!(
                    "no projection for team {resolved_id} (base season {base_season}); team may not have a qualified roster"
                )
            })),
        ));
    };

    // Look up the team's natstat_id so the frontend can build canonical
    // back-links into the played base season (e.g. clicking the team
    // name on the projection page goes to the actual 2026 page).
    let team_meta: Option<(String, Option<String>)> =
        sqlx::query_as(r#"SELECT name, short_name FROM teams WHERE id = $1 LIMIT 1"#)
            .bind(resolved_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("team meta fetch failed: {e}") })),
                )
            })?;

    let baseline_map = fetch_baseline_adj_em(pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("baseline fetch failed: {e}") })),
            )
        })?;
    let baseline = baseline_map.get(&resolved_id).copied();

    let row = predict_team(&projection, &state.predictor, baseline);

    // Name lookup for the returning + arrival player_ids. PlayerRow is
    // stat-only (matches the roster model's input shape); names live on
    // the `players` table and we batch-fetch them here so the UI can
    // render a per-player roster without re-querying. Recruits already
    // carry their name on `RecruitMeta`.
    let mut player_ids: Vec<Uuid> = Vec::new();
    for r in &projection.returning {
        player_ids.push(r.player_id);
    }
    for a in &projection.arrivals {
        player_ids.push(a.player_id);
    }
    let names: std::collections::HashMap<Uuid, String> = if player_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (id) id, name
            FROM players
            WHERE id = ANY($1)
            "#,
        )
        .bind(&player_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("name fetch failed: {e}") })),
            )
        })?;
        rows.into_iter().collect()
    };

    // Look up source-team display names + UUIDs for arrivals. We carry
    // `player_id`, and `player_team` maps that → source team via the
    // base-season player_season_stats row. Arrivals link out to their
    // source-team page in the played base season (e.g. UCLA in 2025
    // for a 2025-portal transfer), per the cross-season link rule.
    let arrival_sources: std::collections::HashMap<Uuid, (Uuid, String)> =
        if projection.arrivals.is_empty() {
            std::collections::HashMap::new()
        } else {
            let arrival_pids: Vec<Uuid> = projection.arrivals.iter().map(|a| a.player_id).collect();
            let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
                r#"
            SELECT pss.player_id, t.id, COALESCE(t.short_name, t.name)
            FROM player_season_stats pss
            JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
            WHERE pss.season = $1 AND pss.player_id = ANY($2)
            "#,
            )
            .bind(base_season)
            .bind(&arrival_pids)
            .fetch_all(pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!("arrival-source fetch failed: {e}")
                    })),
                )
            })?;
            rows.into_iter()
                .map(|(pid, tid, tname)| (pid, (tid, tname)))
                .collect()
        };

    // Serialize each cohort with names attached. We keep stats off the
    // wire for v1 (the model's PlayerRow is hard to read for a user);
    // the next iteration can add per-player CamPom + minutes + position.
    let returning_json: Vec<Value> = projection
        .returning
        .iter()
        .map(|r| {
            json!({
                "player_id": r.player_id,
                "name": names.get(&r.player_id).cloned().unwrap_or_else(|| "(unknown)".to_string()),
                "mpg": r.mpg,
                "ppg": r.ppg,
                "cam_v3": r.cam_v3,
                "primary_class": r.primary_class,
            })
        })
        .collect();
    let arrivals_json: Vec<Value> = projection
        .arrivals
        .iter()
        .map(|a| {
            let source = arrival_sources.get(&a.player_id);
            json!({
                "player_id": a.player_id,
                "name": names.get(&a.player_id).cloned().unwrap_or_else(|| "(unknown)".to_string()),
                "source_team_id": source.map(|(tid, _)| *tid),
                "source_team_name": source.map(|(_, n)| n.clone()),
                "mpg": a.mpg,
                "ppg": a.ppg,
                "cam_v3": a.cam_v3,
                "primary_class": a.primary_class,
            })
        })
        .collect();
    let recruits_json: Vec<Value> = projection
        .recruits
        .iter()
        .map(|(row, meta)| {
            json!({
                "recruit_id": meta.recruit_id,
                "name": meta.name,
                "composite_rank": meta.composite_rank,
                "star_rating": meta.star_rating,
                "tier": meta.tier,
                "projected_cam_v3": row.cam_v3,
            })
        })
        .collect();
    let departures_json: Vec<Value> = projection
        .departures
        .iter()
        .map(|d| match d {
            cstat_core::roster_projection::DepartureReason::GraduatedSenior { player_id, name } => {
                json!({"kind": "senior", "player_id": player_id, "name": name})
            }
            cstat_core::roster_projection::DepartureReason::Transferred {
                player_id,
                name,
                destination,
            } => json!({
                "kind": "transferred",
                "player_id": player_id,
                "name": name,
                "destination": destination,
            }),
            cstat_core::roster_projection::DepartureReason::DraftGone { player_id, name } => {
                json!({"kind": "draft_gone", "player_id": player_id, "name": name})
            }
        })
        .collect();
    let uncertain_json: Vec<Value> = projection
        .uncertain
        .iter()
        .map(|(_, meta)| {
            json!({
                "player_id": meta.player_id,
                "name": meta.name,
                "reason": meta.reason,
            })
        })
        .collect();

    Ok(Json(json!({
        "year": year,
        "base_season": base_season,
        "team": {
            "id": resolved_id,
            "name": team_meta.as_ref().map(|(n, _)| n.clone()),
            "short_name": team_meta.as_ref().and_then(|(_, s)| s.clone()),
        },
        "projection": row,
        "returning": returning_json,
        "arrivals": arrivals_json,
        "recruits": recruits_json,
        "departures": departures_json,
        "uncertain": uncertain_json,
    })))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_is_50_50_midpoint_with_baseline() {
        // Model says +30, last year was +25 → display +27.5.
        let v = shrink(30.0, Some(25.0));
        assert!((v - 27.5).abs() < 1e-6);
    }

    #[test]
    fn shrink_passes_through_when_baseline_missing() {
        // No baseline (new D-I program) → raw model output unchanged.
        let v = shrink(12.3, None);
        assert!((v - 12.3).abs() < 1e-6);
    }

    #[test]
    fn shrink_preserves_negative_spread() {
        // Floor > Ceiling at the raw model layer (declared cohort is a
        // net drag); the shrinkage halves the spread but keeps the sign.
        let f = shrink(50.0, Some(25.0)); // 37.5
        let c = shrink(20.0, Some(25.0)); // 22.5
        assert!(c < f, "negative-spread anomaly should survive shrinkage");
        let raw_spread = 20.0_f32 - 50.0; // -30
        let shrunk_spread = c - f; // -15
        assert!((shrunk_spread - raw_spread / 2.0).abs() < 1e-5);
    }
}
