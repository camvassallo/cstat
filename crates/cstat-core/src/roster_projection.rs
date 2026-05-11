//! 2027 roster projection: compose a hypothetical N+1 roster per team from
//! N's qualified roster minus departures plus incoming portal transfers,
//! with a separate "uncertain" bucket for declared-but-uncommitted NBA
//! draft entrants so the API can surface floor (all-`?`-leave) and
//! ceiling (all-`?`-return) bounds.
//!
//! This module is data composition only — it does not run inference. The
//! caller (the API route) builds features via
//! [`roster_features::build_roster_features`] over the materialized
//! roster and feeds them to [`Predictor::predict_adj_em`].
//!
//! Honest scope for v1 (frozen-stats, no growth model, no recruits):
//! - **Returning players**: use their *N* (most recently completed
//!   season) stats verbatim, with their N-season MPG. Real coaches
//!   would reallocate minutes after departures; we don't try to model
//!   that. Teams that lose lots of players will look thinner than they
//!   actually will be — the route surfaces a `roster_size` count so
//!   the UI can flag obviously-incomplete projections.
//! - **Incoming transfers**: use their N stats from their *source*
//!   team. So the incoming row's `mpg` is the role they played at their
//!   old school, not what they'll play at the destination.
//! - **Recruits**: out of scope. Big-roster-loss teams that recruit a
//!   strong freshman class will look pessimistic.
//! - **Growth**: out of scope. A junior who's about to break out as a
//!   senior is just their junior line in the model's view.
//!
//! These limitations are documented; the next iteration (Phase 5c growth
//! model + Phase 5b recruiting cohort) is the path to better projections.

use crate::roster_features::{PlayerRow, QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use crate::team_name_match::team_match_score;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

/// Which NBA-draft scenario to materialize. The floor / ceiling pair is
/// the API's honesty story: we don't know if a `declared` player will
/// withdraw before the deadline, so we project both bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftScenario {
    /// Treat every roster member flagged `declared` (NBA early entry,
    /// status unresolved) as gone. Conservative projection.
    Floor,
    /// Treat every `declared` flag as a withdrawal — the player stays.
    /// Optimistic projection.
    Ceiling,
}

/// Reason a player is no longer on the projected roster. Stored for
/// auditability in the route response — users want to know *why* a
/// team's projection dropped.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DepartureReason {
    /// `class_year = 'Sr'` at season N → graduating.
    GraduatedSenior { player_id: Uuid, name: String },
    /// In the `transfers` table for portal class N, source = this team.
    Transferred {
        player_id: Uuid,
        name: String,
        destination: Option<String>,
    },
    /// On the NBA-draft early-entrants list with status `gone` (firm
    /// commitment, not just `declared`). Always counts as departing.
    DraftGone { player_id: Uuid, name: String },
}

/// A declared-but-uncommitted draft entrant. They count as returning in
/// the ceiling scenario and as departing in the floor scenario.
#[derive(Debug, Clone, Serialize)]
pub struct UncertainPlayer {
    pub player_id: Uuid,
    pub name: String,
    /// Free-text reason ("declared for NBA draft", "in portal but
    /// uncommitted", etc.). Keep human-readable for the UI tooltip.
    pub reason: String,
}

/// One team's projected N+1 roster. The caller picks `Floor` or
/// `Ceiling` via [`Self::for_scenario`] to materialize the player rows
/// fed into the model.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectedRoster {
    /// The season-N team UUID. We project AGAINST this team's identity;
    /// no season-(N+1) `teams` row exists yet because the upcoming
    /// season hasn't been ingested.
    pub team_id: Uuid,
    /// Torvik short name or NatStat short_name — whichever is populated.
    pub team_name: String,
    /// Full NatStat name; used for UI links.
    pub team_full_name: String,
    /// Returning players who are firmly back (not Sr, not in portal,
    /// not draft-gone, not draft-declared). Carry their season-N
    /// PlayerRow verbatim.
    pub returning: Vec<PlayerRow>,
    /// Incoming portal transfers committed to this team. Carry their
    /// season-N PlayerRow from their *source* team.
    pub arrivals: Vec<PlayerRow>,
    /// Players who are returning in the ceiling scenario but gone in
    /// the floor scenario (declared draft entrants whose withdrawal
    /// status is still TBD). Their PlayerRow lives in `returning` only
    /// in the ceiling materialization.
    pub uncertain: Vec<(PlayerRow, UncertainPlayer)>,
    /// Audit trail: who left and why. Sized for UI display, not used by
    /// inference.
    pub departures: Vec<DepartureReason>,
}

