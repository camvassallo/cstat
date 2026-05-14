//! Ingest pipeline for 247Sports composite recruit rankings.
//!
//! Sister to [`crate::ingest::transfers`], with the same two entry points
//! (`ingest_live` for paginated scraping and `bootstrap_from_snapshot` for
//! offline replay) and a two-pass cstat join strategy:
//!
//! * **Pass 1** — [`resolve_team_joins`]: at ingest time, match each row's
//!   `committed_school` text against `teams.short_name` / `teams.name` via
//!   [`cstat_core::team_name_match::team_match_score`] and write
//!   `committed_team_id`.
//! * **Pass 2** — [`resolve_player_joins`]: post-arrival, when the recruit's
//!   freshman cstat-season (`year + 1`) has been ingested, match
//!   `(full_name, committed_team_natstat_id)` against the `players` table.
//!   Mostly a no-op until the recruit actually shows up in box scores.
//!
//! `year` here is the recruiting class year (= spring of HS graduation, =
//! 247's URL `{year}-basketball` slug). A class-of-2026 recruit first
//! appears in cstat-season 2027 box scores.

use crate::tfs_recruits::{InstitutionGroup, Recruit247Client, RecruitError, RecruitRow};
use cstat_core::team_name_match::team_match_score;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_PAGES_PER_GROUP: u32 = 50;

