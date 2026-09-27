use axum::{Router, extract::State, http::StatusCode, response::Json, routing::get};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/seasons", get(list_seasons))
}

/// Seasons the site can show, so the frontend doesn't have to hardcode them.
///
/// Returns `{ seasons: [int], default: int, upcoming: int | null }`, and the
/// three answer genuinely different questions. Conflating the first two is
/// what #386 was:
///
/// - **`seasons`** — every season with a **played** game, newest first. The
///   list the navbar picker offers on Rankings, Players and Teams.
/// - **`default`** — where to land with no `?season=`: the newest of those.
/// - **`upcoming`** — the next season that is *projected* but not yet played,
///   for the Future tab and the team projection ledger. `null` when there is
///   none.
///
/// The distinction only starts to matter the day a schedule is ingested ahead
/// of tip-off, and then it matters immediately. A scheduled-but-unplayed game
/// satisfies "is there a row in `games`" exactly as well as a completed one,
/// so the previous `SELECT DISTINCT season FROM games` would have made 2027
/// both the newest season and the default the moment its 1,827 games landed —
/// dropping every visitor with no `?season=` onto a season with no results, no
/// `team_season_stats`, no rankings and no players.
///
/// Requiring a played game is a **no-op until that happens**: every season the
/// database holds today has played games. It is written now precisely so that
/// it is already true when the schedule arrives.
async fn list_seasons(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let seasons = played_seasons(&state.db.pool).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("seasons query failed: {e}") })),
        )
    })?;
    let default = seasons.first().copied();
    let upcoming = next_projected_season(&state.db.pool, default)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("upcoming-season query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({
        "seasons": seasons,
        "default": default,
        "upcoming": upcoming,
    })))
}