impl ProjectedRoster {
    /// Materialize the player list the model should see under a given
    /// scenario. Returning + arrivals always; uncertain only under
    /// ceiling.
    pub fn for_scenario(&self, scenario: DraftScenario) -> Vec<PlayerRow> {
        let mut out: Vec<PlayerRow> =
            Vec::with_capacity(self.returning.len() + self.arrivals.len() + self.uncertain.len());
        out.extend(self.returning.iter().cloned());
        out.extend(self.arrivals.iter().cloned());
        if scenario == DraftScenario::Ceiling {
            out.extend(self.uncertain.iter().map(|(p, _)| p.clone()));
        }
        out
    }
}

/// One row of the draft early-entrants JSON. Fields match the v1 shape
/// described in `data/draft/2026_early_entrants.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftEntrant {
    pub name: String,
    pub current_team: String,
    pub status: String,
}

/// Load + parse `data/draft/{year}_early_entrants.json`. Caller is the
/// route handler; the route holds `season` and constructs the path.
pub fn load_draft_entrants(path: &Path) -> Result<Vec<DraftEntrant>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Vec<DraftEntrant> = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// Helper struct for the batch roster fetch. We pull every qualified
/// player on every team for the base season in one query; partition by
/// team_id in Rust. PlayerRow fields plus team_id, full name (for
/// audit), class_year (for senior detection).
#[derive(sqlx::FromRow, Clone)]
struct RosterRow {
    player_id: Uuid,
    player_name: String,
    team_id: Uuid,
    class_year: Option<String>,
    total_min: f64,
    mpg: f64,
    ppg: Option<f64>,
    rpg: Option<f64>,
    apg: Option<f64>,
    spg: Option<f64>,
    bpg: Option<f64>,
    topg: Option<f64>,
    ts: Option<f64>,
    efg: Option<f64>,
    usg: Option<f64>,
    ast_pct: Option<f64>,
    tov_pct: Option<f64>,
    orb_pct: Option<f64>,
    drb_pct: Option<f64>,
    stl_pct: Option<f64>,
    blk_pct: Option<f64>,
    ft_rate: Option<f64>,
    primary_class: Option<String>,
    cam_v3: Option<f64>,
}

impl RosterRow {
    fn into_player_row(self) -> PlayerRow {
        PlayerRow {
            player_id: self.player_id,
            total_min: self.total_min,
            mpg: self.mpg,
            ppg: self.ppg,
            rpg: self.rpg,
            apg: self.apg,
            spg: self.spg,
            bpg: self.bpg,
            topg: self.topg,
            ts: self.ts,
            efg: self.efg,
            usg: self.usg,
            ast_pct: self.ast_pct,
            tov_pct: self.tov_pct,
            orb_pct: self.orb_pct,
            drb_pct: self.drb_pct,
            stl_pct: self.stl_pct,
            blk_pct: self.blk_pct,
            ft_rate: self.ft_rate,
            primary_class: self.primary_class,
            cam_v3: self.cam_v3,
        }
    }
}

/// One row from `teams` for the base season — minimum we need for
/// 247-side name resolution.
#[derive(sqlx::FromRow, Clone)]
struct TeamRow {
    id: Uuid,
    name: String,
    short_name: Option<String>,
}

/// One row from `transfers` for the base year. We only need the
/// resolved `cstat_player_id` (per the ingest resolver) and the raw
/// 247 destination name; the source team is back-derived from the
/// player's PSS row, and the audit-trail name comes from the same
/// `roster_rows` query that built the rest of the projection.
#[derive(sqlx::FromRow)]
struct TransferLink {
    cstat_player_id: Option<Uuid>,
    destination_institution: Option<String>,
}

