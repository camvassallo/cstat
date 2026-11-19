//! Per-page social-preview meta injection for the SPA entity routes
//! (`/players/:id`, `/teams/:id`, `/coaches/:id`) and Predict-page matchup
//! links (`/predict?home=…&away=…`).
//!
//! The frontend is a client-rendered SPA, so social unfurlers (Twitterbot,
//! Discordbot, Slackbot, facebookexternalhit, …) — which don't run JS — only
//! ever see the static `index.html` `<head>`. These handlers serve that same
//! `index.html` but with the `<!--SSR_META_START--> … <!--SSR_META_END-->`
//! region swapped for per-entity `<title>` + canonical + OG/Twitter tags, so a
//! shared player/team/coach link unfurls with that entity's name and stats.
//!
//! Robustness: a non-UUID segment (e.g. `/players/compare`), a missing entity,
//! or a DB error all fall back to the default `index.html` head — React still
//! mounts and renders the page normally. Each page's `og:image` is a generated
//! card (`routes::og_image`), which itself falls back to the static
//! `/og-image.png` when it cannot render.
//!
//! `spa_document` applies the same injection to every OTHER page route (`/`,
//! `/players`, `/portle`, …): those keep the file's default
//! title/description but get a `rel="canonical"` + `og:url` built from the
//! request path. That canonical is not cosmetic. The site answers on both
//! `camalytics.org` and `campom.org` — the old brand stays open rather than
//! being 301'd away (`docs/domain_migration.md`) — so the cross-domain canonical
//! is the only thing telling a crawler the two hosts are one site and not two
//! competing copies of it. Every document route must emit one.

use axum::{
    extract::{Path, Query, State},
    http::{Uri, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

use crate::AppState;
use crate::routes::og_image::MatchupQuery;
use crate::routes::predict;
use crate::routes::sitemap::public_base_url;
use crate::routes::social_cards::season_label;
use cstat_core::projection::Venue;
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
    /// The marker region's own contents as shipped in `index.html` (title,
    /// description, OG/Twitter defaults), minus its `og:url`. Re-emitted by
    /// `render_default_with_canonical` alongside a path-correct URL pair.
    default_meta: String,
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
                // An empty region while the markers ARE present means the head
                // did not parse the way we expect. That would silently disable
                // the canonical on every non-entity page, so say so.
                let default_meta = match Self::default_meta(&full) {
                    Some(m) => m,
                    None => {
                        if region.is_some() {
                            warn!(
                                path,
                                "SSR_META region present but unparseable; \
                                 non-entity pages will serve without a canonical"
                            );
                        }
                        String::new()
                    }
                };
                Self {
                    full,
                    region,
                    default_meta,
                }
            }
            Err(e) => {
                warn!(path, error = %e, "could not read index.html; per-page social meta disabled");
                Self {
                    full: String::new(),
                    region: None,
                    default_meta: String::new(),
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

    /// The marker region's contents with its `og:url` **tag** removed. The
    /// static `og:url` in `index.html` names the homepage, which is wrong on
    /// every other route, so the server re-emits a path-correct one next to the
    /// canonical. `index.html` stays the source of truth for the *text*
    /// defaults; the server owns the URL-shaped tags.
    ///
    /// Removal is by tag rather than by line on purpose. A line filter reads as
    /// equivalent while `index.html` is pretty-printed, but it fails
    /// catastrophically and silently the day anything minifies the head onto one
    /// line: that single line contains `og:url`, so the filter drops the ENTIRE
    /// region, `default_meta` comes back empty, and every non-entity page
    /// quietly stops emitting a canonical — which is the whole mechanism holding
    /// the two serving hosts together. Tag-scoped removal is indifferent to
    /// line structure, and matches the property value rather than one quoting
    /// style, so a reformat of `index.html` cannot slip a second `og:url`
    /// through.
    fn default_meta(full: &str) -> Option<String> {
        let start = full.find(Self::START)? + Self::START.len();
        let end = full.find(Self::END)?;
        if end < start {
            return None;
        }
        let block = strip_og_url_tags(&full[start..end]);
        let trimmed = block.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.to_string())
    }

    /// The default head plus a `rel="canonical"` + `og:url` for `path` — the
    /// cross-domain signal that pins every serving host to one indexable URL.
    /// Degrades to the unchanged file if the markers or the region are missing.
    fn render_default_with_canonical(&self, path: &str) -> String {
        if self.region.is_none() || self.default_meta.is_empty() {
            return self.full.clone();
        }
        let url = esc(&canonical_url(path));
        let meta = format!(
            "{}\n    <link rel=\"canonical\" href=\"{url}\" />\n    \
             <meta property=\"og:url\" content=\"{url}\" />",
            self.default_meta
        );
        self.render(&meta)
    }

    /// The page with the SSR_META region replaced by `meta`. Falls back to the
    /// unchanged file if the markers weren't found.
    fn render(&self, meta: &str) -> String {
        match &self.region {
            Some((prefix, suffix)) => format!("{prefix}\n{meta}\n    {suffix}"),
            None => self.full.clone(),
        }
    }
}

/// `GET /players/:id` (HTML document) — inject the player's meta, else default.
pub async fn player_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<MetaParams>,
    uri: Uri,
) -> Response {
    page(
        &state,
        uri.path(),
        build_player_meta(&state, &id, p.season).await,
    )
}

