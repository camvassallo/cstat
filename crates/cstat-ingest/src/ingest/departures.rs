//! Bootstrap the `player_departures` table from the version-controlled
//! `data/departures/{year}_departures.json` captures.
//!
//! Sibling of `ingest::draft`, and deliberately shaped the same way: the JSON
//! files are the source of record, this loader is the bridge into the DB, and
//! the DB is what the roster projection reads (`fetch_player_departures`) and
//! what `sync_to_prod.sh` ships. Idempotent: upserts on
//! `(year, player_name, current_team)`.
//!
//! Unlike the draft captures — where a historical list can be scraped from
//! Tankathon — there is no feed for these. Every row is entered by hand from a
//! news report, which is exactly why they were invisible to the projection
//! before issue #215. `departures-audit` (see `crate::departures_audit`) is the
//! companion that tells you *which* players to go looking for.

use cstat_core::roster_projection::load_player_departures;
use sqlx::PgPool;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum DepartureIngestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

/// Per-file load result for the CLI summary.
pub struct DepartureLoadReport {
    pub year: i32,
    pub rows: usize,
}

/// Load every `{year}_departures.json` under `dir` into `player_departures`.
/// A missing directory is an error — the caller asked for a load; silently
/// writing nothing is how the pre-#215 JSON-on-disk draft path went wrong.
pub async fn bootstrap_from_dir(
    pool: &PgPool,
    dir: &Path,
) -> Result<Vec<DepartureLoadReport>, DepartureIngestError> {
    let mut reports = Vec::new();
    let mut entries: Vec<(i32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Filenames are `{year}_departures`.
        let Some(year_str) = stem.strip_suffix("_departures") else {
            continue;
        };
        let Ok(year) = year_str.parse::<i32>() else {
            warn!(file = %path.display(), "skipping departures file with non-numeric year");
            continue;
        };
        entries.push((year, path));
    }
    entries.sort_by_key(|(y, _)| *y);

    for (year, path) in entries {
        let departures = load_player_departures(&path)?;
        let n = bootstrap_year(pool, year, &departures).await?;
        info!(year, rows = n, "loaded player departures");
        reports.push(DepartureLoadReport { year, rows: n });
    }
    Ok(reports)
}

/// Replace one year's departures. Returns the row count written.
///
/// Deletes the year before inserting, for the same reason as the returns
/// loader: the JSON file is the source of record, and an upsert-only load
/// honours additions but silently ignores removals. Retracting a departure —
/// a report that turned out to be wrong, a player who withdrew — would leave
/// him deleted from his team's projection with nothing in the file to explain
/// it. Scoped to `year` and transactional, so a mid-load failure cannot leave
/// the season empty; a year with no file in `dir` is never touched.
async fn bootstrap_year(
    pool: &PgPool,
    year: i32,
    departures: &[cstat_core::roster_projection::PlayerDeparture],
) -> Result<usize, DepartureIngestError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM player_departures WHERE year = $1")
        .bind(year)
        .execute(&mut *tx)
        .await?;
    for d in departures {
        sqlx::query(
            r#"
            INSERT INTO player_departures
                (year, player_name, current_team, reason, destination, source, note)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (year, player_name, current_team)
            DO UPDATE SET reason = EXCLUDED.reason,
                          destination = EXCLUDED.destination,
                          source = EXCLUDED.source,
                          note = EXCLUDED.note
            "#,
        )
        .bind(year)
        .bind(&d.name)
        .bind(&d.current_team)
        .bind(&d.reason)
        .bind(&d.destination)
        .bind(&d.source)
        .bind(&d.note)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(departures.len())
}
