use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::freshman_model::{FreshmanFeatureRow, build_freshman_features};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/recruits/{year}", get(recruit_list))
}

/// One row pulled from the `recruits` table joined to its committed school.
/// `cam_*` / `primary_class` columns are mostly NULL for class-of-2026 — they
/// fill in once the recruit's freshman cstat-season (`year + 1`) ingests box
/// scores and Pass 2 of the recruit join resolves `cstat_player_id`.
///
/// Schema in `migrations/020_recruits.sql`; ingest in
/// `crates/cstat-ingest/src/ingest/recruits.rs`.
#[derive(sqlx::FromRow)]
struct RecruitRow {
    composite_rank: Option<i32>,
    full_name: String,
    position: Option<String>,
    height: Option<String>,
    weight: Option<i32>,
    city: Option<String>,
    state: Option<String>,
    high_school: Option<String>,
    composite_rating: Option<f32>,
    star_rating: Option<i16>,
    previous_rank: Option<i32>,
    position_rank: Option<i32>,
    state_rank: Option<i32>,
    committed_school: Option<String>,
    committed_team_id: Option<Uuid>,
    commit_status: Option<String>,
    profile_url: Option<String>,
    photo_url: Option<String>,
    cstat_player_id: Option<Uuid>,
    // Joined: clickable destination chip. Pulled via the FK row's id, so the
    // season this resolves to is whichever season Pass 1 of the recruit join
    // landed on (= most recent ingested ≤ year+1).
    committed_team_name: Option<String>,
    committed_team_short_name: Option<String>,
    // Joined cstat player data — NULL until the freshman cstat-season ingests
    // and Pass 2 of the recruit join fills `cstat_player_id`. Same shape as
    // the transfers route so the frontend can render with one column set.
    campom: Option<f64>,
    campom_pct: Option<f64>,
    primary_class: Option<String>,
    secondary_class: Option<String>,
    minutes_per_game: Option<f64>,
    games_played: Option<i32>,
    // Freshman-impact prior model inputs (Phase 6). Same join chain as
    // `freshman_model::fetch_freshman_features` — committed-team AdjEM
    // from the season BEFORE the recruit arrived (`r.year`), peer-class
    // mean rating across the same (year, committed_team) bucket. NULL
    // for solo signings / defunct programs; sentinel-encoded inside
    // `build_freshman_features`.
    committed_team_prior_adjem: Option<f64>,
    peer_class_strength: Option<f64>,
    // Raw recruit fields the freshman model needs but the existing route
    // didn't surface — `year` for `years_since_recruit`, the others to
    // mirror the FreshmanFeatureRow struct.
    recruit_year: Option<i32>,
}

#[derive(Serialize)]
struct EnrichedRecruit {
    composite_rank: Option<i32>,
    name: String,
    position: Option<String>,
    height: Option<String>,
    weight: Option<i32>,
    city: Option<String>,
    state: Option<String>,
    high_school: Option<String>,
    composite_rating: Option<f32>,
    star_rating: Option<i16>,
    previous_rank: Option<i32>,
    position_rank: Option<i32>,
    state_rank: Option<i32>,
    committed_school: Option<String>,
    committed_school_short: Option<String>,
    committed_team_id: Option<Uuid>,
    commit_status: Option<String>,
    profile_url: Option<String>,
    photo_url: Option<String>,
    // cstat-side enrichment (mostly NULL until cstat-season year+1 ingests)
    player_id: Option<Uuid>,
    campom: Option<f64>,
    campom_pct: Option<f64>,
    primary_class: Option<String>,
    secondary_class: Option<String>,
    minutes_per_game: Option<f64>,
    games_played: Option<i32>,
    // Phase 6 freshman-impact projection — mean + q10/q90 band. Available
    // for every recruit (uses pre-college features only; doesn't depend on
    // cstat_player_id being resolved). For unranked recruits with missing
    // school context, the model returns a sensible unranked-cohort baseline
    // via the sentinel branch.
    projected_campom_mean: Option<f32>,
    projected_campom_lower: Option<f32>,
    projected_campom_upper: Option<f32>,
}

