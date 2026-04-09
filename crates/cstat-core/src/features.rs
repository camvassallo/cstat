use sqlx::PgPool;
use uuid::Uuid;

use crate::inference::NUM_FEATURES;

/// Team-level stats pulled from `team_season_stats`.
#[derive(Debug, sqlx::FromRow)]
struct TeamStats {
    wins: i32,
    losses: i32,
    adj_offense: Option<f64>,
    adj_defense: Option<f64>,
    adj_efficiency_margin: Option<f64>,
    effective_fg_pct: Option<f64>,
    turnover_pct: Option<f64>,
    off_rebound_pct: Option<f64>,
    ft_rate: Option<f64>,
    opp_effective_fg_pct: Option<f64>,
    opp_turnover_pct: Option<f64>,
    def_rebound_pct: Option<f64>,
    opp_ft_rate: Option<f64>,
    adj_tempo: Option<f64>,
    sos: Option<f64>,
    elo_rating: Option<f64>,
    point_diff: Option<f64>,
    pythag_win_pct: Option<f64>,
    road_win_pct: Option<f64>,
}

impl TeamStats {
    fn win_pct(&self) -> f64 {
        let total = self.wins + self.losses;
        if total == 0 {
            0.5
        } else {
            self.wins as f64 / total as f64
        }
    }
}

/// Minutes-weighted roster aggregates from `player_season_stats`.
#[derive(Debug, sqlx::FromRow)]
struct RosterAgg {
    roster_size: Option<i64>,
    w_ppg: Option<f64>,
    w_rpg: Option<f64>,
    w_apg: Option<f64>,
    w_spg: Option<f64>,
    w_bpg: Option<f64>,
    w_topg: Option<f64>,
    w_ts_pct: Option<f64>,
    w_efg_pct: Option<f64>,
    w_usage: Option<f64>,
    w_bpm: Option<f64>,
    w_player_sos: Option<f64>,
    w_obpm: Option<f64>,
    w_dbpm: Option<f64>,
    w_ortg: Option<f64>,
    w_ast_pct: Option<f64>,
    w_tov_pct: Option<f64>,
    w_stl_pct: Option<f64>,
    w_blk_pct: Option<f64>,
    star_ppg: Option<f64>,
    star_bpm: Option<f64>,
    star_ortg: Option<f64>,
    minutes_stddev: Option<f64>,
}

/// Rolling form aggregates from recent `player_game_stats`.
#[derive(Debug, sqlx::FromRow)]
struct RollingForm {
    w_rolling_gs: Option<f64>,
    w_rolling_ts: Option<f64>,
    w_ppg_trend: Option<f64>,
    w_gs_trend: Option<f64>,
}

