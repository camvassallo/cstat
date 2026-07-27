//! Model-artifact provenance for the Layer 3 derived products (issue #238).
//!
//! #223/#237 gave every trained node an `input_provenance` stamp in its model
//! meta, closing Layer 0 -> Layer 1 and Layer 1 -> Layer 2. Layer 3 has no meta
//! to stamp: `team_preseason_projection`, the per-team backtest dumps, and
//! `coach_season_cae` are produced by CLI runs that write rows and files. This
//! module builds the fingerprint those runs record instead — *which model
//! artifact produced this* — into the `artifact_provenance` table (migration
//! 047) and into the backtest dump envelope.
//!
//! ## Why the bytes, and not just the meta
//!
//! For a model with a meta, `input_provenance` already says what it trained on,
//! and that is the more meaningful comparison — it can be re-evaluated against
//! the live database. But `projections-backtest` scores with the per-season
//! LOSO models in `models/roster_impact_loso/`, and **those carry no meta at
//! all**. They are gitignored, so they never show up in `git status`, and
//! `roster_adjo` exports no per-season ONNX whatsoever. Their only stable
//! identity is their content.
//!
//! Hashing content is only meaningful because of #222: ONNX export is
//! byte-stable (`test_frame_determinism.py::test_onnx_export_is_byte_stable`),
//! so a no-op retrain over unchanged data reproduces the same digest rather
//! than looking like drift. Before that work this fingerprint would have
//! changed on every rerun and been worthless.
//!
//! Deliberately **not** hashed: file mtime, size alone, or path. Timestamps are
//! excluded throughout the #223 design for exactly the reason above — a
//! deterministic re-run must be provable as a no-op, and stamping the clock
//! would flag it as drift.

use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The name a model artifact is recorded under. Matches the ONNX file stem so
/// `models/<stem>.onnx` and `models/<stem>_meta.json` both resolve from it.
pub const ROSTER_IMPACT: &str = "roster_impact_model";
pub const ROSTER_ADJO: &str = "roster_adjo_model";
pub const TRAJECTORY_MEAN: &str = "trajectory_mean_model";
pub const FRESHMAN_MEAN: &str = "freshman_mean_model";

#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

