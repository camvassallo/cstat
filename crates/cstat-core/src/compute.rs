use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Pure stat formulas (no DB required)
// ---------------------------------------------------------------------------

/// Estimate possessions from box score components.
/// Standard formula: Poss ≈ FGA − OREB + TOV + 0.44 × FTA
pub fn possessions(fga: f64, oreb: f64, tov: f64, fta: f64) -> f64 {
    fga - oreb + tov + 0.44 * fta
}

/// Box score line for game score calculation.
pub struct BoxScore {
    pub pts: f64,
    pub fgm: f64,
    pub fga: f64,
    pub ftm: f64,
    pub fta: f64,
    pub oreb: f64,
    pub dreb: f64,
    pub stl: f64,
    pub ast: f64,
    pub blk: f64,
    pub pf: f64,
    pub tov: f64,
}

/// Hollinger game score:
/// GmSc = PTS + 0.4×FGM − 0.7×FGA − 0.4×(FTA−FTM) + 0.7×OREB + 0.3×DREB
///        + STL + 0.7×AST + 0.7×BLK − 0.4×PF − TOV
pub fn game_score(b: &BoxScore) -> f64 {
    b.pts + 0.4 * b.fgm - 0.7 * b.fga - 0.4 * (b.fta - b.ftm)
        + 0.7 * b.oreb
        + 0.3 * b.dreb
        + b.stl
        + 0.7 * b.ast
        + 0.7 * b.blk
        - 0.4 * b.pf
        - b.tov
}

/// Effective field goal percentage: eFG% = (FGM + 0.5 × 3PM) / FGA
pub fn effective_fg_pct(fgm: f64, tpm: f64, fga: f64) -> Option<f64> {
    if fga > 0.0 {
        Some((fgm + 0.5 * tpm) / fga)
    } else {
        None
    }
}

/// True shooting percentage: TS% = PTS / (2 × (FGA + 0.44 × FTA))
pub fn true_shooting_pct(pts: f64, fga: f64, fta: f64) -> Option<f64> {
    let denom = 2.0 * (fga + 0.44 * fta);
    if denom > 0.0 { Some(pts / denom) } else { None }
}

/// Turnover percentage: TOV% = TOV / (FGA + 0.44 × FTA + TOV)
pub fn turnover_pct(tov: f64, fga: f64, fta: f64) -> Option<f64> {
    let denom = fga + 0.44 * fta + tov;
    if denom > 0.0 { Some(tov / denom) } else { None }
}

/// Assist-to-turnover ratio with safe division.
pub fn ast_to_ratio(ast: f64, tov: f64) -> f64 {
    if tov > 0.0 {
        ast / tov
    } else if ast > 0.0 {
        ast
    } else {
        0.0
    }
}

/// Deduplicate player records that share the same (name, team_id, season).
/// NatStat assigns different player codes across seasons, creating duplicate entries.
/// For each pair: keep the primary (most games), delete overlapping game stats,
/// reassign non-overlapping game stats, then remove the duplicate player record.
pub async fn deduplicate_players(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Find duplicate groups: same (name, team_id, season) with >1 player
    let dupes: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT p.name, p.team_id
         FROM players p
         WHERE p.season = $1 AND p.team_id IS NOT NULL
         GROUP BY p.name, p.team_id
         HAVING COUNT(*) > 1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    if dupes.is_empty() {
        info!(season, "no duplicate players found");
        return Ok(0);
    }

    info!(pairs = dupes.len(), season, "found duplicate player groups");

    let mut merged = 0u64;

    for (name, team_id) in &dupes {
        // Get all player IDs for this (name, team_id, season), ordered by game count desc
        let players: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT p.id, COUNT(pgs.id) as game_count
             FROM players p
             LEFT JOIN player_game_stats pgs ON pgs.player_id = p.id
             WHERE p.name = $1 AND p.team_id = $2 AND p.season = $3
             GROUP BY p.id
             ORDER BY game_count DESC",
        )
        .bind(name)
        .bind(team_id)
        .bind(season)
        .fetch_all(pool)
        .await?;

        if players.len() < 2 {
            continue;
        }

        let primary_id = players[0].0;

        for &(dup_id, _) in &players[1..] {
            // Step 1: Delete duplicate game_stats for overlapping games
            // (both player codes appear in the same game — identical stats)
            let r1 = sqlx::query(
                "DELETE FROM player_game_stats
                 WHERE player_id = $1
                   AND game_id IN (
                       SELECT game_id FROM player_game_stats WHERE player_id = $2
                   )",
            )
            .bind(dup_id)
            .bind(primary_id)
            .execute(pool)
            .await?;

            // Step 2: Reassign non-overlapping game_stats from dup → primary
            let r2 =
                sqlx::query("UPDATE player_game_stats SET player_id = $1 WHERE player_id = $2")
                    .bind(primary_id)
                    .bind(dup_id)
                    .execute(pool)
                    .await?;

            // Step 3: Delete dup's season stats and percentiles
            sqlx::query("DELETE FROM player_season_stats WHERE player_id = $1")
                .bind(dup_id)
                .execute(pool)
                .await?;

            sqlx::query("DELETE FROM player_percentiles WHERE player_id = $1")
                .bind(dup_id)
                .execute(pool)
                .await?;

            // Step 4: Delete the duplicate player record
            sqlx::query("DELETE FROM players WHERE id = $1")
                .bind(dup_id)
                .execute(pool)
                .await?;

            info!(
                primary = %primary_id,
                duplicate = %dup_id,
                name = %name,
                overlapping_deleted = r1.rows_affected(),
                reassigned = r2.rows_affected(),
                "merged duplicate player"
            );
            merged += 1;
        }
    }

    // Delete stale season stats for affected primaries so recompute picks them up fresh
    // (We'll recompute in the next pipeline step anyway)
    sqlx::query(
        "DELETE FROM player_season_stats WHERE player_id IN (
            SELECT p.id FROM players p
            WHERE p.season = $1
        ) AND season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(merged, season, "player deduplication complete");
    Ok(merged)
}

/// Backfill derived columns on player_game_stats that can be computed from existing data.
pub async fn backfill_game_stats(pool: &PgPool) -> Result<u64, sqlx::Error> {
    // Scrub fake rebound zeros. NatStat omits rebound data for ~68% of games
    // by returning `reb=0` for every player record. The ingest layer nulls
    // `total_rebounds` when `reb=0` AND `oreb>0` (impossible), but leaves
    // `total_rebounds=0` for players with `oreb=0` in the same game.
    //
    // Only null out rows with `total_rebounds=0` in games that have the
    // impossible pattern — don't touch rows where Torvik backfill has
    // already provided real rebound data (total_rebounds > 0).
    let r0 = sqlx::query(
        "UPDATE player_game_stats pgs
         SET total_rebounds = NULL,
             def_rebounds = NULL
         WHERE pgs.game_id IN (
             SELECT DISTINCT game_id
             FROM player_game_stats
             WHERE total_rebounds IS NULL AND off_rebounds > 0
         )
         AND pgs.total_rebounds = 0",
    )
    .execute(pool)
    .await?;

    // def_rebounds = total_rebounds - off_rebounds (NatStat "reb" = total rebounds;
    // ingestion now maps reb → total_rebounds and derives def_rebounds = total - oreb.
    // This backfill catches any rows where def_rebounds is NULL but can be derived.)
    let r1 = sqlx::query(
        "UPDATE player_game_stats
         SET def_rebounds = total_rebounds - off_rebounds
         WHERE total_rebounds IS NOT NULL
           AND off_rebounds IS NOT NULL
           AND total_rebounds >= off_rebounds
           AND def_rebounds IS NULL",
    )
    .execute(pool)
    .await?;

    // ast_to_ratio = assists / turnovers (guard against div by zero)
    let r2 = sqlx::query(
        "UPDATE player_game_stats
         SET ast_to_ratio = CASE
             WHEN turnovers > 0 THEN assists::float / turnovers
             WHEN assists > 0 THEN assists::float
             ELSE 0.0
         END
         WHERE assists IS NOT NULL
           AND turnovers IS NOT NULL
           AND ast_to_ratio IS NULL",
    )
    .execute(pool)
    .await?;

    // game_score (John Hollinger formula):
    // GmSc = PTS + 0.4*FGM - 0.7*FGA - 0.4*(FTA-FTM) + 0.7*OREB + 0.3*DREB
    //        + STL + 0.7*AST + 0.7*BLK - 0.4*PF - TOV
    let r3 = sqlx::query(
        "UPDATE player_game_stats
         SET game_score = ROUND((
             COALESCE(points, 0)
             + 0.4 * COALESCE(fgm, 0)
             - 0.7 * COALESCE(fga, 0)
             - 0.4 * (COALESCE(fta, 0) - COALESCE(ftm, 0))
             + 0.7 * COALESCE(off_rebounds, 0)
             + 0.3 * COALESCE(def_rebounds, total_rebounds::int - COALESCE(off_rebounds, 0), 0)
             + COALESCE(steals, 0)
             + 0.7 * COALESCE(assists, 0)
             + 0.7 * COALESCE(blocks, 0)
             - 0.4 * COALESCE(fouls, 0)
             - COALESCE(turnovers, 0)
         )::numeric, 1)::float
         WHERE points IS NOT NULL
           AND game_score IS NULL",
    )
    .execute(pool)
    .await?;

    let total = r0.rows_affected() + r1.rows_affected() + r2.rows_affected() + r3.rows_affected();
    info!(
        scrubbed_fake_reb_zeros = r0.rows_affected(),
        def_rebounds = r1.rows_affected(),
        ast_to_ratio = r2.rows_affected(),
        game_score = r3.rows_affected(),
        "backfilled derived game stats"
    );
    Ok(total)
}

