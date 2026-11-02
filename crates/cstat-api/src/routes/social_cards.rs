//! Social-post images for the drafts bot (`cstat-ingest social`,
//! docs/social_drafts.md): a player's skill radar, a game's matchup card, and
//! a team outlook card.
//!
//! 1200x675, Twitter's inline 16:9, in the same palette and with the same
//! embedded fonts as the Open Graph player card (`og_image`). They are public
//! because Slack, and later Twitter, fetch them by URL. Each draws only from
//! the database by id, never from query text, so nobody can mint an
//! official-looking card with numbers of their choosing. Fail-soft like the
//! OG card: anything unrenderable redirects to the static share image.

use std::f64::consts::PI;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use super::og_image::{ACCENT, BG, FG, LIGHT, MUTED, RULE, rasterize_sized, truncate, xml_escape};
use crate::AppState;
use cstat_core::projection::{self, Venue};
use cstat_core::queries;

const W: u32 = 1200;
const H: u32 = 675;

/// The site's five-band grade scale (`web/src/components/scale.ts`), worst first.
const BAND: [&str; 5] = ["#f87171", "#fb923c", "#facc15", "#60a5fa", "#4ade80"];

#[derive(Deserialize)]
pub struct CardParams {
    season: Option<i32>,
}

fn png(bytes: Option<Vec<u8>>) -> Response {
    match bytes {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                // Numbers move on the nightly; the bot adds a date cache-buster.
                (header::CACHE_CONTROL, "public, max-age=21600"),
            ],
            png,
        )
            .into_response(),
        None => Redirect::temporary("/og-image.png").into_response(),
    }
}

fn parse_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id.strip_suffix(".png").unwrap_or(id)).ok()
}

// ---------------------------------------------------------------------------
// Player radar
// ---------------------------------------------------------------------------

/// `GET /og/radar/players/{id}.png?season=`
pub async fn radar_card(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<CardParams>,
) -> Response {
    png(render_radar(&state, &id, p.season).await)
}

/// One spoke: label and a 0-1 percentile.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Axis {
    pub label: &'static str,
    pub value: Option<f64>,
}

/// The eight axes of the player page's radar, ported from `resolveAxes` in
/// `web/src/components/radarAxes.ts` so the card and the page agree. Keep the
/// two in step.
pub(crate) fn radar_axes(
    pc: Option<&queries::PercentilesRow>,
    games_played: Option<i32>,
    tpa: Option<i32>,
    dgbpm_pct: Option<f64>,
) -> Vec<Axis> {
    let clamp = |v: f64| v.clamp(0.0, 1.0);
    // 3-Point is volume-gated: a rare make at high accuracy should not spike
    // the spoke, so low-volume shooters shrink toward the 30th percentile,
    // with full credit at two attempts a game.
    let tpa_pg = match (tpa, games_played) {
        (Some(a), Some(g)) if g > 0 => Some(a as f64 / g as f64),
        _ => None,
    };
    let vol = tpa_pg.map(|t| clamp(t / 2.0)).unwrap_or(0.0);
    let three = pc
        .and_then(|p| p.tp_pct_pct)
        .map(|raw| raw * vol + 0.3 * (1.0 - vol));
    let reb = pc.and_then(|p| match (p.orb_pct_pct, p.drb_pct_pct) {
        (Some(o), Some(d)) => Some((o + d) / 2.0),
        (o, d) => o.or(d),
    });
    vec![
        Axis {
            label: "Scoring",
            value: pc.and_then(|p| p.ppg_pct),
        },
        Axis {
            label: "Efficiency",
            value: pc.and_then(|p| p.true_shooting_pct_pct),
        },
        Axis {
            label: "3-Point",
            value: three,
        },
        Axis {
            label: "Playmaking",
            value: pc.and_then(|p| p.ast_pct_pct.or(p.apg_pct)),
        },
        Axis {
            label: "Free Throws",
            value: pc.and_then(|p| p.ft_rate_pct),
        },
        Axis {
            label: "Rebounding",
            value: reb,
        },
        Axis {
            label: "Defense",
            value: dgbpm_pct,
        },
        Axis {
            label: "Blocks",
            value: pc.and_then(|p| p.blk_pct_pct),
        },
    ]
    .into_iter()
    .map(|a| Axis {
        value: a.value.map(clamp),
        ..a
    })
    .collect()
}

