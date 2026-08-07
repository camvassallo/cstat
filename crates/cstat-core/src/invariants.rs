//! Post-compute data-quality invariants.
//!
//! Structural "the pipeline did its job" checks that must hold after
//! `compute_all` for a season — distinct from unit tests in that they run
//! against a live database and are meant to be consumed programmatically:
//! the `simulate` replay harness runs them after every simulated nightly
//! (M4), and the nightly quality gates will reuse them to alert instead of
//! silently serving corrupt derived stats (M5).
//!
//! Each check returns a violation count plus a small sample of offending
//! ids for the log/alert message. Checks are season-scoped, cheap relative
//! to `compute_all`, and side-effect free.

use sqlx::{PgPool, Row};

/// How bad a violated check is.
///
/// `Error` = the pipeline produced something wrong from the data it had —
/// always actionable, fails a simulate run. `Warning` = the *source* data has
/// a hole the pipeline faithfully reflects (e.g. NatStat never delivered a
/// team's box rows for a final game — the main DB carries ~26 such games
/// across 2015–2026, and the 2020 CSV export lacks statlines for some games
/// outright). Worth surfacing, but failing every run on a static source gap
/// is alarm fatigue, not signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One failed invariant: `count` offending rows for `check`, with up to a
/// handful of identifying `samples` for the report.
#[derive(Debug, Clone)]
pub struct InvariantViolation {
    pub check: &'static str,
    pub severity: Severity,
    pub count: i64,
    pub samples: Vec<String>,
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.severity {
            Severity::Error => "",
            Severity::Warning => " [warning]",
        };
        write!(f, "{}{}: {} violation(s)", self.check, tag, self.count)?;
        if !self.samples.is_empty() {
            write!(f, " (e.g. {})", self.samples.join(", "))?;
        }
        Ok(())
    }
}

/// Run every season-scoped invariant; returns the violations found (empty =
/// healthy). Individual query errors propagate — a check that can't run is a
/// harness bug, not a data finding.
pub async fn check_season(
    pool: &PgPool,
    season: i32,
) -> Result<Vec<InvariantViolation>, sqlx::Error> {
    let mut violations = Vec::new();

    for check in [
        team_with_games_missing_adjem(pool, season).await?,
        completed_game_missing_team_stats(pool, season).await?,
        wl_record_mismatch(pool, season).await?,
        fully_swapped_games_remain(pool, season).await?,
        phantom_swapped_games_remain(pool, season).await?,
        pbp_present_but_lineups_empty(pool, season).await?,
        pbp_date_coverage_gap(pool, season).await?,
        torvik_rows_unlinked(pool, season).await?,
    ]
    .into_iter()
    .flatten()
    {
        violations.push(check);
    }

    Ok(violations)
}

