//! Guard: no `torvik_player_stats` join may key on `player_id` without
//! collapsing to one profile per `(player, season)`.
//!
//! `torvik_player_stats` is UNIQUE on `(torvik_pid, season)`, **not** on
//! `(player_id, season)`. A few hundred pairs carry two or three profiles for
//! one human, so `... torvik_player_stats x ON x.player_id = <expr>` multiplies
//! every row it touches — and does it silently, since neither a row count nor
//! a type is wrong afterwards.
//!
//! The reason this is a source scan rather than only a data test is the shape
//! of the arc it closes. The same join was filed four times over four months:
//! #307 (three `queries.rs` read paths), #311 (the feature builders, where it
//! reached the served model), #312 (the four route-level display lists) and
//! #257 before them. Every one was a NEW call site copying an existing one,
//! and every data test written for the previous round kept passing, because a
//! test that reproduces a query cannot know about a query nobody reproduced.
//! Reading the source is what generalizes.
//!
//! Joins on `torvik_pid` are exempt and deliberately common — that IS the
//! unique key, so `tps_nm1.torvik_pid = tps.torvik_pid` (trajectory's N-1
//! chain), `tps.torvik_pid = tpgs.pid` (the point-in-time builder) and
//! `t2.torvik_pid = t1.torvik_pid` (cross-season resolution) cannot fan out.
//! The scan only objects to `player_id` in the ON clause.
//!
//! Escape hatch for a join that is genuinely safe some other way — a
//! subquery that already aggregates, say — is the marker
//! `ALLOW_TORVIK_PLAYER_ID_JOIN` inside the SQL string, on any line of the
//! matched join.
//!
//! Not DB-gated: it reads files, so it runs in CI like an ordinary test.

use std::path::{Path, PathBuf};

const ALLOW_MARKER: &str = "ALLOW_TORVIK_PLAYER_ID_JOIN";

/// How far past a `JOIN torvik_player_stats` line to look for the ON clause.
/// The joins in this repo put it on the same line or the next one; the comment
/// blocks that explain them sit *before* the JOIN, not between it and its ON.
const ON_CLAUSE_LOOKAHEAD: usize = 3;

/// Keywords that end a join's ON clause. Without this the lookahead reads
/// straight into the NEXT join and judges this one by its neighbour's key —
/// `trajectory.rs` joins Torvik on `torvik_pid` and then joins
/// `player_season_stats` on `player_id` immediately below, which is exactly
/// the safe-then-unrelated pair that a naive window calls a violation.
const CLAUSE_ENDS: [&str; 6] = [
    "JOIN ", "WHERE ", "GROUP BY", "ORDER BY", "LIMIT ", "UNION ",
];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/cstat-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/cstat-core")
        .to_path_buf()
}

/// Only `crates/*/src`. Test files are excluded on purpose: several of them
/// hold the bare spelling as a deliberate control, running it beside the
/// collapsed one to prove the collapse removes duplicates rather than rows.
/// Those are the fan-out's guards, not instances of it.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Bare `torvik_player_stats` joins keyed on `player_id`, as
/// `(path, line number, the join line)`.
///
/// A collapsed site opens with `JOIN (` or `JOIN LATERAL (` and names the
/// table *inside* the subquery, where the `player_id` predicate is a filter
/// against the outer row rather than a fan-out. So matching on the table name
/// sitting directly after `JOIN` is what separates the two spellings, and it
/// is why the four fixes in #312 do not trip this.
fn bare_player_id_joins(root: &Path) -> Vec<(PathBuf, usize, String)> {
    let mut files = Vec::new();
    for crate_dir in std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
    {
        rust_sources(&crate_dir.path().join("src"), &mut files);
    }
    files.sort();

    let mut hits = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            // `JOIN torvik_player_stats <alias>` — the table joined directly,
            // not a subquery that happens to read from it.
            if !trimmed.contains("JOIN torvik_player_stats") {
                continue;
            }
            // This join's own ON clause and nothing past it.
            let mut window = String::from(trimmed);
            for next in lines
                .iter()
                .skip(i + 1)
                .take(ON_CLAUSE_LOOKAHEAD)
                .map(|l| l.trim_start())
            {
                if CLAUSE_ENDS.iter().any(|k| next.starts_with(k)) {
                    break;
                }
                window.push('\n');
                window.push_str(next);
                if next.contains(" ON ") || next.starts_with("ON ") {
                    break;
                }
            }
            if window.contains(ALLOW_MARKER) {
                continue;
            }
            // The fan-out is specifically an ON clause equating the *cstat*
            // player id. A join on `torvik_pid` is on the unique key.
            if window.contains(".player_id =") || window.contains(".player_id=") {
                hits.push((path.clone(), i + 1, trimmed.to_string()));
            }
        }
    }
    hits
}

