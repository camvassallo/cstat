use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

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

    let total = r1.rows_affected() + r2.rows_affected() + r3.rows_affected();
    info!(
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
            usage_rate, bpm, ast_pct, tov_pct, orb_pct, drb_pct, stl_pct, blk_pct
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
            -- Usage rate (avg of per-game NatStat usage)
            ROUND(AVG(pgs.usage_rate)::numeric, 3),
            -- BPM placeholder (avg game_score as proxy until we compute real BPM)
            ROUND(AVG(pgs.game_score)::numeric, 2),
            -- AST% = AST / (teammate FGM) ≈ AST / (team_FGM - player_FGM) * (team_minutes / player_minutes)
            -- Simplified: AST / (team_FGM_per_min * minutes - FGM) where team_FGM_per_min comes from team_fgm
            CASE WHEN SUM(COALESCE(pgs.team_fgm, 0)) - SUM(pgs.fgm) > 0
                THEN ROUND((SUM(pgs.assists)::float /
                    (SUM(COALESCE(pgs.team_fgm, 0))::float - SUM(pgs.fgm)::float))::numeric, 3)
                ELSE NULL END,
            -- TOV% = TOV / (FGA + 0.44 * FTA + TOV)
            CASE WHEN (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))) > 0
                THEN ROUND((SUM(COALESCE(pgs.turnovers, 0))::float /
                    (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))))::numeric, 3)
                ELSE NULL END,
            -- ORB% = player_OREB / (player_OREB + opp_DREB_while_on_floor)
            -- Approximate: player_OREB / (player_minutes/team_minutes * team_OREB_opportunities)
            -- Simpler proxy: OREB per minute / league avg OREB per minute
            CASE WHEN SUM(pgs.minutes) > 0 AND SUM(COALESCE(pgs.off_rebounds, 0)) > 0
                THEN ROUND((SUM(COALESCE(pgs.off_rebounds, 0))::float / SUM(pgs.minutes) * 40.0)::numeric, 1)
                ELSE 0.0 END,
            -- DRB% = DREB per 40 minutes as proxy
            CASE WHEN SUM(pgs.minutes) > 0 AND SUM(COALESCE(pgs.def_rebounds, 0)) > 0
                THEN ROUND((SUM(COALESCE(pgs.def_rebounds, 0))::float / SUM(pgs.minutes) * 40.0)::numeric, 1)
                ELSE 0.0 END,
            -- STL% = steals / (minutes/team_minutes * opp_possessions)
            -- Approximate: STL per 40 minutes as proxy
            CASE WHEN SUM(pgs.minutes) > 0
                THEN ROUND((SUM(COALESCE(pgs.steals, 0))::float / SUM(pgs.minutes) * 40.0)::numeric, 1)
                ELSE 0.0 END,
            -- BLK% = blocks per 40 minutes as proxy
            CASE WHEN SUM(pgs.minutes) > 0
                THEN ROUND((SUM(COALESCE(pgs.blocks, 0))::float / SUM(pgs.minutes) * 40.0)::numeric, 1)
                ELSE 0.0 END
        FROM player_game_stats pgs
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
            fg_pct_pct, tp_pct_pct, ft_pct_pct, true_shooting_pct_pct,
            usage_rate_pct, offensive_rating_pct, defensive_rating_pct,
            bpm_pct, player_sos_pct
        )
        SELECT
            gen_random_uuid(),
            pss.player_id,
            pss.season,
            PERCENT_RANK() OVER (ORDER BY pss.ppg),
            PERCENT_RANK() OVER (ORDER BY pss.rpg),
            PERCENT_RANK() OVER (ORDER BY pss.apg),
            PERCENT_RANK() OVER (ORDER BY pss.spg),
            PERCENT_RANK() OVER (ORDER BY pss.bpg),
            PERCENT_RANK() OVER (ORDER BY pss.fg_pct),
            PERCENT_RANK() OVER (ORDER BY pss.tp_pct),
            PERCENT_RANK() OVER (ORDER BY pss.ft_pct),
            PERCENT_RANK() OVER (ORDER BY pss.true_shooting_pct),
            PERCENT_RANK() OVER (ORDER BY pss.usage_rate),
            PERCENT_RANK() OVER (ORDER BY pss.offensive_rating),
            PERCENT_RANK() OVER (ORDER BY pss.defensive_rating DESC),
            PERCENT_RANK() OVER (ORDER BY pss.bpm),
            PERCENT_RANK() OVER (ORDER BY pss.player_sos)
        FROM player_season_stats pss
        WHERE pss.season = $1
          AND pss.games_played >= 10
          AND pss.minutes_per_game >= 10
        ON CONFLICT (player_id, season) DO UPDATE
        SET ppg_pct = EXCLUDED.ppg_pct,
            rpg_pct = EXCLUDED.rpg_pct,
            apg_pct = EXCLUDED.apg_pct,
            spg_pct = EXCLUDED.spg_pct,
            bpg_pct = EXCLUDED.bpg_pct,
            fg_pct_pct = EXCLUDED.fg_pct_pct,
            tp_pct_pct = EXCLUDED.tp_pct_pct,
            ft_pct_pct = EXCLUDED.ft_pct_pct,
            true_shooting_pct_pct = EXCLUDED.true_shooting_pct_pct,
            usage_rate_pct = EXCLUDED.usage_rate_pct,
            offensive_rating_pct = EXCLUDED.offensive_rating_pct,
            defensive_rating_pct = EXCLUDED.defensive_rating_pct,
            bpm_pct = EXCLUDED.bpm_pct,
            player_sos_pct = EXCLUDED.player_sos_pct",
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
    // ORB% = OREB / (OREB + Opp DREB) — needs opponent data, approximate for now
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

