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
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use cstat_core::projection::{self, Venue};

use crate::AppState;
use crate::routes::projections::fetch_conferences;

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
    /// Expected record for a season not yet played: the sum of this team's
    /// per-game win probabilities over the games the schedule currently
    /// carries. Null on a played season, where the real record is the answer.
    ///
    /// An **expectation, not a simulation**. Summing independent win
    /// probabilities gives the mean number of wins; it cannot express "wins
    /// the league", and must not be presented as if it could.
    #[sqlx(default)]
    expected_conference_wins: Option<f64>,
    #[sqlx(default)]
    expected_conference_games: Option<i64>,
    #[sqlx(default)]
    expected_wins: Option<f64>,
    /// How many of this team's scheduled games carry a projection. Shown
    /// because NatStat publishes the slate in pieces — an expected record
    /// over a third of a season is a real number about a partial schedule,
    /// and reads as a whole-season claim if the denominator is hidden.
    #[sqlx(default)]
    projected_games: Option<i64>,
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

    // A season nobody has played has no standings to report, but it does have
    // a projection — and between April and tip-off that is the only table
    // there is. Served from the same per-game arithmetic `/api/predict` and
    // the team page use, with no second model: each game's win probability is
    // `preseason_only_win_prob` over the two anchors, and a team's expected
    // record is their sum.
    if !season_has_played_games(pool, season).await {
        return projected_standings(pool, &code, season).await;
    }

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

/// One row of the target season's schedule, in the shape the expected-record
/// loop needs: who, against whom, and where.
#[derive(sqlx::FromRow)]
struct ScheduleSlot {
    team_id: Uuid,
    opponent_id: Option<Uuid>,
    is_home: Option<bool>,
    is_neutral: Option<bool>,
}

/// Has any game in this season been played to a final score?
///
/// Season-level rather than per-team, unlike the team page's gate: a league
/// table is a statement about a whole season, so a single played game means
/// the real standings have started and are the better answer for everyone in
/// it. Errs toward `true` on a query failure, which routes to the played
/// path and its own honest errors.
async fn season_has_played_games(pool: &PgPool, season: i32) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM games \
         WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL)",
    )
    .bind(season)
    .fetch_one(pool)
    .await
    .unwrap_or(true)
}

