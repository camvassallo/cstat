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
use cstat_core::team_name_match::team_match_score;
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
    /// Base-season CamPom, so a departure candidate can be ranked by what
    /// missing him would cost. Same column the projection reads.
    cam_v3: Option<f64>,
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
               max(tps.cam_gbpm_v3_psos) AS cam_v3,
               COALESCE(bool_or(pss.games_played >= $2 AND pss.minutes_per_game >= $3), false)
                   AS qualified
        FROM players p
        JOIN teams t ON t.id = p.team_id
        LEFT JOIN player_season_stats pss
               ON pss.player_id = p.id AND pss.season = p.season
        -- One Torvik profile per (player, season), same de-duplication the
        -- projection applies (#311) — without it a player carrying two profile
        -- rows fans the join out and double-counts in the GROUP BY.
        LEFT JOIN (
            SELECT DISTINCT ON (player_id, season) player_id, season, cam_gbpm_v3_psos
            FROM torvik_player_stats
            ORDER BY player_id, season, cam_gbpm_v3_psos DESC NULLS LAST
        ) tps ON tps.player_id = p.id AND tps.season = p.season
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
    // Keyed by (team, normalized name), NOT by name alone.
    //
    // cstat carries genuine same-name players across different teams in one
    // season — two Josh Reeds (Drexel and Penn St.) and two Marvin McGhees
    // (UC Santa Barbara and UT Rio Grande Valley) in 2026 alone. With
    // name-only sets, the sibling departing *anywhere in D-I* put the name in
    // `departed`, and a perfectly good row for the other one was reported as
    // "still a departure — the row matched nobody (check the team string)".
    // The message named the team; the check did not contain one.
    let mut departed: HashSet<(&str, String)> = HashSet::new();
    let mut uncertain: HashSet<(&str, String)> = HashSet::new();
    let mut returning: HashSet<(&str, String)> = HashSet::new();
    for p in &projections {
        let team = p.team_name.as_str();
        for d in &p.departures {
            departed.insert((team, normalize_player_name(departure_name(d))));
        }
        for (_, u) in &p.uncertain {
            uncertain.insert((team, normalize_player_name(&u.name)));
        }
        for r in &p.returning {
            if let Some(m) = meta_by_id.get(&r.player_id) {
                returning.insert((team, normalize_player_name(&m.name)));
            }
        }
    }

    let team_keys: Vec<(&str, &str)> = projections
        .iter()
        .map(|p| (p.team_name.as_str(), p.team_full_name.as_str()))
        .collect();

    let mut unplaced_real: Vec<(&PlayerReturn, &str)> = Vec::new();
    let mut unplaced_benign: Vec<&PlayerReturn> = Vec::new();
    for r in &returns {
        let key = normalize_player_name(&r.name);
        if !known_names.contains(&key) {
            unplaced_real.push((r, "no player by that name in the season — check spelling"));
            continue;
        }
        // Resolve the row's team the same way the projection does, so a team
        // string the projection accepts is never rejected here.
        //
        // BEST match, not first. Several program names prefix another's —
        // "Penn" also matches Penn St., "Texas A&M" matches Texas A&M-Corpus
        // Christi, "California" matches California Baptist — so taking the
        // first team that matches at all lands on the wrong program and
        // reports a correctly-placed row as unplaced. `roster_projection`'s
        // own resolver minimises the score; this has to do the same or the two
        // disagree about which team a curated row names.
        let Some(proj) = best_team_index(&team_keys, &r.current_team).map(|i| &projections[i])
        else {
            unplaced_real.push((r, "no team by that name — check the team string"));
            continue;
        };
        let team = proj.team_name.as_str();
        if departed.contains(&(team, key.clone())) {
            unplaced_real.push((r, "still a departure on that team — the row matched nobody"));
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
            ReturnStatus::Granted if !returning.contains(&(team, key.clone())) => {
                unplaced_real.push((r, "curated `granted` but not in the team's returning core"))
            }
            ReturnStatus::Contested if !uncertain.contains(&(team, key)) => unplaced_real.push((
                r,
                "curated `contested` but not in that team's uncertain bucket",
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

    // --- 3. Returners the official roster no longer lists. ---------------
    // The sharpest signal in this report, and the only section that is closer
    // to a detector than a worklist: it compares the projection's returning
    // cohort against what the school itself publishes. Gated hard on
    // `status = 'ok'` — schools publish "(Returners)" subsets and last
    // season's rosters all summer, and reading absence off either invents
    // departures wholesale (Gonzaga's four-man page would report nine).
    let rosters = fetch_official_rosters(pool, base + 1).await?;

    println!();
    if rosters.verdicts.is_empty() {
        println!(
            "OFFICIAL ROSTERS: none fetched for {}. Run `cstat-ingest rosters --year {}` \
             to enable the two roster-backed sections.",
            base + 1,
            base + 1
        );
    } else {
        let mut missing: Vec<(f64, &PlayerMeta, f64)> = Vec::new();
        for proj in &projections {
            let Some(listed) = rosters.trusted.get(&proj.team_name) else {
                continue;
            };
            let loose = rosters.trusted_loose.get(&proj.team_name);
            for r in &proj.returning {
                let Some(m) = meta_by_id.get(&r.player_id) else {
                    continue;
                };
                let key = normalize_player_name(&m.name);
                let present = listed.contains(&key)
                    || loose_key(&key).is_some_and(|k| loose.is_some_and(|s| s.contains(&k)));
                if !present {
                    missing.push((r.cam_v3.unwrap_or(0.0), m, r.mpg));
                }
            }
        }
        missing.sort_by(|a, b| b.0.total_cmp(&a.0));

        println!(
            "RETURNERS ABSENT FROM THE OFFICIAL ROSTER — {} trusted roster(s) of {} fetched, \
             {} name(s) absent, showing {}:",
            rosters.trusted.len(),
            rosters.verdicts.len(),
            missing.len(),
            missing.len().min(opts.limit),
        );
        println!(
            "  {:<26} {:<30} {:<4} {:>5} {:>7}",
            "PLAYER", "TEAM", "CLS", "MPG", "CAM"
        );
        for (cam, m, mpg) in missing.iter().take(opts.limit) {
            println!(
                "  {:<26} {:<30} {:<4} {:>5.1} {:>7.2}",
                truncate(&m.name, 26),
                truncate(&m.team_name, 30),
                m.class_year.as_deref().unwrap_or("?"),
                mpg,
                cam,
            );
        }
        println!(
            "  Absence is evidence, not proof — a walk-on omitted from the published roster \
             looks identical to an exit."
        );
        println!(
            "  Confirm against the news, then write the real ones into \
             data/departures/{base}_departures.json."
        );

        // --- 4. Seniors the school still lists — the 5-in-5 capture. -----
        // The mirror of section 3, and it reads PRESENCE rather than absence,
        // so it deliberately uses every fetch regardless of status: a name on
        // a partial page is still a name on the page. This is the only
        // automatic signal cstat has for the population `docs/eligibility_5in5.md`
        // describes as invisible — a senior taking the extra year AT THE SAME
        // SCHOOL, who enters no portal and appears in no feed.
        let mut staying: Vec<(f64, &PlayerMeta, String)> = Vec::new();
        for proj in &projections {
            let Some(listed) = rosters.any.get(&proj.team_name) else {
                continue;
            };
            for d in &proj.departures {
                let DepartureReason::GraduatedSenior { player_id, name } = d else {
                    continue;
                };
                let key = normalize_player_name(name);
                if !listed.contains(&key) {
                    continue;
                }
                let Some(m) = meta_by_id.get(player_id) else {
                    continue;
                };
                let label = rosters
                    .class_labels
                    .get(&(proj.team_name.clone(), key))
                    .cloned()
                    .unwrap_or_else(|| "?".to_string());
                staying.push((m.cam_v3.unwrap_or(0.0), m, label));
            }
        }
        staying.sort_by(|a, b| b.0.total_cmp(&a.0));

        println!();
        println!(
            "SENIORS STILL ON THE OFFICIAL ROSTER ({}) — the projection deletes these as \
             graduated, showing {}:",
            staying.len(),
            staying.len().min(opts.limit),
        );
        println!(
            "  {:<26} {:<30} {:<16} {:>7}",
            "PLAYER", "TEAM", "SCHOOL SAYS", "CAM"
        );
        for (cam, m, label) in staying.iter().take(opts.limit) {
            println!(
                "  {:<26} {:<30} {:<16} {:>7.2}",
                truncate(&m.name, 26),
                truncate(&m.team_name, 30),
                truncate(label, 16),
                cam,
            );
        }
        println!(
            "  Each is a candidate for data/returns/{base}_returns.json — `granted` if his \
             eligibility is settled, `contested` if it is being litigated."
        );
    }

    // --- 5. At-risk returners, ranked by what their exit would cost. -----

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

/// Official-roster rows for a target season, indexed for the two audit
/// sections that read them.
///
/// The `trusted` / `any` split is the whole point and is not a convenience.
/// A player's PRESENCE on a published roster is a fact from one row and holds
/// no matter how partial the page was, so section 4 reads `any`. A player's
/// ABSENCE is a claim about the completeness of the whole page, so section 3
/// reads `trusted`, which contains only fetches the ingest marked `ok`.
struct OfficialRosters {
    /// `teams.short_name` → normalized names, restricted to `status = 'ok'`.
    /// Membership is checked against BOTH this and [`Self::trusted_loose`].
    trusted: HashMap<String, HashSet<String>>,
    /// The same cohort keyed by first initial + surname.
    ///
    /// Exists because the two sides spell the same human differently often
    /// enough to matter: the school prints a middle name cstat omits ("David
    /// Ugonna Ike" vs "David Ike") or an initial where cstat has the full name.
    /// A miss here is a *false departure* on the report, which is the expensive
    /// direction — it sends someone to check a player who never left. The
    /// looser key can in principle collide (two same-surname players sharing an
    /// initial), and that collision hides a real departure instead. That is the
    /// cheaper error: it leaves the status quo, where nothing detected the
    /// departure at all.
    trusted_loose: HashMap<String, HashSet<String>>,
    /// `teams.short_name` → normalized names, every status.
    any: HashMap<String, HashSet<String>>,
    /// `(team, normalized name)` → the school's verbatim eligibility label
    /// ("R-Sr.", "5th", "Gr."), which is the informative part of section 4.
    class_labels: HashMap<(String, String), String>,
    /// `teams.short_name` → fetch status, for the section headers.
    verdicts: HashMap<String, String>,
}

#[derive(sqlx::FromRow)]
struct RosterRow {
    team_short_name: String,
    status: String,
    normalized_name: Option<String>,
    class_year_raw: Option<String>,
}

async fn fetch_official_rosters(pool: &PgPool, season: i32) -> Result<OfficialRosters> {
    // LEFT JOIN rather than INNER: a fetch with zero players still has to reach
    // `verdicts`, or a school that published nothing is indistinguishable from
    // one we never asked.
    let rows: Vec<RosterRow> = sqlx::query_as::<_, RosterRow>(
        r#"
        SELECT f.team_short_name, f.status,
               p.normalized_name, p.class_year_raw
        FROM team_roster_fetches f
        LEFT JOIN team_roster_players p
               ON p.season = f.season AND p.team_short_name = f.team_short_name
        WHERE f.season = $1
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    let mut out = OfficialRosters {
        trusted: HashMap::new(),
        trusted_loose: HashMap::new(),
        any: HashMap::new(),
        class_labels: HashMap::new(),
        verdicts: HashMap::new(),
    };
    for r in rows {
        out.verdicts
            .insert(r.team_short_name.clone(), r.status.clone());
        let Some(name) = r.normalized_name else {
            continue;
        };
        if r.status == "ok" {
            out.trusted
                .entry(r.team_short_name.clone())
                .or_default()
                .insert(name.clone());
            if let Some(loose) = loose_key(&name) {
                out.trusted_loose
                    .entry(r.team_short_name.clone())
                    .or_default()
                    .insert(loose);
            }
        }
        out.any
            .entry(r.team_short_name.clone())
            .or_default()
            .insert(name.clone());
        if let Some(cls) = r.class_year_raw {
            out.class_labels.insert((r.team_short_name, name), cls);
        }
    }
    Ok(out)
}

/// Index of the team a curated row's `current_team` names, mirroring
/// `roster_projection::resolve_team_id`.
///
/// The mirroring is the point. Several program names prefix another's — "Penn"
/// also matches Penn St., "Texas A&M" matches Texas A&M-Corpus Christi,
/// "California" matches California Baptist — so *any* match is not good enough;
/// it has to be the same match the projection made, or the audit reports a
/// correctly-placed row as doing nothing and sends someone to fix a file that
/// is already right.
fn best_team_index(teams: &[(&str, &str)], want: &str) -> Option<usize> {
    teams
        .iter()
        .enumerate()
        .filter_map(|(i, (short, full))| {
            team_match_score(Some(short), full, want).map(|score| (score, i))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, i)| i)
}

/// First initial + transliteration-folded surname, from an already-normalized
/// name. `None` for a single-token name, where the key would be no looser than
/// the name itself.
fn loose_key(normalized: &str) -> Option<String> {
    let mut parts = normalized.split_whitespace();
    let first = parts.next()?;
    let last = parts.next_back()?;
    Some(format!(
        "{} {}",
        first.chars().next()?,
        fold_umlaut_spellings(last)
    ))
}

/// Collapse the two ways a German name reaches the two sources.
///
/// `normalize_player_name` folds `ü` to `u`, so Virginia's "Johann Grünloh"
/// becomes `grunloh` while cstat's transliterated "Johann Gruenloh" becomes
/// `gruenloh`, and a rostered player reads as departed. Collapsing the
/// digraphs makes both `grunloh`.
///
/// This *is* lossy for names that legitimately contain the digraph — "samuel"
/// folds to "samul" — but it is applied to both sides of the comparison, so
/// those still match each other. The only failure it can introduce is two
/// distinct surnames colliding, which hides a departure rather than inventing
/// one: the safe direction for a worklist.
fn fold_umlaut_spellings(s: &str) -> String {
    s.replace('\u{00df}', "s")
        .replace("ue", "u")
        .replace("oe", "o")
        .replace("ae", "a")
        .replace("ss", "s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_team_index_prefers_the_exact_program_over_one_it_prefixes() {
        // Ordered so the WRONG team comes first: taking the first match rather
        // than the best is exactly the bug this guards.
        let teams = [
            ("Penn St.", "Penn State Nittany Lions"),
            ("Penn", "Penn Quakers"),
            ("California Baptist", "California Baptist Lancers"),
            ("California", "California Golden Bears"),
            (
                "Texas A&M Corpus Christi",
                "Texas A&M Corpus Christi Islanders",
            ),
            ("Texas A&M", "Texas A&M Aggies"),
        ];
        for (want, expect) in [
            ("Penn", "Penn"),
            ("Penn St.", "Penn St."),
            ("California", "California"),
            ("Texas A&M", "Texas A&M"),
        ] {
            let got = best_team_index(&teams, want).map(|i| teams[i].0);
            assert_eq!(got, Some(expect), "{want} resolved to {got:?}");
        }
    }

    #[test]
    fn best_team_index_declines_a_name_that_matches_nothing() {
        let teams = [("Duke", "Duke Blue Devils")];
        assert_eq!(best_team_index(&teams, "Nowhere State"), None);
    }

    #[test]
    fn loose_key_pairs_a_middle_name_with_its_plain_form() {
        // The school prints the middle name, cstat does not. Both must reduce
        // to the same key or the projection's returner reads as departed.
        assert_eq!(loose_key("david ugonna ike"), loose_key("david ike"));
        assert_eq!(loose_key("david ike").as_deref(), Some("d ike"));
    }

    #[test]
    fn loose_key_separates_same_surname_teammates() {
        // Georgia rosters two Millenders. Different initials must stay
        // distinct, or one covers for the other's departure.
        assert_ne!(
            loose_key("marcus millender"),
            loose_key("kemauri millender")
        );
    }

    #[test]
    fn loose_key_bridges_umlaut_and_transliterated_spellings() {
        // Virginia publishes "Grünloh" (normalizes to grunloh); cstat carries
        // the transliterated "Gruenloh". Both must reach one key.
        assert_eq!(loose_key("johann grunloh"), loose_key("johann gruenloh"));
    }

    #[test]
    fn loose_key_declines_single_token_names() {
        assert_eq!(loose_key("pele"), None);
        assert_eq!(loose_key(""), None);
    }
}
