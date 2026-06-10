use sqlx::PgPool;
use sqlx::Row;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

/// Defensive self-heal: ensure every (team, season) with `team_game_stats`
/// rows also has a `team_season_stats` row. Without this, the compute
/// pipeline silently no-ops on the missing-row teams (four-factors / adj
/// eff / wins-losses are all UPDATEs, and UPDATE-without-row is a no-op),
/// leaving them with blank stats on the team-detail page.
///
/// The gap arises when a `teams` row gets created by the box-score path
/// (auto-create on first perf encounter) rather than the `/teams` API
/// step that normally seeds `team_season_stats`. Empirically this hits
/// D-I-transitioning programs: NatStat's `/teams` endpoint may not
/// return a team in its final D1 season (Hartford 2023, St. Francis NY
/// 2023) or its first D1 season (Le Moyne 2024). Idempotent: no-op when
/// every relevant row already exists.
async fn seed_missing_team_season_stats(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        INSERT INTO team_season_stats (id, team_id, season)
        SELECT gen_random_uuid(), t.id, t.season
        FROM teams t
        WHERE t.season = $1
          AND EXISTS (
              SELECT 1 FROM team_game_stats tgs
              WHERE tgs.team_id = t.id AND tgs.season = t.season
          )
          AND NOT EXISTS (
              SELECT 1 FROM team_season_stats tss
              WHERE tss.team_id = t.id AND tss.season = t.season
          )
        ON CONFLICT (team_id, season) DO NOTHING
        "#,
    )
    .bind(season)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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

            // Step 3.5: Reassign player_id on tables whose FK to players is
            // RESTRICT (no CASCADE) and whose rows are valuable enough to
            // keep through the merge. Torvik has UNIQUE(torvik_pid, season),
            // not on player_id, so two surviving rows for the merged player
            // is allowed; downstream consumers (compute_individual_ratings)
            // are tolerant of the duplication. transfers/recruits use
            // cstat_player_id (nullable) and are typically NULL during this
            // step, but get covered too so re-resolves don't regress later.
            sqlx::query("UPDATE torvik_player_stats SET player_id = $1 WHERE player_id = $2")
                .bind(primary_id)
                .bind(dup_id)
                .execute(pool)
                .await?;
            sqlx::query("UPDATE transfers SET cstat_player_id = $1 WHERE cstat_player_id = $2")
                .bind(primary_id)
                .bind(dup_id)
                .execute(pool)
                .await?;
            sqlx::query("UPDATE recruits SET cstat_player_id = $1 WHERE cstat_player_id = $2")
                .bind(primary_id)
                .bind(dup_id)
                .execute(pool)
                .await?;
            // play_by_play has a RESTRICT FK to players(id); reassign the dup's
            // PBP rows to the primary so the Step 4 delete doesn't violate it.
            // (Local-only table; on prod it's empty, so this is a no-op there.)
            sqlx::query("UPDATE play_by_play SET player_id = $1 WHERE player_id = $2")
                .bind(primary_id)
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
            -- Reads team totals from team_game_stats (populated for every ingested season)
            -- instead of pgs.team_fga / team_fta / team_turnovers (denormalized columns the
            -- bootstrap-csv path doesn't populate for pre-2021 seasons).
            CASE WHEN SUM(pgs.minutes) > 0
                  AND SUM(COALESCE(tgs.fga, 0) + 0.44 * COALESCE(tgs.fta, 0)
                          + COALESCE(tgs.turnovers, 0)) > 0
                THEN ROUND((
                    (SUM(pgs.fga + 0.44 * COALESCE(pgs.fta, 0) + COALESCE(pgs.turnovers, 0))::float
                        * (SUM(COALESCE(tgs.minutes, 200))::float / 5.0))
                    / (SUM(pgs.minutes)::float
                        * SUM(COALESCE(tgs.fga, 0) + 0.44 * COALESCE(tgs.fta, 0)
                              + COALESCE(tgs.turnovers, 0))::float)
                )::numeric, 3)
                ELSE NULL END,
            -- AST% (Basketball Reference): AST / ((MP / (Team_MP / 5)) × Team_FGM − Player_FGM)
            -- Stored as a fraction (multiply by 100 for percent).
            -- Reads team FGM from team_game_stats (authoritative, populated for every
            -- ingested season) instead of pgs.team_fgm (NatStat playerperfs passthrough,
            -- which 2024 and earlier seasons may be missing).
            CASE WHEN (5.0 * SUM(pgs.minutes)::float * SUM(COALESCE(tgs.fgm, 0))::float
                       / NULLIF(SUM(COALESCE(tgs.minutes, 200))::float, 0)
                       - SUM(pgs.fgm)::float) > 0
                THEN ROUND((SUM(pgs.assists)::float / (
                    5.0 * SUM(pgs.minutes)::float * SUM(COALESCE(tgs.fgm, 0))::float
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
            orb_pct_pct, drb_pct_pct, stl_pct_pct, blk_pct_pct, ft_rate_pct,
            paint_rate_pct, paint_fg_pct_pct, perimeter_fg_pct_pct,
            transition_pts_per40_pct, second_chance_pts_per40_pct,
            points_off_turnovers_per40_pct, fouls_drawn_per40_pct
        )
        WITH best AS (
            SELECT DISTINCT ON (player_id)
                player_id, season, ppg, rpg, apg, spg, bpg,
                fg_pct, tp_pct, ft_pct, effective_fg_pct, true_shooting_pct,
                usage_rate, offensive_rating, defensive_rating,
                player_sos, ast_pct, tov_pct, minutes_per_game, topg,
                orb_pct, drb_pct, stl_pct, blk_pct, ft_rate,
                paint_rate, paint_fg_pct, perimeter_fg_pct,
                transition_pts_per40, second_chance_pts_per40,
                points_off_turnovers_per40, fouls_drawn_per40
            FROM player_season_stats
            WHERE season = $1
              AND games_played >= 10
              AND minutes_per_game >= 10
            -- `team_id` is a deterministic tiebreak so a transfer with two
            -- team-rows tied on games_played always resolves to the same row
            -- (and matches the `rates` CTE in queries::get_player_pbp_profile,
            -- which orders identically — so a displayed rate and its percentile
            -- come from the same team-row).
            ORDER BY player_id, games_played DESC, team_id
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
            PERCENT_RANK() OVER (ORDER BY b.ft_rate),
            -- Tier-1 PBP rate percentiles, ranked over NON-NULL values only.
            -- A plain PERCENT_RANK would count no-PBP players (NULL, sorted last)
            -- in its denominator, compressing every real percentile. That's ~1%
            -- with a full season loaded but balloons in-season (early-season the
            -- partition is mostly sparse/NULL PBP), so we rank explicitly:
            --   (rank among non-NULL − 1) / (count of non-NULL − 1)
            -- which is PERCENT_RANK restricted to players who have the stat.
            -- count(x) OVER () counts non-NULLs; rank() puts NULLs last so they
            -- never shift a real row's rank; the CASE keeps no-data rows badge-less.
            CASE WHEN b.paint_rate IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.paint_rate) - 1.0) / nullif(count(b.paint_rate) OVER () - 1, 0) END,
            CASE WHEN b.paint_fg_pct IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.paint_fg_pct) - 1.0) / nullif(count(b.paint_fg_pct) OVER () - 1, 0) END,
            CASE WHEN b.perimeter_fg_pct IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.perimeter_fg_pct) - 1.0) / nullif(count(b.perimeter_fg_pct) OVER () - 1, 0) END,
            CASE WHEN b.transition_pts_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.transition_pts_per40) - 1.0) / nullif(count(b.transition_pts_per40) OVER () - 1, 0) END,
            CASE WHEN b.second_chance_pts_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.second_chance_pts_per40) - 1.0) / nullif(count(b.second_chance_pts_per40) OVER () - 1, 0) END,
            CASE WHEN b.points_off_turnovers_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.points_off_turnovers_per40) - 1.0) / nullif(count(b.points_off_turnovers_per40) OVER () - 1, 0) END,
            CASE WHEN b.fouls_drawn_per40 IS NULL THEN NULL ELSE (rank() OVER (ORDER BY b.fouls_drawn_per40) - 1.0) / nullif(count(b.fouls_drawn_per40) OVER () - 1, 0) END
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
            ft_rate_pct = EXCLUDED.ft_rate_pct,
            paint_rate_pct = EXCLUDED.paint_rate_pct,
            paint_fg_pct_pct = EXCLUDED.paint_fg_pct_pct,
            perimeter_fg_pct_pct = EXCLUDED.perimeter_fg_pct_pct,
            transition_pts_per40_pct = EXCLUDED.transition_pts_per40_pct,
            second_chance_pts_per40_pct = EXCLUDED.second_chance_pts_per40_pct,
            points_off_turnovers_per40_pct = EXCLUDED.points_off_turnovers_per40_pct,
            fouls_drawn_per40_pct = EXCLUDED.fouls_drawn_per40_pct",
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

    // wins/losses: derive from team_game_stats.win, which is authoritative for
    // the season (we already trust it for AdjEM, four factors, point_diff).
    // Unconditional overwrite — the team-detail ingest (NatStat /teams) also
    // writes these, but that path lags behind game ingest, so compute should
    // always have the last word. Without this, a season ingested before the
    // /teams call lands shows 0-0 records everywhere.
    let r3 = sqlx::query(
        "UPDATE team_season_stats tss SET
            wins = sub.wins,
            losses = sub.losses,
            updated_at = now()
        FROM (
            SELECT team_id,
                COUNT(*) FILTER (WHERE win IS TRUE)::int AS wins,
                COUNT(*) FILTER (WHERE win IS FALSE)::int AS losses
            FROM team_game_stats
            WHERE season = $1
            GROUP BY team_id
        ) sub
        WHERE tss.team_id = sub.team_id
          AND tss.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    let total = r1.rows_affected() + r2.rows_affected() + r3.rows_affected();
    info!(
        is_conference = r1.rows_affected(),
        point_diff = r2.rows_affected(),
        wins_losses = r3.rows_affected(),
        season,
        "computed derived game fields"
    );
    Ok(total)
}