/// The league table for a season that has not been played.
///
/// Three things have to come from somewhere other than the played path:
///
/// * **Membership.** The target season's `teams.conference` is NULL until
///   Torvik publishes, so the league is resolved from the base season plus
///   `data/conference_realignment.json` — reusing `fetch_conferences`, whose
///   precedence rule is subtle enough (an ingested value that still reports
///   the league a team *left* loses to the capture) that a second copy would
///   drift.
/// * **The conference slate.** `games.is_conference` is useless here: it is
///   `false` for all 1,709 ingested 2027 games, because it is computed as
///   "both teams carry the same non-null conference" and both are NULL. Using
///   it would give every team a zero-game league schedule. Membership from
///   the resolved map decides which games are league games instead.
/// * **The record.** The sum of per-game win probabilities. No new model —
///   `preseason_only_margin` is the same function the Predict page and the
///   team-page schedule serve, so a game cannot read differently here.
async fn projected_standings(
    pool: &PgPool,
    code: &str,
    season: i32,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let base_season = season - 1;
    let err = |e: sqlx::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("projected standings query failed: {e}") })),
        )
    };

    // `fetch_conferences` keys on BASE-season team ids, and everything else
    // here is keyed on the target season's, so the two are bridged through
    // `natstat_id` — `teams.id` is season-scoped and cannot join across.
    let conferences = fetch_conferences(pool, base_season, season)
        .await
        .map_err(err)?;
    let links: Vec<(Uuid, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT t.id, tgt.id, COALESCE(tgt.short_name, tgt.name, t.short_name, t.name) \
         FROM teams t \
         LEFT JOIN teams tgt ON tgt.natstat_id = t.natstat_id AND tgt.season = $2 \
         WHERE t.season = $1",
    )
    .bind(base_season)
    .bind(season)
    .fetch_all(pool)
    .await
    .map_err(err)?;

    // Target-season team id -> (name, resolved conference), for every team we
    // can place. A base team with no target row has not been ingested for the
    // new season and simply is not on the board.
    let mut league: Vec<(Uuid, String)> = Vec::new();
    let mut conf_of: HashMap<Uuid, String> = HashMap::new();
    for (base_id, target_id, name) in links {
        let Some(target_id) = target_id else { continue };
        let Some(c) = conferences.get(&base_id).and_then(|t| t.conference.clone()) else {
            continue;
        };
        if c == code {
            league.push((target_id, name));
        }
        conf_of.insert(target_id, c);
    }
    if league.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("no teams in conference {code} for season {season}"),
            })),
        ));
    }

    let anchors = projection::fetch_preseason_adj_em_map(pool, season)
        .await
        .map_err(err)?;
    let games: Vec<ScheduleSlot> = sqlx::query_as(
        "SELECT team_id, opponent_id, is_home, is_neutral FROM schedules WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .map_err(err)?;

    let mut rows: Vec<Value> = Vec::new();
    for (team_id, name) in &league {
        let Some(&team_em) = anchors.get(team_id) else {
            // Too thin to project (74 of 364 on the 2027 board). The row
            // still belongs in the league; it simply has no numbers.
            rows.push(json!({
                "team_id": team_id, "team_name": name,
                "expected_wins": null, "expected_conference_wins": null,
                "expected_conference_games": null, "projected_games": null,
                "adj_em": null,
            }));
            continue;
        };
        let (mut x_all, mut x_conf) = (0.0_f64, 0.0_f64);
        let (mut n_all, mut n_conf) = (0_i64, 0_i64);
        for g in &games {
            if g.team_id != *team_id {
                continue;
            }
            let Some(opp_id) = &g.opponent_id else {
                continue;
            };
            let Some(&opp_em) = anchors.get(opp_id) else {
                continue;
            };
            let venue = if g.is_neutral.unwrap_or(false) {
                Venue::Neutral
            } else if g.is_home.unwrap_or(false) {
                Venue::Home
            } else {
                Venue::Away
            };
            let p = projection::preseason_only_win_prob(projection::preseason_only_margin(
                team_em - opp_em,
                venue,
            ));
            x_all += p;
            n_all += 1;
            if conf_of.get(opp_id).map(String::as_str) == Some(code) {
                x_conf += p;
                n_conf += 1;
            }
        }
        rows.push(json!({
            "team_id": team_id,
            "team_name": name,
            "expected_wins": round1(x_all),
            "expected_conference_wins": round1(x_conf),
            "expected_conference_games": n_conf,
            "projected_games": n_all,
            "adj_em": round1(team_em as f64),
        }));
    }

    // Ordered by expected conference win RATE, not expected wins.
    //
    // Raw expected wins is the natural mirror of the played table and is
    // wrong here, because NatStat publishes the slate in pieces: with about a
    // third of 2026-27 out, sorting by the sum ranks teams by how much of
    // their schedule has been published. Measured on the real board it put
    // BYU (18 league games listed) first and Houston seventh — on the best
    // projection in the conference, with two games listed. That is a table
    // about NatStat's publishing order wearing the clothes of a standings
    // projection.
    //
    // The rate normalises it, and it is self-correcting rather than a
    // stopgap: once every slate is complete the denominators are equal and
    // ordering by rate is ordering by wins.
    rows.sort_by(|a, b| {
        // `None` = no league games listed yet. Not "worst" — unknown — so it
        // falls through to the projection rather than being sunk to the
        // bottom of a table it has no evidence in.
        let key = |v: &Value| {
            let n = v["expected_conference_games"].as_f64().unwrap_or(0.0);
            let rate = (n > 0.0).then(|| v["expected_conference_wins"].as_f64().unwrap_or(0.0) / n);
            (rate, v["adj_em"].as_f64().unwrap_or(f64::MIN))
        };
        let (ka, kb) = (key(a), key(b));
        match (ka.0, kb.0) {
            (Some(ra), Some(rb)) => (rb, kb.1)
                .partial_cmp(&(ra, ka.1))
                .unwrap_or(std::cmp::Ordering::Equal),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, None) => kb.1.partial_cmp(&ka.1).unwrap_or(std::cmp::Ordering::Equal),
        }
    });

    let rated: Vec<f64> = league
        .iter()
        .filter_map(|(id, _)| anchors.get(id).map(|&e| e as f64))
        .collect();
    let mean_adj_em = (!rated.is_empty()).then(|| rated.iter().sum::<f64>() / rated.len() as f64);

    Ok(Json(json!({
        "conference": code,
        "season": season,
        "projected": true,
        "teams": rows,
        "mean_adj_em": mean_adj_em.map(round1),
        // Deliberately absent for a projected season: a league's rank among
        // leagues and its non-conference record are facts about games played.
        "strength_rank": null,
        "conference_count": null,
        "non_conference_wins": null,
        "non_conference_losses": null,
    })))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
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
