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

/// Every `{year}_returns.json` under `dir`, oldest first. Shared by the loader
/// and [`resolve_reason`] so the two cannot disagree about which files count.
fn returns_files(dir: &Path) -> Result<Vec<(i32, std::path::PathBuf)>, ReturnIngestError> {
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
    Ok(entries)
}

/// Load every `{year}_returns.json` under `dir` into `player_returns`.
/// A missing directory is an error — the caller asked for a load, and silently
/// writing nothing is the failure mode this whole capture exists to avoid.
pub async fn bootstrap_from_dir(
    pool: &PgPool,
    dir: &Path,
) -> Result<Vec<ReturnLoadReport>, ReturnIngestError> {
    let mut reports = Vec::new();
    for (year, path) in returns_files(dir)? {
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

    // Replace the year wholesale rather than upserting into it.
    //
    // `data/returns/README.md` says the JSON files are the source of record,
    // and an upsert-only load quietly breaks that promise in one direction:
    // rows ADDED to the file appear, rows REMOVED from it do not disappear.
    // Deleting a curated row is not a hypothetical cleanup — it is the correct
    // response to one of the two ways the class-of-2022 appeal can end. If the
    // Tenth Circuit rules for the NCAA, those players are departures again and
    // the rows must go; under upsert-only they would sit in `player_returns`
    // restoring 23 players to their teams forever, while the file and the
    // loader's own output both said they were gone.
    //
    // Scoped to `year` and run in the same transaction as the inserts, so a
    // failure mid-load cannot leave the season empty. Years with no file in
    // `dir` are never touched.
    let mut tx = pool.begin().await?;
    let removed = sqlx::query("DELETE FROM player_returns WHERE year = $1")
        .bind(year)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;

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
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    if removed > returns.len() {
        info!(
            year,
            removed,
            kept = returns.len(),
            "returns: file has fewer rows than the table did — the difference is deleted, \
             which restores those players to departing"
        );
    }
    Ok((returns.len(), contested))
}

/// The key order curated return rows are written in. Identity first, then the
/// two behaviour-bearing fields, then provenance.
const RETURN_KEY_ORDER: &[&str] = &["name", "current_team", "status", "reason", "source", "note"];

/// Render curated returns with a stable key order.
///
/// `serde_json::to_string_pretty` cannot do this: without the `preserve_order`
/// feature a `Value` object is a `BTreeMap`, so it emits keys alphabetically —
/// `current_team, name, note, reason, source, status`. Round-tripping the file
/// through it reorders every row, so a 23-row resolution produces a 48-row
/// diff and the change is unreviewable. Reviewability is the entire reason
/// [`resolve_reason`] stops before loading, so churning the file would defeat
/// the feature.
///
/// Enabling `preserve_order` workspace-wide would fix it globally and is the
/// wrong trade here: `cstat_core::provenance` builds fingerprints out of
/// `json!` values, and silently changing their key order risks every model node
/// reporting drift at once.
///
/// Keys outside [`RETURN_KEY_ORDER`] are emitted after the known ones, sorted,
/// so a field this binary does not model still survives a round-trip.
fn render_returns(rows: &[serde_json::Value]) -> Result<String, ReturnIngestError> {
    let enc = |v: &serde_json::Value| -> Result<String, ReturnIngestError> {
        serde_json::to_string(v).map_err(|e| std::io::Error::other(e.to_string()).into())
    };
    let mut out = String::from("[\n");
    for (i, row) in rows.iter().enumerate() {
        let Some(obj) = row.as_object() else {
            return Err(std::io::Error::other("returns file must be an array of objects").into());
        };
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort_by_key(|k| {
            (
                RETURN_KEY_ORDER
                    .iter()
                    .position(|o| o == k)
                    .unwrap_or(usize::MAX),
                (*k).clone(),
            )
        });
        out.push_str("  {\n");
        for (j, k) in keys.iter().enumerate() {
            out.push_str(&format!(
                "    {}: {}",
                enc(&serde_json::Value::from(k.as_str()))?,
                enc(&obj[*k])?
            ));
            out.push_str(if j + 1 < keys.len() { ",\n" } else { "\n" });
        }
        out.push_str(if i + 1 < rows.len() {
            "  },\n"
        } else {
            "  }\n"
        });
    }
    out.push_str("]\n");
    Ok(out)
}

/// What a court outcome does to a cohort of curated returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The claim succeeded — the players are eligible. Flip `contested` to
    /// `granted` so they project as ordinary returners.
    Granted,
    /// The claim failed — the players are not coming back. Remove the rows
    /// entirely rather than writing some "denied" status: the projection's
    /// default for an unlisted senior is already "departing", so a deleted row
    /// IS the correct outcome, and inventing a third status would mean teaching
    /// the projection a state it has no use for.
    Departed,
}

