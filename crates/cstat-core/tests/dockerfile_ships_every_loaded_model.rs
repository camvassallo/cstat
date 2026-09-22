//! Every model artifact the serve path opens must be in the Dockerfile.
//!
//! The image copies `training/models/*` **by name**, not by directory. So an
//! artifact can be trained, committed, allowlisted in `.gitignore`, validated
//! at boot and green in CI, and still be absent from the deployed container —
//! at which point `Predictor::load` fails with "File at
//! training/models/X.onnx does not exist" and the API does not start. Every
//! local check passes, because locally the file is right there.
//!
//! That is what shipping `roster_adjd_model.onnx` (#378/#379) did: the PR was
//! green on all six CI jobs and took prod's API down on deploy.
//!
//! This closes the gap by reading both sides of the contract from source:
//! every `model_dir.join("…")` in `inference.rs`, checked against the
//! Dockerfile's COPY list. It needs no database, no model files and no
//! network — it is a grep with an opinion, which is the point: it has to run
//! in the same CI job that was green while the image was broken.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/cstat-core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cstat-core has a grandparent")
        .to_path_buf()
}

/// Artifact names `Predictor::load` passes to `model_dir.join(...)`.
///
/// Parsed rather than hand-listed: a hand-list is the same kind of thing as
/// the Dockerfile's COPY block, and would go stale for the same reason.
fn loaded_artifacts(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (idx, _) in src.match_indices("model_dir.join(\"") {
        let rest = &src[idx + "model_dir.join(\"".len()..];
        let Some(end) = rest.find('"') else { continue };
        out.insert(rest[..end].to_string());
    }
    out
}

/// Paths under `training/models/` the Dockerfile copies into the image.
fn shipped_artifacts(dockerfile: &str) -> BTreeSet<String> {
    dockerfile
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with("training/models/"))
        })
        .filter_map(|p| p.strip_prefix("training/models/").map(str::to_string))
        .filter(|p| !p.is_empty() && p != "/")
        .collect()
}

#[test]
fn dockerfile_ships_every_model_the_predictor_loads() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("crates/cstat-core/src/inference.rs"))
        .expect("read inference.rs");
    let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).expect("read Dockerfile");

    let loaded = loaded_artifacts(&src);
    let shipped = shipped_artifacts(&dockerfile);

    // Sanity: if either parse silently returns nothing, the test would pass
    // vacuously — which is the failure mode it exists to prevent.
    assert!(
        loaded.len() >= 10,
        "parsed only {} model_dir.join(...) names from inference.rs — the parse broke, \
         not the Dockerfile",
        loaded.len()
    );
    assert!(
        shipped.len() >= 10,
        "parsed only {} training/models/ paths from the Dockerfile — the parse broke",
        shipped.len()
    );

    let missing: Vec<&String> = loaded.difference(&shipped).collect();
    assert!(
        missing.is_empty(),
        "these artifacts are loaded by Predictor::load but NOT copied into the image, so the \
         API will fail to boot on deploy with \"does not exist\" while every local check \
         passes: {missing:?}\n\nAdd them to the `COPY training/models/...` block in the \
         Dockerfile."
    );
}

/// The mirror direction: an artifact in the image that nothing loads is dead
/// weight, and usually means a model was removed from the serve path without
/// being removed from the deploy.
///
/// A warning rather than a failure — `roster_model.onnx` is deliberately
/// shipped and deliberately not loaded at boot (it is materialized lazily by
/// `projections-backtest`), so a hard assert here would be wrong today.
#[test]
fn image_carries_no_unexplained_model() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("crates/cstat-core/src/inference.rs"))
        .expect("read inference.rs");
    let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).expect("read Dockerfile");

    // Known-good extras, each shipped for a reason that is not
    // `Predictor::load` opening it by name:
    //   * the box-score roster model is materialized lazily on first
    //     `predict_adj_em` (only `projections-backtest` reaches it) and its
    //     meta is validated there, not at boot;
    //   * the game-model metas are the documented wire-lock for the feature
    //     order in `features.rs` / `inference.rs` — shipped so the running
    //     image carries the contract its binary was built against.
    const EXPECTED_EXTRAS: &[&str] = &[
        "roster_model.onnx",
        "roster_model_meta.json",
        "model_meta.json",
        "pit_model_meta.json",
    ];

    let loaded = loaded_artifacts(&src);
    let extras: Vec<String> = shipped_artifacts(&dockerfile)
        .into_iter()
        .filter(|p| !loaded.contains(p) && !EXPECTED_EXTRAS.contains(&p.as_str()))
        .collect();

    assert!(
        extras.is_empty(),
        "the image copies model artifacts nothing in inference.rs loads: {extras:?}. \
         If that is deliberate, add them to EXPECTED_EXTRAS with the reason; if it is \
         leftover, drop them from the Dockerfile."
    );
}
