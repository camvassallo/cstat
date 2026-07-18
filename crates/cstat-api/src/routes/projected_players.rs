//! Per-player projected-CamPom ranking for an upcoming (not-yet-played)
//! season. Thin read over the materialized `player_season_projection` table
//! (migration 045), which `cstat-ingest compute-projections` populates from the
//! trajectory (returners / transfers) and freshman (recruits) models.
//!
//! The `/players` page calls this when the season picker is set to the upcoming
//! projected year; the actual-season path (`players::player_list`) is untouched.

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
    Router::new().route("/api/projected-players/{year}", get(projected_player_list))
}

/// One projected player, ordered by `campom` (the projected-CamPom mean) DESC.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct ProjectedPlayerRow {
    /// Real season-scoped `players.id` for returners/transfers; `recruits.id`
    /// for freshmen. The frontend links returners/transfers to their base
    /// season's detail page; freshmen (no player page) are non-linked.
    player_id: Uuid,
    name: String,
    /// `returning` | `transfer` | `freshman`.
    source: String,
    /// Base-season team the player is projected onto (destination for a
    /// transfer) — same base-season UUID `/api/projections/{year}` emits, so the
    /// frontend links it to the team's future page (`?season={year}&view=projected`).
    team_id: Uuid,
    /// Torvik short name (e.g. "Duke", not "Duke Blue Devils").
    team_name: String,
    natstat_id: Option<String>,
    /// Projected CamPom mean (the ranking key).
    campom: f32,
    campom_lower: Option<f32>,
    campom_upper: Option<f32>,
    class_year: Option<String>,
    primary_archetype: Option<String>,
    composite_rank: Option<i32>,
    star_rating: Option<i16>,
}

async fn projected_player_list(
    State(state): State<Arc<AppState>>,
    Path(year): Path<i32>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let players: Vec<ProjectedPlayerRow> = sqlx::query_as(
        r#"
        SELECT
            player_id,
            name,
            source,
            team_id,
            team_name,
            natstat_id,
            projected_cam_mean  AS campom,
            projected_cam_lower AS campom_lower,
            projected_cam_upper AS campom_upper,
            class_year,
            primary_archetype,
            composite_rank,
            star_rating
        FROM player_season_projection
        WHERE target_season = $1
        ORDER BY projected_cam_mean DESC, name ASC
        "#,
    )
    .bind(year)
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("projected-players query failed: {e}") })),
        )
    })?;

    Ok(Json(json!({
        "target_season": year,
        "base_season": year - 1,
        "count": players.len(),
        "players": players,
    })))
}
