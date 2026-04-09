use crate::NatStatClient;
use crate::client::NatStatError;
use crate::extract_results;
use chrono::NaiveDate;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// Ingest all MBB games for a season, including player box scores via hydration.
///
/// Uses `games;boxscores` hydration to get game results + box scores in fewer API calls.
pub async fn ingest_games(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let pages = client
        .get_all_pages("games", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let games = extract_results(page);
        for game in games {
            if upsert_game(game, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, "games ingested");
    Ok(count)
}

/// Ingest games for a specific date range.
pub async fn ingest_games_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<u64, NatStatError> {
    let range = format!("{},{}", start, end);
    let pages = client.get_all_pages("games", Some(&range), None).await?;

    let mut count = 0u64;
    for page in &pages {
        let games = extract_results(page);
        for game in games {
            if upsert_game(game, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, start, end, "games ingested for date range");
    Ok(count)
}

/// Ingest player performances (box scores) for all games in a season.
///
/// Uses the `playerperfs` endpoint filtered by season.
pub async fn ingest_player_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let pages = client
        .get_all_pages("playerperfs", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            if upsert_player_game_stats(perf, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, "player performances ingested");
    Ok(count)
}

/// Ingest player performances for a specific team and season.
pub async fn ingest_player_performances_by_team(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<u64, NatStatError> {
    let range = format!("{},{}", season, team_code);
    let pages = client
        .get_all_pages("playerperfs", Some(&range), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            if upsert_player_game_stats(perf, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(
        count,
        season, team_code, "player performances ingested for team"
    );
    Ok(count)
}

/// Ingest player performances for a specific date range.
pub async fn ingest_player_performances_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<u64, NatStatError> {
    let range = format!("{},{}", start, end);
    let pages = client
        .get_all_pages("playerperfs", Some(&range), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            if upsert_player_game_stats(perf, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(
        count,
        season, start, end, "player performances ingested for date range"
    );
    Ok(count)
}

async fn upsert_game(game: &Value, pool: &PgPool, season: i32) -> Result<bool, NatStatError> {
    let natstat_id = match game
        .get("id")
        .or_else(|| game.get("code"))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(false),
    };

    let game_date = match game
        .get("gameday")
        .or_else(|| game.get("gamedate"))
        .or_else(|| game.get("date"))
        .and_then(|d| d.as_str())
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    {
        Some(d) => d,
        None => {
            warn!(natstat_id, "game missing date, skipping");
            return Ok(false);
        }
    };

    // Resolve home/away team IDs — NatStat v4 uses "home-code" / "visitor-code"
    let home_code = game
        .get("home-code")
        .or_else(|| game.get("home_code"))
        .and_then(|c| c.as_str());
    let away_code = game
        .get("visitor-code")
        .or_else(|| game.get("away-code"))
        .or_else(|| game.get("away_code"))
        .and_then(|c| c.as_str());

    let home_team_id = resolve_team_id(pool, home_code, season).await?;
    let away_team_id = resolve_team_id(pool, away_code, season).await?;

    let home_score = game
        .get("score-home")
        .or_else(|| game.get("home_score"))
        .and_then(|s| {
            s.as_i64()
                .or_else(|| s.as_str().and_then(|v| v.parse().ok()))
        })
        .map(|s| s as i32);

    let away_score = game
        .get("score-vis")
        .or_else(|| game.get("away_score"))
        .and_then(|s| {
            s.as_i64()
                .or_else(|| s.as_str().and_then(|v| v.parse().ok()))
        })
        .map(|s| s as i32);

    let is_neutral = game
        .get("neutral")
        .or_else(|| game.get("is_neutral"))
        .and_then(|n| {
            n.as_bool()
                .or_else(|| n.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| n.as_i64().map(|i| i != 0))
        })
        .unwrap_or(false);

    let is_conference = game
        .get("conference")
        .or_else(|| game.get("is_conference"))
        .and_then(|c| {
            c.as_bool()
                .or_else(|| c.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| c.as_i64().map(|i| i != 0))
        });

    let is_postseason = game
        .get("postseason")
        .or_else(|| game.get("is_postseason"))
        .and_then(|p| {
            p.as_bool()
                .or_else(|| p.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| p.as_i64().map(|i| i != 0))
        });

    let venue = game
        .get("venue")
        .and_then(|v| v.get("name").or(Some(v)))
        .and_then(|v| v.as_str())
        .or_else(|| game.get("venue-name").and_then(|v| v.as_str()));

    let venue_code = game
        .get("venue-code")
        .or_else(|| game.get("venue").and_then(|v| v.get("code")))
        .and_then(|c| c.as_str());

    let overtime = game
        .get("overtime")
        .and_then(|o| o.as_str())
        .filter(|o| *o != "N");

    let attendance = game
        .get("attendance")
        .and_then(|a| {
            a.as_i64()
                .or_else(|| a.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|a| a as i32);

    let status = game
        .get("gamestatus")
        .or_else(|| game.get("status"))
        .and_then(|s| s.as_str());

    // Half scores: line-home/line-vis contain {p1: "32", p2: "35"}
    let home_half1 = game
        .get("line-home")
        .and_then(|l| l.get("p1"))
        .and_then(get_i32_val);
    let home_half2 = game
        .get("line-home")
        .and_then(|l| l.get("p2"))
        .and_then(get_i32_val);
    let away_half1 = game
        .get("line-vis")
        .and_then(|l| l.get("p1"))
        .and_then(get_i32_val);
    let away_half2 = game
        .get("line-vis")
        .and_then(|l| l.get("p2"))
        .and_then(get_i32_val);

    sqlx::query(
        "INSERT INTO games (id, natstat_id, season, game_date, home_team_id, away_team_id,
         home_score, away_score, is_neutral_site, is_conference, is_postseason, venue,
         venue_code, overtime, attendance, status,
         home_half1, home_half2, away_half1, away_half2)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17, $18, $19, $20)
         ON CONFLICT (natstat_id) DO UPDATE
         SET home_score = COALESCE(EXCLUDED.home_score, games.home_score),
             away_score = COALESCE(EXCLUDED.away_score, games.away_score),
             home_team_id = COALESCE(EXCLUDED.home_team_id, games.home_team_id),
             away_team_id = COALESCE(EXCLUDED.away_team_id, games.away_team_id),
             is_neutral_site = EXCLUDED.is_neutral_site,
             venue = COALESCE(EXCLUDED.venue, games.venue),
             venue_code = COALESCE(EXCLUDED.venue_code, games.venue_code),
             overtime = COALESCE(EXCLUDED.overtime, games.overtime),
             attendance = COALESCE(EXCLUDED.attendance, games.attendance),
             status = COALESCE(EXCLUDED.status, games.status),
             home_half1 = COALESCE(EXCLUDED.home_half1, games.home_half1),
             home_half2 = COALESCE(EXCLUDED.home_half2, games.home_half2),
             away_half1 = COALESCE(EXCLUDED.away_half1, games.away_half1),
             away_half2 = COALESCE(EXCLUDED.away_half2, games.away_half2),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(&natstat_id)
    .bind(season)
    .bind(game_date)
    .bind(home_team_id)
    .bind(away_team_id)
    .bind(home_score)
    .bind(away_score)
    .bind(is_neutral)
    .bind(is_conference)
    .bind(is_postseason)
    .bind(venue)
    .bind(venue_code)
    .bind(overtime)
    .bind(attendance)
    .bind(status)
    .bind(home_half1)
    .bind(home_half2)
    .bind(away_half1)
    .bind(away_half2)
    .execute(pool)
    .await?;

    Ok(true)
}

async fn upsert_player_game_stats(
    perf: &Value,
    pool: &PgPool,
    season: i32,
) -> Result<bool, NatStatError> {
    // Get player code — NatStat v4 uses "player-code" or nested player.code
    let player_natstat_id = match perf
        .get("player-code")
        .or_else(|| perf.get("player_code"))
        .or_else(|| perf.get("player").and_then(|p| p.get("code")))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(false),
    };

    // Get game code — NatStat v4 uses "game-code" or nested game.code
    let game_natstat_id = match perf
        .get("game-code")
        .or_else(|| perf.get("game_code"))
        .or_else(|| perf.get("game").and_then(|g| g.get("code")))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(false),
    };

    // Resolve IDs
    let player_id: Option<Uuid> =
        sqlx::query_as("SELECT id FROM players WHERE natstat_id = $1 AND season = $2")
            .bind(&player_natstat_id)
            .bind(season)
            .fetch_optional(pool)
            .await?
            .map(|(id,): (Uuid,)| id);

    let game_row: Option<(Uuid, NaiveDate)> =
        sqlx::query_as("SELECT id, game_date FROM games WHERE natstat_id = $1")
            .bind(&game_natstat_id)
            .fetch_optional(pool)
            .await?;

    let (Some(player_id), Some((game_id, game_date))) = (player_id, game_row) else {
        return Ok(false);
    };

    // Resolve team — NatStat v4 uses "team-code" or nested team.code
    let team_code = perf
        .get("team-code")
        .or_else(|| perf.get("team_code"))
        .or_else(|| perf.get("team").and_then(|t| t.get("code")))
        .and_then(|c| c.as_str());
    let Some(team_id) = resolve_team_id(pool, team_code, season).await? else {
        return Ok(false);
    };

    // playerperfs returns stats flat on the perf object (not nested under "stats")
    let minutes = get_f64(perf, &["min", "minutes", "mp"]);
    let points = get_i32(perf, &["pts", "points"]);
    let fgm = get_i32(perf, &["fgm", "fg"]);
    let fga = get_i32(perf, &["fga"]);
    let fg_pct = get_f64(perf, &["fgpct", "fg_pct", "fgp"]);
    let tpm = get_i32(perf, &["threefm", "tpm", "3pm", "fg3m"]);
    let tpa = get_i32(perf, &["threefa", "tpa", "3pa", "fg3a"]);
    let tp_pct = get_f64(perf, &["threefgpct", "tp_pct", "3p_pct"]);
    let ftm = get_i32(perf, &["ftm", "ft"]);
    let fta = get_i32(perf, &["fta"]);
    let ft_pct = get_f64(perf, &["ftpct", "ft_pct", "ftp"]);
    let off_rebounds = get_i32(perf, &["oreb", "orb"]);
    let def_rebounds = get_i32(perf, &["dreb", "drb"]);
    let total_rebounds = get_i32(perf, &["reb", "trb"]);
    let assists = get_i32(perf, &["ast", "assists"]);
    let turnovers = get_i32(perf, &["to", "tov"]);
    let steals = get_i32(perf, &["stl", "steals"]);
    let blocks = get_i32(perf, &["blk", "blocks"]);
    let fouls = get_i32(perf, &["pf", "fouls"]);
    let plus_minus = get_i32(perf, &["plus_minus", "pm"]);

    // NatStat advanced metrics
    let starter = perf
        .get("starter")
        .and_then(|s| s.as_str())
        .map(|s| s == "Y");
    let efficiency = get_f64(perf, &["eff", "efficiency"]);
    let usage_rate = get_f64(perf, &["usgpct", "usage_rate"]).map(|v| v / 100.0);
    let two_fg_pct = get_f64(perf, &["twofgpct"]);
    let presence_rate = get_f64(perf, &["presencerate"]);
    let adj_presence_rate = get_f64(perf, &["adjpresencerate"]);
    let perf_score = get_f64(perf, &["perfscore"]);
    let perf_score_season_avg = get_f64(perf, &["perfscoreseasonavg"]);
    let team_possessions = get_i32(perf, &["teamposs"]);

    // Team context stats (needed for rate stat calculations)
    let team_fga = get_i32(perf, &["teamfga"]);
    let team_fta = get_i32(perf, &["teamfta"]);
    let team_turnovers = get_i32(perf, &["teamto"]);
    let team_fgm = get_i32(perf, &["teamfgm"]);

    // Update player jersey number if available
    let jersey_number = perf
        .get("player-number")
        .and_then(|n| n.as_str().map(String::from).or_else(|| Some(n.to_string())))
        .map(|s| s.trim_matches('"').to_string());
    if let Some(ref jersey) = jersey_number {
        sqlx::query("UPDATE players SET jersey_number = $1, updated_at = now() WHERE id = $2 AND jersey_number IS NULL")
            .bind(jersey)
            .bind(player_id)
            .execute(pool)
            .await?;
    }

    // Game context: is_home from game.loc
    let is_home = perf
        .get("game")
        .and_then(|g| g.get("loc"))
        .and_then(|l| l.as_str())
        .map(|l| l == "H");

    // Resolve opponent
    let opponent_code = perf
        .get("opponent")
        .and_then(|o| o.get("code"))
        .or_else(|| perf.get("opponent-code"))
        .and_then(|c| c.as_str());
    let opponent_id = resolve_team_id(pool, opponent_code, season).await?;

    sqlx::query(
        "INSERT INTO player_game_stats (
            id, player_id, game_id, team_id, season, game_date, opponent_id, is_home,
            minutes, points, fgm, fga, fg_pct, tpm, tpa, tp_pct,
            ftm, fta, ft_pct, off_rebounds, def_rebounds, total_rebounds,
            assists, turnovers, steals, blocks, fouls, plus_minus,
            starter, efficiency, usage_rate, two_fg_pct,
            presence_rate, adj_presence_rate, perf_score, perf_score_season_avg,
            team_possessions, team_fga, team_fta, team_turnovers, team_fgm
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28,
                 $29, $30, $31, $32, $33, $34, $35, $36, $37, $38, $39, $40, $41)
         ON CONFLICT (player_id, game_id) DO UPDATE
         SET minutes = COALESCE(EXCLUDED.minutes, player_game_stats.minutes),
             points = COALESCE(EXCLUDED.points, player_game_stats.points),
             fgm = COALESCE(EXCLUDED.fgm, player_game_stats.fgm),
             fga = COALESCE(EXCLUDED.fga, player_game_stats.fga),
             fg_pct = COALESCE(EXCLUDED.fg_pct, player_game_stats.fg_pct),
             tpm = COALESCE(EXCLUDED.tpm, player_game_stats.tpm),
             tpa = COALESCE(EXCLUDED.tpa, player_game_stats.tpa),
             tp_pct = COALESCE(EXCLUDED.tp_pct, player_game_stats.tp_pct),
             ftm = COALESCE(EXCLUDED.ftm, player_game_stats.ftm),
             fta = COALESCE(EXCLUDED.fta, player_game_stats.fta),
             ft_pct = COALESCE(EXCLUDED.ft_pct, player_game_stats.ft_pct),
             off_rebounds = COALESCE(EXCLUDED.off_rebounds, player_game_stats.off_rebounds),
             def_rebounds = COALESCE(EXCLUDED.def_rebounds, player_game_stats.def_rebounds),
             total_rebounds = COALESCE(EXCLUDED.total_rebounds, player_game_stats.total_rebounds),
             assists = COALESCE(EXCLUDED.assists, player_game_stats.assists),
             turnovers = COALESCE(EXCLUDED.turnovers, player_game_stats.turnovers),
             steals = COALESCE(EXCLUDED.steals, player_game_stats.steals),
             blocks = COALESCE(EXCLUDED.blocks, player_game_stats.blocks),
             fouls = COALESCE(EXCLUDED.fouls, player_game_stats.fouls),
             plus_minus = COALESCE(EXCLUDED.plus_minus, player_game_stats.plus_minus),
             starter = COALESCE(EXCLUDED.starter, player_game_stats.starter),
             efficiency = COALESCE(EXCLUDED.efficiency, player_game_stats.efficiency),
             usage_rate = COALESCE(EXCLUDED.usage_rate, player_game_stats.usage_rate),
             two_fg_pct = COALESCE(EXCLUDED.two_fg_pct, player_game_stats.two_fg_pct),
             presence_rate = COALESCE(EXCLUDED.presence_rate, player_game_stats.presence_rate),
             adj_presence_rate = COALESCE(EXCLUDED.adj_presence_rate, player_game_stats.adj_presence_rate),
             perf_score = COALESCE(EXCLUDED.perf_score, player_game_stats.perf_score),
             perf_score_season_avg = COALESCE(EXCLUDED.perf_score_season_avg, player_game_stats.perf_score_season_avg),
             team_possessions = COALESCE(EXCLUDED.team_possessions, player_game_stats.team_possessions),
             team_fga = COALESCE(EXCLUDED.team_fga, player_game_stats.team_fga),
             team_fta = COALESCE(EXCLUDED.team_fta, player_game_stats.team_fta),
             team_turnovers = COALESCE(EXCLUDED.team_turnovers, player_game_stats.team_turnovers),
             team_fgm = COALESCE(EXCLUDED.team_fgm, player_game_stats.team_fgm),
             is_home = COALESCE(EXCLUDED.is_home, player_game_stats.is_home)",
    )
    .bind(Uuid::new_v4())
    .bind(player_id)
    .bind(game_id)
    .bind(team_id)
    .bind(season)
    .bind(game_date)
    .bind(opponent_id)
    .bind(is_home)
    .bind(minutes)
    .bind(points)
    .bind(fgm)
    .bind(fga)
    .bind(fg_pct)
    .bind(tpm)
    .bind(tpa)
    .bind(tp_pct)
    .bind(ftm)
    .bind(fta)
    .bind(ft_pct)
    .bind(off_rebounds)
    .bind(def_rebounds)
    .bind(total_rebounds)
    .bind(assists)
    .bind(turnovers)
    .bind(steals)
    .bind(blocks)
    .bind(fouls)
    .bind(plus_minus)
    .bind(starter)
    .bind(efficiency)
    .bind(usage_rate)
    .bind(two_fg_pct)
    .bind(presence_rate)
    .bind(adj_presence_rate)
    .bind(perf_score)
    .bind(perf_score_season_avg)
    .bind(team_possessions)
    .bind(team_fga)
    .bind(team_fta)
    .bind(team_turnovers)
    .bind(team_fgm)
    .execute(pool)
    .await?;

    Ok(true)
}

async fn resolve_team_id(
    pool: &PgPool,
    code: Option<&str>,
    season: i32,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(code) = code else { return Ok(None) };
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
            .bind(code)
            .bind(season)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Extract an i32 from a single JSON value (not searching multiple keys).
fn get_i32_val(v: &Value) -> Option<i32> {
    v.as_i64()
        .map(|i| i as i32)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Extract a float from a JSON value, trying multiple field names.
fn get_f64(v: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(val) = v.get(*key) {
            if let Some(f) = val.as_f64() {
                return Some(f);
            }
            if let Some(s) = val.as_str()
                && let Ok(f) = s.parse::<f64>()
            {
                return Some(f);
            }
        }
    }
    None
}

/// Extract an int from a JSON value, trying multiple field names.
fn get_i32(v: &Value, keys: &[&str]) -> Option<i32> {
    for key in keys {
        if let Some(val) = v.get(*key) {
            if let Some(i) = val.as_i64() {
                return Some(i as i32);
            }
            if let Some(s) = val.as_str()
                && let Ok(i) = s.parse::<i32>()
            {
                return Some(i);
            }
        }
    }
    None
}

/// Ingest team performances for all teams in a season.
pub async fn ingest_all_team_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let teams: Vec<(String,)> =
        sqlx::query_as("SELECT natstat_id FROM teams WHERE season = $1 ORDER BY natstat_id")
            .bind(season)
            .fetch_all(pool)
            .await?;

    let total_teams = teams.len();
    let mut total = 0u64;
    for (i, (team_code,)) in teams.iter().enumerate() {
        if (i + 1) % 25 == 0 || i + 1 == total_teams {
            info!(
                progress = format!("{}/{}", i + 1, total_teams),
                "ingesting team performances"
            );
        }
        match ingest_team_performances(client, pool, season, team_code).await {
            Ok(count) => total += count,
            Err(e) => {
                tracing::warn!(team_code, error = %e, "failed to ingest team performances, skipping")
            }
        }
    }

    info!(total, season, "all team performances ingested");
    Ok(total)
}

/// Ingest team performances (team-level box scores) for a specific team and season.
pub async fn ingest_team_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<u64, NatStatError> {
    let range = format!("{},{}", season, team_code);
    let pages = client
        .get_all_pages("teamperfs", Some(&range), None)
        .await?;

    let mut count = 0u64;
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            if upsert_team_game_stats(perf, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, season, team_code, "team performances ingested");
    Ok(count)
}

async fn upsert_team_game_stats(
    perf: &Value,
    pool: &PgPool,
    season: i32,
) -> Result<bool, NatStatError> {
    let team_code = perf
        .get("team-code")
        .or_else(|| perf.get("team").and_then(|t| t.get("code")))
        .and_then(|c| c.as_str());

    let game_natstat_id = perf
        .get("game")
        .and_then(|g| g.get("id"))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
        .map(|s| s.trim_matches('"').to_string());

    let Some(game_id_str) = game_natstat_id else {
        return Ok(false);
    };

    let team_id = resolve_team_id(pool, team_code, season).await?;
    let Some(team_id) = team_id else {
        return Ok(false);
    };

    let game_row: Option<(Uuid, NaiveDate)> =
        sqlx::query_as("SELECT id, game_date FROM games WHERE natstat_id = $1")
            .bind(&game_id_str)
            .fetch_optional(pool)
            .await?;

    let Some((game_id, game_date)) = game_row else {
        return Ok(false);
    };

    let opponent_code = perf
        .get("opponent")
        .and_then(|o| o.get("code"))
        .and_then(|c| c.as_str());
    let opponent_id = resolve_team_id(pool, opponent_code, season).await?;

    let is_home = perf
        .get("game")
        .and_then(|g| g.get("location"))
        .and_then(|l| l.as_str())
        .map(|l| l == "H");

    let win = perf
        .get("game")
        .and_then(|g| g.get("winorloss"))
        .and_then(|w| w.as_str())
        .map(|w| w == "W");

    let league = perf.get("league").and_then(|l| l.as_str());

    let stats = perf.get("stats").unwrap_or(perf);

    let minutes = get_i32(stats, &["min"]);
    let points = get_i32(stats, &["pts"]);
    let fgm = get_i32(stats, &["fgm"]);
    let fga = get_i32(stats, &["fga"]);
    let tpm = get_i32(stats, &["threefm"]);
    let tpa = get_i32(stats, &["threefa"]);
    let ftm = get_i32(stats, &["ftm"]);
    let fta = get_i32(stats, &["fta"]);
    let off_rebounds = get_i32(stats, &["oreb"]);
    let total_rebounds = get_i32(stats, &["reb"]);
    let assists = get_i32(stats, &["ast"]);
    let steals = get_i32(stats, &["stl"]);
    let blocks = get_i32(stats, &["blk"]);
    let turnovers = get_i32(stats, &["to"]);
    let fouls = get_i32(stats, &["f", "pf"]);

    // Derive def_rebounds = total - off
    let def_rebounds = match (total_rebounds, off_rebounds) {
        (Some(t), Some(o)) => Some(t - o),
        _ => None,
    };

    sqlx::query(
        "INSERT INTO team_game_stats (
            id, team_id, game_id, season, game_date, opponent_id, is_home, win, league,
            minutes, points, fgm, fga, tpm, tpa, ftm, fta,
            off_rebounds, def_rebounds, total_rebounds, assists, steals, blocks, turnovers, fouls
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
        ON CONFLICT (team_id, game_id) DO UPDATE
        SET points = COALESCE(EXCLUDED.points, team_game_stats.points),
            fgm = COALESCE(EXCLUDED.fgm, team_game_stats.fgm),
            fga = COALESCE(EXCLUDED.fga, team_game_stats.fga),
            tpm = COALESCE(EXCLUDED.tpm, team_game_stats.tpm),
            tpa = COALESCE(EXCLUDED.tpa, team_game_stats.tpa),
            ftm = COALESCE(EXCLUDED.ftm, team_game_stats.ftm),
            fta = COALESCE(EXCLUDED.fta, team_game_stats.fta),
            off_rebounds = COALESCE(EXCLUDED.off_rebounds, team_game_stats.off_rebounds),
            def_rebounds = COALESCE(EXCLUDED.def_rebounds, team_game_stats.def_rebounds),
            total_rebounds = COALESCE(EXCLUDED.total_rebounds, team_game_stats.total_rebounds),
            assists = COALESCE(EXCLUDED.assists, team_game_stats.assists),
            steals = COALESCE(EXCLUDED.steals, team_game_stats.steals),
            blocks = COALESCE(EXCLUDED.blocks, team_game_stats.blocks),
            turnovers = COALESCE(EXCLUDED.turnovers, team_game_stats.turnovers),
            fouls = COALESCE(EXCLUDED.fouls, team_game_stats.fouls),
            is_home = COALESCE(EXCLUDED.is_home, team_game_stats.is_home),
            win = COALESCE(EXCLUDED.win, team_game_stats.win)",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(game_id)
    .bind(season)
    .bind(game_date)
    .bind(opponent_id)
    .bind(is_home)
    .bind(win)
    .bind(league)
    .bind(minutes)
    .bind(points)
    .bind(fgm)
    .bind(fga)
    .bind(tpm)
    .bind(tpa)
    .bind(ftm)
    .bind(fta)
    .bind(off_rebounds)
    .bind(def_rebounds)
    .bind(total_rebounds)
    .bind(assists)
    .bind(steals)
    .bind(blocks)
    .bind(turnovers)
    .bind(fouls)
    .execute(pool)
    .await?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_f64_from_number() {
        let v = json!({"min": 32.5});
        assert_eq!(get_f64(&v, &["min"]), Some(32.5));
    }

    #[test]
    fn test_get_f64_from_string() {
        let v = json!({"min": "32.5"});
        assert_eq!(get_f64(&v, &["min"]), Some(32.5));
    }

    #[test]
    fn test_get_f64_fallback_key() {
        let v = json!({"mp": 28.0});
        assert_eq!(get_f64(&v, &["min", "minutes", "mp"]), Some(28.0));
    }

    #[test]
    fn test_get_i32_from_number() {
        let v = json!({"pts": 25});
        assert_eq!(get_i32(&v, &["pts"]), Some(25));
    }

    #[test]
    fn test_get_i32_from_string() {
        let v = json!({"pts": "25"});
        assert_eq!(get_i32(&v, &["pts"]), Some(25));
    }
}
