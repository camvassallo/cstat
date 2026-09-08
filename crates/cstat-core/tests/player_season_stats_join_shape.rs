//! Guard: no `player_season_stats` join may key on `player_id` without also
//! constraining `team_id`.
//!
//! `player_season_stats` is UNIQUE on `(player_id, team_id, season)`, **not**
//! `(player_id, season)`. A player with two stints in one season therefore has
//! two rows, and `... JOIN player_season_stats x ON x.player_id = <expr> AND
//! x.season = <expr>` multiplies every row it touches — silently, since
//! nothing about the result's type or shape is wrong afterwards.
//!
//! This is the sibling of `torvik_join_shape.rs`, and it exists for the same
//! reason that one does: the fault kept being re-introduced by new call sites
//! copying old ones. #319 collapsed the `torvik_player_stats` join in
//! `trajectory.rs` and left the `player_season_stats` join one line below it
//! untouched, because nobody was looking for a second fan-out on the same
//! query. #331 found it there plus three more places.
//!
//! **A join carrying `team_id` is exempt** — that closes the key. The three
//! archetype-distribution queries in `queries.rs` do exactly this and are the
//! pattern to copy.
//!
//! Escape hatch for a site that is safe some other way — an aggregate that
//! absorbs the duplication, typically — is the marker
//! `ALLOW_PSS_PLAYER_ID_JOIN`, in the ON clause or in the comment block
//! immediately above the join. Both, because this repo consistently explains a
//! join *above* it, and a marker you have to place unnaturally is one people
//! work around instead of using.
//!
//! **What this does NOT cover:** `FROM player_season_stats` with the keys in a
//! `WHERE`. That is the shape the two `trajectory.rs` queries had, and it is
//! deliberately out of scope — legitimate season-wide aggregate reads use it
//! constantly (`AVG(ppg) FROM player_season_stats WHERE season = $1`), so
//! flagging it would be noise rather than signal. Collapsing subqueries there
//! is a review question, not a mechanical one.
//!
//! Not DB-gated: it reads files, so it runs in CI like an ordinary test.

use std::path::{Path, PathBuf};

const ALLOW_MARKER: &str = "ALLOW_PSS_PLAYER_ID_JOIN";

/// How far past a `JOIN player_season_stats` line to look for the full ON
/// clause. Three is not arbitrary: the archetype-distribution joins in
/// `queries.rs` put `player_id`, `season` and `team_id` on three separate
/// continuation lines, so a shorter window would read the safe spelling as a
/// violation.
const ON_CLAUSE_LOOKAHEAD: usize = 4;

/// How far above the join to look for the allow marker, so it can live in the
/// explanatory comment where it belongs rather than wedged into the SQL.
const MARKER_LOOKBEHIND: usize = 8;

/// Keywords that end a join's ON clause, so the window cannot run into the
/// next join and judge this one by its neighbour's key.
const CLAUSE_ENDS: [&str; 11] = [
    "JOIN ",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "OUTER JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "WHERE ",
    "GROUP BY",
    "ORDER BY",
    "LIMIT ",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/cstat-core")
        .to_path_buf()
}

/// Only `crates/*/src`. Test files are excluded on purpose: some hold the bare
/// spelling as a deliberate control to prove a collapse removes duplicates
/// rather than rows.
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

/// Collect a join's own ON clause, stopping before the next clause.
fn on_clause_window(lines: &[&str], i: usize) -> String {
    let mut window = String::from(lines[i].trim_start());
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
    }
    window
}

