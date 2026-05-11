//! Ingest pipeline for the 247Sports transfer portal.
//!
//! Two entry points:
//! - [`ingest_live`] — paginate through the 247 API and upsert each row.
//! - [`bootstrap_from_snapshot`] — load `data/transfers/{year}_raw.json` (a
//!   previously-captured full fetch) without hitting the network. Useful for
//!   reproducible local dev and for the initial production seed.
//!
//! Both paths funnel into [`upsert_player`], which maps one player JSON
//! object → one row in the `transfers` table.
//!
//! Post-ingest, [`resolve_cstat_joins`] fills `cstat_player_id` on rows whose
//! `(full_name, source_institution)` matches a cstat `players` row from the
//! prior college season.

use crate::tfs::{TfsClient, TfsError};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// Allowed values for `transfers.status`. Mirrors the CHECK constraint in
/// `migrations/019_transfers.sql`. If 247 introduces a new value, widen the
/// CHECK AND this array — keep them in sync.
const ALLOWED_STATUS: &[&str] = &["Entered", "Committed", "Withdrawn"];
const ALLOWED_INSTITUTION_STATUS: &[&str] = &["HS", "T"];
const ALLOWED_ELIGIBILITY_TYPE: &[&str] = &["Immediate", "Withdrawn", "PendingAppeal", "TBD"];

