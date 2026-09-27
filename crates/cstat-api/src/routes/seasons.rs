use axum::{Router, extract::State, http::StatusCode, response::Json, routing::get};
use chrono::NaiveDate;
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
/// - **`default`** — where to land with no `?season=`. Normally the newest
///   played season, but the **upcoming** one once the previous season is over
///   and a projection exists for the next: in September, the interesting board
///   is next season's projection, not last season's final table (#394).
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
    let pool = &state.db.pool;
    let seasons = played_seasons(pool).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("seasons query failed: {e}") })),
        )
    })?;
    let newest_played = seasons.first().copied();
    let upcoming = next_projected_season(pool, newest_played)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("upcoming-season query failed: {e}") })),
            )
        })?;
    let last_game = match newest_played {
        Some(s) => last_played_game(pool, s).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("last-game query failed: {e}") })),
            )
        })?,
        None => None,
    };

    // The clock is read at the edge, as in `routes/predict.rs`: `today_utc()`
    // is where the replay harness's `CSTAT_SIMULATED_DATE` override lives, and
    // `CURRENT_DATE` in the SQL would not see it.
    let default = if prefer_upcoming(last_game, cstat_ingest::today_utc(), upcoming) {
        upcoming
    } else {
        newest_played
    };

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

/// Days of silence after which the newest played season is treated as over.
///
/// Measured rather than guessed, on eleven seasons of prod data: the longest
/// gap between consecutive game DATES *within* a season is **4 days**, while
/// the off-season runs about **220**. Thirty days sits an order of magnitude
/// clear of the first and well inside the second, so no in-season lull can
/// reach it and no off-season can fail to.
const SEASON_DORMANT_AFTER_DAYS: i64 = 30;

// The threshold has to clear both populations it separates, and those are
// measurements, not preferences — so assert it at compile time rather than in
// a test someone can skip. Shrinking it below an in-season lull would make the
// site jump to next year's projections mid-season; growing it past an
// off-season would mean it never leads with the projection at all.
const _: () = {
    /// Longest gap between consecutive game dates within a season, 2024-2026.
    const LONGEST_IN_SEASON_GAP: i64 = 4;
    /// Roughly April to November.
    const SHORTEST_OFF_SEASON: i64 = 180;
    assert!(SEASON_DORMANT_AFTER_DAYS > LONGEST_IN_SEASON_GAP * 2);
    assert!(SEASON_DORMANT_AFTER_DAYS < SHORTEST_OFF_SEASON / 2);
};

/// Should the site lead with the upcoming projection rather than the newest
/// played season?
///
/// Pure, and separately tested, because the obvious version of this rule is
/// wrong in a way that would not show up for six weeks. "Prefer the upcoming
/// season whenever it has projections" **breaks on Nov 1**: the nightly's
/// `projections` step writes `current_natstat_season() + 1`, that function
/// rolls over on Nov 1, and so from that night a projection for the season
/// *after* the one about to tip off exists. Once the new season plays a game
/// it becomes the newest played and `upcoming` correctly advances a year — at
/// which point the naive rule would land every visitor on **next year's
/// projections for the whole season**.
///
/// What separates the two cases is not whether a projection exists, it is
/// whether the season in hand is still being played. Hence the gate is
/// recency of the last completed game, which needs no calendar constant and
/// cannot rot the way a hardcoded Nov-1 test would.
///
/// `None` for `last_game` means nothing has ever been played — a fresh
/// bootstrap — and the projection is then the only thing there is to show.
fn prefer_upcoming(last_game: Option<NaiveDate>, today: NaiveDate, upcoming: Option<i32>) -> bool {
    if upcoming.is_none() {
        return false;
    }
    match last_game {
        Some(d) => (today - d).num_days() > SEASON_DORMANT_AFTER_DAYS,
        None => true,
    }
}

/// Date of the most recent completed game in a season.
async fn last_played_game(pool: &PgPool, season: i32) -> Result<Option<NaiveDate>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<NaiveDate>>(
        "SELECT max(game_date) FROM games \
         WHERE season = $1 AND home_score IS NOT NULL AND away_score IS NOT NULL",
    )
    .bind(season)
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

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn leads_with_the_projection_once_the_previous_season_is_over() {
        // Late September: 2026 ended in April, 2027 is projected. The whole
        // point of #394 — the interesting board is next season's.
        assert!(prefer_upcoming(
            Some(date("2026-04-06")),
            date("2026-09-27"),
            Some(2027)
        ));

        // A fresh bootstrap with nothing ever played: the projection is the
        // only thing there is to show.
        assert!(prefer_upcoming(None, date("2026-09-27"), Some(2027)));

        // Nothing projected ahead — there is nowhere else to go.
        assert!(!prefer_upcoming(
            Some(date("2026-04-06")),
            date("2026-09-27"),
            None
        ));
    }

    #[test]
    fn never_leads_with_next_year_while_a_season_is_being_played() {
        // THE REGRESSION THIS GATE EXISTS FOR. From Nov 1 the nightly writes
        // `current_natstat_season() + 1`, so once 2027 tips off, `upcoming`
        // is 2028 and a projection for it genuinely exists. Keying on "is
        // something projected" alone would land every visitor on the 2028
        // forecast for the whole 2026-27 season.
        let mid_season = date("2027-01-15");
        assert!(!prefer_upcoming(
            Some(date("2027-01-14")),
            mid_season,
            Some(2028)
        ));

        // Not just the day after a game: the longest gap between game dates
        // WITHIN a season measured over 2024-2026 is four days, so a lull
        // must not read as an off-season either.
        for gap in [1, 4, 10, 30] {
            let last = mid_season - chrono::Duration::days(gap);
            assert!(
                !prefer_upcoming(Some(last), mid_season, Some(2028)),
                "a {gap}-day gap is an in-season lull, not an off-season"
            );
        }

        // And the boundary is a boundary: past the threshold it flips.
        let last = mid_season - chrono::Duration::days(SEASON_DORMANT_AFTER_DAYS + 1);
        assert!(prefer_upcoming(Some(last), mid_season, Some(2028)));
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