/// Estimate missing team_game_stats.def_rebounds from the box score.
///
/// When NatStat's `reb` field is missing (NULL after ingestion guard), we can estimate
/// defensive rebounds using: DREB ≈ opponent_missed_FGA - opponent_OREB.
///
/// Validated against 3,178 games with real data: correlation=0.840, MAE=2.38, bias=-0.86.
/// This fills ~69% of team games that would otherwise have NULL DREB, giving the four
/// factors (ORB%/DRB%) calculation full coverage instead of a sparse ~31% sample.
pub async fn estimate_missing_team_rebounds(
    pool: &PgPool,
    season: i32,
) -> Result<u64, sqlx::Error> {
    // Estimate def_rebounds from opponent's missed field goals minus opponent's offensive rebounds.
    // Also backfill total_rebounds = off_rebounds + estimated def_rebounds.
    let result = sqlx::query(
        "UPDATE team_game_stats tgs SET
            def_rebounds = GREATEST((opp.fga - opp.fgm) - opp.off_rebounds, 0),
            total_rebounds = tgs.off_rebounds + GREATEST((opp.fga - opp.fgm) - opp.off_rebounds, 0)
        FROM team_game_stats opp
        WHERE opp.game_id = tgs.game_id
          AND opp.team_id = tgs.opponent_id
          AND tgs.season = $1
          AND tgs.def_rebounds IS NULL
          AND tgs.off_rebounds IS NOT NULL
          AND opp.fga IS NOT NULL
          AND opp.fgm IS NOT NULL
          AND opp.off_rebounds IS NOT NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "estimated missing team defensive rebounds from box score"
    );
    Ok(result.rows_affected())
}

/// Compute player_season_stats by aggregating player_game_stats.
///
/// **Unit conventions** (carry through to API consumers and ML features):
/// - Shooting splits (`fg_pct`, `tp_pct`, `ft_pct`, `effective_fg_pct`,
///   `true_shooting_pct`) are stored as **fractions** (0.0–1.0).
/// - Rate stats (`usage_rate`, `ast_pct`, `tov_pct`, `ft_rate`) are also
///   stored as **fractions**, despite their `_pct` names — multiply by 100
///   to compare against Torvik or other percent-scaled sources.
/// - Possession-based percentages (`orb_pct`, `drb_pct`, `stl_pct`, `blk_pct`)
///   are stored as **percent** (0–100), matching Basketball Reference convention.
pub async fn compute_player_season_stats(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Clear existing for this season so we recompute cleanly
    sqlx::query("DELETE FROM player_season_stats WHERE season = $1")
        .bind(season)
        .execute(pool)
        .await?;

    let result = sqlx::query(
        "INSERT INTO player_season_stats (
            id, player_id, team_id, season,
            games_played, games_started, minutes_per_game,
            ppg, rpg, apg, spg, bpg, topg, fpg,
            fg_pct, tp_pct, ft_pct, effective_fg_pct, true_shooting_pct,
            usage_rate, ast_pct, tov_pct, orb_pct, drb_pct, stl_pct, blk_pct,
            ft_rate
        )
        SELECT
            gen_random_uuid(),
            pgs.player_id,
            pgs.team_id,
            pgs.season,
            -- Games
            COUNT(*),
            COUNT(*) FILTER (WHERE pgs.starter = true),
            ROUND(AVG(pgs.minutes)::numeric, 1),
            -- Per-game averages
            ROUND(AVG(pgs.points)::numeric, 1),
            ROUND(AVG(pgs.total_rebounds)::numeric, 1),
            ROUND(AVG(pgs.assists)::numeric, 1),
            ROUND(AVG(pgs.steals)::numeric, 1),
            ROUND(AVG(pgs.blocks)::numeric, 1),
            ROUND(AVG(pgs.turnovers)::numeric, 1),
            ROUND(AVG(pgs.fouls)::numeric, 1),
            -- Shooting percentages (season totals, not avg of per-game pcts)
            CASE WHEN SUM(pgs.fga) > 0
                THEN ROUND((SUM(pgs.fgm)::float / SUM(pgs.fga))::numeric, 3)
                ELSE NULL END,
            CASE WHEN SUM(pgs.tpa) > 0
                THEN ROUND((SUM(pgs.tpm)::float / SUM(pgs.tpa))::numeric, 3)
                ELSE NULL END,
            CASE WHEN SUM(pgs.fta) > 0
                THEN ROUND((SUM(pgs.ftm)::float / SUM(pgs.fta))::numeric, 3)
                ELSE NULL END,
            -- eFG% = (FGM + 0.5 * 3PM) / FGA
            CASE WHEN SUM(pgs.fga) > 0
                THEN ROUND(((SUM(pgs.fgm) + 0.5 * SUM(COALESCE(pgs.tpm, 0)))::float / SUM(pgs.fga))::numeric, 3)
                ELSE NULL END,
            -- TS% = PTS / (2 * (FGA + 0.44 * FTA))
            CASE WHEN (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0))) > 0
                THEN ROUND((SUM(pgs.points)::float / (2.0 * (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)))))::numeric, 3)
                ELSE NULL END,
            -- USG% (Basketball Reference): 100 × ((Plays × Tm_MP/5) / (MP × Tm_Plays))
            -- where Plays = FGA + 0.44×FTA + TOV. Stored as a fraction (multiply by 100 for percent).
            CASE WHEN SUM(pgs.minutes) > 0
                  AND SUM(COALESCE(pgs.team_fga, 0) + 0.44 * COALESCE(pgs.team_fta, 0)
                          + COALESCE(pgs.team_turnovers, 0)) > 0
                THEN ROUND((
                    (SUM(pgs.fga + 0.44 * COALESCE(pgs.fta, 0) + COALESCE(pgs.turnovers, 0))::float
                        * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0))
                    / (SUM(pgs.minutes)::float
                        * SUM(COALESCE(pgs.team_fga, 0) + 0.44 * COALESCE(pgs.team_fta, 0)
                              + COALESCE(pgs.team_turnovers, 0))::float)
                )::numeric, 3)
                ELSE NULL END,
            -- AST% (Basketball Reference): AST / ((MP / (Team_MP / 5)) × Team_FGM − Player_FGM)
            -- Stored as a fraction (multiply by 100 for percent).
            CASE WHEN (5.0 * SUM(pgs.minutes)::float * SUM(COALESCE(pgs.team_fgm, 0))::float
                       / NULLIF(SUM(COALESCE(tgs.minutes, 200))::float, 0)
                       - SUM(pgs.fgm)::float) > 0
                THEN ROUND((SUM(pgs.assists)::float / (
                    5.0 * SUM(pgs.minutes)::float * SUM(COALESCE(pgs.team_fgm, 0))::float
                        / NULLIF(SUM(COALESCE(tgs.minutes, 200))::float, 0)
                    - SUM(pgs.fgm)::float
                ))::numeric, 3)
                ELSE NULL END,
            -- TOV% = TOV / (FGA + 0.44 * FTA + TOV)
            CASE WHEN (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))) > 0
                THEN ROUND((SUM(COALESCE(pgs.turnovers, 0))::float /
                    (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))))::numeric, 3)
                ELSE NULL END,
            -- ORB% = 100 * (ORB * (Tm MP / 5)) / (MP * (Tm ORB + Opp DRB))
            CASE WHEN SUM(pgs.minutes) > 0
                      AND SUM(COALESCE(tgs.off_rebounds, 0) + COALESCE(opp.def_rebounds, 0)) > 0
                THEN ROUND((100.0 * SUM(COALESCE(pgs.off_rebounds, 0))::float
                    * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0)
                    / (SUM(pgs.minutes)::float
                    * SUM(COALESCE(tgs.off_rebounds, 0) + COALESCE(opp.def_rebounds, 0))::float))::numeric, 1)
                ELSE NULL END,
            -- DRB% = 100 * (DRB * (Tm MP / 5)) / (MP * (Tm DRB + Opp ORB))
            CASE WHEN SUM(pgs.minutes) > 0
                      AND SUM(COALESCE(tgs.def_rebounds, 0) + COALESCE(opp.off_rebounds, 0)) > 0
                THEN ROUND((100.0 * SUM(COALESCE(pgs.def_rebounds, 0))::float
                    * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0)
                    / (SUM(pgs.minutes)::float
                    * SUM(COALESCE(tgs.def_rebounds, 0) + COALESCE(opp.off_rebounds, 0))::float))::numeric, 1)
                ELSE NULL END,
            -- STL% = 100 * (STL * (Tm MP / 5)) / (MP * Opp Poss)
            -- Opp Poss ≈ Opp FGA - Opp ORB + Opp TOV + 0.44 * Opp FTA
            CASE WHEN SUM(pgs.minutes) > 0
                      AND SUM(COALESCE(opp.fga, 0) - COALESCE(opp.off_rebounds, 0)
                            + COALESCE(opp.turnovers, 0) + 0.44 * COALESCE(opp.fta, 0)) > 0
                THEN ROUND((100.0 * SUM(COALESCE(pgs.steals, 0))::float
                    * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0)
                    / (SUM(pgs.minutes)::float
                    * SUM(COALESCE(opp.fga, 0)::float - COALESCE(opp.off_rebounds, 0)::float
                        + COALESCE(opp.turnovers, 0)::float + 0.44 * COALESCE(opp.fta, 0)::float)))::numeric, 1)
                ELSE NULL END,
            -- BLK% = 100 * (BLK * (Tm MP / 5)) / (MP * (Opp FGA - Opp 3PA))
            CASE WHEN SUM(pgs.minutes) > 0
                      AND SUM(COALESCE(opp.fga, 0) - COALESCE(opp.tpa, 0)) > 0
                THEN ROUND((100.0 * SUM(COALESCE(pgs.blocks, 0))::float
                    * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0)
                    / (SUM(pgs.minutes)::float
                    * SUM(COALESCE(opp.fga, 0) - COALESCE(opp.tpa, 0))::float))::numeric, 1)
                ELSE NULL END,
            -- FT Rate = FTA / FGA
            CASE WHEN SUM(pgs.fga) > 0
                THEN ROUND((SUM(COALESCE(pgs.fta, 0))::float / SUM(pgs.fga)::float)::numeric, 3)
                ELSE NULL END
        FROM player_game_stats pgs
        LEFT JOIN team_game_stats tgs
            ON tgs.game_id = pgs.game_id AND tgs.team_id = pgs.team_id
        LEFT JOIN team_game_stats opp
            ON opp.game_id = pgs.game_id AND opp.team_id = pgs.opponent_id
        WHERE pgs.season = $1
          AND pgs.minutes IS NOT NULL
          AND pgs.minutes > 0
        GROUP BY pgs.player_id, pgs.team_id, pgs.season",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "computed player season stats"
    );
    Ok(result.rows_affected())
}

