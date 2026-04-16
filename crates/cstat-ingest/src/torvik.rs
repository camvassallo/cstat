//! Barttorvik data client and ingestion.
//!
//! Fetches player season stats (CSV) and per-game box scores (gzip JSON)
//! from barttorvik.com's public endpoints. No authentication required.

use flate2::read::GzDecoder;
use reqwest::Client;
use serde_json::Value;
use std::io::Read;
use tracing::info;

/// Raw player season stats from the Torvik CSV endpoint.
#[derive(Debug, Clone)]
pub struct TorkvikPlayerSeason {
    pub player_name: String,
    pub team: String,
    pub conf: String,
    pub gp: Option<i32>,
    pub min_per: Option<f64>,
    pub o_rtg: Option<f64>,
    pub usage: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub ft_pct: Option<f64>,
    pub two_pm: Option<i32>,
    pub two_pa: Option<i32>,
    pub two_p_pct: Option<f64>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub tp_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    pub class_year: Option<String>,
    pub height: Option<String>,
    pub jersey_number: Option<String>,
    pub porpag: Option<f64>,
    pub adj_oe: Option<f64>,
    pub personal_foul_rate: Option<f64>,
    pub year: Option<i32>,
    pub pid: Option<i32>,
    pub player_type: Option<String>,
    pub recruiting_rank: Option<f64>,
    pub ast_to_tov: Option<f64>,
    pub rim_made: Option<f64>,
    pub rim_attempted: Option<f64>,
    pub mid_made: Option<f64>,
    pub mid_attempted: Option<f64>,
    pub rim_pct: Option<f64>,
    pub mid_pct: Option<f64>,
    pub dunks_made: Option<f64>,
    pub dunks_attempted: Option<f64>,
    pub dunk_pct: Option<f64>,
    pub nba_pick: Option<f64>,
    pub d_rtg: Option<f64>,
    pub adj_de: Option<f64>,
    pub dporpag: Option<f64>,
    pub stops: Option<f64>,
    pub bpm: Option<f64>,
    pub obpm: Option<f64>,
    pub dbpm: Option<f64>,
    pub gbpm: Option<f64>,
    pub total_minutes: Option<f64>,
    pub ogbpm: Option<f64>,
    pub dgbpm: Option<f64>,
    pub oreb_pg: Option<f64>,
    pub dreb_pg: Option<f64>,
    pub treb_pg: Option<f64>,
    pub ast_pg: Option<f64>,
    pub stl_pg: Option<f64>,
    pub blk_pg: Option<f64>,
    pub ppg: Option<f64>,
}

/// Raw per-game player stats from the Torvik gzip JSON endpoint.
#[derive(Debug, Clone)]
pub struct TorkvikGameRow {
    pub date_str: String,
    pub opponent: String,
    pub game_uid: String,
    pub team: String,
    pub player_name: String,
    pub pid: Option<i32>,
    pub year: Option<i32>,
    pub location: Option<String>,
    pub class_year: Option<String>,
    pub height_inches: Option<i32>,
    // Box score
    pub minutes_pct: Option<f64>,
    pub o_rtg: Option<f64>,
    pub usage: Option<f64>,
    pub pts: Option<f64>,
    pub oreb: Option<f64>,
    pub dreb: Option<f64>,
    pub ast: Option<f64>,
    pub tov: Option<f64>,
    pub stl: Option<f64>,
    pub blk: Option<f64>,
    pub pf: Option<f64>,
    // Shooting
    pub two_pm: Option<i32>,
    pub two_pa: Option<i32>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub rim_made: Option<i32>,
    pub rim_attempted: Option<i32>,
    pub mid_made: Option<i32>,
    pub mid_attempted: Option<i32>,
    pub dunks_made: Option<i32>,
    pub dunks_attempted: Option<i32>,
    // Advanced
    pub bpm: Option<f64>,
    pub obpm: Option<f64>,
    pub dbpm: Option<f64>,
    pub possessions: Option<f64>,
}

/// Client for fetching data from barttorvik.com.
pub struct TorkvikClient {
    http: Client,
}

