//! Conference standings — the league view behind the conference cell on the
//! rankings board.
//!
//! Two questions a reader arrives with that the team-level board cannot
//! answer: how did this league's teams finish against each other, and how good
//! is the league. Both are arithmetic over data already stored; neither had a
//! home before now.
//!
//! **Conference records are derived, not stored.** `team_season_stats` carries
//! overall `wins`/`losses` but no conference split, so the record is computed
//! from `games.is_conference` — which `compute_derived_game_fields` sets as
//! "both teams carry the same non-null conference". That predicate is exactly
//! right for this and is complete where it matters: in 2026 it is non-null for
//! every D-I-vs-D-I game, and the 578 nulls are all games where one side has
//! no `teams` row at all (a non-Division-I opponent), which are correctly not
//! conference games.
//!
//! Deriving rather than materialising is deliberate at this size. One league
//! is at most ~20 teams over ~30 games, the aggregate is indexed on
//! `(season, team_id)`, and a stored column would be a fourth place for W-L to
//! disagree with itself — `compute_derived_game_fields` already overwrites
//! overall W-L unconditionally for exactly that reason.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/conferences/{code}", get(conference_detail))
}

#[derive(Deserialize)]
struct ConferenceParams {
    season: Option<i32>,
}

/// One team's line in the standings.
#[derive(Serialize, sqlx::FromRow)]
struct StandingsRow {
    team_id: Uuid,
    team_name: String,
    /// Conference record, derived from `games.is_conference`. Zero-filled
    /// rather than null for a team that has played no conference games yet —
    /// "0-0" is the true answer in November, where a null would read as
    /// missing data.
    conference_wins: i64,
    conference_losses: i64,
    /// Overall record, from `team_season_stats`, which
    /// `compute_derived_game_fields` keeps authoritative.
    wins: i32,
    losses: i32,
    adj_em: Option<f64>,
    /// NATIONAL rank, computed here rather than read: `team_season_stats`
    /// stores `sos_rank` and `elo_rank` but no AdjEM rank, and the rankings
    /// board derives it the same way. National rather than within-conference
    /// because the league table already shows the within-league order — the
    /// useful extra fact is where each team sits in the country.
    adj_em_rank: Option<i64>,
    adj_o: Option<f64>,
    adj_d: Option<f64>,
    sos: Option<f64>,
    sos_rank: Option<i32>,
}

async fn conference_detail(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
    Query(params): Query<ConferenceParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let pool = &state.db.pool;
    let season = params.season.unwrap_or_else(crate::default_season);

    let (teams, strength, non_conference) = tokio::try_join!(
        standings(pool, &code, season),
        conference_strength(pool, &code, season),
        non_conference_record(pool, &code, season),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("conference query failed: {e}") })),
        )
    })?;

    // A code with no teams in this season is a 404 rather than an empty
    // table: the two are different answers, and the realignment years make
    // "this league did not exist yet" a real one.
    if teams.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("no teams in conference {code} for season {season}"),
            })),
        ));
    }

    Ok(Json(json!({
        "conference": code,
        "season": season,
        "teams": teams,
        "mean_adj_em": strength.as_ref().map(|s| s.mean_adj_em),
        "strength_rank": strength.as_ref().map(|s| s.strength_rank),
        "conference_count": strength.as_ref().map(|s| s.conference_count),
        "non_conference_wins": non_conference.0,
        "non_conference_losses": non_conference.1,
    })))
}

