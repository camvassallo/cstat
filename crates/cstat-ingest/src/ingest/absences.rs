//! Bootstrap the `player_absences` table from the version-controlled
//! `data/absences/{year}_absences.json` captures (issue #217).
//!
//! Sibling of `ingest::departures`, shaped the same way: the JSON files are the
//! source of record, this loader is the bridge into the DB, and the DB is what
//! the roster projection reads (`fetch_player_absences`) and what
//! `sync_to_prod.sh` ships.
//!
//! No feed reports a season-long injury in a form cstat can read, so every row
//! is entered by hand from a report — a RotoWire player page is the preferred
//! `source`. A row removes the player from his team's scored roster for the
//! target season without recording him as having left the program.

use cstat_core::roster_projection::{PlayerAbsence, load_player_absences};
use sqlx::PgPool;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum AbsenceIngestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

/// Per-file load result for the CLI summary.
pub struct AbsenceLoadReport {
    pub year: i32,
    pub rows: usize,
}

/// Load every `{year}_absences.json` under `dir` into `player_absences`. A
/// missing directory is an error — the caller asked for a load.
pub async fn bootstrap_from_dir(
    pool: &PgPool,
    dir: &Path,
) -> Result<Vec<AbsenceLoadReport>, AbsenceIngestError> {
    let mut reports = Vec::new();
    let mut entries: Vec<(i32, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Filenames are `{year}_absences`.
        let Some(year_str) = stem.strip_suffix("_absences") else {
            continue;
        };
        let Ok(year) = year_str.parse::<i32>() else {
            warn!(file = %path.display(), "skipping absences file with non-numeric year");
            continue;
        };
        entries.push((year, path));
    }
    entries.sort_by_key(|(y, _)| *y);

    for (year, path) in entries {
        let absences = load_player_absences(&path)?;
        let n = bootstrap_year(pool, year, &absences).await?;
        info!(year, rows = n, "loaded player absences");
        reports.push(AbsenceLoadReport { year, rows: n });
    }
    Ok(reports)
}

/// Replace one year's absences. Returns the row count written.
///
/// Deletes the year before inserting, like the departures and returns loaders:
/// the file is the source of record, and an upsert-only load would leave a
/// retracted absence (a player cleared to return mid-season) silently in force.
/// Scoped to `year` and transactional.
async fn bootstrap_year(
    pool: &PgPool,
    year: i32,
    absences: &[PlayerAbsence],
) -> Result<usize, AbsenceIngestError> {
    // Validate before touching the table, so a typo cannot half-apply a year.
    for a in absences {
        if let Some(share) = a.availability
            && !(share > 0.0 && share < 1.0)
        {
            return Err(std::io::Error::other(format!(
                "{} ({}): availability {share} must be strictly between 0 and 1 \
                 (omit it for a whole-season absence)",
                a.name, a.current_team
            ))
            .into());
        }
    }
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM player_absences WHERE year = $1")
        .bind(year)
        .execute(&mut *tx)
        .await?;
    for a in absences {
        sqlx::query(
            r#"
            INSERT INTO player_absences
                (year, player_name, current_team, injury, availability, source, note)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (year, player_name, current_team)
            DO UPDATE SET injury = EXCLUDED.injury,
                          availability = EXCLUDED.availability,
                          source = EXCLUDED.source,
                          note = EXCLUDED.note
            "#,
        )
        .bind(year)
        .bind(&a.name)
        .bind(&a.current_team)
        .bind(&a.injury)
        .bind(a.availability)
        .bind(&a.source)
        .bind(&a.note)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(absences.len())
}