/// Populate schedules table from games.
pub async fn compute_schedules(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Clear existing for this season
    sqlx::query("DELETE FROM schedules WHERE season = $1")
        .bind(season)
        .execute(pool)
        .await?;

    // Insert home team perspective
    let r1 = sqlx::query(
        "INSERT INTO schedules (id, game_id, team_id, season, game_date, opponent_id,
         is_home, is_neutral, team_score, opponent_score)
        SELECT gen_random_uuid(), g.id, g.home_team_id, g.season, g.game_date,
               g.away_team_id, NOT g.is_neutral_site, g.is_neutral_site,
               g.home_score, g.away_score
        FROM games g
        WHERE g.season = $1
          AND g.home_team_id IS NOT NULL
        ON CONFLICT (game_id, team_id) DO NOTHING",
    )
    .bind(season)
    .execute(pool)
    .await?;

    // Insert away team perspective
    let r2 = sqlx::query(
        "INSERT INTO schedules (id, game_id, team_id, season, game_date, opponent_id,
         is_home, is_neutral, team_score, opponent_score)
        SELECT gen_random_uuid(), g.id, g.away_team_id, g.season, g.game_date,
               g.home_team_id, false, g.is_neutral_site,
               g.away_score, g.home_score
        FROM games g
        WHERE g.season = $1
          AND g.away_team_id IS NOT NULL
        ON CONFLICT (game_id, team_id) DO NOTHING",
    )
    .bind(season)
    .execute(pool)
    .await?;

    let total = r1.rows_affected() + r2.rows_affected();
    info!(total, season, "computed schedules");
    Ok(total)
}

/// Compute player percentile rankings across all players in a season.
/// Requires player_season_stats to be populated first.
pub async fn compute_player_percentiles(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Clear existing
    sqlx::query("DELETE FROM player_percentiles WHERE season = $1")
        .bind(season)
        .execute(pool)
        .await?;

    // Only rank players with meaningful minutes (e.g., > 10 mpg and > 10 games)
    let result = sqlx::query(
        "INSERT INTO player_percentiles (
            id, player_id, season,
            ppg_pct, rpg_pct, apg_pct, spg_pct, bpg_pct,
            fg_pct_pct, tp_pct_pct, ft_pct_pct, effective_fg_pct_pct, true_shooting_pct_pct,
            usage_rate_pct, offensive_rating_pct, defensive_rating_pct,
            player_sos_pct,
            ast_pct_pct, tov_pct_pct, mpg_pct, topg_pct,
            orb_pct_pct, drb_pct_pct, stl_pct_pct, blk_pct_pct, ft_rate_pct
        )
        WITH best AS (
            SELECT DISTINCT ON (player_id)
                player_id, season, ppg, rpg, apg, spg, bpg,
                fg_pct, tp_pct, ft_pct, effective_fg_pct, true_shooting_pct,
                usage_rate, offensive_rating, defensive_rating,
                player_sos, ast_pct, tov_pct, minutes_per_game, topg,
                orb_pct, drb_pct, stl_pct, blk_pct, ft_rate
            FROM player_season_stats
            WHERE season = $1
              AND games_played >= 10
              AND minutes_per_game >= 10
            ORDER BY player_id, games_played DESC
        )
        SELECT
            gen_random_uuid(),
            b.player_id,
            b.season,
            PERCENT_RANK() OVER (ORDER BY b.ppg),
            PERCENT_RANK() OVER (ORDER BY b.rpg),
            PERCENT_RANK() OVER (ORDER BY b.apg),
            PERCENT_RANK() OVER (ORDER BY b.spg),
            PERCENT_RANK() OVER (ORDER BY b.bpg),
            PERCENT_RANK() OVER (ORDER BY b.fg_pct),
            PERCENT_RANK() OVER (ORDER BY b.tp_pct),
            PERCENT_RANK() OVER (ORDER BY b.ft_pct),
            PERCENT_RANK() OVER (ORDER BY b.effective_fg_pct),
            PERCENT_RANK() OVER (ORDER BY b.true_shooting_pct),
            PERCENT_RANK() OVER (ORDER BY b.usage_rate),
            PERCENT_RANK() OVER (ORDER BY b.offensive_rating),
            PERCENT_RANK() OVER (ORDER BY b.defensive_rating DESC),
            PERCENT_RANK() OVER (ORDER BY b.player_sos),
            PERCENT_RANK() OVER (ORDER BY b.ast_pct),
            PERCENT_RANK() OVER (ORDER BY b.tov_pct DESC),
            PERCENT_RANK() OVER (ORDER BY b.minutes_per_game),
            PERCENT_RANK() OVER (ORDER BY b.topg DESC),
            PERCENT_RANK() OVER (ORDER BY b.orb_pct),
            PERCENT_RANK() OVER (ORDER BY b.drb_pct),
            PERCENT_RANK() OVER (ORDER BY b.stl_pct),
            PERCENT_RANK() OVER (ORDER BY b.blk_pct),
            PERCENT_RANK() OVER (ORDER BY b.ft_rate)
        FROM best b
        ON CONFLICT (player_id, season) DO UPDATE
        SET ppg_pct = EXCLUDED.ppg_pct,
            rpg_pct = EXCLUDED.rpg_pct,
            apg_pct = EXCLUDED.apg_pct,
            spg_pct = EXCLUDED.spg_pct,
            bpg_pct = EXCLUDED.bpg_pct,
            fg_pct_pct = EXCLUDED.fg_pct_pct,
            tp_pct_pct = EXCLUDED.tp_pct_pct,
            ft_pct_pct = EXCLUDED.ft_pct_pct,
            effective_fg_pct_pct = EXCLUDED.effective_fg_pct_pct,
            true_shooting_pct_pct = EXCLUDED.true_shooting_pct_pct,
            usage_rate_pct = EXCLUDED.usage_rate_pct,
            offensive_rating_pct = EXCLUDED.offensive_rating_pct,
            defensive_rating_pct = EXCLUDED.defensive_rating_pct,
            player_sos_pct = EXCLUDED.player_sos_pct,
            ast_pct_pct = EXCLUDED.ast_pct_pct,
            tov_pct_pct = EXCLUDED.tov_pct_pct,
            mpg_pct = EXCLUDED.mpg_pct,
            topg_pct = EXCLUDED.topg_pct,
            orb_pct_pct = EXCLUDED.orb_pct_pct,
            drb_pct_pct = EXCLUDED.drb_pct_pct,
            stl_pct_pct = EXCLUDED.stl_pct_pct,
            blk_pct_pct = EXCLUDED.blk_pct_pct,
            ft_rate_pct = EXCLUDED.ft_rate_pct",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "computed player percentiles"
    );
    Ok(result.rows_affected())
}