fn bare_joins_in(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        // The table joined directly — not `JOIN (SELECT ... FROM
        // player_season_stats ...)`, where the predicate filters the subquery
        // against the outer row instead of fanning out.
        if !trimmed.contains("JOIN player_season_stats") {
            continue;
        }
        let window = on_clause_window(&lines, i);
        let preceding = lines[i.saturating_sub(MARKER_LOOKBEHIND)..i].join("\n");
        if window.contains(ALLOW_MARKER) || preceding.contains(ALLOW_MARKER) {
            continue;
        }
        let keys_on_player = window.contains(".player_id =") || window.contains(".player_id=");
        let closes_with_team = window.contains("team_id");
        if keys_on_player && !closes_with_team {
            hits.push((i + 1, trimmed.to_string()));
        }
    }
    hits
}

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
        for (line_no, line) in bare_joins_in(&text) {
            hits.push((path.clone(), line_no, line));
        }
    }
    hits
}

#[test]
fn no_pss_join_keys_on_player_id_without_team_id() {
    let hits = bare_player_id_joins(&repo_root());
    assert!(
        hits.is_empty(),
        "`player_season_stats` joined on `player_id` without `team_id` — it is \
         UNIQUE on (player_id, team_id, season), so this multiplies every row it \
         touches whenever a player has two stints in a season, silently. Add \
         `AND x.team_id = <expr>` (see the archetype-distribution queries in \
         `queries.rs`), or collapse with `LEFT JOIN LATERAL (SELECT * FROM \
         player_season_stats s WHERE ... ORDER BY s.games_played DESC NULLS LAST, \
         s.minutes_per_game DESC NULLS LAST, s.team_id LIMIT 1)`. If the site is \
         genuinely safe — an aggregate that absorbs the duplication — mark it \
         `{ALLOW_MARKER}`.\n{}",
        hits.iter()
            .map(|(p, n, l)| format!("  {}:{n}  {l}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// The guard has to actually fire, or a refactor that breaks its matching
/// leaves a green test protecting nothing. This is the pre-#331 spelling from
/// `trajectory.rs`.
#[test]
fn guard_fires_on_the_pre_fix_spelling() {
    let bare = "        LEFT JOIN player_season_stats pss_nm1\n            \
                ON pss_nm1.player_id = tps_nm1.player_id AND pss_nm1.season = pss.season - 1\n        \
                WHERE pss.player_id = $1";
    assert_eq!(
        bare_joins_in(bare).len(),
        1,
        "guard no longer detects the join shape it exists to catch"
    );
}

/// ...and must not fire on the two spellings that are safe, or the escape
/// hatch becomes the only way to ship and the guard gets switched off.
#[test]
fn guard_accepts_team_id_and_lateral_spellings() {
    let with_team_id = "            LEFT JOIN player_season_stats pss\n                \
                        ON pss.player_id = p.id\n               \
                        AND pss.season = pa.season\n               \
                        AND pss.team_id = p.team_id";
    assert!(
        bare_joins_in(with_team_id).is_empty(),
        "a join that closes the key with team_id must pass"
    );

    let lateral = "        LEFT JOIN LATERAL (\n            \
                   SELECT * FROM player_season_stats s\n            \
                   WHERE s.player_id = tps_nm1.player_id AND s.season = pss.season - 1\n            \
                   ORDER BY s.games_played DESC NULLS LAST\n            \
                   LIMIT 1\n        ) pss_nm1 ON TRUE";
    assert!(
        bare_joins_in(lateral).is_empty(),
        "a collapsing LATERAL subquery must pass"
    );
}

/// The allow marker must work from the comment above the join, which is where
/// this repo puts the explanation. If only the in-clause position worked, the
/// marker would be awkward enough that people would drop the guard instead.
#[test]
fn allow_marker_is_honoured_from_the_comment_above() {
    let marked = "        -- ALLOW_PSS_PLAYER_ID_JOIN: read only through bool_or() under\n                          -- a GROUP BY, so the fan-out is absorbed.\n                          LEFT JOIN player_season_stats pss\n                                 ON pss.player_id = p.id AND pss.season = p.season";
    assert!(
        bare_joins_in(marked).is_empty(),
        "allow marker in the preceding comment block must suppress the finding"
    );
}
