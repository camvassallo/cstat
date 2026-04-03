use crate::NatStatClient;
use crate::client::NatStatError;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// Ingest all MBB teams for a given season from teamcodes.
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
        let teams = extract_results(page);
        for team in teams {
            if upsert_team(team, pool, season).await? {
                count += 1;
            }
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

/// Ingest detailed team data (TCR, ELO) from the /teams endpoint.
/// Fetches `/teams/mbb/{TEAMCODE}` for each team in the DB.
pub async fn ingest_team_details(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let teams: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, natstat_id FROM teams WHERE season = $1 ORDER BY natstat_id")
            .bind(season)
            .fetch_all(pool)
            .await?;

    let mut count = 0u64;
    for (team_id, team_code) in &teams {
        match ingest_single_team_details(client, pool, season, team_id, team_code).await {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => warn!(team_code, error = %e, "failed to ingest team details, skipping"),
        }
    }

    info!(count, season, "team details ingested");
    Ok(count)
}

/// Ingest details for a single team.
pub async fn ingest_single_team_details(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_id: &Uuid,
    team_code: &str,
) -> Result<bool, NatStatError> {
    let response = client.get("teams", Some(team_code), None, None).await?;
    let results = extract_results(&response);

    let Some(team) = results.first() else {
        return Ok(false);
    };

    // Extract ELO
    let elo_rank = team.get("elo").and_then(|e| e.get("rank")).and_then(|r| {
        r.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or(r.as_f64())
    });

    // Find the current season competition entry
    let season_key = format!("season_{season}");
    let competition = team.get(&season_key).and_then(|s| s.get("competition_0"));

    let (wins, losses, conference) = if let Some(comp) = competition {
        let w = comp
            .get("wins")
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
            .map(|v| v as i32);
        let l = comp
            .get("losses")
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
            .map(|v| v as i32);
        let conf = comp
            .get("league")
            .or_else(|| comp.get("conference"))
            .and_then(|c| c.as_str());
        (w, l, conf)
    } else {
        (None, None, None)
    };

    // Extract TCR (Team Composite Rating)
    let tcr = competition.and_then(|c| c.get("tcr"));
    let tcr_rank = tcr
        .and_then(|t| t.get("tcrrank"))
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
        .map(|v| v as i32);
    let tcr_points = get_f64_from(tcr, "tcrpoints");
    let tcr_adjusted = get_f64_from(tcr, "tcradjusted");
    let efficiency = get_f64_from(tcr, "efficiency");
    let defense = get_f64_from(tcr, "defense");
    let point_diff = get_f64_from(tcr, "pointdiff");
    let pythag_win_pct = get_f64_from(tcr, "pythagwinpct");
    let luck = get_f64_from(tcr, "luck");
    let opp_win_pct = get_f64_from(tcr, "oppwinpct");
    let opp_opp_win_pct = get_f64_from(tcr, "oppoppwinpct");
    let road_win_pct = get_f64_from(tcr, "roadwinpct");

    // Update conference on the team record if we got it
    if let Some(conf) = conference {
        sqlx::query("UPDATE teams SET conference = $1, updated_at = now() WHERE id = $2")
            .bind(conf)
            .bind(team_id)
            .execute(pool)
            .await?;
    }

    sqlx::query(
        "INSERT INTO team_season_stats (id, team_id, season, wins, losses, elo,
         tcr_rank, tcr_points, tcr_adjusted, efficiency, defense, point_diff,
         pythag_win_pct, luck, opp_win_pct, opp_opp_win_pct, road_win_pct, conference)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
         ON CONFLICT (team_id, season) DO UPDATE
         SET wins = COALESCE(EXCLUDED.wins, team_season_stats.wins),
             losses = COALESCE(EXCLUDED.losses, team_season_stats.losses),
             elo = COALESCE(EXCLUDED.elo, team_season_stats.elo),
             tcr_rank = COALESCE(EXCLUDED.tcr_rank, team_season_stats.tcr_rank),
             tcr_points = COALESCE(EXCLUDED.tcr_points, team_season_stats.tcr_points),
             tcr_adjusted = COALESCE(EXCLUDED.tcr_adjusted, team_season_stats.tcr_adjusted),
             efficiency = COALESCE(EXCLUDED.efficiency, team_season_stats.efficiency),
             defense = COALESCE(EXCLUDED.defense, team_season_stats.defense),
             point_diff = COALESCE(EXCLUDED.point_diff, team_season_stats.point_diff),
             pythag_win_pct = COALESCE(EXCLUDED.pythag_win_pct, team_season_stats.pythag_win_pct),
             luck = COALESCE(EXCLUDED.luck, team_season_stats.luck),
             opp_win_pct = COALESCE(EXCLUDED.opp_win_pct, team_season_stats.opp_win_pct),
             opp_opp_win_pct = COALESCE(EXCLUDED.opp_opp_win_pct, team_season_stats.opp_opp_win_pct),
             road_win_pct = COALESCE(EXCLUDED.road_win_pct, team_season_stats.road_win_pct),
             conference = COALESCE(EXCLUDED.conference, team_season_stats.conference),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(season)
    .bind(wins.unwrap_or(0))
    .bind(losses.unwrap_or(0))
    .bind(elo_rank)
    .bind(tcr_rank)
    .bind(tcr_points)
    .bind(tcr_adjusted)
    .bind(efficiency)
    .bind(defense)
    .bind(point_diff)
    .bind(pythag_win_pct)
    .bind(luck)
    .bind(opp_win_pct)
    .bind(opp_opp_win_pct)
    .bind(road_win_pct)
    .bind(conference)
    .execute(pool)
    .await?;

    Ok(true)
}

fn get_f64_from(parent: Option<&Value>, key: &str) -> Option<f64> {
    parent?.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn extract_results(page: &Value) -> Vec<&Value> {
    const META_KEYS: &[&str] = &["meta", "user", "query", "success", "error", "warnings"];
    if let Some(obj) = page.as_object() {
        for (key, value) in obj {
            if META_KEYS.contains(&key.as_str()) {
                continue;
            }
            return match value {
                Value::Array(arr) => arr.iter().collect(),
                Value::Object(inner) => inner.values().collect(),
                _ => vec![],
            };
        }
    }
    vec![]
}
