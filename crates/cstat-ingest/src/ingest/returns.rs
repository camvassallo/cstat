//! Bootstrap the `player_returns` table from the version-controlled
//! `data/returns/{year}_returns.json` captures.
//!
//! Mirror of `ingest::departures`, for the opposite direction: those rows say
//! "this player the projection thinks is returning has actually gone", these
//! say "this player the projection thinks has graduated is actually staying".
//! The JSON files are the source of record, this loader is the bridge into the
//! DB, and the DB is what the roster projection reads
//! (`fetch_player_returns`) and what `sync_to_prod.sh` ships. Idempotent:
//! upserts on `(year, player_name, current_team)`.
//!
//! Why hand-curated. The NCAA's age-based 5-in-5 rule (issue #220) invalidated
//! cstat's only eligibility mechanism — a `class_year == 'Sr'` string check —
//! for the 2027 season onward. Seniors who take the extra year *elsewhere*
//! resolve themselves through the 247 portal feed. Seniors who take it *at the
//! same school* appear in no feed at all: not the portal, not the draft list,
//! and not Torvik's `class_year`, which does not exist for a season that hasn't
//! been played. Until that changes, a human reading the news is the only
//! source, exactly as it was for the non-portal exits in issue #215.
//!
//! The one automatic signal — a senior who entered the portal and withdrew —
//! is derived in `compose_all_projections` and needs no row here. Use this file
//! to *override* it (a `granted` row promotes him out of `uncertain`), or to
//! record the players it can't see.

use cstat_core::roster_projection::load_player_returns;
use sqlx::PgPool;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum ReturnIngestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error(
        "{year}_returns.json: {name} ({team}) has status {status:?}; \
         expected \"granted\" or \"contested\""
    )]
    BadStatus {
        year: i32,
        name: String,
        team: String,
        status: String,
    },
}

/// Per-file load result for the CLI summary.
pub struct ReturnLoadReport {
    pub year: i32,
    pub rows: usize,
    /// How many of the rows are still unresolved. Surfaced separately because
    /// it is the number that should be shrinking as the litigation settles.
    pub contested: usize,
}

/// Load every `{year}_returns.json` under `dir` into `player_returns`.
/// A missing directory is an error — the caller asked for a load, and silently
/// writing nothing is the failure mode this whole capture exists to avoid.
pub async fn bootstrap_from_dir(
    pool: &PgPool,
    dir: &Path,
) -> Result<Vec<ReturnLoadReport>, ReturnIngestError> {
    let mut reports = Vec::new();
    let mut entries: Vec<(i32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Filenames are `{year}_returns`.
        let Some(year_str) = stem.strip_suffix("_returns") else {
            continue;
        };
        let Ok(year) = year_str.parse::<i32>() else {
            warn!(file = %path.display(), "skipping returns file with non-numeric year");
            continue;
        };
        entries.push((year, path));
    }
    entries.sort_by_key(|(y, _)| *y);

    for (year, path) in entries {
        let returns = load_player_returns(&path)?;
        let (n, contested) = bootstrap_year(pool, year, &returns).await?;
        info!(year, rows = n, contested, "loaded player returns");
        reports.push(ReturnLoadReport {
            year,
            rows: n,
            contested,
        });
    }
    Ok(reports)
}

/// Upsert one year's returns. Returns `(rows written, contested count)`.
async fn bootstrap_year(
    pool: &PgPool,
    year: i32,
    returns: &[cstat_core::roster_projection::PlayerReturn],
) -> Result<(usize, usize), ReturnIngestError> {
    // Validate the whole file before writing any of it. `status` is
    // behaviour-bearing — it picks the `returning` vs `uncertain` bucket — and
    // the DB CHECK would reject a typo mid-loop, leaving a half-applied
    // capture. Fail before the first INSERT instead.
    for r in returns {
        let s = r.status.trim().to_ascii_lowercase();
        if s != "granted" && s != "contested" {
            return Err(ReturnIngestError::BadStatus {
                year,
                name: r.name.clone(),
                team: r.current_team.clone(),
                status: r.status.clone(),
            });
        }
    }

    let mut contested = 0usize;
    for r in returns {
        let status = r.status.trim().to_ascii_lowercase();
        if status == "contested" {
            contested += 1;
        }
        sqlx::query(
            r#"
            INSERT INTO player_returns
                (year, player_name, current_team, status, reason, source, note)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (year, player_name, current_team)
            DO UPDATE SET status = EXCLUDED.status,
                          reason = EXCLUDED.reason,
                          source = EXCLUDED.source,
                          note = EXCLUDED.note
            "#,
        )
        .bind(year)
        .bind(&r.name)
        .bind(&r.current_team)
        .bind(&status)
        .bind(&r.reason)
        .bind(&r.source)
        .bind(&r.note)
        .execute(pool)
        .await?;
    }
    Ok((returns.len(), contested))
}