/// Compute team four factors and efficiency from team_game_stats.
/// Updates existing team_season_stats rows with derived offensive/defensive metrics.
pub async fn compute_team_four_factors(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Offensive four factors from team's own game stats
    // Possessions ≈ FGA - OREB + TOV + 0.44 * FTA
    // Offensive efficiency = Points / Possessions * 100
    // eFG% = (FGM + 0.5 * 3PM) / FGA
    // TOV% = TOV / Possessions
    // ORB% = OREB / (OREB + Opp DREB) — computed via reb_agg self-join below
    // FT Rate = FTA / FGA
    let result = sqlx::query(
        "WITH team_agg AS (
            SELECT
                tgs.team_id,
                -- Offensive stats
                SUM(tgs.fga) as fga,
                SUM(tgs.fgm) as fgm,
                SUM(tgs.tpa) as tpa,
                SUM(tgs.tpm) as tpm,
                SUM(tgs.fta) as fta,
                SUM(tgs.ftm) as ftm,
                SUM(tgs.off_rebounds) as oreb,
                SUM(tgs.def_rebounds) as dreb,
                SUM(tgs.turnovers) as tov,
                SUM(tgs.points) as pts,
                COUNT(*) as games,
                -- Possessions estimate
                SUM(tgs.fga) - SUM(tgs.off_rebounds) + SUM(tgs.turnovers) + 0.44 * SUM(tgs.fta) as poss
            FROM team_game_stats tgs
            WHERE tgs.season = $1
            GROUP BY tgs.team_id
        ),
        opp_agg AS (
            -- Opponent (defensive) stats: what opponents did against this team
            SELECT
                tgs.opponent_id as team_id,
                SUM(tgs.fga) as opp_fga,
                SUM(tgs.fgm) as opp_fgm,
                SUM(tgs.tpm) as opp_tpm,
                SUM(tgs.fta) as opp_fta,
                SUM(tgs.ftm) as opp_ftm,
                SUM(tgs.off_rebounds) as opp_oreb,
                SUM(tgs.def_rebounds) as opp_dreb,
                SUM(tgs.turnovers) as opp_tov,
                SUM(tgs.points) as opp_pts,
                SUM(tgs.fga) - SUM(tgs.off_rebounds) + SUM(tgs.turnovers) + 0.44 * SUM(tgs.fta) as opp_poss
            FROM team_game_stats tgs
            WHERE tgs.season = $1
              AND tgs.opponent_id IS NOT NULL
            GROUP BY tgs.opponent_id
        ),
        reb_agg AS (
            -- Rebound rates via game-level self-join on team_game_stats.
            -- ORB% = team_OREB / (team_OREB + opp_DREB)
            -- DRB% = team_DREB / (team_DREB + opp_OREB)
            SELECT
                tgs.team_id,
                ROUND((SUM(tgs.off_rebounds)::float /
                    NULLIF(SUM(tgs.off_rebounds) + SUM(opp.def_rebounds), 0))::numeric, 3) as off_rebound_pct,
                ROUND((SUM(tgs.def_rebounds)::float /
                    NULLIF(SUM(tgs.def_rebounds) + SUM(opp.off_rebounds), 0))::numeric, 3) as def_rebound_pct
            FROM team_game_stats tgs
            JOIN team_game_stats opp ON opp.game_id = tgs.game_id AND opp.team_id = tgs.opponent_id
            WHERE tgs.season = $1
              AND tgs.off_rebounds IS NOT NULL AND tgs.def_rebounds IS NOT NULL AND tgs.def_rebounds > 0
              AND opp.off_rebounds IS NOT NULL AND opp.def_rebounds IS NOT NULL AND opp.def_rebounds > 0
            GROUP BY tgs.team_id
        )
        UPDATE team_season_stats tss SET
            -- Offensive efficiency = pts / poss * 100
            adj_offense = ROUND((t.pts / NULLIF(t.poss, 0) * 100)::numeric, 1),
            -- Defensive efficiency = opp_pts / opp_poss * 100
            adj_defense = ROUND((o.opp_pts / NULLIF(o.opp_poss, 0) * 100)::numeric, 1),
            -- Efficiency margin
            adj_efficiency_margin = ROUND(((t.pts / NULLIF(t.poss, 0) - o.opp_pts / NULLIF(o.opp_poss, 0)) * 100)::numeric, 1),
            -- Tempo = possessions per game (avg of own + opponent)
            adj_tempo = ROUND(((t.poss + COALESCE(o.opp_poss, t.poss)) / (2.0 * t.games))::numeric, 1),
            -- Offensive four factors
            effective_fg_pct = ROUND(((t.fgm + 0.5 * t.tpm)::float / NULLIF(t.fga, 0))::numeric, 3),
            turnover_pct = ROUND((t.tov::float / NULLIF(t.poss, 0))::numeric, 3),
            off_rebound_pct = r.off_rebound_pct,
            ft_rate = ROUND((t.fta::float / NULLIF(t.fga, 0))::numeric, 3),
            -- Defensive four factors
            opp_effective_fg_pct = ROUND(((o.opp_fgm + 0.5 * o.opp_tpm)::float / NULLIF(o.opp_fga, 0))::numeric, 3),
            opp_turnover_pct = ROUND((o.opp_tov::float / NULLIF(o.opp_poss, 0))::numeric, 3),
            opp_ft_rate = ROUND((o.opp_fta::float / NULLIF(o.opp_fga, 0))::numeric, 3),
            def_rebound_pct = r.def_rebound_pct,
            updated_at = now()
        FROM team_agg t
        LEFT JOIN opp_agg o ON t.team_id = o.team_id
        LEFT JOIN reb_agg r ON t.team_id = r.team_id
        WHERE tss.team_id = t.team_id AND tss.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "computed team four factors"
    );
    Ok(result.rows_affected())
}

/// KenPom-style opponent-adjusted efficiency ratings.
/// Iteratively adjusts each team's offensive and defensive efficiency
/// by the quality of opponents faced until ratings converge.
///
/// Algorithm:
/// 1. Compute raw per-game efficiency for each team (pts / possessions * 100)
/// 2. Initialize each team's adjusted off/def to their raw averages
/// 3. For each iteration:
///    a. For each game, compute expected efficiency = league_avg * (opponent_rating / league_avg)
///    b. Adjusted efficiency = raw_efficiency * (league_avg / opponent_rating)
///    c. Average across all games for each team
/// 4. Repeat until max change between iterations < threshold
type GameRow = (
    Uuid,
    Option<Uuid>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
);

