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

/// Page cap for the national commits feed. It carries every commit at every
/// level (unranked/international/prep/G-League), so it runs larger than the
/// ranked composite — historically ~12–19 pages/class — but the cap gives
/// generous headroom and guards against a runaway if 247's empty-page stop
/// signal ever changes. Hitting it is warned, not silently truncated.
const MAX_COMMIT_PAGES: u32 = 60;

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

    #[error(
        "247 commits feed for {year} returned zero recruits on page 1 — the endpoint or its `ri-page__` row markup likely changed; refusing to report a successful empty ingest"
    )]
    EmptyCommitsFeed { year: i32 },
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

/// Is 247's record for recruiting class `year` still a live statement of where
/// these players are going?
///
/// **This is the most important rule in this module.** A 247 recruit row is a
/// living document, not an archival record of what happened in a signing
/// period. Once a player is on campus 247 keeps editing his recruit row, and
/// two columns drift in ways that are actively wrong for our purposes:
///
/// * `committedInstitution` becomes the school he **transferred to**. Sampled
///   against the class-of-2014..2025 rows already in the table, 1,592 rows had
///   a different school in the JSON than the HTML scrape recorded — and on the
///   ones we could check against box scores, the *old* value matched the team
///   he actually played his freshman season for (7 of 8), not the new one.
///   Taking the new value silently re-points a recruit at a team he reached
///   two years later, which is exactly backwards for a freshman-arrival model.
/// * `compositeNationalRank` disagrees with the scrape on 3,541 historical
///   rows, and not randomly: the drift is ~0 at the top of a class and grows
///   monotonically with depth (+45.7 by rank 500 for 2019). The scraped rank is
///   internally consistent with `composite_rating` (mean |rank − rating-order|
///   = 0.58); the JSON's is not (48.0). Whatever pool 247 now ranks against, it
///   is not the one the stored ratings came from, and mixing the two produces a
///   column that agrees with neither.
///
/// So: for a **live** class the feed is authoritative and overwrites, which is
/// the entire point of refreshing it nightly — commits, decommits and re-ranks
/// all have to land. For a **settled** class the feed may add rows and fill
/// NULLs but must never overwrite, because the stored value is the contemporary
/// one and is what the served freshman and trajectory models were trained on.
///
/// The boundary is `current_natstat_season()`, which is also where it belongs:
/// class C first plays in season C+1, so a class stops being live at exactly
/// the November rollover that puts its players into box scores.
///
/// This is the same shape as the rule `scripts/sync_to_prod.sh --columns`
/// applies — merge past seasons, let the live side own the current one.
pub fn class_is_live(year: i32) -> bool {
    year >= crate::current_natstat_season()
}

/// The recruiting classes the nightly refreshes for cstat season `season`.
///
/// A recruiting class `C` first plays in cstat season `C + 1`, so for season
/// `S` the class *arriving* is `S` itself and `S + 1` is the one actively being
/// recruited. Both are live: the season number rolls over in November, in the
/// middle of the early signing period, and the next class opens in the spring —
/// a single-year guess is wrong on one side of each boundary.
///
/// Classes older than `S` are settled (they signed a year ago and are on
/// rosters) and are not re-fetched.
pub fn nightly_ingest_class_years(season: i32) -> [i32; 2] {
    [season, season + 1]
}

/// The recruiting classes the nightly runs the resolution passes over.
///
/// One year wider than [`nightly_ingest_class_years`] on the **near** side, and
/// that year is the whole point. [`resolve_player_joins`] matches class `C`
/// against season `C + 1` box scores, so the class it can finally resolve is
/// `season - 1` — the freshmen playing right now. That class is no longer
/// ingested, so resolving only the ingested years would mean the nightly
/// forever ingests classes it cannot yet resolve and never revisits the one it
/// can, leaving `cstat_player_id` NULL for every arriving freshman.
///
/// Both passes early-return when nothing is outstanding, so the extra year
/// costs a query, not a fetch.
pub fn nightly_resolve_class_years(season: i32) -> [i32; 3] {
    [season - 1, season, season + 1]
}

