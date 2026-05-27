use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, types::JsonValue};
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Sort enums (prevent SQL injection by mapping to column names)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamSortField {
    #[default]
    AdjEfficiencyMargin,
    AdjOffense,
    AdjDefense,
    AdjTempo,
    Sos,
    Elo,
    Wins,
    PointDiff,
}

impl TeamSortField {
    pub fn column(&self) -> &'static str {
        match self {
            Self::AdjEfficiencyMargin => "tss.adj_efficiency_margin",
            Self::AdjOffense => "tss.adj_offense",
            Self::AdjDefense => "tss.adj_defense",
            Self::AdjTempo => "tss.adj_tempo",
            Self::Sos => "tss.sos",
            Self::Elo => "tss.elo_rating",
            Self::Wins => "tss.wins",
            Self::PointDiff => "tss.point_diff",
        }
    }

    /// Defense is lower-is-better; flip the default sort for it.
    pub fn default_desc(&self) -> bool {
        !matches!(self, Self::AdjDefense)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerSortField {
    #[default]
    Campom,
    Ppg,
    Rpg,
    Apg,
    Spg,
    Bpg,
    Topg,
    OffensiveRating,
    DefensiveRating,
    NetRating,
    MinutesPerGame,
    EffectiveFgPct,
    TrueShootingPct,
    UsageRate,
    GamesPlayed,
    AstPct,
    TovPct,
    OrbPct,
    DrbPct,
    StlPct,
    BlkPct,
    FtRate,
}

impl PlayerSortField {
    pub fn column(&self) -> &'static str {
        match self {
            Self::Campom => "tps.cam_gbpm_v3_psos",
            Self::Ppg => "pss.ppg",
            Self::Rpg => "pss.rpg",
            Self::Apg => "pss.apg",
            Self::Spg => "pss.spg",
            Self::Bpg => "pss.bpg",
            Self::Topg => "pss.topg",
            Self::OffensiveRating => "pss.offensive_rating",
            Self::DefensiveRating => "pss.defensive_rating",
            Self::NetRating => "pss.net_rating",
            Self::MinutesPerGame => "pss.minutes_per_game",
            Self::EffectiveFgPct => "pss.effective_fg_pct",
            Self::TrueShootingPct => "pss.true_shooting_pct",
            Self::UsageRate => "pss.usage_rate",
            Self::GamesPlayed => "pss.games_played",
            Self::AstPct => "pss.ast_pct",
            Self::TovPct => "pss.tov_pct",
            Self::OrbPct => "pss.orb_pct",
            Self::DrbPct => "pss.drb_pct",
            Self::StlPct => "pss.stl_pct",
            Self::BlkPct => "pss.blk_pct",
            Self::FtRate => "pss.ft_rate",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

impl SortOrder {
    pub fn sql(&self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
pub struct TeamRanking {
    pub rank: i64,
    pub team_id: Uuid,
    pub name: String,
    pub conference: Option<String>,
    pub wins: i32,
    pub losses: i32,
    pub adj_offense: Option<f64>,
    pub adj_offense_rank: Option<i64>,
    pub adj_defense: Option<f64>,
    pub adj_defense_rank: Option<i64>,
    pub adj_efficiency_margin: Option<f64>,
    pub adj_tempo: Option<f64>,
    pub adj_tempo_rank: Option<i64>,
    pub sos: Option<f64>,
    pub sos_rank: Option<i32>,
    pub elo_rating: Option<f64>,
    pub elo_rank: Option<i32>,
    pub point_diff: Option<f64>,
    pub pythag_win_pct: Option<f64>,
    pub road_win_pct: Option<f64>,
    // Four factors (offense)
    pub effective_fg_pct: Option<f64>,
    pub effective_fg_pct_rank: Option<i64>,
    pub turnover_pct: Option<f64>,
    pub turnover_pct_rank: Option<i64>,
    pub off_rebound_pct: Option<f64>,
    pub off_rebound_pct_rank: Option<i64>,
    pub ft_rate: Option<f64>,
    pub ft_rate_rank: Option<i64>,
    // Four factors (defense)
    pub opp_effective_fg_pct: Option<f64>,
    pub opp_effective_fg_pct_rank: Option<i64>,
    pub opp_turnover_pct: Option<f64>,
    pub opp_turnover_pct_rank: Option<i64>,
    pub def_rebound_pct: Option<f64>,
    pub def_rebound_pct_rank: Option<i64>,
    pub opp_ft_rate: Option<f64>,
    pub opp_ft_rate_rank: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct TeamProfile {
    pub id: Uuid,
    pub name: String,
    pub short_name: Option<String>,
    pub conference: Option<String>,
    pub division: Option<String>,
    pub season: i32,
    // Season stats
    pub wins: Option<i32>,
    pub losses: Option<i32>,
    pub adj_offense: Option<f64>,
    pub adj_offense_rank: Option<i64>,
    pub adj_defense: Option<f64>,
    pub adj_defense_rank: Option<i64>,
    pub adj_efficiency_margin: Option<f64>,
    pub adj_efficiency_margin_rank: Option<i64>,
    pub adj_tempo: Option<f64>,
    pub adj_tempo_rank: Option<i64>,
    pub sos: Option<f64>,
    pub sos_rank: Option<i32>,
    pub elo_rating: Option<f64>,
    pub elo_rank: Option<i32>,
    pub point_diff: Option<f64>,
    pub pythag_win_pct: Option<f64>,
    pub road_win_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub effective_fg_pct_rank: Option<i64>,
    pub turnover_pct: Option<f64>,
    pub turnover_pct_rank: Option<i64>,
    pub off_rebound_pct: Option<f64>,
    pub off_rebound_pct_rank: Option<i64>,
    pub ft_rate: Option<f64>,
    pub ft_rate_rank: Option<i64>,
    pub opp_effective_fg_pct: Option<f64>,
    pub opp_effective_fg_pct_rank: Option<i64>,
    pub opp_turnover_pct: Option<f64>,
    pub opp_turnover_pct_rank: Option<i64>,
    pub def_rebound_pct: Option<f64>,
    pub def_rebound_pct_rank: Option<i64>,
    pub opp_ft_rate: Option<f64>,
    pub opp_ft_rate_rank: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct ScheduleEntry {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub opponent_id: Option<Uuid>,
    pub opponent_name: Option<String>,
    pub is_home: Option<bool>,
    pub is_neutral: Option<bool>,
    pub team_score: Option<i32>,
    pub opponent_score: Option<i32>,
    pub is_conference: Option<bool>,
    pub is_postseason: Option<bool>,
    /// Predicted margin **from the requested team's perspective** (positive =
    /// requested team favored). Populated by the API layer for every game on
    /// the schedule — upcoming games get the model's pre-game forecast,
    /// completed games get a "what we'd predict today" projection (current
    /// team state, not pre-game; muted in the UI). Left null only when
    /// prediction inputs are missing (no opponent UUID resolved, feature
    /// extraction failed). Pre-game predictions for historical games are a
    /// future roadmap item (point-in-time `game_forecasts` backfill).
    #[sqlx(default)]
    pub projected_margin: Option<f64>,
    /// Probability the requested team wins, derived from `projected_margin`.
    #[sqlx(default)]
    pub projected_win_prob: Option<f64>,
    /// Projected score for the *requested team*. Integer; rounded so
    /// `projected_score_team + projected_score_opp == round(predicted_total)`.
    #[sqlx(default)]
    pub projected_score_team: Option<i32>,
    /// Projected score for the opponent.
    #[sqlx(default)]
    pub projected_score_opp: Option<i32>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct RosterEntry {
    pub player_id: Uuid,
    pub name: String,
    pub position: Option<String>,
    pub class_year: Option<String>,
    pub height_inches: Option<i32>,
    pub jersey_number: Option<String>,
    pub games_played: i32,
    pub minutes_per_game: Option<f64>,
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    pub fg_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    pub ft_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,
    pub usage_rate: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub gbpm: Option<f64>,
    pub campom: Option<f64>,
    pub campom_pct: Option<f64>,
    pub ppg_pct: Option<f64>,
    pub rpg_pct: Option<f64>,
    pub apg_pct: Option<f64>,
    pub spg_pct: Option<f64>,
    pub bpg_pct: Option<f64>,
    pub topg_pct: Option<f64>,
    pub true_shooting_pct_pct: Option<f64>,
    pub usage_rate_pct: Option<f64>,
    pub ast_pct_pct: Option<f64>,
    pub tov_pct_pct: Option<f64>,
    pub orb_pct_pct: Option<f64>,
    pub drb_pct_pct: Option<f64>,
    pub stl_pct_pct: Option<f64>,
    pub blk_pct_pct: Option<f64>,
    pub primary_class: Option<String>,
    pub secondary_class: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PlayerRow {
    pub player_id: Uuid,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub conference: Option<String>,
    pub position: Option<String>,
    pub class_year: Option<String>,
    pub season: i32,
    pub games_played: i32,
    pub minutes_per_game: Option<f64>,
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    pub fg_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    pub ft_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,
    pub usage_rate: Option<f64>,
    pub offensive_rating: Option<f64>,
    pub defensive_rating: Option<f64>,
    pub net_rating: Option<f64>,
    pub player_sos: Option<f64>,
    pub campom: Option<f64>,
    pub campom_pct: Option<f64>,
    // Rate stats — surfaced on the Players tab Rate view.
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    // Percentiles — drive the red→green gradient on each stat cell.
    pub ppg_pct: Option<f64>,
    pub rpg_pct: Option<f64>,
    pub apg_pct: Option<f64>,
    pub spg_pct: Option<f64>,
    pub bpg_pct: Option<f64>,
    pub topg_pct: Option<f64>,
    pub mpg_pct: Option<f64>,
    pub usage_rate_pct: Option<f64>,
    pub true_shooting_pct_pct: Option<f64>,
    pub ast_pct_pct: Option<f64>,
    pub tov_pct_pct: Option<f64>,
    pub orb_pct_pct: Option<f64>,
    pub drb_pct_pct: Option<f64>,
    pub stl_pct_pct: Option<f64>,
    pub blk_pct_pct: Option<f64>,
    // Archetype — surfaced when the page filters by class.
    pub primary_class: Option<String>,
    pub secondary_class: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PlayerProfile {
    pub id: Uuid,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub conference: Option<String>,
    pub position: Option<String>,
    pub class_year: Option<String>,
    pub height_inches: Option<i32>,
    pub weight_lbs: Option<i32>,
    pub jersey_number: Option<String>,
    pub season: i32,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PlayerSeasonStatsRow {
    pub games_played: i32,
    pub games_started: Option<i32>,
    pub minutes_per_game: Option<f64>,
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    pub fg_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    pub ft_pct: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,
    pub offensive_rating: Option<f64>,
    pub defensive_rating: Option<f64>,
    pub net_rating: Option<f64>,
    pub usage_rate: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    pub player_sos: Option<f64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PercentilesRow {
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
    pub player_sos_pct: Option<f64>,
    pub ast_pct_pct: Option<f64>,
    pub tov_pct_pct: Option<f64>,
    pub mpg_pct: Option<f64>,
    pub topg_pct: Option<f64>,
    pub orb_pct_pct: Option<f64>,
    pub drb_pct_pct: Option<f64>,
    pub stl_pct_pct: Option<f64>,
    pub blk_pct_pct: Option<f64>,
    pub ft_rate_pct: Option<f64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct GameLogEntry {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub opponent_id: Option<Uuid>,
    pub opponent_name: Option<String>,
    pub is_home: Option<bool>,
    pub minutes: Option<f64>,
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
    pub total_rebounds: Option<i32>,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub turnovers: Option<i32>,
    pub game_score: Option<f64>,
    pub rolling_ppg: Option<f64>,
    pub rolling_game_score: Option<f64>,
    pub rolling_ts_pct: Option<f64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct GameResult {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub season: i32,
    pub home_team_id: Option<Uuid>,
    pub home_team_name: Option<String>,
    pub away_team_id: Option<Uuid>,
    pub away_team_name: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub is_neutral_site: bool,
    pub is_conference: Option<bool>,
    pub is_postseason: Option<bool>,
}

// ---------------------------------------------------------------------------
// Team queries
// ---------------------------------------------------------------------------

pub async fn get_team_rankings(
    pool: &PgPool,
    season: i32,
    sort: TeamSortField,
    order: Option<SortOrder>,
) -> Result<Vec<TeamRanking>, sqlx::Error> {
    let order = order.unwrap_or_else(|| {
        if sort.default_desc() {
            SortOrder::Desc
        } else {
            SortOrder::Asc
        }
    });

    let query = format!(
        r#"
        SELECT
            ROW_NUMBER() OVER (ORDER BY tss.adj_efficiency_margin DESC NULLS LAST) AS rank,
            t.id AS team_id,
            COALESCE(t.short_name, t.name) AS name,
            t.conference,
            tss.wins,
            tss.losses,
            tss.adj_offense,
            RANK() OVER (ORDER BY tss.adj_offense DESC NULLS LAST) AS adj_offense_rank,
            tss.adj_defense,
            RANK() OVER (ORDER BY tss.adj_defense ASC NULLS LAST) AS adj_defense_rank,
            tss.adj_efficiency_margin,
            tss.adj_tempo,
            RANK() OVER (ORDER BY tss.adj_tempo DESC NULLS LAST) AS adj_tempo_rank,
            tss.sos,
            tss.sos_rank,
            tss.elo_rating,
            tss.elo_rank,
            tss.point_diff,
            tss.pythag_win_pct,
            tss.road_win_pct,
            tss.effective_fg_pct,
            RANK() OVER (ORDER BY tss.effective_fg_pct DESC NULLS LAST) AS effective_fg_pct_rank,
            tss.turnover_pct,
            RANK() OVER (ORDER BY tss.turnover_pct ASC NULLS LAST) AS turnover_pct_rank,
            tss.off_rebound_pct,
            RANK() OVER (ORDER BY tss.off_rebound_pct DESC NULLS LAST) AS off_rebound_pct_rank,
            tss.ft_rate,
            RANK() OVER (ORDER BY tss.ft_rate DESC NULLS LAST) AS ft_rate_rank,
            tss.opp_effective_fg_pct,
            RANK() OVER (ORDER BY tss.opp_effective_fg_pct ASC NULLS LAST) AS opp_effective_fg_pct_rank,
            tss.opp_turnover_pct,
            RANK() OVER (ORDER BY tss.opp_turnover_pct DESC NULLS LAST) AS opp_turnover_pct_rank,
            tss.def_rebound_pct,
            RANK() OVER (ORDER BY tss.def_rebound_pct DESC NULLS LAST) AS def_rebound_pct_rank,
            tss.opp_ft_rate,
            RANK() OVER (ORDER BY tss.opp_ft_rate ASC NULLS LAST) AS opp_ft_rate_rank
        FROM teams t
        JOIN team_season_stats tss ON tss.team_id = t.id AND tss.season = t.season
        WHERE t.season = $1
          AND tss.adj_efficiency_margin IS NOT NULL
        ORDER BY {} {} NULLS LAST
        "#,
        sort.column(),
        order.sql(),
    );

    sqlx::query_as::<_, TeamRanking>(&query)
        .bind(season)
        .fetch_all(pool)
        .await
}

/// Map a season-scoped team UUID to the equivalent UUID for `season`, joining
/// on the cross-season `natstat_id`. When `team_id` already belongs to
/// `season` the join finds itself, so this is a safe no-op for the matching
/// case. Returns `None` if `team_id` doesn't exist or no team carries the
/// same `natstat_id` in the requested season.
pub async fn resolve_team_id_for_season(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT t2.id
        FROM teams t1
        JOIN teams t2 ON t2.natstat_id = t1.natstat_id AND t2.season = $2
        WHERE t1.id = $1
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Map a season-scoped player UUID to the equivalent UUID for `season`.
/// Tries the cross-season `natstat_id` first (the common case) and falls
/// back to `torvik_pid` to handle transfers — NatStat issues a fresh
/// `natstat_id` per (player, team), so without this fallback a season swap
/// from a transfer's destination back to their prior school 404s.
pub async fn resolve_player_id_for_season(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<Uuid>, sqlx::Error> {
    // 1. natstat_id match (handles the 90% non-transfer case and is faster).
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT p2.id
        FROM players p1
        JOIN players p2 ON p2.natstat_id = p1.natstat_id AND p2.season = $2
        WHERE p1.id = $1
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await?;
    if let Some((id,)) = row {
        return Ok(Some(id));
    }

    // 2. torvik_pid fallback (transfers — same human, different natstat_id).
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT t2.player_id
        FROM torvik_player_stats t1
        JOIN torvik_player_stats t2
          ON t2.torvik_pid = t1.torvik_pid AND t2.season = $2
        WHERE t1.player_id = $1
          AND t2.player_id IS NOT NULL
        LIMIT 1
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Seasons in which this player has any row in `players`, joined across
/// teams. Used by the detail page to constrain the season selector.
///
/// NatStat assigns a fresh `natstat_id` when a player changes teams, so a
/// pure `natstat_id` join breaks for transfers (Lendeborg is `58189293` at
/// UAB and `87905686` at Michigan). Torvik's `torvik_pid` is the stable
/// cross-season+team identifier — same number for the same human regardless
/// of school — so we UNION:
///   1. all (natstat_id) seasons (covers the non-transfer 90%)
///   2. all seasons sharing the player's `torvik_pid` (covers transfers and
///      links the prior-team row to the new-team row)
///
/// 96% of player rows have a torvik link; the 4% without fall back to the
/// natstat_id-only branch, which is the same behaviour as before — no
/// regression for those edge cases.
pub async fn get_player_available_seasons(
    pool: &PgPool,
    player_id: Uuid,
) -> Result<Vec<i32>, sqlx::Error> {
    let rows: Vec<(i32,)> = sqlx::query_as(
        r#"
        WITH this AS (
            SELECT
                p.natstat_id AS nat_id,
                (
                    SELECT t.torvik_pid
                    FROM torvik_player_stats t
                    WHERE t.player_id = p.id
                    LIMIT 1
                ) AS tor_pid
            FROM players p
            WHERE p.id = $1
        )
        SELECT DISTINCT season FROM (
            SELECT p.season
            FROM players p, this
            WHERE p.natstat_id = this.nat_id
              AND EXISTS (SELECT 1 FROM player_game_stats pgs WHERE pgs.player_id = p.id)
            UNION
            SELECT t.season
            FROM torvik_player_stats t, this
            WHERE this.tor_pid IS NOT NULL
              AND t.torvik_pid = this.tor_pid
        ) x
        ORDER BY season DESC
        "#,
    )
    .bind(player_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// Team analogue of `get_player_available_seasons`.
///
/// Gates on `EXISTS (team_game_stats)` so reclassifying programs (D2↔D1 like
/// Le Moyne, D1↔D3 like Hartford) only surface seasons where actual games
/// were played. Ghost `teams` rows created by enrichment paths in non-D1
/// seasons are filtered out — they have zero `team_game_stats` rows.
pub async fn get_team_available_seasons(
    pool: &PgPool,
    team_id: Uuid,
) -> Result<Vec<i32>, sqlx::Error> {
    let rows: Vec<(i32,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT t.season
        FROM teams t
        WHERE t.natstat_id = (SELECT natstat_id FROM teams WHERE id = $1)
          AND EXISTS (SELECT 1 FROM team_game_stats tgs WHERE tgs.team_id = t.id)
        ORDER BY t.season DESC
        "#,
    )
    .bind(team_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

pub async fn get_team_by_id(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Option<TeamProfile>, sqlx::Error> {
    sqlx::query_as::<_, TeamProfile>(
        r#"
        WITH ranked AS (
            SELECT
                tss.team_id,
                tss.wins, tss.losses,
                tss.adj_offense,
                RANK() OVER (ORDER BY tss.adj_offense DESC NULLS LAST) AS adj_offense_rank,
                tss.adj_defense,
                RANK() OVER (ORDER BY tss.adj_defense ASC NULLS LAST) AS adj_defense_rank,
                tss.adj_efficiency_margin,
                RANK() OVER (ORDER BY tss.adj_efficiency_margin DESC NULLS LAST) AS adj_efficiency_margin_rank,
                tss.adj_tempo,
                RANK() OVER (ORDER BY tss.adj_tempo DESC NULLS LAST) AS adj_tempo_rank,
                tss.sos, tss.sos_rank, tss.elo_rating, tss.elo_rank,
                tss.point_diff, tss.pythag_win_pct, tss.road_win_pct,
                tss.effective_fg_pct,
                RANK() OVER (ORDER BY tss.effective_fg_pct DESC NULLS LAST) AS effective_fg_pct_rank,
                tss.turnover_pct,
                RANK() OVER (ORDER BY tss.turnover_pct ASC NULLS LAST) AS turnover_pct_rank,
                tss.off_rebound_pct,
                RANK() OVER (ORDER BY tss.off_rebound_pct DESC NULLS LAST) AS off_rebound_pct_rank,
                tss.ft_rate,
                RANK() OVER (ORDER BY tss.ft_rate DESC NULLS LAST) AS ft_rate_rank,
                tss.opp_effective_fg_pct,
                RANK() OVER (ORDER BY tss.opp_effective_fg_pct ASC NULLS LAST) AS opp_effective_fg_pct_rank,
                tss.opp_turnover_pct,
                RANK() OVER (ORDER BY tss.opp_turnover_pct DESC NULLS LAST) AS opp_turnover_pct_rank,
                tss.def_rebound_pct,
                RANK() OVER (ORDER BY tss.def_rebound_pct DESC NULLS LAST) AS def_rebound_pct_rank,
                tss.opp_ft_rate,
                RANK() OVER (ORDER BY tss.opp_ft_rate ASC NULLS LAST) AS opp_ft_rate_rank
            FROM team_season_stats tss
            WHERE tss.season = $2
        )
        SELECT
            t.id, COALESCE(t.short_name, t.name) AS name, t.short_name, t.conference, t.division, t.season,
            r.wins, r.losses,
            r.adj_offense, r.adj_offense_rank,
            r.adj_defense, r.adj_defense_rank,
            r.adj_efficiency_margin, r.adj_efficiency_margin_rank,
            r.adj_tempo, r.adj_tempo_rank,
            r.sos, r.sos_rank, r.elo_rating, r.elo_rank,
            r.point_diff, r.pythag_win_pct, r.road_win_pct,
            r.effective_fg_pct, r.effective_fg_pct_rank,
            r.turnover_pct, r.turnover_pct_rank,
            r.off_rebound_pct, r.off_rebound_pct_rank,
            r.ft_rate, r.ft_rate_rank,
            r.opp_effective_fg_pct, r.opp_effective_fg_pct_rank,
            r.opp_turnover_pct, r.opp_turnover_pct_rank,
            r.def_rebound_pct, r.def_rebound_pct_rank,
            r.opp_ft_rate, r.opp_ft_rate_rank
        FROM teams t
        LEFT JOIN ranked r ON r.team_id = t.id
        WHERE t.id = $1 AND t.season = $2
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

pub async fn get_team_schedule(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Vec<ScheduleEntry>, sqlx::Error> {
    sqlx::query_as::<_, ScheduleEntry>(
        r#"
        SELECT
            s.game_id,
            s.game_date,
            s.opponent_id,
            COALESCE(opp.short_name, opp.name) AS opponent_name,
            s.is_home,
            s.is_neutral,
            s.team_score,
            s.opponent_score,
            g.is_conference,
            g.is_postseason
        FROM schedules s
        LEFT JOIN teams opp ON opp.id = s.opponent_id AND opp.season = s.season
        LEFT JOIN games g ON g.id = s.game_id
        WHERE s.team_id = $1 AND s.season = $2
        ORDER BY s.game_date
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_all(pool)
    .await
}

pub async fn get_team_roster(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Vec<RosterEntry>, sqlx::Error> {
    sqlx::query_as::<_, RosterEntry>(
        r#"
        SELECT
            p.id AS player_id,
            p.name,
            p.position,
            p.class_year,
            p.height_inches,
            p.jersey_number,
            pss.games_played,
            pss.minutes_per_game,
            pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
            pss.fg_pct, pss.tp_pct, pss.ft_pct,
            pss.effective_fg_pct, pss.true_shooting_pct,
            pss.usage_rate,
            pss.ast_pct, pss.tov_pct,
            pss.orb_pct, pss.drb_pct, pss.stl_pct, pss.blk_pct,
            tps.gbpm,
            tps.cam_gbpm_v3_psos     AS campom,
            tps.cam_gbpm_v3_psos_pct AS campom_pct,
            pp.ppg_pct, pp.rpg_pct, pp.apg_pct, pp.spg_pct, pp.bpg_pct, pp.topg_pct,
            pp.true_shooting_pct_pct,
            pp.usage_rate_pct,
            pp.ast_pct_pct, pp.tov_pct_pct,
            pp.orb_pct_pct, pp.drb_pct_pct, pp.stl_pct_pct, pp.blk_pct_pct,
            pa.primary_class, pa.secondary_class
        FROM players p
        JOIN player_season_stats pss ON pss.player_id = p.id AND pss.team_id = p.team_id AND pss.season = p.season
        LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = p.season
        LEFT JOIN player_percentiles pp ON pp.player_id = p.id AND pp.season = p.season
        LEFT JOIN player_archetypes pa ON pa.player_id = p.id AND pa.season = p.season
        WHERE p.team_id = $1 AND p.season = $2
        ORDER BY tps.cam_gbpm_v3_psos DESC NULLS LAST, pss.minutes_per_game DESC NULLS LAST
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// Player queries
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn search_players(
    pool: &PgPool,
    search: Option<&str>,
    team_id: Option<Uuid>,
    season: i32,
    sort: PlayerSortField,
    order: Option<SortOrder>,
    archetype: Option<&str>,
    include_secondary_archetype: bool,
    limit: i64,
    offset: i64,
) -> Result<(Vec<PlayerRow>, i64), sqlx::Error> {
    let order = order.unwrap_or(SortOrder::Desc);
    let search_pattern = search.map(|s| format!("%{s}%"));

    // Archetype filter: $4 holds the class name (or NULL); $5 toggles whether
    // a player matches via secondary_class as well as primary_class.
    let archetype_param = archetype.map(str::to_string);

    // Count query
    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        WHERE pss.season = $1
          AND pss.games_played >= 5
          AND pss.minutes_per_game >= 10
          AND ($2::uuid IS NULL OR pss.team_id = $2)
          AND ($3::text IS NULL OR p.name ILIKE $3)
          AND (
              $4::text IS NULL
              OR pa.primary_class = $4
              OR ($5::bool AND pa.secondary_class = $4)
          )
        "#,
    )
    .bind(season)
    .bind(team_id)
    .bind(&search_pattern)
    .bind(&archetype_param)
    .bind(include_secondary_archetype)
    .fetch_one(pool)
    .await?;

    let query = format!(
        r#"
        SELECT
            p.id AS player_id,
            p.name,
            p.team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            t.conference,
            p.position,
            p.class_year,
            pss.season,
            pss.games_played,
            pss.minutes_per_game,
            pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
            pss.fg_pct, pss.tp_pct, pss.ft_pct,
            pss.effective_fg_pct, pss.true_shooting_pct,
            pss.usage_rate,
            pss.offensive_rating, pss.defensive_rating, pss.net_rating,
            pss.player_sos,
            tps.cam_gbpm_v3_psos     AS campom,
            tps.cam_gbpm_v3_psos_pct AS campom_pct,
            pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
            pss.stl_pct, pss.blk_pct, pss.ft_rate,
            pp.ppg_pct, pp.rpg_pct, pp.apg_pct, pp.spg_pct, pp.bpg_pct, pp.topg_pct,
            pp.mpg_pct, pp.usage_rate_pct, pp.true_shooting_pct_pct,
            pp.ast_pct_pct, pp.tov_pct_pct, pp.orb_pct_pct, pp.drb_pct_pct,
            pp.stl_pct_pct, pp.blk_pct_pct,
            pa.primary_class, pa.secondary_class
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
        LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = pss.season
        LEFT JOIN player_percentiles pp ON pp.player_id = pss.player_id AND pp.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        WHERE pss.season = $1
          AND pss.games_played >= 5
          AND pss.minutes_per_game >= 10
          AND ($2::uuid IS NULL OR pss.team_id = $2)
          AND ($3::text IS NULL OR p.name ILIKE $3)
          AND (
              $4::text IS NULL
              OR pa.primary_class = $4
              OR ($5::bool AND pa.secondary_class = $4)
          )
        ORDER BY {} {} NULLS LAST
        LIMIT $6 OFFSET $7
        "#,
        sort.column(),
        order.sql(),
    );

    let rows = sqlx::query_as::<_, PlayerRow>(&query)
        .bind(season)
        .bind(team_id)
        .bind(&search_pattern)
        .bind(&archetype_param)
        .bind(include_secondary_archetype)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    Ok((rows, total))
}

pub async fn get_player_by_id(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PlayerProfile>, sqlx::Error> {
    sqlx::query_as::<_, PlayerProfile>(
        r#"
        SELECT
            p.id, p.name, p.team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            t.conference,
            p.position, p.class_year,
            p.height_inches, p.weight_lbs, p.jersey_number,
            p.season
        FROM players p
        LEFT JOIN teams t ON t.id = p.team_id AND t.season = p.season
        WHERE p.id = $1 AND p.season = $2
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

pub async fn get_player_season_stats(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PlayerSeasonStatsRow>, sqlx::Error> {
    sqlx::query_as::<_, PlayerSeasonStatsRow>(
        r#"
        SELECT
            games_played, games_started, minutes_per_game,
            ppg, rpg, apg, spg, bpg, topg,
            fg_pct, tp_pct, ft_pct,
            effective_fg_pct, true_shooting_pct,
            offensive_rating, defensive_rating, net_rating,
            usage_rate,
            ast_pct, tov_pct, orb_pct, drb_pct, stl_pct, blk_pct,
            ft_rate, player_sos
        FROM player_season_stats
        WHERE player_id = $1 AND season = $2
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

pub async fn get_player_percentiles(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PercentilesRow>, sqlx::Error> {
    sqlx::query_as::<_, PercentilesRow>(
        r#"
        SELECT
            ppg_pct, rpg_pct, apg_pct, spg_pct, bpg_pct,
            fg_pct_pct, tp_pct_pct, ft_pct_pct,
            effective_fg_pct_pct, true_shooting_pct_pct,
            usage_rate_pct, offensive_rating_pct, defensive_rating_pct,
            player_sos_pct,
            ast_pct_pct, tov_pct_pct, mpg_pct, topg_pct,
            orb_pct_pct, drb_pct_pct, stl_pct_pct, blk_pct_pct, ft_rate_pct
        FROM player_percentiles
        WHERE player_id = $1 AND season = $2
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

pub async fn get_player_game_log(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Vec<GameLogEntry>, sqlx::Error> {
    sqlx::query_as::<_, GameLogEntry>(
        r#"
        SELECT
            pgs.game_id,
            pgs.game_date,
            pgs.opponent_id,
            COALESCE(opp.short_name, opp.name) AS opponent_name,
            pgs.is_home,
            pgs.minutes,
            pgs.points, pgs.fgm, pgs.fga, pgs.fg_pct,
            pgs.tpm, pgs.tpa, pgs.tp_pct,
            pgs.ftm, pgs.fta, pgs.ft_pct,
            pgs.total_rebounds, pgs.assists, pgs.steals, pgs.blocks, pgs.turnovers,
            pgs.game_score,
            pgs.rolling_ppg, pgs.rolling_game_score, pgs.rolling_ts_pct
        FROM player_game_stats pgs
        LEFT JOIN teams opp ON opp.id = pgs.opponent_id AND opp.season = pgs.season
        WHERE pgs.player_id = $1 AND pgs.season = $2
        ORDER BY pgs.game_date
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// League averages
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
pub struct LeagueAverages {
    pub avg_ppg: Option<f64>,
    pub avg_game_score: Option<f64>,
}

pub async fn get_league_averages(
    pool: &PgPool,
    season: i32,
) -> Result<LeagueAverages, sqlx::Error> {
    sqlx::query_as::<_, LeagueAverages>(
        r#"
        SELECT
            (SELECT AVG(ppg) FROM player_season_stats
             WHERE season = $1 AND games_played >= 10 AND minutes_per_game >= 10) AS avg_ppg,
            (SELECT AVG(game_score) FROM player_game_stats pgs
             JOIN player_season_stats pss ON pss.player_id = pgs.player_id AND pss.season = pgs.season
             WHERE pgs.season = $1 AND pss.games_played >= 10 AND pss.minutes_per_game >= 10) AS avg_game_score
        "#,
    )
    .bind(season)
    .fetch_one(pool)
    .await
}

// ---------------------------------------------------------------------------
// Torvik advanced stats
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
pub struct TorkvikStatsRow {
    // Impact metrics
    pub gbpm: Option<f64>,
    pub ogbpm: Option<f64>,
    pub dgbpm: Option<f64>,
    pub stops: Option<f64>,
    // Efficiency
    pub adj_oe: Option<f64>,
    pub adj_de: Option<f64>,
    // Shot zones
    pub rim_pct: Option<f64>,
    pub rim_made: Option<f64>,
    pub rim_attempted: Option<f64>,
    pub mid_pct: Option<f64>,
    pub mid_made: Option<f64>,
    pub mid_attempted: Option<f64>,
    pub dunk_pct: Option<f64>,
    pub dunks_made: Option<f64>,
    pub dunks_attempted: Option<f64>,
    pub two_p_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    // Rates (possession-based)
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    pub personal_foul_rate: Option<f64>,
    // Shooting volume
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub two_pm: Option<i32>,
    pub two_pa: Option<i32>,
    // Context
    pub recruiting_rank: Option<f64>,
    pub hometown: Option<String>,
    // CamPom (canonical site-wide composite)
    pub campom: Option<f64>,
    pub campom_pct: Option<f64>,
    // Percentiles (computed on-the-fly)
    pub gbpm_pct: Option<f64>,
    pub ogbpm_pct: Option<f64>,
    pub dgbpm_pct: Option<f64>,
    pub adj_oe_pct: Option<f64>,
    pub adj_de_pct: Option<f64>,
    pub orb_pct_pct: Option<f64>,
    pub drb_pct_pct: Option<f64>,
    pub stl_pct_pct: Option<f64>,
    pub blk_pct_pct: Option<f64>,
    pub ft_rate_pct: Option<f64>,
    pub fc_rate_pct: Option<f64>,
    // Shot zone percentiles
    pub rim_pct_pct: Option<f64>,
    pub mid_pct_pct: Option<f64>,
    pub dunk_pct_pct: Option<f64>,
    pub tp_pct_pct: Option<f64>,
}

pub async fn get_torvik_stats(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<TorkvikStatsRow>, sqlx::Error> {
    sqlx::query_as::<_, TorkvikStatsRow>(
        r#"
        WITH ranked AS (
            SELECT *,
                PERCENT_RANK() OVER (ORDER BY gbpm)    AS gbpm_pct,
                PERCENT_RANK() OVER (ORDER BY ogbpm)   AS ogbpm_pct,
                PERCENT_RANK() OVER (ORDER BY dgbpm)   AS dgbpm_pct,
                PERCENT_RANK() OVER (ORDER BY adj_oe)  AS adj_oe_pct,
                PERCENT_RANK() OVER (ORDER BY adj_de DESC) AS adj_de_pct,
                PERCENT_RANK() OVER (ORDER BY orb_pct) AS orb_pct_pct,
                PERCENT_RANK() OVER (ORDER BY drb_pct) AS drb_pct_pct,
                PERCENT_RANK() OVER (ORDER BY stl_pct) AS stl_pct_pct,
                PERCENT_RANK() OVER (ORDER BY blk_pct) AS blk_pct_pct,
                PERCENT_RANK() OVER (ORDER BY ft_rate) AS ft_rate_pct,
                PERCENT_RANK() OVER (ORDER BY personal_foul_rate DESC) AS fc_rate_pct,
                CASE WHEN rim_attempted > 0 THEN PERCENT_RANK() OVER (
                    PARTITION BY CASE WHEN rim_attempted > 0 THEN 1 ELSE 0 END ORDER BY rim_pct
                ) END AS rim_pct_pct,
                CASE WHEN mid_attempted > 0 THEN PERCENT_RANK() OVER (
                    PARTITION BY CASE WHEN mid_attempted > 0 THEN 1 ELSE 0 END ORDER BY mid_pct
                ) END AS mid_pct_pct,
                CASE WHEN dunks_attempted > 0 THEN PERCENT_RANK() OVER (
                    PARTITION BY CASE WHEN dunks_attempted > 0 THEN 1 ELSE 0 END ORDER BY dunk_pct
                ) END AS dunk_pct_pct,
                CASE WHEN tpa > 0 THEN PERCENT_RANK() OVER (
                    PARTITION BY CASE WHEN tpa > 0 THEN 1 ELSE 0 END ORDER BY tp_pct
                ) END AS tp_pct_pct
            FROM torvik_player_stats
            WHERE season = $2
              AND games_played >= 10
              AND minutes_per_game >= 10
        )
        SELECT gbpm, ogbpm, dgbpm, stops,
               adj_oe, adj_de,
               rim_pct, rim_made, rim_attempted,
               mid_pct, mid_made, mid_attempted,
               dunk_pct, dunks_made, dunks_attempted,
               two_p_pct, tp_pct, tpm, tpa,
               orb_pct, drb_pct, stl_pct, blk_pct,
               ft_rate, personal_foul_rate,
               ftm, fta, two_pm, two_pa,
               recruiting_rank, player_type AS hometown,
               cam_gbpm_v3_psos     AS campom,
               cam_gbpm_v3_psos_pct AS campom_pct,
               gbpm_pct, ogbpm_pct, dgbpm_pct,
               adj_oe_pct, adj_de_pct,
               orb_pct_pct, drb_pct_pct, stl_pct_pct, blk_pct_pct,
               ft_rate_pct, fc_rate_pct,
               rim_pct_pct, mid_pct_pct, dunk_pct_pct, tp_pct_pct
        FROM ranked
        WHERE player_id = $1
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

// ---------------------------------------------------------------------------
// Game queries
// ---------------------------------------------------------------------------

/// Most-recent completed games for the score ticker. Ordered date DESC.
pub async fn get_recent_games(
    pool: &PgPool,
    season: i32,
    limit: i64,
) -> Result<Vec<GameResult>, sqlx::Error> {
    sqlx::query_as::<_, GameResult>(
        r#"
        SELECT
            g.id AS game_id,
            g.game_date,
            g.season,
            g.home_team_id,
            COALESCE(ht.short_name, ht.name) AS home_team_name,
            g.away_team_id,
            COALESCE(at.short_name, at.name) AS away_team_name,
            g.home_score,
            g.away_score,
            g.is_neutral_site,
            g.is_conference,
            g.is_postseason
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id AND ht.season = g.season
        LEFT JOIN teams at ON at.id = g.away_team_id AND at.season = g.season
        WHERE g.season = $1 AND g.home_score IS NOT NULL
        ORDER BY g.game_date DESC, g.id
        LIMIT $2
        "#,
    )
    .bind(season)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Soonest upcoming games (scheduled, not yet played) for the score ticker.
/// Returns rows when `games` has unplayed entries dated today or later — in
/// the offseason this is empty until next season's schedule is ingested.
pub async fn get_upcoming_games(
    pool: &PgPool,
    season: i32,
    limit: i64,
) -> Result<Vec<GameResult>, sqlx::Error> {
    sqlx::query_as::<_, GameResult>(
        r#"
        SELECT
            g.id AS game_id,
            g.game_date,
            g.season,
            g.home_team_id,
            COALESCE(ht.short_name, ht.name) AS home_team_name,
            g.away_team_id,
            COALESCE(at.short_name, at.name) AS away_team_name,
            g.home_score,
            g.away_score,
            g.is_neutral_site,
            g.is_conference,
            g.is_postseason
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id AND ht.season = g.season
        LEFT JOIN teams at ON at.id = g.away_team_id AND at.season = g.season
        WHERE g.season = $1
          AND g.home_score IS NULL
          AND g.game_date >= CURRENT_DATE
        ORDER BY g.game_date ASC, g.id
        LIMIT $2
        "#,
    )
    .bind(season)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn get_games(
    pool: &PgPool,
    date: Option<NaiveDate>,
    team_id: Option<Uuid>,
    season: i32,
    limit: i64,
    offset: i64,
) -> Result<Vec<GameResult>, sqlx::Error> {
    sqlx::query_as::<_, GameResult>(
        r#"
        SELECT
            g.id AS game_id,
            g.game_date,
            g.season,
            g.home_team_id,
            COALESCE(ht.short_name, ht.name) AS home_team_name,
            g.away_team_id,
            COALESCE(at.short_name, at.name) AS away_team_name,
            g.home_score,
            g.away_score,
            g.is_neutral_site,
            g.is_conference,
            g.is_postseason
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id AND ht.season = g.season
        LEFT JOIN teams at ON at.id = g.away_team_id AND at.season = g.season
        WHERE g.season = $1
          AND g.home_score IS NOT NULL
          AND ($2::date IS NULL OR g.game_date = $2)
          AND ($3::uuid IS NULL OR g.home_team_id = $3 OR g.away_team_id = $3)
        ORDER BY g.game_date DESC, g.id
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(season)
    .bind(date)
    .bind(team_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// Player archetypes (Phase 5a)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
pub struct PlayerArchetypeRow {
    pub primary_class: String,
    pub secondary_class: Option<String>,
    pub primary_score: f64,
    pub secondary_score: Option<f64>,
    pub affinity_scores: JsonValue,
    pub cluster_id: i32,
}

pub async fn get_player_archetype(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PlayerArchetypeRow>, sqlx::Error> {
    sqlx::query_as::<_, PlayerArchetypeRow>(
        r#"
        SELECT primary_class, secondary_class, primary_score, secondary_score,
               affinity_scores, cluster_id
        FROM player_archetypes
        WHERE player_id = $1 AND season = $2
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Serialize, FromRow)]
pub struct SimilarPlayerRow {
    pub player_id: Uuid,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub primary_class: String,
    pub secondary_class: Option<String>,
    /// Euclidean distance in standardized feature space (0 = identical).
    pub distance: f64,
    /// Convenience: 1 / (1 + distance) — 1.0 is identical, decays smoothly.
    pub similarity: f64,
}

pub async fn get_similar_players(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
    limit: i64,
) -> Result<Vec<SimilarPlayerRow>, sqlx::Error> {
    sqlx::query_as::<_, SimilarPlayerRow>(
        r#"
        WITH target AS (
            SELECT feature_vector AS fv
            FROM player_archetypes
            WHERE player_id = $1 AND season = $2
        ),
        candidates AS (
            SELECT
                pa.player_id,
                pa.primary_class,
                pa.secondary_class,
                sqrt(SUM(POWER(pa_v::double precision - tg_v::double precision, 2))) AS distance
            FROM player_archetypes pa
            CROSS JOIN target
            CROSS JOIN LATERAL unnest(pa.feature_vector, target.fv) AS u(pa_v, tg_v)
            WHERE pa.season = $2 AND pa.player_id <> $1
            GROUP BY pa.player_id, pa.primary_class, pa.secondary_class
        )
        SELECT
            c.player_id,
            p.name,
            p.team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            c.primary_class,
            c.secondary_class,
            c.distance,
            (1.0 / (1.0 + c.distance)) AS similarity
        FROM candidates c
        JOIN players p ON p.id = c.player_id
        LEFT JOIN teams t ON t.id = p.team_id AND t.season = $2
        ORDER BY c.distance ASC
        LIMIT $3
        "#,
    )
    .bind(player_id)
    .bind(season)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Serialize, FromRow)]
pub struct ArchetypeCount {
    pub primary_class: String,
    pub count: i64,
    /// Sum of (minutes_per_game × games_played) across class members, scoped
    /// to the team / season being queried. Used by the team page to weight
    /// the distribution by who actually plays vs. who's on the bench.
    /// May be NULL on queries that don't compute it (e.g. season-wide counts).
    pub total_minutes: Option<f64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct ArchetypeExemplar {
    pub primary_class: String,
    pub player_id: Uuid,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub primary_score: f64,
}

pub async fn get_archetype_exemplars(
    pool: &PgPool,
    season: i32,
    per_class: i64,
) -> Result<Vec<ArchetypeExemplar>, sqlx::Error> {
    // Rank within each class by CamPom — the site-wide canonical player
    // valuation. Surfaces the highest-impact (and most recognizable)
    // representatives, with the cluster `primary_score` as a tiebreaker so
    // ties resolve toward the purest cluster fit. Going by raw fit-purity
    // alone surfaced obscure role players.
    //
    // Torvik can have multiple rows per (player_id, season) for transfer
    // players (different torvik_pid per stint), so we pre-aggregate to one
    // row per player before joining — otherwise ROW_NUMBER counts the
    // duplicates and we end up with the same name twice in a class.
    sqlx::query_as::<_, ArchetypeExemplar>(
        r#"
        WITH torvik_dedup AS (
            SELECT player_id, MAX(cam_gbpm_v3_psos) AS campom
            FROM torvik_player_stats
            WHERE season = $1 AND player_id IS NOT NULL
            GROUP BY player_id
        ),
        ranked AS (
            SELECT
                pa.primary_class,
                pa.primary_score,
                p.id AS player_id,
                p.name,
                p.team_id,
                COALESCE(t.short_name, t.name) AS team_name,
                ROW_NUMBER() OVER (
                    PARTITION BY pa.primary_class
                    ORDER BY tps.campom DESC NULLS LAST, pa.primary_score DESC
                ) AS rn
            FROM player_archetypes pa
            JOIN players p ON p.id = pa.player_id
            LEFT JOIN teams t ON t.id = p.team_id AND t.season = pa.season
            LEFT JOIN torvik_dedup tps ON tps.player_id = p.id
            WHERE pa.season = $1
        )
        SELECT primary_class, player_id, name, team_id, team_name, primary_score
        FROM ranked
        WHERE rn <= $2
        ORDER BY primary_class, rn
        "#,
    )
    .bind(season)
    .bind(per_class)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Serialize, FromRow)]
pub struct ArchetypeClassSummary {
    pub primary_class: String,
    pub count: i64,
    /// Mean CamPom across cluster members. Used for ordering archetypes from
    /// most to least impactful on the glossary page.
    pub mean_campom: Option<f64>,
}

pub async fn get_archetype_class_summary(
    pool: &PgPool,
    season: i32,
) -> Result<Vec<ArchetypeClassSummary>, sqlx::Error> {
    // Pre-dedupe Torvik to one row per player_id — transfer players can have
    // multiple torvik_pid stints for the same season, which would inflate
    // COUNT(*) and skew AVG(campom) when joined directly.
    sqlx::query_as::<_, ArchetypeClassSummary>(
        r#"
        WITH torvik_dedup AS (
            SELECT player_id, AVG(cam_gbpm_v3_psos) AS campom
            FROM torvik_player_stats
            WHERE season = $1 AND player_id IS NOT NULL
            GROUP BY player_id
        )
        SELECT
            pa.primary_class,
            COUNT(*) AS count,
            AVG(tps.campom) AS mean_campom
        FROM player_archetypes pa
        LEFT JOIN torvik_dedup tps ON tps.player_id = pa.player_id
        WHERE pa.season = $1
        GROUP BY pa.primary_class
        ORDER BY mean_campom DESC NULLS LAST
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await
}

/// Per-class roster breakdown for one team, indexed against the D-I-wide
/// minute-weighted distribution.
///
/// `index = team_share / d1_share`: values above 1 mean the team is loaded
/// with that class relative to the league, values below 1 mean light. All
/// classes present in the season's clustering are returned, including ones
/// the team has zero minutes of (so under-indexed and missing classes are
/// detectable).
#[derive(Debug, Serialize, FromRow)]
pub struct ArchetypeShare {
    pub primary_class: String,
    pub team_count: i64,
    pub team_minutes: f64,
    pub team_share: f64,
    pub d1_share: f64,
    pub index: Option<f64>,
}

pub async fn get_team_archetype_index(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Vec<ArchetypeShare>, sqlx::Error> {
    // Each player contributes their minutes to two classes: their primary at
    // 1.0× and their secondary at 0.5×. This captures hybrid players (e.g.
    // a Druid clustered with Sorcerer secondary still pulls some weight to
    // Sorcerer) without going all the way to a full affinity-vector mix.
    // Both team and D-I aggregates use the same weighting so the comparison
    // stays apples-to-apples.
    sqlx::query_as::<_, ArchetypeShare>(
        r#"
        WITH player_min AS (
            SELECT
                pa.player_id,
                pa.primary_class,
                pa.secondary_class,
                p.team_id,
                COALESCE(pss.minutes_per_game * pss.games_played, 0) AS minutes
            FROM player_archetypes pa
            JOIN players p ON p.id = pa.player_id
            LEFT JOIN player_season_stats pss
                ON pss.player_id = p.id
               AND pss.season = pa.season
               AND pss.team_id = p.team_id
            WHERE pa.season = $2
        ),
        weighted AS (
            SELECT player_id, primary_class AS class, team_id,
                   minutes AS weighted_minutes
            FROM player_min
            UNION ALL
            SELECT player_id, secondary_class AS class, team_id,
                   minutes * 0.5 AS weighted_minutes
            FROM player_min
            WHERE secondary_class IS NOT NULL
        ),
        team_min AS (
            SELECT
                class AS primary_class,
                COUNT(DISTINCT player_id) AS team_count,
                SUM(weighted_minutes) AS team_minutes
            FROM weighted
            WHERE team_id = $1
            GROUP BY class
        ),
        d1_min AS (
            SELECT
                class AS primary_class,
                SUM(weighted_minutes) AS d1_minutes
            FROM weighted
            GROUP BY class
        ),
        joined AS (
            SELECT
                d.primary_class,
                COALESCE(t.team_count, 0) AS team_count,
                COALESCE(t.team_minutes, 0.0) AS team_minutes,
                d.d1_minutes
            FROM d1_min d
            LEFT JOIN team_min t ON t.primary_class = d.primary_class
        )
        SELECT
            primary_class,
            team_count,
            team_minutes,
            CASE
                WHEN SUM(team_minutes) OVER () > 0
                    THEN team_minutes / SUM(team_minutes) OVER ()
                ELSE 0.0
            END AS team_share,
            CASE
                WHEN SUM(d1_minutes) OVER () > 0
                    THEN d1_minutes / SUM(d1_minutes) OVER ()
                ELSE 0.0
            END AS d1_share,
            CASE
                WHEN SUM(d1_minutes) OVER () > 0
                     AND SUM(team_minutes) OVER () > 0
                     AND d1_minutes > 0
                    THEN (team_minutes / SUM(team_minutes) OVER ())
                       / (d1_minutes / SUM(d1_minutes) OVER ())
                ELSE NULL
            END AS index
        FROM joined
        ORDER BY team_minutes DESC NULLS LAST
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_all(pool)
    .await
}

/// Bulk variant of `get_team_archetype_index`: returns the same per-class
/// distribution for every team in `team_ids` in a single round-trip.
///
/// Used by the transfers route to score roster fit across hundreds of
/// destinations without firing one query per team. Output is a map of
/// team_id → distribution rows; teams with zero archetype coverage for
/// the season (no `player_archetypes` rows joining to their roster) are
/// absent from the map, which `roster_fit::compute_fit_score` treats as
/// a maximum-gap baseline.
///
/// Weighting matches the single-team query: primary 1.0× + secondary
/// 0.5×, both for team and D-I aggregates, so per-team shares are
/// directly comparable to the Identity/Gaps UI on TeamDetail.
///
/// Note: classes the team has zero minutes in do not appear as rows.
/// Callers should treat absence as `index = 0.0` (= the candidate fills
/// a 100% gap) per `roster_fit::lookup_index`.
pub async fn get_archetype_distributions_for_teams(
    pool: &PgPool,
    team_ids: &[Uuid],
    season: i32,
) -> Result<HashMap<Uuid, Vec<ArchetypeShare>>, sqlx::Error> {
    if team_ids.is_empty() {
        return Ok(HashMap::new());
    }

    #[derive(FromRow)]
    struct Row {
        team_id: Uuid,
        primary_class: String,
        team_count: i64,
        team_minutes: f64,
        team_share: f64,
        d1_share: f64,
        index: Option<f64>,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        WITH player_min AS (
            SELECT
                pa.player_id,
                pa.primary_class,
                pa.secondary_class,
                p.team_id,
                COALESCE(pss.minutes_per_game * pss.games_played, 0) AS minutes
            FROM player_archetypes pa
            JOIN players p ON p.id = pa.player_id
            LEFT JOIN player_season_stats pss
                ON pss.player_id = p.id
               AND pss.season = pa.season
               AND pss.team_id = p.team_id
            WHERE pa.season = $2
        ),
        weighted AS (
            SELECT player_id, primary_class AS class, team_id,
                   minutes AS weighted_minutes
            FROM player_min
            UNION ALL
            SELECT player_id, secondary_class AS class, team_id,
                   minutes * 0.5 AS weighted_minutes
            FROM player_min
            WHERE secondary_class IS NOT NULL
        ),
        team_class AS (
            SELECT
                team_id,
                class,
                COUNT(DISTINCT player_id) AS team_count,
                SUM(weighted_minutes) AS team_minutes
            FROM weighted
            WHERE team_id = ANY($1)
            GROUP BY team_id, class
        ),
        team_totals AS (
            SELECT team_id, SUM(team_minutes) AS team_total
            FROM team_class
            GROUP BY team_id
        ),
        d1_class AS (
            SELECT class, SUM(weighted_minutes) AS d1_minutes
            FROM weighted
            GROUP BY class
        ),
        d1_total AS (
            SELECT SUM(weighted_minutes) AS d1_total FROM weighted
        )
        SELECT
            tc.team_id,
            tc.class AS primary_class,
            tc.team_count,
            tc.team_minutes,
            CASE
                WHEN tt.team_total > 0
                    THEN tc.team_minutes / tt.team_total
                ELSE 0.0
            END AS team_share,
            CASE
                WHEN dt.d1_total > 0
                    THEN d1c.d1_minutes / dt.d1_total
                ELSE 0.0
            END AS d1_share,
            CASE
                WHEN tt.team_total > 0 AND dt.d1_total > 0 AND d1c.d1_minutes > 0
                    THEN (tc.team_minutes / tt.team_total)
                       / (d1c.d1_minutes / dt.d1_total)
                ELSE NULL
            END AS index
        FROM team_class tc
        JOIN team_totals tt ON tt.team_id = tc.team_id
        JOIN d1_class d1c ON d1c.class = tc.class
        CROSS JOIN d1_total dt
        ORDER BY tc.team_id, tc.team_minutes DESC
        "#,
    )
    .bind(team_ids)
    .bind(season)
    .fetch_all(pool)
    .await?;

    let mut out: HashMap<Uuid, Vec<ArchetypeShare>> = HashMap::new();
    for row in rows {
        out.entry(row.team_id).or_default().push(ArchetypeShare {
            primary_class: row.primary_class,
            team_count: row.team_count,
            team_minutes: row.team_minutes,
            team_share: row.team_share,
            d1_share: row.d1_share,
            index: row.index,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Prior meetings between two teams (Phase 4b — Predict page Previous Matchups)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
pub struct PriorMeetingHeadline {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub home_team_id: Option<Uuid>,
    pub home_team_name: Option<String>,
    pub away_team_id: Option<Uuid>,
    pub away_team_name: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub is_neutral_site: bool,
    pub is_postseason: Option<bool>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct TeamGameBox {
    pub game_id: Uuid,
    pub team_id: Uuid,
    pub points: Option<i32>,
    pub fgm: Option<i32>,
    pub fga: Option<i32>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub off_rebounds: Option<i32>,
    pub total_rebounds: Option<i32>,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub turnovers: Option<i32>,
    pub fouls: Option<i32>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct PlayerGameBox {
    pub game_id: Uuid,
    pub player_id: Uuid,
    pub player_name: String,
    pub team_id: Uuid,
    pub starter: Option<bool>,
    pub minutes: Option<f64>,
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
    pub game_score: Option<f64>,
}

/// Completed games between `team_a` and `team_b` in the given season,
/// regardless of which side hosted. Newest first. Returns the headline only —
/// box-score details are fetched separately so the caller can skip them when
/// the meeting list is empty.
pub async fn get_prior_meetings(
    pool: &PgPool,
    team_a: Uuid,
    team_b: Uuid,
    season: i32,
) -> Result<Vec<PriorMeetingHeadline>, sqlx::Error> {
    sqlx::query_as::<_, PriorMeetingHeadline>(
        r#"
        SELECT
            g.id AS game_id,
            g.game_date,
            g.home_team_id,
            COALESCE(ht.short_name, ht.name) AS home_team_name,
            g.away_team_id,
            COALESCE(at.short_name, at.name) AS away_team_name,
            g.home_score,
            g.away_score,
            g.is_neutral_site,
            g.is_postseason
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id AND ht.season = g.season
        LEFT JOIN teams at ON at.id = g.away_team_id AND at.season = g.season
        WHERE g.season = $3
          AND g.home_score IS NOT NULL
          AND (
              (g.home_team_id = $1 AND g.away_team_id = $2)
              OR (g.home_team_id = $2 AND g.away_team_id = $1)
          )
        ORDER BY g.game_date DESC, g.id
        "#,
    )
    .bind(team_a)
    .bind(team_b)
    .bind(season)
    .fetch_all(pool)
    .await
}

/// Team-level box-score rows for a set of game IDs. Both sides of each game
/// are returned (so the caller groups by `game_id` to assemble per-game
/// pairs). Empty input → empty result, no DB round-trip.
pub async fn get_team_game_boxes(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<Vec<TeamGameBox>, sqlx::Error> {
    if game_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, TeamGameBox>(
        r#"
        SELECT
            game_id, team_id,
            points, fgm, fga, tpm, tpa, ftm, fta,
            off_rebounds, total_rebounds, assists, steals, blocks, turnovers, fouls
        FROM team_game_stats
        WHERE game_id = ANY($1)
        "#,
    )
    .bind(game_ids)
    .fetch_all(pool)
    .await
}

/// Player-level box-score rows for a set of game IDs. Includes every player
/// who appears in `player_game_stats` for those games (both teams, all
/// minutes). Caller filters by team and minutes as needed.
pub async fn get_player_game_boxes(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<Vec<PlayerGameBox>, sqlx::Error> {
    if game_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, PlayerGameBox>(
        r#"
        SELECT
            pgs.game_id,
            pgs.player_id,
            p.name AS player_name,
            pgs.team_id,
            pgs.starter,
            pgs.minutes,
            pgs.points,
            pgs.fgm, pgs.fga,
            pgs.tpm, pgs.tpa,
            pgs.ftm, pgs.fta,
            pgs.off_rebounds, pgs.def_rebounds, pgs.total_rebounds,
            pgs.assists, pgs.steals, pgs.blocks, pgs.turnovers, pgs.fouls,
            pgs.game_score
        FROM player_game_stats pgs
        JOIN players p ON p.id = pgs.player_id
        WHERE pgs.game_id = ANY($1)
        ORDER BY pgs.team_id, pgs.minutes DESC NULLS LAST
        "#,
    )
    .bind(game_ids)
    .fetch_all(pool)
    .await
}
