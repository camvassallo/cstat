//! Head-coach ingestion from barttorvik's `coachdict.json`.
//!
//! Pulls the all-seasons coach dictionary (year → team name → coach;
//! see [`crate::torvik::TorkvikClient::fetch_coachdict`]) and lands it into the
//! `coaches` entity table + `coach_seasons` mapping (migration 024).
//!
//! Two things make this more than a dumb upsert:
//!   1. **Team-name join** — coachdict uses Torvik-style names ("North
//!      Carolina", "Texas A&M Corpus Chris"); we resolve each to a
//!      season-scoped `teams.id` with the shared
//!      [`team_match_score`] reconciliation (same scorer the transfers /
//!      recruits / roster_projection paths use). Unmatched rows are still
//!      stored (NULL `team_id`) so the coach-tenure history is complete.
//!   2. **`is_new_hc` flag** — `coachdict[Y][team] != coachdict[Y-1][team]`,
//!      computed from the FULL coachdict (every year), so it's populated even
//!      for the earliest ingested season. NULL when Y-1 has no entry.
//!
//! We only ingest seasons we carry `teams` for (the cstat data footprint) —
//! coachdict reaches back to 1893, but rows for seasons we can't join are noise.
//! The Y-1 lookup still consults the full dict regardless.

use crate::torvik::TorkvikClient;
use cstat_core::team_name_match::team_match_score;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct CoachIngestReport {
    pub seasons: usize,
    pub rows: u64,
    pub matched_teams: u64,
    pub unmatched_teams: u64,
    pub distinct_coaches: usize,
    pub new_hc: u64,
    pub inverted_skipped: u64,
}

#[derive(sqlx::FromRow)]
struct CandidateTeam {
    team_id: Uuid,
    natstat_id: String,
    short_name: Option<String>,
    full_name: String,
}