/// Every team with at least one *solver-eligible* box score must come out of
/// compute with a `team_season_stats` row carrying a non-NULL AdjEM. The
/// eligibility filter mirrors `compute_adjusted_efficiency`'s input exactly
/// (resolved opponent, points/fga present, and estimated possessions > 0): a
/// team whose only games so far are vs non-D1 opponents (`opponent_id` NULL —
/// common in the season's first week) legitimately has no AdjEM yet and must
/// not trip the gate. A violation therefore means the solver skipped a team
/// it should have rated, and the rankings/predict surfaces would serve holes.
async fn team_with_games_missing_adjem(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT t.natstat_id
        FROM teams t
        WHERE t.season = $1
          AND EXISTS (SELECT 1 FROM team_game_stats tgs
                      WHERE tgs.team_id = t.id AND tgs.season = $1
                        AND tgs.opponent_id IS NOT NULL
                        AND tgs.points IS NOT NULL AND tgs.fga IS NOT NULL
                        AND (tgs.fga - COALESCE(tgs.off_rebounds, 0)
                             + COALESCE(tgs.turnovers, 0)
                             + 0.44 * COALESCE(tgs.fta, 0)) > 0)
          AND NOT EXISTS (SELECT 1 FROM team_season_stats tss
                          WHERE tss.team_id = t.id AND tss.season = $1
                            AND tss.adj_efficiency_margin IS NOT NULL)
        ORDER BY t.natstat_id
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "team_with_games_missing_adjem",
        Severity::Error,
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// A completed game between two resolved teams must have both sides'
/// `team_game_stats` rows — four factors / AdjEM / W-L all recompute from
/// them, so a missing side silently skews every derived team metric.
async fn completed_game_missing_team_stats(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT g.natstat_id
        FROM games g
        WHERE g.season = $1
          AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
          AND g.home_team_id IS NOT NULL AND g.away_team_id IS NOT NULL
          AND (SELECT COUNT(*) FROM team_game_stats tgs WHERE tgs.game_id = g.id) < 2
        ORDER BY g.natstat_id
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "completed_game_missing_team_stats",
        Severity::Warning,
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// `compute_derived_game_fields` unconditionally rebuilds W-L from
/// `team_game_stats.win`, so post-compute the two must agree exactly.
/// Drift means compute didn't finish (or a write raced it).
async fn wl_record_mismatch(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT t.natstat_id
        FROM team_season_stats tss
        JOIN teams t ON t.id = tss.team_id
        WHERE tss.season = $1
          AND (tss.wins <> (SELECT COUNT(*) FROM team_game_stats tgs
                            WHERE tgs.team_id = tss.team_id AND tgs.season = $1
                              AND tgs.win IS TRUE)
            OR tss.losses <> (SELECT COUNT(*) FROM team_game_stats tgs
                              WHERE tgs.team_id = tss.team_id AND tgs.season = $1
                                AND tgs.win IS FALSE))
        ORDER BY t.natstat_id
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "wl_record_mismatch",
        Severity::Error,
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// No fully-swapped game may survive compute — the same bidirectional
/// cross-tag detector `compute::correct_swapped_games` uses (issue #119),
/// season-scoped. `pub` so `tests/swapped_games.rs` asserts through this
/// exact query instead of carrying its own copy.
///
/// Season scoping mirrors `compute::correct_swapped_games` **exactly** — the
/// box rows' own `season` column (`player_game_stats.season` /
/// `team_game_stats.season`), not a `games.season` join. The invariant's job
/// is "did compute's repair leave a swap behind?", so it must select the same
/// game set compute operated on; a `games.season` anchor would (a) count
/// `DISTINCT team_id` across all seasons' box rows on a cross-season
/// game_id collision, breaking the `= 2` gate, and (b) diverge from what
/// compute actually processed.
pub async fn fully_swapped_games_remain(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        WITH gt AS (
            SELECT pgs.game_id, pgs.team_id AS labeled, pl.team_id AS real_team, COUNT(*) AS n
            FROM player_game_stats pgs
            JOIN players pl ON pl.id = pgs.player_id
            WHERE pgs.season = $2
            GROUP BY pgs.game_id, pgs.team_id, pl.team_id
        ),
        two_team AS (
            SELECT game_id FROM team_game_stats WHERE season = $2
            GROUP BY game_id HAVING COUNT(DISTINCT team_id) = 2
        ),
        sides AS (
            SELECT game_id, labeled,
                   SUM(n) AS tot,
                   SUM(n) FILTER (WHERE real_team IS DISTINCT FROM labeled) AS mis
            FROM gt
            WHERE game_id IN (SELECT game_id FROM two_team)
            GROUP BY game_id, labeled
        )
        SELECT g.natstat_id
        FROM sides s
        JOIN games g ON g.id = s.game_id
        GROUP BY s.game_id, g.natstat_id
        HAVING COUNT(*) = 2 AND MIN(s.mis::float8 / NULLIF(s.tot, 0)) >= $1
        ORDER BY g.natstat_id
        "#,
    )
    .bind(crate::compute::SWAPPED_GAME_MIN_CROSS_SHARE)
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "fully_swapped_games_remain",
        Severity::Error,
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// No phantom-swapped game may survive compute — the same gate
/// `compute::repair_phantom_swapped_games` uses (issue #140), season-scoped.
/// `pub` so `tests/swapped_games.rs` asserts through this exact query
/// instead of carrying its own copy.
///
/// Two robustness notes: game membership mirrors
/// `compute::repair_phantom_swapped_games` — the box rows' own `season`
/// column (see [`fully_swapped_games_remain`] for why matching compute's
/// scoping matters), and the opponent-roster resolution uses `IN` over the
/// game's *other* team ids rather than a scalar `=` subquery — a box row
/// labeled with a third team (neither of the game's two `team_game_stats`
/// sides) would make the scalar form return two rows and turn the whole
/// check into a Postgres error instead of a finding.
pub async fn phantom_swapped_games_remain(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        WITH np AS (
            SELECT p.id, p.season, p.team_id,
                   regexp_replace(lower(p.name),'[^a-z0-9]','','g') AS nn,
                   lower(regexp_replace(split_part(
                       regexp_replace(p.name,'( Jr\.?| Sr\.?| III| II| IV)$','','i'),' ',
                       array_length(string_to_array(
                           regexp_replace(p.name,'( Jr\.?| Sr\.?| III| II| IV)$','','i'),' '),1)),
                       '[^a-z0-9]','','g')) AS ln,
                   (SELECT count(*) FROM player_game_stats x WHERE x.player_id=p.id) AS gp
            FROM players p
            WHERE p.season = $2
        ),
        games2 AS (
            SELECT game_id FROM team_game_stats WHERE season = $2
            GROUP BY game_id HAVING count(DISTINCT team_id) = 2
        ),
        resolved AS (
            SELECT pgs.id AS pgs_id, pgs.game_id,
                   (SELECT EXISTS (
                       SELECT 1 FROM np r
                       WHERE r.gp > 1
                         AND r.team_id IN (SELECT tg.team_id FROM team_game_stats tg
                                           WHERE tg.game_id = pgs.game_id AND tg.team_id <> pgs.team_id)
                         AND (r.nn = np.nn OR r.ln = np.ln))) AS resolves
            FROM player_game_stats pgs
            JOIN np ON np.id = pgs.player_id
            WHERE np.gp = 1 AND pgs.game_id IN (SELECT game_id FROM games2)
        ),
        sides AS (
            SELECT pgs.game_id, pgs.team_id,
                   count(*) AS box,
                   count(*) FILTER (WHERE r.resolves) AS res
            FROM player_game_stats pgs
            LEFT JOIN resolved r ON r.pgs_id = pgs.id
            WHERE pgs.game_id IN (SELECT game_id FROM games2)
            GROUP BY pgs.game_id, pgs.team_id
        )
        SELECT g.natstat_id
        FROM sides s JOIN games g ON g.id = s.game_id
        GROUP BY s.game_id, g.natstat_id
        HAVING count(*) = 2 AND min(s.res::float8 / s.box) >= $1 AND min(s.res) >= 3
        ORDER BY g.natstat_id
        "#,
    )
    .bind(crate::compute::PHANTOM_SWAP_MIN_RESOLVE_SHARE)
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "phantom_swapped_games_remain",
        Severity::Error,
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// If a season has any `play_by_play`, `compute_pbp_lineups` must have produced
/// `lineup_aggregates` from it. An empty rollup despite present PBP means step 10
/// silently produced nothing — the exact failure this PR's prod-owned PBP path
/// must never hit. It cannot false-fire on a PBP-less prod (no PBP → no check),
/// nor in the offline `simulate`/replay harnesses (they seed no PBP fixtures);
/// it fires only once prod holds PBP but the rollup is empty. Verified to hold
/// across all 12 ingested seasons. Uses `EXISTS` for the PBP side so it stays
/// cheap against the multi-million-row table (short-circuits at the first row).
async fn pbp_present_but_lineups_empty(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM play_by_play WHERE season = $1) AS has_pbp, \
                (SELECT count(*) FROM lineup_aggregates WHERE season = $1) AS aggs",
    )
    .bind(season)
    .fetch_one(pool)
    .await?;
    let has_pbp: bool = row.get("has_pbp");
    let aggs: i64 = row.get("aggs");
    let sample = (has_pbp && aggs == 0).then(|| {
        format!(
            "season {season} has play_by_play but 0 lineup_aggregates — compute_pbp_lineups produced nothing"
        )
    });
    Ok(violation(
        "pbp_present_but_lineups_empty",
        Severity::Error,
        sample.into_iter(),
    ))
}

/// A game date must clear this share of its completed games having play-by-play
/// before [`pbp_date_coverage_gap`] stays quiet.
///
/// PBP coverage is never complete — NatStat simply never publishes some games —
/// so a per-*game* check would report 170–380 violations a night and be noise.
/// Per *date* the picture is clean: across 2021–2026 no game date sits below
/// 66% coverage, and not one is at zero. 50% therefore leaves 16 points of
/// headroom over the worst real date while still catching the failure this
/// exists for, which lands at 0%.
pub const PBP_DATE_MIN_COVERAGE: f64 = 0.5;

/// Completed games a date needs before its coverage share means anything.
///
/// The one 0%-coverage date in twelve ingested seasons (2019-12-24) had exactly
/// one game on it — a single unpublished game, not a pipeline failure. A share
/// computed over one or two games is a coin flip, so require a real slate.
pub const PBP_DATE_MIN_GAMES: i64 = 3;

/// Days a game date is left alone before its PBP coverage is judged.
///
/// The nightly ingests date D's box scores and its PBP in the same run (the run
/// on D+1), so if NatStat ever publishes PBP behind the box score, the newest
/// date would read as a hole on every single run. Two days of slack costs
/// nothing operationally — the backfill is one `cstat-ingest playbyplay
/// --from X --to Y` either way — and buys immunity to a feed-ordering quirk we
/// cannot observe from here.
const PBP_SETTLE_DAYS: i64 = 2;

/// A game date whose completed games are mostly missing play-by-play means a
/// `playbyplay` night was lost — and nothing else notices (issue #247).
///
/// The self-heal gap scan counts a date as covered on the box-score steps alone
/// (`BOX_SCORE_STEPS`), so a night where `games`/`player_perfs`/`team_perfs`
/// succeeded and `playbyplay` failed is never revisited. `compute_pbp_lineups`
/// is a season-scoped DELETE-then-rebuild, so from that night on it rebuilds the
/// whole season around the hole and `lineup_aggregates` / `player_on_off` /
/// `lineup_stints` quietly undercount for the rest of the year. The other two
/// signals both read that as healthy: [`pbp_present_but_lineups_empty`] is
/// all-or-nothing on the season, and the `row_counts` gate compares against a
/// prior run that was equally short, so the shortfall never looks like a shrink.
///
/// `Warning`, not `Error`: PBP is a source feed with genuine gaps, and what the
/// pipeline does with the rows it has is correct. It is also best-effort by
/// design (it feeds display surfaces and 3-of-60 trajectory features), so a hole
/// is worth a line in the run summary, not a red build.
///
/// Skipped entirely when the season has no PBP at all — a PBP-less prod, the
/// `simulate` replay, and the `ingest_replay` fixtures all seed none, and none
/// of them should see this fire. That gate rides on the same aggregate the check
/// already computes, so it costs nothing.
pub async fn pbp_date_coverage_gap(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    // `EXISTS` per game rather than a join+aggregate over `play_by_play`: the
    // table is 30M+ rows with a `(game_id, seq)` primary key, so this is one
    // index probe per game and the whole check runs in ~0.3s for a full season.
    let rows = sqlx::query(
        r#"
        WITH d AS (
            SELECT g.game_date,
                   count(*) AS games,
                   count(*) FILTER (
                       WHERE EXISTS (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id)
                   ) AS with_pbp
            FROM games g
            WHERE g.season = $1
              AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
              AND g.game_date <= CURRENT_DATE - $2::int
            GROUP BY g.game_date
        ),
        gate AS (SELECT COALESCE(sum(with_pbp), 0) AS total FROM d)
        SELECT d.game_date, d.games, d.with_pbp
        FROM d, gate
        WHERE gate.total > 0
          AND d.games >= $3
          AND d.with_pbp::float8 / d.games < $4
        ORDER BY d.game_date
        "#,
    )
    .bind(season)
    .bind(PBP_SETTLE_DAYS as i32)
    .bind(PBP_DATE_MIN_GAMES)
    .bind(PBP_DATE_MIN_COVERAGE)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "pbp_date_coverage_gap",
        Severity::Warning,
        rows.iter().map(|r| {
            format!(
                "{} ({}/{} games)",
                r.get::<chrono::NaiveDate, _>("game_date"),
                r.get::<i64, _>("with_pbp"),
                r.get::<i64, _>("games"),
            )
        }),
    ))
}