async fn get_team_stats(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<TeamStats, sqlx::Error> {
    sqlx::query_as::<_, TeamStats>(
        r#"
        SELECT wins, losses,
               adj_offense, adj_defense, adj_efficiency_margin,
               effective_fg_pct, turnover_pct, off_rebound_pct, ft_rate,
               opp_effective_fg_pct, opp_turnover_pct, def_rebound_pct, opp_ft_rate,
               adj_tempo, sos, elo_rating,
               point_diff, pythag_win_pct, road_win_pct
        FROM team_season_stats
        WHERE team_id = $1 AND season = $2
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_one(pool)
    .await
}

async fn get_roster_agg(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<RosterAgg, sqlx::Error> {
    // Minutes-weighted averages across players with >= 5 games and >= 10 mpg.
    // Star player = highest minutes_per_game on the team.
    sqlx::query_as::<_, RosterAgg>(
        r#"
        WITH qualified AS (
            SELECT *,
                   minutes_per_game * games_played AS total_minutes
            FROM player_season_stats
            WHERE team_id = $1
              AND season = $2
              AND games_played >= 5
              AND minutes_per_game >= 10
        ),
        star AS (
            SELECT ppg AS star_ppg,
                   bpm AS star_bpm,
                   offensive_rating AS star_ortg
            FROM qualified
            ORDER BY total_minutes DESC
            LIMIT 1
        ),
        agg AS (
            SELECT
                COUNT(*)::bigint AS roster_size,
                SUM(ppg * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_ppg,
                SUM(rpg * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_rpg,
                SUM(apg * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_apg,
                SUM(spg * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_spg,
                SUM(bpg * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_bpg,
                SUM(topg * total_minutes)          / NULLIF(SUM(total_minutes), 0) AS w_topg,
                SUM(true_shooting_pct * total_minutes)  / NULLIF(SUM(total_minutes), 0) AS w_ts_pct,
                SUM(effective_fg_pct * total_minutes)   / NULLIF(SUM(total_minutes), 0) AS w_efg_pct,
                SUM(usage_rate * total_minutes)    / NULLIF(SUM(total_minutes), 0) AS w_usage,
                SUM(bpm * total_minutes)           / NULLIF(SUM(total_minutes), 0) AS w_bpm,
                SUM(player_sos * total_minutes)    / NULLIF(SUM(total_minutes), 0) AS w_player_sos,
                SUM(obpm * total_minutes)          / NULLIF(SUM(total_minutes), 0) AS w_obpm,
                SUM(dbpm * total_minutes)          / NULLIF(SUM(total_minutes), 0) AS w_dbpm,
                SUM(offensive_rating * total_minutes)   / NULLIF(SUM(total_minutes), 0) AS w_ortg,
                SUM(ast_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_ast_pct,
                SUM(tov_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_tov_pct,
                SUM(stl_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_stl_pct,
                SUM(blk_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_blk_pct,
                STDDEV(minutes_per_game) AS minutes_stddev
            FROM qualified
        )
        SELECT agg.*, star.*
        FROM agg CROSS JOIN star
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_one(pool)
    .await
}

async fn get_rolling_form(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<RollingForm, sqlx::Error> {
    // Get the most recent rolling stats for each player on the team,
    // then compute minutes-weighted team averages.
    // Also compute trends: rolling - season average.
    sqlx::query_as::<_, RollingForm>(
        r#"
        WITH latest_games AS (
            -- Most recent game per player on this team
            SELECT DISTINCT ON (player_id)
                   player_id, minutes,
                   rolling_game_score, rolling_ts_pct, rolling_ppg,
                   game_score
            FROM player_game_stats
            WHERE team_id = $1
              AND season = $2
              AND minutes IS NOT NULL
              AND minutes > 0
            ORDER BY player_id, game_date DESC
        ),
        season_avg AS (
            SELECT player_id,
                   AVG(game_score) AS avg_gs,
                   AVG(CASE WHEN points > 0 THEN
                       points::double precision / NULLIF(2.0 * (fga + 0.44 * fta), 0)
                   END) AS avg_ts,
                   AVG(points::double precision) AS avg_ppg
            FROM player_game_stats
            WHERE team_id = $1
              AND season = $2
              AND minutes IS NOT NULL
              AND minutes > 0
            GROUP BY player_id
        )
        SELECT
            SUM(lg.rolling_game_score * lg.minutes) / NULLIF(SUM(lg.minutes), 0) AS w_rolling_gs,
            SUM(lg.rolling_ts_pct * lg.minutes)     / NULLIF(SUM(lg.minutes), 0) AS w_rolling_ts,
            SUM((lg.rolling_ppg - sa.avg_ppg) * lg.minutes)
                / NULLIF(SUM(lg.minutes), 0) AS w_ppg_trend,
            SUM((lg.rolling_game_score - sa.avg_gs) * lg.minutes)
                / NULLIF(SUM(lg.minutes), 0) AS w_gs_trend
        FROM latest_games lg
        JOIN season_avg sa USING (player_id)
        WHERE lg.rolling_game_score IS NOT NULL
        "#,
    )
    .bind(team_id)
    .bind(season)
    .fetch_one(pool)
    .await
}

/// Build the 47-element feature vector for a matchup.
///
/// Features are home − away differences, matching the order in `model_meta.json`.
pub async fn build_game_features(
    pool: &PgPool,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
) -> Result<[f32; NUM_FEATURES], sqlx::Error> {
    // Fetch all data in parallel
    let (home_ts, away_ts, home_roster, away_roster, home_form, away_form) = tokio::try_join!(
        get_team_stats(pool, home_team_id, season),
        get_team_stats(pool, away_team_id, season),
        get_roster_agg(pool, home_team_id, season),
        get_roster_agg(pool, away_team_id, season),
        get_rolling_form(pool, home_team_id, season),
        get_rolling_form(pool, away_team_id, season),
    )?;

    let d = |home: Option<f64>, away: Option<f64>| -> f32 {
        (home.unwrap_or(0.0) - away.unwrap_or(0.0)) as f32
    };

    let features: [f32; NUM_FEATURES] = [
        // Context
        if is_neutral { 0.0 } else { 1.0 },             // venue
        if is_conference { 1.0 } else { 0.0 },          // is_conference_game
        (home_ts.win_pct() - away_ts.win_pct()) as f32, // diff_win_pct
        // Adjusted efficiency
        d(home_ts.adj_offense, away_ts.adj_offense),
        -d(home_ts.adj_defense, away_ts.adj_defense), // flipped: lower defense = better
        d(home_ts.adj_efficiency_margin, away_ts.adj_efficiency_margin),
        // Four factors (offense)
        d(home_ts.effective_fg_pct, away_ts.effective_fg_pct),
        d(home_ts.turnover_pct, away_ts.turnover_pct),
        d(home_ts.off_rebound_pct, away_ts.off_rebound_pct),
        d(home_ts.ft_rate, away_ts.ft_rate),
        // Four factors (defense) — flipped
        -d(home_ts.opp_effective_fg_pct, away_ts.opp_effective_fg_pct),
        d(home_ts.opp_turnover_pct, away_ts.opp_turnover_pct),
        d(home_ts.def_rebound_pct, away_ts.def_rebound_pct),
        -d(home_ts.opp_ft_rate, away_ts.opp_ft_rate),
        // Tempo & power
        d(home_ts.adj_tempo, away_ts.adj_tempo),
        d(home_ts.sos, away_ts.sos),
        d(home_ts.elo_rating, away_ts.elo_rating),
        d(home_ts.point_diff, away_ts.point_diff),
        d(home_ts.pythag_win_pct, away_ts.pythag_win_pct),
        d(home_ts.road_win_pct, away_ts.road_win_pct),
        // Roster box score
        d(
            home_roster.roster_size.map(|v| v as f64),
            away_roster.roster_size.map(|v| v as f64),
        ),
        d(home_roster.w_ppg, away_roster.w_ppg),
        d(home_roster.w_rpg, away_roster.w_rpg),
        d(home_roster.w_apg, away_roster.w_apg),
        d(home_roster.w_spg, away_roster.w_spg),
        d(home_roster.w_bpg, away_roster.w_bpg),
        d(home_roster.w_topg, away_roster.w_topg),
        d(home_roster.w_ts_pct, away_roster.w_ts_pct),
        d(home_roster.w_efg_pct, away_roster.w_efg_pct),
        // Roster advanced
        d(home_roster.w_usage, away_roster.w_usage),
        d(home_roster.w_bpm, away_roster.w_bpm),
        d(home_roster.w_player_sos, away_roster.w_player_sos),
        d(home_roster.w_obpm, away_roster.w_obpm),
        d(home_roster.w_dbpm, away_roster.w_dbpm),
        d(home_roster.w_ortg, away_roster.w_ortg),
        d(home_roster.w_ast_pct, away_roster.w_ast_pct),
        d(home_roster.w_tov_pct, away_roster.w_tov_pct),
        d(home_roster.w_stl_pct, away_roster.w_stl_pct),
        d(home_roster.w_blk_pct, away_roster.w_blk_pct),
        // Star power
        d(home_roster.star_ppg, away_roster.star_ppg),
        d(home_roster.star_bpm, away_roster.star_bpm),
        d(home_roster.star_ortg, away_roster.star_ortg),
        // Depth
        d(home_roster.minutes_stddev, away_roster.minutes_stddev),
        // Rolling form
        d(home_form.w_rolling_gs, away_form.w_rolling_gs),
        d(home_form.w_rolling_ts, away_form.w_rolling_ts),
        d(home_form.w_ppg_trend, away_form.w_ppg_trend),
        d(home_form.w_gs_trend, away_form.w_gs_trend),
    ];

    Ok(features)
}
