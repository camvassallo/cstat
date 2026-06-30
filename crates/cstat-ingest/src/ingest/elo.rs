use super::utils::{parse_f64, parse_i32};
use crate::NatStatClient;
use crate::{extract_results, team_id_by_code_and_season};
use chrono::NaiveDate;
use sqlx::{PgPool, QueryBuilder};
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

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

            let Some(team_id) = team_id_by_code_and_season(pool, Some(team_code), season).await?
            else {
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

/// One forecast staged in memory before the batched upsert.
struct ForecastRow {
    game_id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    home_elo_before: Option<f64>,
    away_elo_before: Option<f64>,
    home_elo_after: Option<f64>,
    away_elo_after: Option<f64>,
    home_win_exp: Option<f64>,
    away_win_exp: Option<f64>,
    elo_k: Option<f64>,
    elo_adjust: Option<f64>,
    elo_points: Option<f64>,
    home_moneyline: Option<i32>,
    away_moneyline: Option<i32>,
    spread: Option<f64>,
    spread_fav_id: Option<Uuid>,
    over_under: Option<f64>,
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

    // Prefetch the season's teams (natstat_id -> id) and games
    // ((date, home, away) -> id) once, so the per-forecast loop is pure HashMap
    // lookups instead of ~5 DB round-trips each. With ~5,700 forecasts that
    // collapses a ~28k-query N+1 — which ran in seconds against a localhost DB
    // but stalled for tens of minutes against the prod DB over a high-latency
    // (cross-region / public-proxy) connection — into a few prefetches plus a
    // batched insert.
    let teams: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT natstat_id, id FROM teams WHERE season = $1 AND natstat_id IS NOT NULL",
    )
    .bind(season)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let games: HashMap<(NaiveDate, Uuid, Uuid), Uuid> =
        sqlx::query_as::<_, (Uuid, NaiveDate, Uuid, Uuid)>(
            "SELECT id, game_date, home_team_id, away_team_id FROM games \
             WHERE season = $1 AND game_date IS NOT NULL \
               AND home_team_id IS NOT NULL AND away_team_id IS NOT NULL",
        )
        .bind(season)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id, date, home, away)| ((date, home, away), id))
        .collect();

    let team_id = |code: Option<&str>| code.and_then(|c| teams.get(c).copied());

    let mut rows: Vec<ForecastRow> = Vec::new();
    for page in &pages {
        for entry in extract_results(page) {
            let Some(game_day) = entry.get("gameday").and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(game_date) = NaiveDate::parse_from_str(game_day, "%Y-%m-%d") else {
                continue;
            };
            let (Some(home_team_id), Some(away_team_id)) = (
                team_id(entry.get("home-code").and_then(|v| v.as_str())),
                team_id(entry.get("visitor-code").and_then(|v| v.as_str())),
            ) else {
                continue;
            };
            let Some(&game_id) = games.get(&(game_date, home_team_id, away_team_id)) else {
                // Game not in our DB (exhibition, non-D1, etc.)
                continue;
            };

            let forecast = entry.get("forecast");
            let elo = forecast.and_then(|f| f.get("elo"));
            let ml = forecast.and_then(|f| f.get("moneyline"));
            let spread_data = forecast.and_then(|f| f.get("spread"));
            let ou = forecast.and_then(|f| f.get("overunder"));

            rows.push(ForecastRow {
                game_id,
                game_date,
                home_team_id,
                away_team_id,
                home_elo_before: elo.and_then(|e| e.get("helobefore")).and_then(parse_f64),
                away_elo_before: elo.and_then(|e| e.get("velobefore")).and_then(parse_f64),
                home_elo_after: elo.and_then(|e| e.get("heloafter")).and_then(parse_f64),
                away_elo_after: elo.and_then(|e| e.get("veloafter")).and_then(parse_f64),
                home_win_exp: elo.and_then(|e| e.get("helowinexp")).and_then(parse_f64),
                away_win_exp: elo.and_then(|e| e.get("velowinexp")).and_then(parse_f64),
                elo_k: elo.and_then(|e| e.get("elok")).and_then(parse_f64),
                elo_adjust: elo.and_then(|e| e.get("eloadjust")).and_then(parse_f64),
                elo_points: elo.and_then(|e| e.get("elopoints")).and_then(parse_f64),
                home_moneyline: ml.and_then(|m| m.get("homemoneyline")).and_then(parse_i32),
                away_moneyline: ml.and_then(|m| m.get("vismoneyline")).and_then(parse_i32),
                spread: spread_data
                    .and_then(|s| s.get("spread"))
                    .and_then(parse_f64),
                spread_fav_id: team_id(
                    spread_data
                        .and_then(|s| s.get("favourite"))
                        .and_then(|v| v.as_str()),
                ),
                over_under: ou.and_then(|o| o.get("overunder")).and_then(parse_f64),
            });
        }
    }

    // The forecast feed can list the same game more than once, and a batched
    // INSERT ... ON CONFLICT cannot affect the same conflict key twice in one
    // statement. Keep the last occurrence per game_id — matching the original
    // row-by-row upsert, where a later row overwrote an earlier one.
    let rows: Vec<ForecastRow> = {
        let mut by_game: HashMap<Uuid, ForecastRow> = HashMap::with_capacity(rows.len());
        for r in rows {
            by_game.insert(r.game_id, r);
        }
        by_game.into_values().collect()
    };
    let count = rows.len() as u64;

    // Batched upsert. 20 columns/row; chunk so each statement stays well under
    // Postgres' 65535-bind-parameter cap (1000 * 20 = 20000).
    for chunk in rows.chunks(1000) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO game_forecasts (\
             id, game_id, season, game_date, home_team_id, away_team_id, \
             home_elo_before, away_elo_before, home_elo_after, away_elo_after, \
             home_win_exp, away_win_exp, elo_k, elo_adjust, elo_points, \
             home_moneyline, away_moneyline, spread, spread_favorite_team_id, over_under) ",
        );
        qb.push_values(chunk, |mut b, r| {
            b.push_bind(Uuid::new_v4())
                .push_bind(r.game_id)
                .push_bind(season)
                .push_bind(r.game_date)
                .push_bind(r.home_team_id)
                .push_bind(r.away_team_id)
                .push_bind(r.home_elo_before)
                .push_bind(r.away_elo_before)
                .push_bind(r.home_elo_after)
                .push_bind(r.away_elo_after)
                .push_bind(r.home_win_exp)
                .push_bind(r.away_win_exp)
                .push_bind(r.elo_k)
                .push_bind(r.elo_adjust)
                .push_bind(r.elo_points)
                .push_bind(r.home_moneyline)
                .push_bind(r.away_moneyline)
                .push_bind(r.spread)
                .push_bind(r.spread_fav_id)
                .push_bind(r.over_under);
        });
        qb.push(
            " ON CONFLICT (game_id) DO UPDATE SET \
             home_elo_before = EXCLUDED.home_elo_before, \
             away_elo_before = EXCLUDED.away_elo_before, \
             home_elo_after = EXCLUDED.home_elo_after, \
             away_elo_after = EXCLUDED.away_elo_after, \
             home_win_exp = EXCLUDED.home_win_exp, \
             away_win_exp = EXCLUDED.away_win_exp, \
             elo_k = EXCLUDED.elo_k, \
             elo_adjust = EXCLUDED.elo_adjust, \
             elo_points = EXCLUDED.elo_points, \
             home_moneyline = EXCLUDED.home_moneyline, \
             away_moneyline = EXCLUDED.away_moneyline, \
             spread = EXCLUDED.spread, \
             spread_favorite_team_id = EXCLUDED.spread_favorite_team_id, \
             over_under = EXCLUDED.over_under",
        );
        qb.build().execute(pool).await?;
    }

    info!(
        count,
        season, "ingested game forecasts from /forecasts endpoint"
    );
    Ok(count)
}