impl Default for TorkvikClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TorkvikClient {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .user_agent("cstat/0.1")
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    /// Fetch player season stats CSV for a given year.
    pub async fn fetch_player_stats(&self, year: i32) -> anyhow::Result<Vec<TorkvikPlayerSeason>> {
        let url = format!("https://barttorvik.com/getadvstats.php?year={year}&csv=1");
        info!(year, "fetching Torvik player stats");
        let body = self.http.get(&url).send().await?.text().await?;
        let players = parse_player_csv(&body)?;
        info!(year, count = players.len(), "parsed Torvik player stats");
        Ok(players)
    }

    /// Fetch per-game player stats (gzip JSON) for a given year.
    pub async fn fetch_game_stats(&self, year: i32) -> anyhow::Result<Vec<TorkvikGameRow>> {
        let url = format!("https://barttorvik.com/{year}_all_advgames.json.gz");
        info!(year, "fetching Torvik game stats (gzip)");
        let bytes = self.http.get(&url).send().await?.bytes().await?;

        // The server may send Content-Encoding: gzip (auto-decompressed by reqwest)
        // or raw gzip bytes. Try parsing as JSON first, fall back to gzip decompress.
        let json_str = match serde_json::from_slice::<Vec<Vec<Value>>>(&bytes) {
            Ok(rows) => {
                let games: Vec<TorkvikGameRow> =
                    rows.iter().filter_map(|r| parse_game_row(r)).collect();
                info!(year, count = games.len(), "parsed Torvik game stats");
                return Ok(games);
            }
            Err(_) => {
                let mut decoder = GzDecoder::new(&bytes[..]);
                let mut s = String::new();
                decoder.read_to_string(&mut s)?;
                s
            }
        };

        let rows: Vec<Vec<Value>> = serde_json::from_str(&json_str)?;
        let games: Vec<TorkvikGameRow> = rows.iter().filter_map(|r| parse_game_row(r)).collect();
        info!(year, count = games.len(), "parsed Torvik game stats");
        Ok(games)
    }
}

// ---------------------------------------------------------------------------
// CSV parsing (headerless, positional columns)
// ---------------------------------------------------------------------------

fn parse_player_csv(body: &str) -> anyhow::Result<Vec<TorkvikPlayerSeason>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body.as_bytes());
    let mut players = Vec::new();

    for result in rdr.records() {
        let rec = result?;
        if rec.len() < 64 {
            continue;
        }
        players.push(TorkvikPlayerSeason {
            player_name: rec.get(0).unwrap_or("").to_string(),
            team: rec.get(1).unwrap_or("").to_string(),
            conf: rec.get(2).unwrap_or("").to_string(),
            gp: parse_int(&rec, 3),
            min_per: parse_f64(&rec, 4),
            o_rtg: parse_f64(&rec, 5),
            usage: parse_f64(&rec, 6),
            effective_fg_pct: parse_f64(&rec, 7),
            true_shooting_pct: parse_f64(&rec, 8),
            orb_pct: parse_f64(&rec, 9),
            drb_pct: parse_f64(&rec, 10),
            ast_pct: parse_f64(&rec, 11),
            tov_pct: parse_f64(&rec, 12),
            ftm: parse_int(&rec, 13),
            fta: parse_int(&rec, 14),
            ft_pct: parse_f64(&rec, 15),
            two_pm: parse_int(&rec, 16),
            two_pa: parse_int(&rec, 17),
            two_p_pct: parse_f64(&rec, 18),
            tpm: parse_int(&rec, 19),
            tpa: parse_int(&rec, 20),
            tp_pct: parse_f64(&rec, 21),
            blk_pct: parse_f64(&rec, 22),
            stl_pct: parse_f64(&rec, 23),
            ft_rate: parse_f64(&rec, 24),
            class_year: non_empty(&rec, 25),
            height: non_empty(&rec, 26),
            jersey_number: non_empty(&rec, 27),
            porpag: parse_f64(&rec, 28),
            adj_oe: parse_f64(&rec, 29),
            personal_foul_rate: parse_f64(&rec, 30),
            year: parse_int(&rec, 31),
            pid: parse_int(&rec, 32),
            player_type: non_empty(&rec, 33),
            recruiting_rank: parse_f64(&rec, 34),
            ast_to_tov: parse_f64(&rec, 35),
            rim_made: parse_f64(&rec, 36),
            rim_attempted: parse_f64(&rec, 37),
            mid_made: parse_f64(&rec, 38),
            mid_attempted: parse_f64(&rec, 39),
            rim_pct: parse_f64(&rec, 40),
            mid_pct: parse_f64(&rec, 41),
            dunks_made: parse_f64(&rec, 42),
            dunks_attempted: parse_f64(&rec, 43),
            dunk_pct: parse_f64(&rec, 44),
            nba_pick: parse_f64(&rec, 45),
            d_rtg: parse_f64(&rec, 46),
            adj_de: parse_f64(&rec, 47),
            dporpag: parse_f64(&rec, 48),
            stops: parse_f64(&rec, 49),
            bpm: parse_f64(&rec, 50),
            obpm: parse_f64(&rec, 51),
            dbpm: parse_f64(&rec, 52),
            gbpm: parse_f64(&rec, 53),
            total_minutes: parse_f64(&rec, 54),
            ogbpm: parse_f64(&rec, 55),
            dgbpm: parse_f64(&rec, 56),
            oreb_pg: parse_f64(&rec, 57),
            dreb_pg: parse_f64(&rec, 58),
            treb_pg: parse_f64(&rec, 59),
            ast_pg: parse_f64(&rec, 60),
            stl_pg: parse_f64(&rec, 61),
            blk_pg: parse_f64(&rec, 62),
            ppg: parse_f64(&rec, 63),
        });
    }
    Ok(players)
}