/// `GET /teams/:id` (HTML document).
pub async fn team_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<MetaParams>,
    uri: Uri,
) -> Response {
    page(
        &state,
        uri.path(),
        build_team_meta(&state, &id, p.season).await,
    )
}

/// `GET /coaches/:id` (HTML document). Coaches are season-agnostic.
pub async fn coach_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Response {
    page(&state, uri.path(), build_coach_meta(&state, &id).await)
}

/// `GET /predict` (HTML document). A link that names a matchup
/// (`?home=…&away=…`, as the Schedule tab, the ticker and the page itself
/// build) unfurls with that matchup and its generated card; a bare `/predict`
/// gets the default head, like any other page.
pub async fn predict_document(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MatchupQuery>,
    uri: Uri,
) -> Response {
    page(&state, uri.path(), build_predict_meta(&state, &q).await)
}

/// Every non-entity SPA route (`/`, `/players`, `/portle`, and any
/// path React Router owns): the default head, plus a canonical for this path.
///
/// This is the SPA's catch-all, reached once `ServeDir` has failed to match a
/// real file, so it also answers unknown paths — which still return 200 with
/// `index.html` so React Router can render its own not-found state, exactly as
/// the plain `ServeFile` fallback it replaced did.
pub async fn spa_document(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    page(&state, uri.path(), None)
}

/// Wrap the rendered HTML with a short edge-cacheable `Cache-Control`.
///
/// `path` drives the canonical when there is no entity meta, so a page whose
/// entity failed to resolve is still pinned to the canonical host rather than
/// being left for a crawler to attribute to whichever of our two hosts it
/// happened to fetch.
fn page(state: &AppState, path: &str, meta: Option<String>) -> Response {
    let html = match meta {
        Some(m) => state.spa_index.render(&m),
        None => state.spa_index.render_default_with_canonical(path),
    };
    ([(header::CACHE_CONTROL, "public, max-age=300")], Html(html)).into_response()
}

/// Absolute canonical URL for a request path, on the canonical origin.
///
/// The query string is dropped: `?season=` and friends select a view of the
/// same page, and the sitemap lists only the clean form, so every variant
/// collapses onto one indexable URL. A trailing slash is trimmed (except on the
/// root) for the same reason — `/players` and `/players/` are one page.
fn canonical_url(path: &str) -> String {
    let base = public_base_url();
    // `/index.html` is reachable (ServeDir will happily serve the file) and is
    // the same page as `/`. Fold it in rather than leaving a second URL for the
    // homepage — the one page we least want duplicated.
    let path = path.strip_suffix("index.html").unwrap_or(path);
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}{trimmed}")
    }
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
        Some(team) => format!("{} — {} ({}) | Camalytics", prof.name, team, season),
        None => format!("{} ({}) | Camalytics", prof.name, season),
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
        parts.push(format!("{cam:+.1} CAM"));
    }
    if let Some(ss) = queries::get_player_season_stats(pool, resolved, season)
        .await
        .ok()
        .flatten()
        .filter(|s| s.games_played > 0)
    {
        let line: Vec<String> = [("PPG", ss.ppg), ("RPG", ss.rpg), ("APG", ss.apg)]
            .into_iter()
            .filter_map(|(l, v)| v.map(|v| format!("{v:.1} {l}")))
            .collect();
        if !line.is_empty() {
            parts.push(line.join(", "));
        }
    }
    let desc = if parts.is_empty() {
        format!(
            "{} — college basketball analytics on Camalytics.",
            prof.name
        )
    } else {
        parts.join(" · ")
    };

    let base = public_base_url();
    let url = format!("{base}/players/{resolved}");
    // Players, teams and coaches each get a generated card.
    // The season rides along so a link to a past season's page previews that
    // season, not the player's current one.
    let img = OgImage::generated(
        format!("{base}/og/players/{resolved}.png?season={season}"),
        format!(
            "{} player card: CAM rating, stats and skill radar",
            prof.name
        ),
    );
    Some(render_tags(&title, &desc, &url, &img))
}

