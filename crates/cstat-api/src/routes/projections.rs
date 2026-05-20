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
use cstat_core::roster_impact::{apply_projected_cam_v3, build_roster_impact_features};
use cstat_core::roster_projection::{
    DraftScenario, FreshmanTier, ProjectedRoster, compose_all_projections, load_draft_entrants,
    load_mock_draft, normalize_player_name, project_returner_cam_v3,
};
use cstat_core::trajectory::{
    TRAJECTORY_NUM_FEATURES, build_trajectory_features, fetch_player_trajectory_rows,
    fetch_trajectory_oof,
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

/// Minimum (returning + arrivals + recruits) count we'll score. Below
/// this the roster is too sparse to be a real rotation — a 1–6 player
/// roster can't be honestly projected — so smaller rosters return null
/// predictions and a `too_thin = true` flag, letting the UI render them
/// honestly (an explanation chip) instead of hiding the row.
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
    /// Probability-weighted blend of `ceiling` and `floor`:
    /// `p̄·ceiling + (1−p̄)·floor`, where `p̄` is the mean chance the
    /// uncertain (declared-draft) cohort returns (see
    /// `mean_return_probability`). Collapses to the common value when
    /// there are no uncertain players. The headline sortable number;
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
    /// The team's *actual* `adj_efficiency_margin` for the target season
    /// (`year` itself). `None` for the live/upcoming forecast year (not
    /// played yet) and for any team without a target-season row. Lets
    /// the historical projection view render a "Projected vs Actual"
    /// accuracy column — a user-facing backtest of a past forecast.
    actual_adj_em: Option<f32>,
}

/// Weight on the base-season AdjEM when blending it with the Phase B
/// impact-aggregation model's raw projection. Tuned on the end-to-end
/// `cstat-ingest projections-backtest` against actual 2025 + 2026 AdjEM
/// (496 pooled team-years; see `docs/projections_methodology.md`).
///
/// Phase B's raw output is a far stronger projector than the old
/// box-score model — raw MAE 6.58 vs the box-score model's 9.97 — so
/// the blend leans much less on baseline persistence than Phase A did
/// (`0.55` vs the old `0.80`). The MAE curve is flat across 0.50–0.60;
/// `0.55` is the backtest optimum (blended MAE 5.88, beating both
/// baseline-persistence 6.53 and the old Phase A pipeline 6.23).
const SHRINK_WEIGHT: f32 = 0.55;

/// Additive calibration offset applied to the blended projection.
///
/// **Zero under Phase B.** Phase A needed `+2.0` because the box-score
/// roster model ran a structural −4.8 low (it never saw freshman upside
/// or returner growth). The Phase B model consumes *projected* cam_v3
/// directly, so its raw output is near-unbiased (+0.44) and the blended
/// pipeline's residual bias is ≈−0.25 — within backtest noise. The
/// offset is kept as a named `0.0` knob so the methodology doc's
/// re-tuning playbook (grid-search weight *and* offset) stays valid.
const PROJECTION_OFFSET: f32 = 0.0;

/// Blend the raw model output with the baseline AdjEM and apply the
/// calibration offset. With no baseline (e.g. a brand-new D-I program)
/// the blend collapses to the offset-corrected raw value.
fn shrink(raw: f32, baseline: Option<f32>) -> f32 {
    match baseline {
        Some(b) => SHRINK_WEIGHT * b + (1.0 - SHRINK_WEIGHT) * raw + PROJECTION_OFFSET,
        None => raw + PROJECTION_OFFSET,
    }
}

/// P(a declared-for-draft player withdraws and returns to college),
/// keyed off their Tankathon mock-draft pick. A projected top-30 pick
/// almost never withdraws; a second-round projection is a genuine
/// toss-up; a declared player absent from the 60-pick board most
/// likely returns. `None` = declared but off the board.
fn return_probability_from_pick(pick: Option<i32>) -> f32 {
    match pick {
        Some(p) if p <= 30 => 0.05,
        Some(_) => 0.50,
        None => 0.85,
    }
}

