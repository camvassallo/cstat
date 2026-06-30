//! `sitemap.xml` + child sitemaps, generated from the DB so Google can discover
//! every player/team/coach page directly (not only those reachable by crawling
//! links). A sitemap index points at per-type child sitemaps to stay well under
//! the 50,000-URL / 50 MB per-file cap.
//!
//! URLs are the clean, query-less canonical form (`/players/{uuid}`), matching
//! the `rel="canonical"` the per-page meta injects (see `routes::meta`). Only the
//! newest season's entities are listed to avoid duplicate-content bloat across
//! seasons; coach pages are career-scoped (one per rated coach).

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// Public origin for absolute URLs, e.g. `https://campom.org` (no trailing
/// slash). Overridable via `PUBLIC_BASE_URL` for staging/local. Shared with the
/// per-page meta injector.
pub fn public_base_url() -> String {
    std::env::var("PUBLIC_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://campom.org".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Newest season that actually has games (mirrors `/api/seasons`), so the
/// sitemap tracks real data rather than the date-derived calendar season which
/// can lead the data in the off-season. Falls back to the calendar season.
async fn newest_season(state: &AppState) -> i32 {
    sqlx::query_scalar::<_, Option<i32>>("SELECT max(season) FROM games")
        .fetch_one(&state.db.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(crate::default_season)
}

fn xml_response(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            // Crawlers re-fetch periodically; an hour at the edge is plenty and
            // keeps these DB-backed routes cheap.
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}

fn db_err(e: sqlx::Error) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("sitemap query failed: {e}"),
    )
}

/// `GET /sitemap.xml` — the sitemap index.
pub async fn index(State(state): State<Arc<AppState>>) -> Response {
    let base = public_base_url();
    let _ = &state; // index is static; state kept for a uniform handler signature
    let children = [
        "sitemap-static.xml",
        "sitemap-teams.xml",
        "sitemap-players.xml",
        "sitemap-coaches.xml",
    ];
    let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    out.push('\n');
    for c in children {
        out.push_str(&format!("  <sitemap><loc>{base}/{c}</loc></sitemap>\n"));
    }
    out.push_str("</sitemapindex>\n");
    xml_response(out)
}

/// `GET /sitemap-static.xml` — the fixed top-level pages.
pub async fn static_pages(State(_state): State<Arc<AppState>>) -> Response {
    // Only param-less, indexable routes (tools like /players/compare are
    // omitted — no SEO value). Kept in sync with `web/src/App.tsx`.
    let paths = [
        "/", "/players", "/predict", "/archetypes", "/projected", "/draft", "/lineups", "/coaches",
    ];
    xml_response(urlset(paths.iter().map(|p| p.to_string())))
}

/// `GET /sitemap-teams.xml` — every team page for the newest season.
pub async fn teams(State(state): State<Arc<AppState>>) -> Result<Response, (StatusCode, String)> {
    let season = newest_season(&state).await;
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM teams WHERE season = $1 ORDER BY id")
            .bind(season)
            .fetch_all(&state.db.pool)
            .await
            .map_err(db_err)?;
    Ok(xml_response(urlset(
        ids.into_iter().map(|id| format!("/teams/{id}")),
    )))
}

/// `GET /sitemap-players.xml` — every player page for the newest season.
pub async fn players(State(state): State<Arc<AppState>>) -> Result<Response, (StatusCode, String)> {
    let season = newest_season(&state).await;
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM players WHERE season = $1 ORDER BY id")
            .bind(season)
            .fetch_all(&state.db.pool)
            .await
            .map_err(db_err)?;
    Ok(xml_response(urlset(
        ids.into_iter().map(|id| format!("/players/{id}")),
    )))
}

/// `GET /sitemap-coaches.xml` — every rated coach (career-scoped, one per coach).
pub async fn coaches(State(state): State<Arc<AppState>>) -> Result<Response, (StatusCode, String)> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT coach_id FROM coach_ratings ORDER BY coach_id")
            .fetch_all(&state.db.pool)
            .await
            .map_err(db_err)?;
    Ok(xml_response(urlset(
        ids.into_iter().map(|id| format!("/coaches/{id}")),
    )))
}

/// Render a `<urlset>` from an iterator of site-relative paths. Paths are clean
/// (no query params) and contain only URL-safe characters (UUIDs / fixed
/// slugs), so no XML entity escaping is required.
fn urlset(paths: impl Iterator<Item = String>) -> String {
    let base = public_base_url();
    let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    out.push('\n');
    for p in paths {
        out.push_str(&format!("  <url><loc>{base}{p}</loc></url>\n"));
    }
    out.push_str("</urlset>\n");
    out
}
