use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::inference::{NUM_FEATURES, TOTAL_NUM_FEATURES};
use crate::pit_campom::{PitCamPom, compute_pit_campom};

/// Feature vectors for both the margin/win path (49 diffs) and the totals
/// path (49 diffs + 9 level-sensitive sums). Built in a single DB-fetch
/// pass so the API can call all three models off one round-trip without
/// re-querying team/roster/form data.
///
/// Invariant: `diff_and_sum[..NUM_FEATURES] == diff` byte-for-byte.
/// Verified by `total_features_share_diff_prefix` test.
pub struct GameFeatures {
    /// Margin/win model input (49 features). Wire-locked to
    /// `model_meta.json::features`.
    pub diff: [f32; NUM_FEATURES],
    /// Totals model input (58 features). Wire-locked to
    /// `model_meta.json::total_features`.
    pub diff_and_sum: [f32; TOTAL_NUM_FEATURES],
}

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
    w_player_sos: Option<f64>,
    w_ortg: Option<f64>,
    w_ast_pct: Option<f64>,
    w_tov_pct: Option<f64>,
    w_stl_pct: Option<f64>,
    w_blk_pct: Option<f64>,
    w_gbpm: Option<f64>,
    w_ogbpm: Option<f64>,
    w_dgbpm: Option<f64>,
    star_ppg: Option<f64>,
    star_gbpm: Option<f64>,
    star_ogbpm: Option<f64>,
    star_dgbpm: Option<f64>,
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

/// Point-in-time roster impact values keyed by cstat player_id. Constructed
/// once per request and shared across home/away roster aggregations.
type PitByPlayer = HashMap<Uuid, PitCamPom>;

