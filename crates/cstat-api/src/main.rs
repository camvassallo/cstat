use anyhow::Result;
use axum::{Router, extract::State, http::StatusCode, response::Json, routing::get};
use cstat_core::Database;
use cstat_ingest::NatStatClient;
use serde_json::{Value, json};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

/// Shared application state.
pub struct AppState {
    pub db: Database,
    pub natstat: NatStatClient,
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

    let state = Arc::new(AppState { db, natstat });

    // Build router
    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/status", get(api_status))
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
