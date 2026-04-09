use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A college basketball team.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Team {
    pub id: Uuid,
    pub natstat_id: String,
    pub name: String,
    pub short_name: Option<String>,
    pub conference: Option<String>,
    pub division: Option<String>,
    pub season: i32,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

/// Aggregated team-level stats for a season (fetched from NatStat, not yet player-derived).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TeamSeasonStats {
    pub id: Uuid,
    pub team_id: Uuid,
    pub season: i32,

    // Record
    pub wins: i32,
    pub losses: i32,

    // Four factors (offense)
    pub adj_offense: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub turnover_pct: Option<f64>,
    pub off_rebound_pct: Option<f64>,
    pub ft_rate: Option<f64>,

    // Four factors (defense)
    pub adj_defense: Option<f64>,
    pub opp_effective_fg_pct: Option<f64>,
    pub opp_turnover_pct: Option<f64>,
    pub def_rebound_pct: Option<f64>,
    pub opp_ft_rate: Option<f64>,

    // Tempo & efficiency
    pub adj_tempo: Option<f64>,
    pub adj_efficiency_margin: Option<f64>,

    // Strength of schedule
    pub sos: Option<f64>,
    pub sos_rank: Option<i32>,

    // Ratings
    pub elo: Option<f64>,
    pub rpi: Option<f64>,

    // NatStat TCR / power metrics (migration 002)
    pub tcr_rank: Option<i32>,
    pub tcr_points: Option<f64>,
    pub tcr_adjusted: Option<f64>,
    pub efficiency: Option<f64>,
    pub defense: Option<f64>,
    pub point_diff: Option<f64>,
    pub pythag_win_pct: Option<f64>,
    pub luck: Option<f64>,
    pub opp_win_pct: Option<f64>,
    pub opp_opp_win_pct: Option<f64>,
    pub road_win_pct: Option<f64>,
    pub conference: Option<String>,

    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

/// Per-game box score stats for a team.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TeamGameStats {
    pub id: Uuid,
    pub team_id: Uuid,
    pub game_id: Uuid,
    pub season: i32,
    pub game_date: NaiveDate,
    pub opponent_id: Option<Uuid>,
    pub is_home: Option<bool>,
    pub win: Option<bool>,
    pub league: Option<String>,

    // Box score
    pub minutes: Option<i32>,
    pub points: Option<i32>,
    pub fgm: Option<i32>,
    pub fga: Option<i32>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub off_rebounds: Option<i32>,
    pub def_rebounds: Option<i32>,
    pub total_rebounds: Option<i32>,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub turnovers: Option<i32>,
    pub fouls: Option<i32>,

    pub created_at: chrono::NaiveDateTime,
}

/// A single game on a team's schedule.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Schedule {
    pub id: Uuid,
    pub game_id: Uuid,
    pub team_id: Uuid,
    pub season: i32,
    pub game_date: NaiveDate,
    pub opponent_id: Option<Uuid>,
    pub is_home: Option<bool>,
    pub is_neutral: Option<bool>,
    pub team_score: Option<i32>,
    pub opponent_score: Option<i32>,
    pub created_at: chrono::NaiveDateTime,
}
