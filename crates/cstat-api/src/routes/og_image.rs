//! Generated link-preview images for player, team and coach pages and
//! Predict-page matchups (`GET /og/players/{id}.png`, `/og/teams/{id}.png`,
//! `/og/coaches/{id}.png`, `/og/predict.png?home=&away=…`).
//!
//! Renders 1200x630 PNG cards from SVG templates via `resvg`: a player's name,
//! team, archetype, CAM, stat strip and skill radar; a team's rating,
//! efficiency row, recent form and top players; a coach's career grade and
//! its season-by-season bars; a matchup's projected score, win probability
//! and spread, from the same handler `/api/predict` runs. Uses a vendored DejaVu font
//! (loaded once) so it works in the slim Docker runtime without system fonts.
//! The per-page meta injector (`routes::meta`) points each page's `og:image`
//! at its card.
//!
//! Fail-soft: a non-UUID id, a missing player, team or coach, or any render error
//! redirects to the static `/og-image.png`, so a shared link always resolves
//! to *some* card.

use std::sync::{Arc, OnceLock};

use axum::{
    extract::{Path, Query, RawQuery, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use resvg::{tiny_skia, usvg};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::predict::{self, PredictParams};
use super::social_cards::{
    Axis, cam_tier, class_color, radar_axes, radar_chart, season_label, top_players,
};
use crate::AppState;
use axum::Json;
use chrono::NaiveDate;
use cstat_core::projection::Venue;
use cstat_core::queries;

const FONT_REGULAR: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf");

/// Card dimensions. 1200x630 is the canonical Open Graph size (1.91:1).
pub const CARD_W: u32 = 1200;
pub const CARD_H: u32 = 630;

// Brand palette, matching the navbar chrome (web/src/index.css) so a shared
// card and the page it links to look like the same product.
pub(crate) const BG: &str = "#041f3f"; // --brand-navy-deep, matches the site theme-color
pub(crate) const ACCENT: &str = "#8fc0e8"; // --brand-blue-bright
pub(crate) const FG: &str = "#ffffff";
pub(crate) const MUTED: &str = "#9db8d4";
pub(crate) const LIGHT: &str = "#e5e7eb";
pub(crate) const RULE: &str = "#0b3f78"; // --brand-navy-line

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
                // Edge-cache a few hours; CAM values only move on the nightly.
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

async fn render_player_card(state: &AppState, id: &str, season: Option<i32>) -> Option<Vec<u8>> {
    let card = load_player_card(state, id, season).await?;
    rasterize(&build_svg(&card))
}

/// Everything the link-preview card shows.
pub(crate) struct PlayerCard {
    pub name: String,
    /// "Duke · ACC".
    pub team_line: String,
    /// Position, class, height.
    pub bio: Vec<String>,
    pub archetype: Option<String>,
    pub cam: Option<f64>,
    pub cam_o: Option<f64>,
    pub cam_d: Option<f64>,
    /// Top-right label: which season the numbers are from, and games played.
    pub period: String,
    /// The stat strip: (label, formatted value).
    pub stats: Vec<(&'static str, String)>,
    /// Radar spokes; empty when there are no percentiles to draw.
    pub axes: Vec<Axis>,
}

async fn load_player_card(state: &AppState, id: &str, season: Option<i32>) -> Option<PlayerCard> {
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

    // The numbers come from this season once he has played in it. Before that
    // (a returner in the preseason) they come from last season, labelled as
    // such, so a link shared in October still shows who the player is.
    let played = |ss: &Option<queries::PlayerSeasonStatsRow>| {
        ss.as_ref().is_some_and(|s| s.games_played > 0)
    };
    let mut stat_season = season;
    let mut stat_pid = resolved;
    let mut ss = queries::get_player_season_stats(pool, resolved, season)
        .await
        .ok()
        .flatten();
    if !played(&ss)
        && let Some(prev) = queries::resolve_player_id_for_season(pool, uuid, season - 1)
            .await
            .ok()
            .flatten()
    {
        let prev_ss = queries::get_player_season_stats(pool, prev, season - 1)
            .await
            .ok()
            .flatten();
        if played(&prev_ss) {
            stat_season = season - 1;
            stat_pid = prev;
            ss = prev_ss;
        }
    }
    let ss = ss.filter(|s| s.games_played > 0);

    let tv = queries::get_torvik_stats(pool, stat_pid, stat_season)
        .await
        .ok()
        .flatten();
    let pc = queries::get_player_percentiles(pool, stat_pid, stat_season)
        .await
        .ok()
        .flatten();
    let mut archetype = None;
    for (pid, s) in [(resolved, season), (stat_pid, stat_season)] {
        archetype = queries::get_player_archetype(pool, pid, s)
            .await
            .ok()
            .flatten()
            .map(|a| a.primary_class)
            .filter(|c| !c.is_empty());
        if archetype.is_some() {
            break;
        }
    }

    // The display CAM, as the player page shows it: the blended rating, else
    // the projection for a player Torvik does not cover.
    let (cam, cam_o, cam_d) = match tv.as_ref().and_then(|t| t.campom) {
        Some(c) => (
            Some(c),
            tv.as_ref().and_then(|t| t.campom_o),
            tv.as_ref().and_then(|t| t.campom_d),
        ),
        None => (
            ss.as_ref().and_then(|s| s.projected_cam),
            ss.as_ref().and_then(|s| s.projected_cam_o),
            ss.as_ref().and_then(|s| s.projected_cam_d),
        ),
    };

    let axes = match &pc {
        Some(pc) => radar_axes(
            Some(pc),
            ss.as_ref().map(|s| s.games_played),
            tv.as_ref().and_then(|t| t.tpa),
            tv.as_ref().and_then(|t| t.dgbpm_pct),
        ),
        None => Vec::new(),
    };
    let axes = if axes.iter().all(|a| a.value.is_none()) {
        Vec::new()
    } else {
        axes
    };

    let mut period = season_label(stat_season);
    if stat_season != season {
        period.push_str(" stats");
        // A transfer's numbers are from somewhere else; say where.
        let prev_team = queries::get_player_by_id(pool, stat_pid, stat_season)
            .await
            .ok()
            .flatten()
            .and_then(|p| p.team_name);
        if let Some(t) = prev_team.filter(|t| Some(t) != prof.team_name.as_ref()) {
            period = format!("{period} · {t}");
        }
    }
    if let Some(s) = &ss {
        period = format!("{period} · {} GP", s.games_played);
    }

    let mut stats = Vec::new();
    if let Some(s) = &ss {
        for (label, v) in [("PPG", s.ppg), ("RPG", s.rpg), ("APG", s.apg)] {
            if let Some(v) = v {
                stats.push((label, format!("{v:.1}")));
            }
        }
        if let Some(ts) = s.true_shooting_pct {
            stats.push(("TS%", format!("{:.1}", ts * 100.0)));
        }
    }

    let team_line = [prof.team_name.as_deref(), prof.conference.as_deref()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let mut bio = Vec::new();
    if let Some(pos) = prof.position.as_deref().filter(|s| !s.is_empty()) {
        bio.push(pos.to_string());
    }
    if let Some(cls) = prof.class_year.as_deref().filter(|s| !s.is_empty()) {
        bio.push(cls.to_string());
    }
    if let Some(h) = prof.height_inches.filter(|h| (60..=96).contains(h)) {
        bio.push(format!("{}'{}\"", h / 12, h % 12));
    }

    Some(PlayerCard {
        name: prof.name,
        team_line,
        bio,
        archetype,
        cam,
        cam_o,
        cam_d,
        period,
        stats,
        axes,
    })
}

/// Split a name into a first line and a surname line, keeping a generational
/// suffix with the surname ("Wade Taylor IV" → "Wade" / "Taylor IV").
pub(crate) fn split_name(name: &str) -> (String, String) {
    let toks: Vec<&str> = name.split_whitespace().collect();
    if toks.len() < 2 {
        return (String::new(), name.trim().to_string());
    }
    let is_suffix = |t: &str| {
        matches!(
            t.trim_end_matches('.').to_ascii_lowercase().as_str(),
            "jr" | "sr" | "ii" | "iii" | "iv" | "v"
        )
    };
    let mut cut = toks.len() - 1;
    if cut >= 2 && is_suffix(toks[cut]) {
        cut -= 1;
    }
    (toks[..cut].join(" "), toks[cut..].join(" "))
}

/// Largest font size, up to `max_px`, at which `text` fits `width` pixels.
/// `em` is the face's average advance per character (DejaVu Sans runs wide:
/// about 0.62 regular and 0.70 bold), so this errs toward smaller.
fn fit_px(text: &str, max_px: f64, width: f64, em: f64) -> f64 {
    let n = text.chars().count().max(1) as f64;
    (width / (n * em)).min(max_px).floor()
}

/// Rough rendered width of `text` at `px`, same estimate as [`fit_px`].
fn text_w(text: &str, px: f64, em: f64) -> f64 {
    text.chars().count() as f64 * px * em
}

fn build_svg(c: &PlayerCard) -> String {
    let has_radar = !c.axes.is_empty();
    // The left column stops short of the radar's left-hand labels.
    let col_w = if has_radar { 560.0 } else { 1072.0 };
    let x0 = 64.0;
    let mut out = String::new();

    // Name: surname large on a fixed baseline, first name just above it, so a
    // long surname that shrinks to fit closes up toward the first name instead
    // of opening a gap above the team line.
    let (first, last) = split_name(&c.name);
    let last = truncate(&last, 24);
    let last_px = fit_px(&last, 88.0, col_w, 0.70);
    if !first.is_empty() {
        let first = truncate(&first, 30);
        let px = fit_px(&first, 44.0, col_w, 0.62);
        let y = (240.0 - last_px * 0.82 - 8.0).round();
        out.push_str(&format!(
            r#"<text x="{x0}" y="{y}" font-family="DejaVu Sans" font-size="{px}" fill="{LIGHT}">{}</text>"#,
            xml_escape(&first)
        ));
    }
    out.push_str(&format!(
        r#"<text x="{x0}" y="240" font-family="DejaVu Sans" font-weight="bold" font-size="{last_px}" fill="{FG}">{}</text>"#,
        xml_escape(&last)
    ));
    if !c.team_line.is_empty() {
        let t = truncate(&c.team_line, if has_radar { 30 } else { 60 });
        out.push_str(&format!(
            r#"<text x="{x0}" y="290" font-family="DejaVu Sans" font-size="30" fill="{MUTED}">{}</text>"#,
            xml_escape(&t)
        ));
    }

    // Archetype pill, then position / class / height beside it.
    let mut bx = x0;
    if let Some(class) = &c.archetype {
        let w = (34 + class.chars().count() * 15) as f64;
        let fg = if class == "Rogue" || class == "Paladin" {
            "#111827"
        } else {
            FG
        };
        out.push_str(&format!(
            r#"<rect x="{x0}" y="314" width="{w}" height="40" rx="20" fill="{}"/><text x="{}" y="342" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="21" fill="{fg}">{}</text>"#,
            class_color(class),
            x0 + w / 2.0,
            xml_escape(class)
        ));
        bx += w + 18.0;
    }
    if !c.bio.is_empty() {
        out.push_str(&format!(
            r#"<text x="{bx}" y="343" font-family="DejaVu Sans" font-size="24" fill="{LIGHT}">{}</text>"#,
            xml_escape(&c.bio.join(" · "))
        ));
    }

    // CAM: the number in its tier color, tier and halves beside and below.
    if let Some(cam) = c.cam {
        let (tier, band) = cam_tier(cam);
        let num = format!("{cam:+.1}");
        let lx = x0 + text_w(&num, 88.0, 0.66) + 22.0;
        out.push_str(&format!(
            r#"<text x="{x0}" y="460" font-family="DejaVu Sans" font-weight="bold" font-size="88" fill="{band}">{num}</text>
<text x="{lx}" y="418" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="3" fill="{MUTED}">CAM</text>
<text x="{lx}" y="452" font-family="DejaVu Sans" font-size="24" fill="{LIGHT}">{tier}</text>"#
        ));
        if let (Some(o), Some(d)) = (c.cam_o, c.cam_d) {
            out.push_str(&format!(
                r#"<text x="{x0}" y="500" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">CAMO <tspan font-weight="bold" fill="{LIGHT}">{o:+.1}</tspan><tspan dx="28">CAMD </tspan><tspan font-weight="bold" fill="{LIGHT}">{d:+.1}</tspan></text>"#
            ));
        }
    }

    // Stat strip.
    if !c.stats.is_empty() {
        out.push_str(&format!(
            r#"<line x1="{x0}" y1="522" x2="{}" y2="522" stroke="{RULE}" stroke-width="2"/>"#,
            x0 + (c.stats.len() as f64 * 128.0 - 28.0).max(100.0)
        ));
        for (i, (label, v)) in c.stats.iter().enumerate() {
            let x = x0 + i as f64 * 128.0;
            out.push_str(&format!(
                r#"<text x="{x}" y="568" font-family="DejaVu Sans" font-weight="bold" font-size="34" fill="{FG}">{}</text><text x="{x}" y="594" font-family="DejaVu Sans" font-size="18" letter-spacing="2" fill="{MUTED}">{label}</text>"#,
                xml_escape(v)
            ));
        }
    }

    // Radar.
    if has_radar {
        let (cx, cy) = (915.0, 330.0);
        let color = c.archetype.as_deref().map(class_color).unwrap_or(ACCENT);
        out.push_str(&radar_chart(&c.axes, color, (cx, cy), 150.0, 20.0));
        out.push_str(&format!(
            r#"<text x="{cx}" y="566" text-anchor="middle" font-family="DejaVu Sans" font-size="18" fill="{MUTED}">Percentile among D-I players</text>"#
        ));
    }

    // A player with nothing but a bio (a freshman before opening night) gets
    // the name block centered rather than stranded at the top of an empty card.
    let shift = if c.cam.is_none() && c.stats.is_empty() && !has_radar {
        110
    } else {
        0
    };

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="12" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="{x0}" y="78" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
  <text x="1136" y="78" text-anchor="end" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">{}</text>
  <g transform="translate(0 {shift})">{out}</g>
  <text x="1136" y="602" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{MUTED}">camalytics.org</text>
</svg>"##,
        xml_escape(&c.period)
    )
}

// ---------------------------------------------------------------------------
// Team card
// ---------------------------------------------------------------------------

/// `GET /og/teams/{id}.png?season=` — the team page's link-preview card, else
/// the static one. Distinct from the bot's `/og/team/{id}` outlook card, which
/// is 16:9 and built for a preseason post.
pub async fn team_card(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<OgParams>,
) -> Response {
    match render_team_card(&state, &id, p.season).await {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

/// One cell of the efficiency row: label, value, national rank.
pub(crate) struct TeamStat {
    pub label: &'static str,
    pub value: String,
    pub rank: Option<i64>,
}

/// Everything the team link-preview card shows.
pub(crate) struct TeamOgCard {
    pub name: String,
    /// "ACC · 4-1".
    pub sub: String,
    pub season: String,
    /// Headline: national rank, AdjEM, and what it is ("Adj. efficiency
    /// margin", or "Preseason projection" before the team has played).
    pub rating: Option<(i64, f64, &'static str)>,
    pub stats: Vec<TeamStat>,
    /// Most recent results, oldest first: true = win.
    pub form: Vec<bool>,
    /// "Next: vs Kansas · Nov 21".
    pub next: Option<String>,
    pub roster_label: String,
    pub players: Vec<(String, f64, Option<String>)>,
}

async fn render_team_card(state: &AppState, id: &str, season: Option<i32>) -> Option<Vec<u8>> {
    let card = load_team_card(state, id, season).await?;
    rasterize(&team_svg(&card))
}

async fn load_team_card(state: &AppState, id: &str, season: Option<i32>) -> Option<TeamOgCard> {
    let pool = &state.db.pool;
    let id = id.strip_suffix(".png").unwrap_or(id);
    let uuid = Uuid::parse_str(id).ok()?;
    let season = season.unwrap_or_else(crate::default_season);
    let tid = queries::resolve_team_id_for_season(pool, uuid, season)
        .await
        .ok()
        .flatten()
        .unwrap_or(uuid);
    let t = queries::get_team_by_id(pool, tid, season)
        .await
        .ok()
        .flatten()?;
    let pre = queries::get_team_card(pool, tid, season)
        .await
        .ok()
        .flatten();

    let (w, l) = (t.wins.unwrap_or(0), t.losses.unwrap_or(0));
    let played = w + l > 0;
    let mut sub: Vec<String> = Vec::new();
    if let Some(c) = t.conference.as_deref().filter(|s| !s.is_empty()) {
        sub.push(c.to_string());
    }
    if played {
        sub.push(format!("{w}-{l}"));
    }

    // Before a team has played, its rating IS its projection, so say so.
    let preseason = pre
        .as_ref()
        .and_then(|p| Some((p.preseason_rank?, f64::from(p.preseason_adj_em?))));
    let rated = t
        .adj_efficiency_margin_rank
        .zip(t.adj_efficiency_margin)
        .map(|(r, em)| (r, em, "Adj. efficiency margin"));
    let rating = match (played, preseason) {
        (true, _) | (false, None) => rated,
        (false, Some((r, em))) => Some((r, em, "Preseason projection")),
    };

    let mut stats = Vec::new();
    if played {
        let cells = [
            (
                "OFFENSE",
                t.adj_offense.map(|v| format!("{v:.1}")),
                t.adj_offense_rank,
            ),
            (
                "DEFENSE",
                t.adj_defense.map(|v| format!("{v:.1}")),
                t.adj_defense_rank,
            ),
            (
                "TEMPO",
                t.adj_tempo.map(|v| format!("{v:.1}")),
                t.adj_tempo_rank,
            ),
            (
                "SOS",
                t.sos.map(|v| format!("{v:+.1}")),
                t.sos_rank.map(i64::from),
            ),
        ];
        for (label, value, rank) in cells {
            if let Some(value) = value {
                stats.push(TeamStat { label, value, rank });
            }
        }
    }

    let schedule = queries::get_team_schedule(pool, tid, season)
        .await
        .unwrap_or_default();
    let results: Vec<bool> = schedule
        .iter()
        .filter_map(|g| Some(g.team_score? > g.opponent_score?))
        .collect();
    let form = results[results.len().saturating_sub(5)..].to_vec();
    let today = chrono::Utc::now().date_naive();
    let next = schedule
        .iter()
        .find(|g| g.team_score.is_none() && g.game_date >= today)
        .and_then(|g| {
            let opp = g.opponent_name.as_deref()?;
            let at = if g.is_home == Some(false) && g.is_neutral != Some(true) {
                "at"
            } else {
                "vs"
            };
            Some(format!(
                "Next: {at} {} · {}",
                truncate(opp, 22),
                g.game_date.format("%b %-d")
            ))
        });

    // Top players by CAM: this season once it has them, else last season's
    // for the same program, labelled.
    let mut players = top_players(pool, tid, season).await;
    let mut roster_label = "Top players by CAM".to_string();
    if players.is_empty()
        && let Some(prev) = queries::resolve_team_id_for_season(pool, uuid, season - 1)
            .await
            .ok()
            .flatten()
    {
        players = top_players(pool, prev, season - 1).await;
        roster_label = format!("Top players by CAM · {}", season_label(season - 1));
    }

    Some(TeamOgCard {
        name: t.name,
        sub: sub.join(" · "),
        season: season_label(season),
        rating,
        stats,
        form,
        next,
        roster_label,
        players,
    })
}

/// Win and loss chip colors: the grade scale's ends.
const WIN: &str = "#4ade80";
const LOSS: &str = "#f87171";

pub(crate) fn team_svg(c: &TeamOgCard) -> String {
    let x0 = 64.0;
    let has_roster = !c.players.is_empty();
    let col_w = if has_roster { 560.0 } else { 1072.0 };
    let mut out = String::new();

    let name = truncate(&c.name, 26);
    let px = fit_px(&name, 76.0, col_w, 0.70);
    out.push_str(&format!(
        r#"<text x="{x0}" y="186" font-family="DejaVu Sans" font-weight="bold" font-size="{px}" fill="{FG}">{}</text>"#,
        xml_escape(&name)
    ));
    if !c.sub.is_empty() {
        out.push_str(&format!(
            r#"<text x="{x0}" y="232" font-family="DejaVu Sans" font-size="30" fill="{MUTED}">{}</text>"#,
            xml_escape(&truncate(&c.sub, 34))
        ));
    }

    // Rank large, the margin and what it measures on the line beneath, so a
    // three-digit rank never pushes the label into the roster column.
    if let Some((rank, em, what)) = c.rating {
        out.push_str(&format!(
            r#"<text x="{x0}" y="330" font-family="DejaVu Sans" font-weight="bold" font-size="84" fill="{ACCENT}">No. {rank}</text>
<text x="{x0}" y="372" font-family="DejaVu Sans" font-size="22" fill="{MUTED}"><tspan font-weight="bold" font-size="28" fill="{FG}">{em:+.1}</tspan> {}</text>"#,
            what.to_lowercase()
        ));
    }

    // Efficiency row.
    for (i, s) in c.stats.iter().enumerate() {
        let x = x0 + i as f64 * 140.0;
        let rank = s.rank.map(|r| format!("No. {r}")).unwrap_or_default();
        out.push_str(&format!(
            r#"<text x="{x}" y="420" font-family="DejaVu Sans" font-size="17" letter-spacing="2" fill="{MUTED}">{}</text><text x="{x}" y="456" font-family="DejaVu Sans" font-weight="bold" font-size="32" fill="{FG}">{}</text><text x="{x}" y="482" font-family="DejaVu Sans" font-size="20" fill="{ACCENT}">{rank}</text>"#,
            s.label,
            xml_escape(&s.value)
        ));
    }

    // Recent form chips, then the next game.
    if !c.form.is_empty() {
        out.push_str(&format!(
            r#"<line x1="{x0}" y1="508" x2="{}" y2="508" stroke="{RULE}" stroke-width="2"/><text x="{x0}" y="557" font-family="DejaVu Sans" font-size="17" letter-spacing="2" fill="{MUTED}">LAST {}</text>"#,
            x0 + 532.0,
            c.form.len()
        ));
        for (i, won) in c.form.iter().enumerate() {
            let x = x0 + 100.0 + i as f64 * 46.0;
            let (fill, letter) = if *won { (WIN, "W") } else { (LOSS, "L") };
            out.push_str(&format!(
                r##"<rect x="{x}" y="530" width="38" height="38" rx="8" fill="{fill}"/><text x="{}" y="558" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="22" fill="#0b1220">{letter}</text>"##,
                x + 19.0
            ));
        }
    }
    if let Some(next) = &c.next {
        let y = if c.form.is_empty() { 556 } else { 602 };
        out.push_str(&format!(
            r#"<text x="{x0}" y="{y}" font-family="DejaVu Sans" font-size="22" fill="{LIGHT}">{}</text>"#,
            xml_escape(next)
        ));
    }

    // Roster column.
    if has_roster {
        out.push_str(&format!(
            r#"<text x="680" y="150" font-family="DejaVu Sans" font-weight="bold" font-size="20" letter-spacing="3" fill="{ACCENT}">{}</text>"#,
            xml_escape(&c.roster_label.to_uppercase())
        ));
        for (i, (name, cam, class)) in c.players.iter().enumerate() {
            let y = 212 + i as i32 * 76;
            let (_, band) = cam_tier(*cam);
            // Leave room for the CAM value at the right edge.
            let name = truncate(name, 24);
            let name_px = fit_px(&name, 28.0, 316.0, 0.68);
            out.push_str(&format!(
                r#"<line x1="680" y1="{}" x2="1136" y2="{}" stroke="{RULE}" stroke-width="2"/>
<text x="680" y="{y}" font-family="DejaVu Sans" font-weight="bold" font-size="{name_px}" fill="{FG}">{}</text>
<text x="680" y="{}" font-family="DejaVu Sans" font-size="20" fill="{}">{}</text>
<text x="1136" y="{}" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="38" fill="{band}">{cam:+.1}</text>"#,
                y - 40,
                y - 40,
                xml_escape(&name),
                y + 26,
                class.as_deref().map(class_color).unwrap_or(MUTED),
                xml_escape(class.as_deref().unwrap_or("")),
                y + 13,
            ));
        }
    }

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="12" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="{x0}" y="78" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
  <text x="1136" y="78" text-anchor="end" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">{}</text>
  {out}
  <text x="1136" y="602" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{MUTED}">camalytics.org</text>
</svg>"##,
        xml_escape(&c.season)
    )
}

// ---------------------------------------------------------------------------
// Coach card
// ---------------------------------------------------------------------------

/// `GET /og/coaches/{id}.png` — the coach page's link-preview card, else the
/// static one. Coaches are not season-scoped, so there is no `?season=`.
pub async fn coach_card(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match render_coach_card(&state, &id).await {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

/// The career grade, as the coach page's header shows it.
pub(crate) struct CoachGrade {
    pub cae: f64,
    pub ci_low: f64,
    pub ci_high: f64,
}

/// Everything the coach link-preview card shows.
pub(crate) struct CoachOgCard {
    pub name: String,
    /// "Saint Mary's · 2015–2027".
    pub sub: String,
    /// "9 of 11 seasons scored".
    pub coverage: String,
    pub grade: Option<CoachGrade>,
    /// Career-mean team AdjEM / AdjO / AdjD.
    pub strength: Vec<(&'static str, String)>,
    /// One entry per season worked, oldest first: (season, raw CAE if graded).
    pub seasons: Vec<(i32, Option<f64>)>,
}

async fn render_coach_card(state: &AppState, id: &str) -> Option<Vec<u8>> {
    let card = load_coach_card(state, id).await?;
    rasterize(&coach_svg(&card))
}

async fn load_coach_card(state: &AppState, id: &str) -> Option<CoachOgCard> {
    let pool = &state.db.pool;
    let id = id.strip_suffix(".png").unwrap_or(id);
    let uuid = Uuid::parse_str(id).ok()?;
    let name = queries::get_coach_name(pool, uuid).await.ok().flatten()?;
    let rating = queries::get_coach_rating(pool, uuid).await.ok().flatten();
    let seasons = queries::get_coach_seasons(pool, uuid)
        .await
        .unwrap_or_default();

    let mut sub = Vec::new();
    if let Some(team) = seasons.iter().rev().find_map(|s| s.team_name.clone()) {
        sub.push(team);
    }
    if let (Some(first), Some(last)) = (seasons.first(), seasons.last()) {
        sub.push(tenure_span(first.season, last.season));
    }
    let scored = seasons.iter().filter(|s| s.cae_raw.is_some()).count();
    let coverage = if seasons.is_empty() {
        String::new()
    } else {
        format!(
            "{scored} of {} {} scored",
            seasons.len(),
            if seasons.len() == 1 {
                "season"
            } else {
                "seasons"
            }
        )
    };

    let grade = rating.as_ref().map(|r| CoachGrade {
        cae: r.cae_shrunk,
        ci_low: r.ci_low,
        ci_high: r.ci_high,
    });
    let mut strength = Vec::new();
    if let Some(r) = &rating {
        for (label, v) in [
            ("TEAM ADJEM", r.career_adj_em.map(|v| format!("{v:+.1}"))),
            ("ADJ. OFFENSE", r.career_adj_o.map(|v| format!("{v:.1}"))),
            ("ADJ. DEFENSE", r.career_adj_d.map(|v| format!("{v:.1}"))),
        ] {
            if let Some(v) = v {
                strength.push((label, v));
            }
        }
    }

    Some(CoachOgCard {
        name,
        sub: sub.join(" · "),
        coverage,
        grade,
        strength,
        seasons: seasons.iter().map(|s| (s.season, s.cae_raw)).collect(),
    })
}

/// "2015–2027", or "2024" for a single season (`web/src/components/cae.ts`).
fn tenure_span(first: i32, last: i32) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first}–{last}")
    }
}

/// Signed CAE with no stray "-0.0" (`fmtCae` in `web/src/components/cae.ts`).
pub(crate) fn fmt_cae(v: f64) -> String {
    let s = format!("{v:.1}");
    if s == "0.0" || s == "-0.0" {
        "0.0".into()
    } else if v > 0.0 {
        format!("+{s}")
    } else {
        s
    }
}

/// Diverging gray → green (over) / red (under), saturating at ±4 AdjEM, the
/// practical range of the shrunk rating (`caeColor` in `cae.ts`).
pub(crate) fn cae_color(v: f64) -> String {
    let t = (v / 4.0).clamp(-1.0, 1.0);
    let (to, a) = if t >= 0.0 {
        ((0x4a, 0xde, 0x80), t)
    } else {
        ((0xf8, 0x71, 0x71), -t)
    };
    let mix = |from: i32, to: i32| (from as f64 + (to - from) as f64 * a).round() as i32;
    format!(
        "rgb({}, {}, {})",
        mix(0xe5, to.0),
        mix(0xe7, to.1),
        mix(0xeb, to.2)
    )
}

pub(crate) fn coach_svg(c: &CoachOgCard) -> String {
    let x0 = 64.0;
    let graded: Vec<f64> = c.seasons.iter().filter_map(|s| s.1).collect();
    let has_chart = !graded.is_empty();
    let col_w = if has_chart { 560.0 } else { 1072.0 };
    let mut out = String::new();

    // Name, bottom-anchored like the player card.
    let (first, last) = split_name(&c.name);
    let last = truncate(&last, 24);
    let last_px = fit_px(&last, 80.0, col_w, 0.70);
    if !first.is_empty() {
        let first = truncate(&first, 30);
        let px = fit_px(&first, 40.0, col_w, 0.62);
        let y = (222.0 - last_px * 0.82 - 8.0).round();
        out.push_str(&format!(
            r#"<text x="{x0}" y="{y}" font-family="DejaVu Sans" font-size="{px}" fill="{LIGHT}">{}</text>"#,
            xml_escape(&first)
        ));
    }
    out.push_str(&format!(
        r#"<text x="{x0}" y="222" font-family="DejaVu Sans" font-weight="bold" font-size="{last_px}" fill="{FG}">{}</text>"#,
        xml_escape(&last)
    ));
    if !c.sub.is_empty() {
        out.push_str(&format!(
            r#"<text x="{x0}" y="270" font-family="DejaVu Sans" font-size="28" fill="{MUTED}">{}</text>"#,
            xml_escape(&truncate(&c.sub, 36))
        ));
    }

    match &c.grade {
        Some(g) => {
            let num = fmt_cae(g.cae);
            let lx = x0 + text_w(&num, 88.0, 0.66) + 22.0;
            out.push_str(&format!(
                r#"<text x="{x0}" y="386" font-family="DejaVu Sans" font-weight="bold" font-size="88" fill="{}">{num}</text>
<text x="{lx}" y="344" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="3" fill="{MUTED}">CAE</text>
<text x="{lx}" y="378" font-family="DejaVu Sans" font-size="22" fill="{LIGHT}">above expectation</text>
<text x="{x0}" y="426" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">95% interval {} to {} · {}</text>"#,
                cae_color(g.cae),
                fmt_cae(g.ci_low),
                fmt_cae(g.ci_high),
                xml_escape(&c.coverage),
            ));
        }
        None if !c.coverage.is_empty() => {
            out.push_str(&format!(
                r#"<text x="{x0}" y="360" font-family="DejaVu Sans" font-size="26" fill="{LIGHT}">Not yet graded</text>
<text x="{x0}" y="396" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">{}</text>"#,
                xml_escape(&c.coverage)
            ));
        }
        None => {}
    }

    // Career team strength: context, not part of the grade.
    if !c.strength.is_empty() {
        out.push_str(&format!(
            r#"<line x1="{x0}" y1="466" x2="{}" y2="466" stroke="{RULE}" stroke-width="2"/>"#,
            x0 + 532.0
        ));
        for (i, (label, v)) in c.strength.iter().enumerate() {
            let x = x0 + i as f64 * 184.0;
            out.push_str(&format!(
                r#"<text x="{x}" y="506" font-family="DejaVu Sans" font-size="17" letter-spacing="2" fill="{MUTED}">{label}</text><text x="{x}" y="546" font-family="DejaVu Sans" font-weight="bold" font-size="32" fill="{FG}">{}</text>"#,
                xml_escape(v)
            ));
        }
        out.push_str(&format!(
            r#"<text x="{x0}" y="576" font-family="DejaVu Sans" font-size="17" fill="{MUTED}">Career team strength</text>"#
        ));
    }

    // Per-season bars: actual minus projection, by season. Ungraded seasons
    // keep their slot (time stays continuous) and show a small dash.
    if has_chart {
        let seasons: Vec<&(i32, Option<f64>)> = c.seasons.iter().rev().take(14).rev().collect();
        let (left, right, top, bottom) = (680.0, 1136.0, 196.0, 506.0);
        let lim = graded.iter().fold(4.0_f64, |m, v| m.max(v.abs())).min(20.0);
        let zero = (top + bottom) / 2.0;
        let scale = (bottom - top) / 2.0 / lim;
        let slot = (right - left) / seasons.len() as f64;
        let bar = (slot * 0.62).min(34.0);
        out.push_str(&format!(
            r#"<text x="{left}" y="150" font-family="DejaVu Sans" font-weight="bold" font-size="20" letter-spacing="3" fill="{ACCENT}">ABOVE EXPECTATION BY SEASON</text>
<line x1="{left}" y1="{zero}" x2="{right}" y2="{zero}" stroke="{RULE}" stroke-width="2"/>"#
        ));
        let every = if seasons.len() > 8 { 2 } else { 1 };
        for (i, (season, v)) in seasons.iter().enumerate() {
            let cx = left + slot * (i as f64 + 0.5);
            match v {
                Some(v) => {
                    let h = (v.abs() * scale).max(2.0);
                    let y = if *v >= 0.0 { zero - h } else { zero };
                    out.push_str(&format!(
                        r#"<rect x="{:.1}" y="{y:.1}" width="{bar:.1}" height="{h:.1}" rx="3" fill="{}"/>"#,
                        cx - bar / 2.0,
                        cae_color(*v)
                    ));
                }
                None => out.push_str(&format!(
                    r#"<line x1="{:.1}" y1="{zero}" x2="{:.1}" y2="{zero}" stroke="{MUTED}" stroke-width="4"/>"#,
                    cx - 6.0,
                    cx + 6.0
                )),
            }
            // Label every other season on a long tenure, always the latest.
            if (seasons.len() - 1 - i).is_multiple_of(every) {
                out.push_str(&format!(
                    r#"<text x="{cx:.1}" y="540" text-anchor="middle" font-family="DejaVu Sans" font-size="18" fill="{MUTED}">'{:02}</text>"#,
                    season % 100
                ));
            }
        }
        out.push_str(&format!(
            r#"<text x="{left}" y="576" font-family="DejaVu Sans" font-size="17" fill="{MUTED}">Team rating minus its roster projection</text>"#
        ));
    }

    // An ungraded coach with no team history is a name and a line; center it.
    let shift = if c.grade.is_none() && c.strength.is_empty() && !has_chart {
        100
    } else {
        0
    };

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="12" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="{x0}" y="78" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
  <text x="1136" y="78" text-anchor="end" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">Head coach</text>
  <g transform="translate(0 {shift})">{out}</g>
  <text x="1136" y="602" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{MUTED}">camalytics.org</text>
</svg>"##
    )
}

// ---------------------------------------------------------------------------
// Matchup (Predict page) card
// ---------------------------------------------------------------------------

/// The Predict page's URL parameters, as a `/predict?…` link carries them.
/// Shared by the page's meta injector and the card route, which forwards the
/// same query string so the two always describe one matchup.
#[derive(Deserialize, Serialize, Default, Clone)]
pub(crate) struct MatchupQuery {
    pub home: Option<String>,
    pub away: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub away_season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of_date: Option<String>,
}

impl MatchupQuery {
    /// Parse a raw query string leniently. A link with a parameter that does
    /// not parse (`season=`, `season=abc`) is treated as naming no matchup, so
    /// the page still serves its default head and the card its static image;
    /// a typed `Query` extractor would answer 400 instead, and `/predict`
    /// served any query string before it had meta of its own.
    pub fn parse(raw: Option<&str>) -> Self {
        raw.and_then(|q| serde_urlencoded::from_str(q).ok())
            .unwrap_or_default()
    }

    /// The predictor's parameters, or `None` when a team is missing. A venue
    /// or date that does not parse is dropped (the page does the same with a
    /// venue it does not recognise) rather than failing the card.
    pub fn to_predict(&self) -> Option<PredictParams> {
        let home = self
            .home
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let away = self
            .away
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let venue = match self.venue.as_deref() {
            Some("home") => Some(Venue::Home),
            Some("away") => Some(Venue::Away),
            Some("neutral") => Some(Venue::Neutral),
            _ => None,
        };
        Some(PredictParams {
            home: home.to_string(),
            away: away.to_string(),
            venue,
            neutral: false,
            season: self.season,
            home_season: self.home_season,
            away_season: self.away_season,
            as_of_date: self
                .as_of_date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        })
    }

    /// `home=…&away=…` with only the fields that are set, for rebuilding the
    /// page and image URLs.
    pub fn query_string(&self) -> String {
        serde_urlencoded::to_string(self).unwrap_or_default()
    }
}

/// How long the card waits on the predictor. The OG routes sit outside the
/// data routes' request timeout, so this is the card's own.
const PREDICT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Matchup cards rendering at once. The OG routes also sit outside the data
/// routes' load shedding, and this one runs the full predictor on every new
/// query string, which a crawler (or anyone varying the query to miss the edge
/// cache) can mint without limit. Past the cap the request gets the static
/// card rather than queueing model runs behind each other.
const MATCHUP_CONCURRENCY: usize = 4;

fn matchup_permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    PERMITS.get_or_init(|| tokio::sync::Semaphore::new(MATCHUP_CONCURRENCY))
}

/// `GET /og/predict.png?home=&away=&venue=&season=…` — the Predict page's
/// link-preview card, from the same handler `/api/predict` runs. Fail-soft
/// like the others: a team that does not resolve, or a prediction that errors
/// or times out, redirects to the static card.
pub async fn matchup_card(State(state): State<Arc<AppState>>, RawQuery(raw): RawQuery) -> Response {
    let q = MatchupQuery::parse(raw.as_deref());
    let png = match matchup_permits().try_acquire() {
        Err(_) => {
            tracing::warn!("og matchup card: at capacity, serving the static card");
            None
        }
        Ok(_permit) => {
            match tokio::time::timeout(PREDICT_TIMEOUT, render_matchup_card(&state, &q)).await {
                Ok(png) => png,
                Err(_) => {
                    tracing::warn!("og matchup card: prediction timed out");
                    None
                }
            }
        }
    };
    match png {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

/// One side of the matchup.
pub(crate) struct MatchupSide {
    pub name: String,
    /// Rank, then conference and record ("No. 12", "Big Ten · 4-1").
    pub rank: Option<i64>,
    pub sub: String,
    pub score: Option<i32>,
    pub win_prob: f64,
    /// Best player by CAM.
    pub star: Option<(String, f64)>,
}

pub(crate) struct MatchupOgCard {
    /// Visitor (or the first-named team at a neutral site).
    pub left: MatchupSide,
    /// Host (or the second-named team).
    pub right: MatchupSide,
    pub neutral: bool,
    /// "Purdue by 3.4 · total 148.5".
    pub line: String,
    /// Top-right label.
    pub label: String,
    /// What the prediction is built on, when the reader should know.
    pub note: Option<String>,
}

async fn render_matchup_card(state: &Arc<AppState>, q: &MatchupQuery) -> Option<Vec<u8>> {
    let card = load_matchup_card(state, q).await?;
    rasterize(&matchup_svg(&card))
}

async fn load_matchup_card(state: &Arc<AppState>, q: &MatchupQuery) -> Option<MatchupOgCard> {
    let params = q.to_predict()?;
    let (home_season, away_season) = params.resolved_seasons(crate::default_season());
    let Json(v) = predict::predict(State(state.clone()), Query(params))
        .await
        .ok()?;

    let pool = &state.db.pool;
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64());
    let int = |k: &str| v.get(k).and_then(|x| x.as_i64()).map(|x| x as i32);
    let id = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let home_name = v.get("home_team")?.as_str()?.to_string();
    let away_name = v.get("away_team")?.as_str()?.to_string();
    let home_p = num("home_win_probability")?;
    let margin = num("predicted_margin")?;
    let total = num("predicted_total");
    let basis = v
        .get("prediction_basis")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let venue = v.get("venue").and_then(|x| x.as_str()).unwrap_or("home");
    let cross_era = home_season != away_season;

    let side = |name: String, tid: Option<Uuid>, season: i32, score: Option<i32>, p: f64| async move {
        let card = match tid {
            Some(t) => queries::get_team_card(pool, t, season).await.ok().flatten(),
            None => None,
        };
        let star = match tid {
            Some(t) => top_players(pool, t, season)
                .await
                .into_iter()
                .next()
                .map(|(n, c, _)| (n, c)),
            None => None,
        };
        let played = card
            .as_ref()
            .is_some_and(|c| c.wins.unwrap_or(0) + c.losses.unwrap_or(0) > 0);
        let rank = card.as_ref().and_then(|c| {
            if played {
                c.adj_em_rank
            } else {
                c.preseason_rank.or(c.adj_em_rank)
            }
        });
        let mut sub = Vec::new();
        if cross_era {
            sub.push(season_label(season));
        }
        if let Some(c) = card.as_ref().and_then(|c| c.conference.clone()) {
            sub.push(c);
        }
        if played && let Some(c) = &card {
            sub.push(format!("{}-{}", c.wins.unwrap_or(0), c.losses.unwrap_or(0)));
        }
        MatchupSide {
            name,
            rank,
            sub: sub.join(" · "),
            score,
            win_prob: p,
            star,
        }
    };
    let home = side(
        home_name.clone(),
        id("home_team_id"),
        home_season,
        int("predicted_home_score"),
        home_p,
    )
    .await;
    let away = side(
        away_name.clone(),
        id("away_team_id"),
        away_season,
        int("predicted_away_score"),
        1.0 - home_p,
    )
    .await;

    // The host sits on the right, as a scoreboard reads. At a neutral site the
    // two keep the order the link named them in.
    let neutral = venue == "neutral";
    let (left, right) = if venue == "away" {
        (home, away)
    } else {
        (away, home)
    };
    let (left, right) = if neutral {
        (right, left)
    } else {
        (left, right)
    };

    let favourite = if margin >= 0.0 {
        &home_name
    } else {
        &away_name
    };
    let mut line = if margin.abs() < 0.05 {
        "Even".to_string()
    } else {
        format!("{} by {:.1}", truncate(favourite, 22), margin.abs())
    };
    if let Some(t) = total {
        line.push_str(&format!(" · total {t:.1}"));
    }
    let note = match basis {
        "preseason_only" => Some("Built on preseason projections".to_string()),
        "preseason" => Some("Early season: leans on preseason projections".to_string()),
        "cross_era" => Some("What-if: these teams are from different seasons".to_string()),
        "pit" => v
            .get("as_of_date")
            .and_then(|d| d.as_str())
            .map(|d| format!("As of {d}")),
        _ => None,
    };
    let label = if cross_era {
        "What-if matchup".to_string()
    } else {
        format!("Game prediction · {}", season_label(home_season))
    };

    Some(MatchupOgCard {
        left,
        right,
        neutral,
        line,
        label,
        note,
    })
}

/// The Predict page's two side colors (`web/src/pages/Predict.tsx`).
const SIDE_LEFT: &str = "#3b82f6";
const SIDE_RIGHT: &str = "#ef4444";

pub(crate) fn matchup_svg(c: &MatchupOgCard) -> String {
    let mut out = String::new();
    let favourite_left = c.left.win_prob >= c.right.win_prob;

    for (s, left) in [(&c.left, true), (&c.right, false)] {
        let (x, anchor) = if left {
            (64.0, "start")
        } else {
            (1136.0, "end")
        };
        let color = if left { SIDE_LEFT } else { SIDE_RIGHT };
        let fav = favourite_left == left;
        if let Some(r) = s.rank {
            out.push_str(&format!(
                r#"<text x="{x}" y="156" text-anchor="{anchor}" font-family="DejaVu Sans" font-weight="bold" font-size="24" fill="{ACCENT}">No. {r}</text>"#
            ));
        }
        let name = truncate(&s.name, 22);
        let px = fit_px(&name, 60.0, 450.0, 0.70);
        out.push_str(&format!(
            r#"<text x="{x}" y="218" text-anchor="{anchor}" font-family="DejaVu Sans" font-weight="bold" font-size="{px}" fill="{FG}">{}</text>"#,
            xml_escape(&name)
        ));
        if !s.sub.is_empty() {
            out.push_str(&format!(
                r#"<text x="{x}" y="256" text-anchor="{anchor}" font-family="DejaVu Sans" font-size="24" fill="{MUTED}">{}</text>"#,
                xml_escape(&truncate(&s.sub, 32))
            ));
        }
        // The projected score, or the win probability when there is no score
        // (a preseason-only projection has a margin but no total).
        let big = match s.score {
            Some(p) => p.to_string(),
            None => format!("{:.0}%", s.win_prob * 100.0),
        };
        out.push_str(&format!(
            r#"<text x="{x}" y="388" text-anchor="{anchor}" font-family="DejaVu Sans" font-weight="bold" font-size="112" fill="{}">{big}</text>"#,
            if fav { FG } else { MUTED }
        ));
        if s.score.is_some() {
            out.push_str(&format!(
                r#"<text x="{x}" y="432" text-anchor="{anchor}" font-family="DejaVu Sans" font-weight="bold" font-size="28" fill="{color}">{:.0}%<tspan font-weight="normal" font-size="20" fill="{MUTED}">&#160;to win</tspan></text>"#,
                s.win_prob * 100.0
            ));
        }
        if let Some((n, cam)) = &s.star {
            let (_, band) = cam_tier(*cam);
            out.push_str(&format!(
                r#"<text x="{x}" y="560" text-anchor="{anchor}" font-family="DejaVu Sans" font-size="20" fill="{LIGHT}">{} <tspan font-weight="bold" fill="{band}">{cam:+.1}</tspan><tspan fill="{MUTED}"> CAM</tspan></text>"#,
                xml_escape(&truncate(n, 22))
            ));
        }
    }

    // "at" / "vs" between the two, and the spread and total under it.
    out.push_str(&format!(
        r#"<text x="600" y="350" text-anchor="middle" font-family="DejaVu Sans" font-size="34" fill="{MUTED}">{}</text>"#,
        if c.neutral { "vs" } else { "at" }
    ));
    out.push_str(&format!(
        r#"<text x="600" y="478" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="26" fill="{LIGHT}">{}</text>"#,
        xml_escape(&c.line)
    ));

    // Win-probability bar.
    let (bx, bw) = (64.0, 1072.0);
    let split = bx + bw * c.left.win_prob.clamp(0.0, 1.0);
    out.push_str(&format!(
        r#"<rect x="{bx}" y="500" width="{bw}" height="16" rx="8" fill="{SIDE_RIGHT}"/><rect x="{bx}" y="500" width="{:.1}" height="16" rx="8" fill="{SIDE_LEFT}"/><rect x="{:.1}" y="496" width="4" height="24" fill="{BG}"/>"#,
        (split - bx).max(0.0),
        split - 2.0
    ));

    let note = c
        .note
        .as_deref()
        .map(|n| {
            format!(
                r#"<text x="64" y="602" font-family="DejaVu Sans" font-size="18" fill="{MUTED}">{}</text>"#,
                xml_escape(n)
            )
        })
        .unwrap_or_default();

    format!(
        r##"<svg width="{CARD_W}" height="{CARD_H}" viewBox="0 0 {CARD_W} {CARD_H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="{CARD_W}" height="{CARD_H}" fill="{BG}"/>
  <rect x="0" y="0" width="12" height="{CARD_H}" fill="{ACCENT}"/>
  <text x="64" y="78" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
  <text x="1136" y="78" text-anchor="end" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">{}</text>
  {out}
  {note}
  <text x="1136" y="602" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{MUTED}">camalytics.org</text>
</svg>"##,
        xml_escape(&c.label)
    )
}

fn rasterize(svg: &str) -> Option<Vec<u8>> {
    rasterize_sized(svg, CARD_W, CARD_H)
}

/// Rasterize an SVG of the given pixel size with the embedded fonts.
pub(crate) fn rasterize_sized(svg: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    let opt = usvg::Options {
        font_family: "DejaVu Sans".to_string(),
        fontdb: fontdb().clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

/// Truncate to at most `max` chars (not bytes), appending an ellipsis. Keeps
/// long names from overflowing the card.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// XML text escaping for values placed inside SVG `<text>` elements.
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(axes: Vec<Axis>) -> PlayerCard {
        PlayerCard {
            name: "Wade Taylor IV".into(),
            team_line: "Texas A&M · SEC".into(),
            bio: vec!["G".into(), "Sr".into(), "6'0\"".into()],
            archetype: Some("Rogue".into()),
            cam: Some(16.2),
            cam_o: Some(11.0),
            cam_d: Some(5.2),
            period: "2025-26 · 31 GP".into(),
            stats: vec![
                ("PPG", "18.4".into()),
                ("RPG", "3.9".into()),
                ("APG", "4.1".into()),
                ("TS%", "54.8".into()),
            ],
            axes,
        }
    }

    fn assert_png(png: Option<Vec<u8>>) {
        let png = png.expect("should rasterize");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            png.len() > 1000,
            "png unexpectedly tiny: {} bytes",
            png.len()
        );
    }

    #[test]
    fn renders_a_png() {
        // End-to-end: the SVG template rasterizes to a valid PNG with the
        // embedded font (no DB needed). Guards the resvg API + font loading.
        let axes = vec![
            Axis {
                label: "Scoring",
                value: Some(0.9),
            },
            Axis {
                label: "Efficiency",
                value: None,
            },
            Axis {
                label: "3-Point",
                value: Some(0.5),
            },
        ];
        assert_png(rasterize(&build_svg(&card(axes))));
        // No radar, no CAM, no stats: a player who has not played still gets a card.
        let mut bare = card(Vec::new());
        bare.cam = None;
        bare.stats.clear();
        bare.archetype = None;
        assert_png(rasterize(&build_svg(&bare)));
    }

    #[test]
    fn names_split_with_suffixes() {
        assert_eq!(
            split_name("Wade Taylor IV"),
            ("Wade".into(), "Taylor IV".into())
        );
        assert_eq!(
            split_name("Ace Baldwin Jr."),
            ("Ace".into(), "Baldwin Jr.".into())
        );
        assert_eq!(
            split_name("Cooper Flagg"),
            ("Cooper".into(), "Flagg".into())
        );
        assert_eq!(
            split_name("Johni Broome Jr"),
            ("Johni".into(), "Broome Jr".into())
        );
        assert_eq!(split_name("Bol"), (String::new(), "Bol".into()));
    }

    #[test]
    fn long_names_shrink_to_fit() {
        assert_eq!(fit_px("Flagg", 88.0, 560.0, 0.70), 88.0);
        let px = fit_px("Ndongo-Ndjock", 88.0, 560.0, 0.70);
        assert!(px < 88.0 && text_w("Ndongo-Ndjock", px, 0.70) <= 560.0);
    }

    #[test]
    fn team_card_renders() {
        let full = TeamOgCard {
            name: "Saint Mary's".into(),
            sub: "WCC · 4-1".into(),
            season: "2026-27".into(),
            rating: Some((18, 19.4, "Adj. efficiency margin")),
            stats: vec![TeamStat {
                label: "OFFENSE",
                value: "117.2".into(),
                rank: Some(31),
            }],
            form: vec![true, true, false, true, true],
            next: Some("Next: at Gonzaga · Nov 21".into()),
            roster_label: "Top players by CAM".into(),
            players: vec![("Paulius Murauskas".into(), 14.1, Some("Barbarian".into()))],
        };
        let svg = team_svg(&full);
        assert!(svg.contains("Saint Mary&apos;s"), "name is escaped");
        assert_png(rasterize(&svg));
        // A team with nothing yet still gets a card.
        let bare = TeamOgCard {
            rating: None,
            stats: vec![],
            form: vec![],
            next: None,
            players: vec![],
            ..full
        };
        assert_png(rasterize(&team_svg(&bare)));
    }

    #[test]
    fn coach_card_renders() {
        let full = CoachOgCard {
            name: "Randy Bennett".into(),
            sub: "Saint Mary's · 2015–2027".into(),
            coverage: "11 of 13 seasons scored".into(),
            grade: Some(CoachGrade {
                cae: 2.4,
                ci_low: 0.6,
                ci_high: 4.2,
            }),
            strength: vec![("TEAM ADJEM", "+14.2".into())],
            seasons: vec![(2025, Some(3.1)), (2026, None), (2027, Some(-1.2))],
        };
        assert_png(rasterize(&coach_svg(&full)));
        let bare = CoachOgCard {
            grade: None,
            strength: vec![],
            seasons: vec![(2027, None)],
            ..full
        };
        assert_png(rasterize(&coach_svg(&bare)));
    }

    #[test]
    fn cae_formatting_follows_the_site() {
        assert_eq!(fmt_cae(2.04), "+2.0");
        assert_eq!(fmt_cae(-0.04), "0.0");
        assert_eq!(fmt_cae(-1.25), "-1.2");
        assert_eq!(cae_color(0.0), "rgb(229, 231, 235)");
        assert_eq!(cae_color(9.0), "rgb(74, 222, 128)");
        assert_eq!(cae_color(-4.0), "rgb(248, 113, 113)");
    }

    fn side(name: &str, score: Option<i32>, p: f64) -> MatchupSide {
        MatchupSide {
            name: name.into(),
            rank: Some(12),
            sub: "Big Ten · 4-1".into(),
            score,
            win_prob: p,
            star: Some(("Braden Smith".into(), 18.4)),
        }
    }

    #[test]
    fn matchup_card_renders() {
        let c = MatchupOgCard {
            left: side("Purdue", Some(74), 0.42),
            right: side("Gonzaga", Some(77), 0.58),
            neutral: false,
            line: "Gonzaga by 3.4 · total 151.0".into(),
            label: "Game prediction · 2026-27".into(),
            note: None,
        };
        assert_png(rasterize(&matchup_svg(&c)));
        // Preseason-only: no scores, the probabilities stand in.
        let c = MatchupOgCard {
            left: side("Texas A&M", None, 0.6),
            right: side("Saint Mary's", None, 0.4),
            neutral: true,
            note: Some("Built on preseason projections".into()),
            ..c
        };
        let svg = matchup_svg(&c);
        assert!(svg.contains("60%") && svg.contains(">vs<"));
        assert_png(rasterize(&svg));
    }

    #[test]
    fn matchup_query_round_trips() {
        let q: MatchupQuery =
            serde_urlencoded::from_str("home=Texas%20A%26M&away=Duke&venue=neutral&season=2027")
                .unwrap();
        let p = q.to_predict().unwrap();
        assert_eq!(p.home, "Texas A&M");
        assert_eq!(p.venue, Some(Venue::Neutral));
        assert_eq!(
            q.query_string(),
            "home=Texas+A%26M&away=Duke&venue=neutral&season=2027"
        );
        // A missing or blank team is no matchup; a junk venue is dropped.
        let q: MatchupQuery = serde_urlencoded::from_str("home=Duke&away=%20").unwrap();
        assert!(q.to_predict().is_none());
        let q: MatchupQuery = serde_urlencoded::from_str("home=A&away=B&venue=moon").unwrap();
        assert_eq!(q.to_predict().unwrap().venue, None);
        // A parameter that does not parse is no matchup, never an error.
        assert!(
            MatchupQuery::parse(Some("home=A&away=B&season="))
                .to_predict()
                .is_none()
        );
        assert!(
            MatchupQuery::parse(Some("home=A&away=B&season=abc"))
                .to_predict()
                .is_none()
        );
        assert!(MatchupQuery::parse(None).to_predict().is_none());
        assert!(
            MatchupQuery::parse(Some("home=A&away=B&season=2027"))
                .to_predict()
                .is_some()
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
