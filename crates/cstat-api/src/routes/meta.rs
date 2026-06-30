//! Per-page social-preview meta injection for the SPA entity routes
//! (`/players/:id`, `/teams/:id`, `/coaches/:id`).
//!
//! The frontend is a client-rendered SPA, so social unfurlers (Twitterbot,
//! Discordbot, Slackbot, facebookexternalhit, …) — which don't run JS — only
//! ever see the static `index.html` `<head>`. These handlers serve that same
//! `index.html` but with the `<!--SSR_META_START--> … <!--SSR_META_END-->`
//! region swapped for per-entity `<title>` + canonical + OG/Twitter tags, so a
//! shared player/team/coach link unfurls with that entity's name and stats.
//!
//! Robustness: a non-UUID segment (e.g. `/players/compare`), a missing entity,
//! or a DB error all fall back to serving the default `index.html` unchanged —
//! React still mounts and renders the page normally. The image stays the static
//! default card (per-entity generated images are a later phase; see
//! `docs/seo_social_previews_plan.md`).

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

use crate::AppState;
use crate::routes::sitemap::public_base_url;
use cstat_core::queries;

#[derive(Deserialize)]
pub struct MetaParams {
    season: Option<i32>,
}

/// `index.html` parsed once at startup, split around the SSR_META markers so a
/// request only has to concatenate (prefix, per-page tags, suffix).
#[derive(Clone)]
pub struct SpaTemplate {
    full: String,
    /// `(prefix-incl-START-marker, suffix-incl-END-marker)`. `None` when the
    /// markers (or the file) are absent — handlers then serve `full` unchanged.
    region: Option<(String, String)>,
}

impl SpaTemplate {
    const START: &'static str = "<!--SSR_META_START-->";
    const END: &'static str = "<!--SSR_META_END-->";

    /// Read and parse `{spa_dir}/index.html`. Failures degrade to "serve the
    /// file unchanged" (or empty if unreadable) rather than erroring — per-page
    /// meta is an enhancement, never load-bearing for serving the page.
    pub fn load(spa_dir: &str) -> Self {
        let path = format!("{spa_dir}/index.html");
        match std::fs::read_to_string(&path) {
            Ok(full) => {
                let region = Self::split(&full);
                if region.is_none() {
                    warn!(
                        path,
                        "index.html has no SSR_META markers; per-page social meta disabled"
                    );
                }
                Self { full, region }
            }
            Err(e) => {
                warn!(path, error = %e, "could not read index.html; per-page social meta disabled");
                Self {
                    full: String::new(),
                    region: None,
                }
            }
        }
    }

    fn split(full: &str) -> Option<(String, String)> {
        let start = full.find(Self::START)? + Self::START.len();
        let end = full.find(Self::END)?;
        if end < start {
            return None;
        }
        Some((full[..start].to_string(), full[end..].to_string()))
    }

    /// The page with the SSR_META region replaced by `meta`. Falls back to the
    /// unchanged file if the markers weren't found.
    fn render(&self, meta: &str) -> String {
        match &self.region {
            Some((prefix, suffix)) => format!("{prefix}\n{meta}\n    {suffix}"),
            None => self.full.clone(),
        }
    }

    fn default_page(&self) -> String {
        self.full.clone()
    }
}

/// `GET /players/:id` (HTML document) — inject the player's meta, else default.
pub async fn player_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<MetaParams>,
) -> Response {
    page(&state, build_player_meta(&state, &id, p.season).await)
}

/// `GET /teams/:id` (HTML document).
pub async fn team_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<MetaParams>,
) -> Response {
    page(&state, build_team_meta(&state, &id, p.season).await)
}

/// `GET /coaches/:id` (HTML document). Coaches are season-agnostic.
pub async fn coach_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    page(&state, build_coach_meta(&state, &id).await)
}

/// Wrap the rendered HTML with a short edge-cacheable `Cache-Control`.
fn page(state: &AppState, meta: Option<String>) -> Response {
    let html = match meta {
        Some(m) => state.spa_index.render(&m),
        None => state.spa_index.default_page(),
    };
    ([(header::CACHE_CONTROL, "public, max-age=300")], Html(html)).into_response()
}

async fn build_player_meta(state: &AppState, id: &str, season: Option<i32>) -> Option<String> {
    let pool = &state.db.pool;
    let uuid = Uuid::parse_str(id).ok()?; // non-UUID (e.g. "compare") → default page
    let season = season.unwrap_or_else(crate::default_season);
    let resolved = queries::resolve_player_id_for_season(pool, uuid, season)
        .await
        .ok()
        .flatten()
        .unwrap_or(uuid);
    let prof = queries::get_player_by_id(pool, resolved, season)
        .await
        .ok()
        .flatten()?;

    let title = match prof.team_name.as_deref().filter(|s| !s.is_empty()) {
        Some(team) => format!("{} — {} ({}) | CamPom", prof.name, team, season),
        None => format!("{} ({}) | CamPom", prof.name, season),
    };

    let mut parts: Vec<String> = Vec::new();
    if let Some(pos) = prof.position.as_deref().filter(|s| !s.is_empty()) {
        parts.push(pos.to_string());
    }
    if let Some(cls) = prof.class_year.as_deref().filter(|s| !s.is_empty()) {
        parts.push(cls.to_string());
    }
    if let Some(a) = queries::get_player_archetype(pool, resolved, season)
        .await
        .ok()
        .flatten()
        .map(|a| a.primary_class)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("{a} archetype"));
    }
    if let Some(cam) = queries::get_torvik_stats(pool, resolved, season)
        .await
        .ok()
        .flatten()
        .and_then(|t| t.campom)
    {
        parts.push(format!("{cam:+.1} CamPom"));
    }
    let desc = if parts.is_empty() {
        format!("{} — college basketball analytics on CamPom.", prof.name)
    } else {
        parts.join(" · ")
    };

    let url = format!("{}/players/{}", public_base_url(), resolved);
    Some(render_tags(&title, &desc, &url))
}