pub async fn compute_adjusted_efficiency(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Fetch all team game stats: team_id, opponent_id, points, fga, off_rebounds, turnovers, fta
    let games: Vec<GameRow> = sqlx::query_as(
        "SELECT team_id, opponent_id, points, fga, off_rebounds, turnovers, fta
             FROM team_game_stats
             WHERE season = $1 AND points IS NOT NULL AND fga IS NOT NULL",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    if games.is_empty() {
        return Ok(0);
    }

    // Build per-game data: (team_id, opponent_id, off_efficiency, def_efficiency)
    struct GameEff {
        team_id: Uuid,
        opponent_id: Uuid,
        points: f64,
        possessions: f64,
    }

    let mut game_data: Vec<GameEff> = Vec::new();

    for (team_id, opponent_id, points, fga, oreb, tov, fta) in &games {
        let Some(opp_id) = opponent_id else { continue };
        let pts = *points.as_ref().unwrap_or(&0) as f64;
        let fga = *fga.as_ref().unwrap_or(&0) as f64;
        let oreb = *oreb.as_ref().unwrap_or(&0) as f64;
        let tov = *tov.as_ref().unwrap_or(&0) as f64;
        let fta = *fta.as_ref().unwrap_or(&0) as f64;
        let poss = fga - oreb + tov + 0.44 * fta;
        if poss <= 0.0 {
            continue;
        }

        game_data.push(GameEff {
            team_id: *team_id,
            opponent_id: *opp_id,
            points: pts,
            possessions: poss,
        });
    }

    // Compute raw season averages per team
    struct TeamRaw {
        total_off_pts: f64,
        total_off_poss: f64,
        total_def_pts: f64,
        total_def_poss: f64,
    }

    let mut raw: HashMap<Uuid, TeamRaw> = HashMap::new();
    for g in &game_data {
        let entry = raw.entry(g.team_id).or_insert(TeamRaw {
            total_off_pts: 0.0,
            total_off_poss: 0.0,
            total_def_pts: 0.0,
            total_def_poss: 0.0,
        });
        entry.total_off_pts += g.points;
        entry.total_off_poss += g.possessions;
    }
    // Defensive: what opponents scored against this team
    for g in &game_data {
        if let Some(entry) = raw.get_mut(&g.opponent_id) {
            entry.total_def_pts += g.points;
            entry.total_def_poss += g.possessions;
        }
    }

    // League average efficiency
    let total_pts: f64 = raw.values().map(|r| r.total_off_pts).sum();
    let total_poss: f64 = raw.values().map(|r| r.total_off_poss).sum();
    let league_avg = if total_poss > 0.0 {
        total_pts / total_poss * 100.0
    } else {
        100.0
    };

    // Initialize adjusted ratings to raw
    let mut adj_off: HashMap<Uuid, f64> = HashMap::new();
    let mut adj_def: HashMap<Uuid, f64> = HashMap::new();
    for (team_id, r) in &raw {
        let off = if r.total_off_poss > 0.0 {
            r.total_off_pts / r.total_off_poss * 100.0
        } else {
            league_avg
        };
        let def = if r.total_def_poss > 0.0 {
            r.total_def_pts / r.total_def_poss * 100.0
        } else {
            league_avg
        };
        adj_off.insert(*team_id, off);
        adj_def.insert(*team_id, def);
    }

    // Iterative adjustment
    let max_iterations = 50;
    let convergence_threshold = 0.01;

    for iteration in 0..max_iterations {
        // For each team, compute adjusted efficiency as:
        //   adj_off = raw_off * (league_avg / avg_opponent_adj_def)
        //   adj_def = raw_def * (league_avg / avg_opponent_adj_off)
        // where avg_opponent_adj_* is the possession-weighted average of opponents faced.
        //
        // This is equivalent to: "what would this team's efficiency be if they
        // played an average schedule?"

        // For each team, compute avg opponent adj_def (for offense adjustment)
        // and avg opponent adj_off (for defense adjustment).
        // Both keyed by the team whose rating we're adjusting.
        let mut opp_def_sum: HashMap<Uuid, (f64, f64)> = HashMap::new();
        let mut opp_off_sum: HashMap<Uuid, (f64, f64)> = HashMap::new();

        for g in &game_data {
            let opp_def = adj_def.get(&g.opponent_id).copied().unwrap_or(league_avg);

            // team_id's offense faced opponent_id's defense
            let e = opp_def_sum.entry(g.team_id).or_insert((0.0, 0.0));
            e.0 += opp_def * g.possessions;
            e.1 += g.possessions;

            // opponent_id's defense faced team_id's offense
            // So for opponent_id's defensive adjustment, accumulate team_id's adj_off
            let team_off = adj_off.get(&g.team_id).copied().unwrap_or(league_avg);
            let e = opp_off_sum.entry(g.opponent_id).or_insert((0.0, 0.0));
            e.0 += team_off * g.possessions;
            e.1 += g.possessions;
        }

        let mut max_change: f64 = 0.0;

        // Update offensive ratings
        for (team_id, r) in &raw {
            if r.total_off_poss <= 0.0 {
                continue;
            }
            let raw_off = r.total_off_pts / r.total_off_poss * 100.0;
            let avg_opp_def = match opp_def_sum.get(team_id) {
                Some((s, p)) if *p > 0.0 => s / p,
                _ => league_avg,
            };
            // Scale: if opponents' defense is tougher than avg, boost our rating
            let new_val = raw_off * (league_avg / avg_opp_def);
            let old_val = adj_off.get(team_id).copied().unwrap_or(league_avg);
            max_change = max_change.max((new_val - old_val).abs());
            adj_off.insert(*team_id, new_val);
        }

        // Update defensive ratings
        for (team_id, r) in &raw {
            if r.total_def_poss <= 0.0 {
                continue;
            }
            let raw_def = r.total_def_pts / r.total_def_poss * 100.0;
            let avg_opp_off = match opp_off_sum.get(team_id) {
                Some((s, p)) if *p > 0.0 => s / p,
                _ => league_avg,
            };
            // Scale: if opponents' offense is weaker than avg, boost (worsen) our def rating
            let new_val = raw_def * (league_avg / avg_opp_off);
            let old_val = adj_def.get(team_id).copied().unwrap_or(league_avg);
            max_change = max_change.max((new_val - old_val).abs());
            adj_def.insert(*team_id, new_val);
        }

        if max_change < convergence_threshold {
            info!(
                iteration = iteration + 1,
                max_change, "adjusted efficiency converged"
            );
            break;
        }
    }

    // Compute SOS: average of opponents' adjusted efficiency margin
    let mut sos: HashMap<Uuid, f64> = HashMap::new();
    let mut opp_counts: HashMap<Uuid, (f64, u32)> = HashMap::new();
    for g in &game_data {
        let opp_margin = adj_off.get(&g.opponent_id).copied().unwrap_or(league_avg)
            - adj_def.get(&g.opponent_id).copied().unwrap_or(league_avg);
        let entry = opp_counts.entry(g.team_id).or_insert((0.0, 0));
        entry.0 += opp_margin;
        entry.1 += 1;
    }
    for (team_id, (total, count)) in &opp_counts {
        if *count > 0 {
            sos.insert(*team_id, total / *count as f64);
        }
    }

    // Rank SOS
    let mut sos_vec: Vec<(Uuid, f64)> = sos.iter().map(|(k, v)| (*k, *v)).collect();
    sos_vec.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let sos_ranks: HashMap<Uuid, i32> = sos_vec
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, (i + 1) as i32))
        .collect();

    // Write to DB
    let mut updated = 0u64;
    for (team_id, off) in &adj_off {
        let def = adj_def.get(team_id).copied().unwrap_or(league_avg);
        let margin = off - def;
        let team_sos = sos.get(team_id).copied();
        let team_sos_rank = sos_ranks.get(team_id).copied();

        let result = sqlx::query(
            "UPDATE team_season_stats SET
                adj_offense = ROUND($1::numeric, 1),
                adj_defense = ROUND($2::numeric, 1),
                adj_efficiency_margin = ROUND($3::numeric, 1),
                sos = ROUND($4::numeric, 1),
                sos_rank = $5,
                updated_at = now()
             WHERE team_id = $6 AND season = $7",
        )
        .bind(*off)
        .bind(def)
        .bind(margin)
        .bind(team_sos)
        .bind(team_sos_rank)
        .bind(team_id)
        .bind(season)
        .execute(pool)
        .await?;

        updated += result.rows_affected();
    }

    info!(
        updated,
        league_avg = format!("{:.1}", league_avg),
        season,
        "computed adjusted efficiency"
    );
    Ok(updated)
}

/// Compute per-player strength of schedule.
/// Player SOS = average adjusted efficiency margin of opponents the player actually faced,
/// weighted by minutes played in each game.
pub async fn compute_player_sos(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "WITH player_opp_strength AS (
            SELECT
                pgs.player_id,
                pgs.team_id,
                SUM(
                    COALESCE(tss.adj_efficiency_margin, 0) * COALESCE(pgs.minutes, 1)
                ) / NULLIF(SUM(COALESCE(pgs.minutes, 1)), 0) as weighted_opp_em
            FROM player_game_stats pgs
            LEFT JOIN team_season_stats tss ON tss.team_id = pgs.opponent_id AND tss.season = $1
            WHERE pgs.season = $1
              AND pgs.minutes IS NOT NULL
              AND pgs.minutes > 0
            GROUP BY pgs.player_id, pgs.team_id
        )
        UPDATE player_season_stats pss SET
            player_sos = ROUND(pos.weighted_opp_em::numeric, 1),
            updated_at = now()
        FROM player_opp_strength pos
        WHERE pss.player_id = pos.player_id
          AND pss.team_id = pos.team_id
          AND pss.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "computed player SOS"
    );
    Ok(result.rows_affected())
}

/// Compute rolling averages (last 5 games) for each player game.
/// Uses window functions to look at the previous 5 games by date.
pub async fn compute_rolling_averages(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "WITH rolling AS (
            SELECT
                pgs.id,
                AVG(pgs.points) OVER w as rolling_ppg,
                AVG(pgs.total_rebounds) OVER w as rolling_rpg,
                AVG(pgs.assists) OVER w as rolling_apg,
                CASE WHEN SUM(pgs.fga) OVER w > 0
                    THEN (SUM(pgs.fgm) OVER w)::float / (SUM(pgs.fga) OVER w)
                    ELSE NULL END as rolling_fg_pct,
                CASE WHEN ((SUM(pgs.fga) OVER w) + 0.44 * (SUM(COALESCE(pgs.fta, 0)) OVER w)) > 0
                    THEN (SUM(pgs.points) OVER w)::float /
                        (2.0 * ((SUM(pgs.fga) OVER w) + 0.44 * (SUM(COALESCE(pgs.fta, 0)) OVER w)))
                    ELSE NULL END as rolling_ts_pct,
                AVG(pgs.game_score) OVER w as rolling_game_score
            FROM player_game_stats pgs
            WHERE pgs.season = $1
              AND pgs.minutes IS NOT NULL
              AND pgs.minutes > 0
            WINDOW w AS (
                PARTITION BY pgs.player_id, pgs.team_id
                ORDER BY pgs.game_date
                ROWS BETWEEN 5 PRECEDING AND 1 PRECEDING
            )
        )
        UPDATE player_game_stats pgs SET
            rolling_ppg = ROUND(r.rolling_ppg::numeric, 1),
            rolling_rpg = ROUND(r.rolling_rpg::numeric, 1),
            rolling_apg = ROUND(r.rolling_apg::numeric, 1),
            rolling_fg_pct = ROUND(r.rolling_fg_pct::numeric, 3),
            rolling_ts_pct = ROUND(r.rolling_ts_pct::numeric, 3),
            rolling_game_score = ROUND(r.rolling_game_score::numeric, 1)
        FROM rolling r
        WHERE pgs.id = r.id
          AND r.rolling_ppg IS NOT NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = result.rows_affected(),
        season, "computed rolling averages"
    );
    Ok(result.rows_affected())
}

