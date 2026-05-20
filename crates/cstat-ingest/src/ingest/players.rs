use crate::NatStatClient;
use crate::client::NatStatError;
use crate::{current_natstat_season, extract_results, team_id_by_code_and_season};
use chrono::NaiveDate;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// Warn that NatStat's `/players` endpoint returns current-season rosters
/// regardless of the `season` we'll stamp on them. Logged once per top-level
/// command (the `team` subcommand or `ingest_all_rosters`) — the inner
/// fetcher skips it so a 367-team batch doesn't print 367 identical lines.
fn warn_if_historical_season(season: i32, scope: &'static str) {
    let current = current_natstat_season();
    if season != current {
        warn!(
            season,
            current,
            scope,
            "/players endpoint has no historical-roster filter — \
             it returns the current roster only. Box-score ingestion is the \
             authority for historical team_id; roster ingest will only fill \
             metadata fields on existing rows.",
        );
    }
}

/// Ingest players for a specific team using `/players/mbb/{TEAMCODE}`.
/// Returns full roster with height, weight, hometown, nationality.
pub async fn ingest_team_roster(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<u64, NatStatError> {
    warn_if_historical_season(season, "ingest_team_roster");
    ingest_team_roster_inner(client, pool, season, team_code).await
}

async fn ingest_team_roster_inner(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<u64, NatStatError> {
    let response = client.get("players", Some(team_code), None, None).await?;
    let players = extract_results(&response);

    let team_id = team_id_by_code_and_season(pool, Some(team_code), season).await?;

    let mut count = 0u64;
    for player in &players {
        if upsert_player(player, pool, season, team_id).await? {
            count += 1;
        }
    }

    info!(count, season, team_code, "team roster ingested");
    Ok(count)
}

/// Ingest rosters for all teams in the DB for a season.
pub async fn ingest_all_rosters(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    warn_if_historical_season(season, "ingest_all_rosters");

    let teams: Vec<(String,)> =
        sqlx::query_as("SELECT natstat_id FROM teams WHERE season = $1 ORDER BY natstat_id")
            .bind(season)
            .fetch_all(pool)
            .await?;

    let mut total = 0u64;
    for (team_code,) in &teams {
        match ingest_team_roster_inner(client, pool, season, team_code).await {
            Ok(count) => total += count,
            Err(e) => warn!(team_code, error = %e, "failed to ingest roster, skipping"),
        }
    }

    info!(total, season, "all rosters ingested");
    Ok(total)
}

async fn upsert_player(
    player: &Value,
    pool: &PgPool,
    season: i32,
    team_id: Option<Uuid>,
) -> Result<bool, NatStatError> {
    let natstat_id = match player
        .get("code")
        .or_else(|| player.get("id"))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(false),
    };

    // Same defensive chain as `upsert_team::pick_team_name` — NatStat
    // returns `"name": {}` (an empty object) on some records, which
    // makes `Value::as_str()` return None. Without the per-field
    // `as_str()` guard the chain short-circuits on the first key
    // present (even when its value isn't a string) and lands at the
    // "Unknown" fallback. Players with `&` in the name on the teams
    // side were the canary; preempt the same shape on players.
    let name = player
        .get("name")
        .and_then(|n| n.as_str())
        .or_else(|| player.get("full_name").and_then(|n| n.as_str()))
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
        .or_else(|| player.get("player-number"))
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

    let hometown = player.get("hometown").and_then(|h| h.as_str());
    let nationality = player
        .get("nation")
        .or_else(|| player.get("nationality"))
        .and_then(|n| n.as_str());

    let date_of_birth = player
        .get("dateofbirth")
        .and_then(|d| d.as_str())
        .filter(|d| *d != "0000-00-00")
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());

    // NatStat /players/mbb/{TEAMCODE} has no historical-roster filter — it returns
    // the *current* roster regardless of the `season` we stamp these rows with.
    // The box-score path (games.rs) is the authority for `team_id` per season, so
    // never overwrite an existing `team_id` here; only fill it on first insert.
    sqlx::query(
        "INSERT INTO players (id, natstat_id, name, team_id, season, position, height_inches,
         weight_lbs, class_year, jersey_number, hometown, nationality, date_of_birth)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (natstat_id, season) DO UPDATE
         SET name = EXCLUDED.name,
             position = COALESCE(EXCLUDED.position, players.position),
             height_inches = COALESCE(EXCLUDED.height_inches, players.height_inches),
             weight_lbs = COALESCE(EXCLUDED.weight_lbs, players.weight_lbs),
             class_year = COALESCE(EXCLUDED.class_year, players.class_year),
             jersey_number = COALESCE(EXCLUDED.jersey_number, players.jersey_number),
             hometown = COALESCE(EXCLUDED.hometown, players.hometown),
             nationality = COALESCE(EXCLUDED.nationality, players.nationality),
             date_of_birth = COALESCE(EXCLUDED.date_of_birth, players.date_of_birth),
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
    .bind(hometown)
    .bind(nationality)
    .bind(date_of_birth)
    .execute(pool)
    .await?;

    Ok(true)
}

/// Parse height string like "6-4" or "6'4\"" to inches.
fn parse_height(h: &str) -> Option<i32> {
    let parts: Vec<&str> = h.split(['-', '\'', '"', ' ']).collect();
    if parts.len() >= 2 {
        let feet: i32 = parts[0].trim().parse().ok()?;
        let inches: i32 = parts[1].trim().parse().ok()?;
        Some(feet * 12 + inches)
    } else {
        None
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
