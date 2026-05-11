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
use std::path::Path;
use thiserror::Error;
use tracing::{info, warn};

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
    let status = string(p.get("status")).unwrap_or_else(|| "Entered".to_string());

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
    .bind(string(p.get("institutionStatus")))
    .bind(parse_dt(p.get("statusDate")))
    .bind(p.get("eligibility").and_then(|e| string(e.get("type"))))
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

/// Resolve `cstat_player_id` on transfer rows by joining to cstat `players`
/// on `(year = season, lower(full_name) = lower(name), lower(source) = lower(team))`.
///
/// v1: case-insensitive exact match. v2 should reuse the suffix-stripping /
/// punctuation-normalization logic from `scripts/parse_247_transfer_html.py`
/// for the ~few-percent of rows where suffixes ("Jr.", "III") differ between
/// 247 and cstat.
pub async fn resolve_cstat_joins(pool: &PgPool, year: i32) -> Result<u64, TransferIngestError> {
    // Both `t.year` and `p.season` bind to the same `$1`. Different concepts
    // that numerically coincide:
    //   - `t.year` is the *transfer class year* (calendar year of the spring
    //     portal cycle). 247's `year=2026` = spring-2026 portal.
    //   - `p.season` is the *cstat-season end-year* (NCAA academic year's
    //     spring half). 2025-26 academic year = season 2026.
    // Spring 2026 portal → players whose last completed season was 2025-26
    // → both labelled `2026`. The semantics differ; the integer matches.
    let result = sqlx::query(
        r#"
        UPDATE transfers t
        SET cstat_player_id = p.id
        FROM players p
        JOIN teams tm ON tm.id = p.team_id
        WHERE t.year = $1
          AND p.season = $1
          AND tm.season = $1
          AND lower(p.name) = lower(t.full_name)
          AND lower(tm.name) = lower(t.source_institution)
          AND t.cstat_player_id IS DISTINCT FROM p.id
        "#,
    )
    .bind(year)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    if n > 0 {
        info!(year, resolved = n, "cstat_player_id resolved");
    } else {
        warn!(
            year,
            "no cstat_player_id matches found — check naming normalization"
        );
    }
    Ok(n)
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
}