/// Ingest coachdict into `coaches` + `coach_seasons`. When `year_filter` is
/// `Some(Y)`, only season Y is written (the Y-1 change flag still reads the
/// full dict); otherwise every season present in the `teams` table is ingested.
pub async fn ingest_coaches(
    client: &TorkvikClient,
    pool: &PgPool,
    year_filter: Option<i32>,
) -> anyhow::Result<CoachIngestReport> {
    let coachdict = client.fetch_coachdict().await?;

    // Target seasons = those we carry teams for (optionally filtered to one).
    let mut seasons: Vec<i32> = sqlx::query_scalar(
        "SELECT DISTINCT season FROM teams WHERE ($1::int IS NULL OR season = $1) ORDER BY season",
    )
    .bind(year_filter)
    .fetch_all(pool)
    .await?;
    seasons.retain(|s| coachdict.contains_key(s));

    // The upcoming *projection* season (max-played + 1) has no `teams` rows yet
    // (no games), so it never appears above — but coachdict carries it (offseason
    // hires land before tip-off). Ingest it for the Future tab: resolve its
    // coachdict team names against the BASE (max-played) season's teams so
    // `team_natstat_id` is populated (team_id stays NULL — there's no target-season
    // team to reference). `fetch_coach_cae` joins on natstat_id and prefers the
    // target season, so the projection ledger then shows the incoming coach.
    let max_played: Option<i32> = sqlx::query_scalar("SELECT MAX(season) FROM teams")
        .fetch_one(pool)
        .await?;
    if let Some(mp) = max_played {
        let upcoming = mp + 1;
        let wanted = year_filter.is_none() || year_filter == Some(upcoming);
        if wanted && coachdict.contains_key(&upcoming) && !seasons.contains(&upcoming) {
            seasons.push(upcoming);
        }
    }

    if seasons.is_empty() {
        warn!(
            ?year_filter,
            "no overlapping seasons between teams table and coachdict"
        );
        return Ok(CoachIngestReport::default());
    }

    let mut report = CoachIngestReport::default();
    let mut coach_cache: HashMap<String, Uuid> = HashMap::new();

    for season in &seasons {
        let teams_this_season = &coachdict[season];
        let prev = coachdict.get(&(season - 1));

        // The upcoming projection season has no teams of its own, so match its
        // coach names against the base (max-played) season's teams and store
        // `team_natstat_id` only — `team_id` would wrongly point at a base-season
        // UUID for a target-season row.
        let is_upcoming = max_played.is_some_and(|mp| *season > mp);
        let cand_season = if is_upcoming {
            max_played.expect("is_upcoming implies max_played is Some")
        } else {
            *season
        };
        let candidates: Vec<CandidateTeam> = sqlx::query_as(
            r#"SELECT id AS team_id, natstat_id, short_name, name AS full_name
               FROM teams WHERE season = $1"#,
        )
        .bind(cand_season)
        .fetch_all(pool)
        .await?;

        for (team_name, coach) in teams_this_season {
            // Resolve coachdict team name → season-scoped cstat team (best score).
            let matched = resolve_team(&candidates, team_name);

            // coachdict carries a redundant *inverted* entry for some coaches:
            // alongside `"LSU" -> "Matt McMahon"` it also stores
            // `"Matt McMahon" -> "LSU"`. Skip the inverted half so we don't mint
            // junk coach entities named after teams. It's the inverted half when
            // the mirror (`coach -> team_name`) exists in this season AND the
            // value resolves to a real team while the key (team_name) does not —
            // which keeps legitimately-unmatched teams (2021 Ivies, Le Moyne).
            let mirror_back = teams_this_season
                .get(coach)
                .map(|v| v == team_name)
                .unwrap_or(false);
            let value_is_team = resolve_team(&candidates, coach).is_some();
            if is_inverted_entry(mirror_back, matched.is_some(), value_is_team) {
                report.inverted_skipped += 1;
                continue;
            }

            let coach_id = get_or_create_coach(pool, &mut coach_cache, coach).await?;

            match matched {
                Some(_) => report.matched_teams += 1,
                None => report.unmatched_teams += 1,
            }

            // is_new_hc: known-changed / known-same / unknown (NULL).
            let is_new_hc = coaching_change_flag(prev, team_name, coach);
            if is_new_hc == Some(true) {
                report.new_hc += 1;
            }

            sqlx::query(
                r#"INSERT INTO coach_seasons
                       (coach_id, season, coachdict_team_name, team_id, team_natstat_id, is_new_hc)
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT (season, coachdict_team_name) DO UPDATE SET
                       coach_id        = EXCLUDED.coach_id,
                       team_id         = EXCLUDED.team_id,
                       team_natstat_id = EXCLUDED.team_natstat_id,
                       is_new_hc       = EXCLUDED.is_new_hc,
                       fetched_at      = NOW()"#,
            )
            .bind(coach_id)
            .bind(season)
            .bind(team_name)
            // Upcoming-season rows carry only the cross-season natstat_id; the
            // base-season team_id must not leak into a target-season row.
            .bind(if is_upcoming {
                None
            } else {
                matched.map(|c| c.team_id)
            })
            .bind(matched.map(|c| c.natstat_id.clone()))
            .bind(is_new_hc)
            .execute(pool)
            .await?;

            report.rows += 1;
        }
    }

    report.seasons = seasons.len();
    report.distinct_coaches = coach_cache.len();
    info!(
        seasons = report.seasons,
        rows = report.rows,
        matched = report.matched_teams,
        unmatched = report.unmatched_teams,
        coaches = report.distinct_coaches,
        new_hc = report.new_hc,
        inverted_skipped = report.inverted_skipped,
        "coachdict ingest complete"
    );
    Ok(report)
}

/// Best-scoring cstat team for a coachdict team name within one season's
/// candidates, or `None` when nothing matches (shared `team_match_score`).
fn resolve_team<'a>(candidates: &'a [CandidateTeam], name: &str) -> Option<&'a CandidateTeam> {
    candidates
        .iter()
        .filter_map(|c| {
            team_match_score(c.short_name.as_deref(), &c.full_name, name).map(|s| (s, c))
        })
        .min_by_key(|(s, _)| *s)
        .map(|(_, c)| c)
}

