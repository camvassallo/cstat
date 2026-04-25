use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A college basketball player.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Player {
    pub id: Uuid,
    pub natstat_id: String,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub season: i32,
    pub position: Option<String>,
    pub height_inches: Option<i32>,
    pub weight_lbs: Option<i32>,
    pub class_year: Option<String>,
    pub jersey_number: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

/// Per-game box score stats for a player.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PlayerGameStats {
    pub id: Uuid,
    pub player_id: Uuid,
    pub game_id: Uuid,
    pub team_id: Uuid,
    pub season: i32,
    pub game_date: NaiveDate,
    pub opponent_id: Option<Uuid>,
    pub is_home: Option<bool>,
    pub is_neutral: Option<bool>,

    // Minutes
    pub minutes: Option<f64>,

    // Scoring
    pub points: Option<i32>,
    pub fgm: Option<i32>,
    pub fga: Option<i32>,
    pub fg_pct: Option<f64>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub tp_pct: Option<f64>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub ft_pct: Option<f64>,

    // Rebounds
    pub off_rebounds: Option<i32>,
    pub def_rebounds: Option<i32>,
    pub total_rebounds: Option<i32>,

    // Playmaking & turnovers
    pub assists: Option<i32>,
    pub turnovers: Option<i32>,
    pub ast_to_ratio: Option<f64>,

    // Defense
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub fouls: Option<i32>,

    // Advanced (if NatStat provides per-game)
    pub offensive_rating: Option<f64>,
    pub defensive_rating: Option<f64>,
    pub usage_rate: Option<f64>,
    pub game_score: Option<f64>,
    pub plus_minus: Option<i32>,

    // Rolling averages (last 5 games, migration 005)
    pub rolling_ppg: Option<f64>,
    pub rolling_rpg: Option<f64>,
    pub rolling_apg: Option<f64>,
    pub rolling_fg_pct: Option<f64>,
    pub rolling_ts_pct: Option<f64>,
    pub rolling_game_score: Option<f64>,

    pub created_at: chrono::NaiveDateTime,
}

/// Aggregated season stats for a player (computed from game stats).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PlayerSeasonStats {
    pub id: Uuid,
    pub player_id: Uuid,
    pub team_id: Uuid,
    pub season: i32,

    pub games_played: i32,
    pub games_started: Option<i32>,
    pub minutes_per_game: Option<f64>,

    // Per-game averages
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    pub fpg: Option<f64>,

    // Shooting
    pub fg_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    pub ft_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,

    // Advanced
    pub offensive_rating: Option<f64>,
    pub defensive_rating: Option<f64>,
    pub net_rating: Option<f64>,
    pub usage_rate: Option<f64>,
    pub bpm: Option<f64>,
    pub obpm: Option<f64>,
    pub dbpm: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,

    // Player SOS: strength of schedule based on opponents this player actually faced
    pub player_sos: Option<f64>,

    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

/// Percentile rankings for a player relative to all D-I players.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PlayerPercentiles {
    pub id: Uuid,
    pub player_id: Uuid,
    pub season: i32,

    pub ppg_pct: Option<f64>,
    pub rpg_pct: Option<f64>,
    pub apg_pct: Option<f64>,
    pub spg_pct: Option<f64>,
    pub bpg_pct: Option<f64>,
    pub fg_pct_pct: Option<f64>,
    pub tp_pct_pct: Option<f64>,
    pub ft_pct_pct: Option<f64>,
    pub effective_fg_pct_pct: Option<f64>,
    pub true_shooting_pct_pct: Option<f64>,
    pub usage_rate_pct: Option<f64>,
    pub offensive_rating_pct: Option<f64>,
    pub defensive_rating_pct: Option<f64>,
    pub bpm_pct: Option<f64>,
    pub player_sos_pct: Option<f64>,

    pub created_at: chrono::NaiveDateTime,
}
