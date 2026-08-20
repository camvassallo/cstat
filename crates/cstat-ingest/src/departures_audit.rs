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
//!   2. **Unplaced eligibility returns** — the same check for `player_returns`
//!      (issue #220, the NCAA 5-in-5 rule), which has the identical failure
//!      mode running the other way: a typo'd `granted` row leaves the player
//!      deleted from his team by the `class_year == 'Sr'` inference, which is
//!      the exact bug the capture was built to fix. Audited here rather than in
//!      a command of its own because the two captures are curated in the same
//!      sitting off the same news, and a second tool nobody remembers to run is
//!      not a safety net.
//!
//!   3. **At-risk returners** — everyone the projection currently believes is
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
use cstat_core::roster_features::{QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use cstat_core::roster_projection::{
    DepartureReason, PlayerDeparture, PlayerReturn, ReturnStatus, compose_all_projections,
    fetch_draft_entrants, fetch_player_departures, fetch_player_returns, normalize_player_name,
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
    /// Clears the projection's roster gate (`QUAL_MIN_GAMES_PLAYED` /
    /// `QUAL_MIN_MPG`) on *any* of the player's season rows —
    /// `player_season_stats` is unique on `(player_id, team_id, season)`, so a
    /// player with rows on two teams in one season has two, and `bool_or`
    /// collapses them rather than fanning the join out. Sub-gate players never
    /// enter `compose_all_projections` at all, so a capture row naming one is
    /// *correctly* a no-op rather than a mistake — see the classification below.
    qualified: bool,
}

/// Run the audit and print its report to stdout. Returns the number of curated
/// rows — across `player_departures` AND `player_returns` — that failed to do
/// what they claim, i.e. real mistakes a caller should treat as fatal. Rows
/// naming a player below the projection's GP/MPG gate are reported but not
/// counted; those are correctly no-ops, because a sub-gate player never enters
/// `compose_all_projections` in the first place.
pub async fn run(pool: &PgPool, predictor: &Predictor, opts: &AuditOptions) -> Result<usize> {
    let base = opts.base_season;
    let entrants = fetch_draft_entrants(pool, base).await?;
    let captured = fetch_player_departures(pool, base).await?;
    let returns = fetch_player_returns(pool, base).await?;

    // `false` = don't retro-exclude redshirt recruits. The audit is about the
    // returning cohort, which that gate doesn't touch.
    let projections =
        compose_all_projections(pool, base, &entrants, &captured, predictor, false).await?;

    println!(
        "Attrition audit — base season {base} → projecting {}",
        base + 1
    );
    println!(
        "  {} capture row(s) in player_departures, {} in player_returns, \
         {} team projection(s)",
        captured.len(),
        returns.len(),
        projections.len(),
    );

    let meta: Vec<PlayerMeta> = sqlx::query_as::<_, PlayerMeta>(
        r#"
        SELECT p.id, p.name, t.name AS team_name, p.class_year, p.nationality,
               COALESCE(bool_or(pss.games_played >= $2 AND pss.minutes_per_game >= $3), false)
                   AS qualified
        FROM players p
        JOIN teams t ON t.id = p.team_id
        LEFT JOIN player_season_stats pss
               ON pss.player_id = p.id AND pss.season = p.season
        WHERE p.season = $1
        GROUP BY p.id, p.name, t.name, p.class_year, p.nationality
        "#,
    )
    .bind(base)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(pool)
    .await?;
    let meta_by_id: HashMap<Uuid, &PlayerMeta> = meta.iter().map(|m| (m.id, m)).collect();

    // --- 1. Capture rows that resolved to nobody. ------------------------
    // A matched row shows up as a LeftProgram departure on exactly one team.
    // Counted per normalized name rather than set-membership: cstat genuinely
    // carries same-name players in one season (issue #138 — two Jake Davises in
    // 2026), so two capture rows sharing a name must consume two resolved
    // departures or one of them is silently doing nothing.
    let mut matched: HashMap<String, usize> = HashMap::new();
    for d in projections.iter().flat_map(|p| p.departures.iter()) {
        if let DepartureReason::LeftProgram { name, .. } = d {
            *matched.entry(normalize_player_name(name)).or_default() += 1;
        }
    }
    // Split the failures three ways, because "didn't resolve" means very
    // different things depending on whether the *name* exists at all:
    //
    //   - name unknown to season N  → almost certainly a typo. Hard failure:
    //     this is the case the whole section exists to catch, and keying the
    //     benign test on the name would let every misspelling slip through it.
    //   - name known AND qualified  → the name is right but it still didn't
    //     resolve, so the team string is wrong (or a same-name sibling ate the
    //     match). Hard failure — the projection is still carrying him.
    //   - name known but sub-gate   → correctly a no-op. Sub-gate players never
    //     enter `compose_all_projections`, so there was nothing to remove;
    //     recording a deep-bench player's exit must not break the command.
    let mut known_names: HashSet<String> = HashSet::new();
    let mut qualified_names: HashSet<String> = HashSet::new();
    for m in &meta {
        let key = normalize_player_name(&m.name);
        if m.qualified {
            qualified_names.insert(key.clone());
        }
        known_names.insert(key);
    }
    let mut unmatched_real: Vec<(&PlayerDeparture, &str)> = Vec::new();
    let mut unmatched_benign: Vec<&PlayerDeparture> = Vec::new();
    for d in &captured {
        let key = normalize_player_name(&d.name);
        match matched.get_mut(&key) {
            Some(n) if *n > 0 => *n -= 1,
            _ if !known_names.contains(&key) => {
                unmatched_real.push((d, "no player by that name in the season — check spelling"))
            }
            _ if qualified_names.contains(&key) => unmatched_real.push((
                d,
                "name exists but not on that team — check the team string",
            )),
            _ => unmatched_benign.push(d),
        }
    }

    println!();
    if unmatched_real.is_empty() {
        println!("UNMATCHED CAPTURE ROWS: none — every player_departures row resolved.");
    } else {
        println!(
            "UNMATCHED CAPTURE ROWS ({}) — these are silently doing NOTHING:",
            unmatched_real.len()
        );
        for (d, why) in &unmatched_real {
            println!("  {:<28} {:<24} {why}", d.name, d.current_team);
        }
        println!(
            "  Fix data/departures/{base}_departures.json to match cstat's players.name / \
             teams.short_name, then re-run `cstat-ingest departures`."
        );
    }
    if !unmatched_benign.is_empty() {
        println!();
        println!(
            "  Note: {} row(s) name a player below the projection's \
             {QUAL_MIN_GAMES_PLAYED} GP / {QUAL_MIN_MPG:.0} MPG gate. Harmless — they were \
             never on the projected roster, so there was nothing to remove:",
            unmatched_benign.len()
        );
        for d in &unmatched_benign {
            println!("    {:<28} {}", d.name, d.current_team);
        }
    }

    // --- 2. Eligibility returns that didn't place their player. ----------
    // Same failure mode as section 1, mirrored. A `player_returns` row claims
    // "this player the `Sr` inference deletes is actually coming back"; if the
    // (name, team) match misses, the claim is silently void and the player
    // stays deleted — indistinguishable, from the outside, from never having
    // curated him. `status` makes the assertion sharper than the departures
    // case, so we can check the bucket too and not just the absence of a
    // departure: `granted` must land in `returning`, `contested` in
    // `uncertain`.
    let mut departed_names: HashSet<String> = HashSet::new();
    for d in projections.iter().flat_map(|p| p.departures.iter()) {
        departed_names.insert(normalize_player_name(departure_name(d)));
    }
    let mut uncertain_names: HashSet<String> = HashSet::new();
    let mut returning_names: HashSet<String> = HashSet::new();
    for p in &projections {
        for (_, u) in &p.uncertain {
            uncertain_names.insert(normalize_player_name(&u.name));
        }
        for r in &p.returning {
            if let Some(m) = meta_by_id.get(&r.player_id) {
                returning_names.insert(normalize_player_name(&m.name));
            }
        }
    }

    let mut unplaced_real: Vec<(&PlayerReturn, &str)> = Vec::new();
    let mut unplaced_benign: Vec<&PlayerReturn> = Vec::new();
    for r in &returns {
        let key = normalize_player_name(&r.name);
        if !known_names.contains(&key) {
            unplaced_real.push((r, "no player by that name in the season — check spelling"));
            continue;
        }
        if departed_names.contains(&key) {
            unplaced_real.push((
                r,
                "still a departure — the row matched nobody (check the team string)",
            ));
            continue;
        }
        // Known name, not departing, but never entered the projection: a
        // sub-gate player was never on the roster to restore. Harmless, and
        // checked before the bucket assertions below so it isn't reported as
        // one of them.
        if !qualified_names.contains(&key) {
            unplaced_benign.push(r);
            continue;
        }
        match r.parsed_status() {
            ReturnStatus::Granted if !returning_names.contains(&key) => {
                unplaced_real.push((r, "curated `granted` but not in the team's returning core"))
            }
            ReturnStatus::Contested if !uncertain_names.contains(&key) => unplaced_real.push((
                r,
                "curated `contested` but not in any team's uncertain bucket",
            )),
            _ => {}
        }
    }

    println!();
    if returns.is_empty() {
        // Not a warning. An empty capture is the correct starting point for a
        // season nobody has curated yet — see data/returns/README.md.
        println!("ELIGIBILITY RETURNS: none captured for {base}.");
    } else if unplaced_real.is_empty() {
        println!(
            "ELIGIBILITY RETURNS: all {} player_returns row(s) placed their player.",
            returns.len()
        );
    } else {
        println!(
            "UNPLACED ELIGIBILITY RETURNS ({}) — these are silently doing NOTHING:",
            unplaced_real.len()
        );
        for (r, why) in &unplaced_real {
            println!("  {:<28} {:<24} {why}", r.name, r.current_team);
        }
        println!(
            "  Fix data/returns/{base}_returns.json to match cstat's players.name / \
             teams.short_name, then re-run `cstat-ingest returns`."
        );
    }
    if !unplaced_benign.is_empty() {
        println!();
        println!(
            "  Note: {} return row(s) name a player below the projection's \
             {QUAL_MIN_GAMES_PLAYED} GP / {QUAL_MIN_MPG:.0} MPG gate. Harmless — they were \
             never on the projected roster, so there was nothing to restore:",
            unplaced_benign.len()
        );
        for r in &unplaced_benign {
            println!("    {:<28} {}", r.name, r.current_team);
        }
    }

    // --- 3. At-risk returners, ranked by what their exit would cost. -----

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

    // Both captures share one exit code: the caller's contract is "did any
    // curated row fail to do what it says", and which file it lives in doesn't
    // change the answer.
    Ok(unmatched_real.len() + unplaced_real.len())
}

/// The departing player's display name, whatever the reason variant.
fn departure_name(d: &DepartureReason) -> &str {
    match d {
        DepartureReason::GraduatedSenior { name, .. }
        | DepartureReason::Transferred { name, .. }
        | DepartureReason::DraftGone { name, .. }
        | DepartureReason::LeftProgram { name, .. } => name,
    }
}

/// Clip a display string to `max` chars so the fixed-width table stays aligned
/// on long program names ("George Washington Revolutionaries").
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