/// Seasons with at least one game played to a final score, newest first.
///
/// A final score rather than merely a row in `games`: the schedule for a
/// season is ingested weeks before any of it is played, and a season nobody
/// has played is not one to land a visitor on.
async fn played_seasons(pool: &PgPool) -> Result<Vec<i32>, sqlx::Error> {
    let rows: Vec<(i32,)> = sqlx::query_as(
        "SELECT DISTINCT season FROM games \
         WHERE season IS NOT NULL \
           AND home_score IS NOT NULL AND away_score IS NOT NULL \
         ORDER BY season DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// The next season that has a preseason projection but has not been played —
/// the Future tab's target.
///
/// Keyed on `team_preseason_projection` rather than on arithmetic. The
/// frontend's `upcomingProjectionSeason()` has been `newest_played + 1`, which
/// asserts a season exists rather than checking, and is a constant someone has
/// to remember to edit every November. This table is the thing that actually
/// decides whether the Future page and `/api/predict`'s preseason regime
/// (#387) can answer at all: with no rows, both have nothing to serve, and
/// claiming the season is "upcoming" only routes people to an empty page.
///
/// **`MIN`, not `MAX`, of the candidates**, which is not cosmetic. The
/// nightly's `projections` step writes `current_natstat_season() + 1`, and
/// `current_natstat_season()` rolls over on Nov 1 while the season does not
/// tip off for another few days. In that window the table holds BOTH 2027
/// (written earlier) and 2028 (written from Nov 1), and neither has been
/// played — `MAX` would skip a season forward and point the Future tab at
/// 2028 during exactly the fortnight 2027 is the interesting one. The next
/// unplayed season is the smallest one above the newest played.
async fn next_projected_season(
    pool: &PgPool,
    newest_played: Option<i32>,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i32>>(
        "SELECT min(season) FROM team_preseason_projection WHERE season > $1",
    )
    // No played season at all (an empty or freshly-bootstrapped database):
    // every projected season is "upcoming", so compare against a floor no
    // real season reaches rather than skipping the lookup.
    .bind(newest_played.unwrap_or(i32::MIN))
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    /// TEMP `games` and `team_preseason_projection` shadowing the real tables
    /// through `search_path`, on a pinned single-connection pool.
    ///
    /// Same discipline as `cstat-ingest`'s `tests/ledger_write_failures.rs`,
    /// and asserted rather than assumed: resolve both names and refuse to run
    /// unless they are temp, so a pool that ever reconnected fails loudly
    /// instead of reading a developer's real database and quietly passing on
    /// whatever happens to be in it.
    ///
    /// Only the columns these two queries read. Separate statements because
    /// `sqlx::query` speaks the extended protocol, which takes one per call.
    const TEMP_DDL: [&str; 2] = [
        "CREATE TEMP TABLE games (
            season      integer,
            home_score  integer,
            away_score  integer
        )",
        "CREATE TEMP TABLE team_preseason_projection (
            season      integer NOT NULL,
            team_id     uuid    NOT NULL DEFAULT gen_random_uuid()
        )",
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
            sqlx::query(ddl)
                .execute(&pool)
                .await
                .expect("create temp tables");
        }
        for table in ["games", "team_preseason_projection"] {
            let schema: String = sqlx::query_scalar(
                "SELECT n.nspname FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.oid = $1::regclass",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("resolve table");
            assert!(
                schema.starts_with("pg_temp"),
                "refusing to run: `{table}` resolved to `{schema}`, not a temp schema — \
                 this test would read the REAL database in DATABASE_URL"
            );
        }
        Some(pool)
    }

    async fn add_game(pool: &PgPool, season: i32, played: bool) {
        let score = played.then_some(70);
        sqlx::query("INSERT INTO games (season, home_score, away_score) VALUES ($1, $2, $2)")
            .bind(season)
            .bind(score)
            .execute(pool)
            .await
            .expect("insert game");
    }

    async fn add_projection(pool: &PgPool, season: i32) {
        sqlx::query("INSERT INTO team_preseason_projection (season) VALUES ($1)")
            .bind(season)
            .execute(pool)
            .await
            .expect("insert projection");
    }

    #[tokio::test]
    async fn an_ingested_schedule_does_not_become_the_default_season() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        for s in [2025, 2026] {
            add_game(&pool, s, true).await;
        }
        // #386 itself: the 2027 schedule lands, 1,827 rows, none played.
        add_game(&pool, 2027, false).await;

        let seasons = played_seasons(&pool).await.expect("query");
        assert_eq!(
            seasons,
            vec![2026, 2025],
            "a season with only scheduled games must not enter the picker list"
        );
        assert_eq!(
            seasons.first().copied(),
            Some(2026),
            "and must not become the season the site lands on"
        );

        // The first result flips it, with nothing else changing.
        add_game(&pool, 2027, true).await;
        assert_eq!(
            played_seasons(&pool).await.expect("query").first().copied(),
            Some(2027),
            "once a 2027 game is played the default must follow it"
        );
    }

    #[tokio::test]
    async fn upcoming_is_the_next_unplayed_season_not_the_furthest() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        add_game(&pool, 2026, true).await;
        add_projection(&pool, 2026).await;
        add_projection(&pool, 2027).await;

        assert_eq!(
            next_projected_season(&pool, Some(2026))
                .await
                .expect("query"),
            Some(2027),
            "the season projected beyond the newest played one"
        );

        // The Nov-1 window, and the reason this is MIN rather than MAX:
        // `current_natstat_season()` rolls over before the season tips off, so
        // the nightly starts writing 2028 while 2027 is still unplayed. MAX
        // would point the Future tab at 2028 for the fortnight in which 2027
        // is the whole story.
        add_projection(&pool, 2028).await;
        assert_eq!(
            next_projected_season(&pool, Some(2026))
                .await
                .expect("query"),
            Some(2027),
            "with both 2027 and 2028 projected and neither played, the next one is 2027"
        );

        // Once 2027 is genuinely under way, the target moves on by itself.
        add_game(&pool, 2027, true).await;
        let newest = played_seasons(&pool).await.expect("query")[0];
        assert_eq!(newest, 2027);
        assert_eq!(
            next_projected_season(&pool, Some(newest))
                .await
                .expect("query"),
            Some(2028)
        );
    }

    #[tokio::test]
    async fn upcoming_is_null_when_nothing_is_projected_ahead() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        add_game(&pool, 2026, true).await;
        // Historical rows only — `compute-projections` writes these for past
        // seasons too, so "the table is non-empty" is not the question.
        add_projection(&pool, 2025).await;
        add_projection(&pool, 2026).await;

        assert_eq!(
            next_projected_season(&pool, Some(2026))
                .await
                .expect("query"),
            None,
            "past projections must not be reported as an upcoming season"
        );
    }

    #[tokio::test]
    async fn a_database_with_no_played_season_still_reports_an_upcoming_one() {
        let Some(pool) = temp_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        // A fresh bootstrap: projections written, not a game played anywhere.
        // `default` is None here, and the `i32::MIN` floor is what keeps the
        // Future tab reachable instead of comparing against nothing.
        add_projection(&pool, 2027).await;

        assert!(played_seasons(&pool).await.expect("query").is_empty());
        assert_eq!(
            next_projected_season(&pool, None).await.expect("query"),
            Some(2027)
        );
    }
}