fn parse_f64(rec: &csv::StringRecord, idx: usize) -> Option<f64> {
    rec.get(idx)?.trim().parse().ok()
}

fn parse_int(rec: &csv::StringRecord, idx: usize) -> Option<i32> {
    rec.get(idx)?.trim().parse::<f64>().ok().map(|v| v as i32)
}

fn non_empty(rec: &csv::StringRecord, idx: usize) -> Option<String> {
    let s = rec.get(idx)?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Gzip JSON parsing (array of arrays, positional)
// ---------------------------------------------------------------------------

fn parse_game_row(row: &[Value]) -> Option<TorkvikGameRow> {
    if row.len() < 53 {
        return None;
    }
    Some(TorkvikGameRow {
        date_str: val_str(row, 0)?,
        opponent: val_str(row, 5)?,
        game_uid: val_str(row, 6)?,
        team: val_str(row, 47)?,
        player_name: val_str(row, 48)?,
        pid: val_i32(row, 51),
        year: val_i32(row, 52),
        location: val_str_opt(row, 46),
        class_year: val_str_opt(row, 50),
        height_inches: val_i32(row, 49),
        minutes_pct: val_f64(row, 8),
        o_rtg: val_f64(row, 9),
        usage: val_f64(row, 10),
        pts: val_f64(row, 33),
        oreb: val_f64(row, 34),
        dreb: val_f64(row, 35),
        ast: val_f64(row, 36),
        tov: val_f64(row, 37),
        stl: val_f64(row, 38),
        blk: val_f64(row, 39),
        pf: val_f64(row, 42),
        two_pm: val_i32(row, 23),
        two_pa: val_i32(row, 24),
        tpm: val_i32(row, 25),
        tpa: val_i32(row, 26),
        ftm: val_i32(row, 27),
        fta: val_i32(row, 28),
        rim_made: val_i32(row, 19),
        rim_attempted: val_i32(row, 20),
        mid_made: val_i32(row, 21),
        mid_attempted: val_i32(row, 22),
        dunks_made: val_i32(row, 17),
        dunks_attempted: val_i32(row, 18),
        bpm: val_f64(row, 44),
        obpm: val_f64(row, 30),
        dbpm: val_f64(row, 31),
        possessions: val_f64(row, 43),
    })
}

fn val_str(row: &[Value], idx: usize) -> Option<String> {
    match &row[idx] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn val_str_opt(row: &[Value], idx: usize) -> Option<String> {
    match &row[idx] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn val_f64(row: &[Value], idx: usize) -> Option<f64> {
    match &row[idx] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn val_i32(row: &[Value], idx: usize) -> Option<i32> {
    match &row[idx] {
        Value::Number(n) => n.as_f64().map(|v| v as i32),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|v| v as i32),
        _ => None,
    }
}