async fn render_radar(state: &AppState, id: &str, season: Option<i32>) -> Option<Vec<u8>> {
    let pool = &state.db.pool;
    let uuid = parse_id(id)?;
    let season = season.unwrap_or_else(crate::default_season);

    // Early in a season nobody has enough games for percentiles yet, so a
    // card for a returning player falls back to last season, labeled as such.
    for s in [season, season - 1] {
        let Some(pid) = queries::resolve_player_id_for_season(pool, uuid, s)
            .await
            .ok()
            .flatten()
            .or((s == season).then_some(uuid))
        else {
            continue;
        };
        let pc = queries::get_player_percentiles(pool, pid, s)
            .await
            .ok()
            .flatten();
        let Some(pc) = pc else { continue };
        let prof = queries::get_player_by_id(pool, pid, s)
            .await
            .ok()
            .flatten()?;
        let ss = queries::get_player_season_stats(pool, pid, s)
            .await
            .ok()
            .flatten();
        let tv = queries::get_torvik_stats(pool, pid, s).await.ok().flatten();
        let archetype = queries::get_player_archetype(pool, pid, s)
            .await
            .ok()
            .flatten()
            .map(|a| a.primary_class)
            .filter(|c| !c.is_empty());
        let axes = radar_axes(
            Some(&pc),
            ss.as_ref().map(|s| s.games_played),
            tv.as_ref().and_then(|t| t.tpa),
            tv.as_ref().and_then(|t| t.dgbpm_pct),
        );
        if axes.iter().all(|a| a.value.is_none()) {
            continue;
        }
        let label = if s == season {
            season_label(s)
        } else {
            format!("{} season", season_label(s))
        };
        let svg = radar_svg(&RadarCard {
            name: &prof.name,
            subtitle: &[prof.team_name.as_deref().unwrap_or(""), &label]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            cam: tv.as_ref().and_then(|t| t.campom),
            cam_o: tv.as_ref().and_then(|t| t.campom_o),
            cam_d: tv.as_ref().and_then(|t| t.campom_d),
            archetype: archetype.as_deref(),
            axes: &axes,
        });
        return rasterize_sized(&svg, W, H);
    }
    None
}

/// "2025-26" for season 2026.
fn season_label(season: i32) -> String {
    format!("{}-{:02}", season - 1, season % 100)
}

pub(crate) struct RadarCard<'a> {
    pub name: &'a str,
    pub subtitle: &'a str,
    pub cam: Option<f64>,
    pub cam_o: Option<f64>,
    pub cam_d: Option<f64>,
    pub archetype: Option<&'a str>,
    pub axes: &'a [Axis],
}

