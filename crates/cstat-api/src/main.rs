mod routes;

use anyhow::Result;
use axum::{Router, extract::State, response::Json, routing::get};
use chrono::Datelike;
use cstat_core::{Database, Predictor};
use cstat_ingest::NatStatClient;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::info;

/// Shared application state.
pub struct AppState {
    pub db: Database,
    pub natstat: NatStatClient,
    pub predictor: Predictor,
}

/// Default season: Nov+ = next year, otherwise current year.
pub fn default_season() -> i32 {
    let now = chrono::Utc::now().naive_utc().date();
    if now.month() >= 11 {
        now.year() + 1
    } else {
        now.year()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

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

    db.migrate().await?;
    info!("migrations complete");

    // NatStat client
    let natstat_api_key = std::env::var("NATSTAT_API_KEY").expect("NATSTAT_API_KEY must be set");
    let natstat = NatStatClient::new(db.pool.clone(), natstat_api_key, 500);

    // ONNX models
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

    // Static file serving for React SPA
    let spa_dir = std::env::var("SPA_DIR").unwrap_or_else(|_| "web/dist".into());
    let spa = ServeDir::new(&spa_dir)
        .not_found_service(ServeFile::new(format!("{spa_dir}/index.html")));

    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/status", get(api_status))
        .merge(routes::api_routes())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
        .fallback_service(spa);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("listening on {}", bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn api_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let remaining = state.natstat.rate_limit_remaining().await;
    Json(json!({
        "status": "ok",
        "rate_limit_remaining": remaining,
    }))
}
