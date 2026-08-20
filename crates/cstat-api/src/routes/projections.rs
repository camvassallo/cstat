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
    DraftScenario, ProjectedRoster, UncertainCause, compose_all_projections, fetch_draft_entrants,
    fetch_player_departures, load_mock_draft, normalize_player_name, project_returner_cam_v3,
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
const MIN_QUALIFYING_FOR_PROJECTION: usize =
    cstat_core::roster_projection::MIN_QUALIFYING_FOR_PROJECTION;

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
    /// uncertain cohort returns (see
    /// `mean_return_probability`). Collapses to the common value when
    /// there are no uncertain players. The headline sortable number;
    /// `None` for too-thin rosters.
    midpoint_adj_em: Option<f32>,
    /// Projected next-season offensive efficiency (absolute ~105), from the
    /// AdjO half of the NET+SPLIT decomposition — blended over the same
    /// floor/ceiling scenarios and baseline-shrunk like `midpoint_adj_em`.
    /// Display-only descriptive band; the served net (`midpoint_adj_em`) is
    /// untouched. `None` for too-thin rosters.
    projected_adj_o: Option<f32>,
    /// Projected next-season defensive efficiency (absolute ~105), DERIVED
    /// as `projected_adj_o − midpoint_adj_em` so the split reconciles
    /// exactly to the served net (lower = better defense, KenPom
    /// convention). `None` for too-thin rosters.
    projected_adj_d: Option<f32>,
    /// Count of qualifying returning players (excludes Sr, outbound
    /// portal, firm draft departures, and the uncertain cohort).
    returning_count: usize,
    /// Σ base-season cam_v3 of the returning players (talent retained,
    /// measured by their prior-season production). The Future grid's
    /// "Returning" column surfaces this instead of the raw count.
    returning_cam_v3_sum: f32,
    /// Σ *projected* next-season cam_v3 of the returning players (the
    /// trajectory model's forecast, falling back to prior cam_v3 where
    /// the player didn't pass the trajectory gate). This is the forward
    /// frame the roster-flow ledger displays; `returning_cam_v3_sum`
    /// (prior) stays the denominator for the continuity % and the
    /// "prior → projected" tooltip.
    returning_projected_cam_v3_sum: f32,
    /// Count of incoming portal arrivals committed to this team.
    arrivals_count: usize,
    /// Σ base-season cam_v3 of the incoming portal arrivals (talent
    /// gained, measured by their prior-school production). The Future
    /// grid's "Incoming" column surfaces this instead of the raw count.
    arrivals_cam_v3_sum: f32,
    /// Σ *projected* next-season cam_v3 of the incoming arrivals (forward
    /// frame, same trajectory forecast as returners). Paired with the
    /// prior `arrivals_cam_v3_sum` so the ledger shows projected value
    /// while the % stays on the prior base.
    arrivals_projected_cam_v3_sum: f32,
    /// Count of incoming HS recruits committed to this team. Each
    /// recruit carries the freshman-impact model's per-recruit projected
    /// cam_v3 (see `freshman_row` in `roster_projection.rs`).
    recruits_count: usize,
    /// Σ *projected* freshman-season cam_v3 across the recruit class —
    /// the freshman-impact model's per-recruit point estimate (recruits
    /// have no prior season, so unlike the other cohorts this is a
    /// forward projection, not last-season production). The Future grid's
    /// "Recruits" column surfaces this instead of the raw count.
    recruits_cam_v3_sum: f32,
    /// Up to the top 5 recruits by composite_rank for UI display. Each
    /// entry is `{name, composite_rank, star_rating, tier}` from
    /// `RecruitMeta`.
    top_recruits: Vec<serde_json::Value>,
    /// Count of players in the uncertain bucket — the spread
    /// (ceiling - floor) is roughly proportional to this.
    uncertain_count: usize,
    /// Σ base-season cam_v3 of the uncertain cohort (declared-draft plus,
    /// since issue #220, unsettled 5-in-5 eligibility).
    /// They were on last season's roster, so this completes the
    /// "last season's roster value" base the ledger normalizes against:
    /// `base = returning + departures + uncertain` (all prior-season).
    uncertain_cam_v3_sum: f32,
    /// Count of recorded departures (Sr + outbound + firm draft-gone).
    departures_count: usize,
    /// Σ base-season cam_v3 across all departures (graduating seniors +
    /// outbound portal + firm draft-gone) — talent leaving the program.
    /// The Future grid's "Departures" column surfaces this instead of the
    /// raw count.
    departures_cam_v3_sum: f32,
    /// Per-cohort Σ of the base-season CamPom O/D halves (cam_o/cam_d,
    /// envelope-gated per player like every other serving surface), in the
    /// *prior-season* frame — there is no projected O/D split (the
    /// trajectory model forecasts net only), so these describe the O/D
    /// shape of the talent moving, not a forecast. Players whose split is
    /// gated (or who lack torvik coverage) contribute 0 to both halves,
    /// mirroring the cam_v3 sums' COALESCE convention. Recruits have no
    /// prior season, hence no recruit O/D pair.
    returning_cam_o_sum: f32,
    returning_cam_d_sum: f32,
    arrivals_cam_o_sum: f32,
    arrivals_cam_d_sum: f32,
    departures_cam_o_sum: f32,
    departures_cam_d_sum: f32,
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

    /// The baseline weight used in the served blend for this team:
    /// `midpoint ≈ baseline_weight·(last-yr AdjEM) + (1−baseline_weight)·roster`.
    /// The stable 0.45 for continuity rosters, ramping down toward 0.20 for
    /// roster-overhaul teams (low talent retained) — last season's result is a
    /// stale anchor when the roster turns over, so the blend leans on the roster
    /// projection. See `roster_projection::transition_shrink_weight`. Lets the UI
    /// flag "leaning on the new roster" and keeps the blend auditable.
    baseline_weight: f32,

    // --- Display-only coach grade. NEVER folded into any AdjEM field above. ---
    // A point-in-time backtest (`training/pit_cae_backtest.py`, 2026-06-03)
    // showed an additive coach term DOES beat the projection's noise floor
    // (+0.13 MAE) but FAILS a program-persistence null — a team-keyed term does
    // better (+0.18), so the lift is program-level projection bias, not
    // coaching. The served projection therefore stays roster-only; CAE is
    // surfaced here purely descriptively. See ROADMAP §6 / the
    // project_pit_cae_program_null finding. The projection math (`shrink`,
    // `predict_team`) must never read these fields.
    /// The coach leading this program into the target season (resolved via
    /// `coach_seasons`, preferring the target season, else the base season for
    /// the live forecast). `None` when unmatched.
    coach_id: Option<Uuid>,
    coach_name: Option<String>,
    /// Career EB-shrunk Coach-Above-Expectation (`coach_ratings.cae_shrunk`),
    /// positive when the program has historically beaten its roster projection
    /// under this coach. `None` when the coach has no career rating yet (a thin
    /// or entirely-unscored tenure).
    coach_cae_shrunk: Option<f64>,
    /// `n/(n+k)` credibility weight ∈ [0,1]; low = thin tenure, soft grade.
    coach_cae_reliability: Option<f64>,
    coach_n_seasons: Option<i32>,
    /// Did this coach differ from the program's prior-season coach? (coachdict
    /// `is_new_hc`). Drives the "New HC" badge on the Future tab. `None` = can't
    /// tell (no prior-season coachdict entry).
    coach_is_new_hc: Option<bool>,
    /// For a new hire, the coach's prior-season program (coachdict name) — e.g.
    /// "South Florida" for Bryan Hodgson → Providence. `None` for a first-time /
    /// promoted D-I coach with no prior coachdict row. Display-only.
    coach_prev_team: Option<String>,
}