async fn build_predict_meta(state: &AppState, q: &MatchupQuery) -> Option<String> {
    let pool = &state.db.pool;
    let params = q.to_predict()?;
    let (home_season, away_season) = params.resolved_seasons(crate::default_season());
    // Both names must resolve, so the title only ever names real teams in the
    // site's own spelling, never whatever text the link carried.
    let home = predict::find_team(pool, &params.home, home_season)
        .await
        .ok()?;
    let away = predict::find_team(pool, &params.away, away_season)
        .await
        .ok()?;

    let cross_era = home_season != away_season;
    let label = |name: &str, season: i32| {
        if cross_era {
            format!("{} {name}", season_label(season))
        } else {
            name.to_string()
        }
    };
    let (h, a) = (
        label(&home.name, home_season),
        label(&away.name, away_season),
    );
    let matchup = match params.venue {
        Some(Venue::Neutral) => format!("{h} vs {a}"),
        Some(Venue::Away) => format!("{h} at {a}"),
        _ => format!("{a} at {h}"),
    };
    let title = format!("{matchup}: game prediction | Camalytics");
    let desc = format!("Projected score, spread and win probability for {matchup}.");

    // Rebuilt from what resolved rather than echoed, so the shared URL and the
    // image URL carry canonical names and no parameter the page would ignore.
    let clean = MatchupQuery {
        home: Some(home.name.clone()),
        away: Some(away.name.clone()),
        venue: q
            .venue
            .clone()
            .filter(|v| matches!(v.as_str(), "home" | "away" | "neutral")),
        season: q.season,
        home_season: q.home_season,
        away_season: q.away_season,
        as_of_date: params.as_of_date.map(|d| d.to_string()),
    }
    .query_string();
    let base = public_base_url();
    let img = OgImage::generated(
        format!("{base}/og/predict.png?{clean}"),
        format!("{matchup}: projected score and win probability"),
    );
    // The canonical stays the bare page (one indexable URL); og:url keeps the
    // matchup, or an unfurler that re-fetches og:url would preview a blank form.
    Some(render_tags_with(
        &title,
        &desc,
        &format!("{base}/predict"),
        &format!("{base}/predict?{clean}"),
        &img,
    ))
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

    let title = format!("{} {} | Camalytics", t.name, season);

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
        format!("{} — college basketball analytics on Camalytics.", t.name)
    } else {
        format!("{} · {}", t.name, parts.join(" · "))
    };

    let base = public_base_url();
    let url = format!("{base}/teams/{resolved}");
    // The season rides along for the same reason as the player card's.
    let img = OgImage::generated(
        format!("{base}/og/teams/{resolved}.png?season={season}"),
        format!(
            "{} team card: rating, efficiency, recent form and top players",
            t.name
        ),
    );
    Some(render_tags(&title, &desc, &url, &img))
}