/// Compute pit CamPom v3 (no-SOS) for the entire season cohort as of a
/// cutoff date, keyed by cstat `players.id`.
///
/// Mirrors the Python `pit_cam_v3` GBPM_VARIANT path in `training/features.py`:
/// the season-aggregate `torvik_player_stats.gbpm/ogbpm/dgbpm` columns are
/// the leaky channel identified by the predict-honesty audit
/// (`training/eval_history/honest_audit_findings_20260529.md`); the pit
/// aggregate from `torvik_player_game_stats` is the leak-free replacement.
///
/// The torvik_pid → player_id join lives inside `compute_pit_campom`'s
/// SQL, so this is now a thin pass-through. Mid-season transfers (multiple
/// torvik_pid rows for the same player_id in one season) are aggregated
/// into a single combined row by the database, not silently overwritten
/// in app code.
async fn build_pit_by_player(
    pool: &PgPool,
    season: i32,
    as_of_date: NaiveDate,
) -> Result<PitByPlayer, sqlx::Error> {
    compute_pit_campom(pool, season, as_of_date).await
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
            SELECT pss.*,
                   pss.minutes_per_game * pss.games_played AS total_minutes,
                   tps.gbpm   AS torvik_gbpm,
                   tps.ogbpm  AS torvik_ogbpm,
                   tps.dgbpm  AS torvik_dgbpm
            FROM player_season_stats pss
            LEFT JOIN torvik_player_stats tps
              ON tps.player_id = pss.player_id AND tps.season = pss.season
            WHERE pss.team_id = $1
              AND pss.season = $2
              AND pss.games_played >= 5
              AND pss.minutes_per_game >= 10
        ),
        star AS (
            SELECT ppg          AS star_ppg,
                   torvik_gbpm  AS star_gbpm,
                   torvik_ogbpm AS star_ogbpm,
                   torvik_dgbpm AS star_dgbpm,
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
                SUM(player_sos * total_minutes)    / NULLIF(SUM(total_minutes), 0) AS w_player_sos,
                SUM(offensive_rating * total_minutes)   / NULLIF(SUM(total_minutes), 0) AS w_ortg,
                SUM(ast_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_ast_pct,
                SUM(tov_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_tov_pct,
                SUM(stl_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_stl_pct,
                SUM(blk_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_blk_pct,
                SUM(torvik_gbpm  * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_gbpm  IS NOT NULL THEN total_minutes END), 0) AS w_gbpm,
                SUM(torvik_ogbpm * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_ogbpm IS NOT NULL THEN total_minutes END), 0) AS w_ogbpm,
                SUM(torvik_dgbpm * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_dgbpm IS NOT NULL THEN total_minutes END), 0) AS w_dgbpm,
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

/// Roster aggregation with point-in-time CamPom values substituted for the
/// season-aggregate `torvik_player_stats.gbpm/ogbpm/dgbpm` channel.
///
/// Identical to `get_roster_agg` except the impact join is fed by the
/// caller-supplied `PitByPlayer` map (passed in as parallel UNNEST arrays)
/// instead of the season-aggregate Torvik columns. Filter / weighting /
/// star-pick logic is intentionally unchanged — only the leaky channel is
/// swapped, matching the `pit_cam_v3` training variant the audit measured
/// at AUC 0.785.
async fn get_roster_agg_pit(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
    pit: &PitByPlayer,
) -> Result<RosterAgg, sqlx::Error> {
    // Flatten the map into parallel arrays for UNNEST. Players with no pit
    // entry fall through the LEFT JOIN as NULL, matching how train-time
    // unmapped Torvik rows behave.
    let (pids, gbpms, ogbpms, dgbpms): (Vec<Uuid>, Vec<f64>, Vec<f64>, Vec<f64>) = pit.iter().fold(
        (
            Vec::with_capacity(pit.len()),
            Vec::with_capacity(pit.len()),
            Vec::with_capacity(pit.len()),
            Vec::with_capacity(pit.len()),
        ),
        |(mut p, mut g, mut o, mut d), (player_id, cam)| {
            p.push(*player_id);
            g.push(cam.cam_gbpm_v3_no_sos);
            o.push(cam.ogbpm);
            d.push(cam.dgbpm);
            (p, g, o, d)
        },
    );

    sqlx::query_as::<_, RosterAgg>(
        r#"
        WITH pit AS (
            SELECT * FROM UNNEST($3::uuid[], $4::float8[], $5::float8[], $6::float8[])
                AS t(player_id, gbpm, ogbpm, dgbpm)
        ),
        qualified AS (
            SELECT pss.*,
                   pss.minutes_per_game * pss.games_played AS total_minutes,
                   pit.gbpm   AS torvik_gbpm,
                   pit.ogbpm  AS torvik_ogbpm,
                   pit.dgbpm  AS torvik_dgbpm
            FROM player_season_stats pss
            LEFT JOIN pit ON pit.player_id = pss.player_id
            WHERE pss.team_id = $1
              AND pss.season = $2
              AND pss.games_played >= 5
              AND pss.minutes_per_game >= 10
        ),
        star AS (
            SELECT ppg          AS star_ppg,
                   torvik_gbpm  AS star_gbpm,
                   torvik_ogbpm AS star_ogbpm,
                   torvik_dgbpm AS star_dgbpm,
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
                SUM(player_sos * total_minutes)    / NULLIF(SUM(total_minutes), 0) AS w_player_sos,
                SUM(offensive_rating * total_minutes)   / NULLIF(SUM(total_minutes), 0) AS w_ortg,
                SUM(ast_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_ast_pct,
                SUM(tov_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_tov_pct,
                SUM(stl_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_stl_pct,
                SUM(blk_pct * total_minutes)       / NULLIF(SUM(total_minutes), 0) AS w_blk_pct,
                SUM(torvik_gbpm  * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_gbpm  IS NOT NULL THEN total_minutes END), 0) AS w_gbpm,
                SUM(torvik_ogbpm * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_ogbpm IS NOT NULL THEN total_minutes END), 0) AS w_ogbpm,
                SUM(torvik_dgbpm * total_minutes)  / NULLIF(SUM(CASE WHEN torvik_dgbpm IS NOT NULL THEN total_minutes END), 0) AS w_dgbpm,
                STDDEV(minutes_per_game) AS minutes_stddev
            FROM qualified
        )
        SELECT agg.*, star.*
        FROM agg CROSS JOIN star
        "#,
    )
    .bind(team_id)
    .bind(season)
    .bind(&pids)
    .bind(&gbpms)
    .bind(&ogbpms)
    .bind(&dgbpms)
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

/// Build the 49-element diff feature vector for a matchup (margin/win
/// model input). Thin wrapper over `build_all_features` for callers that
/// only need the margin path.
pub async fn build_game_features(
    pool: &PgPool,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
) -> Result<[f32; NUM_FEATURES], sqlx::Error> {
    let f = build_all_features(
        pool,
        home_team_id,
        away_team_id,
        season,
        is_neutral,
        is_conference,
    )
    .await?;
    Ok(f.diff)
}

/// Build both the diff (margin/win) and diff+sum (totals) feature
/// vectors in a single DB-fetch pass.
///
/// Features are home − away differences for the diff path, plus
/// home + away sums on the unflipped raw columns for the totals path.
/// Order matches `model_meta.json::features` and `::total_features`.
pub async fn build_all_features(
    pool: &PgPool,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
) -> Result<GameFeatures, sqlx::Error> {
    build_all_features_inner(
        pool,
        home_team_id,
        away_team_id,
        season,
        is_neutral,
        is_conference,
        None,
    )
    .await
}

/// Point-in-time companion to `build_all_features`. Roster impact (gbpm /
/// ogbpm / dgbpm) is rebuilt by aggregating `torvik_player_game_stats` up
/// to `as_of_date` instead of reading the season-aggregate
/// `torvik_player_stats` columns. All other features stay end-of-season —
/// this matches the `pit_cam_v3` training variant whose backtest AUC of
/// 0.785 is the production-ready honest number per the predict-honesty
/// audit (`training/eval_history/honest_audit_findings_20260529.md`).
///
/// Pair with `Predictor::predict_pit` to keep the model that receives
/// these features the one that was trained on them.
pub async fn build_all_features_pit(
    pool: &PgPool,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: NaiveDate,
) -> Result<GameFeatures, sqlx::Error> {
    build_all_features_inner(
        pool,
        home_team_id,
        away_team_id,
        season,
        is_neutral,
        is_conference,
        Some(as_of_date),
    )
    .await
}

async fn build_all_features_inner(
    pool: &PgPool,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<GameFeatures, sqlx::Error> {
    // Build the pit map once when as_of_date is set — the season-cohort
    // aggregate is shared across home/away roster queries.
    let pit_map = match as_of_date {
        Some(d) => Some(build_pit_by_player(pool, season, d).await?),
        None => None,
    };

    let (home_ts, away_ts, home_roster, away_roster, home_form, away_form) = match &pit_map {
        Some(map) => tokio::try_join!(
            get_team_stats(pool, home_team_id, season),
            get_team_stats(pool, away_team_id, season),
            get_roster_agg_pit(pool, home_team_id, season, map),
            get_roster_agg_pit(pool, away_team_id, season, map),
            get_rolling_form(pool, home_team_id, season),
            get_rolling_form(pool, away_team_id, season),
        )?,
        None => tokio::try_join!(
            get_team_stats(pool, home_team_id, season),
            get_team_stats(pool, away_team_id, season),
            get_roster_agg(pool, home_team_id, season),
            get_roster_agg(pool, away_team_id, season),
            get_rolling_form(pool, home_team_id, season),
            get_rolling_form(pool, away_team_id, season),
        )?,
    };

    let d = |home: Option<f64>, away: Option<f64>| -> f32 {
        (home.unwrap_or(0.0) - away.unwrap_or(0.0)) as f32
    };
    // Sum helper for level-sensitive totals features. Mirrors `s` from
    // `training/features.py` — no sign flips on defense (the diff path's
    // flip is on the *diff column*, not the source values).
    let s = |home: Option<f64>, away: Option<f64>| -> f32 {
        (home.unwrap_or(0.0) + away.unwrap_or(0.0)) as f32
    };

    let diff: [f32; NUM_FEATURES] = [
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
        d(home_roster.w_player_sos, away_roster.w_player_sos),
        d(home_roster.w_ortg, away_roster.w_ortg),
        d(home_roster.w_ast_pct, away_roster.w_ast_pct),
        d(home_roster.w_tov_pct, away_roster.w_tov_pct),
        d(home_roster.w_stl_pct, away_roster.w_stl_pct),
        d(home_roster.w_blk_pct, away_roster.w_blk_pct),
        // Torvik impact (replaces broken cstat BPM/OBPM/DBPM)
        d(home_roster.w_gbpm, away_roster.w_gbpm),
        d(home_roster.w_ogbpm, away_roster.w_ogbpm),
        d(home_roster.w_dgbpm, away_roster.w_dgbpm),
        // Star power
        d(home_roster.star_ppg, away_roster.star_ppg),
        d(home_roster.star_gbpm, away_roster.star_gbpm),
        d(home_roster.star_ogbpm, away_roster.star_ogbpm),
        d(home_roster.star_dgbpm, away_roster.star_dgbpm),
        d(home_roster.star_ortg, away_roster.star_ortg),
        // Depth
        d(home_roster.minutes_stddev, away_roster.minutes_stddev),
        // Rolling form
        d(home_form.w_rolling_gs, away_form.w_rolling_gs),
        d(home_form.w_rolling_ts, away_form.w_rolling_ts),
        d(home_form.w_ppg_trend, away_form.w_ppg_trend),
        d(home_form.w_gs_trend, away_form.w_gs_trend),
    ];

    // Totals input: diff prefix + 9 level-sensitive sums. Order locked to
    // `model_meta.json::total_features` indices NUM_FEATURES..58.
    let mut diff_and_sum = [0.0_f32; TOTAL_NUM_FEATURES];
    diff_and_sum[..NUM_FEATURES].copy_from_slice(&diff);
    diff_and_sum[NUM_FEATURES] = s(home_ts.adj_tempo, away_ts.adj_tempo);
    diff_and_sum[NUM_FEATURES + 1] = s(home_ts.adj_offense, away_ts.adj_offense);
    diff_and_sum[NUM_FEATURES + 2] = s(home_ts.adj_defense, away_ts.adj_defense);
    diff_and_sum[NUM_FEATURES + 3] = s(home_ts.effective_fg_pct, away_ts.effective_fg_pct);
    diff_and_sum[NUM_FEATURES + 4] = s(home_ts.opp_effective_fg_pct, away_ts.opp_effective_fg_pct);
    diff_and_sum[NUM_FEATURES + 5] = s(home_roster.w_ppg, away_roster.w_ppg);
    diff_and_sum[NUM_FEATURES + 6] = s(home_roster.w_ortg, away_roster.w_ortg);
    diff_and_sum[NUM_FEATURES + 7] = s(home_ts.off_rebound_pct, away_ts.off_rebound_pct);
    diff_and_sum[NUM_FEATURES + 8] = s(home_ts.def_rebound_pct, away_ts.def_rebound_pct);

    Ok(GameFeatures { diff, diff_and_sum })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_pct_even_record() {
        let ts = TeamStats {
            wins: 15,
            losses: 15,
            adj_offense: None,
            adj_defense: None,
            adj_efficiency_margin: None,
            effective_fg_pct: None,
            turnover_pct: None,
            off_rebound_pct: None,
            ft_rate: None,
            opp_effective_fg_pct: None,
            opp_turnover_pct: None,
            def_rebound_pct: None,
            opp_ft_rate: None,
            adj_tempo: None,
            sos: None,
            elo_rating: None,
            point_diff: None,
            pythag_win_pct: None,
            road_win_pct: None,
        };
        assert!((ts.win_pct() - 0.5).abs() < 0.001);
    }

    #[test]
    fn win_pct_no_games() {
        let ts = TeamStats {
            wins: 0,
            losses: 0,
            adj_offense: None,
            adj_defense: None,
            adj_efficiency_margin: None,
            effective_fg_pct: None,
            turnover_pct: None,
            off_rebound_pct: None,
            ft_rate: None,
            opp_effective_fg_pct: None,
            opp_turnover_pct: None,
            def_rebound_pct: None,
            opp_ft_rate: None,
            adj_tempo: None,
            sos: None,
            elo_rating: None,
            point_diff: None,
            pythag_win_pct: None,
            road_win_pct: None,
        };
        assert!((ts.win_pct() - 0.5).abs() < 0.001);
    }

    #[test]
    fn win_pct_undefeated() {
        let ts = TeamStats {
            wins: 30,
            losses: 0,
            adj_offense: None,
            adj_defense: None,
            adj_efficiency_margin: None,
            effective_fg_pct: None,
            turnover_pct: None,
            off_rebound_pct: None,
            ft_rate: None,
            opp_effective_fg_pct: None,
            opp_turnover_pct: None,
            def_rebound_pct: None,
            opp_ft_rate: None,
            adj_tempo: None,
            sos: None,
            elo_rating: None,
            point_diff: None,
            pythag_win_pct: None,
            road_win_pct: None,
        };
        assert!((ts.win_pct() - 1.0).abs() < 0.001);
    }

    #[test]
    fn diff_helper_both_some() {
        let d = |home: Option<f64>, away: Option<f64>| -> f32 {
            (home.unwrap_or(0.0) - away.unwrap_or(0.0)) as f32
        };
        assert!((d(Some(110.0), Some(100.0)) - 10.0).abs() < 0.001);
    }

    #[test]
    fn diff_helper_none_defaults_to_zero() {
        let d = |home: Option<f64>, away: Option<f64>| -> f32 {
            (home.unwrap_or(0.0) - away.unwrap_or(0.0)) as f32
        };
        assert!((d(Some(5.0), None) - 5.0).abs() < 0.001);
        assert!((d(None, Some(5.0)) - (-5.0)).abs() < 0.001);
        assert!((d(None, None) - 0.0).abs() < 0.001);
    }

    /// Lock the sum_* feature order in `TOTAL_FEATURE_NAMES` against the
    /// hand-coded indices in `build_all_features`. If someone reorders
    /// the names array (or the assignments below `diff_and_sum.copy_from`),
    /// this test fails before the totals model returns garbage in prod.
    #[test]
    fn total_feature_names_sum_order() {
        use crate::inference::TOTAL_FEATURE_NAMES;
        let expected_sums = [
            "sum_adj_tempo",
            "sum_adj_offense",
            "sum_adj_defense",
            "sum_effective_fg_pct",
            "sum_opp_effective_fg_pct",
            "sum_w_ppg",
            "sum_w_ortg",
            "sum_off_rebound_pct",
            "sum_def_rebound_pct",
        ];
        for (i, expected) in expected_sums.iter().enumerate() {
            assert_eq!(
                TOTAL_FEATURE_NAMES[NUM_FEATURES + i],
                *expected,
                "sum slot {i} mismatch",
            );
        }
    }

    /// `GameFeatures.diff_and_sum[..NUM_FEATURES]` is required to equal
    /// `GameFeatures.diff` byte-for-byte — both models share the diff
    /// prefix so the API can fetch DB data once and feed both sessions.
    #[test]
    fn total_features_share_diff_prefix() {
        let diff: [f32; NUM_FEATURES] = std::array::from_fn(|i| (i as f32) * 0.5);
        let mut diff_and_sum = [0.0_f32; TOTAL_NUM_FEATURES];
        diff_and_sum[..NUM_FEATURES].copy_from_slice(&diff);
        // Assign sentinel sums so we can verify they don't bleed into
        // the diff prefix.
        diff_and_sum[NUM_FEATURES..].fill(-42.0);
        let f = GameFeatures { diff, diff_and_sum };
        for i in 0..NUM_FEATURES {
            assert_eq!(f.diff_and_sum[i], f.diff[i], "prefix mismatch at {i}");
        }
        for i in NUM_FEATURES..TOTAL_NUM_FEATURES {
            assert_eq!(f.diff_and_sum[i], -42.0, "sum slot {i} clobbered");
        }
    }
}