/// Populate individual ORTG/DRTG/net_rating from Torvik passthrough.
///
/// The prior box-score heuristic produced unusable values — same family of
/// formula bug as cstat's old BPM/OBPM/DBPM (see ROADMAP "Compute Pipeline
/// Audit"). Torvik publishes per-player season ORTG/DRTG (Dean Oliver style)
/// that correlates ~1.0 with their reference implementation; we passthrough.
///
/// **For consumers:** `pss.offensive_rating` / `defensive_rating` / `net_rating`
/// now hold Torvik `o_rtg` / `d_rtg` values (rounded to one decimal). Players
/// without a Torvik match (~1.4%) have NULLs in these columns.
pub async fn compute_individual_ratings(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Clear stale heuristic values so unmatched Torvik players see NULLs
    // instead of garbage from prior pipeline runs.
    sqlx::query(
        "UPDATE player_season_stats
            SET offensive_rating = NULL, defensive_rating = NULL, net_rating = NULL
            WHERE season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    let r1 = sqlx::query(
        "UPDATE player_season_stats pss SET
            offensive_rating = ROUND(t.o_rtg::numeric, 1),
            defensive_rating = ROUND(t.d_rtg::numeric, 1),
            net_rating       = ROUND((t.o_rtg - t.d_rtg)::numeric, 1)
        FROM torvik_player_stats t
        WHERE pss.player_id = t.player_id
          AND pss.season = t.season
          AND pss.season = $1
          AND t.o_rtg IS NOT NULL
          AND t.d_rtg IS NOT NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = r1.rows_affected(),
        season, "populated ORTG/DRTG from Torvik passthrough"
    );
    Ok(r1.rows_affected())
}

// ---------------------------------------------------------------------------
// CamPom — composite player valuation (see docs/campom_methodology.md)
// ---------------------------------------------------------------------------

/// Tunable parameters for the CamPom composite. Each is the input to the
/// hyperparameter grid search planned in ROADMAP §4f, where the predict model
/// is the fitness function. Keep changes to one constant per PR.
pub const CAMPOM_OFFENSE_EXPONENT: f64 = 0.7;
pub const CAMPOM_DEFENSE_DISCOUNT: f64 = 0.1;
pub const CAMPOM_USG_REF: f64 = 17.873_577_08;
pub const CAMPOM_MINUTES_EXPONENT: f64 = 0.5;
pub const CAMPOM_GP_K: f64 = 8.0;
pub const CAMPOM_SOS_TRANSFER_RATE: f64 = 0.5;
/// Transfer rate applied to player-level SOS in the parallel `_psos` tier.
/// Scaled down from the conference-SOS rate because `player_sos` (cstat
/// minutes-weighted opponent adj-efficiency-margin) has ~2.5× the magnitude
/// of `conf_sos` (CamPom GBPM units). 0.15 gives a Big Ten player roughly
/// the same ±2 GBPM adjustment as the conf-SOS path.
pub const CAMPOM_PLAYER_SOS_TRANSFER_RATE: f64 = 0.15;
/// Minimum games-played threshold for a player to count toward conference
/// quality (`conf_sos`). Filters out small-sample noise from the SOS table.
pub const CAMPOM_SOS_MIN_GP: i32 = 20;

/// Compute CamPom composite player-valuation metrics.
///
/// Reads inputs from `torvik_player_stats` (`ogbpm`, `dgbpm`, `usage_rate`,
/// `min_per`, `total_minutes`, `games_played`, `conf`) and writes the full
/// chain of intermediates and final composites back to the same row.
///
/// The o-side and d-side components of `adj_gbpm` are tracked separately so
/// each tier (`cam_gbpm`, `cam_gbpm_v2`, `cam_gbpm_v3`) gets first-class
/// offensive / defensive splits in addition to the total.
pub async fn compute_campom(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Wipe any stale composite values first so unmatched / unqualified rows
    // (missing inputs) end up NULL rather than retaining last run's numbers.
    sqlx::query(
        "UPDATE torvik_player_stats SET
             min_factor = NULL, mp_factor = NULL, gp_weight = NULL,
             adj_gbpm = NULL, conf_sos = NULL, sos_adj = NULL, adj_gbpm_sos = NULL,
             cam_gbpm = NULL, cam_o_gbpm = NULL, cam_d_gbpm = NULL, min_adj_gbpm = NULL,
             cam_gbpm_v2 = NULL, cam_o_gbpm_v2 = NULL, cam_d_gbpm_v2 = NULL, min_adj_gbpm_v2 = NULL,
             cam_gbpm_v3 = NULL, cam_o_gbpm_v3 = NULL, cam_d_gbpm_v3 = NULL, min_adj_gbpm_v3 = NULL,
             psos_adj = NULL, adj_gbpm_psos = NULL,
             cam_gbpm_v3_psos = NULL, cam_o_gbpm_v3_psos = NULL,
             cam_d_gbpm_v3_psos = NULL, min_adj_gbpm_v3_psos = NULL,
             cam_gbpm_v3_psos_pct = NULL,
             updated_at = now()
         WHERE season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    // Season constants. Computed over the full cohort (no GP filter) per the
    // methodology doc; only the SOS table uses the GP>=20 stable subset.
    //
    // Column-naming gotcha: `torvik_player_stats.total_minutes` actually holds
    // minutes-per-game (Torvik's `mp`), and `minutes_per_game` actually holds
    // Min% (Torvik's `Min_per`, copied to the new `min_per` column in
    // migration 014). The misnomers predate this work; CamPom reads what each
    // column truly contains. A schema rename is a follow-up.
    let row: (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT AVG(total_minutes)::float8 AS mean_mp,
                AVG(min_per)::float8 AS mean_min_per
           FROM torvik_player_stats
          WHERE season = $1
            AND ogbpm IS NOT NULL AND dgbpm IS NOT NULL
            AND usage_rate IS NOT NULL AND min_per IS NOT NULL
            AND total_minutes IS NOT NULL AND games_played IS NOT NULL
            AND games_played > 0",
    )
    .bind(season)
    .fetch_one(pool)
    .await?;

    let (mean_mp, mean_min_per) = match (row.0, row.1) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => (a, b),
        _ => {
            info!(season, "compute_campom: no qualified torvik rows; skipping");
            return Ok(0);
        }
    };

    info!(season, mean_mp, mean_min_per, "CamPom season constants");

    // Step 1-3: per-row intermediates and conference-neutral composites.
    // Done in one UPDATE; SOS is layered on after we know per-conference means.
    //
    // adj_gbpm offense component: OGBPM × (USG/USG_REF)^OFFENSE_EXPONENT × ...
    // adj_gbpm defense component: DGBPM × (1 − DEFENSE_DISCOUNT × USG/USG_REF)
    let r1 = sqlx::query(
        "UPDATE torvik_player_stats SET
             min_factor   = power(total_minutes / $2, $5),
             mp_factor    = power(min_per / $3, $5),
             gp_weight    = games_played::float8 / (games_played::float8 + $4),
             adj_gbpm     = ogbpm * power(usage_rate / $6, $7)
                          + dgbpm * (1.0 - $8 * usage_rate / $6),
             cam_o_gbpm   = ogbpm * power(usage_rate / $6, $7)
                          * power(min_per / $3, $5),
             cam_d_gbpm   = dgbpm * (1.0 - $8 * usage_rate / $6)
                          * power(min_per / $3, $5),
             cam_gbpm     = (ogbpm * power(usage_rate / $6, $7)
                           + dgbpm * (1.0 - $8 * usage_rate / $6))
                          * power(min_per / $3, $5),
             min_adj_gbpm = (ogbpm * power(usage_rate / $6, $7)
                           + dgbpm * (1.0 - $8 * usage_rate / $6))
                          * power(total_minutes / $2, $5),
             updated_at   = now()
         WHERE season = $1
           AND ogbpm IS NOT NULL AND dgbpm IS NOT NULL
           AND usage_rate IS NOT NULL
           AND min_per IS NOT NULL AND min_per > 0
           AND total_minutes IS NOT NULL AND games_played IS NOT NULL
           AND games_played > 0",
    )
    .bind(season) // $1
    .bind(mean_mp) // $2
    .bind(mean_min_per) // $3
    .bind(CAMPOM_GP_K) // $4
    .bind(CAMPOM_MINUTES_EXPONENT) // $5
    .bind(CAMPOM_USG_REF) // $6
    .bind(CAMPOM_OFFENSE_EXPONENT) // $7
    .bind(CAMPOM_DEFENSE_DISCOUNT) // $8
    .execute(pool)
    .await?;

    // Tier 2: GP-shrunk versions (× gp_weight)
    sqlx::query(
        "UPDATE torvik_player_stats SET
             cam_gbpm_v2     = cam_gbpm     * gp_weight,
             cam_o_gbpm_v2   = cam_o_gbpm   * gp_weight,
             cam_d_gbpm_v2   = cam_d_gbpm   * gp_weight,
             min_adj_gbpm_v2 = min_adj_gbpm * gp_weight
         WHERE season = $1 AND adj_gbpm IS NOT NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    // Step 4: conference SOS, restricted to stable estimates (GP >= threshold).
    // conf_sos = avg(adj_gbpm in conference) − overall_mean(adj_gbpm).
    let r4 = sqlx::query(
        "WITH stable AS (
             SELECT conf, adj_gbpm
               FROM torvik_player_stats
              WHERE season = $1
                AND games_played >= $2
                AND adj_gbpm IS NOT NULL
                AND conf IS NOT NULL
         ),
         overall AS (SELECT AVG(adj_gbpm) AS mean FROM stable),
         conf_q  AS (SELECT conf, AVG(adj_gbpm) - (SELECT mean FROM overall) AS sos
                       FROM stable GROUP BY conf)
         UPDATE torvik_player_stats t SET
             conf_sos     = c.sos,
             sos_adj      = c.sos * $3,
             adj_gbpm_sos = t.adj_gbpm + c.sos * $3
           FROM conf_q c
          WHERE t.season = $1
            AND t.conf = c.conf
            AND t.adj_gbpm IS NOT NULL",
    )
    .bind(season) // $1
    .bind(CAMPOM_SOS_MIN_GP) // $2
    .bind(CAMPOM_SOS_TRANSFER_RATE) // $3
    .execute(pool)
    .await?;

    // Tier 3: SOS-then-volume-then-shrinkage. SOS is applied on top of
    // adj_gbpm so it scales with mp_factor (and is subsequently shrunk by GP).
    //
    // Offensive / defensive split of SOS: proportional to each side's signed
    // contribution to adj_gbpm. If adj_gbpm is ~0, fall back to a 50/50 split
    // to avoid divide-by-zero blowups (only ~handful of players land there).
    sqlx::query(
        "UPDATE torvik_player_stats SET
             cam_gbpm_v3     = adj_gbpm_sos * mp_factor * gp_weight,
             min_adj_gbpm_v3 = adj_gbpm_sos * min_factor * gp_weight,
             cam_o_gbpm_v3   = (
                 ogbpm * power(usage_rate / $2, $3)
                 + CASE
                     WHEN abs(adj_gbpm) > 1e-9
                       THEN sos_adj * (ogbpm * power(usage_rate / $2, $3)) / adj_gbpm
                     ELSE sos_adj * 0.5
                   END
               ) * mp_factor * gp_weight,
             cam_d_gbpm_v3   = (
                 dgbpm * (1.0 - $4 * usage_rate / $2)
                 + CASE
                     WHEN abs(adj_gbpm) > 1e-9
                       THEN sos_adj * (dgbpm * (1.0 - $4 * usage_rate / $2)) / adj_gbpm
                     ELSE sos_adj * 0.5
                   END
               ) * mp_factor * gp_weight
         WHERE season = $1
           AND adj_gbpm_sos IS NOT NULL",
    )
    .bind(season) // $1
    .bind(CAMPOM_USG_REF) // $2
    .bind(CAMPOM_OFFENSE_EXPONENT) // $3
    .bind(CAMPOM_DEFENSE_DISCOUNT) // $4
    .execute(pool)
    .await?;

    // Parallel Tier-3: same machinery, but with cstat's player-level SOS
    // (`player_season_stats.player_sos`, minutes-weighted opponent
    // adj-efficiency-margin) instead of conference-level. Only populated for
    // players with a `player_sos` row; others stay NULL.
    let r_psos = sqlx::query(
        "UPDATE torvik_player_stats t SET
             psos_adj      = pss.player_sos * $2,
             adj_gbpm_psos = t.adj_gbpm + pss.player_sos * $2,
             cam_gbpm_v3_psos     = (t.adj_gbpm + pss.player_sos * $2)
                                  * t.mp_factor * t.gp_weight,
             min_adj_gbpm_v3_psos = (t.adj_gbpm + pss.player_sos * $2)
                                  * t.min_factor * t.gp_weight,
             cam_o_gbpm_v3_psos   = (
                 t.ogbpm * power(t.usage_rate / $3, $4)
                 + CASE
                     WHEN abs(t.adj_gbpm) > 1e-9
                       THEN (pss.player_sos * $2)
                              * (t.ogbpm * power(t.usage_rate / $3, $4))
                              / t.adj_gbpm
                     ELSE (pss.player_sos * $2) * 0.5
                   END
               ) * t.mp_factor * t.gp_weight,
             cam_d_gbpm_v3_psos   = (
                 t.dgbpm * (1.0 - $5 * t.usage_rate / $3)
                 + CASE
                     WHEN abs(t.adj_gbpm) > 1e-9
                       THEN (pss.player_sos * $2)
                              * (t.dgbpm * (1.0 - $5 * t.usage_rate / $3))
                              / t.adj_gbpm
                     ELSE (pss.player_sos * $2) * 0.5
                   END
               ) * t.mp_factor * t.gp_weight
           FROM player_season_stats pss
          WHERE pss.player_id = t.player_id
            AND pss.season = t.season
            AND t.season = $1
            AND t.adj_gbpm IS NOT NULL
            AND pss.player_sos IS NOT NULL",
    )
    .bind(season) // $1
    .bind(CAMPOM_PLAYER_SOS_TRANSFER_RATE) // $2
    .bind(CAMPOM_USG_REF) // $3
    .bind(CAMPOM_OFFENSE_EXPONENT) // $4
    .bind(CAMPOM_DEFENSE_DISCOUNT) // $5
    .execute(pool)
    .await?;

    // Percentile companion for the canonical site-wide CamPom score.
    // Restricted to qualified players (>=10 GP, >=10 MPG via the misnamed
    // `total_minutes` column which actually holds MP). Unqualified players
    // get NULL — the API/UI defaults filter them out.
    let r_pct = sqlx::query(
        "WITH ranked AS (
             SELECT torvik_pid, season,
                    PERCENT_RANK() OVER (ORDER BY cam_gbpm_v3_psos) AS pct
               FROM torvik_player_stats
              WHERE season = $1
                AND cam_gbpm_v3_psos IS NOT NULL
                AND games_played >= 10
                AND total_minutes >= 10
         )
         UPDATE torvik_player_stats t
            SET cam_gbpm_v3_psos_pct = r.pct
           FROM ranked r
          WHERE t.torvik_pid = r.torvik_pid
            AND t.season = r.season",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        per_player = r1.rows_affected(),
        with_sos = r4.rows_affected(),
        with_psos = r_psos.rows_affected(),
        with_pct = r_pct.rows_affected(),
        season,
        "computed CamPom composites"
    );
    Ok(r1.rows_affected())
}