/// Display-only coach CAE attached to a projected team (see `ProjectedTeam`'s
/// coach fields). Decorative — must not influence the projection.
struct CoachCae {
    coach_id: Uuid,
    coach_name: String,
    cae_shrunk: Option<f64>,
    reliability: Option<f64>,
    n_seasons: Option<i32>,
    is_new_hc: Option<bool>,
    prev_team: Option<String>,
}

// The stable baseline weight + tuning history live on
// `cstat_core::roster_projection::PROJECTION_SHRINK_WEIGHT`; `predict_team`
// derives the per-team weight from `transition_shrink_weight` (lower for
// roster-overhaul teams), so there's no fixed local weight to alias here.

/// Additive calibration offset applied to the blended projection.
///
/// **Zero under roster-impact.** box-score needed `+2.0` because the box-score
/// roster model ran a structural −4.8 low (it never saw freshman upside
/// or returner growth). The roster-impact v2 model consumes *projected* cam_v3
/// directly, so its raw output is near-unbiased (+0.62) and the blended
/// pipeline's residual bias at `SHRINK_WEIGHT` is ≈−0.10 — within
/// backtest noise. The offset is kept as a named `0.0` knob so the
/// methodology doc's re-tuning playbook (grid-search weight *and*
/// offset) stays valid.
const PROJECTION_OFFSET: f32 = cstat_core::roster_projection::PROJECTION_OFFSET;