/// Is `(team_name -> coach)` the inverted half of a coachdict bidirectional
/// pair? True when the mirror `coach -> team_name` exists, the *value* resolves
/// to a real team, and the *key* does not — i.e. the key is actually the coach.
/// Guards against dropping legitimately-unmatched teams (where no mirror exists).
fn is_inverted_entry(mirror_back: bool, key_is_team: bool, value_is_team: bool) -> bool {
    mirror_back && value_is_team && !key_is_team
}

/// Offseason coaching-change flag for `team` in the season whose prior season
/// is `prev` (the `coachdict[Y-1]` map). `Some(true)` = coach differs from last
/// year, `Some(false)` = known same, `None` = can't tell (no prior entry).
/// Name-string equality IS the comparison, so a Bruce → Steven Pearl handoff
/// reads as a change and Rick ≠ Richard Pitino are never conflated.
fn coaching_change_flag(
    prev: Option<&HashMap<String, String>>,
    team: &str,
    coach: &str,
) -> Option<bool> {
    prev.and_then(|p| p.get(team))
        .map(|prev_coach| prev_coach != coach)
}

/// Resolve a coach name to a `coaches.id`, creating the entity on first sight.
/// `canonical_name` is UNIQUE and the dedup key — Rick Pitino and Richard
/// Pitino are distinct names → distinct rows (see migration 024).
async fn get_or_create_coach(
    pool: &PgPool,
    cache: &mut HashMap<String, Uuid>,
    name: &str,
) -> anyhow::Result<Uuid> {
    if let Some(id) = cache.get(name) {
        return Ok(*id);
    }
    let id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO coaches (canonical_name) VALUES ($1)
           ON CONFLICT (canonical_name) DO UPDATE SET canonical_name = EXCLUDED.canonical_name
           RETURNING id"#,
    )
    .bind(name)
    .fetch_one(pool)
    .await?;
    cache.insert(name.to_string(), id);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn season(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(t, c)| (t.to_string(), c.to_string()))
            .collect()
    }

    #[test]
    fn change_flag_none_when_no_prior_season() {
        assert_eq!(coaching_change_flag(None, "Duke", "Jon Scheyer"), None);
    }

    #[test]
    fn change_flag_none_when_team_absent_prior() {
        let prev = season(&[("Kansas", "Bill Self")]);
        // Team transitioning into D-I — no prior coachdict entry → unknown, not false.
        assert_eq!(
            coaching_change_flag(Some(&prev), "Duke", "Jon Scheyer"),
            None
        );
    }

    #[test]
    fn change_flag_false_when_same_coach() {
        let prev = season(&[("Duke", "Jon Scheyer")]);
        assert_eq!(
            coaching_change_flag(Some(&prev), "Duke", "Jon Scheyer"),
            Some(false)
        );
    }

    #[test]
    fn change_flag_true_on_handoff() {
        // Bruce Pearl → Steven Pearl at Auburn is a change, not a same-surname no-op.
        let prev = season(&[("Auburn", "Bruce Pearl")]);
        assert_eq!(
            coaching_change_flag(Some(&prev), "Auburn", "Steven Pearl"),
            Some(true)
        );
    }

    #[test]
    fn rick_and_richard_pitino_are_distinct_identities() {
        // Name string is identity; the father/son pair must never conflate.
        let prev = season(&[("Iona", "Rick Pitino")]);
        assert_eq!(
            coaching_change_flag(Some(&prev), "Iona", "Richard Pitino"),
            Some(true)
        );
    }

    #[test]
    fn inverted_entry_dropped_only_when_value_is_the_team() {
        // "Matt McMahon" -> "LSU": mirror exists, value (LSU) is a team, key isn't → drop.
        assert!(is_inverted_entry(true, false, true));
        // "LSU" -> "Matt McMahon": key (LSU) is a team → keep (the canonical half).
        assert!(!is_inverted_entry(true, true, false));
        // Legit unmatched team (2021 "Brown"): no mirror back → keep.
        assert!(!is_inverted_entry(false, false, false));
    }
}
