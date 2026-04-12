use crate::NatStatClient;
use crate::extract_results;
use serde_json::Value;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

/// Helper to extract f64 from a JSON value that may be string or number.
fn parse_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Helper to extract i32 from a JSON value that may be string or number.
fn parse_i32(v: &Value) -> Option<i32> {
    v.as_i64()
        .map(|i| i as i32)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Ingest real ELO ratings from the /elo endpoint.
/// Updates team_season_stats.elo_rating and elo_rank for all teams.
/// ~4 API calls per season (367 teams / 100 per page).
pub async fn ingest_elo_ratings(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, crate::client::NatStatError> {
    let pages = client
        .get_all_pages("elo", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;

    for page in &pages {
        let entries = extract_results(page);
        for entry in entries {
            let Some(team_code) = entry.get("code").and_then(|v| v.as_str()) else {
                continue;
            };
            let elo_rating = entry.get("elo").and_then(parse_f64);
            let elo_rank = entry.get("elorank").and_then(parse_i32);

            if elo_rating.is_none() {
                continue;
            }

            // Look up team by natstat_id + season
            let team_row: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                    .bind(team_code)
                    .bind(season)
                    .fetch_optional(pool)
                    .await?;

            let Some((team_id,)) = team_row else {
                continue;
            };

            // Update team_season_stats with real ELO rating
            let result = sqlx::query(
                "UPDATE team_season_stats
                 SET elo_rating = $1, elo_rank = $2, updated_at = now()
                 WHERE team_id = $3 AND season = $4",
            )
            .bind(elo_rating)
            .bind(elo_rank)
            .bind(team_id)
            .bind(season)
            .execute(pool)
            .await?;

            if result.rows_affected() > 0 {
                count += 1;
            }
        }
    }

    // NatStat's `elorank` field is paginated (resets to 1 on each page of 100),
    // so per-row ranks collide. Recompute a single global ranking from elo_rating.
    let reranked = sqlx::query(
        "WITH ranked AS (
             SELECT team_id,
                    DENSE_RANK() OVER (ORDER BY elo_rating DESC) AS rk
             FROM team_season_stats
             WHERE season = $1 AND elo_rating IS NOT NULL
         )
         UPDATE team_season_stats t
         SET elo_rank = ranked.rk, updated_at = now()
         FROM ranked
         WHERE t.team_id = ranked.team_id AND t.season = $1",
    )
    .bind(season)
    .execute(pool)
    .await?;

    info!(
        count,
        reranked = reranked.rows_affected(),
        season,
        "ingested ELO ratings from /elo endpoint"
    );
    Ok(count)
}

/// Ingest per-game forecasts from the /forecasts endpoint.
/// Stores pre/post-game ELO, win expectancy, and betting lines.
/// ~57 API calls per season (5,695 games / 100 per page).
pub async fn ingest_game_forecasts(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, crate::client::NatStatError> {
    let pages = client
        .get_all_pages("forecasts", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;

    for page in &pages {
        let entries = extract_results(page);
        for entry in entries {
            let Some(game_day) = entry.get("gameday").and_then(|v| v.as_str()) else {
                continue;
            };
            let game_date = match chrono::NaiveDate::parse_from_str(game_day, "%Y-%m-%d") {
                Ok(d) => d,
                Err(_) => continue,
            };

            let home_code = entry.get("home-code").and_then(|v| v.as_str());
            let away_code = entry.get("visitor-code").and_then(|v| v.as_str());

            let (Some(home_code), Some(away_code)) = (home_code, away_code) else {
                continue;
            };

            // Look up team IDs
            let home_row: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                    .bind(home_code)
                    .bind(season)
                    .fetch_optional(pool)
                    .await?;

            let away_row: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                    .bind(away_code)
                    .bind(season)
                    .fetch_optional(pool)
                    .await?;

            let home_team_id = home_row.map(|(id,)| id);
            let away_team_id = away_row.map(|(id,)| id);

            // Find the game by matching teams and date
            let game_row: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM games
                 WHERE season = $1 AND game_date = $2
                   AND home_team_id = $3 AND away_team_id = $4",
            )
            .bind(season)
            .bind(game_date)
            .bind(home_team_id)
            .bind(away_team_id)
            .fetch_optional(pool)
            .await?;

            let Some((game_id,)) = game_row else {
                // Game not in our DB (exhibition, non-D1, etc.)
                continue;
            };

            // Extract forecast data
            let forecast = entry.get("forecast");
            let elo = forecast.and_then(|f| f.get("elo"));

            let home_elo_before = elo.and_then(|e| e.get("helobefore")).and_then(parse_f64);
            let away_elo_before = elo.and_then(|e| e.get("velobefore")).and_then(parse_f64);
            let home_elo_after = elo.and_then(|e| e.get("heloafter")).and_then(parse_f64);
            let away_elo_after = elo.and_then(|e| e.get("veloafter")).and_then(parse_f64);
            let home_win_exp = elo.and_then(|e| e.get("helowinexp")).and_then(parse_f64);
            let away_win_exp = elo.and_then(|e| e.get("velowinexp")).and_then(parse_f64);
            let elo_k = elo.and_then(|e| e.get("elok")).and_then(parse_f64);
            let elo_adjust = elo.and_then(|e| e.get("eloadjust")).and_then(parse_f64);
            let elo_points = elo.and_then(|e| e.get("elopoints")).and_then(parse_f64);

            // Betting lines
            let ml = forecast.and_then(|f| f.get("moneyline"));
            let home_moneyline = ml.and_then(|m| m.get("homemoneyline")).and_then(parse_i32);
            let away_moneyline = ml.and_then(|m| m.get("vismoneyline")).and_then(parse_i32);

            let spread_data = forecast.and_then(|f| f.get("spread"));
            let spread = spread_data
                .and_then(|s| s.get("spread"))
                .and_then(parse_f64);
            let spread_fav_code = spread_data
                .and_then(|s| s.get("favourite"))
                .and_then(|v| v.as_str());
            // Look up spread favorite team ID
            let spread_fav_id = if let Some(code) = spread_fav_code {
                let row: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                        .bind(code)
                        .bind(season)
                        .fetch_optional(pool)
                        .await?;
                row.map(|(id,)| id)
            } else {
                None
            };

            let ou = forecast.and_then(|f| f.get("overunder"));
            let over_under = ou.and_then(|o| o.get("overunder")).and_then(parse_f64);

            sqlx::query(
                "INSERT INTO game_forecasts (
                    id, game_id, season, game_date,
                    home_team_id, away_team_id,
                    home_elo_before, away_elo_before,
                    home_elo_after, away_elo_after,
                    home_win_exp, away_win_exp,
                    elo_k, elo_adjust, elo_points,
                    home_moneyline, away_moneyline,
                    spread, spread_favorite_team_id, over_under
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
                ) ON CONFLICT (game_id) DO UPDATE SET
                    home_elo_before = EXCLUDED.home_elo_before,
                    away_elo_before = EXCLUDED.away_elo_before,
                    home_elo_after = EXCLUDED.home_elo_after,
                    away_elo_after = EXCLUDED.away_elo_after,
                    home_win_exp = EXCLUDED.home_win_exp,
                    away_win_exp = EXCLUDED.away_win_exp,
                    elo_k = EXCLUDED.elo_k,
                    elo_adjust = EXCLUDED.elo_adjust,
                    elo_points = EXCLUDED.elo_points,
                    home_moneyline = EXCLUDED.home_moneyline,
                    away_moneyline = EXCLUDED.away_moneyline,
                    spread = EXCLUDED.spread,
                    spread_favorite_team_id = EXCLUDED.spread_favorite_team_id,
                    over_under = EXCLUDED.over_under",
            )
            .bind(Uuid::new_v4())
            .bind(game_id)
            .bind(season)
            .bind(game_date)
            .bind(home_team_id)
            .bind(away_team_id)
            .bind(home_elo_before)
            .bind(away_elo_before)
            .bind(home_elo_after)
            .bind(away_elo_after)
            .bind(home_win_exp)
            .bind(away_win_exp)
            .bind(elo_k)
            .bind(elo_adjust)
            .bind(elo_points)
            .bind(home_moneyline)
            .bind(away_moneyline)
            .bind(spread)
            .bind(spread_fav_id)
            .bind(over_under)
            .execute(pool)
            .await?;

            count += 1;
        }
    }

    info!(
        count,
        season, "ingested game forecasts from /forecasts endpoint"
    );
    Ok(count)
}