/// Blend the raw model output with the baseline AdjEM at an explicit baseline
/// `weight` and apply the calibration offset. With no baseline (e.g. a
/// brand-new D-I program) the blend collapses to the offset-corrected raw
/// value. `weight` is the stable [`SHRINK_WEIGHT`] for continuity rosters and
/// lower for overhaul rosters (see
/// [`cstat_core::roster_projection::transition_shrink_weight`]) — the shared
/// `score_projection_adj_em` derives the same weight from the same roster, so
/// this route and `compute-projections` never diverge.
fn shrink(raw: f32, baseline: Option<f32>, weight: f32) -> f32 {
    match baseline {
        Some(b) => weight * b + (1.0 - weight) * raw + PROJECTION_OFFSET,
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

/// Return probability for a player whose *eligibility* is unsettled rather
/// than his draft status (issue #220). Deliberately the neutral 0.5: a waiver
/// decision or an injunction is a genuine coin-flip we have no feed for, and
/// inventing a tuned constant would assert precision we don't have.
///
/// It is also the value that keeps `compute_projections::IN_SEASON_P_RETURN`
/// honest. That constant hard-codes 0.5 on the argument that the uncertain
/// bucket empties once a season is underway — true of draft declarants, false
/// of contested eligibility, which can stay open into the season (an
/// injunction is a mid-season state by definition) and which the new in-season
/// portal refresh can add to on any night. Weighting this cohort at 0.5 is
/// what keeps the materialized `team_preseason_projection` equal to the served
/// `/api/projections` midpoint, which the module doc there claims holds by
/// construction.
const ELIGIBILITY_UNSETTLED_RETURN_PROBABILITY: f32 = 0.5;

/// Mean return probability across a team's uncertain cohort. Used to
/// probability-weight the floor/ceiling midpoint — a flat 50/50 average
/// over-penalizes exactly the draft-talent-heavy (i.e. top) teams. Returns
/// `0.5` for an empty cohort, where it's unused anyway (floor == ceiling, so
/// the weight cancels).
///
/// Weighted **per cause**, not per player-name. The mock board answers "will
/// he come back to college instead of being drafted", so it may only be
/// consulted for players who actually declared. Running an
/// eligibility-contested senior through it inverts the signal on the players
/// who matter most: being good enough to appear on the board would score him
/// 0.05 — i.e. collapse his team's midpoint onto the floor that assumes he is
/// absent — over a question the draft has no bearing on. Scouts list good
/// seniors; that is not evidence about a waiver desk.
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
        .map(|(_, u)| player_return_probability(u, mock_by_name))
        .sum();
    sum / p.uncertain.len() as f32
}

/// One uncertain player's return probability, dispatched on why he is
/// uncertain. Split out from the mean so the dispatch is unit-testable without
/// standing up a whole `ProjectedRoster`.
fn player_return_probability(
    u: &cstat_core::roster_projection::UncertainPlayer,
    mock_by_name: &std::collections::HashMap<String, (i32, String)>,
) -> f32 {
    match u.cause {
        UncertainCause::DraftDeclared => {
            let pick = mock_by_name
                .get(&normalize_player_name(&u.name))
                .map(|(pick, _)| *pick);
            return_probability_from_pick(pick)
        }
        UncertainCause::EligibilityUnsettled => ELIGIBILITY_UNSETTLED_RETURN_PROBABILITY,
    }
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
    // Floor at 2016, the earliest target we can compose: projections need a
    // played base season (`year - 1`) plus that base season's
    // trajectory_oof_predictions, which start at target_season 2016 (a 2015
    // target would need 2014 base data we don't ingest).
    if !(2016..=2030).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "year out of range — projections supported for 2016–2030",
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
    // spring cycle, read from the `draft_entrants` table. An empty result
    // (year not loaded) degrades to seniors + portal-only departures — partial
    // coverage beats a 500. A DB error is real and bubbles up.
    let entrants = fetch_draft_entrants(&state.db.pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("fetch_draft_entrants failed: {e}") })),
            )
        })?;

    // Curated exits no feed reports (pro signings abroad, retirements,
    // dismissals). Same degrade-to-empty story as the entrants above.
    let departures = fetch_player_departures(&state.db.pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("fetch_player_departures failed: {e}") })),
            )
        })?;

    let projections = compose_all_projections(
        &state.db.pool,
        base_season,
        &entrants,
        &departures,
        &state.predictor,
        cstat_ingest::target_season_retro_complete(year),
    )
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

    // Base-season AdjO per team — the shrink anchor for the projected AdjO
    // half. Same keying as baseline_map; a miss just skips AdjO shrinkage.
    let baseline_o_map = fetch_baseline_adj_o(&state.db.pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("baseline AdjO fetch failed: {e}") })),
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
    // pass across every team — the roster-impact model scores rosters of
    // *projected* cam_v3 (recruits already carry the freshman model's
    // value). One trajectory fetch + inference for the whole slate; a
    // failure logs and degrades to current-season cam_v3.
    let mut traj_ids: Vec<Uuid> = Vec::new();
    for p in &projections {
        traj_ids.extend(p.returning.iter().map(|r| r.player_id));
        traj_ids.extend(p.arrivals.iter().map(|a| a.player_id));
        traj_ids.extend(p.uncertain.iter().map(|(row, _)| row.player_id));
    }
    let projected_cam = project_returner_cam_v3(&state.db.pool, &state.predictor, &traj_ids, year)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "trajectory cam_v3 projection failed; projecting on current-season cam_v3",
            );
            std::collections::HashMap::new()
        });

    // Display-only coach grade per team (descriptive; never feeds the
    // projection — see the coach fields on `ProjectedTeam`). A failure here is
    // cosmetic, so degrade to an empty map rather than 500 the whole page.
    let coach_map = fetch_coach_cae(&state.db.pool, base_season, year)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "coach CAE fetch failed; projections render without it");
            std::collections::HashMap::new()
        });

    // Base-season CamPom O/D split per player, for the cohort O/D sums
    // (display-only roster-shape decoration). Cosmetic — degrade to empty
    // (sums read 0 and the UI hides the split) rather than 500.
    let cam_od_map = fetch_cam_od_map(&state.db.pool, base_season)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cam O/D fetch failed; projections render without splits");
            std::collections::HashMap::new()
        });

    let mut rows: Vec<ProjectedTeam> = Vec::with_capacity(projections.len());
    for p in &projections {
        let baseline = baseline_map.get(&p.team_id).copied();
        let baseline_o = baseline_o_map.get(&p.team_id).copied();
        let actual = actual_map.get(&p.team_id).copied();
        let p_return = mean_return_probability(p, &mock_by_name);
        let Some(mut row) = predict_team(
            p,
            &state.predictor,
            baseline,
            actual,
            p_return,
            &projected_cam,
            &cam_od_map,
            baseline_o,
        ) else {
            continue;
        };
        if let Some(cc) = coach_map.get(&p.team_id) {
            row.coach_id = Some(cc.coach_id);
            row.coach_name = Some(cc.coach_name.clone());
            row.coach_cae_shrunk = cc.cae_shrunk;
            row.coach_cae_reliability = cc.reliability;
            row.coach_n_seasons = cc.n_seasons;
            row.coach_is_new_hc = cc.is_new_hc;
            row.coach_prev_team = cc.prev_team.clone();
        }
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
#[allow(clippy::too_many_arguments)] // cohesive per-team scoring inputs; a
// struct wrapper would just move the noise without aiding readability.
fn predict_team(
    p: &ProjectedRoster,
    predictor: &Predictor,
    baseline: Option<f32>,
    actual: Option<f32>,
    p_return: f32,
    projected_cam: &std::collections::HashMap<Uuid, f64>,
    cam_od: &std::collections::HashMap<Uuid, (f32, f32)>,
    baseline_o: Option<f32>,
) -> Option<ProjectedTeam> {
    // Recruits count toward the qualifying-size gate: a returners-thin
    // team with a strong freshman class (e.g. Duke with 4 incoming
    // 5-stars) is no longer "too thin to project". The roster-impact model
    // sees them via build_roster_impact_features just like returners.
    // Only the ranked composite cohort counts, matching the scored roster —
    // display-only commits-feed recruits (#175) are excluded so the gate and
    // `for_scenario` agree.
    let qualifying = p.returning.len() + p.arrivals.len() + p.projecting_recruits_count();

    // Per-cohort Σ CamPom for the UI's roster-flow columns. Returning
    // uses prior-season production; recruits use the synthesized
    // freshman-model projection their PlayerRow already carries (no prior
    // season exists). Missing cam_v3 contributes 0.
    let returning_cam_v3_sum: f32 = p
        .returning
        .iter()
        .map(|r| r.cam_v3.unwrap_or(0.0))
        .sum::<f64>() as f32;
    // Exclude redshirt / did-not-play recruits (completed seasons only) so a
    // no-show doesn't inflate the displayed recruit contribution, mirroring
    // their exclusion from the scored AdjEM. NOTE this does not fully equal the
    // scored roster: the commits-feed cohort (`feeds_projection == false`) is
    // still summed here, as it always has been — a pre-existing display choice,
    // not changed by this PR. No-op on the live upcoming projection, where
    // did_not_play is always false.
    let recruits_cam_v3_sum: f32 = p
        .recruits
        .iter()
        .filter(|(_, m)| !m.did_not_play)
        .map(|(row, _)| row.cam_v3.unwrap_or(0.0))
        .sum::<f64>() as f32;

    // Forward-frame sums for the roster-flow ledger: each returner /
    // arrival's *projected* next-season cam_v3 (the trajectory forecast
    // in `projected_cam`), falling back to their prior cam_v3 when the
    // player has no projection (didn't pass the trajectory gate). The
    // displayed value is forward; the prior sums above stay the % base.
    let proj_or_prior = |pid: Uuid, prior: Option<f64>| -> f64 {
        projected_cam
            .get(&pid)
            .copied()
            .unwrap_or_else(|| prior.unwrap_or(0.0))
    };
    let returning_projected_cam_v3_sum: f32 = p
        .returning
        .iter()
        .map(|r| proj_or_prior(r.player_id, r.cam_v3))
        .sum::<f64>() as f32;
    let arrivals_projected_cam_v3_sum: f32 = p
        .arrivals
        .iter()
        .map(|a| proj_or_prior(a.player_id, a.cam_v3))
        .sum::<f64>() as f32;
    // Prior-season value of the uncertain cohort — completes the
    // last-season roster base (returning + departures + uncertain).
    let uncertain_cam_v3_sum: f32 = p
        .uncertain
        .iter()
        .map(|(row, _)| row.cam_v3.unwrap_or(0.0))
        .sum::<f64>() as f32;

    // Prior-season O/D split sums per cohort (gated/uncovered players
    // contribute 0 to both halves — same convention as the cam_v3 sums).
    let od_sum = |ids: &mut dyn Iterator<Item = Uuid>| -> (f32, f32) {
        ids.filter_map(|id| cam_od.get(&id))
            .fold((0.0, 0.0), |(o, d), (po, pd)| (o + po, d + pd))
    };
    let (returning_cam_o_sum, returning_cam_d_sum) =
        od_sum(&mut p.returning.iter().map(|r| r.player_id));
    let (arrivals_cam_o_sum, arrivals_cam_d_sum) =
        od_sum(&mut p.arrivals.iter().map(|a| a.player_id));
    let (departures_cam_o_sum, departures_cam_d_sum) =
        od_sum(&mut p.departures.iter().map(|d| d.player_id()));

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
                // Redshirt / non-enroll (completed seasons only) — the frontend
                // greys + tags these so a graded report card explains why they
                // add nothing. Always false for the live upcoming projection.
                "did_not_play": m.did_not_play,
            })
        })
        .collect();

    // Turnover-aware baseline weight: stable 0.45, lower for overhaul rosters.
    // Derived from `p` by the shared helper, identical to what the offline
    // `score_projection_adj_em` computes, so the two serving paths never diverge.
    let baseline_weight = cstat_core::roster_projection::transition_shrink_weight(p);

    // Every team produces a row — too-thin rosters get null predictions
    // and a `too_thin = true` flag instead of being silently dropped.
    // This keeps "what happened to X?" auditable from the response.
    // `adjo_floor`/`adjo_ceiling` are the already-shrunk AdjO bounds (or
    // None for too-thin); blended on the same `p_return` weight as the net,
    // then AdjD derived as AdjO − AdjEM so the split reconciles exactly.
    let base = |floor: Option<f32>,
                ceiling: Option<f32>,
                too_thin: bool,
                adjo_floor: Option<f32>,
                adjo_ceiling: Option<f32>|
     -> ProjectedTeam {
        let blend = |f: Option<f32>, c: Option<f32>| {
            f.zip(c).map(|(f, c)| p_return * c + (1.0 - p_return) * f)
        };
        let midpoint_adj_em = blend(floor, ceiling);
        let projected_adj_o = blend(adjo_floor, adjo_ceiling);
        let projected_adj_d = projected_adj_o.zip(midpoint_adj_em).map(|(o, em)| o - em);
        ProjectedTeam {
            team_id: p.team_id,
            team_name: p.team_name.clone(),
            team_full_name: p.team_full_name.clone(),
            ceiling_adj_em: ceiling,
            floor_adj_em: floor,
            midpoint_adj_em,
            projected_adj_o,
            projected_adj_d,
            returning_count: p.returning.len(),
            returning_cam_v3_sum,
            returning_projected_cam_v3_sum,
            arrivals_count: p.arrivals.len(),
            arrivals_cam_v3_sum: p.inbound_cam_v3_sum,
            arrivals_projected_cam_v3_sum,
            // Total committed recruits, INCLUDING redshirt / did-not-play
            // commits — this must match the `top_recruits` list (which shows
            // them greyed) so the Projected tooltip's count agrees with the
            // names it lists, and the `recruits_count === 0` dash-guard only
            // fires when a team truly has no commits. `recruits_cam_v3_sum`
            // separately excludes did_not_play (they contributed zero); the
            // greyed "— redshirt (did not play)" marker on the excluded name
            // explains why the count can exceed the summed cohort.
            recruits_count: p.recruits.len(),
            recruits_cam_v3_sum,
            top_recruits: top_recruits.clone(),
            uncertain_count: p.uncertain.len(),
            uncertain_cam_v3_sum,
            departures_count: p.departures.len(),
            departures_cam_v3_sum: p.departures_cam_v3_sum,
            returning_cam_o_sum,
            returning_cam_d_sum,
            arrivals_cam_o_sum,
            arrivals_cam_d_sum,
            departures_cam_o_sum,
            departures_cam_d_sum,
            too_thin,
            baseline_adj_em: baseline,
            actual_adj_em: actual,
            baseline_weight,
            // Coach fields are decorative and filled by the handler after this
            // returns (predict_team has no DB access); default to absent here.
            coach_id: None,
            coach_name: None,
            coach_cae_shrunk: None,
            coach_cae_reliability: None,
            coach_n_seasons: None,
            coach_is_new_hc: None,
            coach_prev_team: None,
        }
    };

    if qualifying < MIN_QUALIFYING_FOR_PROJECTION {
        // Below the gate — the model can't honestly project a 1-6
        // qualifying-player roster (no freshmen / recruits modeled, so
        // the rate-stat aggregates over-weight the few starters). Surface
        // the row with metadata so the UI can show "—" and a tooltip.
        return Some(base(None, None, true, None, None));
    }

    // Score each scenario with the roster-impact model, AND the AdjO half
    // on the identical feature vector (one build, two model runs). Returns
    // (net AdjEM, AdjO); AdjD is derived downstream as AdjO − AdjEM.
    // Overwrite each returner / arrival's `cam_v3` with the trajectory
    // model's projection (recruits already carry the freshman model's
    // value from `freshman_row`); `build_roster_impact_features`
    // then does its own cam_v3-ranked canonical-MPG rotation
    // normalization — no separate `project_rotation` pass needed.
    // Returns (net AdjEM, AdjO) for the scenario, or None on an ONNX error
    // (logged) so the caller bails the whole team — matches the prior
    // per-scenario error handling without naming `ort::Error` (not a direct
    // dep of this crate).
    let score = |scenario, label: &str| -> Option<(f32, f32)> {
        let mut roster = p.for_scenario(scenario);
        apply_projected_cam_v3(&mut roster, projected_cam);
        let feats =
            build_roster_impact_features(&roster, p.outbound_cam_v3_sum, p.inbound_cam_v3_sum);
        let net = match predictor.predict_roster_impact(&feats) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(team = %p.team_name, error = ?e, "{label} net predict failed");
                return None;
            }
        };
        let adjo = match predictor.predict_roster_adjo(&feats) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(team = %p.team_name, error = ?e, "{label} adjo predict failed");
                return None;
            }
        };
        Some((net, adjo))
    };
    let (floor_raw, floor_o_raw) = score(DraftScenario::Floor, "floor")?;
    let (ceiling_raw, ceiling_o_raw) = score(DraftScenario::Ceiling, "ceiling")?;

    // Baseline-shrink both bounds at the turnover-aware weight. The band
    // shrinks in width by `(1 - baseline_weight)` but stays internally
    // consistent (ceiling ≥ floor preserved for non-anomaly teams;
    // negative-spread anomalies stay negative-spread). The AdjO bounds
    // shrink toward the prior-season *offense* (`baseline_o`) at the SAME
    // weight, so the derived AdjD shrinks toward prior defense and the
    // split stays coherent with the net headline.
    Some(base(
        Some(shrink(floor_raw, baseline, baseline_weight)),
        Some(shrink(ceiling_raw, baseline, baseline_weight)),
        false,
        Some(shrink(floor_o_raw, baseline_o, baseline_weight)),
        Some(shrink(ceiling_o_raw, baseline_o, baseline_weight)),
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
    if !(2016..=2030).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "year out of range — projections supported for 2016–2030",
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
    // Same source as the list route: the `draft_entrants` table. Empty result
    // (year not loaded) degrades to seniors + portal-only departures.
    let entrants = fetch_draft_entrants(pool, base_season).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("fetch_draft_entrants failed: {e}") })),
        )
    })?;
    let departures = fetch_player_departures(pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("fetch_player_departures failed: {e}") })),
            )
        })?;
    let projections = compose_all_projections(
        pool,
        base_season,
        &entrants,
        &departures,
        &state.predictor,
        cstat_ingest::target_season_retro_complete(year),
    )
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
    // Display name is the Torvik short name ("Duke", not "Duke Blue Devils"),
    // COALESCE'd to the full NatStat name for the rare team with no short_name —
    // the same convention every other team-name query uses (issue #172). This
    // was the last surface still surfacing the full NatStat name.
    let team_meta: Option<(String, Option<String>)> = sqlx::query_as(
        r#"SELECT COALESCE(short_name, name) AS name, short_name FROM teams WHERE id = $1 LIMIT 1"#,
    )
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
    let baseline_o = fetch_baseline_adj_o(pool, base_season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("baseline AdjO fetch failed: {e}") })),
            )
        })?
        .get(&resolved_id)
        .copied();

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
            // No season filter: an arrival's `player_id` is season-scoped (one
            // row per player per season), so it pins its own source season —
            // base_season for a normal transfer, an earlier season for a player
            // who sat out (issue #146, e.g. Caden Pierce's Princeton 2025 row).
            let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
                r#"
            SELECT pss.player_id, t.id, COALESCE(t.short_name, t.name)
            FROM player_season_stats pss
            JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
            WHERE pss.player_id = ANY($1)
            "#,
            )
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
    // source row for context), NBA-draft departures, and curated exits.
    for d in &projection.departures {
        let pid = d.player_id();
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
            match fetch_player_trajectory_rows(pool, &need_live).await {
                Ok(row_map) => {
                    let mut ids: Vec<Uuid> = Vec::new();
                    let mut feature_vectors: Vec<[f32; TRAJECTORY_NUM_FEATURES]> = Vec::new();
                    // `src_season` is each player's own season — base_season for a
                    // returner, an earlier season for a sat-out arrival (issue
                    // #146) — so season-derived features stay correct.
                    for (pid, (row, src_season)) in row_map {
                        ids.push(pid);
                        feature_vectors.push(build_trajectory_features(&row, src_season));
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

    // roster-impact scoring input: trajectory means for returners / arrivals
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
    // Cohort O/D sums for the detail payload — same display-only
    // decoration as the list route; degrade to empty on failure.
    let cam_od_map = fetch_cam_od_map(&state.db.pool, base_season)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cam O/D fetch failed; detail renders without splits");
            std::collections::HashMap::new()
        });

    let Some(mut row) = predict_team(
        &projection,
        &state.predictor,
        baseline,
        actual,
        p_return,
        &projected_cam,
        &cam_od_map,
        baseline_o,
    ) else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("inference failed for team {resolved_id}")
            })),
        ));
    };

    // Display-only coach for the projection season — the incoming HC (resolved
    // target-season-first, so a 2027 hire like Hodgson → Providence surfaces),
    // with the new-HC flag + prior program for the "← from X" note. Same source
    // and no-leakage contract as the grid route; a failure is cosmetic.
    let coach_map = fetch_coach_cae(pool, base_season, year)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "coach CAE fetch failed for team detail; rendering without it");
            std::collections::HashMap::new()
        });
    if let Some(cc) = coach_map.get(&resolved_id) {
        row.coach_id = Some(cc.coach_id);
        row.coach_name = Some(cc.coach_name.clone());
        row.coach_cae_shrunk = cc.cae_shrunk;
        row.coach_cae_reliability = cc.reliability;
        row.coach_n_seasons = cc.n_seasons;
        row.coach_is_new_hc = cc.is_new_hc;
        row.coach_prev_team = cc.prev_team.clone();
    }

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
                "position": meta.position,
                "projected_cam_v3": row.cam_v3,
                "projected_campom_lower": meta.projected_campom_lower,
                "projected_campom_upper": meta.projected_campom_upper,
                // Redshirt / non-enroll (completed seasons only); false on the
                // live upcoming projection. Frontend greys + tags these.
                "did_not_play": meta.did_not_play,
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
        .map(|d| d.player_id())
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
            // `reason` is populated for `left_program` only — it's the
            // sub-vocabulary ('pro_overseas' / 'retired' / …) that lets the UI
            // distinguish a pro signing (has a destination) from a retirement
            // (doesn't). The other kinds carry their reason in `kind` itself.
            let (pid, kind, name, destination, destination_team_id, reason) = match d {
                cstat_core::roster_projection::DepartureReason::GraduatedSenior {
                    player_id,
                    name,
                } => (*player_id, "senior", name.clone(), None, None, None),
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
                    None,
                ),
                cstat_core::roster_projection::DepartureReason::DraftGone { player_id, name } => {
                    (*player_id, "draft_gone", name.clone(), None, None, None)
                }
                cstat_core::roster_projection::DepartureReason::LeftProgram {
                    player_id,
                    name,
                    reason,
                    destination,
                } => (
                    *player_id,
                    "left_program",
                    name.clone(),
                    destination.clone(),
                    None,
                    Some(reason.clone()),
                ),
            };
            let meta = departure_meta.get(&pid);
            let (mean, lower, upper) = serialize_proj(&pid);
            json!({
                "kind": kind,
                "reason": reason,
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
            // Mock-draft fields only for players who actually declared. A name
            // match alone is not enough: the chip's own copy reads "declared
            // players who fall off the board often withdraw", so attaching it
            // to an eligibility case tells the user he entered a draft he never
            // entered. `cause` lets the UI render the right chip instead of
            // inferring one from a null.
            let mock_hit = match meta.cause {
                UncertainCause::DraftDeclared => {
                    mock_by_name.get(&normalize_player_name(&meta.name))
                }
                UncertainCause::EligibilityUnsettled => None,
            };
            json!({
                "player_id": meta.player_id,
                "name": meta.name,
                "reason": meta.reason,
                "cause": meta.cause,
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

/// Base-season `team_season_stats.adj_offense` (absolute ~105) per team —
/// the shrink anchor for the projected AdjO half. Same shape/keying as
/// `fetch_baseline_adj_em`; a team missing here just skips AdjO shrinkage.
async fn fetch_baseline_adj_o(
    pool: &sqlx::PgPool,
    base_season: i32,
) -> Result<std::collections::HashMap<Uuid, f32>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        team_id: Uuid,
        adj_offense: f64,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT team_id, adj_offense
        FROM team_season_stats
        WHERE season = $1 AND adj_offense IS NOT NULL
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.team_id, r.adj_offense as f32))
        .collect())
}