pub(crate) fn radar_svg(c: &RadarCard<'_>) -> String {
    let (cx, cy, r) = (870.0, 352.0, 190.0);
    let n = c.axes.len() as f64;
    let pt = |i: usize, rr: f64| {
        let a = -PI / 2.0 + i as f64 * 2.0 * PI / n;
        (cx + rr * a.cos(), cy + rr * a.sin())
    };
    let mut body = String::new();

    // Rings and spokes.
    for f in [0.25, 0.5, 0.75, 1.0] {
        let pts: Vec<String> = (0..c.axes.len())
            .map(|i| {
                let (x, y) = pt(i, r * f);
                format!("{x:.1},{y:.1}")
            })
            .collect();
        body.push_str(&format!(
            r#"<polygon points="{}" fill="none" stroke="{RULE}" stroke-width="2"/>"#,
            pts.join(" ")
        ));
    }
    for (i, a) in c.axes.iter().enumerate() {
        let (x, y) = pt(i, r);
        body.push_str(&format!(
            r#"<line x1="{cx}" y1="{cy}" x2="{x:.1}" y2="{y:.1}" stroke="{RULE}" stroke-width="2"/>"#
        ));
        let (lx, ly) = pt(i, r + 34.0);
        let anchor = if (lx - cx).abs() < 8.0 {
            "middle"
        } else if lx > cx {
            "start"
        } else {
            "end"
        };
        let pct = a
            .value
            .map(|v| format!("{:.0}", v * 100.0))
            .unwrap_or_else(|| "–".into());
        body.push_str(&format!(
            r#"<text x="{lx:.1}" y="{:.1}" text-anchor="{anchor}" font-family="DejaVu Sans" font-size="22" fill="{LIGHT}">{}</text><text x="{lx:.1}" y="{:.1}" text-anchor="{anchor}" font-family="DejaVu Sans" font-weight="bold" font-size="20" fill="{ACCENT}">{pct}</text>"#,
            ly - 4.0,
            a.label,
            ly + 20.0
        ));
    }

    // The player's shape: missing axes sit at the center.
    let color = c.archetype.map(class_color).unwrap_or(ACCENT);
    let shape: Vec<String> = c
        .axes
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let (x, y) = pt(i, r * a.value.unwrap_or(0.0));
            format!("{x:.1},{y:.1}")
        })
        .collect();
    body.push_str(&format!(
        r#"<polygon points="{}" fill="{color}" fill-opacity="0.35" stroke="{color}" stroke-width="4" stroke-linejoin="round"/>"#,
        shape.join(" ")
    ));

    // Left column: who, CAM, archetype.
    let mut left = format!(
        r#"<text x="72" y="190" font-family="DejaVu Sans" font-weight="bold" font-size="58" fill="{FG}">{}</text>
<text x="72" y="238" font-family="DejaVu Sans" font-size="28" fill="{MUTED}">{}</text>"#,
        xml_escape(&truncate(c.name, 20)),
        xml_escape(&truncate(c.subtitle, 34)),
    );
    if let Some(cam) = c.cam {
        let (tier, band) = cam_tier(cam);
        left.push_str(&format!(
            r#"<text x="72" y="370" font-family="DejaVu Sans" font-weight="bold" font-size="104" fill="{band}">{cam:+.1}</text>
<text x="72" y="410" font-family="DejaVu Sans" font-size="26" fill="{MUTED}">CAM · {tier}</text>"#
        ));
        if let (Some(o), Some(d)) = (c.cam_o, c.cam_d) {
            left.push_str(&format!(
                r#"<text x="72" y="450" font-family="DejaVu Sans" font-size="26" fill="{LIGHT}">CAMO {o:+.1}   CAMD {d:+.1}</text>"#
            ));
        }
    }
    if let Some(class) = c.archetype {
        let w = 34 + class.chars().count() as i32 * 17;
        let fg = if class == "Rogue" || class == "Paladin" {
            "#111827"
        } else {
            FG
        };
        left.push_str(&format!(
            r#"<rect x="72" y="492" width="{w}" height="48" rx="24" fill="{}"/>
<text x="{}" y="525" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="24" fill="{fg}">{}</text>"#,
            class_color(class),
            72 + w / 2,
            xml_escape(class)
        ));
    }

    frame(
        &format!("{left}{body}"),
        "Percentiles among all D-I players",
    )
}

/// CAM tier and its band color, as `web/src/components/cam.ts` grades it.
pub(crate) fn cam_tier(cam: f64) -> (&'static str, &'static str) {
    match cam {
        c if c >= 20.0 => ("Elite", BAND[4]),
        c if c >= 15.0 => ("All-Conference", BAND[4]),
        c if c >= 10.0 => ("Quality starter", BAND[3]),
        c if c >= 5.0 => ("Rotation", BAND[2]),
        c if c >= 0.0 => ("Replacement", BAND[1]),
        _ => ("Below replacement", BAND[0]),
    }
}

/// Archetype colors, as `web/src/components/archetypeColors.ts`.
fn class_color(class: &str) -> &'static str {
    match class {
        "Wizard" => "#7c3aed",
        "Sorcerer" => "#dc2626",
        "Warlock" => "#c026d3",
        "Bard" => "#ec4899",
        "Ranger" => "#16a34a",
        "Barbarian" => "#ea580c",
        "Paladin" => "#eab308",
        "Monk" => "#06b6d4",
        "Cleric" => "#854d0e",
        "Druid" => "#059669",
        "Rogue" => "#e5e7eb",
        "Fighter" => "#737373",
        _ => "#64748b",
    }
}

