mod guards;
mod routes;

use anyhow::Result;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::{Router, extract::State, response::Json, routing::get};
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
    /// SPA `index.html` parsed once for per-page social-meta injection
    /// (`routes::meta`). Cheap clone-free reads on the entity document routes.
    pub spa_index: routes::meta::SpaTemplate,
}

/// Default season used by route handlers when `?season=` is omitted.
/// Delegates to `cstat_ingest::current_natstat_season` so the CLI and API
/// agree on what "now" means.
pub use cstat_ingest::current_natstat_season as default_season;

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
    let db = Database::connect_api(&database_url).await?;
    info!("connected to database");

    db.migrate().await?;
    info!("migrations complete");

    // NatStat client
    let natstat_api_key = std::env::var("NATSTAT_API_KEY").expect("NATSTAT_API_KEY must be set");
    let natstat = NatStatClient::new(
        db.pool.clone(),
        natstat_api_key,
        cstat_ingest::rate_budget_from_env(),
    );

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

    // SPA dir is needed both for the static file service (below) and to parse
    // index.html once for per-page social-meta injection.
    let spa_dir = std::env::var("SPA_DIR").unwrap_or_else(|_| "web/dist".into());
    let spa_index = routes::meta::SpaTemplate::load(&spa_dir);

    let state = Arc::new(AppState {
        db,
        natstat,
        predictor,
        spa_index,
    });

    // Static file serving for React SPA. ServeDir handles asset paths
    // that map to real files on disk (`/assets/*`, `/favicon.svg`,
    // `/index.html`); anything else falls through to ServeFile on
    // index.html so React Router can take over on hard navigation,
    // share links, and refresh.
    //
    // Important: use `ServeDir::fallback(…)`, NOT `.not_found_service(…)`.
    // tower-http's `not_found_service` wraps its argument in
    // `SetStatus(404)`, which forces every fallback response to 404
    // regardless of the inner service's status — so direct navigation
    // to `/predict`, `/teams/<id>`, etc. served the right HTML body
    // but with a 404 status, and browsers bailed before React Router
    // could mount. `fallback(…)` skips that wrapper.
    let spa_files =
        ServeDir::new(&spa_dir).fallback(ServeFile::new(format!("{spa_dir}/index.html")));
    // Wrap the static service so content-hashed `/assets/*` get a 1-year
    // immutable `Cache-Control` (the hash is the cache-buster), while
    // index.html / favicon fall through to ServeDir's ETag revalidation so a
    // deploy is picked up immediately. A nested `Router` makes the layer wrap
    // the fallback service unambiguously.
    let spa: Router<()> = Router::new()
        .fallback_service(spa_files)
        .layer(from_fn(guards::static_asset_cache));

    // Serving guards layered onto the data routes only (NOT health/status):
    //   - cache_headers: short-TTL `Cache-Control` so a CDN/browser can serve
    //     most reads without hitting the origin (innermost — tags the
    //     successful response on its way out).
    //   - enforce_timeout: 408 a request that overruns instead of holding its
    //     DB handle open.
    //   - load_shed: 503 past `MAX_INFLIGHT_REQUESTS` (outermost — sheds before
    //     any work or DB acquisition happens).
    // Health stays un-guarded so a saturated server still passes its platform
    // healthcheck rather than getting load-shed and bouncing.
    let data_api = routes::api_routes()
        .layer(from_fn(guards::cache_headers))
        .layer(from_fn_with_state(
            guards::timeout_duration(),
            guards::enforce_timeout,
        ))
        .layer(from_fn_with_state(
            guards::inflight_semaphore(),
            guards::load_shed,
        ));

    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/health/ingest", get(routes::health::ingest_health))
        .route("/api/status", get(api_status))
        // SEO: sitemap index + child sitemaps (DB-generated), and per-page social
        // meta injected into the SPA's index.html for entity document routes.
        // These are non-/api paths that would otherwise fall through to the SPA.
        .route("/sitemap.xml", get(routes::sitemap::index))
        .route("/sitemap-static.xml", get(routes::sitemap::static_pages))
        .route("/sitemap-teams.xml", get(routes::sitemap::teams))
        .route("/sitemap-players.xml", get(routes::sitemap::players))
        .route("/sitemap-coaches.xml", get(routes::sitemap::coaches))
        .route("/players/{id}", get(routes::meta::player_document))
        .route("/teams/{id}", get(routes::meta::team_document))
        .route("/coaches/{id}", get(routes::meta::coach_document))
        .merge(data_api)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
        .fallback_service(spa);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| {
        std::env::var("PORT")
            .map(|p| format!("0.0.0.0:{p}"))
            .unwrap_or_else(|_| "0.0.0.0:8080".into())
    });
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