/// Base-season CamPom O/D split per player, envelope-gated jointly
/// (`abs() <= 30` on both halves — the same regression guard every other
/// serving surface applies; see `queries::get_torvik_stats`). Keyed by
/// player_id; gated/missing players are simply absent and contribute 0
/// to the cohort sums. One query for the whole slate (~5k rows).
async fn fetch_cam_od_map(
    pool: &sqlx::PgPool,
    base_season: i32,
) -> Result<std::collections::HashMap<Uuid, (f32, f32)>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        player_id: Uuid,
        cam_o: f64,
        cam_d: f64,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT player_id, cam_o_gbpm_v3_psos AS cam_o, cam_d_gbpm_v3_psos AS cam_d
        FROM torvik_player_stats
        WHERE season = $1
          AND cam_o_gbpm_v3_psos IS NOT NULL AND cam_d_gbpm_v3_psos IS NOT NULL
          AND abs(cam_o_gbpm_v3_psos) <= 30 AND abs(cam_d_gbpm_v3_psos) <= 30
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.player_id, (r.cam_o as f32, r.cam_d as f32)))
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

/// Display-only coach CAE per projected team, keyed by **base-season**
/// team_id (the key `ProjectedTeam` carries). For each base-season program it
/// resolves the coach leading them into the target season — preferring a
/// `coach_seasons` row at the target season, falling back to the base season
/// (the live forecast's target year isn't ingested yet, and most programs keep
/// their coach across the offseason) — then joins the career `coach_ratings`.
///
/// **Descriptive only.** The PIT backtest (`training/pit_cae_backtest.py`)
/// found the coach term's projection lift is program-level bias, not coaching
/// (a program-keyed null beats it), so this must not feed the served forecast;
/// it only decorates the response. The `LATERAL … LIMIT 1` collapses the
/// known `coach_seasons` team-name-variant fan-out (migration 024) to one row.
async fn fetch_coach_cae(
    pool: &sqlx::PgPool,
    base_season: i32,
    target_season: i32,
) -> Result<std::collections::HashMap<Uuid, CoachCae>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        base_team_id: Uuid,
        coach_id: Uuid,
        coach_name: String,
        cae_shrunk: Option<f64>,
        reliability: Option<f64>,
        n_seasons: Option<i32>,
        is_new_hc: Option<bool>,
        prev_team: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT t_base.id           AS base_team_id,
               co.id               AS coach_id,
               co.canonical_name   AS coach_name,
               cr.cae_shrunk,
               cr.reliability,
               cr.n_seasons,
               pick.is_new_hc,
               prev.coachdict_team_name AS prev_team
        FROM teams t_base
        JOIN LATERAL (
            SELECT cs.coach_id, cs.is_new_hc
            FROM coach_seasons cs
            WHERE cs.team_natstat_id = t_base.natstat_id
              AND cs.season IN ($1, $2)
            ORDER BY (cs.season = $2) DESC
            LIMIT 1
        ) pick ON TRUE
        JOIN coaches co ON co.id = pick.coach_id
        LEFT JOIN coach_ratings cr ON cr.coach_id = co.id
        -- The picked coach's prior-season (base) program, for the "← from X"
        -- note on a new hire. Excludes the current program (a continuing coach
        -- must never read "from {same team}") and orders deterministically so a
        -- coach with multiple base-season name-variant rows resolves stably.
        -- NULL for a first-time / promoted D-I coach (no prior different program).
        LEFT JOIN LATERAL (
            SELECT cs2.coachdict_team_name
            FROM coach_seasons cs2
            WHERE cs2.coach_id = pick.coach_id
              AND cs2.season = $1
              AND cs2.team_natstat_id IS DISTINCT FROM t_base.natstat_id
            ORDER BY (cs2.team_natstat_id IS NOT NULL) DESC, cs2.coachdict_team_name
            LIMIT 1
        ) prev ON TRUE
        WHERE t_base.season = $1
        "#,
    )
    .bind(base_season)
    .bind(target_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.base_team_id,
                CoachCae {
                    coach_id: r.coach_id,
                    coach_name: r.coach_name,
                    cae_shrunk: r.cae_shrunk,
                    reliability: r.reliability,
                    n_seasons: r.n_seasons,
                    is_new_hc: r.is_new_hc,
                    prev_team: r.prev_team,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cstat_core::roster_projection::PROJECTION_SHRINK_WEIGHT as SHRINK_WEIGHT;

    #[test]
    fn shrink_blends_raw_with_baseline_and_offset() {
        let raw = 30.0_f32;
        let baseline = 25.0_f32;
        let expected = SHRINK_WEIGHT * baseline + (1.0 - SHRINK_WEIGHT) * raw + PROJECTION_OFFSET;
        assert!((shrink(raw, Some(baseline), SHRINK_WEIGHT) - expected).abs() < 1e-5);
    }

    #[test]
    fn shrink_offsets_raw_when_baseline_missing() {
        // No baseline (new D-I program) → offset-corrected raw, no anchor.
        let raw = 12.3_f32;
        assert!((shrink(raw, None, SHRINK_WEIGHT) - (raw + PROJECTION_OFFSET)).abs() < 1e-5);
    }

    #[test]
    fn shrink_is_monotonic_in_raw() {
        // The blend preserves ordering: a higher raw projection always
        // yields a higher shrunk output (so floor ≤ ceiling survives).
        assert!(shrink(50.0, Some(25.0), SHRINK_WEIGHT) > shrink(20.0, Some(25.0), SHRINK_WEIGHT));
    }

    #[test]
    fn overhaul_weight_leans_off_a_stale_baseline() {
        // At a lower (overhaul) weight the blend sits closer to the roster
        // projection than to a stale baseline. raw=10, baseline=25:
        // w=0.45 → 16.75; w=0.20 → 13.0 (nearer raw).
        let raw = 10.0_f32;
        let baseline = 25.0_f32;
        let stable = shrink(raw, Some(baseline), SHRINK_WEIGHT);
        let overhaul = shrink(
            raw,
            Some(baseline),
            cstat_core::roster_projection::PROJECTION_SHRINK_WEIGHT_OVERHAUL,
        );
        assert!(overhaul < stable, "lower weight should pull toward raw");
        assert!((overhaul - 13.0).abs() < 1e-5);
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

    #[test]
    fn eligibility_uncertainty_is_not_weighted_by_the_draft_board() {
        use cstat_core::roster_projection::{UncertainCause, UncertainPlayer};

        let uncertain = |cause| UncertainPlayer {
            player_id: Uuid::new_v4(),
            name: "Top Senior".into(),
            reason: "…".into(),
            cause,
        };
        // Same name, on the board as a projected top-5 pick.
        let mut mock = std::collections::HashMap::new();
        mock.insert(
            normalize_player_name("Top Senior"),
            (5_i32, "Wizards".to_string()),
        );

        // A declarant on that board is treated as effectively gone — unchanged.
        assert!(
            (player_return_probability(&uncertain(UncertainCause::DraftDeclared), &mock) - 0.05)
                .abs()
                < 1e-6
        );
        // The same board entry must NOT touch a player whose open question is
        // eligibility. Weighting him 0.05 would collapse his team's midpoint
        // onto the floor that assumes he is absent, on the strength of scouts
        // rating a good senior — which is not evidence about a waiver desk.
        assert!(
            (player_return_probability(&uncertain(UncertainCause::EligibilityUnsettled), &mock)
                - ELIGIBILITY_UNSETTLED_RETURN_PROBABILITY)
                .abs()
                < 1e-6
        );
        // And it is the neutral 0.5, which is what keeps the materialized
        // `team_preseason_projection` equal to the served midpoint.
        assert!((ELIGIBILITY_UNSETTLED_RETURN_PROBABILITY - 0.5).abs() < 1e-6);
    }
}