// ---------------------------------------------------------------------------
// Matchup
// ---------------------------------------------------------------------------

/// `GET /og/matchup/{game_id}.png`
pub async fn matchup_card(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let render = async {
        let mut g = queries::get_matchup_card(&state.db.pool, parse_id(&id)?)
            .await
            .ok()
            .flatten()?;
        fill_preseason_projection(&mut g);
        rasterize_sized(&matchup_svg(&g), W, H)
    };
    png(render.await)
}

/// An upcoming game with no stored projection (a team that has not played
/// yet) gets the preseason-only projection, the same arithmetic the Schedule
/// tab and `/api/predict` serve, with no score: a strength gap carries no tempo.
/// A finished game never does, since that would be a number made up after the
/// fact rather than one shown before tipoff.
fn fill_preseason_projection(g: &mut queries::MatchupCard) {
    let is_final = g.home_score.is_some() && g.away_score.is_some();
    if is_final || g.home_win_prob.is_some() {
        return;
    }
    let (Some(h), Some(a)) = (g.home_preseason_adj_em, g.away_preseason_adj_em) else {
        return;
    };
    let venue = if g.is_neutral_site {
        Venue::Neutral
    } else {
        Venue::Home
    };
    let margin = projection::preseason_only_margin(h - a, venue);
    g.home_win_prob = Some(projection::preseason_only_win_prob(margin));
}

pub(crate) fn matchup_svg(g: &queries::MatchupCard) -> String {
    let record = |w: Option<i32>, l: Option<i32>| match (w, l) {
        (Some(w), Some(l)) if w + l > 0 => format!("{w}-{l}"),
        _ => String::new(),
    };
    let rank = |r: Option<i32>| {
        r.map(|r| format!("No. {r} in our ratings"))
            .unwrap_or_default()
    };
    let is_final = g.home_score.is_some() && g.away_score.is_some();
    let side = |x: i32, name: &str, rec: String, rk: String, lost: bool| {
        let color = if lost { MUTED } else { FG };
        format!(
            r#"<text x="{x}" y="250" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="48" fill="{color}">{}</text>
<text x="{x}" y="296" text-anchor="middle" font-family="DejaVu Sans" font-size="26" fill="{MUTED}">{}</text>
<text x="{x}" y="332" text-anchor="middle" font-family="DejaVu Sans" font-size="24" fill="{ACCENT}">{}</text>"#,
            xml_escape(&truncate(name, 14)),
            xml_escape(&rec),
            xml_escape(&rk)
        )
    };
    let home_won = g.home_score > g.away_score;
    let mut body = side(
        250,
        &g.away_name,
        record(g.away_wins, g.away_losses),
        rank(g.away_rank),
        is_final && home_won,
    );
    body.push_str(&side(
        950,
        &g.home_name,
        record(g.home_wins, g.home_losses),
        rank(g.home_rank),
        is_final && !home_won,
    ));
    let date = g.game_date.format("%b %-d, %Y").to_string();
    let center = match (g.away_score, g.home_score) {
        (Some(a), Some(h)) => {
            let ot = g
                .overtime
                .as_deref()
                .filter(|o| !o.is_empty())
                .unwrap_or("");
            format!(
                r#"<text x="600" y="262" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="64" fill="{FG}">{a}–{h}</text>
<text x="600" y="306" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="24" letter-spacing="4" fill="{ACCENT}">FINAL {}</text>"#,
                xml_escape(ot)
            )
        }
        _ => format!(
            r#"<text x="600" y="262" text-anchor="middle" font-family="DejaVu Sans" font-weight="bold" font-size="44" fill="{MUTED}">{}</text>"#,
            if g.is_neutral_site { "vs" } else { "at" }
        ),
    };
    body.push_str(&center);

    // Win probability bar, away on the left.
    if let Some(hp) = g.home_win_prob {
        let ap = 1.0 - hp;
        let (x0, x1, y) = (120.0, 1080.0, 430.0);
        let split = x0 + (x1 - x0) * ap;
        let label = if is_final {
            "Our pregame win probability"
        } else {
            "Our win probability"
        };
        body.push_str(&format!(
            r#"<text x="600" y="400" text-anchor="middle" font-family="DejaVu Sans" font-size="24" fill="{MUTED}">{label}</text>
<rect x="{x0}" y="{y}" width="{:.1}" height="34" rx="6" fill="{ACCENT}"/>
<rect x="{split:.1}" y="{y}" width="{:.1}" height="34" rx="6" fill="{RULE}"/>
<text x="{x0}" y="{:.0}" font-family="DejaVu Sans" font-weight="bold" font-size="40" fill="{FG}">{:.0}%</text>
<text x="{x1}" y="{:.0}" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="40" fill="{FG}">{:.0}%</text>"#,
            split - x0,
            x1 - split,
            y + 84.0,
            ap * 100.0,
            y + 84.0,
            hp * 100.0,
        ));
        if let (Some(ph), Some(pa)) = (g.projected_home_score, g.projected_away_score) {
            body.push_str(&format!(
                r#"<text x="600" y="{:.0}" text-anchor="middle" font-family="DejaVu Sans" font-size="26" fill="{LIGHT}">Projected: {} {pa}, {} {ph}</text>"#,
                y + 84.0,
                xml_escape(&truncate(&g.away_name, 14)),
                xml_escape(&truncate(&g.home_name, 14)),
            ));
        }
    }
    frame(&body, &date)
}