/// The standings table, ordered the way a league table is read: conference
/// record first, then AdjEM as the tiebreak.
///
/// AdjEM breaks ties rather than overall record because two teams level in the
/// league are separated by how they played, not by who scheduled easier out of
/// conference — and it is a total order, so the list is stable.
async fn standings(
    pool: &PgPool,
    code: &str,
    season: i32,
) -> Result<Vec<StandingsRow>, sqlx::Error> {
    sqlx::query_as::<_, StandingsRow>(
        r#"
        WITH national AS (
            SELECT team_id,
                   rank() OVER (ORDER BY adj_efficiency_margin DESC) AS adj_em_rank
            FROM team_season_stats
            WHERE season = $2 AND adj_efficiency_margin IS NOT NULL
        ), conf AS (
            SELECT s.team_id,
                   count(*) FILTER (WHERE s.team_score > s.opponent_score) AS w,
                   count(*) FILTER (WHERE s.team_score < s.opponent_score) AS l
            FROM schedules s
            JOIN games g ON g.id = s.game_id
            WHERE s.season = $2
              AND g.is_conference
              AND s.team_score IS NOT NULL
              AND s.opponent_score IS NOT NULL
            GROUP BY s.team_id
        )
        SELECT ts.team_id,
               COALESCE(t.short_name, t.name)      AS team_name,
               COALESCE(c.w, 0)                    AS conference_wins,
               COALESCE(c.l, 0)                    AS conference_losses,
               ts.wins,
               ts.losses,
               ts.adj_efficiency_margin            AS adj_em,
               n.adj_em_rank,
               ts.adj_offense                      AS adj_o,
               ts.adj_defense                      AS adj_d,
               ts.sos,
               ts.sos_rank
        FROM team_season_stats ts
        JOIN teams t ON t.id = ts.team_id
        LEFT JOIN conf c ON c.team_id = ts.team_id
        LEFT JOIN national n ON n.team_id = ts.team_id
        WHERE ts.season = $2 AND t.conference = $1
        ORDER BY COALESCE(c.w, 0) DESC,
                 ts.adj_efficiency_margin DESC NULLS LAST,
                 team_name
        "#,
    )
    .bind(code)
    .bind(season)
    .fetch_all(pool)
    .await
}

struct Strength {
    mean_adj_em: f64,
    strength_rank: i64,
    conference_count: i64,
}

/// Mean AdjEM for this league and where that places it among all of them.
///
/// The mean rather than a top-heavy summary on purpose: a league is its whole
/// membership, and "best team" is already the rankings board's answer.
async fn conference_strength(
    pool: &PgPool,
    code: &str,
    season: i32,
) -> Result<Option<Strength>, sqlx::Error> {
    let row: Option<(f64, i64, i64)> = sqlx::query_as(
        r#"
        WITH per_conf AS (
            SELECT t.conference,
                   avg(ts.adj_efficiency_margin) AS mean_em
            FROM team_season_stats ts
            JOIN teams t ON t.id = ts.team_id
            WHERE ts.season = $2
              AND t.conference IS NOT NULL
              AND ts.adj_efficiency_margin IS NOT NULL
            GROUP BY t.conference
        ), ranked AS (
            SELECT conference, mean_em,
                   rank() OVER (ORDER BY mean_em DESC) AS strength_rank,
                   count(*) OVER ()                    AS conference_count
            FROM per_conf
        )
        SELECT mean_em, strength_rank, conference_count FROM ranked WHERE conference = $1
        "#,
    )
    .bind(code)
    .bind(season)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(mean_adj_em, strength_rank, conference_count)| Strength {
            mean_adj_em,
            strength_rank,
            conference_count,
        }),
    )
}

/// The league's combined record against everyone else — the "how did they do
/// in November" number.
///
/// Counts one row per team per game, so a non-conference game between two
/// teams of THIS league would be double-counted; `is_conference` makes that
/// impossible by construction, since such a game is a conference game.
async fn non_conference_record(
    pool: &PgPool,
    code: &str,
    season: i32,
) -> Result<(i64, i64), sqlx::Error> {
    let row: (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*) FILTER (WHERE s.team_score > s.opponent_score),
               count(*) FILTER (WHERE s.team_score < s.opponent_score)
        FROM schedules s
        JOIN games g ON g.id = s.game_id
        JOIN teams t ON t.id = s.team_id
        WHERE s.season = $2
          AND t.conference = $1
          AND COALESCE(g.is_conference, false) = false
          AND s.team_score IS NOT NULL
          AND s.opponent_score IS NOT NULL
        "#,
    )
    .bind(code)
    .bind(season)
    .fetch_one(pool)
    .await?;
    Ok(row)
}