#[derive(Debug, Error)]
pub enum TransferIngestError {
    #[error("TFS API error: {0}")]
    Tfs(#[from] TfsError),

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
pub struct TransferIngestReport {
    pub year: i32,
    pub total_pages: u32,
    pub upserts: u64,
    pub last_updated: Option<String>,
}

/// Ingest one class year of transfers from the live 247 API.
///
/// `incremental = true` short-circuits the loop as soon as we hit a page where
/// every row's `lastUpdateDate` is older than our DB cursor. `false` walks
/// every page (the right choice for the initial seed and for full
/// re-validation).
pub async fn ingest_live(
    client: &TfsClient,
    pool: &PgPool,
    year: i32,
    incremental: bool,
) -> Result<TransferIngestReport, TransferIngestError> {
    let cursor: Option<DateTime<Utc>> = if incremental {
        last_update_cursor(pool, year).await?
    } else {
        None
    };

    let first = client.fetch_page(year, 1).await?;
    let total_pages = first.pagination.page_count.max(1);
    info!(
        year,
        total_pages,
        expected_count = first.pagination.count,
        cursor = ?cursor,
        "starting live transfers ingest"
    );

    let mut report = TransferIngestReport {
        year,
        total_pages,
        upserts: 0,
        last_updated: first.last_updated.clone(),
    };
    report.upserts += apply_page(&first.players, pool, year, cursor).await?;

    for page_num in 2..=total_pages {
        let page = client.fetch_page(year, page_num).await?;
        let applied = apply_page(&page.players, pool, year, cursor).await?;
        report.upserts += applied;

        // Incremental short-circuit: if this entire page predates our cursor,
        // no later page can be newer either (server orders by lastUpdated DESC).
        if incremental && applied == 0 && !page.players.is_empty() {
            info!(
                year,
                page = page_num,
                "incremental short-circuit — every row on this page predates cursor"
            );
            break;
        }
    }

    info!(year, upserts = report.upserts, "live ingest complete");
    Ok(report)
}

/// Load a previously-captured full-fetch snapshot and upsert every row.
///
/// Expected shape:
/// ```json
/// { "players": [ {...}, {...}, ... ], ... }
/// ```
/// — the file produced by the curl-loop documented in ROADMAP §5b
/// "Bootstrap data". Each `players[]` element is a flat player object (already
/// unwrapped from the API's `{"player": {...}}` envelope).
pub async fn bootstrap_from_snapshot(
    pool: &PgPool,
    year: i32,
    path: &Path,
) -> Result<TransferIngestReport, TransferIngestError> {
    let raw = std::fs::read_to_string(path)?;
    let body: Value = serde_json::from_str(&raw)?;
    let players = body
        .get("players")
        .and_then(|v| v.as_array())
        .ok_or_else(|| TransferIngestError::InvalidSnapshot {
            path: path.display().to_string(),
        })?;

    // Footgun guard: if the snapshot wrapper labels itself with a year and it
    // disagrees with the CLI --year, warn loudly. Don't hard-fail — the user
    // may have a reason (e.g. re-ingesting an old file with a fresh year tag).
    if let Some(snap_year) = body.get("year").and_then(|v| v.as_i64())
        && snap_year as i32 != year
    {
        warn!(
            cli_year = year,
            snapshot_year = snap_year,
            "snapshot's year metadata disagrees with --year; proceeding with --year"
        );
    }

    info!(
        year,
        count = players.len(),
        path = %path.display(),
        "bootstrapping transfers from snapshot"
    );

    let upserts = apply_page(players, pool, year, None).await?;
    let last_updated = body
        .get("last_updated")
        .or_else(|| body.get("lastUpdated"))
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(TransferIngestReport {
        year,
        total_pages: 1,
        upserts,
        last_updated,
    })
}

/// Apply a slice of player objects to the DB. Returns the number of rows
/// upserted (skipped rows — predating the incremental cursor, or malformed —
/// don't count).
async fn apply_page(
    players: &[Value],
    pool: &PgPool,
    year: i32,
    cursor: Option<DateTime<Utc>>,
) -> Result<u64, TransferIngestError> {
    let mut applied = 0u64;
    for player in players {
        if let Some(c) = cursor
            && let Some(last) = player
                .get("lastUpdateDate")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
            && last <= c
        {
            continue;
        }
        if upsert_player(player, pool, year).await? {
            applied += 1;
        }
    }
    Ok(applied)
}

/// Insert or update one transfers row from a single player JSON object.
///
/// Returns `Ok(true)` on a successful upsert, `Ok(false)` if the row was
/// skipped (e.g. missing the required `key` field). DB errors propagate; a
/// single malformed JSON row warns and skips so that one bad apple in 247's
/// data doesn't kill the whole 2,000+ row ingest.
pub async fn upsert_player(
    p: &Value,
    pool: &PgPool,
    year: i32,
) -> Result<bool, TransferIngestError> {
    let Some(tfs_key) = p.get("key").and_then(|v| v.as_i64()) else {
        warn!(
            year,
            first_name = ?p.get("firstName").and_then(|v| v.as_str()),
            last_name = ?p.get("lastName").and_then(|v| v.as_str()),
            "skipping player row: missing `key` field"
        );
        return Ok(false);
    };

    let first_name = string(p.get("firstName")).unwrap_or_default();
    let last_name = string(p.get("lastName")).unwrap_or_default();

    // `status` is NOT NULL with a CHECK constraint. If 247 sends an unknown
    // value (e.g. a new enum we haven't whitelisted yet), the INSERT would
    // fail and abort the whole ingest. Skip + warn instead so one stray value
    // doesn't kill 2000+ rows. Same pattern as the missing-`key` case.
    let status = match string(p.get("status")) {
        Some(s) if ALLOWED_STATUS.contains(&s.as_str()) => s,
        Some(other) => {
            warn!(
                year,
                tfs_key,
                status = %other,
                "skipping player row: unknown `status` (widen ALLOWED_STATUS + CHECK to accept)"
            );
            return Ok(false);
        }
        None => "Entered".to_string(),
    };
    // Optional enums — same risk on the CHECK side. Drop unknowns to NULL so
    // the row still lands; the raw value is preserved in `raw_player` for
    // forensics if we want to widen later.
    let institution_status = sanitize_enum(
        p.get("institutionStatus"),
        ALLOWED_INSTITUTION_STATUS,
        "institutionStatus",
        year,
        tfs_key,
    );
    let eligibility_type = sanitize_enum(
        p.get("eligibility").and_then(|e| e.get("type")),
        ALLOWED_ELIGIBILITY_TYPE,
        "eligibility.type",
        year,
        tfs_key,
    );

    // `last_update_date` is NOT NULL in the schema. Fall back to "now" if 247
    // omits it (unlikely but defensive — the column drives incremental refresh).
    let last_update_date = parse_dt(p.get("lastUpdateDate")).unwrap_or_else(Utc::now);

    let source = p.get("transfer").and_then(|t| t.get("source"));
    let primary_dest = primary_destination(p.get("transfer"));

    sqlx::query(
        r#"
        INSERT INTO transfers (
            year, tfs_key,
            first_name, last_name,
            avatar_url, player_profile_url,
            height, weight, position, position_group_name,
            position_key, position_group_key, position_rank,
            rating, transfer_rating, high_school_rating, star_rating,
            rank, transfer_rank, high_school_rank, state_rank, rank_trend,
            status, institution_status, status_date,
            eligibility_type, eligibility_years,
            start_date, end_date,
            transfer_date, transfer_commit_datetime, last_update_date,
            source_institution, source_institution_key, source_logo_url,
            destination_institution, destination_institution_key, destination_logo_url,
            destination_transferred, destination_percentage,
            raw_player
        ) VALUES (
            $1, $2,
            $3, $4,
            $5, $6,
            $7, $8, $9, $10,
            $11, $12, $13,
            $14, $15, $16, $17,
            $18, $19, $20, $21, $22,
            $23, $24, $25,
            $26, $27,
            $28, $29,
            $30, $31, $32,
            $33, $34, $35,
            $36, $37, $38,
            $39, $40,
            $41
        )
        ON CONFLICT (year, tfs_key) DO UPDATE SET
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            avatar_url = EXCLUDED.avatar_url,
            player_profile_url = EXCLUDED.player_profile_url,
            height = EXCLUDED.height,
            weight = EXCLUDED.weight,
            position = EXCLUDED.position,
            position_group_name = EXCLUDED.position_group_name,
            position_key = EXCLUDED.position_key,
            position_group_key = EXCLUDED.position_group_key,
            position_rank = EXCLUDED.position_rank,
            rating = EXCLUDED.rating,
            transfer_rating = EXCLUDED.transfer_rating,
            high_school_rating = EXCLUDED.high_school_rating,
            star_rating = EXCLUDED.star_rating,
            rank = EXCLUDED.rank,
            transfer_rank = EXCLUDED.transfer_rank,
            high_school_rank = EXCLUDED.high_school_rank,
            state_rank = EXCLUDED.state_rank,
            rank_trend = EXCLUDED.rank_trend,
            status = EXCLUDED.status,
            institution_status = EXCLUDED.institution_status,
            status_date = EXCLUDED.status_date,
            eligibility_type = EXCLUDED.eligibility_type,
            eligibility_years = EXCLUDED.eligibility_years,
            start_date = EXCLUDED.start_date,
            end_date = EXCLUDED.end_date,
            transfer_date = EXCLUDED.transfer_date,
            transfer_commit_datetime = EXCLUDED.transfer_commit_datetime,
            last_update_date = EXCLUDED.last_update_date,
            source_institution = EXCLUDED.source_institution,
            source_institution_key = EXCLUDED.source_institution_key,
            source_logo_url = EXCLUDED.source_logo_url,
            destination_institution = EXCLUDED.destination_institution,
            destination_institution_key = EXCLUDED.destination_institution_key,
            destination_logo_url = EXCLUDED.destination_logo_url,
            destination_transferred = EXCLUDED.destination_transferred,
            destination_percentage = EXCLUDED.destination_percentage,
            raw_player = EXCLUDED.raw_player,
            fetched_at = NOW()
        "#,
    )
    .bind(year)
    .bind(tfs_key)
    .bind(&first_name)
    .bind(&last_name)
    .bind(string(p.get("avatar")))
    .bind(string(p.get("playerProfileUrl")))
    .bind(string(p.get("height")))
    .bind(int(p.get("weight")))
    .bind(string(p.get("position")))
    .bind(string(p.get("positionGroupName")))
    .bind(int(p.get("positionKey")))
    .bind(int(p.get("positionGroupKey")))
    .bind(int(p.get("positionRank")))
    .bind(float(p.get("rating")))
    .bind(float(p.get("transferRating")))
    .bind(float(p.get("highSchoolRating")))
    .bind(small_int(p.get("starRating")))
    .bind(int(p.get("rank")))
    .bind(int(p.get("transferRank")))
    .bind(int(p.get("highSchoolRank")))
    .bind(int(p.get("stateRank")))
    .bind(int(p.get("rankTrend")))
    .bind(&status)
    .bind(&institution_status)
    .bind(parse_dt(p.get("statusDate")))
    .bind(&eligibility_type)
    .bind(p.get("eligibility").and_then(|e| small_int(e.get("years"))))
    .bind(parse_dt(p.get("startDate")))
    .bind(parse_dt(p.get("endDate")))
    .bind(parse_dt(p.get("transferDate")))
    .bind(parse_dt(p.get("transferCommitDateTime")))
    .bind(last_update_date)
    .bind(source.and_then(|s| string(s.get("institution"))))
    .bind(source.and_then(|s| int(s.get("institutionKey"))))
    .bind(source.and_then(|s| string(s.get("logo"))))
    .bind(
        primary_dest
            .as_ref()
            .and_then(|d| string(d.get("institution"))),
    )
    .bind(
        primary_dest
            .as_ref()
            .and_then(|d| int(d.get("institutionKey"))),
    )
    .bind(primary_dest.as_ref().and_then(|d| string(d.get("logo"))))
    .bind(
        primary_dest
            .as_ref()
            .and_then(|d| d.get("transferred").and_then(|v| v.as_bool())),
    )
    .bind(
        primary_dest
            .as_ref()
            .and_then(|d| small_int(d.get("percentage"))),
    )
    .bind(p)
    .execute(pool)
    .await?;

    Ok(true)
}

/// Pick the primary destination from `transfer.destination[]`. Tie-breaker:
/// `transferred = true` wins; else highest `percentage`; else first; else None.
fn primary_destination(transfer: Option<&Value>) -> Option<Value> {
    let arr = transfer?.get("destination")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    if let Some(d) = arr.iter().find(|d| {
        d.get("transferred")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }) {
        return Some(d.clone());
    }
    arr.iter()
        .max_by_key(|d| d.get("percentage").and_then(|v| v.as_i64()).unwrap_or(-1))
        .cloned()
        .or_else(|| arr.first().cloned())
}

/// Read our DB's high-water mark for incremental refresh: the max
/// `last_update_date` we've recorded for this year. Returns None on first run.
async fn last_update_cursor(
    pool: &PgPool,
    year: i32,
) -> Result<Option<DateTime<Utc>>, TransferIngestError> {
    let row: Option<(Option<DateTime<Utc>>,)> =
        sqlx::query_as("SELECT MAX(last_update_date) FROM transfers WHERE year = $1")
            .bind(year)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(v,)| v))
}

