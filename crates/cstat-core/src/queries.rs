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
    /// the schedule. Upcoming games get the current-state pre-game forecast
    /// from the end-of-season model bundle; completed games get an honest
    /// point-in-time projection from the `pit` bundle with `as_of_date =
    /// game_date − 1`. Which bundle was used is surfaced via
    /// `is_pre_game_projection` below — read that, not your own copy of the
    /// "played" predicate, when deciding how to frame the cell.
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
    /// True iff the projection above came from the point-in-time bundle
    /// (`as_of_date = game_date − 1`). Mirrors the server-side rule for
    /// "this game is in the past" so the frontend doesn't have to derive
    /// its own predicate from `team_score` / `opponent_score` and
    /// produce a copy that drifts. Set by the API layer alongside the
    /// projection itself; serialized to JSON so the URL builders and
    /// chip-tinting paths can read it directly.
    #[sqlx(default)]
    pub is_pre_game_projection: bool,
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
    /// O/D decomposition (±30 sanity envelope — see TorkvikStatsRow docs).
    pub campom_o: Option<f64>,
    pub campom_d: Option<f64>,
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
    /// Torvik shot-zone volumes (attempts) — drive the team
    /// aggregate shot diet panel on TeamDetail. `NULL` when the
    /// player has no Torvik row.
    pub rim_attempted: Option<f64>,
    pub mid_attempted: Option<f64>,
    pub tpa: Option<i32>,
    pub fta: Option<i32>,
    pub rim_made: Option<f64>,
    pub mid_made: Option<f64>,
    pub tpm: Option<i32>,
    pub ftm: Option<i32>,
    /// PBP on/off splits (from `player_on_off`): team net rating per 100 poss
    /// with vs without the player, and the on−off swing. NULL for a player with
    /// no PBP-derived on/off row (pre-2012 / not loaded / corrupt-gated season).
    /// `on_off_source` (`onfloor`/`replay`) carries the lineup-accuracy caveat;
    /// `on_off_off_poss` is the off-court possession sample (for a thin-sample
    /// flag on heavy-minute starters).
    pub net_on_off: Option<f64>,
    pub on_net_rtg: Option<f64>,
    pub off_net_rtg: Option<f64>,
    pub on_off_source: Option<String>,
    pub on_off_off_poss: Option<f64>,
    /// RAPM (adjusted on/off) — displayed in the roster's Adv view as
    /// RAPM / RAPM-O / RAPM-D (d = points allowed, lower-better); raw on/off
    /// stays in the row for tooltip context. `rapm_paired_poss` is the fit
    /// sample for the UI's ~250-possession display floor. See
    /// docs/rapm_methodology.md.
    pub rapm_net: Option<f64>,
    pub rapm_o: Option<f64>,
    pub rapm_d: Option<f64>,
    pub rapm_paired_poss: Option<f64>,
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
    /// O/D decomposition (±30 sanity envelope — see TorkvikStatsRow docs).
    pub campom_o: Option<f64>,
    pub campom_d: Option<f64>,
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
    // PBP on/off (from `player_on_off`): team net per 100 poss with vs without
    // the player + the swing. NULL when the player has no PBP-derived row.
    pub net_on_off: Option<f64>,
    pub on_net_rtg: Option<f64>,
    pub off_net_rtg: Option<f64>,
    pub on_off_source: Option<String>,
    pub on_off_off_poss: Option<f64>,
    /// RAPM fields — served but no longer displayed on the Players grid (the
    /// column moved to team-context surfaces: roster Adv view + PlayerDetail
    /// panel); kept in the payload for tooling/future use.
    pub rapm_net: Option<f64>,
    pub rapm_paired_poss: Option<f64>,
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

/// A team's 5-man on-floor lineup with its season totals, from the PBP-derived
/// `lineup_aggregates` rollup. `source` is `'natstat_lineups'` (exact
/// server-computed units off the captured lineups object), `'onfloor'` (exact,
/// from the API on-floor five) or `'replay'` (~86%-accurate SUB-replay off the
/// CSV) — exposed so the UI can flag approximate lineups.
#[derive(Debug, Serialize, FromRow)]
pub struct TeamLineup {
    pub lineup: Vec<Uuid>,
    pub player_names: Vec<String>,
    /// Each player's archetype primary class, aligned by index with `lineup` /
    /// `player_names` (NULL for a player with no computed archetype). Drives the
    /// per-player square colors in the lineup waffle.
    pub player_classes: Vec<Option<String>>,
    pub stint_count: i32,
    pub points_for: i32,
    pub points_against: i32,
    pub plus_minus: i32,
    /// PBP-derived possessions (P3) and the tempo-free rates built off them.
    /// `ortg`/`drtg` are points per 100 possessions, on the same scale as team
    /// AdjO/AdjD; `net_rtg = ortg - drtg`. NULL when the lineup logged no
    /// possessions of a given side (so the UI shows "—", not a fake 0).
    pub possessions_for: f64,
    pub possessions_against: f64,
    pub minutes: f64,
    pub ortg: Option<f64>,
    pub drtg: Option<f64>,
    pub net_rtg: Option<f64>,
    pub source: String,
}