/// Ingest one class year of recruits from the live 247 endpoint.
///
/// Paginates per group until the feed says it is done. The two transports
/// signal that differently — the JSON feed publishes `pagination.pageCount`,
/// the HTML scrape serves an empty fragment past the last data page — and
/// [`RecruitPage::is_last_page`] normalizes both. Defensive cap at
/// `MAX_PAGES_PER_GROUP` prevents runaway if either convention changes.
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
            // Consume before testing the stop signal: on the JSON transport the
            // final page carries real rows *and* is flagged last (pageCount is
            // known up front), so breaking first would silently drop it.
            let last = p.is_last_page;
            // Counts pages that returned data. The HTML scrape stops by walking
            // one page PAST the end, and counting that empty sentinel would
            // report one more page than was actually read.
            if !p.players.is_empty() {
                total_pages += 1;
            }
            group_rows += p.players.len() as u64;
            for row in p.players {
                all_rows.push((group, row));
            }
            if last {
                info!(year, ?group, page, "reached last page — stopping");
                break;
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
///
/// **Five columns COALESCE instead of overwriting** — `height`, `weight`,
/// `high_school`, `previous_rank` and `committed_school_slug`. The JSON
/// rankings feed does not carry them (the first three come from the commits
/// feed, the last two only from the HTML scrape), so a plain
/// `EXCLUDED`-overwrite would blank them on every JSON pass. That is not
/// cosmetic: `height` and `previous_rank` are inputs to the served freshman and
/// trajectory projection models, and `previous_rank` has no JSON source at all
/// on any route probed — losing it would flip `recruit_rank_movement` to 0 for
/// the whole table. Every other column overwrites, so a re-ingest still tracks
/// 247'"'"'s live ranking churn.
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
            -- $24 = `class_is_live(year)`. Live: the feed is authoritative and
            -- overwrites (a NULL clears — that is how a decommit lands).
            -- Settled: fill a NULL, never overwrite. See `class_is_live`.
            first_name    = CASE WHEN $24 THEN EXCLUDED.first_name    ELSE COALESCE(recruits.first_name, EXCLUDED.first_name) END,
            last_name     = CASE WHEN $24 THEN EXCLUDED.last_name     ELSE COALESCE(recruits.last_name, EXCLUDED.last_name) END,
            position      = CASE WHEN $24 THEN EXCLUDED.position      ELSE COALESCE(recruits.position, EXCLUDED.position) END,
            city          = CASE WHEN $24 THEN EXCLUDED.city          ELSE COALESCE(recruits.city, EXCLUDED.city) END,
            state         = CASE WHEN $24 THEN EXCLUDED.state         ELSE COALESCE(recruits.state, EXCLUDED.state) END,
            composite_rank    = CASE WHEN $24 THEN EXCLUDED.composite_rank    ELSE COALESCE(recruits.composite_rank, EXCLUDED.composite_rank) END,
            composite_rating  = CASE WHEN $24 THEN EXCLUDED.composite_rating  ELSE COALESCE(recruits.composite_rating, EXCLUDED.composite_rating) END,
            star_rating       = CASE WHEN $24 THEN EXCLUDED.star_rating       ELSE COALESCE(recruits.star_rating, EXCLUDED.star_rating) END,
            position_rank     = CASE WHEN $24 THEN EXCLUDED.position_rank     ELSE COALESCE(recruits.position_rank, EXCLUDED.position_rank) END,
            state_rank        = CASE WHEN $24 THEN EXCLUDED.state_rank        ELSE COALESCE(recruits.state_rank, EXCLUDED.state_rank) END,
            committed_school  = CASE WHEN $24 THEN EXCLUDED.committed_school  ELSE COALESCE(recruits.committed_school, EXCLUDED.committed_school) END,
            commit_status     = CASE WHEN $24 THEN EXCLUDED.commit_status     ELSE COALESCE(recruits.commit_status, EXCLUDED.commit_status) END,
            -- Never overwritten on either side: this feed does not carry them,
            -- so `EXCLUDED` is always NULL here and a plain assignment would
            -- blank what the commits feed or the HTML scrape supplied. `height`
            -- and `previous_rank` are served projection-model inputs.
            height        = COALESCE(EXCLUDED.height, recruits.height),
            weight        = COALESCE(EXCLUDED.weight, recruits.weight),
            high_school   = COALESCE(EXCLUDED.high_school, recruits.high_school),
            previous_rank = COALESCE(EXCLUDED.previous_rank, recruits.previous_rank),
            committed_school_slug = COALESCE(EXCLUDED.committed_school_slug, recruits.committed_school_slug),
            profile_url = COALESCE(EXCLUDED.profile_url, recruits.profile_url),
            photo_url   = COALESCE(EXCLUDED.photo_url, recruits.photo_url),
            -- Frozen on the same rule as the columns it explains. `raw_player`
            -- is the forensic copy of the record the parsed values came from,
            -- so overwriting it on a settled class would destroy the only
            -- remaining copy of the contemporary 247 record — the very thing
            -- the freeze exists to keep — and leave an audit trail that no
            -- longer reproduces its own row. NOT NULL, so no COALESCE needed.
            raw_player  = CASE WHEN $24 THEN EXCLUDED.raw_player ELSE recruits.raw_player END,
            fetched_at  = NOW()
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
    .bind(class_is_live(year))
    .execute(pool)
    .await?;
    Ok(())
}

