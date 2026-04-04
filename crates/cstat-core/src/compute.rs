use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

/// Backfill derived columns on player_game_stats that can be computed from existing data.
pub async fn backfill_game_stats(pool: &PgPool) -> Result<u64, sqlx::Error> {
    // def_rebounds = total_rebounds - off_rebounds
    let r1 = sqlx::query(
        "UPDATE player_game_stats
         SET def_rebounds = total_rebounds - off_rebounds
         WHERE total_rebounds IS NOT NULL
           AND off_rebounds IS NOT NULL
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
            usage_rate, bpm, ast_pct, tov_pct, orb_pct, stl_pct, blk_pct
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
            ROUND(AVG(pgs.usage_rate)::numeric, 1),
            -- BPM placeholder (avg game_score as proxy until we compute real BPM)
            ROUND(AVG(pgs.game_score)::numeric, 1),
            -- AST% = AST / (teammate FGM while on floor) — approximate as AST / (team_poss * minutes_share)
            NULL,
            -- TOV% = TOV / (FGA + 0.44 * FTA + TOV)
            CASE WHEN (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))) > 0
                THEN ROUND((SUM(COALESCE(pgs.turnovers, 0))::float /
                    (SUM(pgs.fga) + 0.44 * SUM(COALESCE(pgs.fta, 0)) + SUM(COALESCE(pgs.turnovers, 0))))::numeric, 3)
                ELSE NULL END,
            -- ORB% approximation (player OREB / team OREB opportunities) — use raw avg for now
            NULL,
            -- STL% and BLK% — need team possession data for proper calc, use per-minute proxies
            NULL,
            NULL
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
            usage_rate_pct, bpm_pct
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
            PERCENT_RANK() OVER (ORDER BY pss.bpm)
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
            bpm_pct = EXCLUDED.bpm_pct",
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
                SUM(COALESCE(tgs.total_rebounds, 0) - COALESCE(tgs.off_rebounds, 0)) as dreb,
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
                SUM(COALESCE(tgs.total_rebounds, 0) - COALESCE(tgs.off_rebounds, 0)) as opp_dreb,
                SUM(tgs.turnovers) as opp_tov,
                SUM(tgs.points) as opp_pts,
                SUM(tgs.fga) - SUM(tgs.off_rebounds) + SUM(tgs.turnovers) + 0.44 * SUM(tgs.fta) as opp_poss
            FROM team_game_stats tgs
            WHERE tgs.season = $1
              AND tgs.opponent_id IS NOT NULL
            GROUP BY tgs.opponent_id
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
            off_rebound_pct = ROUND((t.oreb::float / NULLIF(t.oreb + COALESCE(o.opp_dreb, 0), 0))::numeric, 3),
            ft_rate = ROUND((t.fta::float / NULLIF(t.fga, 0))::numeric, 3),
            -- Defensive four factors
            opp_effective_fg_pct = ROUND(((o.opp_fgm + 0.5 * o.opp_tpm)::float / NULLIF(o.opp_fga, 0))::numeric, 3),
            opp_turnover_pct = ROUND((o.opp_tov::float / NULLIF(o.opp_poss, 0))::numeric, 3),
            opp_ft_rate = ROUND((o.opp_fta::float / NULLIF(o.opp_fga, 0))::numeric, 3),
            -- DRB% = team_DREB / (team_DREB + opp_OREB)
            def_rebound_pct = ROUND((t.dreb::float / NULLIF(t.dreb + COALESCE(o.opp_oreb, 0), 0))::numeric, 3),
            updated_at = now()
        FROM team_agg t
        LEFT JOIN opp_agg o ON t.team_id = o.team_id
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
pub async fn compute_adjusted_efficiency(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Fetch all team game stats: team_id, opponent_id, points, possessions (estimated)
    let games: Vec<(
        Uuid,
        Option<Uuid>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
    )> = sqlx::query_as(
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
    // Also collect opponent points/possessions for defensive efficiency
    // We need to pair games: team A vs team B appears twice (once per team)
    // Group by (team_id, opponent_id) pairs

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

/// Run all compute steps in order.
pub async fn compute_all(pool: &PgPool, season: i32) -> Result<ComputeReport, sqlx::Error> {
    let mut report = ComputeReport::default();

    info!(season, "starting compute pipeline");

    info!("step 1/6: backfilling derived game stats");
    report.backfilled = backfill_game_stats(pool).await?;

    info!("step 2/6: computing player season stats");
    report.player_season_stats = compute_player_season_stats(pool, season).await?;

    info!("step 3/6: computing team four factors");
    report.team_four_factors = compute_team_four_factors(pool, season).await?;

    info!("step 4/6: computing adjusted efficiency (KenPom-style)");
    report.adjusted_efficiency = compute_adjusted_efficiency(pool, season).await?;

    info!("step 5/6: computing schedules");
    report.schedules = compute_schedules(pool, season).await?;

    info!("step 6/6: computing player percentiles");
    report.percentiles = compute_player_percentiles(pool, season).await?;

    info!(season, "compute pipeline complete");
    Ok(report)
}

#[derive(Debug, Default)]
pub struct ComputeReport {
    pub backfilled: u64,
    pub player_season_stats: u64,
    pub team_four_factors: u64,
    pub adjusted_efficiency: u64,
    pub schedules: u64,
    pub percentiles: u64,
}

impl std::fmt::Display for ComputeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Computed: {} backfilled, {} player stats, {} four factors, {} adj efficiency, {} schedules, {} percentiles",
            self.backfilled,
            self.player_season_stats,
            self.team_four_factors,
            self.adjusted_efficiency,
            self.schedules,
            self.percentiles
        )
    }
}