/// Per-file result of a [`resolve_reason`] pass.
pub struct ResolveReport {
    pub year: i32,
    pub matched: usize,
    pub outcome: Resolution,
}

/// Apply a court outcome to every curated return carrying `reason`.
///
/// Eligibility litigation resolves per *cohort*, not per player: one Tenth
/// Circuit ruling decides all 23 class-of-2022 rows in `2026_returns.json` at
/// once. Hand-editing 23 rows on the day a ruling lands is how a capture goes
/// stale, and it is the one edit most likely to be made in a hurry.
///
/// Keyed on `reason` because that column already tags the cohort exactly —
/// `injunction` for the rows riding the class-wide injunction — so no new
/// grouping concept is needed.
///
/// Rewrites the JSON in place and deliberately does **not** load. The file is
/// the source of record; the point of stopping here is that the change lands as
/// a reviewable git diff before it reaches the DB. Run `cstat-ingest returns`
/// after checking it.
///
/// Operates on `serde_json::Value` rather than `PlayerReturn` so that fields
/// this binary does not model — anything added to the capture format later —
/// survive the round-trip instead of being silently dropped.
pub fn resolve_reason(
    dir: &Path,
    reason: &str,
    outcome: Resolution,
    note_suffix: &str,
) -> Result<Vec<ResolveReport>, ReturnIngestError> {
    let mut reports = Vec::new();
    for (year, path) in returns_files(dir)? {
        let raw = std::fs::read_to_string(&path)?;
        let mut rows: Vec<serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;

        let field = |v: &serde_json::Value, k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        // Rows this outcome would actually CHANGE, not merely rows carrying
        // the reason. Without the status test, re-running `--as granted`
        // stamps the resolution note onto an already-granted row a second
        // time, so the capture accumulates "RESOLVED … RESOLVED …" and the
        // command stops being safe to repeat — which it has to be, since the
        // obvious reaction to an ambiguous first run is to run it again.
        let hits = |v: &serde_json::Value| {
            field(v, "reason") == reason
                && match outcome {
                    Resolution::Granted => field(v, "status") != "granted",
                    Resolution::Departed => true,
                }
        };
        let matched = rows.iter().filter(|v| hits(v)).count();
        if matched == 0 {
            continue;
        }
        match outcome {
            Resolution::Departed => rows.retain(|v| !hits(v)),
            Resolution::Granted => {
                for v in rows.iter_mut().filter(|v| hits(v)) {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("status".into(), serde_json::Value::from("granted"));
                        let note = obj
                            .get("note")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string();
                        obj.insert(
                            "note".into(),
                            serde_json::Value::from(format!("{note} {note_suffix}").trim()),
                        );
                    }
                }
            }
        }
        std::fs::write(&path, render_returns(&rows)?)?;
        info!(year, matched, ?outcome, "resolved curated returns");
        reports.push(ResolveReport {
            year,
            matched,
            outcome,
        });
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rows in the cohort, one outside it, and a field this binary does
    /// not model.
    const FILE: &str = r#"[
      {"name":"A One","current_team":"Duke","status":"contested","reason":"injunction",
       "note":"n1","curator_only":"keep me"},
      {"name":"B Two","current_team":"Penn","status":"contested","reason":"injunction","note":"n2"},
      {"name":"C Three","current_team":"Iona","status":"granted","reason":"5in5","note":"n3"}
    ]"#;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cstat_returns_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026_returns.json"), FILE).unwrap();
        dir
    }

    fn rows(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let raw = std::fs::read_to_string(dir.join("2026_returns.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn granted_flips_only_the_cohort_and_keeps_unmodelled_fields() {
        let dir = scratch("granted");
        let r = resolve_reason(&dir, "injunction", Resolution::Granted, "RESOLVED.").unwrap();
        assert_eq!(r[0].matched, 2);
        let out = rows(&dir);
        assert_eq!(out.len(), 3, "granted must not remove rows");
        for v in &out {
            let granted = v["status"] == "granted";
            assert!(granted, "{} should be granted", v["name"]);
        }
        // The row outside the cohort keeps its original note untouched.
        assert_eq!(out[2]["note"], "n3");
        assert!(out[0]["note"].as_str().unwrap().ends_with("RESOLVED."));
        // A field the Rust struct does not model survives the round-trip;
        // deserializing into `PlayerReturn` would have dropped it.
        assert_eq!(out[0]["curator_only"], "keep me");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn granted_rewrite_touches_only_the_changed_fields() {
        // Reviewability is the whole reason `resolve_reason` stops before
        // loading, so the rewrite must not churn the file. Round-tripping
        // through `serde_json::to_string_pretty` reorders every key
        // alphabetically and turns a two-field edit into a whole-file diff.
        //
        // Seeded from the canonical rendering, not the raw fixture, so the
        // comparison measures the resolve's churn rather than the fixture's
        // hand-written whitespace.
        let dir = scratch("stable");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(FILE).unwrap();
        let before = render_returns(&parsed).unwrap();
        std::fs::write(dir.join("2026_returns.json"), &before).unwrap();

        resolve_reason(&dir, "injunction", Resolution::Granted, "R.").unwrap();
        let after = std::fs::read_to_string(dir.join("2026_returns.json")).unwrap();

        let lines: Vec<&str> = before.lines().collect();
        let changed: Vec<&str> = after
            .lines()
            .filter(|l| !lines.contains(l))
            .map(str::trim)
            .collect();
        assert!(
            !changed.is_empty(),
            "the resolve must actually change something"
        );
        assert!(
            changed
                .iter()
                .all(|l| l.starts_with("\"status\"") || l.starts_with("\"note\"")),
            "only status and note may move; got {changed:?}"
        );
        // Key order has to be read off the TEXT: parsing back through
        // `serde_json::Value` sorts the keys, which is the very behaviour
        // `render_returns` exists to work around.
        let first_key = after
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with('"'))
            .unwrap_or_default();
        assert!(
            first_key.starts_with("\"name\""),
            "identity key must lead each row, got {first_key:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolving_twice_is_a_no_op() {
        // Re-running is the obvious reaction to an ambiguous first run, so it
        // must not stamp the resolution note a second time.
        let dir = scratch("twice");
        let first = resolve_reason(&dir, "injunction", Resolution::Granted, "R.").unwrap();
        assert_eq!(first[0].matched, 2);
        let after_one = std::fs::read_to_string(dir.join("2026_returns.json")).unwrap();

        let second = resolve_reason(&dir, "injunction", Resolution::Granted, "R.").unwrap();
        assert!(second.is_empty(), "second run must report nothing to do");
        assert_eq!(
            std::fs::read_to_string(dir.join("2026_returns.json")).unwrap(),
            after_one,
            "second run must not touch the file"
        );
        assert_eq!(
            rows(&dir)[0]["note"]
                .as_str()
                .unwrap()
                .matches("R.")
                .count(),
            1,
            "the resolution note must be stamped exactly once"
        );

        // Departed is idempotent for the other reason: the rows are gone.
        let third = resolve_reason(&dir, "injunction", Resolution::Departed, "").unwrap();
        assert_eq!(third[0].matched, 2);
        assert!(
            resolve_reason(&dir, "injunction", Resolution::Departed, "")
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn departed_removes_only_the_cohort() {
        let dir = scratch("departed");
        let r = resolve_reason(&dir, "injunction", Resolution::Departed, "unused").unwrap();
        assert_eq!(r[0].matched, 2);
        let out = rows(&dir);
        assert_eq!(out.len(), 1, "the two injunction rows must be gone");
        assert_eq!(out[0]["name"], "C Three");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_reason_nobody_carries_rewrites_nothing() {
        let dir = scratch("nomatch");
        let before = std::fs::read_to_string(dir.join("2026_returns.json")).unwrap();
        let r = resolve_reason(&dir, "medical", Resolution::Departed, "unused").unwrap();
        assert!(r.is_empty());
        // Byte-identical: a no-op must not reformat the curator's file.
        assert_eq!(
            std::fs::read_to_string(dir.join("2026_returns.json")).unwrap(),
            before
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
