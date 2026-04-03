use crate::NatStatClient;
use crate::client::NatStatError;
use serde_json::Value;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

/// Ingest all MBB player codes for a given season.
pub async fn ingest_players(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let pages = client
        .get_all_pages("playercodes", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let players = extract_results(page);
        for player in players {
            if upsert_player(player, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, "players ingested");
    Ok(count)
}

async fn upsert_player(player: &Value, pool: &PgPool, season: i32) -> Result<bool, NatStatError> {
    // Player code is numeric in NatStat
    let natstat_id = match player
        .get("code")
        .or_else(|| player.get("id"))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(false),
    };

    let name = player
        .get("name")
        .or_else(|| player.get("full_name"))
        .and_then(|n| n.as_str())
        .unwrap_or("Unknown");

    let position = player
        .get("position")
        .or_else(|| player.get("pos"))
        .and_then(|p| p.as_str());

    let class_year = player
        .get("class")
        .or_else(|| player.get("year"))
        .and_then(|c| c.as_str());

    let jersey_number = player
        .get("number")
        .or_else(|| player.get("jersey"))
        .and_then(|n| n.as_str().map(String::from).or_else(|| Some(n.to_string())));

    let height_inches = player
        .get("height")
        .and_then(|h| h.as_str())
        .and_then(parse_height);

    let weight_lbs = player
        .get("weight")
        .and_then(|w| {
            w.as_i64()
                .or_else(|| w.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|w| w as i32);

    // Try to find team by code
    let team_code = player
        .get("team")
        .or_else(|| player.get("team_code"))
        .and_then(|t| t.as_str());

    let team_id: Option<Uuid> = if let Some(code) = team_code {
        sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
            .bind(code)
            .bind(season)
            .fetch_optional(pool)
            .await?
            .map(|(id,): (Uuid,)| id)
    } else {
        None
    };

    sqlx::query(
        "INSERT INTO players (id, natstat_id, name, team_id, season, position, height_inches, weight_lbs, class_year, jersey_number)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (natstat_id, season) DO UPDATE
         SET name = EXCLUDED.name,
             team_id = COALESCE(EXCLUDED.team_id, players.team_id),
             position = COALESCE(EXCLUDED.position, players.position),
             height_inches = COALESCE(EXCLUDED.height_inches, players.height_inches),
             weight_lbs = COALESCE(EXCLUDED.weight_lbs, players.weight_lbs),
             class_year = COALESCE(EXCLUDED.class_year, players.class_year),
             jersey_number = COALESCE(EXCLUDED.jersey_number, players.jersey_number),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(&natstat_id)
    .bind(name)
    .bind(team_id)
    .bind(season)
    .bind(position)
    .bind(height_inches)
    .bind(weight_lbs)
    .bind(class_year)
    .bind(jersey_number.as_deref())
    .execute(pool)
    .await?;

    Ok(true)
}

/// Parse height string like "6-4" or "6'4\"" to inches.
fn parse_height(h: &str) -> Option<i32> {
    let parts: Vec<&str> = h.split(['-', '\'', '\"', ' ']).collect();
    if parts.len() >= 2 {
        let feet: i32 = parts[0].trim().parse().ok()?;
        let inches: i32 = parts[1].trim().parse().ok()?;
        Some(feet * 12 + inches)
    } else {
        None
    }
}

fn extract_results(page: &Value) -> Vec<&Value> {
    match page.get("results") {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(Value::Object(obj)) => obj.values().collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_height_dash() {
        assert_eq!(parse_height("6-4"), Some(76));
    }

    #[test]
    fn test_parse_height_apostrophe() {
        assert_eq!(parse_height("6'4"), Some(76));
    }

    #[test]
    fn test_parse_height_invalid() {
        assert_eq!(parse_height("abc"), None);
    }
}