fn sha256_file(path: &Path) -> Result<String, ProvenanceError> {
    let bytes = std::fs::read(path).map_err(|e| ProvenanceError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fingerprint one model: the ONNX bytes plus whatever provenance its meta
/// carries.
///
/// A missing meta, or a meta with no `input_provenance`, is **not** an error
/// here. Every model on disk is unstamped until its next retrain (#237 landed
/// the stamping, not the retrain), and Layer 3 must keep working meanwhile —
/// this is report-only tooling, not a boot guard. The absence is recorded
/// honestly as a null so `check_provenance.py` can say "cannot speak for this"
/// rather than silently implying the artifact is current.
pub fn model_provenance(model_dir: &Path, stem: &str) -> Result<Value, ProvenanceError> {
    let onnx = model_dir.join(format!("{stem}.onnx"));
    let meta_path = model_dir.join(format!("{stem}_meta.json"));

    let mut entry = json!({ "onnx_sha256": sha256_file(&onnx)? });

    if meta_path.exists() {
        let text = std::fs::read_to_string(&meta_path).map_err(|e| ProvenanceError::Io {
            path: meta_path.display().to_string(),
            source: e,
        })?;
        let meta: Value = serde_json::from_str(&text).map_err(|e| ProvenanceError::Parse {
            path: meta_path.display().to_string(),
            source: e,
        })?;
        // Copied verbatim, so a Layer 3 row can be re-evaluated against the
        // live database by the same code path that checks Layer 1 and Layer 2.
        entry["input_provenance"] = meta.get("input_provenance").cloned().unwrap_or(Value::Null);
        entry["oof_provenance"] = meta.get("oof_provenance").cloned().unwrap_or(Value::Null);
    }
    Ok(entry)
}

/// Fingerprint the whole gitignored LOSO set as one unit.
///
/// The backtest scores each target season with its own leave-one-season-out
/// model, so the honest question is not "is this one file current" but "is this
/// *set* the one the committed serving model was exported alongside". The set
/// digest is a SHA-256 over `filename:sha256` pairs in sorted filename order —
/// order-stable for the same reason the #223 SQL digests are, so two runs over
/// an unchanged directory agree.
///
/// Returns `null` when the directory is absent rather than erroring: these
/// files are regenerable diagnostic artifacts and their absence is a legitimate
/// state (a fresh clone has never run the trainer).
pub fn loso_set_provenance(model_dir: &Path) -> Result<Value, ProvenanceError> {
    let dir = model_dir.join("roster_impact_loso");
    if !dir.is_dir() {
        return Ok(Value::Null);
    }
    let read = std::fs::read_dir(&dir).map_err(|e| ProvenanceError::Io {
        path: dir.display().to_string(),
        source: e,
    })?;

    let mut files: Vec<(String, String)> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| ProvenanceError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("onnx") {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        files.push((name, sha256_file(&path)?));
    }
    files.sort();

    let mut hasher = Sha256::new();
    for (name, digest) in &files {
        hasher.update(name.as_bytes());
        hasher.update(b":");
        hasher.update(digest.as_bytes());
        hasher.update(b",");
    }
    Ok(json!({
        "n_models": files.len(),
        "set_digest": format!("{:x}", hasher.finalize()),
        "models": files.into_iter().map(|(n, d)| json!({"file": n, "sha256": d}))
            .collect::<Vec<_>>(),
    }))
}

/// Assemble the `provenance` JSON a Layer 3 producer records.
///
/// `produced_by` names the CLI command, so a row read months later says what to
/// re-run rather than requiring someone to work it out from the artifact name.
pub fn layer3_provenance(
    model_dir: &Path,
    produced_by: &str,
    stems: &[&str],
    include_loso: bool,
) -> Result<Value, ProvenanceError> {
    let mut models = serde_json::Map::new();
    for stem in stems {
        models.insert((*stem).to_string(), model_provenance(model_dir, stem)?);
    }
    let mut out = json!({
        "produced_by": produced_by,
        "models": Value::Object(models),
    });
    if include_loso {
        out["roster_impact_loso"] = loso_set_provenance(model_dir)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cstat_prov_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn model_provenance_reads_the_meta_stamp() {
        let d = tmpdir("meta");
        fs::write(d.join("m.onnx"), b"onnx-bytes").unwrap();
        fs::write(
            d.join("m_meta.json"),
            r#"{"input_provenance":{"a":{"n_rows":1,"digest":"x"}},"oof_provenance":{"b":1}}"#,
        )
        .unwrap();

        let p = model_provenance(&d, "m").unwrap();
        assert_eq!(p["input_provenance"]["a"]["digest"], "x");
        assert_eq!(p["oof_provenance"]["b"], 1);
        assert!(p["onnx_sha256"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn an_unstamped_meta_is_recorded_as_null_not_an_error() {
        // Every model on disk is unstamped until its next retrain, and Layer 3
        // is report-only tooling. Failing here would make the projection
        // pipeline refuse to run over a missing diagnostic stamp.
        let d = tmpdir("unstamped");
        fs::write(d.join("m.onnx"), b"onnx-bytes").unwrap();
        fs::write(d.join("m_meta.json"), r#"{"model":"m"}"#).unwrap();

        let p = model_provenance(&d, "m").unwrap();
        assert!(p["input_provenance"].is_null());
        assert!(p["oof_provenance"].is_null());
    }

    #[test]
    fn identical_bytes_hash_identically_across_runs() {
        // The property #222 buys us: a no-op retrain must be provable as a
        // no-op. If this ever fails, every Layer 3 row looks stale on every run
        // and the report becomes noise.
        let d = tmpdir("stable");
        fs::write(d.join("m.onnx"), b"same-bytes").unwrap();
        assert_eq!(
            model_provenance(&d, "m").unwrap()["onnx_sha256"],
            model_provenance(&d, "m").unwrap()["onnx_sha256"],
        );
    }

    #[test]
    fn loso_set_digest_ignores_directory_order_but_not_content() {
        let d = tmpdir("loso");
        let loso = d.join("roster_impact_loso");
        fs::create_dir_all(&loso).unwrap();
        fs::write(loso.join("roster_impact_model_2025.onnx"), b"a").unwrap();
        fs::write(loso.join("roster_impact_model_2026.onnx"), b"b").unwrap();

        let first = loso_set_provenance(&d).unwrap();
        assert_eq!(first["n_models"], 2);

        // Rewriting one file with different content must move the set digest —
        // this is the case that catches a backtest run against a LOSO set from
        // a different frame than the committed serving model.
        fs::write(loso.join("roster_impact_model_2026.onnx"), b"CHANGED").unwrap();
        let second = loso_set_provenance(&d).unwrap();
        assert_ne!(first["set_digest"], second["set_digest"]);
    }

    #[test]
    fn a_missing_loso_dir_is_null_not_an_error() {
        // Gitignored, regenerable, and absent on a fresh clone. Erroring would
        // make `compute-projections` fail over a diagnostic artifact it does
        // not use.
        let d = tmpdir("noloso");
        assert!(loso_set_provenance(&d).unwrap().is_null());
    }
}
