//! Bootstrap the `draft_entrants` table from the version-controlled
//! `data/draft/{year}_early_entrants.json` captures.
//!
//! The JSON files are the source of record (historical lists built from
//! Tankathon past-drafts via `scripts/build_historical_draft_entrants.py`; the
//! live-forecast year is maintained by hand). This loader is the bridge into
//! the DB, which is what the roster projection reads (`fetch_draft_entrants`)
//! and what `sync_to_prod.sh` ships to prod. Idempotent: upserts on
//! `(year, player_name, current_team)`.

use cstat_core::roster_projection::load_draft_entrants;
use sqlx::PgPool;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum DraftIngestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

/// Per-file load result for the CLI summary.
pub struct DraftLoadReport {
    pub year: i32,
    pub rows: usize,
}

/// Load every `{year}_early_entrants.json` under `dir` into `draft_entrants`.
/// `source` is stamped on each row for provenance (e.g. "tankathon").
pub async fn bootstrap_from_dir(
    pool: &PgPool,
    dir: &Path,
    source: &str,
) -> Result<Vec<DraftLoadReport>, DraftIngestError> {
    let mut reports = Vec::new();
    let mut entries: Vec<(i32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Filenames are `{year}_early_entrants`.
        let Some(year_str) = stem.strip_suffix("_early_entrants") else {
            continue;
        };
        let Ok(year) = year_str.parse::<i32>() else {
            warn!(file = %path.display(), "skipping draft file with non-numeric year");
            continue;
        };
        entries.push((year, path));
    }
    entries.sort_by_key(|(y, _)| *y);

    for (year, path) in entries {
        let entrants = load_draft_entrants(&path)?;
        let n = bootstrap_year(pool, year, &entrants, source).await?;
        info!(year, rows = n, "loaded draft entrants");
        reports.push(DraftLoadReport { year, rows: n });
    }
    Ok(reports)
}

/// Upsert one year's entrants. Returns the row count written.
async fn bootstrap_year(
    pool: &PgPool,
    year: i32,
    entrants: &[cstat_core::roster_projection::DraftEntrant],
    source: &str,
) -> Result<usize, DraftIngestError> {
    for e in entrants {
        sqlx::query(
            r#"
            INSERT INTO draft_entrants (year, player_name, current_team, status, source)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (year, player_name, current_team)
            DO UPDATE SET status = EXCLUDED.status, source = EXCLUDED.source
            "#,
        )
        .bind(year)
        .bind(&e.name)
        .bind(&e.current_team)
        .bind(&e.status)
        .bind(source)
        .execute(pool)
        .await?;
    }
    Ok(entrants.len())
}
