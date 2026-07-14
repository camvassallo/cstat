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

/// Cross-tag share above which a 2-team game counts as fully swapped —
/// mirrors `compute::correct_swapped_games` / the `swapped_games.rs` tests.
const MIN_CROSS_SHARE: f64 = 0.80;

/// One failed invariant: `count` offending rows for `check`, with up to a
/// handful of identifying `samples` for the report.
#[derive(Debug, Clone)]
pub struct InvariantViolation {
    pub check: &'static str,
    pub count: i64,
    pub samples: Vec<String>,
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} violation(s)", self.check, self.count)?;
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
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// No fully-swapped game may survive compute — the same bidirectional
/// cross-tag detector `compute::correct_swapped_games` uses (issue #119),
/// season-scoped. `pub` so `tests/swapped_games.rs` asserts through this
/// exact query instead of carrying its own copy.
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
    .bind(MIN_CROSS_SHARE)
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "fully_swapped_games_remain",
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// No phantom-swapped game may survive compute — the same gate
/// `compute::repair_phantom_swapped_games` uses (issue #140), season-scoped.
/// `pub` so `tests/swapped_games.rs` asserts through this exact query
/// instead of carrying its own copy.
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
                         AND r.team_id = (SELECT tg.team_id FROM team_game_stats tg
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
    .bind(MIN_CROSS_SHARE)
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(violation(
        "phantom_swapped_games_remain",
        rows.iter().map(|r| r.get::<String, _>("natstat_id")),
    ))
}

/// Fold a list of offending ids into a violation (None when clean),
/// keeping the first few as samples.
fn violation(check: &'static str, ids: impl Iterator<Item = String>) -> Option<InvariantViolation> {
    let ids: Vec<String> = ids.collect();
    if ids.is_empty() {
        return None;
    }
    Some(InvariantViolation {
        check,
        count: ids.len() as i64,
        samples: ids.into_iter().take(5).collect(),
    })
}