// ---------------------------------------------------------------------------
// Team
// ---------------------------------------------------------------------------

/// `GET /og/team/{id}.png?season=`
pub async fn team_card(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(p): Query<CardParams>,
) -> Response {
    png(render_team(&state, &id, p.season).await)
}

async fn render_team(state: &AppState, id: &str, season: Option<i32>) -> Option<Vec<u8>> {
    let pool = &state.db.pool;
    let uuid = parse_id(id)?;
    let season = season.unwrap_or_else(crate::default_season);
    let tid = queries::resolve_team_id_for_season(pool, uuid, season)
        .await
        .ok()
        .flatten()
        .unwrap_or(uuid);
    let t = queries::get_team_card(pool, tid, season)
        .await
        .ok()
        .flatten()?;

    // Top players by CAM: this season once it has them, else last season's
    // roster for the same program, labeled.
    let mut players = top_players(pool, tid, season).await;
    let mut roster_label = "Top players by CAM".to_string();
    if players.is_empty()
        && let Some(prev) = queries::resolve_team_id_for_season(pool, uuid, season - 1)
            .await
            .ok()
            .flatten()
    {
        players = top_players(pool, prev, season - 1).await;
        roster_label = format!("Top players by CAM, {}", season_label(season - 1));
    }
    rasterize_sized(&team_svg(&t, season, &roster_label, &players), W, H)
}

async fn top_players(
    pool: &sqlx::PgPool,
    team_id: Uuid,
    season: i32,
) -> Vec<(String, f64, Option<String>)> {
    let mut roster: Vec<(String, f64, Option<String>)> =
        queries::get_team_roster(pool, team_id, season)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| Some((p.name, p.campom?, p.primary_class)))
            .collect();
    roster.sort_by(|a, b| b.1.total_cmp(&a.1));
    roster.truncate(5);
    roster
}