#[test]
fn no_torvik_join_keys_on_player_id_without_collapsing() {
    let hits = bare_player_id_joins(&repo_root());
    assert!(
        hits.is_empty(),
        "`torvik_player_stats` joined on `player_id` without collapsing to one \
         profile per (player, season) — this multiplies every row it touches, \
         silently. Use the `DISTINCT ON (player_id, season)` subquery (season-wide \
         queries) or `LEFT JOIN LATERAL ... ORDER BY torvik_pid LIMIT 1` \
         (single-team / small-id-list ones); see `queries::get_team_roster`. \
         If a site is genuinely safe some other way, mark it \
         `{ALLOW_MARKER}`.\n{}",
        hits.iter()
            .map(|(p, n, l)| format!("  {}:{n}  {l}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// The guard has to actually fire, or a future refactor that breaks its
/// matching would leave a green test protecting nothing. Feed it the pre-#312
/// spelling of `transfers.rs` and require a hit.
#[test]
fn the_shape_guard_detects_a_bare_join() {
    let dir = std::env::temp_dir().join("cstat_torvik_join_shape_probe");
    let src = dir.join("crates").join("probe").join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("route.rs");
    std::fs::write(
        &file,
        "let sql = r#\"\n\
         FROM player_season_stats pss\n\
         LEFT JOIN torvik_player_stats tps\n\
         ON tps.player_id = p.id AND tps.season = pss.season\n\
         \"#;\n",
    )
    .unwrap();
    let hits = bare_player_id_joins(&dir);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        hits.len(),
        1,
        "the guard no longer recognizes the join shape it exists to catch",
    );
}

/// And it has to stay quiet on the two spellings the repo uses on purpose:
/// a join on the unique key, and the collapsed subquery.
#[test]
fn the_shape_guard_accepts_the_safe_spellings() {
    let dir = std::env::temp_dir().join("cstat_torvik_join_shape_safe");
    let src = dir.join("crates").join("probe").join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("safe.rs"),
        "let sql = r#\"\n\
         LEFT JOIN torvik_player_stats tps_nm1\n\
         ON tps_nm1.torvik_pid = tps.torvik_pid AND tps_nm1.season = pss.season - 1\n\
         LEFT JOIN (\n\
         SELECT DISTINCT ON (player_id, season) * FROM torvik_player_stats\n\
         WHERE player_id IS NOT NULL ORDER BY player_id, season, torvik_pid\n\
         ) tps ON tps.player_id = p.id AND tps.season = pss.season\n\
         LEFT JOIN LATERAL (\n\
         SELECT * FROM torvik_player_stats t\n\
         WHERE t.player_id = p.id AND t.season = $1 ORDER BY t.torvik_pid LIMIT 1\n\
         ) tps2 ON TRUE\n\
         \"#;\n",
    )
    .unwrap();
    let hits = bare_player_id_joins(&dir);
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        hits.is_empty(),
        "the guard rejects a spelling the repo uses deliberately:\n{}",
        hits.iter()
            .map(|(p, n, l)| format!("  {}:{n}  {l}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
