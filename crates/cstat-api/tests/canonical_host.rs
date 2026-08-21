//! Guard: no source file may hardcode the legacy `campom.org` origin.
//!
//! The site answers on two hosts. `camalytics.org` is canonical; `campom.org`
//! stays open indefinitely so the links, social cards, and index entries built
//! under the old brand keep working (`docs/domain_migration.md`). Because there
//! is deliberately no 301 between them, the ONLY thing marking them as one site
//! is that every absolute URL we emit names the canonical origin — so a single
//! stray `https://campom.org/...` in a served surface silently re-splits the
//! two hosts in Google's index, and does it quietly enough to survive a review.
//!
//! Scoped to code and served config. Prose (ROADMAP, docs/, the preview script)
//! is exempt: those record real history where naming the old domain is correct.
//!
//! What trips it is the legacy host in URL or string-literal position — the
//! shapes that actually reach a crawler. Naming the old domain in a comment is
//! fine and frequently necessary, since the two-host arrangement is exactly what
//! the surrounding code has to explain.
//!
//! Escape hatch for a deliberate literal — a "formerly campom.org" link in the
//! UI, say — is the marker `ALLOW_LEGACY_HOST` on the same line.

use std::path::{Path, PathBuf};

const LEGACY_HOST: &str = "campom.org";
const ALLOW_MARKER: &str = "ALLOW_LEGACY_HOST";

/// The legacy host in a position that would actually be served: right after a
/// scheme separator, or opening a quoted string literal. Backticks are
/// deliberately NOT in this list — they mark a code span in a doc comment, which
/// is prose about the arrangement, not a URL that ships.
fn is_hardcoded_use(line: &str) -> bool {
    ["//", "\"", "\'"]
        .iter()
        .any(|p| line.contains(&format!("{p}{LEGACY_HOST}")))
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/cstat-api.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("manifest dir has a grandparent")
        .to_path_buf()
}

/// Directories that never hold hand-written source.
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | "node_modules" | "dist" | ".git" | "eval_history" | "__pycache__"
    )
}

fn is_scanned_file(path: &Path) -> bool {
    if path.file_name().and_then(|n| n.to_str()) == Some("robots.txt") {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "ts" | "tsx" | "html")
    )
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !is_skipped_dir(&name) {
                collect(&path, out);
            }
        } else if is_scanned_file(&path) {
            out.push(path);
        }
    }
}

#[test]
fn no_source_file_hardcodes_the_legacy_host() {
    let root = repo_root();
    let mut files = Vec::new();
    for sub in ["crates", "web"] {
        collect(&root.join(sub), &mut files);
    }
    // Root-level config that names an origin but has no scanned extension.
    files.push(root.join(".env.example"));
    assert!(
        files.len() > 20,
        "guard scanned only {} files from {} — the walk is broken, not the repo clean",
        files.len(),
        root.display()
    );

    let mut offenders = Vec::new();
    for path in &files {
        // This file names the host it forbids, by construction.
        if path.file_name().and_then(|n| n.to_str()) == Some("canonical_host.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if is_hardcoded_use(line) && !line.contains(ALLOW_MARKER) {
                let rel = path.strip_prefix(&root).unwrap_or(path);
                offenders.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "hardcoded legacy host `{LEGACY_HOST}` found. Absolute URLs must name the \
         canonical origin (`routes::sitemap::CANONICAL_ORIGIN`) so the two serving \
         hosts stay one site in the index — the old host is kept alive, not \
         advertised. If the literal is deliberate, add `{ALLOW_MARKER}` on the \
         line.\n  {}",
        offenders.join("\n  ")
    );
}