async fn build_team_meta(state: &AppState, id: &str, season: Option<i32>) -> Option<String> {
    let pool = &state.db.pool;
    let uuid = Uuid::parse_str(id).ok()?;
    let season = season.unwrap_or_else(crate::default_season);
    let resolved = queries::resolve_team_id_for_season(pool, uuid, season)
        .await
        .ok()
        .flatten()
        .unwrap_or(uuid);
    let t = queries::get_team_by_id(pool, resolved, season)
        .await
        .ok()
        .flatten()?;

    let title = format!("{} {} | CamPom", t.name, season);

    let mut parts: Vec<String> = Vec::new();
    if let Some(em) = t.adj_efficiency_margin {
        let rank = t
            .adj_efficiency_margin_rank
            .map(|r| format!(" (#{r})"))
            .unwrap_or_default();
        parts.push(format!("AdjEM {em:+.1}{rank}"));
    }
    if let (Some(w), Some(l)) = (t.wins, t.losses) {
        parts.push(format!("{w}-{l}"));
    }
    if let Some(c) = t.conference.as_deref().filter(|s| !s.is_empty()) {
        parts.push(c.to_string());
    }
    let desc = if parts.is_empty() {
        format!("{} — college basketball analytics on CamPom.", t.name)
    } else {
        format!("{} · {}", t.name, parts.join(" · "))
    };

    let url = format!("{}/teams/{}", public_base_url(), resolved);
    Some(render_tags(&title, &desc, &url))
}

async fn build_coach_meta(state: &AppState, id: &str) -> Option<String> {
    let pool = &state.db.pool;
    let uuid = Uuid::parse_str(id).ok()?;
    let name = queries::get_coach_name(pool, uuid).await.ok().flatten()?;

    let title = format!("{name} | CamPom");
    let mut desc = format!("{name} — college basketball coach");
    if let Some(r) = queries::get_coach_rating(pool, uuid).await.ok().flatten() {
        desc.push_str(&format!(
            " · CAE {:+.1} over {} season{}",
            r.cae_shrunk,
            r.n_seasons,
            if r.n_seasons == 1 { "" } else { "s" }
        ));
    }
    desc.push_str(" — on CamPom.");

    let url = format!("{}/coaches/{}", public_base_url(), uuid);
    Some(render_tags(&title, &desc, &url))
}

/// Build the replacement `<head>` tag block. All dynamic values are HTML-escaped
/// (names can contain `&`, `'`, `"`, e.g. "Texas A&M", "Saint Mary's").
fn render_tags(title: &str, desc: &str, url: &str) -> String {
    let t = esc(title);
    let d = esc(desc);
    let u = esc(url);
    format!(
        "<title>{t}</title>\n    \
         <link rel=\"canonical\" href=\"{u}\" />\n    \
         <meta name=\"description\" content=\"{d}\" />\n    \
         <meta property=\"og:url\" content=\"{u}\" />\n    \
         <meta property=\"og:title\" content=\"{t}\" />\n    \
         <meta property=\"og:description\" content=\"{d}\" />\n    \
         <meta name=\"twitter:title\" content=\"{t}\" />\n    \
         <meta name=\"twitter:description\" content=\"{d}\" />"
    )
}

/// Minimal HTML entity escaping for text placed in attribute values / elements.
/// `&` must be replaced first.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_chars() {
        assert_eq!(esc("Texas A&M"), "Texas A&amp;M");
        assert_eq!(esc("Saint Mary's"), "Saint Mary&#39;s");
        assert_eq!(esc("a\"b<c>"), "a&quot;b&lt;c&gt;");
    }

    #[test]
    fn template_splits_and_renders() {
        let html = "<head>\n<!--SSR_META_START-->\n<title>x</title>\n<!--SSR_META_END-->\n</head>";
        let t = SpaTemplate {
            full: html.to_string(),
            region: SpaTemplate::split(html),
        };
        assert!(t.region.is_some());
        let out = t.render("<title>NEW</title>");
        assert!(out.contains("<title>NEW</title>"));
        assert!(!out.contains("<title>x</title>"));
        assert!(out.contains("</head>"));
    }

    #[test]
    fn template_without_markers_serves_full() {
        let html = "<head><title>x</title></head>";
        let t = SpaTemplate {
            full: html.to_string(),
            region: SpaTemplate::split(html),
        };
        assert!(t.region.is_none());
        assert_eq!(t.render("ignored"), html);
        assert_eq!(t.default_page(), html);
    }
}
