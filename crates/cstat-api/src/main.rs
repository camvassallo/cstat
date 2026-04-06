use anyhow::Result;
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::{Database, Predictor};
use cstat_ingest::NatStatClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

/// Shared application state.
pub struct AppState {
    pub db: Database,
    pub natstat: NatStatClient,
    pub predictor: Predictor,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cstat_api=info,cstat_ingest=info,tower_http=info".into()),
        )
        .init();

    // Connect to database
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db = Database::connect(&database_url).await?;
    info!("connected to database");

    // Run migrations
    db.migrate().await?;
    info!("migrations complete");

    // Create NatStat client
    let natstat_api_key = std::env::var("NATSTAT_API_KEY").expect("NATSTAT_API_KEY must be set");
    let natstat = NatStatClient::new(db.pool.clone(), natstat_api_key, 500);

    // Load ONNX models
    let model_dir = std::env::var("MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("training/models"));
    let predictor = Predictor::load(&model_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to load ONNX models from {}: {}",
            model_dir.display(),
            e
        )
    })?;
    info!("loaded ONNX models from {}", model_dir.display());

    let state = Arc::new(AppState {
        db,
        natstat,
        predictor,
    });

    // Build router
    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/status", get(api_status))
        .route("/api/predict", get(predict))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("listening on {}", bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn api_status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, StatusCode> {
    let remaining = state.natstat.rate_limit_remaining().await;
    Ok(Json(json!({
        "status": "ok",
        "rate_limit_remaining": remaining,
    })))
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
    let season = params.season.unwrap_or(2026);

    // Look up teams by name (case-insensitive) or UUID
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

    // Check if same conference
    let is_conference =
        home_team.conference.is_some() && home_team.conference == away_team.conference;

    // Extract features
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

    // Run inference
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
    id: uuid::Uuid,
    name: String,
    conference: Option<String>,
}

async fn find_team(
    pool: &sqlx::PgPool,
    query: &str,
    season: i32,
) -> Result<TeamLookup, sqlx::Error> {
    // Try UUID first
    if let Ok(id) = query.parse::<uuid::Uuid>() {
        return sqlx::query_as::<_, TeamLookup>(
            "SELECT id, name, conference FROM teams WHERE id = $1 AND season = $2",
        )
        .bind(id)
        .bind(season)
        .fetch_one(pool)
        .await;
    }

    // Case-insensitive exact match first, then prefix match
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

    // Prefix match (e.g. "Michigan" -> "Michigan Wolverines")
    sqlx::query_as::<_, TeamLookup>(
        "SELECT id, name, conference FROM teams WHERE LOWER(name) LIKE LOWER($1) || '%' AND season = $2 LIMIT 1",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
}