async fn build_coach_meta(state: &AppState, id: &str) -> Option<String> {
    let pool = &state.db.pool;
    let uuid = Uuid::parse_str(id).ok()?;
    let name = queries::get_coach_name(pool, uuid).await.ok().flatten()?;

    let title = format!("{name} | Camalytics");
    let mut desc = format!("{name} — college basketball coach");
    if let Some(r) = queries::get_coach_rating(pool, uuid).await.ok().flatten() {
        desc.push_str(&format!(
            " · CAE {:+.1} over {} season{}",
            r.cae_shrunk,
            r.n_seasons,
            if r.n_seasons == 1 { "" } else { "s" }
        ));
    }
    desc.push_str(" — on Camalytics.");

    let base = public_base_url();
    let url = format!("{base}/coaches/{uuid}");
    let img = OgImage::generated(
        format!("{base}/og/coaches/{uuid}.png"),
        format!("{name} coach card: above-expectation grade by season"),
    );
    Some(render_tags(&title, &desc, &url, &img))
}

/// The image a card points at: absolute URL + pixel dimensions (the `og:image`
/// width/height hints help unfurlers lay out the preview before the image loads).
struct OgImage {
    url: String,
    w: u32,
    h: u32,
    /// Alt text, read out by screen readers on X and other unfurlers.
    alt: String,
}

impl OgImage {
    /// A generated 1200x630 card (`routes::og_image`). Each card route
    /// redirects to the static `/og-image.png` when it cannot render, so the
    /// tag never points at nothing.
    fn generated(url: String, alt: String) -> Self {
        Self {
            url,
            w: crate::routes::og_image::CARD_W,
            h: crate::routes::og_image::CARD_H,
            alt,
        }
    }
}

/// Build the replacement `<head>` tag block. All dynamic values are HTML-escaped
/// (names can contain `&`, `'`, `"`, e.g. "Texas A&M", "Saint Mary's").
fn render_tags(title: &str, desc: &str, url: &str, img: &OgImage) -> String {
    render_tags_with(title, desc, url, url, img)
}

/// [`render_tags`] with a `rel="canonical"` that differs from `og:url`: a page
/// whose share link carries state the canonical deliberately drops.
fn render_tags_with(
    title: &str,
    desc: &str,
    canonical: &str,
    og_url: &str,
    img: &OgImage,
) -> String {
    let t = esc(title);
    let d = esc(desc);
    let c = esc(canonical);
    let u = esc(og_url);
    let img_url = esc(&img.url);
    let alt = esc(&img.alt);
    let (iw, ih) = (img.w, img.h);
    format!(
        "<title>{t}</title>\n    \
         <link rel=\"canonical\" href=\"{c}\" />\n    \
         <meta name=\"description\" content=\"{d}\" />\n    \
         <meta property=\"og:url\" content=\"{u}\" />\n    \
         <meta property=\"og:title\" content=\"{t}\" />\n    \
         <meta property=\"og:description\" content=\"{d}\" />\n    \
         <meta property=\"og:image\" content=\"{img_url}\" />\n    \
         <meta property=\"og:image:width\" content=\"{iw}\" />\n    \
         <meta property=\"og:image:height\" content=\"{ih}\" />\n    \
         <meta property=\"og:image:alt\" content=\"{alt}\" />\n    \
         <meta name=\"twitter:title\" content=\"{t}\" />\n    \
         <meta name=\"twitter:description\" content=\"{d}\" />\n    \
         <meta name=\"twitter:image\" content=\"{img_url}\" />\n    \
         <meta name=\"twitter:image:alt\" content=\"{alt}\" />"
    )
}