/// Compute individual offensive/defensive rating and BPM splits.
///
/// Offensive rating (box-score approximation):
///   Points produced per 100 possessions, using Dean Oliver's simplified formula.
///   PProd ≈ (FGM + AST * 0.5) * (PTS / (FGM + AST * 0.5 + FTM * 0.5)) + FTM * 0.5
///   Individual possessions ≈ FGA + 0.44 * FTA + TOV
///   ORTG = PProd / individual_possessions * 100
///
/// Defensive rating: approximated from team defensive efficiency + individual defensive stats.
///   Base = team adj_defense, then adjust by steal/block/rebound contribution relative to team avg.
///
/// BPM split: use the offensive/defensive share of game_score.
///   OBPM ≈ BPM * (off_component / total_component)
///   DBPM ≈ BPM - OBPM
pub async fn compute_individual_ratings(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Step 1: Compute per-player ORTG from aggregated box scores
    let r1 = sqlx::query(
        "WITH player_prod AS (
            SELECT
                pss.player_id, pss.team_id,
                pss.ppg, pss.apg, pss.rpg, pss.spg, pss.bpg, pss.topg, pss.fpg,
                pss.fg_pct, pss.ft_pct, pss.games_played, pss.minutes_per_game,
                pss.bpm,
                -- Offensive components per game
                pss.ppg + 0.4 * (pss.ppg * COALESCE(pss.fg_pct, 0.4)) - 0.7 * (pss.ppg / NULLIF(COALESCE(pss.fg_pct, 0.4), 0) * (1 - COALESCE(pss.fg_pct, 0.4)))
                as off_component,
                -- Defensive components per game
                pss.rpg * 0.3 + pss.spg + pss.bpg * 0.7 - pss.fpg * 0.4
                as def_component,
                -- Individual possessions per game
                CASE WHEN pss.fg_pct IS NOT NULL AND pss.fg_pct > 0
                    THEN pss.ppg / NULLIF(pss.fg_pct, 0) + 0.44 * COALESCE(
                        pss.ppg * COALESCE(pss.ft_pct, 0.7) / NULLIF(COALESCE(pss.ft_pct, 0.7), 0), 0
                    ) * 0.44 + pss.topg
                    ELSE pss.ppg * 1.1 + pss.topg
                END as indiv_poss,
                tss.adj_offense as team_ortg,
                tss.adj_defense as team_drtg
            FROM player_season_stats pss
            LEFT JOIN team_season_stats tss ON tss.team_id = pss.team_id AND tss.season = $1
            WHERE pss.season = $1
              AND pss.minutes_per_game >= 5
              AND pss.games_played >= 3
        )
        UPDATE player_season_stats pss SET
            -- ORTG: scale by team context
            offensive_rating = ROUND(CASE
                WHEN pp.indiv_poss > 0 AND pp.team_ortg IS NOT NULL
                THEN pp.team_ortg * (1 + (pp.off_component - pp.ppg) / NULLIF(pp.ppg, 0) * 0.2)
                ELSE pp.team_ortg
            END::numeric, 1),
            -- DRTG: team base adjusted by individual defensive contribution
            defensive_rating = ROUND(CASE
                WHEN pp.team_drtg IS NOT NULL
                THEN pp.team_drtg * (1 - pp.def_component / 10.0 * 0.1)
                ELSE NULL
            END::numeric, 1),
            -- Net rating
            net_rating = ROUND(CASE
                WHEN pp.team_ortg IS NOT NULL AND pp.team_drtg IS NOT NULL
                THEN (pp.team_ortg * (1 + (pp.off_component - pp.ppg) / NULLIF(pp.ppg, 0) * 0.2))
                   - (pp.team_drtg * (1 - pp.def_component / 10.0 * 0.1))
                ELSE NULL
            END::numeric, 1),
            -- BPM split: offensive share
            obpm = ROUND(CASE
                WHEN (pp.off_component + pp.def_component) > 0
                THEN pp.bpm * pp.off_component / NULLIF(pp.off_component + GREATEST(pp.def_component, 0.1), 0)
                ELSE pp.bpm * 0.6
            END::numeric, 1),
            -- BPM split: defensive share
            dbpm = ROUND(CASE
                WHEN (pp.off_component + pp.def_component) > 0
                THEN pp.bpm * GREATEST(pp.def_component, 0.1) / NULLIF(pp.off_component + GREATEST(pp.def_component, 0.1), 0)
                ELSE pp.bpm * 0.4
            END::numeric, 1)
        FROM player_prod pp
        WHERE pss.player_id = pp.player_id
          AND pss.team_id = pp.team_id
          AND pss.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count = r1.rows_affected(),
        season, "computed individual ORTG/DRTG and BPM splits"
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

    info!("step 1/12: deduplicating players");
    report.deduplicated_players = deduplicate_players(pool, season).await?;

    info!("step 2/12: backfilling derived game stats");
    report.backfilled = backfill_game_stats(pool).await?;

    info!("step 3/12: estimating missing team defensive rebounds");
    report.estimated_rebounds = estimate_missing_team_rebounds(pool, season).await?;

    info!("step 4/12: computing player season stats (with rate stats)");
    report.player_season_stats = compute_player_season_stats(pool, season).await?;

    info!("step 5/12: computing team four factors");
    report.team_four_factors = compute_team_four_factors(pool, season).await?;

    info!("step 6/12: computing adjusted efficiency (KenPom-style)");
    report.adjusted_efficiency = compute_adjusted_efficiency(pool, season).await?;

    info!("step 7/12: computing individual ORTG/DRTG and BPM splits");
    report.individual_ratings = compute_individual_ratings(pool, season).await?;

    info!("step 8/12: computing player SOS");
    report.player_sos = compute_player_sos(pool, season).await?;

    info!("step 9/12: computing rolling averages");
    report.rolling_averages = compute_rolling_averages(pool, season).await?;

    info!("step 10/12: computing derived game fields");
    report.derived_fields = compute_derived_game_fields(pool, season).await?;

    info!("step 11/12: computing schedules");
    report.schedules = compute_schedules(pool, season).await?;

    info!("step 12/12: computing player percentiles");
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
            "Computed: {} deduped, {} backfilled, {} est rebounds, {} player stats, {} four factors, {} adj eff, {} ORTG/DRTG, {} player SOS, {} rolling avgs, {} derived fields, {} schedules, {} percentiles",
            self.deduplicated_players,
            self.backfilled,
            self.estimated_rebounds,
            self.player_season_stats,
            self.team_four_factors,
            self.adjusted_efficiency,
            self.individual_ratings,
            self.player_sos,
            self.rolling_averages,
            self.derived_fields,
            self.schedules,
            self.percentiles
        )
    }
}