/// Run all compute steps in order.
/// P2a: derive per-`(player, game)` play-by-play aggregates onto
/// `player_game_stats` from the local-only `play_by_play` table. Shot-location
/// split from the `paint` tag, context points from `brk` / `2ch` / `offto`, and
/// fouls drawn from `FOULED` (which marks who DREW the foul, not who shot the
/// FTs). These columns ship to prod; raw `play_by_play` does not.
///
/// Source-duplicate plays — NatStat occasionally emits one play twice (distinct
/// ids, identical content; see `docs/pbp_methodology.md`) — are collapsed by
/// `(game_id, sort_order, description, player_id)` before counting, so the
/// counts aren't inflated.
///
/// Idempotent and season-scoped: recomputes/overwrites every player-game that
/// has PBP. A player-game with no `play_by_play` rows keeps NULL columns
/// (NULL = no PBP data; 0 = had PBP but none of this event), so seasons without
/// PBP loaded are simply untouched.
/// Minimum share of box-score field-goal attempts that the PBP tag stream must
/// account for before we trust (and serve) a season's PBP-derived surfaces. A
/// clean season's `FGA`/`3FA`-tagged plays map ~1:1 to box FGA (every observed
/// season 2015-2026 except 2019 is >0.93); a corrupt source falls far below.
/// 2019's NatStat PBP export mis-tags made field goals as free throws, landing
/// at ~0.55 — caught here. See ROADMAP "2019 PBP tag corruption".
const PBP_MIN_FGA_COVERAGE: f64 = 0.80;

/// Fraction of box-score FGA that the season's PBP `FGA`+`3FA` tags account for.
/// `None` when there's no box FGA to compare against (can't evaluate — don't
/// gate). The signal is a cheap, source-agnostic corruption detector: it keys
/// on tag *coverage*, so it fires for a corrupt export, a partial in-season
/// load, or any future feed regression — not a hardcoded season list.
async fn pbp_fga_coverage(pool: &PgPool, season: i32) -> Result<Option<f64>, sqlx::Error> {
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT \
            (SELECT count(*) FROM play_by_play \
             WHERE season = $1 AND ('FGA' = ANY(tags) OR '3FA' = ANY(tags))), \
            (SELECT sum(fga)::bigint FROM team_game_stats WHERE season = $1)",
    )
    .bind(season)
    .fetch_one(pool)
    .await?;
    let pbp_fga = row.0.unwrap_or(0);
    match row.1 {
        Some(box_fga) if box_fga > 0 => Ok(Some(pbp_fga as f64 / box_fga as f64)),
        _ => Ok(None),
    }
}