async fn recruit_list(
    State(state): State<Arc<AppState>>,
    Path(year): Path<i32>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !(2000..=2100).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "year out of range" })),
        ));
    }

    // Recruit's freshman cstat-season = recruiting year + 1 (e.g. class-of-2026
    // first plays in cstat-season 2027). `cstat_player_id` is resolved against
    // that season's `players` rows, so the LEFT JOINs filter to the same
    // season to avoid pulling a player's prior-season torvik row (would be
    // wrong for one-and-done transfers; harmless for HS recruits but kept
    // consistent for when 2025/2024 classes get ingested).
    let target_season = year + 1;

    let recruits: Vec<RecruitRow> = sqlx::query_as::<_, RecruitRow>(
        r#"
        SELECT
            r.composite_rank,
            r.full_name,
            r.position,
            r.height,
            r.weight,
            r.city,
            r.state,
            r.high_school,
            r.composite_rating,
            r.star_rating,
            r.previous_rank,
            r.position_rank,
            r.state_rank,
            r.committed_school,
            r.committed_team_id,
            r.commit_status,
            r.profile_url,
            r.photo_url,
            r.cstat_player_id,
            r.year                       AS recruit_year,
            t.name                       AS committed_team_name,
            t.short_name                 AS committed_team_short_name,
            tps.cam_gbpm_v3_psos         AS campom,
            tps.cam_gbpm_v3_psos_pct     AS campom_pct,
            pa.primary_class             AS primary_class,
            pa.secondary_class           AS secondary_class,
            pss.minutes_per_game         AS minutes_per_game,
            pss.games_played             AS games_played,
            adjem.adj_efficiency_margin  AS committed_team_prior_adjem,
            peer.mean_rating             AS peer_class_strength
        FROM recruits r
        LEFT JOIN teams t
            ON t.id = r.committed_team_id
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = r.cstat_player_id AND tps.season = $2
        LEFT JOIN player_archetypes pa
            ON pa.player_id = r.cstat_player_id AND pa.season = $2
        LEFT JOIN player_season_stats pss
            ON pss.player_id = r.cstat_player_id AND pss.season = $2
        LEFT JOIN teams tm_prior
            ON tm_prior.natstat_id = t.natstat_id AND tm_prior.season = r.year
        LEFT JOIN team_season_stats adjem
            ON adjem.team_id = tm_prior.id AND adjem.season = r.year
        LEFT JOIN (
            SELECT year, committed_team_id, AVG(composite_rating) AS mean_rating
            FROM recruits
            WHERE composite_rating IS NOT NULL AND committed_team_id IS NOT NULL
            GROUP BY year, committed_team_id
        ) peer
            ON peer.year = r.year AND peer.committed_team_id = r.committed_team_id
        WHERE r.year = $1
        ORDER BY r.composite_rank NULLS LAST, r.full_name
        "#,
    )
    .bind(year)
    .bind(target_season)
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("recruits query failed: {e}") })),
        )
    })?;

    if recruits.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("no recruits data for year {year}"),
            })),
        ));
    }

    // Phase 6 freshman-impact projections — per-recruit mean + q10/q90 band.
    // Inference is single-row per recruit; with ~3 ONNX runs per call (mean
    // + q10 + q90) and ~370 recruits in a typical class, this is ~1100 ONNX
    // runs serialized through the Predictor's mutex sessions. Acceptable for
    // a non-hot-path route; batch inference is a follow-up if needed.
    //
    // On inference error, log and emit NULL projection fields rather than
    // failing the whole route — one borked feature vector shouldn't deny
    // the whole class-of-N response.
    let enriched: Vec<EnrichedRecruit> = recruits
        .into_iter()
        .map(|r| {
            let feature_row = FreshmanFeatureRow {
                composite_rank: r.composite_rank,
                composite_rating: r.composite_rating,
                star_rating: r.star_rating,
                position_rank: r.position_rank,
                previous_rank: r.previous_rank,
                height: r.height.clone(),
                weight: r.weight,
                position: r.position.clone(),
                year: r.recruit_year,
                committed_team_prior_adjem: r.committed_team_prior_adjem,
                peer_class_strength: r.peer_class_strength,
            };
            let features = build_freshman_features(&feature_row);
            let (proj_mean, proj_lo, proj_hi) = match state.predictor.predict_freshman(&features) {
                Ok(p) => (Some(p.mean), Some(p.lower), Some(p.upper)),
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        name = %r.full_name,
                        "freshman predict failed",
                    );
                    (None, None, None)
                }
            };

            EnrichedRecruit {
                composite_rank: r.composite_rank,
                name: r.full_name,
                position: r.position,
                height: r.height,
                weight: r.weight,
                city: r.city,
                state: r.state,
                high_school: r.high_school,
                composite_rating: r.composite_rating,
                star_rating: r.star_rating,
                previous_rank: r.previous_rank,
                position_rank: r.position_rank,
                state_rank: r.state_rank,
                committed_school: r.committed_school,
                committed_school_short: r.committed_team_short_name.or(r.committed_team_name),
                committed_team_id: r.committed_team_id,
                commit_status: r.commit_status,
                profile_url: r.profile_url,
                photo_url: r.photo_url,
                player_id: r.cstat_player_id,
                campom: r.campom,
                campom_pct: r.campom_pct,
                primary_class: r.primary_class,
                secondary_class: r.secondary_class,
                minutes_per_game: r.minutes_per_game,
                games_played: r.games_played,
                projected_campom_mean: proj_mean,
                projected_campom_lower: proj_lo,
                projected_campom_upper: proj_hi,
            }
        })
        .collect();

    Ok(Json(json!({
        "year": year,
        "base_season": target_season,
        "recruits": enriched,
        "total": enriched.len(),
    })))
}
