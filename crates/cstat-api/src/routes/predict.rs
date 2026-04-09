use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/predict", get(predict))
}

#[derive(Deserialize)]
struct PredictParams {
    home: String,
    away: String,
    #[serde(default)]
    neutral: bool,
    season: Option<i32>,
}

async fn predict(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PredictParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);

    let home_team = find_team(&state.db.pool, &params.home, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("home team not found: {}", params.home) })),
            )
        })?;

    let away_team = find_team(&state.db.pool, &params.away, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("away team not found: {}", params.away) })),
            )
        })?;

    let is_conference =
        home_team.conference.is_some() && home_team.conference == away_team.conference;

    let features = cstat_core::features::build_game_features(
        &state.db.pool,
        home_team.id,
        away_team.id,
        season,
        params.neutral,
        is_conference,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("feature extraction failed: {e}") })),
        )
    })?;

    let prediction = state.predictor.predict(&features).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("prediction failed: {e}") })),
        )
    })?;

    let predicted_winner = if prediction.predicted_margin > 0.0 {
        &home_team.name
    } else {
        &away_team.name
    };

    Ok(Json(json!({
        "home_team": home_team.name,
        "away_team": away_team.name,
        "predicted_margin": (prediction.predicted_margin as f64 * 10.0).round() / 10.0,
        "home_win_probability": (prediction.home_win_probability * 1000.0).round() / 1000.0,
        "predicted_winner": predicted_winner,
    })))
}

#[derive(sqlx::FromRow)]
struct TeamLookup {
    id: Uuid,
    name: String,
    conference: Option<String>,
}

async fn find_team(pool: &PgPool, query: &str, season: i32) -> Result<TeamLookup, sqlx::Error> {
    if let Ok(id) = query.parse::<Uuid>() {
        return sqlx::query_as::<_, TeamLookup>(
            "SELECT id, name, conference FROM teams WHERE id = $1 AND season = $2",
        )
        .bind(id)
        .bind(season)
        .fetch_one(pool)
        .await;
    }

    if let Ok(team) = sqlx::query_as::<_, TeamLookup>(
        "SELECT id, name, conference FROM teams WHERE LOWER(name) = LOWER($1) AND season = $2",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
    {
        return Ok(team);
    }

    sqlx::query_as::<_, TeamLookup>(
        "SELECT id, name, conference FROM teams WHERE LOWER(name) LIKE LOWER($1) || '%' AND season = $2 ORDER BY LENGTH(name) LIMIT 1",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
}