/// Top 5-man lineups for a team-season, most-used (by stint count) first.
///
/// The `lineup`, `player_names`, and `player_classes` arrays are all built from
/// a single ordered unnest so they stay index-aligned, and are sorted **by
/// player height ascending** (shortest first, NULLs last) — so a lineup reads
/// like a position card (point guard → center) and the same grid column means
/// the same size tier across every lineup. Names resolve via LEFT JOIN
/// ("Unknown" for a missing `players` row, since array elements carry no FK).
pub async fn get_team_lineups(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
    limit: i64,
) -> Result<Vec<TeamLineup>, sqlx::Error> {
    sqlx::query_as::<_, TeamLineup>(
        r#"
        SELECT
            lp.lineup,
            lp.player_names,
            lp.player_classes,
            la.stint_count,
            la.points_for,
            la.points_against,
            la.plus_minus,
            la.possessions_for,
            la.possessions_against,
            la.minutes,
            la.ortg,
            la.drtg,
            la.net_rtg,
            la.source
        FROM lineup_aggregates la
        CROSS JOIN LATERAL (
            SELECT
                array_agg(u.pid ORDER BY p.height_inches ASC NULLS LAST, u.ord)
                    AS lineup,
                array_agg(COALESCE(p.name, 'Unknown') ORDER BY p.height_inches ASC NULLS LAST, u.ord)
                    AS player_names,
                array_agg(pa.primary_class ORDER BY p.height_inches ASC NULLS LAST, u.ord)
                    AS player_classes
            FROM unnest(la.lineup) WITH ORDINALITY AS u(pid, ord)
            LEFT JOIN players p ON p.id = u.pid AND p.season = la.season
            LEFT JOIN player_archetypes pa ON pa.player_id = u.pid AND pa.season = la.season
        ) lp
        WHERE la.team_id = $1 AND la.season = $2
        -- Most-played lineups first. Minutes is the natural "most-used" measure
        -- (a lineup can rack up many short stints or a few long ones); possessions
        -- is a clock-independent tiebreak in case of a clock-parse gap.
        ORDER BY la.minutes DESC, la.possessions_for DESC, la.stint_count DESC
        LIMIT $3
        "#,
    )
    .bind(team_id)
    .bind(season)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Season rollup of a player's PBP-derived per-game columns (shot location,
/// scoring context, fouls drawn, on-floor plus/minus). All sums are over the
/// games that have play-by-play; `games` is that count.
#[derive(Debug, Serialize, FromRow)]
pub struct PlayerPbpProfile {
    pub games: i64,
    pub paint_fga: i64,
    pub paint_fgm: i64,
    pub perimeter_fga: i64,
    pub perimeter_fgm: i64,
    pub transition_pts: i64,
    pub second_chance_pts: i64,
    pub points_off_turnovers: i64,
    pub fouls_drawn: i64,
    /// Null when the player never appeared in a tracked 5-man stint (the
    /// `plus_minus_pbp` column is gated to 5-man stints, a stricter condition
    /// than having any PBP play) — so the UI shows "—", not a fabricated 0.
    pub plus_minus_pbp: Option<i64>,

    // Season RATE forms (from player_season_stats, primary-team row) + their
    // within-season percentiles (from player_percentiles, 0..1). These make the
    // raw sums above comparable across players — the Tier-1 "comparability" fix.
    // All Option: NULL for a no-percentile player (below the mpg/games gate) or a
    // corruption-gated season. Rates are share (paint_rate, *_fg_pct) or per-40
    // (the *_per40 context-scoring rates).
    pub paint_rate: Option<f64>,
    pub paint_fg_pct: Option<f64>,
    pub perimeter_fg_pct: Option<f64>,
    pub transition_pts_per40: Option<f64>,
    pub second_chance_pts_per40: Option<f64>,
    pub points_off_turnovers_per40: Option<f64>,
    pub fouls_drawn_per40: Option<f64>,
    pub paint_rate_pct: Option<f64>,
    pub paint_fg_pct_pct: Option<f64>,
    pub perimeter_fg_pct_pct: Option<f64>,
    pub transition_pts_per40_pct: Option<f64>,
    pub second_chance_pts_per40_pct: Option<f64>,
    pub points_off_turnovers_per40_pct: Option<f64>,
    pub fouls_drawn_per40_pct: Option<f64>,
}

/// Aggregate a player's PBP-derived `player_game_stats` columns to a season
/// profile. Returns `None` when the player has no play-by-play for the season
/// (pre-2012 / not loaded), so the UI can hide the panel.
pub async fn get_player_pbp_profile(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PlayerPbpProfile>, sqlx::Error> {
    let row: PlayerPbpProfile = sqlx::query_as(
        r#"
        WITH agg AS (
            SELECT
                count(*) FILTER (WHERE paint_fga IS NOT NULL) AS games,
                COALESCE(sum(paint_fga), 0)            AS paint_fga,
                COALESCE(sum(paint_fgm), 0)            AS paint_fgm,
                COALESCE(sum(perimeter_fga), 0)        AS perimeter_fga,
                COALESCE(sum(perimeter_fgm), 0)        AS perimeter_fgm,
                COALESCE(sum(transition_pts), 0)       AS transition_pts,
                COALESCE(sum(second_chance_pts), 0)    AS second_chance_pts,
                COALESCE(sum(points_off_turnovers), 0) AS points_off_turnovers,
                COALESCE(sum(fouls_drawn), 0)          AS fouls_drawn,
                -- NOT coalesced: NULL (no 5-man stint data) must stay NULL so the
                -- UI distinguishes "no on-floor +/- data" from a real even 0.
                sum(plus_minus_pbp)                    AS plus_minus_pbp
            FROM player_game_stats
            WHERE player_id = $1 AND season = $2
        ),
        -- Season rate values from the player's primary-team row (the same
        -- DISTINCT-ON-games_played choice the percentile pass makes), so the rate
        -- and its percentile agree for a mid-season transfer.
        rates AS (
            SELECT paint_rate, paint_fg_pct, perimeter_fg_pct, transition_pts_per40,
                   second_chance_pts_per40, points_off_turnovers_per40, fouls_drawn_per40
            FROM player_season_stats
            WHERE player_id = $1 AND season = $2
            -- Same ordering as the percentile pass (compute_player_percentiles'
            -- DISTINCT ON), incl. the `team_id` tiebreak, so a transfer's
            -- displayed rate and its percentile come from the same team-row.
            ORDER BY games_played DESC NULLS LAST, team_id
            LIMIT 1
        ),
        pct AS (
            SELECT paint_rate_pct, paint_fg_pct_pct, perimeter_fg_pct_pct,
                   transition_pts_per40_pct, second_chance_pts_per40_pct,
                   points_off_turnovers_per40_pct, fouls_drawn_per40_pct
            FROM player_percentiles
            WHERE player_id = $1 AND season = $2
        )
        SELECT agg.*,
               rates.paint_rate, rates.paint_fg_pct, rates.perimeter_fg_pct,
               rates.transition_pts_per40, rates.second_chance_pts_per40,
               rates.points_off_turnovers_per40, rates.fouls_drawn_per40,
               pct.paint_rate_pct, pct.paint_fg_pct_pct, pct.perimeter_fg_pct_pct,
               pct.transition_pts_per40_pct, pct.second_chance_pts_per40_pct,
               pct.points_off_turnovers_per40_pct, pct.fouls_drawn_per40_pct
        FROM agg LEFT JOIN rates ON true LEFT JOIN pct ON true
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_one(pool)
    .await?;
    Ok(if row.games > 0 { Some(row) } else { None })
}

/// A player's season on/off splits from the PBP-derived `player_on_off` rollup:
/// team offense/defense per 100 possessions WITH the player on the floor vs
/// WITHOUT him (same games, bench time only). `net_on_off` is the on−off swing.
/// `source` (`'onfloor'` exact / `'replay'` ~86%) carries the same accuracy
/// caveat as the lineup waffle. Rates are `Option` — a player who never sat has
/// no off-court possessions, so the off rates are NULL (UI shows "—").
///
/// The `rapm_*` fields are the context-adjusted companion ("Adj on/off") from
/// `player_rapm` — a ridge-regressed adjusted +/- holding teammates and
/// opponents constant (docs/rapm_methodology.md). NULL when no fit row exists
/// (e.g. 2019). `rapm_paired_possessions` is the fit sample; the UI applies a
/// ~250-possession display floor on it rather than the table gating rows.
#[derive(Debug, Serialize, FromRow)]
pub struct PlayerOnOff {
    pub games: i32,
    pub on_minutes: f64,
    pub on_possessions_for: f64,
    pub on_possessions_against: f64,
    pub on_points_for: i32,
    pub on_points_against: i32,
    pub on_ortg: Option<f64>,
    pub on_drtg: Option<f64>,
    pub on_net_rtg: Option<f64>,
    pub off_minutes: f64,
    pub off_possessions_for: f64,
    pub off_possessions_against: f64,
    pub off_points_for: i32,
    pub off_points_against: i32,
    pub off_ortg: Option<f64>,
    pub off_drtg: Option<f64>,
    pub off_net_rtg: Option<f64>,
    pub net_on_off: Option<f64>,
    pub source: String,
    pub rapm_o: Option<f64>,
    pub rapm_d: Option<f64>,
    pub rapm_net: Option<f64>,
    pub rapm_paired_possessions: Option<f64>,
}

/// Fetch a player's season on/off split. Returns `None` when the player has no
/// PBP-derived on/off row (pre-2012 / not loaded / corrupt-season-gated), so the
/// UI can hide the panel.
pub async fn get_player_on_off(
    pool: &PgPool,
    player_id: Uuid,
    season: i32,
) -> Result<Option<PlayerOnOff>, sqlx::Error> {
    sqlx::query_as::<_, PlayerOnOff>(
        r#"
        SELECT oo.games,
               oo.on_minutes, oo.on_possessions_for, oo.on_possessions_against,
               oo.on_points_for, oo.on_points_against, oo.on_ortg, oo.on_drtg, oo.on_net_rtg,
               oo.off_minutes, oo.off_possessions_for, oo.off_possessions_against,
               oo.off_points_for, oo.off_points_against, oo.off_ortg, oo.off_drtg, oo.off_net_rtg,
               oo.net_on_off, oo.source,
               pr.o_rapm AS rapm_o, pr.d_rapm AS rapm_d, pr.net_rapm AS rapm_net,
               pr.paired_possessions AS rapm_paired_possessions
        FROM player_on_off oo
        LEFT JOIN player_rapm pr ON pr.player_id = oo.player_id AND pr.season = oo.season
        -- Pin to the player's canonical team. The derivation now credits a player
        -- only to his own team's lineups so (season, player_id) is unique, but
        -- this guard keeps the route correct against any stale pre-fix rows
        -- (e.g. prod before it's recomputed) — never serve a different team's split.
        JOIN players p ON p.id = oo.player_id AND p.season = oo.season AND p.team_id = oo.team_id
        WHERE oo.player_id = $1 AND oo.season = $2
        "#,
    )
    .bind(player_id)
    .bind(season)
    .fetch_optional(pool)
    .await
}

/// One ranked lineup combination (2-, 3-, or 5-man) across the league: the
/// player set with their joint stints / minutes / +/- and per-100 team rates.
/// `lineup`/`player_names`/`player_classes` are aligned by index and height-
/// ordered (shortest→tallest, like the waffle). `source` is the dominant
/// `'onfloor'`/`'replay'` flag.
///
/// `ortg`/`drtg`/`net_rtg` are the RAW on-court rates (what happened on the
/// floor). `adj_ortg`/`adj_drtg`/`adj_net` are those same rates **opponent-
/// adjusted** by the lineup's team schedule (each rate shifted by the team's
/// `adjusted − raw` efficiency on that side), putting them on the KenPom AdjO /
/// AdjD / AdjEM scale used by the team rankings page — `adj_net = adj_ortg −
/// adj_drtg`. The page ranks by `adj_net`; a per-size minutes floor (not
/// shrinkage) keeps thin-sample outliers out. All rates are `Option` (NULL where
/// the combo logged no possessions of a side, or the team has no adjusted-
/// efficiency row) and sort last.
#[derive(Debug, Serialize, FromRow)]
pub struct LineupRanking {
    pub lineup: Vec<Uuid>,
    pub player_names: Vec<String>,
    pub player_classes: Vec<Option<String>>,
    pub team_id: Uuid,
    pub team_name: String,
    pub stints: i64,
    pub minutes: f64,
    pub plus_minus: i64,
    pub possessions_for: f64,
    pub possessions_against: f64,
    pub ortg: Option<f64>,
    pub drtg: Option<f64>,
    pub net_rtg: Option<f64>,
    pub adj_ortg: Option<f64>,
    pub adj_drtg: Option<f64>,
    pub adj_net: Option<f64>,
    pub source: String,
}

/// Rank lineup combinations of `size` (2, 3, or 5) players across all teams for
/// a season, best **opponent-adjusted net** (`adj_net`) first among combos
/// clearing the `min_minutes` floor. The floor (not shrinkage) is what keeps
/// thin-sample outliers out — once the schedule adjustment removes weak-schedule
/// inflation, the high-`adj_net` combos are the genuinely strong units, so a
/// light per-size floor (5-man 100 / trio 200 / duo 300, set by the caller)
/// suffices. Optionally filtered to combos containing `player` and/or belonging
/// to `team`.
///
/// Duos and trios aren't stored — they're exploded at query time from the
/// prod-resident 5-man `lineup_aggregates` (any time N players share the floor
/// they're in *some* 5-man unit, so summing the rows whose `lineup` contains
/// all N is the exact joint on-floor total). `size = 5` reads the rows directly.
/// The combo key is the sorted player-UUID array, so a set counted once per
/// contributing lineup aggregates cleanly across array orderings. Trio explosion
/// is the heaviest (C(5,3)=10× row fan-out, ~1s/season); fine for a per-toggle
/// ranking page, a candidate for materialization if it becomes hot.
pub async fn get_lineup_rankings(
    pool: &PgPool,
    season: i32,
    size: i32,
    min_minutes: f64,
    limit: i64,
    player: Option<Uuid>,
    team: Option<Uuid>,
) -> Result<Vec<LineupRanking>, sqlx::Error> {
    // The combination CTE differs per size (the number of `unnest` joins can't
    // be parameterized at runtime). `size` is validated to {2,3,5} by the
    // caller, so this match is exhaustive of the served values; any other value
    // falls back to 5-man.
    let combo_cte = match size {
        2 => {
            "SELECT la.team_id, la.season,
                    ARRAY[LEAST(a.pid, b.pid), GREATEST(a.pid, b.pid)] AS combo,
                    la.stint_count, la.minutes, la.plus_minus,
                    la.points_for, la.points_against,
                    la.possessions_for, la.possessions_against, la.source
             FROM lineup_aggregates la,
                  LATERAL unnest(la.lineup) WITH ORDINALITY AS a(pid, i),
                  LATERAL unnest(la.lineup) WITH ORDINALITY AS b(pid, j)
             WHERE la.season = $1 AND a.i < b.j"
        }
        3 => {
            "SELECT la.team_id, la.season,
                    ARRAY(SELECT x FROM unnest(ARRAY[a.pid, b.pid, c.pid]) AS x ORDER BY x) AS combo,
                    la.stint_count, la.minutes, la.plus_minus,
                    la.points_for, la.points_against,
                    la.possessions_for, la.possessions_against, la.source
             FROM lineup_aggregates la,
                  LATERAL unnest(la.lineup) WITH ORDINALITY AS a(pid, i),
                  LATERAL unnest(la.lineup) WITH ORDINALITY AS b(pid, j),
                  LATERAL unnest(la.lineup) WITH ORDINALITY AS c(pid, k)
             WHERE la.season = $1 AND a.i < b.j AND b.j < c.k"
        }
        _ => {
            "SELECT la.team_id, la.season,
                    ARRAY(SELECT x FROM unnest(la.lineup) AS x ORDER BY x) AS combo,
                    la.stint_count, la.minutes, la.plus_minus,
                    la.points_for, la.points_against,
                    la.possessions_for, la.possessions_against, la.source
             FROM lineup_aggregates la
             WHERE la.season = $1"
        }
    };

    let sql = format!(
        r#"
        WITH exploded AS ( {combo_cte} ),
        agg AS (
            SELECT team_id, season, combo,
                   sum(stint_count)::bigint AS stints,
                   sum(minutes) AS minutes,
                   sum(plus_minus)::bigint AS plus_minus,
                   sum(points_for) AS points_for,
                   sum(points_against) AS points_against,
                   sum(possessions_for) AS possessions_for,
                   sum(possessions_against) AS possessions_against,
                   mode() WITHIN GROUP (ORDER BY source) AS source
            FROM exploded
            GROUP BY team_id, season, combo
        ),
        -- Per-team RAW offensive / defensive efficiency (points per 100 poss,
        -- possessions = FGA − OREB + TO + 0.44·FTA) straight from the box. The
        -- gap between each side and the opponent-adjusted AdjO / AdjD is the
        -- team's schedule strength on that side, which we add to each of its
        -- lineups so the per-100 rates land on the KenPom adjusted scale (a
        -- weak-schedule mid-major's inflated raw net gets discounted).
        team_box AS (
            SELECT s.team_id,
                   100.0 * sum(s.points) / NULLIF(sum(
                       s.fga - COALESCE(s.off_rebounds,0) + COALESCE(s.turnovers,0) + 0.44 * COALESCE(s.fta,0)
                   ), 0) AS raw_off,
                   100.0 * sum(o.points) / NULLIF(sum(
                       o.fga - COALESCE(o.off_rebounds,0) + COALESCE(o.turnovers,0) + 0.44 * COALESCE(o.fta,0)
                   ), 0) AS raw_def
            FROM team_game_stats s
            JOIN team_game_stats o ON o.game_id = s.game_id AND o.team_id <> s.team_id
            WHERE s.season = $1
            GROUP BY s.team_id
        ),
        sched AS (
            SELECT tb.team_id,
                   (ts.adj_offense - tb.raw_off) AS o_adj,
                   (ts.adj_defense - tb.raw_def) AS d_adj
            FROM team_box tb
            JOIN team_season_stats ts ON ts.team_id = tb.team_id AND ts.season = $1
            WHERE ts.adj_offense IS NOT NULL AND ts.adj_defense IS NOT NULL
        ),
        scored AS (
            SELECT a.team_id, a.season, a.combo, a.stints, a.minutes, a.plus_minus,
                   a.possessions_for, a.possessions_against, a.source,
                   CASE WHEN a.possessions_for > 0
                        THEN 100.0 * a.points_for / a.possessions_for END AS ortg,
                   CASE WHEN a.possessions_against > 0
                        THEN 100.0 * a.points_against / a.possessions_against END AS drtg,
                   sc.o_adj, sc.d_adj
            FROM agg a
            LEFT JOIN sched sc ON sc.team_id = a.team_id
        )
        SELECT
            lp.lineup,
            lp.player_names,
            lp.player_classes,
            s.team_id,
            t.name AS team_name,
            s.stints,
            s.minutes,
            s.plus_minus,
            s.possessions_for,
            s.possessions_against,
            s.ortg,
            s.drtg,
            CASE WHEN s.ortg IS NOT NULL AND s.drtg IS NOT NULL
                 THEN s.ortg - s.drtg END AS net_rtg,
            -- Opponent-adjusted rates: raw per-100 shifted by the team's
            -- schedule adjustment on each side (KenPom AdjO / AdjD / AdjEM scale).
            CASE WHEN s.ortg IS NOT NULL AND s.o_adj IS NOT NULL
                 THEN s.ortg + s.o_adj END AS adj_ortg,
            CASE WHEN s.drtg IS NOT NULL AND s.d_adj IS NOT NULL
                 THEN s.drtg + s.d_adj END AS adj_drtg,
            CASE WHEN s.ortg IS NOT NULL AND s.drtg IS NOT NULL
                  AND s.o_adj IS NOT NULL AND s.d_adj IS NOT NULL
                 THEN (s.ortg + s.o_adj) - (s.drtg + s.d_adj) END AS adj_net,
            s.source
        FROM scored s
        JOIN teams t ON t.id = s.team_id
        CROSS JOIN LATERAL (
            SELECT
                array_agg(u.pid ORDER BY p.height_inches ASC NULLS LAST, u.ord) AS lineup,
                array_agg(COALESCE(p.name, 'Unknown') ORDER BY p.height_inches ASC NULLS LAST, u.ord) AS player_names,
                array_agg(pa.primary_class ORDER BY p.height_inches ASC NULLS LAST, u.ord) AS player_classes
            FROM unnest(s.combo) WITH ORDINALITY AS u(pid, ord)
            LEFT JOIN players p ON p.id = u.pid AND p.season = s.season
            LEFT JOIN player_archetypes pa ON pa.player_id = u.pid AND pa.season = s.season
        ) lp
        WHERE s.minutes >= $2
          AND ($4::uuid IS NULL OR $4 = ANY(s.combo))
          AND ($5::uuid IS NULL OR s.team_id = $5)
        ORDER BY adj_net DESC NULLS LAST, s.minutes DESC
        LIMIT $3
        "#
    );

    sqlx::query_as::<_, LineupRanking>(&sql)
        .bind(season)
        .bind(min_minutes)
        .bind(limit)
        .bind(player)
        .bind(team)
        .fetch_all(pool)
        .await
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

/// Count of teams in `team_season_stats` for the given season. Used as
/// the denominator when converting a per-stat rank into a percentile
/// for the red→green tint on team-detail stat cards. Matches the
/// rankings page's `teams.length` count for that season.
pub async fn get_season_team_count(pool: &PgPool, season: i32) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM team_season_stats
        WHERE season = $1
        "#,
    )
    .bind(season)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
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
            -- O/D split, ±30 sanity envelope (see TorkvikStatsRow docs)
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_o_gbpm_v3_psos END AS campom_o,
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_d_gbpm_v3_psos END AS campom_d,
            pp.ppg_pct, pp.rpg_pct, pp.apg_pct, pp.spg_pct, pp.bpg_pct, pp.topg_pct,
            pp.true_shooting_pct_pct,
            pp.usage_rate_pct,
            pp.ast_pct_pct, pp.tov_pct_pct,
            pp.orb_pct_pct, pp.drb_pct_pct, pp.stl_pct_pct, pp.blk_pct_pct,
            pa.primary_class, pa.secondary_class,
            tps.rim_attempted, tps.mid_attempted, tps.tpa, tps.fta,
            tps.rim_made, tps.mid_made, tps.tpm, tps.ftm,
            oo.net_on_off, oo.on_net_rtg, oo.off_net_rtg,
            oo.source AS on_off_source,
            (oo.off_possessions_for + oo.off_possessions_against) AS on_off_off_poss,
            pr.net_rapm AS rapm_net,
            pr.o_rapm AS rapm_o,
            pr.d_rapm AS rapm_d,
            pr.paired_possessions AS rapm_paired_poss
        FROM players p
        JOIN player_season_stats pss ON pss.player_id = p.id AND pss.team_id = p.team_id AND pss.season = p.season
        LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = p.season
        LEFT JOIN player_percentiles pp ON pp.player_id = p.id AND pp.season = p.season
        LEFT JOIN player_archetypes pa ON pa.player_id = p.id AND pa.season = p.season
        LEFT JOIN player_on_off oo ON oo.player_id = p.id AND oo.season = p.season AND oo.team_id = p.team_id
        LEFT JOIN player_rapm pr ON pr.player_id = p.id AND pr.season = p.season
        WHERE p.team_id = $1 AND p.season = $2
        -- Minutes-first default: consumers that take the order as-is (the
        -- Predict matchup roster panels' top-N slice) show the actual
        -- rotation; TeamDetail re-sorts client-side (also defaulting to MPG).
        ORDER BY pss.minutes_per_game DESC NULLS LAST, tps.cam_gbpm_v3_psos DESC NULLS LAST
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
            -- O/D split, ±30 sanity envelope (see TorkvikStatsRow docs)
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_o_gbpm_v3_psos END AS campom_o,
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_d_gbpm_v3_psos END AS campom_d,
            pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
            pss.stl_pct, pss.blk_pct, pss.ft_rate,
            pp.ppg_pct, pp.rpg_pct, pp.apg_pct, pp.spg_pct, pp.bpg_pct, pp.topg_pct,
            pp.mpg_pct, pp.usage_rate_pct, pp.true_shooting_pct_pct,
            pp.ast_pct_pct, pp.tov_pct_pct, pp.orb_pct_pct, pp.drb_pct_pct,
            pp.stl_pct_pct, pp.blk_pct_pct,
            pa.primary_class, pa.secondary_class,
            oo.net_on_off, oo.on_net_rtg, oo.off_net_rtg,
            oo.source AS on_off_source,
            (oo.off_possessions_for + oo.off_possessions_against) AS on_off_off_poss,
            pr.net_rapm AS rapm_net,
            pr.paired_possessions AS rapm_paired_poss
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
        LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = pss.season
        LEFT JOIN player_percentiles pp ON pp.player_id = pss.player_id AND pp.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        LEFT JOIN player_on_off oo
            ON oo.player_id = p.id AND oo.season = pss.season AND oo.team_id = p.team_id
        LEFT JOIN player_rapm pr
            ON pr.player_id = p.id AND pr.season = pss.season
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
    /// CamPom's offensive/defensive decomposition (cam_o + cam_d = campom;
    /// d positive-good). The compute-side SOS allocation uses bounded
    /// magnitude shares (fixed 2026-06-12 — the original signed-share split
    /// exploded for low-|net| players; see docs/campom_methodology.md "O/D
    /// Decomposition"). NULL outside a ±30 sanity envelope, kept as a
    /// regression guard: best legit split on record is +26.6 (Edey 2024 O),
    /// so the envelope can only ever hide junk.
    pub campom_o: Option<f64>,
    pub campom_d: Option<f64>,
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
               -- O/D split inside the ±30 sanity envelope (a regression
               -- guard — the compute-side allocation is bounded since the
               -- 2026-06-12 magnitude-share fix; see struct docs).
               CASE WHEN abs(cam_o_gbpm_v3_psos) <= 30 AND abs(cam_d_gbpm_v3_psos) <= 30
                    THEN cam_o_gbpm_v3_psos END AS campom_o,
               CASE WHEN abs(cam_o_gbpm_v3_psos) <= 30 AND abs(cam_d_gbpm_v3_psos) <= 30
                    THEN cam_d_gbpm_v3_psos END AS campom_d,
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
/// **Not consumed by any production scoring surface** — kept for ad-hoc
/// validation work (e.g. `training/validate_archetype_balance.py`
/// regresses team AdjEM against per-team archetype-balance metrics
/// across all D-I) and for the upcoming archetype visualization layer
/// (Phase 5b — Team Compare's side-by-side distribution view). The
/// roster-fit chip that originally drove this query was reverted after
/// the balance-is-good prior failed to validate; see
/// `docs/archetype_balance_finding.md`.
///
/// Weighting matches the single-team query: primary 1.0× + secondary
/// 0.5×, both for team and D-I aggregates. Classes a team has zero
/// minutes in do not appear as rows; callers should treat absence as
/// `index = 0.0`.
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

/// Per-class D-I-wide share of weighted minutes for the given season.
///
/// Same `weighted` CTE as `get_team_archetype_index` (primary 1.0× +
/// secondary 0.5×), but aggregated over the entire league with no team
/// filter. Returns `primary_class → share` where shares sum to 1.0
/// across the 12 classes (modulo NULL-secondary players who only
/// contribute to their primary).
///
/// **Not consumed by any production scoring surface** — paired with
/// `roster_fit::build_projected_class_minutes` for the projected-roster
/// fit pipeline, which is preserved for the upcoming archetype
/// visualization layer (Phase 5b). See `docs/archetype_balance_finding.md`
/// for why archetype balance was investigated and dropped as a scoring
/// signal on the transfer surface.
pub async fn get_d1_archetype_shares(
    pool: &PgPool,
    season: i32,
) -> Result<HashMap<String, f64>, sqlx::Error> {
    #[derive(FromRow)]
    struct Row {
        primary_class: String,
        share: f64,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        WITH player_min AS (
            SELECT
                pa.player_id,
                pa.primary_class,
                pa.secondary_class,
                COALESCE(pss.minutes_per_game * pss.games_played, 0) AS minutes
            FROM player_archetypes pa
            JOIN players p ON p.id = pa.player_id
            LEFT JOIN player_season_stats pss
                ON pss.player_id = p.id
               AND pss.season = pa.season
               AND pss.team_id = p.team_id
            WHERE pa.season = $1
        ),
        weighted AS (
            SELECT primary_class AS class, minutes AS weighted_minutes
            FROM player_min
            UNION ALL
            SELECT secondary_class AS class, minutes * 0.5 AS weighted_minutes
            FROM player_min
            WHERE secondary_class IS NOT NULL
        ),
        d1_class AS (
            SELECT class, SUM(weighted_minutes) AS class_min
            FROM weighted
            GROUP BY class
        ),
        d1_total AS (
            SELECT SUM(weighted_minutes) AS total FROM weighted
        )
        SELECT
            d1c.class AS primary_class,
            CASE WHEN dt.total > 0 THEN d1c.class_min / dt.total ELSE 0.0 END AS share
        FROM d1_class d1c
        CROSS JOIN d1_total dt
        "#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.primary_class, r.share))
        .collect())
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

// ---------------------------------------------------------------------------
// Coaches — Coach-Above-Expectation (CAE) leaderboard + detail (PR3 surfacing)
//
// Pure read-path over the precomputed tables from migrations 024/025
// (`coaches`, `coach_seasons`, `coach_ratings`, `coach_season_cae`). No live
// inference: the per-season residuals already store `actual_adjem` and
// `projection`, so even the sparkline runs zero predictions. The HEADLINE
// rating is `cae_shrunk` (raw EB-shrunk), never `cae_raw_mean` — see the
// migration 025 comments and ROADMAP Phase 6 display contract.
// ---------------------------------------------------------------------------

/// One row of the `/coaches` leaderboard: a coach's career-level shrunk CAE
/// plus the most-recent team they coached (for context / a clickable link).
#[derive(Debug, Serialize, FromRow)]
pub struct CoachLeaderboardRow {
    pub coach_id: Uuid,
    pub name: String,
    /// Headline rating — raw EB-shrunk CAE. Default leaderboard sort key.
    pub cae_shrunk: f64,
    /// Unshrunk mean (transparency only; never the headline).
    pub cae_raw_mean: f64,
    /// Prestige-adjusted (projection-quartile-de-biased) shrunk value — a
    /// conservative lower bound, surfaced as a secondary column/toggle.
    pub cae_adj_shrunk: f64,
    /// Season-centered shrunk value — COMPARISON-ONLY (each season's mean
    /// residual removed for era-neutral cross-coach ranking; not an absolute
    /// "how much" measure). Surfaced as a secondary column, never the sort key.
    pub cae_centered_shrunk: f64,
    /// n / (n + k) ∈ [0,1] — the shrinkage weight, shown so thin tenures read
    /// as low-confidence.
    pub reliability: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n_seasons: i32,
    pub first_season: i32,
    pub last_season: i32,
    /// Career mean of the team's actual AdjEM / AdjO / AdjD across the coach's
    /// scored seasons — DISPLAY-ONLY team-strength context ("who delivers strong,
    /// tough-schedule teams"; AdjEM is opponent-adjusted, so SOS is baked in).
    /// NULL when no scored season resolved to a `team_season_stats` row. These
    /// columns must NEVER feed projections: raw AdjEM *is* the projection target,
    /// so round-tripping it is direct leakage (see ROADMAP "Coach rankings").
    pub career_adj_em: Option<f64>,
    pub career_adj_o: Option<f64>,
    pub career_adj_d: Option<f64>,
    /// Evaluative composite `z(cae_shrunk) + z(career_adj_em)` over the qualified
    /// leaderboard population — an alternate "results + overperformance" sort, a
    /// lens not a truth. Computed in Rust post-fetch via [`apply_career_blend`],
    /// so it carries `#[sqlx(default)]` (no backing column) and is `None` until
    /// populated (degenerate populations leave it `None`).
    #[sqlx(default)]
    pub blend: Option<f64>,
    /// The team shown in the leaderboard's "Team" column. When the query is
    /// season-scoped this is the team the coach held *that* season; otherwise
    /// their most recent scored team. NULL if no matched team row.
    pub last_team_id: Option<Uuid>,
    pub last_team_name: Option<String>,
    /// The season of `last_team_*`, so the team link deep-links to the right
    /// season-scoped page. NULL when there's no matched team.
    pub last_team_season: Option<i32>,
}

/// Populate the display-only `blend` lens on a set of career leaderboard rows:
/// `z(cae_shrunk) + z(career_adj_em)`, z-scored over the supplied (already
/// qualified) population. This is an *evaluative* composite — "results +
/// overperformance" — NOT a rigorous metric, and it must NEVER reach a
/// projection: it contains raw AdjEM, the forecast's own target.
///
/// INVARIANT: `rows` must be the **complete** qualified board, not a truncated
/// page — the z-scores are only meaningful over the full population. The
/// frontend satisfies this by requesting the full board (limit 1000 vs ~690
/// all-time coaches); the caller must not blend a page cut short by `limit` (see
/// the guard in `coach_leaderboard`).
pub fn apply_career_blend(rows: &mut [CoachLeaderboardRow]) {
    apply_blend(
        rows,
        |r| r.cae_shrunk,
        |r| r.career_adj_em,
        |r, b| {
            r.blend = b;
        },
    );
}

/// The single-season analog of [`apply_career_blend`]: `z(cae_raw) +
/// z(actual_adjem)` over that season's board. Same "results + overperformance"
/// lens, same display-only / never-a-projection-input wall — just unshrunk
/// (single seasons carry no tenure to shrink over).
pub fn apply_season_blend(rows: &mut [CoachSeasonLeaderboardRow]) {
    apply_blend(
        rows,
        |r| r.cae_raw,
        |r| Some(r.actual_adjem),
        |r, b| r.blend = b,
    );
}

/// Z-score the supplied population on two axes — a CAE term and an AdjEM term —
/// and write their sum back via `set_blend`. Generic over the row type so the
/// career and single-season boards share one definition.
///
/// Rows whose AdjEM term is `None` (no matched team row) contribute only the CAE
/// z-term (AdjEM treated as the population average). A degenerate population
/// (n < 2 or zero variance in a term) zeroes that term; if both terms are
/// undefined every `blend` is left untouched at its `None` default.
fn apply_blend<T>(
    rows: &mut [T],
    cae_of: impl Fn(&T) -> f64,
    adjem_of: impl Fn(&T) -> Option<f64>,
    set_blend: impl Fn(&mut T, Option<f64>),
) {
    if rows.len() < 2 {
        return;
    }
    let (mu_cae, sd_cae) = population_mean_std(rows.iter().map(&cae_of));
    let (mu_em, sd_em) = population_mean_std(rows.iter().filter_map(&adjem_of));
    // Nothing to score on — leave every `blend` at its `None` default.
    if sd_cae.is_none() && sd_em.is_none() {
        return;
    }
    for r in rows.iter_mut() {
        let z_cae = sd_cae.map_or(0.0, |sd| (cae_of(r) - mu_cae) / sd);
        let z_em = match (adjem_of(r), sd_em) {
            (Some(em), Some(sd)) => (em - mu_em) / sd,
            _ => 0.0,
        };
        set_blend(r, Some(z_cae + z_em));
    }
}

/// Population mean and population standard deviation of a sample. `sd` is `None`
/// when there are fewer than two values or the variance is 0 — the cases where a
/// z-score is undefined; callers treat a `None` sd as a 0 contribution.
fn population_mean_std(vals: impl Iterator<Item = f64>) -> (f64, Option<f64>) {
    let v: Vec<f64> = vals.collect();
    let n = v.len() as f64;
    if v.is_empty() {
        return (0.0, None);
    }
    let mu = v.iter().sum::<f64>() / n;
    if v.len() < 2 {
        return (mu, None);
    }
    let var = v.iter().map(|x| (x - mu).powi(2)).sum::<f64>() / n;
    let sd = var.sqrt();
    (mu, (sd > 0.0).then_some(sd))
}

/// Career leaderboard ranked by `cae_shrunk` DESC. The CAE rating is always
/// career-aggregated; `season`, when set, scopes the *list* to coaches who
/// actually coached that season (and shows that season's team), so the navbar
/// season picker is meaningful without changing the rating semantics. `None`
/// season = all-time. `min_seasons` defaults to 3 at the API layer (thin
/// tenures shrink toward 0 and would otherwise top the board on noise); `limit`
/// caps the page size.
pub async fn get_coach_leaderboard(
    pool: &PgPool,
    min_seasons: i32,
    limit: i64,
    season: Option<i32>,
) -> Result<Vec<CoachLeaderboardRow>, sqlx::Error> {
    sqlx::query_as::<_, CoachLeaderboardRow>(
        r#"
        SELECT
            c.id            AS coach_id,
            c.canonical_name AS name,
            cr.cae_shrunk,
            cr.cae_raw_mean,
            cr.cae_adj_shrunk,
            cr.cae_centered_shrunk,
            cr.reliability,
            cr.ci_low,
            cr.ci_high,
            cr.n_seasons,
            cr.first_season,
            cr.last_season,
            st.career_adj_em,
            st.career_adj_o,
            st.career_adj_d,
            lt.team_id      AS last_team_id,
            lt.team_name    AS last_team_name,
            lt.team_season  AS last_team_season
        FROM coach_ratings cr
        JOIN coaches c ON c.id = cr.coach_id
        -- Display-only team-strength means over the coach's scored seasons.
        -- coach_season_cae carries (team_natstat_id, season); resolve each to
        -- the season-scoped team_season_stats via the cross-season natstat key.
        -- AdjEM is opponent-adjusted, so a strength sort inherently rewards hard
        -- schedules — no separate SOS term. NEVER fed back into projections.
        LEFT JOIN LATERAL (
            SELECT
                AVG(tss.adj_efficiency_margin) AS career_adj_em,
                AVG(tss.adj_offense)           AS career_adj_o,
                AVG(tss.adj_defense)           AS career_adj_d
            FROM coach_season_cae csc
            JOIN teams t2 ON t2.natstat_id = csc.team_natstat_id AND t2.season = csc.season
            JOIN team_season_stats tss ON tss.team_id = t2.id AND tss.season = csc.season
            WHERE csc.coach_id = cr.coach_id
        ) st ON TRUE
        LEFT JOIN LATERAL (
            SELECT cs.team_id, cs.season AS team_season,
                   COALESCE(t.short_name, t.name) AS team_name
            FROM coach_seasons cs
            LEFT JOIN teams t ON t.id = cs.team_id
            WHERE cs.coach_id = c.id AND cs.team_id IS NOT NULL
            -- Prefer the selected season's team when scoped; else most recent.
            ORDER BY (CASE WHEN $3::int IS NOT NULL AND cs.season = $3 THEN 0 ELSE 1 END),
                     cs.season DESC
            LIMIT 1
        ) lt ON TRUE
        WHERE cr.n_seasons >= $1
          AND (
            $3::int IS NULL
            OR EXISTS (
              SELECT 1 FROM coach_seasons cs2
              WHERE cs2.coach_id = c.id AND cs2.season = $3
            )
          )
        -- canonical_name breaks exact-tie CAE so paging/order is stable.
        ORDER BY cr.cae_shrunk DESC, c.canonical_name
        LIMIT $2
        "#,
    )
    .bind(min_seasons)
    .bind(limit)
    .bind(season)
    .fetch_all(pool)
    .await
}

/// One row of the *season* leaderboard (the "This season" toggle): a coach's
/// single-season CAE for the selected year, ranked by `cae_raw` DESC. This view
/// is noisier than the career board on purpose — single-season residuals are
/// mostly noise (which is why the career view shrinks them) — so it reads as a
/// "who overachieved this year" board, not a trustworthy rating.
#[derive(Debug, Serialize, FromRow)]
pub struct CoachSeasonLeaderboardRow {
    pub coach_id: Uuid,
    pub name: String,
    pub season: i32,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub actual_adjem: f64,
    pub projection: f64,
    pub cae_raw: f64,
    pub cae_debiased: f64,
    /// Season-centered residual — comparison-only (this season's mean removed).
    pub cae_centered: f64,
    /// That season's team AdjO / AdjD — DISPLAY-ONLY strength context next to the
    /// stored `actual_adjem` (the season's AdjEM). NULL when the team row didn't
    /// resolve. Same no-leakage wall as the career columns.
    pub adj_offense: Option<f64>,
    pub adj_defense: Option<f64>,
    /// Single-season "results + overperformance" lens — `z(cae_raw) +
    /// z(actual_adjem)` over this season's board. Computed in Rust post-fetch via
    /// [`apply_season_blend`], so it carries `#[sqlx(default)]` (no backing
    /// column) and stays `None` on degenerate boards. Display-only.
    #[sqlx(default)]
    pub blend: Option<f64>,
    pub is_new_hc: Option<bool>,
}

/// Single-season CAE leaderboard for `season`, ranked by raw residual DESC.
pub async fn get_coach_season_leaderboard(
    pool: &PgPool,
    season: i32,
    limit: i64,
) -> Result<Vec<CoachSeasonLeaderboardRow>, sqlx::Error> {
    sqlx::query_as::<_, CoachSeasonLeaderboardRow>(
        r#"
        SELECT
            c.id            AS coach_id,
            c.canonical_name AS name,
            csc.season,
            tm.team_id,
            tm.team_name,
            csc.actual_adjem,
            csc.projection,
            csc.cae_raw,
            csc.cae_debiased,
            csc.cae_centered,
            ts.adj_offense,
            ts.adj_defense,
            tm.is_new_hc
        FROM coach_season_cae csc
        JOIN coaches c ON c.id = csc.coach_id
        -- Dedup the coach_seasons join to ONE team row: coachdict carries
        -- redundant name variants for some teams (e.g. "Tennessee Martin" +
        -- "UT Martin", "Saint Joseph's" + "St. Joseph's"), which would otherwise
        -- fan a coach out to two leaderboard rows — one with the matched team,
        -- one with an unmatched (NULL) team. Prefer the matched variant.
        LEFT JOIN LATERAL (
            SELECT cs.team_id, cs.is_new_hc,
                   COALESCE(t.short_name, t.name) AS team_name
            FROM coach_seasons cs
            LEFT JOIN teams t ON t.id = cs.team_id
            WHERE cs.coach_id = csc.coach_id AND cs.season = csc.season
            ORDER BY (cs.team_id IS NOT NULL) DESC, cs.coachdict_team_name
            LIMIT 1
        ) tm ON TRUE
        -- That season's AdjO/AdjD via the cross-season natstat key (display-only
        -- strength context; AdjEM is already stored as actual_adjem).
        LEFT JOIN teams t2 ON t2.natstat_id = csc.team_natstat_id AND t2.season = csc.season
        LEFT JOIN team_season_stats ts ON ts.team_id = t2.id AND ts.season = csc.season
        WHERE csc.season = $1
        ORDER BY csc.cae_raw DESC, c.canonical_name
        LIMIT $2
        "#,
    )
    .bind(season)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Seasons that have scored CAE rows — used to constrain the navbar season
/// picker on the /coaches page to the metric's coverage (2016–2026 today),
/// newest first. The leaderboard is bounded by roster-projection coverage, not
/// coachdict coverage.
pub async fn get_coach_cae_seasons(pool: &PgPool) -> Result<Vec<i32>, sqlx::Error> {
    let rows: Vec<(i32,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT season FROM coach_season_cae ORDER BY season DESC
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// Career-level rating row for the coach detail page header.
#[derive(Debug, Serialize, FromRow)]
pub struct CoachRating {
    pub coach_id: Uuid,
    pub name: String,
    pub cae_shrunk: f64,
    pub cae_raw_mean: f64,
    pub cae_adj_shrunk: f64,
    pub cae_adj_mean: f64,
    /// Season-centered career value — comparison-only (era-neutral ranking).
    pub cae_centered_shrunk: f64,
    pub cae_centered_mean: f64,
    pub reliability: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n_seasons: i32,
    pub first_season: i32,
    pub last_season: i32,
    /// Career-mean team AdjEM / AdjO / AdjD across scored seasons — DISPLAY-ONLY
    /// team-strength context for the detail header. NULL when no scored season
    /// resolved to a `team_season_stats` row. Same hard wall as the leaderboard
    /// columns: never an input to any projection.
    pub career_adj_em: Option<f64>,
    pub career_adj_o: Option<f64>,
    pub career_adj_d: Option<f64>,
}

/// One scored (coach, team, season) — the detail-page sparkline + season table.
/// `actual_adjem`/`projection`/`cae_raw` are stored columns (no inference).
#[derive(Debug, Serialize, FromRow)]
pub struct CoachSeasonRow {
    pub season: i32,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    /// That season's actual team AdjEM. `None` only when the team row didn't
    /// resolve to `team_season_stats` (D-I-transition years, ghost rows).
    /// Present even for *ungraded* seasons — the actual is always known; it's
    /// the projection/CAE that's missing.
    pub actual_adjem: Option<f64>,
    /// Roster-only projection. `None` for **ungraded** seasons — teams the
    /// roster projection dropped (too-thin / heavy-portal-rebuild rosters below
    /// `MIN_QUALIFYING_FOR_PROJECTION`), so no CAE could be computed.
    pub projection: Option<f64>,
    /// Raw CAE (actual − projection). `None` ⇔ ungraded season (no projection).
    pub cae_raw: Option<f64>,
    pub cae_debiased: Option<f64>,
    /// Season-centered residual — comparison-only (this season's mean removed).
    pub cae_centered: Option<f64>,
    /// That season's team AdjO / AdjD — DISPLAY-ONLY strength context next to the
    /// stored `actual_adjem` (which is the season's AdjEM). NULL when the team
    /// row didn't resolve. Same no-leakage wall as the career columns.
    pub adj_offense: Option<f64>,
    pub adj_defense: Option<f64>,
    /// Whether this was the coach's first season at the team (PR E flag).
    pub is_new_hc: Option<bool>,
}

/// One coach's canonical display name. `None` when the id isn't in `coaches`.
/// Sourced independently of `coach_ratings` so the detail page can title a coach
/// who has *only* ungraded seasons (no career rating) — those pages became
/// reachable when `get_coach_seasons` started listing ungraded seasons.
pub async fn get_coach_name(pool: &PgPool, coach_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT canonical_name FROM coaches WHERE id = $1")
        .bind(coach_id)
        .fetch_optional(pool)
        .await
}

/// The career rating for one coach. `None` when the coach exists in `coaches`
/// but never landed in the scored backtest (no `coach_ratings` row).
pub async fn get_coach_rating(
    pool: &PgPool,
    coach_id: Uuid,
) -> Result<Option<CoachRating>, sqlx::Error> {
    sqlx::query_as::<_, CoachRating>(
        r#"
        SELECT
            c.id            AS coach_id,
            c.canonical_name AS name,
            cr.cae_shrunk,
            cr.cae_raw_mean,
            cr.cae_adj_shrunk,
            cr.cae_adj_mean,
            cr.cae_centered_shrunk,
            cr.cae_centered_mean,
            cr.reliability,
            cr.ci_low,
            cr.ci_high,
            cr.n_seasons,
            cr.first_season,
            cr.last_season,
            st.career_adj_em,
            st.career_adj_o,
            st.career_adj_d
        FROM coach_ratings cr
        JOIN coaches c ON c.id = cr.coach_id
        -- Display-only team-strength means (see get_coach_leaderboard for the
        -- join rationale and the no-leakage wall).
        LEFT JOIN LATERAL (
            SELECT
                AVG(tss.adj_efficiency_margin) AS career_adj_em,
                AVG(tss.adj_offense)           AS career_adj_o,
                AVG(tss.adj_defense)           AS career_adj_d
            FROM coach_season_cae csc
            JOIN teams t2 ON t2.natstat_id = csc.team_natstat_id AND t2.season = csc.season
            JOIN team_season_stats tss ON tss.team_id = t2.id AND tss.season = csc.season
            WHERE csc.coach_id = cr.coach_id
        ) st ON TRUE
        WHERE cr.coach_id = $1
        "#,
    )
    .bind(coach_id)
    .fetch_optional(pool)
    .await
}

/// Per-season CAE rows for one coach, oldest → newest (the sparkline order).
pub async fn get_coach_seasons(
    pool: &PgPool,
    coach_id: Uuid,
) -> Result<Vec<CoachSeasonRow>, sqlx::Error> {
    sqlx::query_as::<_, CoachSeasonRow>(
        r#"
        -- Drive off `coach_seasons` (the authoritative roster of every season a
        -- coach worked) rather than `coach_season_cae` (graded seasons only), so
        -- seasons the roster projection couldn't score — heavy-portal rebuilds
        -- below MIN_QUALIFYING_FOR_PROJECTION, e.g. Olen's New Mexico 2026,
        -- McCollum's Drake 2025 — still surface as team entries with the grade
        -- columns left NULL. CAE is descriptive coverage, not a gap in their
        -- career; the detail page renders these rows as "not scored".
        --
        -- DISTINCT ON (season) collapses the coachdict name-variant duplicates
        -- (Shulman, Donahue, Gallagher, …), preferring the row whose team_id
        -- resolved — same dedup the old LATERAL did.
        SELECT DISTINCT ON (cs.season)
            cs.season,
            cs.team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            -- Prefer the actual the grade was computed against; fall back to the
            -- team's own AdjEM so ungraded seasons still show a strength number.
            COALESCE(csc.actual_adjem, ts.adj_efficiency_margin) AS actual_adjem,
            csc.projection,
            csc.cae_raw,
            csc.cae_debiased,
            csc.cae_centered,
            ts.adj_offense,
            ts.adj_defense,
            cs.is_new_hc
        FROM coach_seasons cs
        LEFT JOIN teams t ON t.id = cs.team_id
        -- That season's actual AdjEM / AdjO / AdjD (display-only strength
        -- context); the season-scoped team_id keys straight into the stats.
        LEFT JOIN team_season_stats ts ON ts.team_id = cs.team_id AND ts.season = cs.season
        LEFT JOIN coach_season_cae csc
            ON csc.coach_id = cs.coach_id AND csc.season = cs.season
        WHERE cs.coach_id = $1
          -- Only seasons that have actually been played. The coach ingest also
          -- writes the *upcoming* projection season (max-played + 1, team_id
          -- NULL) so the Future tab can show the incoming HC, but those rows
          -- would render here as a "—" team / "not scored" phantom — exclude
          -- them until the season is played (then they carry a real team_id).
          AND cs.season <= (SELECT MAX(season) FROM games WHERE season IS NOT NULL)
        ORDER BY cs.season, (cs.team_id IS NOT NULL) DESC, cs.coachdict_team_name
        "#,
    )
    .bind(coach_id)
    .fetch_all(pool)
    .await
}

/// The coach of a given (season-scoped) team, plus their career rating if one
/// exists. Powers the dedicated `GET /api/teams/{id}/coach` card route — kept
/// OFF the slow `team_detail` projection-loop path on purpose. Rating fields
/// are `Option` because a coach can lack a `coach_ratings` row.
#[derive(Debug, Serialize, FromRow)]
pub struct TeamCoachCard {
    pub coach_id: Uuid,
    pub name: String,
    /// First season at this team (the PR E new-head-coach flag). NULL when the
    /// prior season's coach is unknown.
    pub is_new_hc: Option<bool>,
    pub cae_shrunk: Option<f64>,
    pub reliability: Option<f64>,
    pub ci_low: Option<f64>,
    pub ci_high: Option<f64>,
    pub n_seasons: Option<i32>,
    pub first_season: Option<i32>,
    pub last_season: Option<i32>,
}

/// Look up the coach for one season-scoped team UUID. `None` when coachdict
/// has no entry for that (team, season) — e.g. an unmatched team-season.
pub async fn get_team_coach(
    pool: &PgPool,
    team_id: Uuid,
) -> Result<Option<TeamCoachCard>, sqlx::Error> {
    sqlx::query_as::<_, TeamCoachCard>(
        r#"
        SELECT
            c.id            AS coach_id,
            c.canonical_name AS name,
            cs.is_new_hc,
            cr.cae_shrunk,
            cr.reliability,
            cr.ci_low,
            cr.ci_high,
            cr.n_seasons,
            cr.first_season,
            cr.last_season
        FROM coach_seasons cs
        JOIN coaches c ON c.id = cs.coach_id
        LEFT JOIN coach_ratings cr ON cr.coach_id = c.id
        WHERE cs.team_id = $1
        -- A team can have two coach_seasons rows (coachdict name variants) that
        -- resolve to the same coach but may differ on is_new_hc; pick
        -- deterministically, preferring a row with a known flag.
        ORDER BY (cs.is_new_hc IS NOT NULL) DESC, cs.coachdict_team_name
        LIMIT 1
        "#,
    )
    .bind(team_id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cae_shrunk: f64, career_adj_em: Option<f64>) -> CoachLeaderboardRow {
        CoachLeaderboardRow {
            coach_id: Uuid::nil(),
            name: String::new(),
            cae_shrunk,
            cae_raw_mean: 0.0,
            cae_adj_shrunk: 0.0,
            cae_centered_shrunk: 0.0,
            reliability: 0.0,
            ci_low: 0.0,
            ci_high: 0.0,
            n_seasons: 0,
            first_season: 0,
            last_season: 0,
            career_adj_em,
            career_adj_o: None,
            career_adj_d: None,
            blend: None,
            last_team_id: None,
            last_team_name: None,
            last_team_season: None,
        }
    }

    #[test]
    fn blend_is_sum_of_two_zscores() {
        // Two coaches, opposite on both axes — the blend is z(CAE)+z(AdjEM).
        // With two symmetric points the population z-scores are ±1 each, so the
        // blend is ±2.
        let mut rows = vec![row(4.0, Some(20.0)), row(-4.0, Some(-20.0))];
        apply_career_blend(&mut rows);
        assert!((rows[0].blend.unwrap() - 2.0).abs() < 1e-9);
        assert!((rows[1].blend.unwrap() + 2.0).abs() < 1e-9);
    }

    #[test]
    fn missing_adjem_contributes_only_cae_term() {
        // The coach with no career AdjEM gets the CAE z-term only (AdjEM term 0).
        let mut rows = vec![row(4.0, None), row(-4.0, Some(-20.0)), row(0.0, Some(20.0))];
        apply_career_blend(&mut rows);
        // CAE population is {4,-4,0}: mean 0, sd = sqrt(32/3). z(4) for row 0.
        let sd_cae = (32.0_f64 / 3.0).sqrt();
        let expected = 4.0 / sd_cae; // AdjEM term is 0 — no career_adj_em.
        assert!((rows[0].blend.unwrap() - expected).abs() < 1e-9);
    }

    fn season_row(cae_raw: f64, actual_adjem: f64) -> CoachSeasonLeaderboardRow {
        CoachSeasonLeaderboardRow {
            coach_id: Uuid::nil(),
            name: String::new(),
            season: 2026,
            team_id: None,
            team_name: None,
            actual_adjem,
            projection: 0.0,
            cae_raw,
            cae_debiased: 0.0,
            cae_centered: 0.0,
            adj_offense: None,
            adj_defense: None,
            blend: None,
            is_new_hc: None,
        }
    }

    #[test]
    fn season_blend_sums_cae_and_adjem_zscores() {
        // actual_adjem is always present on the season board, so both terms
        // contribute for every row. Two symmetric points → blend ±2.
        let mut rows = vec![season_row(8.0, 30.0), season_row(-8.0, -30.0)];
        apply_season_blend(&mut rows);
        assert!((rows[0].blend.unwrap() - 2.0).abs() < 1e-9);
        assert!((rows[1].blend.unwrap() + 2.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_population_leaves_blend_none() {
        // A single row can't be z-scored — blend stays None.
        let mut rows = vec![row(3.0, Some(10.0))];
        apply_career_blend(&mut rows);
        assert!(rows[0].blend.is_none());

        // Zero variance in both terms — every term undefined, blend stays None.
        let mut flat = vec![row(2.0, Some(5.0)), row(2.0, Some(5.0))];
        apply_career_blend(&mut flat);
        assert!(flat.iter().all(|r| r.blend.is_none()));
    }
}