/// Ingest one class year of commits from the public national commits feed
/// (`/season/{year}-basketball/commits/`).
///
/// This is the gap-filler for the composite-rankings ingest ([`ingest_live`]),
/// which only sees ranked players. The commits feed lists every commit —
/// unranked, international, prep, G-League — each carrying its committed school
/// (issue #175). The feed is public, so `client` should be built with
/// [`Recruit247Client::public`] (no subscriber cookie).
///
/// Rows land tagged `institution_group = 'commits'` and are upserted via
/// [`upsert_commit`], which never clobbers a composite-owned row (see there).
/// Run the resolution passes ([`resolve_team_joins`] / [`resolve_player_joins`])
/// afterward exactly as the composite path does.
pub async fn ingest_commits(
    client: &Recruit247Client,
    pool: &PgPool,
    year: i32,
) -> Result<RecruitIngestReport, RecruitIngestError> {
    // First-write-wins dedup on `recruit_key`. 247's paginated feeds can
    // overlap a row across adjacent pages with a staler snapshot on the later
    // page (documented on the transfer-portal path); keeping the first
    // occurrence avoids a stale duplicate clobbering a fresher one.
    let mut rows: Vec<RecruitRow> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut total_pages = 0u32;
    let mut hit_cap = true;
    for page in 1..=MAX_COMMIT_PAGES {
        let p = client.fetch_commits_page(year, page).await?;
        // Consume before testing the stop signal — see the note in `ingest_live`.
        let last = p.is_last_page;
        if !p.players.is_empty() {
            total_pages += 1;
        }
        for row in p.players {
            if seen.insert(row.recruit_key) {
                rows.push(row);
            }
        }
        if last {
            info!(year, page, "reached last commits page — stopping");
            hit_cap = false;
            break;
        }
    }
    if hit_cap {
        warn!(
            year,
            max_pages = MAX_COMMIT_PAGES,
            rows = rows.len(),
            "commits feed hit the page cap before the empty sentinel — commits beyond page {MAX_COMMIT_PAGES} were NOT ingested; raise MAX_COMMIT_PAGES"
        );
    }

    // A zero-row page 1 is never legitimate for this feed (every class has
    // hundreds of commits). Treat it as a structural break — a changed
    // `ri-page__` prefix or endpoint shape — rather than reporting a
    // successful empty ingest that silently stops populating any commits.
    if rows.is_empty() {
        return Err(RecruitIngestError::EmptyCommitsFeed { year });
    }

    for row in &rows {
        upsert_commit(row, pool, year).await?;
    }
    let upserts = rows.len() as u64;

    let mut by_group = BTreeMap::new();
    by_group.insert("commits".to_string(), upserts);
    info!(year, upserts, total_pages, "commits-feed ingest complete");
    Ok(RecruitIngestReport {
        year,
        total_pages,
        upserts,
        by_group,
    })
}