#[derive(Debug, Error)]
pub enum RecruitIngestError {
    #[error("247 recruits API error: {0}")]
    Client(#[from] RecruitError),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("snapshot at {path} is missing a top-level `players` array")]
    InvalidSnapshot { path: String },
}

#[derive(Debug, Default, Clone)]
pub struct RecruitIngestReport {
    pub year: i32,
    pub total_pages: u32,
    pub upserts: u64,
    pub by_group: BTreeMap<String, u64>,
}

/// Snapshot wrapper persisted to `data/recruits/{year}_raw.json` for offline
/// replay. Mirrors the transfers snapshot shape (top-level `players` array).
#[derive(Debug, Serialize, Deserialize)]
pub struct RecruitSnapshot {
    pub year: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    pub players: Vec<RecruitRow>,
}

/// Ingest one class year of recruits from the live 247 endpoint.
///
/// Paginates per group until the parser returns an empty page (247 doesn't
/// publish a page count — empty fragment past the last data page is the
/// only stop signal). Defensive cap at `MAX_PAGES_PER_GROUP` prevents
/// runaway if 247's empty-page convention ever changes.
///
/// If `dump_snapshot` is set, write the combined fetch to disk before
/// upserting — useful for bootstrap-data capture without a second network
/// round-trip.
pub async fn ingest_live(
    client: &Recruit247Client,
    pool: &PgPool,
    year: i32,
    groups: &[InstitutionGroup],
    dump_snapshot: Option<&Path>,
) -> Result<RecruitIngestReport, RecruitIngestError> {
    let mut all_rows: Vec<(InstitutionGroup, RecruitRow)> = Vec::new();
    let mut total_pages = 0u32;
    let mut by_group: BTreeMap<String, u64> = BTreeMap::new();

    for &group in groups {
        let mut group_rows = 0u64;
        for page in 1..=MAX_PAGES_PER_GROUP {
            let p = client.fetch_page(year, group, page).await?;
            if p.is_last_page {
                info!(year, ?group, page, "reached empty page — stopping");
                break;
            }
            total_pages += 1;
            group_rows += p.players.len() as u64;
            for row in p.players {
                all_rows.push((group, row));
            }
        }
        by_group.insert(group.as_db_value().to_string(), group_rows);
        info!(year, ?group, rows = group_rows, "group ingest complete");
    }

    if let Some(path) = dump_snapshot {
        let snapshot = RecruitSnapshot {
            year,
            fetched_at: Some(chrono::Utc::now().to_rfc3339()),
            groups: groups.iter().map(|g| g.as_db_value().to_string()).collect(),
            players: all_rows.iter().map(|(_, r)| r.clone()).collect(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&snapshot)?)?;
        info!(path = %path.display(), rows = snapshot.players.len(), "snapshot written");
    }

    for (group, row) in &all_rows {
        upsert_player(row, pool, year, *group).await?;
    }
    let upserts = all_rows.len() as u64;

    info!(year, upserts, total_pages, "live recruits ingest complete");
    Ok(RecruitIngestReport {
        year,
        total_pages,
        upserts,
        by_group,
    })
}

/// Load a previously-captured snapshot and upsert every row.
///
/// Snapshot shape (single-group; produced by `ingest_live --dump-snapshot`):
/// ```json
/// {
///   "year": 2026,
///   "fetched_at": "2026-05-11T20:00:00Z",
///   "groups": ["highschool"],
///   "players": [ { "recruit_key": ..., ... }, ... ]
/// }
/// ```
///
/// `institution_group` is recovered from `snapshot.groups[0]`. `RecruitRow`
/// itself doesn't carry the group (it's a per-fetch concern, not a per-row
/// concern), so multi-group snapshots are ambiguous — we warn and tag every
/// row with `groups[0]`. The CLI defaults to a single group, so this is
/// usually a non-issue.
pub async fn bootstrap_from_snapshot(
    pool: &PgPool,
    year: i32,
    path: &Path,
) -> Result<RecruitIngestReport, RecruitIngestError> {
    let raw = std::fs::read_to_string(path)?;
    let snapshot: RecruitSnapshot =
        serde_json::from_str(&raw).map_err(|_| RecruitIngestError::InvalidSnapshot {
            path: path.display().to_string(),
        })?;

    if snapshot.year != year {
        warn!(
            cli_year = year,
            snapshot_year = snapshot.year,
            "snapshot's year metadata disagrees with --year; proceeding with --year"
        );
    }
    if snapshot.groups.len() > 1 {
        warn!(
            groups = ?snapshot.groups,
            "snapshot covers multiple institution_groups; all rows will be tagged as the first one"
        );
    }

    let group = snapshot
        .groups
        .first()
        .and_then(|g| InstitutionGroup::parse(g))
        .unwrap_or(InstitutionGroup::HighSchool);
    info!(
        year,
        count = snapshot.players.len(),
        path = %path.display(),
        ?group,
        "bootstrapping recruits from snapshot"
    );

    for row in &snapshot.players {
        upsert_player(row, pool, year, group).await?;
    }
    let upserts = snapshot.players.len() as u64;

    let mut by_group = BTreeMap::new();
    by_group.insert(group.as_db_value().to_string(), upserts);
    Ok(RecruitIngestReport {
        year,
        // 0 = "didn't paginate" (bootstrap loads from a single file).
        total_pages: 0,
        upserts,
        by_group,
    })
}

/// Insert or update one `recruits` row from a parsed `RecruitRow`. The parser
/// filters out malformed rows (missing `recruit_key`) before they reach here,
/// so this only fails on a DB error.
pub async fn upsert_player(
    row: &RecruitRow,
    pool: &PgPool,
    year: i32,
    group: InstitutionGroup,
) -> Result<(), RecruitIngestError> {
    // Whole-row JSON envelope for `raw_player`. Preserves the parsed view +
    // the original `<li>` HTML for forensics. JSONB lets the route handler
    // probe fields the schema doesn't model without a re-scrape.
    let raw_player = serde_json::to_value(row)?;

    sqlx::query(
        r#"
        INSERT INTO recruits (
            year, recruit_key, institution_group,
            first_name, last_name,
            position, height, weight,
            city, state, high_school,
            composite_rank, composite_rating, star_rating,
            previous_rank, position_rank, state_rank,
            committed_school, committed_school_slug, commit_status,
            profile_url, photo_url,
            raw_player
        ) VALUES (
            $1, $2, $3,
            $4, $5,
            $6, $7, $8,
            $9, $10, $11,
            $12, $13, $14,
            $15, $16, $17,
            $18, $19, $20,
            $21, $22,
            $23
        )
        ON CONFLICT (year, recruit_key) DO UPDATE SET
            institution_group = EXCLUDED.institution_group,
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            position = EXCLUDED.position,
            height = EXCLUDED.height,
            weight = EXCLUDED.weight,
            city = EXCLUDED.city,
            state = EXCLUDED.state,
            high_school = EXCLUDED.high_school,
            composite_rank = EXCLUDED.composite_rank,
            composite_rating = EXCLUDED.composite_rating,
            star_rating = EXCLUDED.star_rating,
            previous_rank = EXCLUDED.previous_rank,
            position_rank = EXCLUDED.position_rank,
            state_rank = EXCLUDED.state_rank,
            committed_school = EXCLUDED.committed_school,
            committed_school_slug = EXCLUDED.committed_school_slug,
            commit_status = EXCLUDED.commit_status,
            profile_url = EXCLUDED.profile_url,
            photo_url = EXCLUDED.photo_url,
            raw_player = EXCLUDED.raw_player,
            fetched_at = NOW()
        "#,
    )
    .bind(year)
    .bind(row.recruit_key)
    .bind(group.as_db_value())
    .bind(&row.first_name)
    .bind(&row.last_name)
    .bind(&row.position)
    .bind(&row.height)
    .bind(row.weight)
    .bind(&row.city)
    .bind(&row.state)
    .bind(&row.high_school)
    .bind(row.composite_rank)
    .bind(row.composite_rating)
    .bind(row.star_rating)
    .bind(row.previous_rank)
    .bind(row.position_rank)
    .bind(row.state_rank)
    .bind(&row.committed_school)
    .bind(&row.committed_school_slug)
    .bind(&row.commit_status)
    .bind(&row.profile_url)
    .bind(&row.photo_url)
    .bind(&raw_player)
    .execute(pool)
    .await?;
    Ok(())
}

/// Pass 1: resolve `committed_school` text → `teams.id`.
///
/// Matching is done in Rust (not SQL) so we can reuse the same
/// [`team_match_score`] scoring the transfers route handler uses — exact
/// short-name match beats alias match beats prefix fallback. Teams are
/// pulled from the most recent ingested season ≤ `year + 1` (the cstat
/// season the recruit will first play in); `teams.id` is season-scoped,
/// but downstream consumers can re-resolve via `teams.natstat_id` if a
/// later season's row exists.
pub async fn resolve_team_joins(pool: &PgPool, year: i32) -> Result<u64, RecruitIngestError> {
    #[derive(sqlx::FromRow)]
    struct RecruitNeed {
        id: Uuid,
        committed_school: String,
    }
    #[derive(sqlx::FromRow)]
    struct CandidateTeam {
        team_id: Uuid,
        short_name: Option<String>,
        full_name: String,
    }

    let needs: Vec<RecruitNeed> = sqlx::query_as(
        r#"
        SELECT id, committed_school
        FROM recruits
        WHERE year = $1
          AND committed_school IS NOT NULL
          AND committed_team_id IS NULL
        "#,
    )
    .bind(year)
    .fetch_all(pool)
    .await?;

    if needs.is_empty() {
        info!(year, "no recruits need team resolution");
        return Ok(0);
    }

    // Pick the most recent season we have teams for, capped at year+1 (the
    // recruit's freshman cstat-season). Falls back to the prior season if
    // year+1 hasn't been ingested yet.
    let target_season: Option<i32> =
        sqlx::query_scalar("SELECT MAX(season) FROM teams WHERE season <= $1")
            .bind(year + 1)
            .fetch_one(pool)
            .await?;

    let Some(target_season) = target_season else {
        warn!(
            year,
            "no teams in DB at any season ≤ year+1 — skipping team resolution"
        );
        return Ok(0);
    };

    let candidates: Vec<CandidateTeam> = sqlx::query_as(
        r#"
        SELECT id AS team_id, short_name, name AS full_name
        FROM teams
        WHERE season = $1
        "#,
    )
    .bind(target_season)
    .fetch_all(pool)
    .await?;

    info!(
        year,
        target_season,
        needs = needs.len(),
        candidates = candidates.len(),
        "resolving recruit team joins"
    );

    let mut recruit_ids: Vec<Uuid> = Vec::new();
    let mut team_ids: Vec<Uuid> = Vec::new();
    let mut team_score_miss = 0u64;

    for need in &needs {
        let scored = candidates
            .iter()
            .filter_map(|c| {
                team_match_score(
                    c.short_name.as_deref(),
                    &c.full_name,
                    &need.committed_school,
                )
                .map(|s| (s, c))
            })
            .min_by_key(|(s, _)| *s);

        if let Some((_, c)) = scored {
            recruit_ids.push(need.id);
            team_ids.push(c.team_id);
        } else {
            team_score_miss += 1;
        }
    }

    let result = sqlx::query(
        r#"
        UPDATE recruits r
        SET committed_team_id = m.team_id
        FROM UNNEST($1::uuid[], $2::uuid[]) AS m(recruit_id, team_id)
        WHERE r.id = m.recruit_id
          AND r.committed_team_id IS DISTINCT FROM m.team_id
        "#,
    )
    .bind(&recruit_ids)
    .bind(&team_ids)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    info!(
        year,
        target_season,
        matched = recruit_ids.len(),
        updated = n,
        team_score_miss,
        "committed_team_id resolution complete"
    );
    Ok(n)
}

/// Character-length of the common prefix of two strings, comparing
/// char-by-char (UTF-8 safe). Used by the Tier 2 nickname match to gate
/// last-name + same-initial pairs on a minimum first-name overlap:
/// "Cam"/"Cameron" share 3 chars (accept), "Jacob"/"Jayden" share only
/// 2 (reject). Threshold of 3 was chosen empirically against the
/// class-of-2024/2025 unresolved residue — it catches every real
/// nickname pair we observed (Ben/Benjamin, Cam/Cameron, Cash/Casmir,
/// Chris/Christopher, Nate/Nathaniel, Nic/Nicolus, Timo/Timotej) and
/// rejects the one observed false positive (Jacob/Jayden Ross).
fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn first_token(name: &str) -> Option<&str> {
    name.split_whitespace().next()
}

fn last_token(name: &str) -> Option<&str> {
    name.split_whitespace().next_back()
}

/// Pass 2: resolve `cstat_player_id` for recruits whose freshman cstat-season
/// (`year + 1`) has been ingested.
///
/// Cheap to run on every ingest — when the freshman season doesn't exist yet,
/// the candidate query returns zero rows and we no-op. Once box-score ingest
/// has populated `players` for season `year + 1`, this matches by
/// `(lower(name), committed_team_natstat_id, season)` and fills the FK.
pub async fn resolve_player_joins(pool: &PgPool, year: i32) -> Result<u64, RecruitIngestError> {
    let target_season = year + 1;

    #[derive(sqlx::FromRow)]
    struct RecruitNeed {
        id: Uuid,
        full_name: String,
        committed_team_natstat_id: String,
    }
    #[derive(sqlx::FromRow)]
    struct PlayerCand {
        player_id: Uuid,
        name: String,
        team_natstat_id: String,
    }

    let needs: Vec<RecruitNeed> = sqlx::query_as(
        r#"
        SELECT r.id, r.full_name, t.natstat_id AS committed_team_natstat_id
        FROM recruits r
        JOIN teams t ON t.id = r.committed_team_id
        WHERE r.year = $1
          AND r.committed_team_id IS NOT NULL
          AND r.cstat_player_id IS NULL
        "#,
    )
    .bind(year)
    .fetch_all(pool)
    .await?;

    if needs.is_empty() {
        info!(
            year,
            "no recruits ready for player resolution (none committed or already resolved)"
        );
        return Ok(0);
    }

    let candidates: Vec<PlayerCand> = sqlx::query_as(
        r#"
        SELECT p.id AS player_id, p.name, t.natstat_id AS team_natstat_id
        FROM players p
        JOIN teams t ON t.id = p.team_id AND t.season = p.season
        WHERE p.season = $1
        "#,
    )
    .bind(target_season)
    .fetch_all(pool)
    .await?;

    if candidates.is_empty() {
        info!(
            year,
            target_season,
            "no players in cstat-season {target_season} yet — Pass 2 is a no-op until box-score ingest catches up"
        );
        return Ok(0);
    }

    // ── Tier 1: exact normalized match within team ─────────────────
    //
    // Normalization strips punctuation (so 247's "V.J. Edgecombe"
    // matches cstat's "VJ Edgecombe") and generational suffixes (so
    // "Mikel Brown Jr." matches cstat's "Mikel Brown" / "Mikel Brown
    // jr" — cstat ingest is inconsistent about how it stores the
    // suffix). Same function the transfers route + roster_projection
    // use; consolidated as `cstat_core::roster_projection::normalize_player_name`.
    use cstat_core::roster_projection::normalize_player_name;
    let mut by_key: HashMap<(String, String), Uuid> = HashMap::new();
    for c in &candidates {
        by_key.insert(
            (normalize_player_name(&c.name), c.team_natstat_id.clone()),
            c.player_id,
        );
    }

    let mut recruit_ids: Vec<Uuid> = Vec::new();
    let mut player_ids: Vec<Uuid> = Vec::new();
    let mut claimed_pids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut unresolved_needs: Vec<&RecruitNeed> = Vec::new();
    for need in &needs {
        let key = (
            normalize_player_name(&need.full_name),
            need.committed_team_natstat_id.clone(),
        );
        if let Some(&pid) = by_key.get(&key) {
            recruit_ids.push(need.id);
            player_ids.push(pid);
            claimed_pids.insert(pid);
        } else {
            unresolved_needs.push(need);
        }
    }
    let tier1_matched = recruit_ids.len();

    // ── Tier 2: nickname match within team ──────────────────────────
    //
    // For each unmatched recruit, look at unclaimed cstat candidates on
    // the same team. Match if their normalized last names match AND
    // their normalized first names share a prefix of length ≥3. This
    // catches the long tail (Cam ↔ Cameron, Chris ↔ Christopher, Nic ↔
    // Nicolus, Nate ↔ Nathaniel, Cash ↔ Casmir, Ben ↔ Benjamin, …)
    // without falsely binding Jacob ↔ Jayden (share only "Ja", length 2).
    //
    // Misses Jake ↔ Jacob and similar short-form ↔ long-form pairs that
    // diverge before char 3 — fixing those would need a hardcoded
    // nickname table, deferred. Maintenance cost > marginal coverage.
    //
    // Index unclaimed candidates by (team, last_name) for O(1) per-need.
    // Last name = last whitespace-separated token of the normalized name.
    let mut by_team_last: HashMap<(String, String), Vec<(Uuid, String)>> = HashMap::new();
    for c in &candidates {
        if claimed_pids.contains(&c.player_id) {
            continue;
        }
        let norm = normalize_player_name(&c.name);
        let (Some(ln), Some(fn_)) = (last_token(&norm), first_token(&norm)) else {
            continue;
        };
        by_team_last
            .entry((ln.to_string(), c.team_natstat_id.clone()))
            .or_default()
            .push((c.player_id, fn_.to_string()));
    }

    let mut tier2_matched = 0_usize;
    let mut tier2_ambiguous = 0_usize;
    for need in unresolved_needs {
        let norm = normalize_player_name(&need.full_name);
        let (Some(ln), Some(fn_recruit)) = (last_token(&norm), first_token(&norm)) else {
            continue;
        };
        let bucket_key = (ln.to_string(), need.committed_team_natstat_id.clone());
        let Some(bucket) = by_team_last.get(&bucket_key) else {
            continue;
        };
        // Surviving candidates: same team, same last name, same
        // first-initial, shared prefix ≥3 between first names. We
        // accept a match only when *exactly one* survives — multiple
        // matches mean we can't safely pick one, so we leave it
        // unresolved and bump the ambiguous counter.
        let first_initial_recruit = fn_recruit.chars().next();
        // Filter out pids already claimed in Tier 1 *or* earlier Tier 2
        // iterations — without this, two unresolved recruits on the
        // same team with same last name + prefix-matching first names
        // would both bind to a single candidate (e.g. "Cam Davis" and
        // "Cameron Davis" both → cstat's "Cameron Davis"), violating
        // the one-recruit-per-cstat-player invariant.
        let survivors: Vec<&(Uuid, String)> = bucket
            .iter()
            .filter(|(pid, fn_cand)| {
                !claimed_pids.contains(pid)
                    && fn_cand.chars().next() == first_initial_recruit
                    && shared_prefix_len(fn_cand, fn_recruit) >= 3
            })
            .collect();
        match survivors.as_slice() {
            [(pid, _)] => {
                recruit_ids.push(need.id);
                player_ids.push(*pid);
                claimed_pids.insert(*pid);
                tier2_matched += 1;
            }
            [] => {}
            _ => tier2_ambiguous += 1,
        }
    }

    let result = sqlx::query(
        r#"
        UPDATE recruits r
        SET cstat_player_id = m.player_id
        FROM UNNEST($1::uuid[], $2::uuid[]) AS m(recruit_id, player_id)
        WHERE r.id = m.recruit_id
          AND r.cstat_player_id IS DISTINCT FROM m.player_id
        "#,
    )
    .bind(&recruit_ids)
    .bind(&player_ids)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    info!(
        year,
        target_season,
        needs = needs.len(),
        candidates = candidates.len(),
        tier1_exact = tier1_matched,
        tier2_nickname = tier2_matched,
        tier2_ambiguous,
        matched = recruit_ids.len(),
        updated = n,
        "cstat_player_id resolution complete"
    );
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_prefix_len_basic() {
        // Real nickname pairs that should clear the ≥3 gate.
        assert!(shared_prefix_len("cam", "cameron") >= 3);
        assert!(shared_prefix_len("ben", "benjamin") >= 3);
        assert!(shared_prefix_len("nic", "nicolus") >= 3);
        assert!(shared_prefix_len("chris", "christopher") >= 3);
        assert!(shared_prefix_len("cash", "casmir") >= 3);
        // Real false-positive that should NOT clear the gate (Jacob Ross
        // vs Jayden Ross on the same team — different people).
        assert!(shared_prefix_len("jacob", "jayden") < 3);
        // No overlap at all.
        assert_eq!(shared_prefix_len("zz", "zachiah"), 1);
        // Exact match.
        assert_eq!(shared_prefix_len("fawaz", "fawaz"), 5);
        // Empty strings.
        assert_eq!(shared_prefix_len("", "foo"), 0);
    }

    #[test]
    fn first_and_last_token_split_on_whitespace() {
        assert_eq!(first_token("ben winker"), Some("ben"));
        assert_eq!(last_token("ben winker"), Some("winker"));
        assert_eq!(first_token("vj edgecombe"), Some("vj"));
        assert_eq!(last_token("vj edgecombe"), Some("edgecombe"));
        // Single-token name (rare but possible).
        assert_eq!(first_token("madonna"), Some("madonna"));
        assert_eq!(last_token("madonna"), Some("madonna"));
        // Empty / whitespace-only.
        assert_eq!(first_token(""), None);
        assert_eq!(last_token("   "), None);
    }

    #[test]
    fn snapshot_roundtrip_preserves_rows() {
        let rows = vec![RecruitRow {
            recruit_key: 12345,
            first_name: Some("Test".into()),
            last_name: Some("Player".into()),
            composite_rank: Some(7),
            previous_rank: None,
            composite_rating: Some(0.9999),
            star_rating: Some(5),
            position_rank: Some(1),
            state_rank: Some(1),
            position: Some("PG".into()),
            height: Some("6-3".into()),
            weight: Some(180),
            high_school: Some("Some HS".into()),
            city: Some("Anywhere".into()),
            state: Some("CA".into()),
            committed_school: Some("Duke".into()),
            committed_school_slug: Some("duke".into()),
            commit_status: Some("Signed".into()),
            profile_url: Some("/player/test-player-12345/".into()),
            photo_url: None,
            raw_html: String::new(),
        }];
        let snap = RecruitSnapshot {
            year: 2026,
            fetched_at: Some("2026-05-11T20:00:00Z".into()),
            groups: vec!["highschool".into()],
            players: rows.clone(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: RecruitSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.year, 2026);
        assert_eq!(back.players.len(), 1);
        assert_eq!(back.players[0].recruit_key, 12345);
        assert_eq!(back.players[0].committed_school.as_deref(), Some("Duke"));
    }
}
