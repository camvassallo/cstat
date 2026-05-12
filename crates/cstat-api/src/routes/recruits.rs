use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
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
            t.name                       AS committed_team_name,
            t.short_name                 AS committed_team_short_name,
            tps.cam_gbpm_v3_psos         AS campom,
            tps.cam_gbpm_v3_psos_pct     AS campom_pct,
            pa.primary_class             AS primary_class,
            pa.secondary_class           AS secondary_class,
            pss.minutes_per_game         AS minutes_per_game,
            pss.games_played             AS games_played
        FROM recruits r
        LEFT JOIN teams t
            ON t.id = r.committed_team_id
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = r.cstat_player_id AND tps.season = $2
        LEFT JOIN player_archetypes pa
            ON pa.player_id = r.cstat_player_id AND pa.season = $2
        LEFT JOIN player_season_stats pss
            ON pss.player_id = r.cstat_player_id AND pss.season = $2
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

    let enriched: Vec<EnrichedRecruit> = recruits
        .into_iter()
        .map(|r| EnrichedRecruit {
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
        })
        .collect();

    Ok(Json(json!({
        "year": year,
        "base_season": target_season,
        "recruits": enriched,
        "total": enriched.len(),
    })))
}