pub(crate) fn team_svg(
    t: &queries::TeamCard,
    season: i32,
    roster_label: &str,
    players: &[(String, f64, Option<String>)],
) -> String {
    let mut sub = vec![season_label(season)];
    if let Some(c) = &t.conference {
        sub.push(c.clone());
    }
    if let (Some(w), Some(l)) = (t.wins, t.losses)
        && w + l > 0
    {
        sub.push(format!("{w}-{l}"));
    }
    let mut body = format!(
        r#"<text x="72" y="180" font-family="DejaVu Sans" font-weight="bold" font-size="64" fill="{FG}">{}</text>
<text x="72" y="228" font-family="DejaVu Sans" font-size="28" fill="{MUTED}">{}</text>"#,
        xml_escape(&truncate(&t.name, 22)),
        xml_escape(&sub.join(" · "))
    );

    let mut stat = |y: i32, big: String, label: &str| {
        body.push_str(&format!(
            r#"<text x="72" y="{y}" font-family="DejaVu Sans" font-weight="bold" font-size="76" fill="{ACCENT}">{}</text>
<text x="72" y="{}" font-family="DejaVu Sans" font-size="24" fill="{MUTED}">{}</text>"#,
            xml_escape(&big),
            y + 36,
            xml_escape(label)
        ));
    };
    match (t.adj_em, t.adj_em_rank) {
        (Some(em), Some(r)) => stat(
            350,
            format!("No. {r}"),
            &format!("in our ratings · {em:+.1} adj. efficiency margin"),
        ),
        _ => {
            if let (Some(em), Some(r)) = (t.preseason_adj_em, t.preseason_rank) {
                stat(
                    350,
                    format!("No. {r}"),
                    &format!("in our preseason projection · {em:+.1}"),
                );
            }
        }
    }
    if t.adj_em.is_some()
        && let (Some(em), Some(r)) = (t.preseason_adj_em, t.preseason_rank)
    {
        body.push_str(&format!(
            r#"<text x="72" y="462" font-family="DejaVu Sans" font-size="26" fill="{LIGHT}">Preseason projection: No. {r} ({em:+.1})</text>"#
        ));
    }

    // Roster column.
    body.push_str(&format!(
        r#"<text x="680" y="150" font-family="DejaVu Sans" font-weight="bold" font-size="22" letter-spacing="3" fill="{ACCENT}">{}</text>"#,
        xml_escape(&roster_label.to_uppercase())
    ));
    for (i, (name, cam, class)) in players.iter().enumerate() {
        let y = 215 + i as i32 * 78;
        let (_, band) = cam_tier(*cam);
        body.push_str(&format!(
            r#"<line x1="680" y1="{}" x2="1128" y2="{}" stroke="{RULE}" stroke-width="2"/>
<text x="680" y="{y}" font-family="DejaVu Sans" font-weight="bold" font-size="30" fill="{FG}">{}</text>
<text x="680" y="{}" font-family="DejaVu Sans" font-size="22" fill="{}">{}</text>
<text x="1128" y="{}" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="40" fill="{band}">{cam:+.1}</text>"#,
            y - 42,
            y - 42,
            xml_escape(&truncate(name, 20)),
            y + 28,
            class.as_deref().map(class_color).unwrap_or(MUTED),
            xml_escape(class.as_deref().unwrap_or("")),
            y + 14,
        ));
    }
    frame(&body, "")
}

// ---------------------------------------------------------------------------

