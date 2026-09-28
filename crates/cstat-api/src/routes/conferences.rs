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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    /// TEMP `teams` / `games` / `schedules` / `team_season_stats` shadowing
    /// the real tables through `search_path`, on a pinned single-connection
    /// pool, with the isolation asserted rather than assumed.
    ///
    /// Worth the four tables: the conference record is the one number on this
    /// page that exists nowhere in the database, so a future edit to the
    /// derivation has nothing else to disagree with. Against live data it
    /// cross-checks exactly (364 of 364 teams versus an independent count
    /// straight from `games`), but that check does not run anywhere.
    const TEMP_DDL: [&str; 4] = [
        "CREATE TEMP TABLE teams (
             id uuid PRIMARY KEY, natstat_id text, name text, short_name text,
             conference text, season integer)",
        "CREATE TEMP TABLE games (
             id uuid PRIMARY KEY, season integer, is_conference boolean,
             home_team_id uuid, away_team_id uuid, home_score integer, away_score integer)",
        "CREATE TEMP TABLE schedules (
             game_id uuid, team_id uuid, season integer,
             team_score integer, opponent_score integer)",
        "CREATE TEMP TABLE team_season_stats (
             team_id uuid, season integer, wins integer, losses integer,
             adj_efficiency_margin double precision, adj_offense double precision,
             adj_defense double precision, sos double precision, sos_rank integer)",
    ];

    async fn temp_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&url)
            .await
            .expect("connect");
        for ddl in TEMP_DDL {
            sqlx::query(ddl).execute(&pool).await.expect("create temp");
        }
        for t in ["teams", "games", "schedules", "team_season_stats"] {
            let schema: String = sqlx::query_scalar(
                "SELECT n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.oid = $1::regclass",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .expect("resolve");
            assert!(
                schema.starts_with("pg_temp"),
                "refusing to run: `{t}` resolved to `{schema}`, not a temp schema"
            );
        }
        Some(pool)
    }

    /// One game, written the way the pipeline writes it: a `games` row plus
    /// the two `schedules` rows `compute_schedules` derives from it.
    async fn play(pool: &PgPool, home: Uuid, away: Uuid, hs: i32, aws: i32, conf: bool) {
        let gid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO games (id, season, is_conference, home_team_id, away_team_id, home_score, away_score)
             VALUES ($1, 2026, $2, $3, $4, $5, $6)",
        )
        .bind(gid).bind(conf).bind(home).bind(away).bind(hs).bind(aws)
        .execute(pool).await.expect("game");
        for (team, opp, ts, os) in [(home, away, hs, aws), (away, home, aws, hs)] {
            let _ = opp;
            sqlx::query(
                "INSERT INTO schedules (game_id, team_id, season, team_score, opponent_score)
                 VALUES ($1, $2, 2026, $3, $4)",
            )
            .bind(gid)
            .bind(team)
            .bind(ts)
            .bind(os)
            .execute(pool)
            .await
            .expect("schedule");
        }
    }

    async fn add_team(pool: &PgPool, name: &str, conf: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO teams (id, natstat_id, name, short_name, conference, season)
             VALUES ($1, $2, $3, $3, $4, 2026)",
        )
        .bind(id)
        .bind(name)
        .bind(name)
        .bind(conf)
        .execute(pool)
        .await
        .expect("team");
        sqlx::query(
            "INSERT INTO team_season_stats (team_id, season, wins, losses, adj_efficiency_margin)
             VALUES ($1, 2026, 0, 0, 0.0)",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("stats");
        id
    }

    #[tokio::test]
    async fn conference_record_counts_only_league_games_and_balances() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        let a = add_team(&pool, "Alpha", "TEST").await;
        let b = add_team(&pool, "Beta", "TEST").await;
        let outsider = add_team(&pool, "Gamma", "OTHER").await;

        play(&pool, a, b, 80, 70, true).await; // league game, Alpha wins
        play(&pool, b, a, 90, 60, true).await; // league game, Beta wins
        play(&pool, a, outsider, 75, 70, false).await; // non-conference win
        play(&pool, outsider, a, 88, 70, false).await; // non-conference loss

        let rows = standings(&pool, "TEST", 2026).await.expect("standings");
        assert_eq!(rows.len(), 2, "the outsider must not appear in this league");
        let alpha = rows.iter().find(|r| r.team_name == "Alpha").expect("Alpha");

        // The whole point: non-conference games must not reach this record,
        // even though Alpha played four games in total.
        assert_eq!(
            (alpha.conference_wins, alpha.conference_losses),
            (1, 1),
            "1-1 in league play; the two non-conference games are not league games"
        );

        // Structural invariant, true of every real league: a conference game
        // has one winner and one loser and both teams are in the league, so
        // the column sums must match. Verified across all 31 leagues on live
        // 2026 data, and pinned here so it runs.
        let (w, l): (i64, i64) = rows.iter().fold((0, 0), |(w, l), r| {
            (w + r.conference_wins, l + r.conference_losses)
        });
        assert_eq!(w, l, "league conference wins must equal league losses");
    }

    #[tokio::test]
    async fn the_non_conference_record_is_the_league_against_everyone_else() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        let a = add_team(&pool, "Alpha", "TEST").await;
        let b = add_team(&pool, "Beta", "TEST").await;
        let outsider = add_team(&pool, "Gamma", "OTHER").await;

        play(&pool, a, b, 80, 70, true).await; // league game — must be excluded
        play(&pool, a, outsider, 75, 70, false).await; // Alpha wins out
        play(&pool, b, outsider, 60, 90, false).await; // Beta loses out

        let (w, l) = non_conference_record(&pool, "TEST", 2026)
            .await
            .expect("non-conf");
        assert_eq!(
            (w, l),
            (1, 1),
            "one win and one loss outside the league; the league game is not counted"
        );

        // It counts one row per TEAM per game, so an intra-league game would
        // be double-counted if it ever reached here. `is_conference` makes
        // that impossible by construction — such a game is a conference game
        // — and this is the assertion that would catch the predicate being
        // loosened.
        assert_eq!(
            w + l,
            2,
            "each non-conference game contributes exactly once"
        );
    }
}