/// Upsert one row from the national commits feed.
///
/// Provenance-scoped so the commits and composite passes converge without a
/// fight (issue #175):
///
/// * New rows insert with `institution_group = 'commits'`.
/// * `ON CONFLICT … DO UPDATE` is gated `WHERE recruits.institution_group =
///   'commits'` — so a conflict against a **composite-owned** row (any other
///   group) is a no-op and the richer composite data is preserved untouched.
///   Once the composite pass promotes a player, it owns the row for good.
///
/// The composite ranking columns (`composite_rank` / `composite_rating` /
/// `previous_rank` / `position_rank` / `state_rank`) are deliberately **not**
/// written: the commits feed's visible `.score` is 247's proprietary 0–100
/// rating, a different metric from the 0–1 composite, and its ranks read "NA"
/// for the unranked players that are the whole point here. `star_rating` (an
/// unambiguous 1–5 solid-star count) is persisted when present. (The JSON
/// transport does expose a real composite under `ranking.*`, parsed onto the
/// row for snapshot fidelity, but the rankings feed owns those columns and
/// covers every ranked player — so the division of labor is unchanged.)
///
/// One exception to the provenance gate, added with the JSON transport: the
/// physical columns `height` / `weight` / `high_school` are **gap-filled** onto
/// a composite-owned row when they are NULL there. The JSON rankings feed
/// carries none of the three and this feed is their only JSON source, so
/// without the backfill a ranked, committed recruit ingested rankings-first
/// would keep a NULL `height` forever — and `height` is an input to the served
/// freshman projection model. The fill never overwrites a non-NULL value, so
/// composite data stays authoritative where it exists.
pub async fn upsert_commit(
    row: &RecruitRow,
    pool: &PgPool,
    year: i32,
) -> Result<(), RecruitIngestError> {
    let raw_player = serde_json::to_value(row)?;
    // Parser reports 0 solid stars for unranked players; store NULL rather than
    // a misleading "0-star".
    let star_rating = row.star_rating.filter(|&s| s > 0);

    sqlx::query(
        r#"
        INSERT INTO recruits (
            year, recruit_key, institution_group,
            first_name, last_name,
            position, height, weight,
            city, state, high_school,
            star_rating,
            committed_school, committed_school_slug, commit_status,
            profile_url, photo_url,
            raw_player
        ) VALUES (
            $1, $2, 'commits',
            $3, $4,
            $5, $6, $7,
            $8, $9, $10,
            $11,
            $12, $13, $14,
            $15, $16,
            $17
        )
        ON CONFLICT (year, recruit_key) DO UPDATE SET
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            position = EXCLUDED.position,
            -- $18 = `class_is_live(year)`, same rule as `upsert_player`: the
            -- feed owns a class still being recruited, and may only fill NULLs
            -- on one already on campus. This feed carries `committedInstitution`
            -- too, so it drifts to the transfer destination in exactly the same
            -- way once a player moves.
            height           = CASE WHEN $18 THEN EXCLUDED.height           ELSE COALESCE(recruits.height, EXCLUDED.height) END,
            weight           = CASE WHEN $18 THEN EXCLUDED.weight           ELSE COALESCE(recruits.weight, EXCLUDED.weight) END,
            city             = CASE WHEN $18 THEN EXCLUDED.city             ELSE COALESCE(recruits.city, EXCLUDED.city) END,
            state            = CASE WHEN $18 THEN EXCLUDED.state            ELSE COALESCE(recruits.state, EXCLUDED.state) END,
            high_school      = CASE WHEN $18 THEN EXCLUDED.high_school      ELSE COALESCE(recruits.high_school, EXCLUDED.high_school) END,
            star_rating      = CASE WHEN $18 THEN EXCLUDED.star_rating      ELSE COALESCE(recruits.star_rating, EXCLUDED.star_rating) END,
            committed_school = CASE WHEN $18 THEN EXCLUDED.committed_school ELSE COALESCE(recruits.committed_school, EXCLUDED.committed_school) END,
            commit_status    = CASE WHEN $18 THEN EXCLUDED.commit_status    ELSE COALESCE(recruits.commit_status, EXCLUDED.commit_status) END,
            committed_school_slug = COALESCE(EXCLUDED.committed_school_slug, recruits.committed_school_slug),
            profile_url = COALESCE(EXCLUDED.profile_url, recruits.profile_url),
            photo_url   = COALESCE(EXCLUDED.photo_url, recruits.photo_url),
            -- Frozen with the columns it explains — see `upsert_player`.
            raw_player  = CASE WHEN $18 THEN EXCLUDED.raw_player ELSE recruits.raw_player END,
            fetched_at  = NOW()
        WHERE recruits.institution_group = 'commits'
        "#,
    )
    .bind(year)
    .bind(row.recruit_key)
    .bind(&row.first_name)
    .bind(&row.last_name)
    .bind(&row.position)
    .bind(&row.height)
    .bind(row.weight)
    .bind(&row.city)
    .bind(&row.state)
    .bind(&row.high_school)
    .bind(star_rating)
    .bind(&row.committed_school)
    .bind(&row.committed_school_slug)
    .bind(&row.commit_status)
    .bind(&row.profile_url)
    .bind(&row.photo_url)
    .bind(&raw_player)
    .bind(class_is_live(year))
    .execute(pool)
    .await?;

    // Gap-fill the physical columns onto a composite-owned row (see the doc
    // comment). No-op when the row is commits-owned — the upsert above already
    // wrote them — and never overwrites a value that is already there.
    if row.height.is_some() || row.weight.is_some() || row.high_school.is_some() {
        sqlx::query(
            r#"
            -- Fill-only by construction (COALESCE keeps a non-NULL), so it
            -- needs no liveness gate: it can never overwrite a stored value.
            UPDATE recruits SET
                height      = COALESCE(height, $3),
                weight      = COALESCE(weight, $4),
                high_school = COALESCE(high_school, $5)
            WHERE year = $1
              AND recruit_key = $2
              AND institution_group <> 'commits'
              AND (height IS NULL OR weight IS NULL OR high_school IS NULL)
            "#,
        )
        .bind(year)
        .bind(row.recruit_key)
        .bind(&row.height)
        .bind(row.weight)
        .bind(&row.high_school)
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Pass 1: resolve `committed_school` text → `teams.id`.
///
/// Re-runs against every committed recruit each ingest (not just unresolved
/// rows), so a recommit to a different school updates `committed_team_id`
/// instead of leaving it frozen at the first school (issue #200); the
/// `IS DISTINCT FROM` update writes only rows that actually changed.
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

    // Re-score EVERY committed recruit each run — not just unresolved ones —
    // so a decommit-and-recommit to a different school re-points
    // `committed_team_id` instead of freezing it at the first school forever
    // (issue #200). Mirrors the transfers path (`resolve_cstat_joins` re-scans
    // all rows and lets `IS DISTINCT FROM` below write only the changes); a
    // `committed_team_id IS NULL` gate here is what made recruits go stale on
    // recommit while transfers didn't. Bounded work: a few thousand recruits ×
    // ~360 candidate teams, scored in Rust.
    let needs: Vec<RecruitNeed> = sqlx::query_as(
        r#"
        SELECT id, committed_school
        FROM recruits
        WHERE year = $1
          AND committed_school IS NOT NULL
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

    // On an ACTUAL change (IS DISTINCT FROM), also null cstat_player_id: Pass 2
    // (`resolve_player_joins`) only resolves rows where cstat_player_id IS NULL,
    // so without this a recommit would leave the player FK pointing at the old
    // school's roster. Nulling forces Pass 2 to re-resolve against the new team.
    // Harmless on first resolution (NULL→team): cstat_player_id is already NULL.
    let result = sqlx::query(
        r#"
        UPDATE recruits r
        SET committed_team_id = m.team_id,
            cstat_player_id = NULL
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
            raw_source: String::new(),
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

#[cfg(test)]
mod class_year_window_tests {
    use super::*;

    /// The arriving class and the one being recruited — never a settled class.
    #[test]
    fn ingest_window_is_the_two_live_classes() {
        assert_eq!(nightly_ingest_class_years(2027), [2027, 2028]);
        assert_eq!(nightly_ingest_class_years(2026), [2026, 2027]);
    }

    /// Regression: the resolution window must reach back one year further than
    /// the ingest window, or the freshmen currently in box scores never get a
    /// `cstat_player_id`. In season 2027 that is class 2026 — a class the
    /// ingest window deliberately excludes.
    #[test]
    fn resolve_window_covers_the_class_now_in_box_scores() {
        let season = 2027;
        let resolve = nightly_resolve_class_years(season);
        let ingest = nightly_ingest_class_years(season);
        // `resolve_player_joins(C)` looks at season C + 1, so this is the class
        // whose freshmen are on the floor in `season`.
        let playing_now = season - 1;
        assert!(
            resolve.contains(&playing_now),
            "resolution window {resolve:?} must include class {playing_now}"
        );
        assert!(
            !ingest.contains(&playing_now),
            "sanity: the ingest window is not expected to cover class {playing_now}, \
             which is why the resolution window has to"
        );
    }

    /// The liveness boundary must line up with the ingest window: every class
    /// the nightly refreshes has to be one the feed is allowed to overwrite,
    /// or the nightly would fetch a class and then decline to apply it.
    #[test]
    fn every_ingested_class_is_live() {
        let season = crate::current_natstat_season();
        for y in nightly_ingest_class_years(season) {
            assert!(
                class_is_live(y),
                "class {y} is ingested nightly but treated as settled — its \
                 refresh would be silently dropped"
            );
        }
    }

    /// The converse: the extra class the resolution window reaches back for is
    /// settled by construction — its players are in box scores, which is
    /// exactly why 247 has started rewriting their recruit rows.
    #[test]
    fn the_class_now_in_box_scores_is_settled() {
        let season = crate::current_natstat_season();
        assert!(!class_is_live(season - 1));
        assert!(!class_is_live(season - 2));
    }

    /// Every ingested class is also resolved — a class fetched but never
    /// resolved would sit with NULL joins until someone noticed.
    #[test]
    fn resolve_window_is_a_superset_of_the_ingest_window() {
        for season in [2024, 2025, 2026, 2027, 2028] {
            let resolve = nightly_resolve_class_years(season);
            for y in nightly_ingest_class_years(season) {
                assert!(resolve.contains(&y), "class {y} ingested but not resolved");
            }
        }
    }
}