/// Derive is_conference flag on games where both teams share a conference.
/// Also backfill point_diff on team_season_stats from team_game_stats.
pub async fn compute_derived_game_fields(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Backfill teams.conference from the most common league in team_game_stats
    let r0 = sqlx::query(
        "UPDATE teams t SET conference = sub.league, updated_at = now()
        FROM (
            SELECT team_id, league, ROW_NUMBER() OVER (PARTITION BY team_id ORDER BY COUNT(*) DESC) as rn
            FROM team_game_stats
            WHERE season = $1 AND league IS NOT NULL AND league != ''
            GROUP BY team_id, league
        ) sub
        WHERE t.id = sub.team_id AND sub.rn = 1
          AND t.season = $1
          AND t.conference IS NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;
    if r0.rows_affected() > 0 {
        info!(count = r0.rows_affected(), "backfilled team conferences");
    }

    // is_conference: both teams in same conference
    let r1 = sqlx::query(
        "UPDATE games g SET is_conference = (
            SELECT ht.conference = at.conference AND ht.conference IS NOT NULL
            FROM teams ht, teams at
            WHERE ht.id = g.home_team_id AND at.id = g.away_team_id
        )
        WHERE g.season = $1
          AND g.home_team_id IS NOT NULL
          AND g.away_team_id IS NOT NULL
          AND g.is_conference IS NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    // point_diff: fill from team_game_stats averages
    let r2 = sqlx::query(
        "UPDATE team_season_stats tss SET
            point_diff = sub.avg_diff
        FROM (
            SELECT tgs.team_id,
                ROUND(AVG(tgs.points - opp.points)::numeric, 1) as avg_diff
            FROM team_game_stats tgs
            JOIN team_game_stats opp ON opp.game_id = tgs.game_id AND opp.team_id = tgs.opponent_id
            WHERE tgs.season = $1 AND tgs.points IS NOT NULL AND opp.points IS NOT NULL
            GROUP BY tgs.team_id
        ) sub
        WHERE tss.team_id = sub.team_id
          AND tss.season = $1
          AND tss.point_diff IS NULL",
    )
    .bind(season)
    .execute(pool)
    .await?;

    let total = r1.rows_affected() + r2.rows_affected();
    info!(
        is_conference = r1.rows_affected(),
        point_diff = r2.rows_affected(),
        season,
        "computed derived game fields"
    );
    Ok(total)
}

/// Run all compute steps in order.
pub async fn compute_all(pool: &PgPool, season: i32) -> Result<ComputeReport, sqlx::Error> {
    let mut report = ComputeReport::default();

    info!(season, "starting compute pipeline");

    info!("step 1/13: deduplicating players");
    report.deduplicated_players = deduplicate_players(pool, season).await?;

    info!("step 2/13: backfilling derived game stats");
    report.backfilled = backfill_game_stats(pool).await?;

    info!("step 3/13: estimating missing team defensive rebounds");
    report.estimated_rebounds = estimate_missing_team_rebounds(pool, season).await?;

    info!("step 4/13: computing player season stats (with rate stats)");
    report.player_season_stats = compute_player_season_stats(pool, season).await?;

    info!("step 5/13: computing team four factors");
    report.team_four_factors = compute_team_four_factors(pool, season).await?;

    info!("step 6/13: computing adjusted efficiency (KenPom-style)");
    report.adjusted_efficiency = compute_adjusted_efficiency(pool, season).await?;

    info!("step 7/13: computing individual ORTG/DRTG (Torvik passthrough)");
    report.individual_ratings = compute_individual_ratings(pool, season).await?;

    info!("step 8/13: computing player SOS");
    report.player_sos = compute_player_sos(pool, season).await?;

    info!("step 9/13: computing CamPom composites");
    report.campom = compute_campom(pool, season).await?;

    info!("step 10/13: computing rolling averages");
    report.rolling_averages = compute_rolling_averages(pool, season).await?;

    info!("step 11/13: computing derived game fields");
    report.derived_fields = compute_derived_game_fields(pool, season).await?;

    info!("step 12/13: computing schedules");
    report.schedules = compute_schedules(pool, season).await?;

    info!("step 13/13: computing player percentiles");
    report.percentiles = compute_player_percentiles(pool, season).await?;

    info!(season, "compute pipeline complete");
    Ok(report)
}

#[derive(Debug, Default)]
pub struct ComputeReport {
    pub deduplicated_players: u64,
    pub backfilled: u64,
    pub estimated_rebounds: u64,
    pub player_season_stats: u64,
    pub team_four_factors: u64,
    pub adjusted_efficiency: u64,
    pub individual_ratings: u64,
    pub campom: u64,
    pub player_sos: u64,
    pub rolling_averages: u64,
    pub derived_fields: u64,
    pub schedules: u64,
    pub percentiles: u64,
}

impl std::fmt::Display for ComputeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Computed: {} deduped, {} backfilled, {} est rebounds, {} player stats, {} four factors, {} adj eff, {} ORTG/DRTG, {} CamPom, {} player SOS, {} rolling avgs, {} derived fields, {} schedules, {} percentiles",
            self.deduplicated_players,
            self.backfilled,
            self.estimated_rebounds,
            self.player_season_stats,
            self.team_four_factors,
            self.adjusted_efficiency,
            self.individual_ratings,
            self.campom,
            self.player_sos,
            self.rolling_averages,
            self.derived_fields,
            self.schedules,
            self.percentiles
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    // -- Possessions --------------------------------------------------------

    #[test]
    fn possessions_typical_game() {
        // Duke: 60 FGA, 10 OREB, 12 TOV, 20 FTA
        // Expected: 60 - 10 + 12 + 0.44*20 = 70.8
        assert!(approx(possessions(60.0, 10.0, 12.0, 20.0), 70.8, 0.01));
    }

    #[test]
    fn possessions_zero_free_throws() {
        // No free throws: 55 FGA, 8 OREB, 10 TOV, 0 FTA
        assert!(approx(possessions(55.0, 8.0, 10.0, 0.0), 57.0, 0.01));
    }

    #[test]
    fn possessions_all_zeros() {
        assert!(approx(possessions(0.0, 0.0, 0.0, 0.0), 0.0, 0.01));
    }

    // -- Game Score (Hollinger) ---------------------------------------------

    #[test]
    fn game_score_typical_line() {
        // ~20 pts, 8-15 FG, 2-4 FT, 2 OREB, 5 DREB, 1 STL, 5 AST, 1 BLK, 2 PF, 3 TOV
        // GmSc = 20 + 3.2 - 10.5 - 0.8 + 1.4 + 1.5 + 1 + 3.5 + 0.7 - 0.8 - 3 = 16.2
        let gs = game_score(&BoxScore {
            pts: 20.0,
            fgm: 8.0,
            fga: 15.0,
            ftm: 2.0,
            fta: 4.0,
            oreb: 2.0,
            dreb: 5.0,
            stl: 1.0,
            ast: 5.0,
            blk: 1.0,
            pf: 2.0,
            tov: 3.0,
        });
        assert!(approx(gs, 16.2, 0.01));
    }

    #[test]
    fn game_score_zero_stat_line() {
        let gs = game_score(&BoxScore {
            pts: 0.0,
            fgm: 0.0,
            fga: 0.0,
            ftm: 0.0,
            fta: 0.0,
            oreb: 0.0,
            dreb: 0.0,
            stl: 0.0,
            ast: 0.0,
            blk: 0.0,
            pf: 0.0,
            tov: 0.0,
        });
        assert!(approx(gs, 0.0, 0.01));
    }

    #[test]
    fn game_score_bad_game() {
        // 0 pts, 0-5 FG, 0-0 FT, 0 OREB, 1 DREB, 0 STL, 0 AST, 0 BLK, 4 PF, 4 TOV
        // GmSc = 0 + 0 - 3.5 - 0 + 0 + 0.3 + 0 + 0 + 0 - 1.6 - 4 = -8.8
        let gs = game_score(&BoxScore {
            pts: 0.0,
            fgm: 0.0,
            fga: 5.0,
            ftm: 0.0,
            fta: 0.0,
            oreb: 0.0,
            dreb: 1.0,
            stl: 0.0,
            ast: 0.0,
            blk: 0.0,
            pf: 4.0,
            tov: 4.0,
        });
        assert!(approx(gs, -8.8, 0.01));
    }

    // -- Effective FG% ------------------------------------------------------

    #[test]
    fn efg_typical() {
        // 8 FGM, 2 3PM, 15 FGA → (8 + 1) / 15 = 0.6
        assert!(approx(
            effective_fg_pct(8.0, 2.0, 15.0).unwrap(),
            0.6,
            0.001
        ));
    }

    #[test]
    fn efg_no_threes() {
        // 8 FGM, 0 3PM, 15 FGA → 8/15 = 0.533
        assert!(approx(
            effective_fg_pct(8.0, 0.0, 15.0).unwrap(),
            0.5333,
            0.001
        ));
    }

    #[test]
    fn efg_zero_fga() {
        assert!(effective_fg_pct(0.0, 0.0, 0.0).is_none());
    }

    // -- True Shooting % ----------------------------------------------------

    #[test]
    fn ts_typical() {
        // 20 pts, 15 FGA, 4 FTA → 20 / (2 * (15 + 0.44*4)) = 20 / (2 * 16.76) = 20/33.52 = 0.5966
        assert!(approx(
            true_shooting_pct(20.0, 15.0, 4.0).unwrap(),
            0.5966,
            0.001
        ));
    }

    #[test]
    fn ts_zero_attempts() {
        assert!(true_shooting_pct(0.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn ts_pure_free_throw_scorer() {
        // 10 pts, 0 FGA, 10 FTA → 10 / (2 * (0 + 4.4)) = 10/8.8 = 1.136
        // TS% > 1 is possible with pure FT scoring (theoretical edge case)
        assert!(approx(
            true_shooting_pct(10.0, 0.0, 10.0).unwrap(),
            1.1364,
            0.001
        ));
    }

    // -- Turnover % ---------------------------------------------------------

    #[test]
    fn tov_pct_typical() {
        // 3 TOV, 15 FGA, 4 FTA → 3 / (15 + 0.44*4 + 3) = 3 / 19.76 = 0.1518
        assert!(approx(turnover_pct(3.0, 15.0, 4.0).unwrap(), 0.1518, 0.001));
    }

    #[test]
    fn tov_pct_zero_usage() {
        assert!(turnover_pct(0.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn tov_pct_all_turnovers() {
        // Degenerate: 5 TOV, 0 FGA, 0 FTA → 5/5 = 1.0
        assert!(approx(turnover_pct(5.0, 0.0, 0.0).unwrap(), 1.0, 0.001));
    }

    // -- Assist-to-Turnover Ratio -------------------------------------------

    #[test]
    fn atr_typical() {
        assert!(approx(ast_to_ratio(6.0, 3.0), 2.0, 0.001));
    }

    #[test]
    fn atr_zero_turnovers_with_assists() {
        assert!(approx(ast_to_ratio(5.0, 0.0), 5.0, 0.001));
    }

    #[test]
    fn atr_zero_both() {
        assert!(approx(ast_to_ratio(0.0, 0.0), 0.0, 0.001));
    }

    // -- ComputeReport Display ----------------------------------------------

    #[test]
    fn compute_report_display() {
        let report = ComputeReport::default();
        let s = format!("{report}");
        assert!(s.contains("0 deduped"));
        assert!(s.contains("0 percentiles"));
    }
}
