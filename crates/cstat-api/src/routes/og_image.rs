//! Generated per-player social-card images (`GET /og/players/{id}.png`).
//!
//! Renders a 1200x630 PNG stat card (name, team, CamPom rating, archetype) from
//! an SVG template via `resvg`, using a vendored DejaVu font (loaded once) so it
//! works in the slim Docker runtime without system fonts. The per-page meta
//! injector (`routes::meta`) points a player's `og:image` at this route.
//!
//! Fail-soft: a non-UUID id, a missing player, or any render error redirects to
//! the static `/og-image.png`, so a shared link always resolves to *some* card.

use std::sync::{Arc, OnceLock};

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use resvg::{tiny_skia, usvg};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use cstat_core::queries;

const FONT_REGULAR: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf");

/// Card dimensions. 1200x630 is the canonical Open Graph size (1.91:1).
pub const CARD_W: u32 = 1200;
pub const CARD_H: u32 = 630;

const BG: &str = "#0b0b0c"; // matches the site theme-color
const ACCENT: &str = "#60a5fa"; // the navbar CamPom wordmark blue
const FG: &str = "#ffffff";
const MUTED: &str = "#9ca3af";
const LIGHT: &str = "#e5e7eb";

#[derive(Deserialize)]
pub struct OgParams {
    season: Option<i32>,
}

/// Font database built once from the embedded DejaVu faces and shared across
/// requests (parsing ~1.4 MB of fonts per request would be wasteful).
fn fontdb() -> &'static Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(FONT_REGULAR.to_vec());
        db.load_font_data(FONT_BOLD.to_vec());
        Arc::new(db)
    })
}

/// `GET /og/players/{id}.png` — the player's generated card, else the static one.
pub async fn player_card(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<OgParams>,
) -> Response {
    match render_player_card(&state, &id, p.season).await {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                // Edge-cache a few hours; CamPom values only move on the nightly.
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

async fn render_player_card(state: &AppState, id: &str, season: Option<i32>) -> Option<Vec<u8>> {
    let pool = &state.db.pool;
    let id = id.strip_suffix(".png").unwrap_or(id);
    let uuid = Uuid::parse_str(id).ok()?;
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

    let campom = queries::get_torvik_stats(pool, resolved, season)
        .await
        .ok()
        .flatten()
        .and_then(|t| t.campom);
    let archetype = queries::get_player_archetype(pool, resolved, season)
        .await
        .ok()
        .flatten()
        .map(|a| a.primary_class)
        .filter(|s| !s.is_empty());

    // The detail line: archetype, then position / class.
    let mut detail: Vec<String> = Vec::new();
    if let Some(a) = &archetype {
        detail.push(a.clone());
    }
    if let Some(pos) = prof.position.as_deref().filter(|s| !s.is_empty()) {
        detail.push(pos.to_string());
    }
    if let Some(cls) = prof.class_year.as_deref().filter(|s| !s.is_empty()) {
        detail.push(cls.to_string());
    }

    let subtitle = match prof.team_name.as_deref().filter(|s| !s.is_empty()) {
        Some(team) => format!("{team} · {season}"),
        None => season.to_string(),
    };

    let svg = build_svg(
        &truncate(&prof.name, 24),
        &subtitle,
        campom,
        &detail.join(" · "),
    );
    rasterize(&svg)
}

fn build_svg(name: &str, subtitle: &str, campom: Option<f64>, detail: &str) -> String {
    let name = xml_escape(name);
    let subtitle = xml_escape(subtitle);
    let detail = xml_escape(detail);

    // Big CamPom value block (omitted when unavailable).
    let stat_block = match campom {
        Some(v) => format!(
            r#"<text x="80" y="475" font-family="DejaVu Sans" font-weight="bold" font-size="128" fill="{ACCENT}">{v:+.1}</text>
  <text x="80" y="523" font-family="DejaVu Sans" font-size="28" fill="{MUTED}">CamPom rating</text>"#
        ),
        None => String::new(),
    };
    let detail_line = if detail.is_empty() {
        String::new()
    } else {
        format!(
            r#"<text x="80" y="582" font-family="DejaVu Sans" font-size="34" fill="{LIGHT}">{detail}</text>"#
        )
    };

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="14" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="80" y="104" font-family="DejaVu Sans" font-weight="bold" font-size="32" letter-spacing="6" fill="{ACCENT}">CAMPOM</text>
  <text x="80" y="268" font-family="DejaVu Sans" font-weight="bold" font-size="80" fill="{FG}">{name}</text>
  <text x="80" y="332" font-family="DejaVu Sans" font-size="40" fill="{MUTED}">{subtitle}</text>
  <line x1="80" y1="372" x2="1120" y2="372" stroke="#1f2937" stroke-width="2"/>
  {stat_block}
  {detail_line}
</svg>"##
    )
}

fn rasterize(svg: &str) -> Option<Vec<u8>> {
    let opt = usvg::Options {
        font_family: "DejaVu Sans".to_string(),
        fontdb: fontdb().clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(CARD_W, CARD_H)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

/// Truncate to at most `max` chars (not bytes), appending an ellipsis. Keeps
/// long names from overflowing the card.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// XML text escaping for values placed inside SVG `<text>` elements.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_png() {
        // End-to-end: the SVG template rasterizes to a valid PNG with the
        // embedded font (no DB needed). Guards the resvg API + font loading.
        let svg = build_svg(
            "Cooper Flagg",
            "Duke · 2026",
            Some(6.2),
            "Centaur · Forward · Fr",
        );
        let png = rasterize(&svg).expect("should rasterize");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            png.len() > 1000,
            "png unexpectedly tiny: {} bytes",
            png.len()
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("short", 24), "short");
        let long = "A".repeat(40);
        let t = truncate(&long, 24);
        assert_eq!(t.chars().count(), 24);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(xml_escape("Texas A&M"), "Texas A&amp;M");
    }
}