/// Remove every `<meta property="og:url" …>` tag from `html`, whatever the
/// whitespace or line structure around it. Scans tag-by-tag rather than
/// line-by-line so a minified head behaves the same as a pretty-printed one.
fn strip_og_url_tags(html: &str) -> String {
    // Matched on the property value alone. Keying on a fully-quoted attribute
    // (`property="og:url"`) silently misses the same tag written with single
    // quotes or extra whitespace, which leaves the file's homepage `og:url`
    // sitting next to the one we emit — two conflicting tags, and the unfixed
    // one wins for some unfurlers. Inside a `<meta>` tag this substring can
    // only be that property.
    const NEEDLE: &str = "og:url";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find("<meta") {
        // Everything up to this tag is kept verbatim.
        let after_open = &rest[open..];
        let Some(close) = after_open.find('>') else {
            break; // Unterminated tag — keep the remainder as-is.
        };
        let tag = &after_open[..=close];
        if tag.contains(NEEDLE) {
            // Drop the tag, and the indentation that was leading up to it, so
            // removal doesn't leave a ragged blank line behind.
            out.push_str(rest[..open].trim_end_matches([' ', '\t']));
        } else {
            out.push_str(&rest[..open]);
            out.push_str(tag);
        }
        rest = &after_open[close + 1..];
    }
    out.push_str(rest);
    // Collapse any blank lines a removal left behind.
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
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
    fn share_url_can_differ_from_the_canonical() {
        let img = OgImage::generated(
            "https://camalytics.org/og/predict.png?home=Duke&away=UNC".into(),
            "Duke vs UNC".into(),
        );
        let out = render_tags_with(
            "t",
            "d",
            "https://camalytics.org/predict",
            "https://camalytics.org/predict?home=Duke&away=UNC",
            &img,
        );
        assert!(out.contains(r#"<link rel="canonical" href="https://camalytics.org/predict" />"#));
        assert!(out.contains(
            r#"<meta property="og:url" content="https://camalytics.org/predict?home=Duke&amp;away=UNC" />"#
        ));
        assert!(out.contains(r#"name="twitter:image:alt" content="Duke vs UNC""#));
    }

    #[test]
    fn escapes_special_chars() {
        assert_eq!(esc("Texas A&M"), "Texas A&amp;M");
        assert_eq!(esc("Saint Mary's"), "Saint Mary&#39;s");
        assert_eq!(esc("a\"b<c>"), "a&quot;b&lt;c&gt;");
    }

    /// Build a template the way `load` does, from an in-memory document.
    /// The origin the assertions expect. Read through `public_base_url` rather
    /// than hardcoded so these stay green for a dev with `PUBLIC_BASE_URL` set;
    /// the default value itself is pinned by `sitemap`'s own tests.
    fn base() -> String {
        public_base_url()
    }

    /// Count real `<meta …og:url…>` tags in a rendered document.
    ///
    /// A plain substring count over the whole page is wrong twice over:
    /// `index.html` discusses `og:url` in its head comments, and keying on
    /// `property="og:url"` would miss the same tag written with single quotes —
    /// which is exactly the reformat this is here to catch.
    fn count_og_url_tags(html: &str) -> usize {
        let mut n = 0;
        let mut rest = html;
        while let Some(open) = rest.find("<meta") {
            let after = &rest[open..];
            let Some(close) = after.find('>') else { break };
            if after[..=close].contains("og:url") {
                n += 1;
            }
            rest = &after[close + 1..];
        }
        n
    }

    fn template(html: &str) -> SpaTemplate {
        SpaTemplate {
            full: html.to_string(),
            region: SpaTemplate::split(html),
            default_meta: SpaTemplate::default_meta(html).unwrap_or_default(),
        }
    }

    /// Shaped like the real `index.html` head: a default title/description plus
    /// the homepage `og:url` the server is expected to replace per path.
    const DOC: &str = "<head>\n\
        <!--SSR_META_START-->\n\
        <title>x</title>\n\
        <meta name=\"description\" content=\"d\" />\n\
        <meta property=\"og:url\" content=\"https://camalytics.org/\" />\n\
        <meta property=\"og:image\" content=\"https://camalytics.org/og-image.png\" />\n\
        <!--SSR_META_END-->\n\
        </head>";

    #[test]
    fn template_splits_and_renders() {
        let t = template(DOC);
        assert!(t.region.is_some());
        let out = t.render("<title>NEW</title>");
        assert!(out.contains("<title>NEW</title>"));
        assert!(!out.contains("<title>x</title>"));
        assert!(out.contains("</head>"));
    }

    #[test]
    fn template_without_markers_serves_full() {
        let html = "<head><title>x</title></head>";
        let t = template(html);
        assert!(t.region.is_none());
        assert_eq!(t.render("ignored"), html);
        assert_eq!(t.render_default_with_canonical("/players"), html);
    }

    #[test]
    fn default_meta_keeps_the_text_defaults_and_drops_the_static_og_url() {
        let meta = SpaTemplate::default_meta(DOC).expect("markers present");
        assert!(meta.contains("<title>x</title>"));
        assert!(meta.contains(r#"name="description""#));
        assert!(meta.contains(r#"property="og:image""#));
        assert!(!meta.contains(r#"property="og:url""#));
    }

    /// The whole point of the two-host arrangement: a non-entity page still
    /// names one indexable URL, so `campom.org/players` and
    /// `camalytics.org/players` are one page to a crawler, not two.
    #[test]
    fn non_entity_pages_get_exactly_one_canonical_and_one_og_url() {
        let out = template(DOC).render_default_with_canonical("/players");
        assert_eq!(
            out.matches(r#"<link rel="canonical""#).count(),
            1,
            "exactly one canonical: {out}"
        );
        assert_eq!(
            count_og_url_tags(&out),
            1,
            "the static homepage og:url must be replaced, not duplicated: {out}"
        );
        let b = base();
        assert!(out.contains(&format!(r#"<link rel="canonical" href="{b}/players" />"#)));
        assert!(out.contains(&format!(
            r#"<meta property="og:url" content="{b}/players" />"#
        )));
        // Text defaults survive untouched.
        assert!(out.contains("<title>x</title>"));
    }

    /// The failure a line-based filter would have shipped: if anything ever
    /// minifies the head onto one line, tag-scoped removal must still drop only
    /// the `og:url` and keep the rest.
    #[test]
    fn a_minified_head_still_yields_exactly_one_canonical() {
        let minified = concat!(
            "<head><!--SSR_META_START--><title>x</title>",
            r#"<meta name="description" content="d" />"#,
            r#"<meta property="og:url" content="https://camalytics.org/" />"#,
            r#"<meta property="og:image" content="https://camalytics.org/og-image.png" />"#,
            "<!--SSR_META_END--></head>"
        );
        let t = template(minified);
        assert!(
            !t.default_meta.is_empty(),
            "minified head must still parse: {:?}",
            t.default_meta
        );
        let out = t.render_default_with_canonical("/players");
        assert_eq!(out.matches(r#"<link rel="canonical""#).count(), 1);
        assert_eq!(count_og_url_tags(&out), 1);
        assert!(out.contains(&format!(r#"href="{}/players""#, base())));
        // The neighbouring tags survive — removal is scoped to the one tag.
        assert!(out.contains("<title>x</title>"));
        assert!(out.contains(r#"property="og:image""#));
    }

    /// Every other test here runs against a synthetic fixture, so nothing
    /// catches a reformat of the file we actually ship. This one renders the
    /// real `web/index.html`: if its `og:url` is ever rewritten in a shape the
    /// stripper misses, the page would serve two conflicting `og:url` tags, or
    /// none at all, and no other test would notice.
    #[test]
    fn the_real_index_html_renders_one_canonical_and_one_og_url() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("manifest dir has a grandparent")
            .join("web/index.html");
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        let t = template(&html);
        assert!(
            t.region.is_some(),
            "web/index.html lost its SSR_META markers"
        );
        assert!(
            !t.default_meta.is_empty(),
            "web/index.html's SSR_META region did not parse"
        );

        let out = t.render_default_with_canonical("/predict");
        assert_eq!(
            out.matches(r#"<link rel="canonical""#).count(),
            1,
            "real index.html must yield exactly one canonical"
        );
        assert_eq!(
            count_og_url_tags(&out),
            1,
            "the file's static og:url must be replaced, not left alongside ours"
        );
        assert!(out.contains(&format!(r#"content="{}/predict""#, base())));
        // The text defaults the file owns still survive the round-trip.
        assert!(out.contains("<title>"));
        assert!(out.contains(r#"name="description""#));
        assert!(out.contains(r#"property="og:image""#));
    }

    #[test]
    fn canonical_collapses_query_strings_and_trailing_slashes() {
        // `Uri::path()` already excludes the query; the trailing slash and the
        // root are what this has to get right.
        let b = base();
        assert_eq!(canonical_url("/"), format!("{b}/"));
        assert_eq!(canonical_url(""), format!("{b}/"));
        assert_eq!(canonical_url("/players"), format!("{b}/players"));
        assert_eq!(canonical_url("/players/"), format!("{b}/players"));
        // `/index.html` folds onto the homepage rather than becoming a second
        // URL for it.
        assert_eq!(canonical_url("/index.html"), format!("{b}/"));
    }
}