/// Resolve `cstat_player_id` on transfer rows by matching each 247 portal
/// entry to the cstat `players` row that played the previous college season
/// at the same source school.
///
/// `t.year` (247's transfer class year — spring-2026 portal = 2026) and
/// `p.season` (cstat-season end-year — 2025-26 academic year = season 2026)
/// bind to the same integer by construction: spring-2026 portal entries played
/// their last completed season in 2025-26.
///
/// Matching is done in Rust rather than via SQL JOIN because the route handler
/// already established the correct join logic and a plain `lower(tm.name) =
/// lower(t.source_institution)` against the full NatStat name (e.g. "Kansas
/// Jayhawks" vs 247's "Kansas") resolved 0/4,357 rows. We mirror the route's
/// `normalize` + `team_match_score` so SQL-side and route-side reach identical
/// answers for the same (player, source) pair. If a third consumer appears,
/// promote these helpers to a shared crate.
pub async fn resolve_cstat_joins(pool: &PgPool, year: i32) -> Result<u64, TransferIngestError> {
    #[derive(sqlx::FromRow)]
    struct PortalRow {
        tfs_key: i64,
        full_name: String,
        source_institution: Option<String>,
    }
    #[derive(sqlx::FromRow)]
    struct CandRow {
        player_id: Uuid,
        name: String,
        team_short: Option<String>,
        team_full: Option<String>,
        minutes_per_game: Option<f64>,
    }

    // Only consider rows that have a source school to disambiguate against —
    // a portal row with NULL source can't be matched safely.
    let portal: Vec<PortalRow> = sqlx::query_as(
        r#"
        SELECT tfs_key, full_name, source_institution
        FROM transfers
        WHERE year = $1 AND source_institution IS NOT NULL
        "#,
    )
    .bind(year)
    .fetch_all(pool)
    .await?;

    // One candidate row per (player, team) stint in the season. Mid-season
    // transfers appear twice (once per team) via `player_season_stats`, which
    // is what lets us disambiguate when the 247 source names the *first*
    // team. Pull `minutes_per_game` as the tiebreaker for collisions on a
    // common name where source doesn't disambiguate.
    let candidates: Vec<CandRow> = sqlx::query_as(
        r#"
        SELECT
            p.id                     AS player_id,
            p.name                   AS name,
            t.short_name             AS team_short,
            t.name                   AS team_full,
            pss.minutes_per_game     AS minutes_per_game
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
        WHERE pss.season = $1
        "#,
    )
    .bind(year)
    .fetch_all(pool)
    .await?;

    // Bucket by normalized name for O(1) per-portal-row lookup. Same
    // normalization the route uses (accent fold + suffix strip).
    let mut by_name: HashMap<String, Vec<&CandRow>> = HashMap::new();
    for c in &candidates {
        by_name.entry(normalize_name(&c.name)).or_default().push(c);
    }

    let mut tfs_keys: Vec<i64> = Vec::new();
    let mut player_ids: Vec<Uuid> = Vec::new();
    let mut unmatched_name = 0u64;
    let mut unmatched_team = 0u64;

    for row in &portal {
        let Some(cands) = by_name.get(&normalize_name(&row.full_name)) else {
            unmatched_name += 1;
            continue;
        };
        let source = row.source_institution.as_deref().unwrap_or_default();
        // Prefer the candidate whose team scores best against the 247 source
        // string (lower score = better match; see `team_match_score`). If
        // nothing scores, fall back to the most-played candidate so a common
        // name with stats still lands somewhere — same fallback the route
        // uses for the enrichment join.
        let best = cands
            .iter()
            .filter_map(|c| {
                team_match_score(c.team_short.as_deref(), c.team_full.as_deref()?, source)
                    .map(|s| (s, *c))
            })
            .min_by_key(|(s, _)| *s)
            .map(|(_, c)| c)
            .or_else(|| {
                unmatched_team += 1;
                cands.iter().copied().max_by(|a, b| {
                    a.minutes_per_game
                        .unwrap_or(0.0)
                        .partial_cmp(&b.minutes_per_game.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });
        if let Some(c) = best {
            tfs_keys.push(row.tfs_key);
            player_ids.push(c.player_id);
        }
    }

    // Batched UPDATE via UNNEST — one round-trip regardless of match count.
    // `IS DISTINCT FROM` avoids a no-op write when the row is already
    // resolved to the same player.
    let result = sqlx::query(
        r#"
        UPDATE transfers t
        SET cstat_player_id = m.player_id
        FROM UNNEST($2::bigint[], $3::uuid[]) AS m(tfs_key, player_id)
        WHERE t.year = $1
          AND t.tfs_key = m.tfs_key
          AND t.cstat_player_id IS DISTINCT FROM m.player_id
        "#,
    )
    .bind(year)
    .bind(&tfs_keys)
    .bind(&player_ids)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    info!(
        year,
        portal_rows = portal.len(),
        candidates = candidates.len(),
        matched = tfs_keys.len(),
        updated = n,
        unmatched_name,
        unmatched_team_fallback = unmatched_team,
        "cstat_player_id resolution complete"
    );
    if tfs_keys.is_empty() {
        warn!(
            year,
            "no cstat_player_id matches found — check naming normalization"
        );
    }
    Ok(n)
}

/// Normalize a player name for cross-source matching: lowercase, fold the
/// diacritics we actually see in cstat / 247 data, strip generational
/// suffixes. Mirrors `normalize` in `cstat-api`'s `routes/transfers.rs` so
/// the post-ingest resolution and the runtime enrichment join reach the same
/// answer for the same name. Keep the two copies in sync until extracted.
fn normalize_name(name: &str) -> String {
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
        // "lll" appears in our DB for "Ace Glass III" (typo, three lowercase
        // L's instead of three capital I's); strip like a suffix so 247 matches.
        .filter(|w| !matches!(*w, "jr" | "sr" | "ii" | "iii" | "iv" | "v" | "lll"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 247 short name → cstat team-name prefix that should appear at the start of
/// `teams.name`. Mirrors `TEAM_ALIASES` in `cstat-api`'s `routes/transfers.rs`.
const TEAM_ALIASES: &[(&str, &str)] = &[
    ("uconn", "connecticut"),
    ("ole miss", "mississippi"),
    ("usc", "southern california"),
    ("nc state", "north carolina state"),
    ("miami", "miami (fla.)"),
    ("miami (fl)", "miami (fla.)"),
    ("miami (oh)", "miami (ohio)"),
];

/// Score how well a cstat team matches a 247 short name. Lower is better;
/// `None` means no match. Mirrors `team_match_score` in `cstat-api`'s
/// `routes/transfers.rs`.
fn team_match_score(db_short: Option<&str>, db_full: &str, short: &str) -> Option<u32> {
    let short_lc = short.to_lowercase();
    if let Some(s) = db_short
        && s.to_lowercase() == short_lc
    {
        return Some(0);
    }
    let db_lc = db_full.to_lowercase();
    if db_lc == short_lc {
        return Some(0);
    }
    for (k, v) in TEAM_ALIASES {
        if short_lc == *k && (db_lc == *v || db_lc.starts_with(&format!("{v} "))) {
            return Some(1);
        }
    }
    if db_lc.starts_with(&format!("{short_lc} ")) {
        return Some(2);
    }
    None
}

// --- Small JSON-extraction helpers ----------------------------------------

fn string(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str()).map(String::from)
}

fn int(v: Option<&Value>) -> Option<i32> {
    v.and_then(|v| v.as_i64()).map(|n| n as i32)
}

fn small_int(v: Option<&Value>) -> Option<i16> {
    v.and_then(|v| v.as_i64()).map(|n| n as i16)
}

fn float(v: Option<&Value>) -> Option<f32> {
    v.and_then(|v| v.as_f64()).map(|n| n as f32)
}

fn parse_dt(v: Option<&Value>) -> Option<DateTime<Utc>> {
    v.and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Return the input as `Some(String)` if it's in the allowed set, otherwise
/// `None` (warning logged). Used for nullable enum columns so an unexpected
/// upstream value doesn't trip the DB-side CHECK constraint and abort ingest.
fn sanitize_enum(
    v: Option<&Value>,
    allowed: &[&str],
    field_name: &str,
    year: i32,
    tfs_key: i64,
) -> Option<String> {
    let raw = v.and_then(|v| v.as_str())?;
    if allowed.contains(&raw) {
        Some(raw.to_string())
    } else {
        warn!(
            year,
            tfs_key,
            field = field_name,
            value = raw,
            "unknown enum value — storing NULL (widen the allowed set + CHECK to accept)"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primary_destination_prefers_transferred_true() {
        let t = json!({
            "destination": [
                { "institution": "A", "percentage": 80, "transferred": false },
                { "institution": "B", "percentage": 20, "transferred": true }
            ]
        });
        let d = primary_destination(Some(&t)).unwrap();
        assert_eq!(d.get("institution").and_then(|v| v.as_str()), Some("B"));
    }

    #[test]
    fn primary_destination_falls_back_to_highest_percentage() {
        let t = json!({
            "destination": [
                { "institution": "A", "percentage": 40, "transferred": false },
                { "institution": "B", "percentage": 60, "transferred": false }
            ]
        });
        let d = primary_destination(Some(&t)).unwrap();
        assert_eq!(d.get("institution").and_then(|v| v.as_str()), Some("B"));
    }

    #[test]
    fn primary_destination_none_when_empty() {
        let t = json!({ "destination": [] });
        assert!(primary_destination(Some(&t)).is_none());
    }

    #[test]
    fn sanitize_enum_accepts_known_values() {
        let v = json!("Committed");
        assert_eq!(
            sanitize_enum(Some(&v), ALLOWED_STATUS, "status", 2026, 1),
            Some("Committed".to_string())
        );
    }

    #[test]
    fn sanitize_enum_rejects_unknown_values() {
        let v = json!("Pending");
        assert_eq!(
            sanitize_enum(Some(&v), ALLOWED_STATUS, "status", 2026, 1),
            None
        );
    }

    #[test]
    fn sanitize_enum_passes_through_null_input() {
        assert_eq!(sanitize_enum(None, ALLOWED_STATUS, "status", 2026, 1), None);
    }

    #[test]
    fn parse_dt_handles_z_suffix() {
        let v = json!("2026-05-10T23:30:00Z");
        let dt = parse_dt(Some(&v)).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-10T23:30:00+00:00");
    }

    #[test]
    fn parse_dt_normalizes_offset_to_utc() {
        let v = json!("2026-05-10T19:30:00-04:00");
        let dt = parse_dt(Some(&v)).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-05-10T23:30:00+00:00");
    }

    #[test]
    fn parse_dt_returns_none_for_garbage() {
        let v = json!("not a date");
        assert!(parse_dt(Some(&v)).is_none());
    }

    #[test]
    fn parse_dt_returns_none_for_non_string() {
        let v = json!(1234567890);
        assert!(parse_dt(Some(&v)).is_none());
    }

    #[test]
    fn normalize_lowercases_and_strips_suffixes() {
        assert_eq!(normalize_name("LeBron James Jr."), "lebron james");
        assert_eq!(normalize_name("Ace Glass III"), "ace glass");
        assert_eq!(normalize_name("Ace Glass lll"), "ace glass");
        assert_eq!(normalize_name("Freddie Dilione V"), "freddie dilione");
    }

    #[test]
    fn normalize_folds_accents_and_drops_punctuation() {
        assert_eq!(normalize_name("José Álvarez"), "jose alvarez");
        assert_eq!(normalize_name("A'lahn Sumler"), "alahn sumler");
        assert_eq!(normalize_name("D'Angelo  Russell"), "dangelo russell");
    }

    #[test]
    fn team_match_score_prefers_short_name_exact() {
        assert_eq!(
            team_match_score(Some("Kansas"), "Kansas Jayhawks", "Kansas"),
            Some(0)
        );
    }

    #[test]
    fn team_match_score_handles_uconn_via_alias() {
        // teams.short_name is "Connecticut", 247 sends "UConn".
        assert_eq!(
            team_match_score(Some("Connecticut"), "Connecticut Huskies", "UConn"),
            Some(1)
        );
    }

    #[test]
    fn team_match_score_disambiguates_miami() {
        // Both Miami (Fla.) and Miami (Ohio) score against bare "Miami" — FL
        // via the alias (Some(1)) and OH via the bare-prefix fallback
        // (Some(2)). Disambiguation is by the lower score, which
        // `resolve_cstat_joins` selects via `min_by_key`.
        let fl = team_match_score(Some("Miami FL"), "Miami (Fla.) Hurricanes", "Miami");
        let oh = team_match_score(Some("Miami OH"), "Miami (Ohio) Redhawks", "Miami");
        assert_eq!(fl, Some(1));
        assert_eq!(oh, Some(2));
        assert!(fl < oh, "FL alias must outrank OH bare-prefix fallback");
    }

    #[test]
    fn team_match_score_falls_back_to_prefix() {
        // No short_name on this row: legacy fallback should still match.
        assert_eq!(
            team_match_score(None, "Alabama State Hornets", "Alabama State"),
            Some(2)
        );
    }

    #[test]
    fn team_match_score_returns_none_on_unrelated() {
        assert!(team_match_score(Some("Duke"), "Duke Blue Devils", "Kansas").is_none());
    }
}