/// Mean return probability across a team's uncertain (declared-draft)
/// cohort. Used to probability-weight the floor/ceiling midpoint — a
/// flat 50/50 average over-penalizes exactly the draft-talent-heavy
/// (i.e. top) teams. Returns `0.5` for an empty cohort, where it's
/// unused anyway (floor == ceiling, so the weight cancels).
fn mean_return_probability(
    p: &ProjectedRoster,
    mock_by_name: &std::collections::HashMap<String, (i32, String)>,
) -> f32 {
    if p.uncertain.is_empty() {
        return 0.5;
    }
    let sum: f32 = p
        .uncertain
        .iter()
        .map(|(_, u)| {
            let pick = mock_by_name
                .get(&normalize_player_name(&u.name))
                .map(|(pick, _)| *pick);
            return_probability_from_pick(pick)
        })
        .sum();
    sum / p.uncertain.len() as f32
}

/// Load the Tankathon mock-draft snapshot for `base_season` into a
/// `normalized-name → (pick, team)` map. Best-effort: a missing or
/// malformed file yields an empty map and callers degrade gracefully
/// (no `?`-row chips; the floor/ceiling midpoint falls back to a flat
/// 50/50 average).
fn load_mock_by_name(base_season: i32) -> std::collections::HashMap<String, (i32, String)> {
    let mock_path = PathBuf::from("data/draft").join(format!("{base_season}_mock_draft.json"));
    load_mock_draft(&mock_path)
        .map(|md| {
            md.picks
                .into_iter()
                .map(|p| (normalize_player_name(&p.name), (p.pick, p.team)))
                .collect()
        })
        .unwrap_or_default()
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

    let projections =
        compose_all_projections(&state.db.pool, base_season, &entrants, &state.predictor)
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

    // Actual target-season AdjEM, keyed by base-season team_id. Empty
    // for the live forecast year (target season not played yet) — those
    // teams get `actual_adj_em = None`. Powers the historical view's
    // "Projected vs Actual" accuracy column.
    let actual_map = fetch_actual_adj_em(&state.db.pool, base_season, year)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("actual-AdjEM fetch failed: {e}") })),
            )
        })?;

    // Tankathon mock-draft snapshot — used to probability-weight each
    // team's floor/ceiling midpoint (see `mean_return_probability`).
    let mock_by_name = load_mock_by_name(base_season);

    // Forward-project each returner / arrival's cam_v3 in one batched
    // pass across every team — the Phase B model scores rosters of
    // *projected* cam_v3 (recruits already carry the freshman model's
    // value). One trajectory fetch + inference for the whole slate; a
    // failure logs and degrades to current-season cam_v3.
    let mut traj_ids: Vec<Uuid> = Vec::new();
    for p in &projections {
        traj_ids.extend(p.returning.iter().map(|r| r.player_id));
        traj_ids.extend(p.arrivals.iter().map(|a| a.player_id));
        traj_ids.extend(p.uncertain.iter().map(|(row, _)| row.player_id));
    }
    let projected_cam = project_returner_cam_v3(
        &state.db.pool,
        &state.predictor,
        &traj_ids,
        base_season,
        year,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            error = %e,
            "trajectory cam_v3 projection failed; projecting on current-season cam_v3",
        );
        std::collections::HashMap::new()
    });

    let mut rows: Vec<ProjectedTeam> = Vec::with_capacity(projections.len());
    for p in &projections {
        let baseline = baseline_map.get(&p.team_id).copied();
        let actual = actual_map.get(&p.team_id).copied();
        let p_return = mean_return_probability(p, &mock_by_name);
        let Some(row) = predict_team(
            p,
            &state.predictor,
            baseline,
            actual,
            p_return,
            &projected_cam,
        ) else {
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
    actual: Option<f32>,
    p_return: f32,
    projected_cam: &std::collections::HashMap<Uuid, f64>,
) -> Option<ProjectedTeam> {
    // Recruits count toward the qualifying-size gate: a returners-thin
    // team with a strong freshman class (e.g. Duke with 4 incoming
    // 5-stars) is no longer "too thin to project". The Phase B model
    // sees them via build_roster_impact_features just like returners.
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
        midpoint_adj_em: floor
            .zip(ceiling)
            .map(|(f, c)| p_return * c + (1.0 - p_return) * f),
        returning_count: p.returning.len(),
        arrivals_count: p.arrivals.len(),
        recruits_count: p.recruits.len(),
        recruits_by_tier: recruits_by_tier.clone(),
        top_recruits: top_recruits.clone(),
        uncertain_count: p.uncertain.len(),
        departures_count: p.departures.len(),
        too_thin,
        baseline_adj_em: baseline,
        actual_adj_em: actual,
    };

    if qualifying < MIN_QUALIFYING_FOR_PROJECTION {
        // Below the gate — the model can't honestly project a 1-6
        // qualifying-player roster (no freshmen / recruits modeled, so
        // the rate-stat aggregates over-weight the few starters). Surface
        // the row with metadata so the UI can show "—" and a tooltip.
        return Some(base(None, None, true));
    }

    // Score each scenario with the Phase B impact-aggregation model.
    // Overwrite each returner / arrival's `cam_v3` with the trajectory
    // model's projection (recruits already carry the freshman model's
    // value from `synthesize_freshman_row`); `build_roster_impact_features`
    // then does its own cam_v3-ranked canonical-MPG rotation
    // normalization — no separate `project_rotation` pass needed.
    let score = |scenario| {
        let mut roster = p.for_scenario(scenario);
        apply_projected_cam_v3(&mut roster, projected_cam);
        predictor.predict_roster_impact(&build_roster_impact_features(&roster))
    };
    let floor_raw = match score(DraftScenario::Floor) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(team = %p.team_name, error = ?e, "floor predict failed");
            return None;
        }
    };
    let ceiling_raw = match score(DraftScenario::Ceiling) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(team = %p.team_name, error = ?e, "ceiling predict failed");
            return None;
        }
    };

    // Baseline-shrink both bounds. The band shrinks in width by
    // `(1 - SHRINK_WEIGHT)` but stays internally consistent (ceiling ≥
    // floor preserved for non-anomaly teams; negative-spread anomalies
    // stay negative-spread).
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
    // Match the list route's behavior: missing-file failures are logged
    // and the projection proceeds with an empty cohort (so a single
    // team page doesn't 500 just because the draft list is unavailable).
    let entrants = load_draft_entrants(&entrants_path).unwrap_or_else(|e| {
        tracing::warn!(
            path = %entrants_path.display(),
            error = %e,
            "draft entrants file unavailable; projecting without draft cohort",
        );
        vec![]
    });
    let projections = compose_all_projections(pool, base_season, &entrants, &state.predictor)
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

    // Actual target-season AdjEM for this team (None for the live year).
    let actual = fetch_actual_adj_em(pool, base_season, year)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("actual-AdjEM fetch failed: {e}") })),
            )
        })?
        .get(&resolved_id)
        .copied();

    // Tankathon mock-draft snapshot — drives both the floor/ceiling
    // midpoint weighting (via `mean_return_probability`) and the
    // informational `?`-row chips further down.
    let mock_by_name = load_mock_by_name(base_season);
    let p_return = mean_return_probability(&projection, &mock_by_name);

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

    // Phase 5c trajectory projections — predicted next-season CamPom
    // for every returner / arrival / uncertain on this team. One batch
    // SQL fetch (across all matched player_ids), one batched
    // `predict_trajectory_batch` call. Failure (fetch or inference)
    // logs once at warn and serves NULL projections route-wide; the
    // frontend's null branch renders only the current-season chip. The
    // single page lifts the per-row latency the transfer-portal
    // experience proved out in PR 2.
    //
    // Uncertain players are batched alongside returning because they
    // *are* returners under the ceiling scenario — projecting them
    // gives the UI a "if they withdraw and stay, here's what they'd
    // contribute" number that pairs naturally with the band.
    let mut traj_ids: Vec<Uuid> = Vec::new();
    for r in &projection.returning {
        traj_ids.push(r.player_id);
    }
    for a in &projection.arrivals {
        traj_ids.push(a.player_id);
    }
    for (row, _) in &projection.uncertain {
        traj_ids.push(row.player_id);
    }
    // Departures also get projected so the UI can render "if they had
    // stayed, we'd have projected X" alongside the current cam_v3 chip.
    // Applies to all kinds — seniors (counterfactual), transfers (used
    // by their *destination* team's roster but the chip lives on the
    // source row for context), and NBA-draft departures.
    for d in &projection.departures {
        let pid = match d {
            cstat_core::roster_projection::DepartureReason::GraduatedSenior {
                player_id, ..
            }
            | cstat_core::roster_projection::DepartureReason::Transferred { player_id, .. }
            | cstat_core::roster_projection::DepartureReason::DraftGone { player_id, .. } => {
                *player_id
            }
        };
        traj_ids.push(pid);
    }
    // Precedence: OOF (LOPO held-out) predictions first for any player
    // whose torvik_pid has a row in `trajectory_oof_predictions` at
    // target_season = year. For historical years (target_season the
    // model trained on) this serves honest held-out projections; for
    // the forward year (current year + 1) the OOF table is empty and
    // everything falls through to live inference. See ROADMAP §"Serve
    // held-out trajectory/freshman predictions for historical years".
    let traj_predictions: std::collections::HashMap<
        Uuid,
        cstat_core::trajectory::TrajectoryPrediction,
    > = if traj_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        let mut acc = fetch_trajectory_oof(pool, &traj_ids, year)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = ?e,
                    target_season = year,
                    n = traj_ids.len(),
                    "trajectory OOF lookup failed for projection team detail; falling through to live inference",
                );
                std::collections::HashMap::new()
            });
        let need_live: Vec<Uuid> = traj_ids
            .iter()
            .filter(|pid| !acc.contains_key(*pid))
            .copied()
            .collect();
        if !need_live.is_empty() {
            match fetch_player_trajectory_rows(pool, &need_live, base_season).await {
                Ok(row_map) => {
                    let mut ids: Vec<Uuid> = Vec::new();
                    let mut feature_vectors: Vec<[f32; TRAJECTORY_NUM_FEATURES]> = Vec::new();
                    for (pid, row) in row_map {
                        ids.push(pid);
                        feature_vectors.push(build_trajectory_features(&row, base_season));
                    }
                    match state.predictor.predict_trajectory_batch(&feature_vectors) {
                        Ok(preds) => {
                            for (pid, pred) in ids.into_iter().zip(preds) {
                                acc.insert(pid, pred);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                year,
                                n = feature_vectors.len(),
                                "trajectory batch predict failed for projection team detail; \
                                 serving NULL projections for live-inference cohort",
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        year,
                        n = need_live.len(),
                        "trajectory features fetch failed for projection team detail; \
                         serving NULL projections for live-inference cohort",
                    );
                }
            }
        }
        acc
    };

    // Phase B scoring input: trajectory means for returners / arrivals
    // (recruits already carry the freshman model's value on their
    // synthesized PlayerRow). Reuses the OOF-first `traj_predictions`
    // computed just above rather than re-fetching — the detail route
    // needs the full band for display anyway, and `predict_team` only
    // needs the mean.
    let projected_cam: std::collections::HashMap<Uuid, f64> = traj_predictions
        .iter()
        .map(|(pid, pred)| (*pid, pred.mean as f64))
        .collect();

    // `predict_team` returns None only when the ONNX session errors —
    // the too-thin gate still returns Some with null bounds. The list
    // route skips None rows (`continue`); a single-team detail page
    // can't skip, so surface it as 500 rather than handing the
    // frontend a `projection: null` it isn't typed for.
    let Some(row) = predict_team(
        &projection,
        &state.predictor,
        baseline,
        actual,
        p_return,
        &projected_cam,
    ) else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("inference failed for team {resolved_id}")
            })),
        ));
    };

    // Serialize each cohort with names + per-player projections. `cam_v3`
    // is the source-season (current) CamPom; `projected_campom_*` is
    // the next-season forecast. Recruits don't have a "current" so
    // their `projected_cam_v3` (= synthesized PlayerRow's cam_v3 =
    // freshman model mean) is the headline; `projected_campom_lower/upper`
    // carry the band.
    let serialize_proj = |pid: &Uuid| -> (Option<f32>, Option<f32>, Option<f32>) {
        match traj_predictions.get(pid) {
            Some(p) => (Some(p.mean), Some(p.lower), Some(p.upper)),
            None => (None, None, None),
        }
    };
    let returning_json: Vec<Value> = projection
        .returning
        .iter()
        .map(|r| {
            let (mean, lower, upper) = serialize_proj(&r.player_id);
            json!({
                "player_id": r.player_id,
                "name": names.get(&r.player_id).cloned().unwrap_or_else(|| "(unknown)".to_string()),
                "mpg": r.mpg,
                "ppg": r.ppg,
                "cam_v3": r.cam_v3,
                "primary_class": r.primary_class,
                "projected_campom_mean": mean,
                "projected_campom_lower": lower,
                "projected_campom_upper": upper,
            })
        })
        .collect();
    let arrivals_json: Vec<Value> = projection
        .arrivals
        .iter()
        .map(|a| {
            let source = arrival_sources.get(&a.player_id);
            let (mean, lower, upper) = serialize_proj(&a.player_id);
            json!({
                "player_id": a.player_id,
                "name": names.get(&a.player_id).cloned().unwrap_or_else(|| "(unknown)".to_string()),
                "source_team_id": source.map(|(tid, _)| *tid),
                "source_team_name": source.map(|(_, n)| n.clone()),
                "mpg": a.mpg,
                "ppg": a.ppg,
                "cam_v3": a.cam_v3,
                "primary_class": a.primary_class,
                "projected_campom_mean": mean,
                "projected_campom_lower": lower,
                "projected_campom_upper": upper,
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
                "position": meta.position,
                "projected_cam_v3": row.cam_v3,
                "projected_campom_lower": meta.projected_campom_lower,
                "projected_campom_upper": meta.projected_campom_upper,
            })
        })
        .collect();
    // Enrich departures with the player's base_season archetype + MPG +
    // CamPom v3 so the row visually matches Returning / Arrivals
    // (name · archetype · MPG · CamPom on the right). One batched query
    // rather than N+1: gather every departure's player_id, join the
    // base_season slices of player_archetypes / player_season_stats /
    // torvik_player_stats in one shot, then weave the result back into
    // each departure's JSON.
    //
    // Contract: the player_ids in `projection.departures` are *base_season*
    // pids — for graduating seniors and draft entrants this is obvious
    // (they only exist in base_season), and for outbound transfers the
    // resolver in `roster_projection.rs` keys on
    // `transfers.cstat_player_id` which is resolved against the *source*
    // (base_season) roster, not the destination. The base_season-bound
    // join below depends on that. If the transfer ingest ever starts
    // resolving cstat_player_id against the destination season, this
    // query silently returns all-NULL meta and the UI degrades to plain
    // text — pinned via the comment so the breakage surfaces here, not
    // in a mysterious "where did the archetype chip go" bug report.
    let departure_pids: Vec<Uuid> = projection
        .departures
        .iter()
        .map(|d| match d {
            cstat_core::roster_projection::DepartureReason::GraduatedSenior {
                player_id, ..
            }
            | cstat_core::roster_projection::DepartureReason::Transferred { player_id, .. }
            | cstat_core::roster_projection::DepartureReason::DraftGone { player_id, .. } => {
                *player_id
            }
        })
        .collect();
    struct DepartureMeta {
        primary_class: Option<String>,
        mpg: Option<f64>,
        cam_v3: Option<f64>,
    }
    let departure_meta: std::collections::HashMap<Uuid, DepartureMeta> =
        if departure_pids.is_empty() {
            std::collections::HashMap::new()
        } else {
            sqlx::query_as::<_, (Uuid, Option<String>, Option<f64>, Option<f64>)>(
                "SELECT p.id,
                        pa.primary_class,
                        pss.minutes_per_game,
                        tps.cam_gbpm_v3_psos
                 FROM players p
                 LEFT JOIN player_archetypes pa
                     ON pa.player_id = p.id AND pa.season = $1
                 LEFT JOIN player_season_stats pss
                     ON pss.player_id = p.id AND pss.season = $1
                 LEFT JOIN torvik_player_stats tps
                     ON tps.player_id = p.id AND tps.season = $1
                 WHERE p.id = ANY($2)",
            )
            .bind(base_season)
            .bind(&departure_pids)
            .fetch_all(pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("departure enrichment failed: {e}") })),
                )
            })?
            .into_iter()
            .map(|(pid, primary_class, mpg, cam_v3)| {
                (
                    pid,
                    DepartureMeta {
                        primary_class,
                        mpg,
                        cam_v3,
                    },
                )
            })
            .collect()
        };
    let departures_json: Vec<Value> = projection
        .departures
        .iter()
        .map(|d| {
            let (pid, kind, name, destination, destination_team_id) = match d {
                cstat_core::roster_projection::DepartureReason::GraduatedSenior {
                    player_id,
                    name,
                } => (*player_id, "senior", name.clone(), None, None),
                cstat_core::roster_projection::DepartureReason::Transferred {
                    player_id,
                    name,
                    destination,
                    destination_team_id,
                } => (
                    *player_id,
                    "transferred",
                    name.clone(),
                    destination.clone(),
                    *destination_team_id,
                ),
                cstat_core::roster_projection::DepartureReason::DraftGone { player_id, name } => {
                    (*player_id, "draft_gone", name.clone(), None, None)
                }
            };
            let meta = departure_meta.get(&pid);
            let (mean, lower, upper) = serialize_proj(&pid);
            json!({
                "kind": kind,
                "player_id": pid,
                "name": name,
                "prior_season": base_season,
                "primary_class": meta.and_then(|m| m.primary_class.clone()),
                "mpg": meta.and_then(|m| m.mpg),
                "cam_v3": meta.and_then(|m| m.cam_v3),
                "projected_campom_mean": mean,
                "projected_campom_lower": lower,
                "projected_campom_upper": upper,
                "destination": destination,
                "destination_team_id": destination_team_id,
            })
        })
        .collect();
    let uncertain_json: Vec<Value> = projection
        .uncertain
        .iter()
        .map(|(row, meta)| {
            let (mean, lower, upper) = serialize_proj(&meta.player_id);
            let mock_hit = mock_by_name.get(&normalize_player_name(&meta.name));
            json!({
                "player_id": meta.player_id,
                "name": meta.name,
                "reason": meta.reason,
                "mpg": row.mpg,
                "cam_v3": row.cam_v3,
                "primary_class": row.primary_class,
                "projected_campom_mean": mean,
                "projected_campom_lower": lower,
                "projected_campom_upper": upper,
                "mock_pick": mock_hit.map(|(p, _)| *p),
                "mock_team": mock_hit.map(|(_, t)| t.clone()),
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

/// Pull the *target* season's actual `adj_efficiency_margin`, keyed by
/// the **base-season** team_id. UUIDs are season-scoped, so a
/// base-season `team_id` (what `ProjectedRoster` carries) can't index a
/// target-season-keyed map directly — the join bridges the two via the
/// cross-season `natstat_id`. Returns an empty map for the live forecast
/// year (the target season has no `team_season_stats` rows yet), which
/// surfaces as `actual_adj_em = None` per team.
async fn fetch_actual_adj_em(
    pool: &sqlx::PgPool,
    base_season: i32,
    target_season: i32,
) -> Result<std::collections::HashMap<Uuid, f32>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        base_team_id: Uuid,
        adj_efficiency_margin: f64,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT t_base.id AS base_team_id, tss.adj_efficiency_margin
        FROM teams t_base
        JOIN teams t_tgt
          ON t_tgt.natstat_id = t_base.natstat_id AND t_tgt.season = $2
        JOIN team_season_stats tss
          ON tss.team_id = t_tgt.id AND tss.season = $2
        WHERE t_base.season = $1 AND tss.adj_efficiency_margin IS NOT NULL
        "#,
    )
    .bind(base_season)
    .bind(target_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.base_team_id, r.adj_efficiency_margin as f32))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_blends_raw_with_baseline_and_offset() {
        let raw = 30.0_f32;
        let baseline = 25.0_f32;
        let expected = SHRINK_WEIGHT * baseline + (1.0 - SHRINK_WEIGHT) * raw + PROJECTION_OFFSET;
        assert!((shrink(raw, Some(baseline)) - expected).abs() < 1e-5);
    }

    #[test]
    fn shrink_offsets_raw_when_baseline_missing() {
        // No baseline (new D-I program) → offset-corrected raw, no anchor.
        let raw = 12.3_f32;
        assert!((shrink(raw, None) - (raw + PROJECTION_OFFSET)).abs() < 1e-5);
    }

    #[test]
    fn shrink_is_monotonic_in_raw() {
        // The blend preserves ordering: a higher raw projection always
        // yields a higher shrunk output (so floor ≤ ceiling survives).
        assert!(shrink(50.0, Some(25.0)) > shrink(20.0, Some(25.0)));
    }

    #[test]
    fn return_probability_buckets_by_pick() {
        assert!((return_probability_from_pick(Some(1)) - 0.05).abs() < 1e-6);
        assert!((return_probability_from_pick(Some(30)) - 0.05).abs() < 1e-6);
        assert!((return_probability_from_pick(Some(31)) - 0.50).abs() < 1e-6);
        assert!((return_probability_from_pick(Some(60)) - 0.50).abs() < 1e-6);
        // Declared but off the 60-pick board → most likely returns.
        assert!((return_probability_from_pick(None) - 0.85).abs() < 1e-6);
    }
}
