//! Offseason attrition audit — the proactive half of issue #215.
//!
//! The roster projection can only drop a player it has been *told* about:
//! `players.class_year = 'Sr'`, a 247 portal row, an NBA `draft_entrants` row,
//! or a curated `player_departures` row. Everything else is assumed to be
//! coming back. That assumption is silent, and it is wrong a few times every
//! summer — Mario Saint-Supery signed with Valencia in July 2026 and Gonzaga's
//! 2027 projection kept a 92nd-percentile guard on the roster until someone
//! filed a bug.
//!
//! This audit makes the assumption *legible*. It prints two things:
//!
//!   1. **Unmatched capture rows** — `player_departures` entries that resolve to
//!      no base-season roster player. A typo in the hand-entered name or team is
//!      otherwise a silent no-op: the row sits in the table looking correct
//!      while the player stays on the projected roster. This section is the one
//!      that catches your own mistakes, so it prints first and is never elided.
//!
//!   2. **At-risk returners** — everyone the projection currently believes is
//!      coming back, ranked by base-season CamPom, so the names whose departure
//!      would move a projection the most are at the top. By default the list is
//!      narrowed to non-US players, the cohort with a standing outside option in
//!      the European and Australian pro leagues; `--all-nationalities` widens it
//!      to the domestic list too, which is where dismissals, medical
//!      retirements, and players who simply walk away show up.
//!
//! It is a worklist, not a detector: nothing here knows who actually left. Run
//! it in July, skim the top of the list against the news, and write what you
//! find into `data/departures/{year}_departures.json`.
//!
//! The returning cohort comes from `compose_all_projections` rather than a
//! hand-rolled SQL filter, so the audit and the served projection can never
//! disagree about who counts as returning.

use anyhow::Result;
use cstat_core::inference::Predictor;
use cstat_core::roster_projection::{
    DepartureReason, PlayerDeparture, compose_all_projections, fetch_draft_entrants,
    fetch_player_departures, normalize_player_name,
};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Knobs for one audit run.
pub struct AuditOptions {
    /// Base season N — the completed season whose roster we're projecting
    /// forward into N+1. Same convention as `player_departures.year`.
    pub base_season: i32,
    /// Floor on base-season `cam_gbpm_v3_psos` for the at-risk list. Departures
    /// below this barely move a projection, and listing them buries the ones
    /// that do.
    pub min_cam: f64,
    /// Restrict the at-risk list to non-US players. On by default; the
    /// international cohort is where the unreported exits concentrate.
    pub intl_only: bool,
    /// Cap on printed at-risk rows.
    pub limit: usize,
}

/// One row of roster metadata the projection's `PlayerRow` doesn't carry.
#[derive(sqlx::FromRow)]
struct PlayerMeta {
    id: Uuid,
    name: String,
    team_name: String,
    class_year: Option<String>,
    nationality: Option<String>,
}

/// Run the audit and print its report to stdout. Returns the number of
/// unmatched capture rows so a caller can decide whether that's fatal.
pub async fn run(pool: &PgPool, predictor: &Predictor, opts: &AuditOptions) -> Result<usize> {
    let base = opts.base_season;
    let entrants = fetch_draft_entrants(pool, base).await?;
    let captured = fetch_player_departures(pool, base).await?;

    // `false` = don't retro-exclude redshirt recruits. The audit is about the
    // returning cohort, which that gate doesn't touch.
    let projections =
        compose_all_projections(pool, base, &entrants, &captured, predictor, false).await?;

    println!(
        "Attrition audit — base season {base} → projecting {}",
        base + 1
    );
    println!(
        "  {} capture row(s) in player_departures, {} team projection(s)",
        captured.len(),
        projections.len(),
    );

    // --- 1. Capture rows that resolved to nobody. ------------------------
    // A matched row shows up as a LeftProgram departure on exactly one team;
    // anything in the capture without a corresponding label never resolved.
    let matched: HashSet<String> = projections
        .iter()
        .flat_map(|p| p.departures.iter())
        .filter_map(|d| match d {
            DepartureReason::LeftProgram { name, .. } => Some(normalize_player_name(name)),
            _ => None,
        })
        .collect();
    let unmatched: Vec<&PlayerDeparture> = captured
        .iter()
        .filter(|d| !matched.contains(&normalize_player_name(&d.name)))
        .collect();

    println!();
    if unmatched.is_empty() {
        println!("UNMATCHED CAPTURE ROWS: none — every player_departures row resolved.");
    } else {
        println!(
            "UNMATCHED CAPTURE ROWS ({}) — these are silently doing NOTHING:",
            unmatched.len()
        );
        for d in &unmatched {
            println!(
                "  {:<28} {:<24} reason={} — name/team resolves to no {base} roster player",
                d.name, d.current_team, d.reason,
            );
        }
        println!(
            "  Fix the name or team string in data/departures/{base}_departures.json to match \
             cstat's players.name / teams.short_name, then re-run `cstat-ingest departures`."
        );
    }

    // --- 2. At-risk returners, ranked by what their exit would cost. -----
    let meta: Vec<PlayerMeta> = sqlx::query_as::<_, PlayerMeta>(
        r#"
        SELECT p.id, p.name, t.name AS team_name, p.class_year, p.nationality
        FROM players p
        JOIN teams t ON t.id = p.team_id
        WHERE p.season = $1
        "#,
    )
    .bind(base)
    .fetch_all(pool)
    .await?;
    let meta_by_id: HashMap<Uuid, &PlayerMeta> = meta.iter().map(|m| (m.id, m)).collect();

    let mut at_risk: Vec<(f64, &PlayerMeta, f64)> = Vec::new();
    for p in &projections {
        for r in &p.returning {
            let Some(m) = meta_by_id.get(&r.player_id) else {
                continue;
            };
            let cam = r.cam_v3.unwrap_or(0.0);
            if cam < opts.min_cam {
                continue;
            }
            // Treat a missing nationality as domestic: the field is sparse, and
            // a NULL is not evidence of an outside option.
            let is_intl = m
                .nationality
                .as_deref()
                .is_some_and(|n| !n.eq_ignore_ascii_case("United States"));
            if opts.intl_only && !is_intl {
                continue;
            }
            at_risk.push((cam, m, r.mpg));
        }
    }
    at_risk.sort_by(|a, b| b.0.total_cmp(&a.0));

    let scope = if opts.intl_only {
        "non-US"
    } else {
        "all nationalities"
    };
    println!();
    println!(
        "AT-RISK RETURNERS ({scope}, cam >= {:.1}) — {} match, showing {}:",
        opts.min_cam,
        at_risk.len(),
        at_risk.len().min(opts.limit),
    );
    println!(
        "  {:<26} {:<30} {:<4} {:<16} {:>5} {:>7}",
        "PLAYER", "TEAM", "CLS", "NATIONALITY", "MPG", "CAM"
    );
    for (cam, m, mpg) in at_risk.iter().take(opts.limit) {
        println!(
            "  {:<26} {:<30} {:<4} {:<16} {:>5.1} {:>7.2}",
            truncate(&m.name, 26),
            truncate(&m.team_name, 30),
            m.class_year.as_deref().unwrap_or("?"),
            truncate(m.nationality.as_deref().unwrap_or("?"), 16),
            mpg,
            cam,
        );
    }
    println!();
    println!(
        "  Verify against the news, then add confirmed exits to \
         data/departures/{base}_departures.json and run `cstat-ingest departures`."
    );

    Ok(unmatched.len())
}

/// Clip a display string to `max` chars so the fixed-width table stays aligned
/// on long program names ("George Washington Revolutionaries").
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
