use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A game between two teams.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Game {
    pub id: Uuid,
    pub natstat_id: Option<String>,
    pub season: i32,
    pub game_date: NaiveDate,
    pub home_team_id: Option<Uuid>,
    pub away_team_id: Option<Uuid>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub is_neutral_site: bool,
    pub is_conference: Option<bool>,
    pub is_postseason: Option<bool>,
    pub venue: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}