/// Share of a season's Torvik rotation rows allowed to sit unlinked to a
/// cstat player before the check fires.
///
/// The residual after the matcher's three passes is upstream coverage, not a
/// matching failure: NatStat never ingested those players at all. It sits
/// under 0.5% for ten of the twelve seasons, and at 2.1%/2.0% for 2021/2022,
/// where whole rosters are missing (George Washington, UC San Diego,
/// Bellarmine, Tarleton State — the COVID season and that year's D1
/// transitions). 3% clears that ceiling while leaving no room for a matcher
/// regression: before the fix, unlinked rotation rows ran at ~27% pooled.
pub const TORVIK_UNLINKED_ROTATION_MAX_SHARE: f64 = 0.03;

/// Torvik rows carry a player's advanced metrics, but only reach the site
/// through `players` — the player-SOS step resolves through `player_id`, so
/// an unlinked row silently ends with a NULL `cam_gbpm_v3_psos`, the column
/// the leaderboard sorts on. That is how 1,207 rotation players, Obi Toppin
/// and Ja Morant among them, vanished from the leaderboard without an error
/// (issue #243).
///
/// `Warning`, not `Error`: what survives the matcher is a hole in the source
/// data the pipeline faithfully reflects, and failing every run on a static
/// gap is alarm fatigue. The *share* is what carries signal, so the check
/// fires on [`TORVIK_UNLINKED_ROTATION_MAX_SHARE`] rather than a raw count.
///
/// Rotation minutes come from `total_minutes`, which despite the name holds
/// Torvik's true minutes-per-game; `minutes_per_game` holds Min% (see the
/// column-naming gotcha in `compute::compute_campom`). A season with no
/// Torvik rows ingested is not a violation.
pub async fn torvik_rows_unlinked(
    pool: &PgPool,
    season: i32,
) -> Result<Option<InvariantViolation>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT count(*) FILTER (WHERE total_minutes >= $2) AS rotation, \
                count(*) FILTER (WHERE total_minutes >= $2 AND player_id IS NULL) AS unlinked \
           FROM torvik_player_stats WHERE season = $1",
    )
    .bind(season)
    .bind(ROTATION_MPG)
    .fetch_one(pool)
    .await?;
    let rotation: i64 = row.get("rotation");
    let unlinked: i64 = row.get("unlinked");
    if rotation == 0 || (unlinked as f64 / rotation as f64) <= TORVIK_UNLINKED_ROTATION_MAX_SHARE {
        return Ok(None);
    }

    // Name a few so the report says who was dropped. `player_name` is only
    // populated from the ingest that introduced it, so fall back to the pid.
    let samples = sqlx::query(
        "SELECT COALESCE(player_name, 'pid ' || torvik_pid) AS who, team_name \
           FROM torvik_player_stats \
          WHERE season = $1 AND player_id IS NULL AND total_minutes >= $2 \
          ORDER BY gbpm DESC NULLS LAST LIMIT 5",
    )
    .bind(season)
    .bind(ROTATION_MPG)
    .fetch_all(pool)
    .await?;

    Ok(Some(InvariantViolation {
        check: "torvik_rows_unlinked",
        severity: Severity::Warning,
        count: unlinked,
        samples: samples
            .iter()
            .map(|r| {
                format!(
                    "{} ({})",
                    r.get::<String, _>("who"),
                    r.get::<String, _>("team_name")
                )
            })
            .collect(),
    }))
}

/// Minutes per game at or above which a Torvik row counts as a rotation
/// player. Mirrors `cstat_ingest::ingest::torvik`'s own threshold.
const ROTATION_MPG: f64 = 10.0;

/// Fold a list of offending ids into a violation (None when clean),
/// keeping the first few as samples.
fn violation(
    check: &'static str,
    severity: Severity,
    ids: impl Iterator<Item = String>,
) -> Option<InvariantViolation> {
    let ids: Vec<String> = ids.collect();
    if ids.is_empty() {
        return None;
    }
    Some(InvariantViolation {
        check,
        severity,
        count: ids.len() as i64,
        samples: ids.into_iter().take(5).collect(),
    })
}