/// Resolve a 247 short name to a team_id at the given season by best
/// match score across the supplied teams. `None` when no team matches.
fn resolve_team_id(teams: &[TeamRow], short: &str) -> Option<Uuid> {
    teams
        .iter()
        .filter_map(|t| {
            team_match_score(t.short_name.as_deref(), &t.name, short).map(|s| (s, t.id))
        })
        .min_by_key(|(s, _)| *s)
        .map(|(_, id)| id)
}

/// Match a draft entrant `(name, current_team)` to a season-N player_id
/// by normalized name + team-id resolution. Returns `None` when the
/// player isn't on a cstat-known D-I roster (e.g., walk-ons, foreign
/// transfers we haven't ingested).
fn match_draft_entrant(
    entrant: &DraftEntrant,
    players_by_name: &HashMap<String, Vec<(Uuid, Uuid)>>, // norm_name → [(player_id, team_id)]
    teams: &[TeamRow],
) -> Option<Uuid> {
    let key = normalize_player_name(&entrant.name);
    let candidates = players_by_name.get(&key)?;
    let want_team_id = resolve_team_id(teams, &entrant.current_team)?;
    candidates
        .iter()
        .find(|(_, tid)| *tid == want_team_id)
        .map(|(pid, _)| *pid)
}

/// Player-name normalization for cross-source joins. Same logic the
/// transfers route uses for portal players: lowercase, strip accents,
/// drop generational suffixes. Kept locally rather than promoted to a
/// shared module — only the player-name use case wants the suffix
/// stripping, and it's a small enough function that duplicating is
/// cheaper than yet another shared module.
fn normalize_player_name(name: &str) -> String {
    let folded: String = name
        .chars()
        .flat_map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => {
                Some('a')
            }
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => Some('e'),
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => Some('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => Some('o'),
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => Some('u'),
            'ñ' | 'Ñ' => Some('n'),
            'ç' | 'Ç' => Some('c'),
            _ if c.is_alphabetic() || c.is_whitespace() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    folded
        .split_whitespace()
        .filter(|w| !matches!(*w, "jr" | "sr" | "ii" | "iii" | "iv" | "v" | "lll"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compose every team's projected N+1 roster from base-season-N data.
/// One DB round-trip per source table (teams, players-with-stats,
/// transfers); the partitioning happens in Rust.
///
/// `base_season` = N (the most recently completed season; for cstat's
/// 2026 = 2025-26 college season, the 2026 portal class moves players
/// from N=2026 into N+1=2027). `draft_entrants` is the optional
/// declared/gone list — pass `&[]` to skip draft-cohort handling
/// entirely (every player who isn't a Sr or in the portal is treated
/// as returning).
pub async fn compose_all_projections(
    pool: &PgPool,
    base_season: i32,
    draft_entrants: &[DraftEntrant],
) -> Result<Vec<ProjectedRoster>, sqlx::Error> {
    // --- Pull every input table in one shot. ----------------------------
    let teams: Vec<TeamRow> =
        sqlx::query_as::<_, TeamRow>(r#"SELECT id, name, short_name FROM teams WHERE season = $1"#)
            .bind(base_season)
            .fetch_all(pool)
            .await?;

    let roster_rows: Vec<RosterRow> = sqlx::query_as::<_, RosterRow>(
        r#"
        SELECT
            p.id   AS player_id,
            p.name AS player_name,
            pss.team_id,
            p.class_year,
            (COALESCE(pss.minutes_per_game, 0) * COALESCE(pss.games_played, 0))::float8 AS total_min,
            COALESCE(pss.minutes_per_game, 0)::float8 AS mpg,
            pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
            pss.true_shooting_pct AS ts,
            pss.effective_fg_pct  AS efg,
            pss.usage_rate        AS usg,
            pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
            pss.stl_pct, pss.blk_pct, pss.ft_rate,
            pa.primary_class,
            tps.cam_gbpm_v3_psos AS cam_v3
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        WHERE pss.season = $1
          AND COALESCE(pss.games_played, 0) >= $2
          AND COALESCE(pss.minutes_per_game, 0) >= $3
        "#,
    )
    .bind(base_season)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(pool)
    .await?;

    let transfers: Vec<TransferLink> = sqlx::query_as::<_, TransferLink>(
        r#"
        SELECT cstat_player_id, destination_institution
        FROM transfers WHERE year = $1
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;

    // --- Bucket the inputs by team_id. ----------------------------------
    // Roster + audit metadata per team. The String alongside RosterRow
    // is a clone of the player's cstat name — used downstream for
    // DepartureReason audit messages without re-borrowing the row.
    let mut roster_by_team: HashMap<Uuid, Vec<(RosterRow, String)>> = HashMap::new();
    // Normalized-name → [(player_id, team_id)] for draft-entrant matching.
    let mut players_by_name: HashMap<String, Vec<(Uuid, Uuid)>> = HashMap::new();
    // player_id → source_team_id for outbound transfer attribution.
    let mut player_team: HashMap<Uuid, Uuid> = HashMap::new();
    for row in roster_rows {
        let pid = row.player_id;
        let name = row.player_name.clone();
        let team_id = row.team_id;
        players_by_name
            .entry(normalize_player_name(&name))
            .or_default()
            .push((pid, team_id));
        player_team.insert(pid, team_id);
        roster_by_team.entry(team_id).or_default().push((row, name));
    }

    // Transfers: bucket outbound by source_team_id (= which team is
    // losing a player) and incoming by destination_team_id (= which
    // team is gaining one). The route's existing ingestion populated
    // `cstat_player_id` per row; we use it both as the "outbound
    // player to remove from returning" identity AND as the PlayerRow
    // key to clone into the destination's `arrivals`. Audit display
    // names come from `roster_by_team` (cstat-canonical), not from
    // the 247-side string on the transfer row.
    let mut outbound_by_team: HashMap<Uuid, Vec<(Uuid, Option<String>)>> = HashMap::new();
    let mut incoming_by_team: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for t in &transfers {
        let Some(pid) = t.cstat_player_id else {
            continue;
        };
        let Some(&source_team_id) = player_team.get(&pid) else {
            continue; // resolved cstat_player_id but the player no longer in our roster fetch
        };
        outbound_by_team
            .entry(source_team_id)
            .or_default()
            .push((pid, t.destination_institution.clone()));

        if let Some(dest_str) = t.destination_institution.as_deref()
            && let Some(dest_team_id) = resolve_team_id(&teams, dest_str)
        {
            incoming_by_team.entry(dest_team_id).or_default().push(pid);
        }
    }

    // --- Draft entrants: status → action mapping. -----------------------
    // `gone` ⇒ unconditional departure (audit reason DraftGone).
    // `declared` ⇒ uncertain; included in ceiling, excluded in floor.
    // Anything else (`staying`, `withdrawn`) ⇒ no effect (player returns
    // through the normal returning path).
    let mut firm_draft_gone: HashSet<Uuid> = HashSet::new();
    let mut declared_draft: HashSet<Uuid> = HashSet::new();
    for entrant in draft_entrants {
        let Some(pid) = match_draft_entrant(entrant, &players_by_name, &teams) else {
            continue;
        };
        match entrant.status.as_str() {
            "gone" => {
                firm_draft_gone.insert(pid);
            }
            "declared" => {
                declared_draft.insert(pid);
            }
            _ => {} // "staying", "withdrawn", unknown — no roster impact
        }
    }

    // --- Per-team composition. ------------------------------------------
    // PlayerRow lookup so the incoming-portal arrivals can pull the
    // source-team PlayerRow without re-querying.
    let player_row_lookup: HashMap<Uuid, PlayerRow> = roster_by_team
        .values()
        .flat_map(|rows| {
            rows.iter()
                .map(|(r, _)| (r.player_id, r.clone().into_player_row()))
        })
        .collect();

    let mut out: Vec<ProjectedRoster> = Vec::with_capacity(teams.len());
    for team in &teams {
        let Some(rows) = roster_by_team.get(&team.id) else {
            // Team with no qualified players in the gate — skip rather
            // than emit a zero-feature projection that the model can't
            // sensibly score.
            continue;
        };
        let outbound_pids: HashSet<Uuid> = outbound_by_team
            .get(&team.id)
            .map(|v| v.iter().map(|(p, _)| *p).collect())
            .unwrap_or_default();

        let mut returning: Vec<PlayerRow> = Vec::new();
        let mut uncertain: Vec<(PlayerRow, UncertainPlayer)> = Vec::new();
        let mut departures: Vec<DepartureReason> = Vec::new();

        for (row, name) in rows {
            let pid = row.player_id;
            // Senior graduating? class_year fits {'Sr', 'SR', 'Senior'};
            // cstat normalizes to 'Sr' but tolerate variants.
            let is_senior = row
                .class_year
                .as_deref()
                .is_some_and(|c| matches!(c, "Sr" | "SR" | "Senior" | "sr" | "senior"));
            if is_senior {
                departures.push(DepartureReason::GraduatedSenior {
                    player_id: pid,
                    name: name.clone(),
                });
                continue;
            }
            // Outbound portal commit?
            if outbound_pids.contains(&pid) {
                let dest = outbound_by_team.get(&team.id).and_then(|v| {
                    v.iter()
                        .find(|(p, _)| *p == pid)
                        .and_then(|(_, d)| d.clone())
                });
                departures.push(DepartureReason::Transferred {
                    player_id: pid,
                    name: name.clone(),
                    destination: dest,
                });
                continue;
            }
            // Firm NBA draft departure?
            if firm_draft_gone.contains(&pid) {
                departures.push(DepartureReason::DraftGone {
                    player_id: pid,
                    name: name.clone(),
                });
                continue;
            }
            // Declared (uncertain) → bucket separately so the route can
            // surface floor/ceiling. Player row carried so ceiling
            // materialization includes them.
            if declared_draft.contains(&pid) {
                uncertain.push((
                    row.clone().into_player_row(),
                    UncertainPlayer {
                        player_id: pid,
                        name: name.clone(),
                        reason: "declared for NBA draft (status pending)".into(),
                    },
                ));
                continue;
            }
            // Otherwise: returning.
            returning.push(row.clone().into_player_row());
        }

        // Incoming portal arrivals (their season-N source-team PlayerRow).
        let arrivals: Vec<PlayerRow> = incoming_by_team
            .get(&team.id)
            .map(|pids| {
                pids.iter()
                    .filter_map(|p| player_row_lookup.get(p).cloned())
                    .collect()
            })
            .unwrap_or_default();

        out.push(ProjectedRoster {
            team_id: team.id,
            team_name: team.short_name.clone().unwrap_or_else(|| team.name.clone()),
            team_full_name: team.name.clone(),
            returning,
            arrivals,
            uncertain,
            departures,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(mpg: f64, cam_v3: Option<f64>) -> PlayerRow {
        PlayerRow {
            player_id: Uuid::new_v4(),
            total_min: mpg * 30.0,
            mpg,
            ppg: Some(10.0),
            rpg: None,
            apg: None,
            spg: None,
            bpg: None,
            topg: None,
            ts: Some(0.55),
            efg: None,
            usg: Some(20.0),
            ast_pct: None,
            tov_pct: None,
            orb_pct: None,
            drb_pct: None,
            stl_pct: None,
            blk_pct: None,
            ft_rate: None,
            primary_class: Some("Wizard".into()),
            cam_v3,
        }
    }

    #[test]
    fn for_scenario_includes_uncertain_only_in_ceiling() {
        let returning = vec![pr(30.0, Some(5.0))];
        let arrivals = vec![pr(25.0, Some(4.0))];
        let uncertain = vec![(
            pr(20.0, Some(3.0)),
            UncertainPlayer {
                player_id: Uuid::new_v4(),
                name: "X".into(),
                reason: "draft".into(),
            },
        )];
        let r = ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "Foo".into(),
            team_full_name: "Foo Bar".into(),
            returning,
            arrivals,
            uncertain,
            departures: vec![],
        };
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 2);
        assert_eq!(r.for_scenario(DraftScenario::Ceiling).len(), 3);
    }

    #[test]
    fn normalize_player_name_strips_suffix_and_case() {
        assert_eq!(
            normalize_player_name("Christian Anderson Jr."),
            "christian anderson",
        );
        assert_eq!(normalize_player_name("Cooper Flagg"), "cooper flagg");
    }
}