/// True (with a warning) when the season's PBP tag stream is too sparse to trust
/// — the caller should clear and skip that season's derived surfaces.
async fn pbp_source_is_corrupt(pool: &PgPool, season: i32) -> Result<bool, sqlx::Error> {
    match pbp_fga_coverage(pool, season).await? {
        Some(cov) if cov < PBP_MIN_FGA_COVERAGE => {
            warn!(
                season,
                coverage = format!("{cov:.2}"),
                threshold = PBP_MIN_FGA_COVERAGE,
                "PBP tag stream covers too few box FGA — treating source as corrupt, \
                 clearing and skipping this season's PBP-derived surfaces"
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Minimum share of FGA-tagged plays that must also carry the `paint` location
/// tag before we trust a season's CONTEXTUAL tags (paint/brk/2ch/offto/FOULED).
/// The 2015-2018 NatStat feeds carry only box-event tags (FGA/FGM/REB/TOV/…) —
/// zero contextual tags — so deriving from them yields misleading zeros
/// (paint_fga=0 on every shot, perimeter_fga=all FGA, 0 transition points, 0
/// fouls drawn). Observed paint share of tagged FGA: 2015-2018 ≈ 0.000, every
/// contextual-era season (2020-2026) ≥ 0.41. The signal is effectively binary;
/// 0.05 separates "vocabulary absent" from "present but season-sparse".
const PBP_MIN_PAINT_TAG_COVERAGE: f64 = 0.05;

/// True (with a warning) when the season's PBP feed predates the contextual tag
/// vocabulary — the caller should clear and skip the tag-derived aggregates
/// (the lineup/possession path is unaffected: it reads box-event tags + SUBs,
/// which every vintage carries).
async fn pbp_lacks_context_tags(pool: &PgPool, season: i32) -> Result<bool, sqlx::Error> {
    // Single pass with FILTER — play_by_play has no season index, so two
    // subselects would each full-scan the (32.8M-row local) table.
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT \
            count(*) FILTER (WHERE 'paint' = ANY(tags)), \
            count(*) FILTER (WHERE 'FGA' = ANY(tags) OR '3FA' = ANY(tags)) \
         FROM play_by_play WHERE season = $1",
    )
    .bind(season)
    .fetch_one(pool)
    .await?;
    let (paint, fga) = (row.0.unwrap_or(0), row.1.unwrap_or(0));
    if fga == 0 {
        // No tagged FGA at all — nothing to derive from; the per-game UPDATE
        // below would match zero rows anyway, so don't gate (and don't clear:
        // a not-yet-loaded season should keep its NULLs untouched).
        return Ok(false);
    }
    let cov = paint as f64 / fga as f64;
    if cov < PBP_MIN_PAINT_TAG_COVERAGE {
        warn!(
            season,
            coverage = format!("{cov:.3}"),
            threshold = PBP_MIN_PAINT_TAG_COVERAGE,
            "PBP feed lacks contextual tags (pre-2020 vocabulary) — clearing and \
             skipping this season's tag-derived aggregates"
        );
        return Ok(true);
    }
    Ok(false)
}

pub async fn compute_pbp_aggregates(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Corruption gate: a season whose PBP tags don't cover the box-score FGA
    // (e.g. 2019's mis-tagged export) yields garbage aggregates, so clear any
    // stale values and skip rather than serve them. The contextual-tag gate
    // catches the orthogonal failure: pre-2020 feeds whose box-event tags are
    // fine (so the corrupt gate passes) but which carry no paint/brk/2ch/offto/
    // FOULED vocabulary at all — deriving from those yields zeros, not data.
    if pbp_source_is_corrupt(pool, season).await? || pbp_lacks_context_tags(pool, season).await? {
        sqlx::query(
            "UPDATE player_game_stats \
             SET paint_fga = NULL, paint_fgm = NULL, perimeter_fga = NULL, \
                 perimeter_fgm = NULL, transition_pts = NULL, second_chance_pts = NULL, \
                 points_off_turnovers = NULL, fouls_drawn = NULL \
             WHERE season = $1",
        )
        .bind(season)
        .execute(pool)
        .await?;
        // Clear the season rate rollup too — its source columns above are now
        // NULL, so a stale rollup would be inconsistent.
        sqlx::query(
            "UPDATE player_season_stats \
             SET paint_rate = NULL, paint_fg_pct = NULL, perimeter_fg_pct = NULL, \
                 transition_pts_per40 = NULL, second_chance_pts_per40 = NULL, \
                 points_off_turnovers_per40 = NULL, fouls_drawn_per40 = NULL \
             WHERE season = $1",
        )
        .bind(season)
        .execute(pool)
        .await?;
        return Ok(0);
    }

    let res = sqlx::query(
        "UPDATE player_game_stats pgs
         SET paint_fga            = d.paint_fga,
             paint_fgm            = d.paint_fgm,
             perimeter_fga        = d.tot_fga - d.paint_fga,
             perimeter_fgm        = d.tot_fgm - d.paint_fgm,
             transition_pts       = d.transition_pts,
             second_chance_pts    = d.second_chance_pts,
             points_off_turnovers = d.points_off_turnovers,
             fouls_drawn          = d.fouls_drawn
         FROM (
             WITH dedup AS (
                 SELECT DISTINCT ON (game_id, sort_order, description, player_id)
                        game_id, player_id, tags, points
                 FROM play_by_play
                 WHERE season = $1 AND player_id IS NOT NULL
                 ORDER BY game_id, sort_order, description, player_id, seq
             )
             SELECT
                 player_id,
                 game_id,
                 count(*) FILTER (WHERE 'paint' = ANY(tags) AND ('FGA' = ANY(tags) OR '3FA' = ANY(tags)))::int AS paint_fga,
                 count(*) FILTER (WHERE 'paint' = ANY(tags) AND ('FGM' = ANY(tags) OR '3FM' = ANY(tags)))::int AS paint_fgm,
                 count(*) FILTER (WHERE 'FGA' = ANY(tags) OR '3FA' = ANY(tags))::int AS tot_fga,
                 count(*) FILTER (WHERE 'FGM' = ANY(tags) OR '3FM' = ANY(tags))::int AS tot_fgm,
                 COALESCE(sum(points) FILTER (WHERE 'brk' = ANY(tags)), 0)::int   AS transition_pts,
                 COALESCE(sum(points) FILTER (WHERE '2ch' = ANY(tags)), 0)::int   AS second_chance_pts,
                 COALESCE(sum(points) FILTER (WHERE 'offto' = ANY(tags)), 0)::int AS points_off_turnovers,
                 count(*) FILTER (WHERE 'FOULED' = ANY(tags))::int AS fouls_drawn
             FROM dedup
             GROUP BY player_id, game_id
         ) d
         WHERE pgs.player_id = d.player_id
           AND pgs.game_id = d.game_id
           AND pgs.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;
    let n = res.rows_affected();

    // Clean-recompute the season rates: clear the season first, then repopulate
    // covered players below. Without this, a player who lost ALL PBP coverage
    // since the last run (rare — coverage normally only grows) would keep a stale
    // rate, since the rollup UPDATE only touches (player_id, team_id) pairs still
    // present in PBP-covered games. Symmetric with the corrupt-gate clear above.
    sqlx::query(
        "UPDATE player_season_stats \
         SET paint_rate = NULL, paint_fg_pct = NULL, perimeter_fg_pct = NULL, \
             transition_pts_per40 = NULL, second_chance_pts_per40 = NULL, \
             points_off_turnovers_per40 = NULL, fouls_drawn_per40 = NULL \
         WHERE season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    // Season rate rollup: fold the per-game tag columns just written into the
    // comparable RATE forms on player_season_stats (Tier-1 feature substrate —
    // see migration 036 / docs/pbp_utilization_scope.md). Rates, not counts, so
    // they're robust to NatStat's season-varying tag density; aggregated only
    // over PBP-covered games (`paint_fga IS NOT NULL`) so each rate's numerator
    // and denominator share the same game set. Grouped by (player_id, team_id)
    // to match player_season_stats' grain (a mid-season transfer has one row per
    // team). NULL when a player logged no PBP-covered games.
    sqlx::query(
        "UPDATE player_season_stats pss
         SET paint_rate                 = r.paint_rate,
             paint_fg_pct               = r.paint_fg_pct,
             perimeter_fg_pct           = r.perimeter_fg_pct,
             transition_pts_per40       = r.transition_pts_per40,
             second_chance_pts_per40    = r.second_chance_pts_per40,
             points_off_turnovers_per40 = r.points_off_turnovers_per40,
             fouls_drawn_per40          = r.fouls_drawn_per40
         FROM (
             SELECT
                 player_id, team_id,
                 -- Share of TRACKED shots in the paint. Denominator is the PBP
                 -- total (paint + perimeter), NOT box `fga` — the tag stream and
                 -- the box FGA disagree per game (a low-box-FGA player can carry
                 -- more PBP paint tags than box attempts), and dividing across the
                 -- two sources let paint_rate run past 1. paint_fga is a subset of
                 -- the PBP total by construction, so this is bounded to [0,1].
                 CASE WHEN sum(paint_fga + perimeter_fga) > 0
                      THEN sum(paint_fga)::double precision / sum(paint_fga + perimeter_fga)
                 END AS paint_rate,
                 -- FG% clamped to 1.0: makes/attempts are separate tags, so a rare
                 -- play tags a make without its attempt and fgm can edge past fga.
                 CASE WHEN sum(paint_fga) > 0
                      THEN LEAST(1.0, sum(paint_fgm)::double precision / sum(paint_fga))
                 END AS paint_fg_pct,
                 CASE WHEN sum(perimeter_fga) > 0
                      THEN LEAST(1.0, sum(perimeter_fgm)::double precision / sum(perimeter_fga))
                 END AS perimeter_fg_pct,
                 sum(transition_pts)       * 40.0 / nullif(sum(minutes), 0) AS transition_pts_per40,
                 sum(second_chance_pts)    * 40.0 / nullif(sum(minutes), 0) AS second_chance_pts_per40,
                 sum(points_off_turnovers) * 40.0 / nullif(sum(minutes), 0) AS points_off_turnovers_per40,
                 sum(fouls_drawn)          * 40.0 / nullif(sum(minutes), 0) AS fouls_drawn_per40
             FROM player_game_stats
             WHERE season = $1 AND paint_fga IS NOT NULL
             GROUP BY player_id, team_id
         ) r
         WHERE pss.player_id = r.player_id
           AND pss.team_id = r.team_id
           AND pss.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(season, updated = n, "computed PBP per-player aggregates");
    Ok(n)
}

/// One per-team stint row staged for bulk insert into `lineup_stints`.
struct StintRow {
    game_id: Uuid,
    period: i32,
    start_seq: i32,
    end_seq: i32,
    team_id: Uuid,
    lineup: Vec<Uuid>,
    opp_lineup: Vec<Uuid>,
    points_for: i32,
    points_against: i32,
    possessions_for: f64,
    possessions_against: f64,
    seconds: i32,
}

/// P2b: derive lineup stints, season lineup aggregates, and per-player PBP
/// plus/minus from `play_by_play`. Hybrid sourcing per game — exact API
/// on-floor lineups when stored, SUB-replay (~86%) off the CSV otherwise (see
/// `pbp_replay` and `docs/pbp_methodology.md`).
///
/// `lineup_stints` is local-only (per-stint detail); `lineup_aggregates` and the
/// `plus_minus_pbp` column ship to prod. Season-scoped clean recompute.
pub async fn compute_pbp_lineups(pool: &PgPool, season: i32) -> Result<u64, sqlx::Error> {
    // Build everything in memory first (read-only), then swap it in atomically
    // at the end — see the transaction below. Lets a mid-run failure leave the
    // prior season intact rather than a half-rebuilt, prod-shipped table.

    // Games in this season that have play-by-play.
    let games: Vec<(Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT g.id, g.home_team_id, g.away_team_id
         FROM games g
         WHERE g.season = $1 AND EXISTS (SELECT 1 FROM play_by_play p WHERE p.game_id = g.id)",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    if games.is_empty() {
        return Ok(0); // no PBP loaded for this season — nothing to do
    }

    // Corruption gate: if the PBP tag stream doesn't cover the box-score FGA
    // (2019's mis-tagged export), the derived stints/lineups/+/- are garbage.
    // Clear any prior rows for the season and skip — better an absent surface
    // than a wrong one (the UI hides it, same as a pre-PBP season).
    if pbp_source_is_corrupt(pool, season).await? {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM lineup_stints WHERE season = $1")
            .bind(season)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM lineup_aggregates WHERE season = $1")
            .bind(season)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE player_game_stats SET plus_minus_pbp = NULL WHERE season = $1")
            .bind(season)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM player_on_off WHERE season = $1")
            .bind(season)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(0);
    }

    // Bulk-load all per-game metadata once, rather than ~4 queries per game. The
    // only per-game query left is the plays themselves (PK-indexed by game_id).
    let starter_rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT game_id, team_id, player_id FROM player_game_stats \
         WHERE season = $1 AND starter IS TRUE",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    let mut starters: HashMap<Uuid, Vec<(Uuid, Uuid)>> = HashMap::new();
    for (g, t, p) in starter_rows {
        starters.entry(g).or_default().push((t, p));
    }

    // game_id -> ((team_id, lowercased name) -> player_id) for the null-player
    // sub name fallback.
    let roster_rows: Vec<(Uuid, Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT pgs.game_id, pgs.team_id, lower(pl.name), pgs.player_id \
         FROM player_game_stats pgs JOIN players pl ON pl.id = pgs.player_id \
         WHERE pgs.season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    let mut name_maps: HashMap<Uuid, HashMap<(Uuid, String), Uuid>> = HashMap::new();
    for (g, t, n, p) in roster_rows {
        name_maps.entry(g).or_default().entry((t, n)).or_insert(p);
    }

    // natstat player code -> our UUID for the season (on-floor resolution).
    let code_to_uuid: HashMap<String, Uuid> =
        sqlx::query_as("SELECT natstat_id, id FROM players WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    let empty_names: HashMap<(Uuid, String), Uuid> = HashMap::new();
    let mut rows: Vec<(StintRow, &'static str)> = Vec::new();
    let mut onfloor_games = 0u64;
    let mut replay_games = 0u64;

    for (game_id, home_team, away_team) in &games {
        // nil for an unresolved (NULL) team — it never matches a sub's team and
        // emits no stint rows (guarded by the Option below).
        let ht = home_team.unwrap_or_else(Uuid::nil);
        let vt = away_team.unwrap_or_else(Uuid::nil);
        let gs = starters.get(game_id);
        let pick = |team: Uuid| -> Vec<Uuid> {
            gs.map(|v| {
                v.iter()
                    .filter(|(t, _)| *t == team)
                    .map(|(_, p)| *p)
                    .collect()
            })
            .unwrap_or_default()
        };
        let home_starters = pick(ht);
        let vis_starters = pick(vt);
        let name_map = name_maps.get(game_id).unwrap_or(&empty_names);

        let raw = load_raw_plays(pool, *game_id).await?;
        let (stints, source) = crate::pbp_replay::game_stints(
            ht,
            vt,
            &home_starters,
            &vis_starters,
            name_map,
            &code_to_uuid,
            &raw,
        );
        match source {
            crate::pbp_replay::StintSource::OnFloor => onfloor_games += 1,
            crate::pbp_replay::StintSource::Replay => replay_games += 1,
        }
        let src = source.as_str();
        // Possessions (each side) + on-floor seconds per stint, aligned 1:1.
        let metrics = crate::pbp_replay::stint_metrics(&raw, &stints, ht, vt);
        for (st, m) in stints.into_iter().zip(metrics) {
            // One row per team's perspective (skip a side with no resolved team
            // or an empty lineup — e.g. an unresolved non-D1 opponent).
            if home_team.is_some() && !st.home_lineup.is_empty() {
                rows.push((
                    StintRow {
                        game_id: *game_id,
                        period: st.period,
                        start_seq: st.start_seq,
                        end_seq: st.end_seq,
                        team_id: ht,
                        lineup: st.home_lineup.clone(),
                        opp_lineup: st.vis_lineup.clone(),
                        points_for: st.home_score_delta,
                        points_against: st.vis_score_delta,
                        possessions_for: m.home_possessions,
                        possessions_against: m.vis_possessions,
                        seconds: m.seconds,
                    },
                    src,
                ));
            }
            if away_team.is_some() && !st.vis_lineup.is_empty() {
                rows.push((
                    StintRow {
                        game_id: *game_id,
                        period: st.period,
                        start_seq: st.start_seq,
                        end_seq: st.end_seq,
                        team_id: vt,
                        lineup: st.vis_lineup,
                        opp_lineup: st.home_lineup,
                        points_for: st.vis_score_delta,
                        points_against: st.home_score_delta,
                        possessions_for: m.vis_possessions,
                        possessions_against: m.home_possessions,
                        seconds: m.seconds,
                    },
                    src,
                ));
            }
        }
    }

    // Atomic swap: the delete + reinsert + rollup + plus/minus all commit
    // together, so the prod-shipped `lineup_aggregates` / `plus_minus_pbp` never
    // sit empty or half-rebuilt (and a concurrent sync can't catch a partial
    // season) if the run fails midway.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM lineup_stints WHERE season = $1")
        .bind(season)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM lineup_aggregates WHERE season = $1")
        .bind(season)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE player_game_stats SET plus_minus_pbp = NULL WHERE season = $1")
        .bind(season)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM player_on_off WHERE season = $1")
        .bind(season)
        .execute(&mut *tx)
        .await?;

    insert_lineup_stints(&mut tx, season, &rows).await?;

    // Per-(game, team, lineup) rollup of 5-man stints with a physical-validity
    // flag, materialized once so both served surfaces below read the same set.
    //
    // **Box-minute clamp**: SUB-replay drift (a missed sub-out stretches a stint)
    // can attribute more on-floor time to a 5-man unit than physically possible.
    // A lineup can't have been on the floor longer than its least-playing member
    // played all game, so any (game, lineup) whose summed minutes exceed
    // `min(box minutes of its five) + 1.0` (a tolerance for box-minute integer
    // rounding) is a replay artifact — flagged invalid and excluded from the
    // served aggregates and plus/minus, so a phantom lineup can't out-rank real
    // ones. The raw rows stay in `lineup_stints` (a serving filter, not a delete).
    // COALESCE to a huge ceiling when box minutes are unknown — keep a row we
    // can't prove impossible. See ROADMAP "SUB-replay lineup drift".
    //
    // **0-minute box rows are treated as UNKNOWN, not as a 0-minute ceiling**
    // (`pgs.minutes > 0`, 2026-06-08). NatStat occasionally records a player with
    // 0 box minutes who nonetheless appears in the on-floor lineup data (~1.1k
    // such player-games in 2026, sometimes even flagged `starter=t`) — a box-side
    // artifact, not a real 0-minute member. Without this guard, one such member
    // collapses `min(box) + 1.0` to 1.0 and nukes an otherwise-valid bench
    // lineup, which on `onfloor` data was the single biggest source of the
    // false-positive invalidations that gutted high-minute starters' OFF samples
    // (~20% of all clamp-killed minutes). Excluding the 0/NULL member from the
    // min still constrains the lineup by its *positive*-minute members, so a
    // genuine over-merge (the union reconstruction's known limit) is unaffected —
    // it over-runs those members too. If EVERY member is 0/unknown, the subquery
    // is empty and COALESCE keeps the row (can't prove it impossible).
    sqlx::query(
        "CREATE TEMPORARY TABLE _game_lineups ON COMMIT DROP AS
         SELECT ls.game_id, ls.team_id, ls.lineup,
                count(*)                       AS stints,
                sum(ls.points_for)             AS pf,
                sum(ls.points_against)         AS pa,
                sum(ls.possessions_for)        AS posf,
                sum(ls.possessions_against)    AS posa,
                sum(ls.seconds)                AS secs,
                bool_or(ls.source = 'onfloor') AS has_onfloor,
                (sum(ls.seconds) / 60.0) <= COALESCE((
                    SELECT min(pgs.minutes) FROM player_game_stats pgs
                    WHERE pgs.game_id = ls.game_id AND pgs.player_id = ANY(ls.lineup)
                      AND pgs.minutes > 0
                ), 1e9) + 1.0                  AS valid
         FROM lineup_stints ls
         WHERE ls.season = $1 AND array_length(ls.lineup, 1) = 5
         GROUP BY ls.game_id, ls.team_id, ls.lineup",
    )
    .bind(season)
    .execute(&mut *tx)
    .await?;

    // Season rollup — valid 5-man game-lineups only (off-5 drift never enters the
    // temp table; the box-minute clamp drops physically-impossible game-lineups).
    sqlx::query(
        "INSERT INTO lineup_aggregates
             (season, team_id, lineup, stint_count, points_for, points_against, plus_minus, source,
              possessions_for, possessions_against, minutes, ortg, drtg, net_rtg)
         SELECT $1, team_id, lineup, sum(stints),
                sum(pf), sum(pa), sum(pf - pa),
                CASE WHEN bool_or(has_onfloor) THEN 'onfloor' ELSE 'replay' END,
                sum(posf), sum(posa), sum(secs) / 60.0,
                -- ortg/drtg = points per 100 possessions (same scale as team AdjO/AdjD);
                -- NULL rather than divide-by-zero for a lineup with no logged possessions.
                100.0 * sum(pf) / nullif(sum(posf), 0),
                100.0 * sum(pa) / nullif(sum(posa), 0),
                100.0 * sum(pf) / nullif(sum(posf), 0)
                  - 100.0 * sum(pa) / nullif(sum(posa), 0)
         FROM _game_lineups
         WHERE valid
         GROUP BY team_id, lineup",
    )
    .bind(season)
    .execute(&mut *tx)
    .await?;

    // Per-(player, game) plus/minus: sum each player's valid on-floor stint diffs.
    // Same clamp as the aggregates above so the two surfaces reconcile.
    sqlx::query(
        "UPDATE player_game_stats pgs
         SET plus_minus_pbp = d.pm
         FROM (
             SELECT gl.game_id, p AS player_id, sum(gl.pf - gl.pa)::int AS pm
             FROM _game_lineups gl, unnest(gl.lineup) AS p
             WHERE gl.valid
             GROUP BY gl.game_id, p
         ) d
         WHERE pgs.game_id = d.game_id AND pgs.player_id = d.player_id AND pgs.season = $1",
    )
    .bind(season)
    .execute(&mut *tx)
    .await?;

    // Player on/off splits (item "A"). ON = team offense/defense per 100 poss
    // with the player on the floor; OFF = with him on the bench; `net_on_off` is
    // the swing. Restricted to games he appeared in (isolates rotation from
    // availability — a DNP contributes to neither side). Rates are
    // per-100-possession, NULL-guarded against a side with zero possessions.
    //
    // **Unlike the top-lineup aggregates, on/off reads ALL stints (any lineup
    // size), NOT the 5-man-only, box-minute-clamped `_game_lineups` set.**
    // on/off attributes by individual *presence*, so a 3/4-man stint is still
    // valid evidence for a known player's split — we don't need a clean 5-man
    // unit. This matters because ~19% of 2026 onfloor player codes don't resolve
    // to a roster row (deep-bench / walk-on garbage-time players absent from
    // NatStat's DB); when one is the unresolved 5th, the stint stays sub-5-man
    // and the 5-man rollup drops it — which disproportionately erased the bench
    // (OFF) windows of high-minute starters. Reading all stints recovers them.
    //
    // **Per-player box-minute clamp** (replaces the lineup clamp for this
    // surface): the onfloor reconstruction over-credits a high-minute player's
    // ON time, because his brief rests fall in sparse-onfloor windows where it
    // never registers him leaving (an iron-man reads ~94% on-floor vs ~86% by
    // box). We cap each player's per-game ON accumulators at his box minutes
    // (scale DOWN only — `LEAST(1.0, box/on)`), moving the excess to OFF. This
    // recovers an honest off-court sample for exactly the starters raw on/off
    // failed (Acuff 2026: OFF 11 → ~270 poss). We never scale UP: when the
    // reconstruction UNDER-credits, we can't tell which possessions were his, so
    // we leave them (he stays slightly under — the conservative direction). A
    // player with no positive box-minute row that game is left unscaled.
    sqlx::query(
        "INSERT INTO player_on_off
             (season, team_id, player_id, games,
              on_minutes, on_possessions_for, on_possessions_against,
              on_points_for, on_points_against, on_ortg, on_drtg, on_net_rtg,
              off_minutes, off_possessions_for, off_possessions_against,
              off_points_for, off_points_against, off_ortg, off_drtg, off_net_rtg,
              net_on_off, source)
         WITH stints AS (   -- ALL reconstructed stints (any size), this season
             SELECT game_id, team_id, lineup,
                    points_for pf, points_against pa,
                    possessions_for posf, possessions_against posa, seconds secs,
                    (source = 'onfloor') AS has_onfloor
             FROM lineup_stints WHERE season = $1
         ),
         team_game AS (   -- team totals per game over ALL stints
             SELECT game_id, team_id,
                    sum(pf) tpf, sum(pa) tpa, sum(posf) tposf, sum(posa) tposa, sum(secs) tsecs
             FROM stints GROUP BY game_id, team_id
         ),
         player_on AS (   -- a player's raw ON totals per game
             -- DISTINCT inside the lineup: a reconstruction artifact can emit a
             -- lineup array with a duplicated player; each stint contributes to
             -- a player exactly once.
             --
             -- JOIN players … team_id = s.team_id: credit a player ONLY for his
             -- own team's stints. The replay/onfloor resolution occasionally
             -- maps two same-named players on different teams to one UUID, so a
             -- player's UUID can leak into another team's lineup arrays. Keying
             -- on his canonical `players.team_id` (the box-score authority) drops
             -- those cross-team phantoms, so (season, player_id) is unique and a
             -- player's on/off is never a different team's.
             SELECT s.game_id, s.team_id, pl.p AS player_id,
                    sum(s.pf) opf, sum(s.pa) opa, sum(s.posf) oposf,
                    sum(s.posa) oposa, sum(s.secs) osecs, bool_or(s.has_onfloor) hon
             FROM stints s
             CROSS JOIN LATERAL (SELECT DISTINCT p FROM unnest(s.lineup) AS p) pl
             JOIN players pp ON pp.id = pl.p AND pp.season = $1 AND pp.team_id = s.team_id
             GROUP BY s.game_id, s.team_id, pl.p
         ),
         scaled AS (   -- cap each (player, game) ON at his box minutes (scale DOWN only)
             SELECT o.game_id, o.team_id, o.player_id, o.hon,
                    o.opf, o.opa, o.oposf, o.oposa, o.osecs,
                    LEAST(1.0, COALESCE(b.box_secs, o.osecs) / nullif(o.osecs, 0)) AS sc
             FROM player_on o
             LEFT JOIN (
                 SELECT game_id, player_id, minutes * 60.0 AS box_secs
                 FROM player_game_stats WHERE season = $1 AND minutes > 0
             ) b ON b.game_id = o.game_id AND b.player_id = o.player_id
         ),
         per_game AS (    -- off = team total − scaled on, for games he appeared in
             -- GREATEST(0, …): a per-stint possession estimate can go slightly
             -- negative in a short/garbled stint (e.g. an ORB with no FGA), so a
             -- tiny off slice can edge below 0. Clamp at 0 — a tiny/negative off
             -- sample then yields a NULL rate (nullif below), the honest
             -- no-meaningful-off-court-sample outcome.
             SELECT s.team_id, s.player_id, s.hon,
                    s.sc * s.osecs AS on_secs, s.sc * s.oposf AS on_posf,
                    s.sc * s.oposa AS on_posa, s.sc * s.opf AS on_pf, s.sc * s.opa AS on_pa,
                    GREATEST(0, tg.tsecs - s.sc * s.osecs) AS off_secs,
                    GREATEST(0, tg.tposf - s.sc * s.oposf) AS off_posf,
                    GREATEST(0, tg.tposa - s.sc * s.oposa) AS off_posa,
                    GREATEST(0, tg.tpf   - s.sc * s.opf)   AS off_pf,
                    GREATEST(0, tg.tpa   - s.sc * s.opa)   AS off_pa
             FROM scaled s
             JOIN team_game tg ON tg.game_id = s.game_id AND tg.team_id = s.team_id
         ),
         roll AS (
             SELECT team_id, player_id, count(*) games, bool_or(hon) hon,
                    sum(on_secs) on_secs, sum(on_posf) on_posf, sum(on_posa) on_posa,
                    sum(on_pf) on_pf, sum(on_pa) on_pa,
                    sum(off_secs) off_secs, sum(off_posf) off_posf, sum(off_posa) off_posa,
                    sum(off_pf) off_pf, sum(off_pa) off_pa
             FROM per_game GROUP BY team_id, player_id
         )
         SELECT $1, team_id, player_id, games,
                on_secs / 60.0, on_posf, on_posa, on_pf::int, on_pa::int,
                100.0 * on_pf / nullif(on_posf, 0),
                100.0 * on_pa / nullif(on_posa, 0),
                100.0 * on_pf / nullif(on_posf, 0) - 100.0 * on_pa / nullif(on_posa, 0),
                off_secs / 60.0, off_posf, off_posa, off_pf::int, off_pa::int,
                100.0 * off_pf / nullif(off_posf, 0),
                100.0 * off_pa / nullif(off_posa, 0),
                100.0 * off_pf / nullif(off_posf, 0) - 100.0 * off_pa / nullif(off_posa, 0),
                (100.0 * on_pf / nullif(on_posf, 0) - 100.0 * on_pa / nullif(on_posa, 0))
                  - (100.0 * off_pf / nullif(off_posf, 0) - 100.0 * off_pa / nullif(off_posa, 0)),
                CASE WHEN hon THEN 'onfloor' ELSE 'replay' END
         FROM roll
         -- Minimum ON sample on BOTH ends (>= 100 possessions). on/off is only
         -- meaningful for a real rotation player: below ~100 on-court possessions
         -- the ON rating is noise (a benchwarmer's handful of garbage-time
         -- possessions can read ±600 per 100), and since reading ALL stints now
         -- gives every player who logged a few minutes a tiny ON slice, the old
         -- `> 0` gate let those through and they topped the swing rankings. 100
         -- matches the panel's existing OFF small-sample threshold; it drops only
         -- sub-~3-min/game players (a 9-min/game role player clears it easily),
         -- and the GREATEST-clamped OFF side stays meaningful via team-total
         -- subtraction. The UI hides the panel/column for a dropped player.
         -- (per-stint counts can go slightly negative, so the >= also guards the
         -- rate denominators against a non-positive cameo sample.)
         WHERE on_posf >= 100 AND on_posa >= 100",
    )
    .bind(season)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    info!(
        season,
        games = games.len(),
        onfloor_games,
        replay_games,
        stint_rows = rows.len(),
        "computed PBP lineups"
    );
    Ok(rows.len() as u64)
}

/// Load one game's raw plays for replay/on-floor stint building.
async fn load_raw_plays(
    pool: &PgPool,
    game_id: Uuid,
) -> Result<Vec<crate::pbp_replay::RawPlay>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT seq, period, team_id, player_id, description, tags, \
         score_home, score_vis, onfloor_home, onfloor_vis, clock \
         FROM play_by_play WHERE game_id = $1 ORDER BY seq",
    )
    .bind(game_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| crate::pbp_replay::RawPlay {
            seq: r.get("seq"),
            period: r.get("period"),
            team_id: r.get("team_id"),
            player_id: r.get("player_id"),
            description: r.get("description"),
            tags: r.get("tags"),
            score_home: r.get("score_home"),
            score_vis: r.get("score_vis"),
            onfloor_home: r.get("onfloor_home"),
            onfloor_vis: r.get("onfloor_vis"),
            clock: r.get("clock"),
        })
        .collect())
}

/// Chunked bulk insert into `lineup_stints` (14 cols × 1000 rows = 14k binds,
/// well under Postgres' 65535 cap). Runs inside the caller's transaction.
async fn insert_lineup_stints(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    season: i32,
    rows: &[(StintRow, &'static str)],
) -> Result<(), sqlx::Error> {
    for chunk in rows.chunks(1000) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO lineup_stints (game_id, season, period, start_seq, end_seq, \
             team_id, lineup, opp_lineup, points_for, points_against, source, \
             possessions_for, possessions_against, seconds) ",
        );
        qb.push_values(chunk, |mut b, (r, src)| {
            b.push_bind(r.game_id)
                .push_bind(season)
                .push_bind(r.period)
                .push_bind(r.start_seq)
                .push_bind(r.end_seq)
                .push_bind(r.team_id)
                .push_bind(r.lineup.clone())
                .push_bind(r.opp_lineup.clone())
                .push_bind(r.points_for)
                .push_bind(r.points_against)
                .push_bind(*src)
                .push_bind(r.possessions_for)
                .push_bind(r.possessions_against)
                .push_bind(r.seconds);
        });
        qb.build().execute(&mut **tx).await?;
    }
    Ok(())
}

pub async fn compute_all(pool: &PgPool, season: i32) -> Result<ComputeReport, sqlx::Error> {
    let mut report = ComputeReport::default();

    info!(season, "starting compute pipeline");

    // Step 0 (defensive prerequisite): seed team_season_stats for any team
    // with games but no row. Silent when there's nothing to heal.
    let seeded = seed_missing_team_season_stats(pool, season).await?;
    if seeded > 0 {
        warn!(
            season,
            seeded,
            "seeded {} missing team_season_stats row(s) — D-I transition or upstream gap",
            seeded
        );
    }

    info!("step 1/15: deduplicating players");
    report.deduplicated_players = deduplicate_players(pool, season).await?;

    info!("step 2/15: backfilling derived game stats");
    report.backfilled = backfill_game_stats(pool).await?;

    info!("step 3/15: estimating missing team defensive rebounds");
    report.estimated_rebounds = estimate_missing_team_rebounds(pool, season).await?;

    info!("step 4/15: computing player season stats (with rate stats)");
    report.player_season_stats = compute_player_season_stats(pool, season).await?;

    // PBP steps run after season stats and before team/CamPom steps. Both no-op
    // for seasons with no play_by_play rows loaded (pre-2012 / not ingested).
    info!("step 5/15: computing play-by-play per-player aggregates");
    report.pbp_aggregates = compute_pbp_aggregates(pool, season).await?;

    info!("step 6/15: computing play-by-play lineups & stints");
    report.pbp_lineups = compute_pbp_lineups(pool, season).await?;

    info!("step 7/15: computing team four factors");
    report.team_four_factors = compute_team_four_factors(pool, season).await?;

    info!("step 8/15: computing adjusted efficiency (KenPom-style)");
    report.adjusted_efficiency = compute_adjusted_efficiency(pool, season).await?;

    info!("step 9/15: computing individual ORTG/DRTG (Torvik passthrough)");
    report.individual_ratings = compute_individual_ratings(pool, season).await?;

    info!("step 10/15: computing player SOS");
    report.player_sos = compute_player_sos(pool, season).await?;

    info!("step 11/15: computing CamPom composites");
    report.campom = compute_campom(pool, season).await?;

    info!("step 12/15: computing rolling averages");
    report.rolling_averages = compute_rolling_averages(pool, season).await?;

    info!("step 13/15: computing derived game fields");
    report.derived_fields = compute_derived_game_fields(pool, season).await?;

    info!("step 14/15: computing schedules");
    report.schedules = compute_schedules(pool, season).await?;

    info!("step 15/15: computing player percentiles");
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
    pub pbp_aggregates: u64,
    pub pbp_lineups: u64,
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
            "Computed: {} deduped, {} backfilled, {} est rebounds, {} player stats, {} pbp aggregates, {} pbp lineups, {} four factors, {} adj eff, {} ORTG/DRTG, {} CamPom, {} player SOS, {} rolling avgs, {} derived fields, {} schedules, {} percentiles",
            self.deduplicated_players,
            self.backfilled,
            self.estimated_rebounds,
            self.player_season_stats,
            self.pbp_aggregates,
            self.pbp_lineups,
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

    #[test]
    fn compute_report_display() {
        let report = ComputeReport::default();
        let s = format!("{report}");
        assert!(s.contains("0 deduped"));
        assert!(s.contains("0 pbp aggregates"));
        assert!(s.contains("0 pbp lineups"));
        assert!(s.contains("0 percentiles"));
    }
}
