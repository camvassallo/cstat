use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/archetypes", get(archetypes_index))
}

#[derive(Deserialize)]
struct Params {
    season: Option<i32>,
    per_class: Option<i64>,
}

async fn archetypes_index(
    State(state): State<Arc<AppState>>,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let per_class = params.per_class.unwrap_or(5).clamp(1, 20);
    let pool = &state.db.pool;

    let (counts, exemplars) = tokio::try_join!(
        queries::get_archetype_class_counts(pool, season),
        queries::get_archetype_exemplars(pool, season, per_class),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    // Group exemplars by class for the response payload.
    let mut by_class: HashMap<String, Vec<&queries::ArchetypeExemplar>> = HashMap::new();
    for ex in &exemplars {
        by_class
            .entry(ex.primary_class.clone())
            .or_default()
            .push(ex);
    }

    let classes: Vec<Value> = counts
        .iter()
        .map(|c| {
            let players: Vec<Value> = by_class
                .get(&c.primary_class)
                .map(|exs| {
                    exs.iter()
                        .map(|e| {
                            json!({
                                "player_id": e.player_id,
                                "name": e.name,
                                "team_id": e.team_id,
                                "team_name": e.team_name,
                                "primary_score": e.primary_score,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "name": c.primary_class,
                "count": c.count,
                "exemplars": players,
            })
        })
        .collect();

    Ok(Json(json!({
        "season": season,
        "classes": classes,
    })))
}