/// The shared chrome: background, accent stripe, wordmark, footer.
fn frame(body: &str, footnote: &str) -> String {
    format!(
        r##"<svg width="{W}" height="{H}" viewBox="0 0 {W} {H}" xmlns="http://www.w3.org/2000/svg">
<rect width="{W}" height="{H}" fill="{BG}"/>
<rect x="0" y="0" width="12" height="{H}" fill="{ACCENT}"/>
<text x="72" y="84" font-family="DejaVu Sans" font-weight="bold" font-size="26" letter-spacing="6" fill="{ACCENT}">CAMALYTICS</text>
{body}
<text x="72" y="635" font-family="DejaVu Sans" font-size="22" fill="{MUTED}">{}</text>
<text x="1128" y="635" text-anchor="end" font-family="DejaVu Sans" font-weight="bold" font-size="22" fill="{MUTED}">camalytics.org</text>
</svg>"##,
        xml_escape(footnote)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn assert_png(svg: &str) {
        let png = rasterize_sized(svg, W, H).expect("should rasterize");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            png.len() > 5000,
            "png unexpectedly tiny: {} bytes",
            png.len()
        );
    }

    fn pc() -> queries::PercentilesRow {
        queries::PercentilesRow {
            ppg_pct: Some(0.9),
            true_shooting_pct_pct: Some(0.8),
            tp_pct_pct: Some(0.9),
            apg_pct: Some(0.7),
            ft_rate_pct: Some(0.5),
            orb_pct_pct: Some(0.2),
            drb_pct_pct: Some(0.4),
            blk_pct_pct: Some(1.4),
            ..Default::default()
        }
    }

    #[test]
    fn radar_axes_match_the_web_formulas() {
        // 30 attempts in 30 games is half volume: 0.9 * 0.5 + 0.3 * 0.5.
        let axes = radar_axes(Some(&pc()), Some(30), Some(30), Some(0.6));
        let v = |label: &str| axes.iter().find(|a| a.label == label).unwrap().value;
        assert_eq!(axes.len(), 8);
        assert!((v("3-Point").unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(v("Playmaking"), Some(0.7), "AST% falls back to APG");
        assert!((v("Rebounding").unwrap() - 0.3).abs() < 1e-9);
        assert_eq!(v("Blocks"), Some(1.0), "clamped");
        assert_eq!(v("Defense"), Some(0.6));

        // No attempts recorded: no volume credit, the floor alone.
        let axes = radar_axes(Some(&pc()), None, None, None);
        assert!((axes[2].value.unwrap() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn cards_render() {
        let axes = radar_axes(Some(&pc()), Some(30), Some(60), Some(0.6));
        assert_png(&radar_svg(&RadarCard {
            name: "Braden Smith",
            subtitle: "Purdue · 2025-26",
            cam: Some(18.4),
            cam_o: Some(13.1),
            cam_d: Some(5.3),
            archetype: Some("Wizard"),
            axes: &axes,
        }));

        let g = queries::MatchupCard {
            game_date: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            season: 2027,
            home_name: "Gonzaga".into(),
            away_name: "Purdue".into(),
            home_score: Some(70),
            away_score: Some(74),
            is_neutral_site: true,
            overtime: Some("OT".into()),
            home_wins: Some(0),
            home_losses: Some(1),
            away_wins: Some(1),
            away_losses: Some(0),
            home_rank: Some(5),
            away_rank: Some(12),
            home_win_prob: Some(0.579),
            projected_home_score: Some(77),
            projected_away_score: Some(74),
            home_preseason_adj_em: None,
            away_preseason_adj_em: None,
        };
        assert_png(&matchup_svg(&g));

        let t = queries::TeamCard {
            name: "Texas A&M".into(),
            conference: Some("SEC".into()),
            wins: None,
            losses: None,
            adj_em: None,
            adj_em_rank: None,
            preseason_adj_em: Some(21.3),
            preseason_rank: Some(14),
        };
        let players = vec![(
            "Wade Taylor IV".to_string(),
            16.2,
            Some("Rogue".to_string()),
        )];
        assert_png(&team_svg(&t, 2027, "Top players by CAM, 2025-26", &players));
    }

    #[test]
    fn preseason_fallback_only_before_tipoff() {
        let mut g = queries::MatchupCard {
            game_date: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            season: 2027,
            home_name: "Gonzaga".into(),
            away_name: "Purdue".into(),
            home_score: None,
            away_score: None,
            is_neutral_site: true,
            overtime: None,
            home_wins: None,
            home_losses: None,
            away_wins: None,
            away_losses: None,
            home_rank: None,
            away_rank: None,
            home_win_prob: None,
            projected_home_score: None,
            projected_away_score: None,
            home_preseason_adj_em: Some(35.1),
            away_preseason_adj_em: Some(31.3),
        };
        fill_preseason_projection(&mut g);
        let p = g.home_win_prob.expect("filled");
        assert!(p > 0.5 && p < 0.7, "{p}");

        g.home_win_prob = None;
        g.home_score = Some(70);
        g.away_score = Some(74);
        fill_preseason_projection(&mut g);
        assert!(g.home_win_prob.is_none(), "never invented after the final");
    }

    #[test]
    fn tiers_follow_the_site() {
        assert_eq!(cam_tier(22.0).0, "Elite");
        assert_eq!(cam_tier(10.0).0, "Quality starter");
        assert_eq!(cam_tier(-1.0).0, "Below replacement");
    }
}
