use crate::NatStatClient;
use crate::client::NatStatError;
use serde_json::Value;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

/// Ingest all MBB teams for a given season.
pub async fn ingest_teams(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let pages = client
        .get_all_pages("teamcodes", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let teams = match page.get("results") {
            Some(Value::Array(arr)) => arr,
            Some(Value::Object(obj)) => {
                // NatStat sometimes returns results as an object keyed by code
                return ingest_teams_from_object(obj, pool, season).await;
            }
            _ => continue,
        };

        for team in teams {
            if upsert_team(team, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, "teams ingested");
    Ok(count)
}

async fn ingest_teams_from_object(
    obj: &serde_json::Map<String, Value>,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let mut count = 0u64;
    for (_key, team) in obj {
        if upsert_team(team, pool, season).await? {
            count += 1;
        }
    }
    info!(count, season, "teams ingested");
    Ok(count)
}

async fn upsert_team(team: &Value, pool: &PgPool, season: i32) -> Result<bool, NatStatError> {
    let natstat_id = match team.get("code").and_then(|c| c.as_str()) {
        Some(id) => id,
        None => return Ok(false),
    };

    let name = team
        .get("name")
        .or_else(|| team.get("full_name"))
        .and_then(|n| n.as_str())
        .unwrap_or(natstat_id);

    let short_name = team.get("short_name").and_then(|n| n.as_str());
    let conference = team
        .get("conference")
        .or_else(|| team.get("league"))
        .and_then(|c| c.as_str());
    let division = team.get("division").and_then(|d| d.as_str());

    sqlx::query(
        "INSERT INTO teams (id, natstat_id, name, short_name, conference, division, season)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (natstat_id, season) DO UPDATE
         SET name = EXCLUDED.name,
             short_name = EXCLUDED.short_name,
             conference = EXCLUDED.conference,
             division = EXCLUDED.division,
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(natstat_id)
    .bind(name)
    .bind(short_name)
    .bind(conference)
    .bind(division)
    .bind(season)
    .execute(pool)
    .await?;

    Ok(true)
}

/// Ingest detailed team data (ELO, stats) for teams already in the DB.
pub async fn ingest_team_details(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    // Fetch all teams with full details
    let pages = client
        .get_all_pages("teams", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let results = extract_results(page);
        for team in results {
            let natstat_id = match team.get("code").and_then(|c| c.as_str()) {
                Some(id) => id,
                None => continue,
            };

            // Extract ELO if present
            let elo = team
                .get("elo")
                .and_then(|e| e.get("rating"))
                .and_then(|r| r.as_f64());

            // Look up team_id
            let row: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
                    .bind(natstat_id)
                    .bind(season)
                    .fetch_optional(pool)
                    .await?;

            let Some((team_id,)) = row else { continue };

            // Upsert team_season_stats with what we have
            sqlx::query(
                "INSERT INTO team_season_stats (id, team_id, season, elo)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (team_id, season) DO UPDATE
                 SET elo = COALESCE(EXCLUDED.elo, team_season_stats.elo),
                     updated_at = now()",
            )
            .bind(Uuid::new_v4())
            .bind(team_id)
            .bind(season)
            .bind(elo)
            .execute(pool)
            .await?;

            count += 1;
        }
    }

    info!(count, season, "team details ingested");
    Ok(count)
}

fn extract_results(page: &Value) -> Vec<&Value> {
    match page.get("results") {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(Value::Object(obj)) => obj.values().collect(),
        _ => vec![],
    }
}
