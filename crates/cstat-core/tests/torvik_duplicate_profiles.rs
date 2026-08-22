//! Regression guards for the `torvik_player_stats` fan-out (issue #307).
//!
//! `torvik_player_stats` is UNIQUE on `(torvik_pid, season)`, **not** on
//! `(player_id, season)`: a few hundred `(player, season)` pairs carry two or
//! three Torvik profiles for one human. Any `LEFT JOIN torvik_player_stats
//! ... ON player_id = ... AND season = ...` therefore multiplies its row, and
//! three read paths joined exactly that way -- `get_team_roster` (served a
//! duplicated player, e.g. Mercyhurst 2026 at 15 rows for 14 players),
//! `search_players` (duplicated season-stat rows), and
//! `pick_or_pin_daily_puzzle` (inflated the candidate pool).
//!
//! The collapse takes the lowest `torvik_pid`, the same deterministic tiebreak
//! #306 chose, so where duplicate profiles co-occur across seasons a human
//! keeps one identity every year. The three sites use two different shapes for
//! it -- LATERAL on the single-team roster, DISTINCT ON on the two season-wide
//! queries -- and `roster_and_list_agree_on_which_profile_wins` pins them to
//! the same pick by comparing what the two served functions actually return,
//! not by re-stating either idiom. Let them diverge and the same player shows
//! two different CAM values depending on which page you are looking at.
//!
//! The Portle test is the odd one: it passes against the pre-fix SQL too, and
//! that is the finding rather than a hole. Both copies of a duplicated
//! candidate carry the same `natstat_id`, so they hash to the same
//! `md5(salt:natstat_id)` and the minimum the draw takes is unchanged -- the
//! fan-out inflated the pool without ever skewing it. The collapse there is
//! consistency with its two siblings, and no pinned puzzle needs repairing.
//! The test asserts that equivalence explicitly so a future change to the
//! ordering key can't quietly make the duplication matter.
//!
//! DB-gated: uses whatever the local DB already holds and skips cleanly when
//! `DATABASE_URL` is unset or holds no duplicates.
//!
//!   DATABASE_URL=... cargo test -p cstat-core --test torvik_duplicate_profiles -- --ignored --nocapture

use cstat_core::queries::{self, PlayerSortField, PortleMode, SortOrder};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::collections::HashSet;
use uuid::Uuid;

const SKIP: &str = "DATABASE_URL unset; skipping torvik duplicate-profile test";

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("{SKIP}");
        return None;
    };
    Some(PgPoolOptions::new().connect(&url).await.unwrap())
}

/// `(player_id, season)` pairs carrying more than one Torvik profile — the
/// cohort that fans a naive join out. Empty on a DB that happens to hold no
/// duplicates, in which case the caller skips rather than passing vacuously.
async fn duplicate_pairs(pool: &PgPool) -> Vec<(Uuid, i32)> {
    sqlx::query(
        r#"
        SELECT player_id, season
        FROM torvik_player_stats
        WHERE player_id IS NOT NULL
        GROUP BY player_id, season
        HAVING count(*) > 1
        ORDER BY season DESC, player_id
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get::<Uuid, _>(0), r.get::<i32, _>(1)))
    .collect()
}

/// Every team/season roster holding at least one duplicated player. These are
/// exactly the rosters that fanned out before the fix.
async fn affected_rosters(pool: &PgPool) -> Vec<(Uuid, i32)> {
    sqlx::query(
        r#"
        SELECT DISTINCT p.team_id, p.season
        FROM players p
        JOIN (
            SELECT player_id, season
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            GROUP BY player_id, season
            HAVING count(*) > 1
        ) d ON d.player_id = p.id AND d.season = p.season
        WHERE p.team_id IS NOT NULL
        ORDER BY p.season DESC, p.team_id
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get::<Uuid, _>(0), r.get::<i32, _>(1)))
    .collect()
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn team_roster_serves_one_row_per_player() {
    let Some(pool) = pool().await else { return };

    let rosters = affected_rosters(&pool).await;
    if rosters.is_empty() {
        eprintln!("no duplicated Torvik profiles locally; nothing to guard");
        return;
    }
    eprintln!("{} affected team/season rosters", rosters.len());

    for (team_id, season) in rosters {
        let roster = queries::get_team_roster(&pool, team_id, season)
            .await
            .unwrap();
        let distinct: HashSet<Uuid> = roster.iter().map(|r| r.player_id).collect();
        assert_eq!(
            roster.len(),
            distinct.len(),
            "team {team_id} season {season}: roster fanned out — {} rows for {} players",
            roster.len(),
            distinct.len(),
        );
    }
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn search_players_returns_one_row_per_player() {
    let Some(pool) = pool().await else { return };

    let mut seasons: Vec<i32> = duplicate_pairs(&pool)
        .await
        .into_iter()
        .map(|p| p.1)
        .collect();
    seasons.sort_unstable();
    seasons.dedup();
    if seasons.is_empty() {
        eprintln!("no duplicated Torvik profiles locally; nothing to guard");
        return;
    }

    let mut exercised = 0usize;
    for season in seasons {
        // No team/search/archetype filter and a limit past any real season's
        // eligible pool, so the whole listable set is checked at once. The
        // reported total is the count query, which never joined Torvik and so
        // was always correct — asserting the page against it catches the rows
        // drifting apart from the count as well as the duplication itself.
        let (rows, total) = queries::search_players(
            &pool,
            None,
            None,
            season,
            PlayerSortField::Campom,
            Some(SortOrder::Desc),
            None,
            false,
            20_000,
            0,
        )
        .await
        .unwrap();

        // The count query is the fan-out-free baseline: it never joined
        // Torvik, and every other table `search_players` joins is unique on
        // its key (`player_percentiles` / `player_archetypes` / `player_rapm`
        // on (player, season), `player_on_off` on (player, season, team)), so
        // Torvik was the only thing that could multiply a row. Row count
        // against that total is therefore the exact invariant — and a
        // stronger one than counting distinct players, because
        // `player_season_stats` is UNIQUE on (player_id, team_id, season) and
        // a mid-season transfer legitimately gets a row per school.
        assert_eq!(
            rows.len() as i64,
            total,
            "season {season}: {} listed rows against a count of {total} — the \
             Torvik join is fanning rows out again",
            rows.len(),
        );

        // Not vacuous: check the pre-fix join really did exceed that count.
        // A season can hold duplicated profiles that all sit below the
        // GP>=5 / MPG>=10 gate and so never reached this listing — a
        // legitimate data state, so that season is skipped rather than
        // failed, and `exercised` makes sure at least one season did.
        let bare: i64 = sqlx::query(
            r#"
            SELECT count(*)
            FROM player_season_stats pss
            JOIN players p ON p.id = pss.player_id AND p.season = pss.season
            LEFT JOIN torvik_player_stats tps
                   ON tps.player_id = p.id AND tps.season = pss.season
            WHERE pss.season = $1
              AND pss.games_played >= 5
              AND pss.minutes_per_game >= 10
            "#,
        )
        .bind(season)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        if bare > total {
            exercised += 1;
        } else {
            eprintln!(
                "season {season}: no duplicated profile clears the listing's \
                 GP/MPG gate; nothing to exercise here"
            );
        }
    }

    assert!(
        exercised > 0,
        "no season exercised the fan-out — every assertion above passed \
         vacuously",
    );
}

/// The Portle eligible CTE, with the Torvik join spelled either the pre-fix
/// way (`JOIN_BARE`) or the collapsed way (`JOIN_COLLAPSED`). Returns the
/// candidate row count, the distinct (player, team) count, and the id the draw
/// would pin for `date` — the answer is `md5(salt:natstat_id)`-ordered, so this
/// reproduces `pick_or_pin_daily_puzzle`'s choice without writing a pin.
async fn portle_pool(
    pool: &PgPool,
    torvik_join: &str,
    season: i32,
    date: chrono::NaiveDate,
) -> (i64, i64, Option<String>) {
    let sql = format!(
        r#"
        WITH eligible AS (
            SELECT p.id AS player_id, pss.team_id, p.natstat_id
            FROM player_season_stats pss
            JOIN players p ON p.id = pss.player_id AND p.season = pss.season
            LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
            {torvik_join}
            LEFT JOIN player_archetypes pa ON pa.player_id = pss.player_id AND pa.season = pss.season
            WHERE pss.season = $1
              AND pss.games_played >= 5
              AND pss.minutes_per_game >= 10
              AND tps.cam_gbpm_v3_psos IS NOT NULL
              AND pa.primary_class IS NOT NULL
        )
        SELECT
            (SELECT count(*) FROM eligible),
            (SELECT count(*) FROM (SELECT DISTINCT player_id, team_id FROM eligible) d),
            (SELECT e.natstat_id FROM eligible e
             ORDER BY md5('all' || ':' || $1::text || ':' || $2::text || ':' || e.natstat_id),
                      e.natstat_id
             LIMIT 1)
        "#
    );
    sqlx::query(&sql)
        .bind(season)
        .bind(date)
        .fetch_one(pool)
        .await
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .unwrap()
}

const JOIN_BARE: &str =
    "LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = pss.season";

const JOIN_COLLAPSED: &str = "LEFT JOIN (
                SELECT DISTINCT ON (player_id, season) *
                FROM torvik_player_stats
                WHERE player_id IS NOT NULL
                ORDER BY player_id, season, torvik_pid
            ) tps ON tps.player_id = p.id AND tps.season = pss.season";

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn portle_pool_holds_each_candidate_once_and_pins_the_same_answer() {
    let Some(pool) = pool().await else { return };

    let mut seasons: Vec<i32> = duplicate_pairs(&pool)
        .await
        .into_iter()
        .map(|p| p.1)
        .collect();
    seasons.sort_unstable();
    seasons.dedup();
    if seasons.is_empty() {
        eprintln!("no duplicated Torvik profiles locally; nothing to guard");
        return;
    }

    // `pick_or_pin_daily_puzzle` freezes its answer, so the draw can't be
    // re-run through the served function to observe it. Reproduce the eligible
    // CTE instead, both ways, and compare.
    for &season in &seasons {
        for day in 1..=5 {
            let date = chrono::NaiveDate::from_ymd_opt(2999, 1, day).unwrap();
            let (bare_rows, bare_pairs, bare_pick) =
                portle_pool(&pool, JOIN_BARE, season, date).await;
            let (rows, pairs, pick) = portle_pool(&pool, JOIN_COLLAPSED, season, date).await;

            // The collapse removes candidate rows and nothing else.
            assert_eq!(
                rows, pairs,
                "season {season}: pool holds {rows} rows for {pairs} (player, team) candidates",
            );
            assert!(
                bare_rows > bare_pairs || bare_rows == rows,
                "season {season}: pre-fix pool was already collapsed but the counts moved",
            );
            assert_eq!(
                bare_pairs, pairs,
                "season {season}: the collapse dropped a candidate rather than a duplicate",
            );

            // And no live puzzle moves. Both copies of a duplicated candidate
            // carry the same `natstat_id`, so they hash identically and the
            // minimum is unchanged — the fan-out never actually skewed the
            // draw, it only inflated the pool. Asserted rather than assumed,
            // because it is the reason this call site needed no data fixup.
            assert_eq!(
                bare_pick, pick,
                "season {season} {date}: the collapse moved the pinned answer",
            );
        }
    }

    // The served path still pins something for the newest affected season —
    // the collapse must not have emptied the CTE.
    let season = *seasons.last().unwrap();
    let date = chrono::NaiveDate::from_ymd_opt(2999, 3, 7).unwrap();
    let pinned = queries::pick_or_pin_daily_puzzle(&pool, PortleMode::All, season, date)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM portle_daily_puzzle WHERE mode = $1 AND season = $2 AND puzzle_date = $3",
    )
    .bind(PortleMode::All.as_str())
    .bind(season)
    .bind(date)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        pinned,
        portle_pool(&pool, JOIN_COLLAPSED, season, date).await.2,
        "season {season}: the served pin disagrees with the pool's own ordering",
    );
}

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn roster_and_list_agree_on_which_profile_wins() {
    let Some(pool) = pool().await else { return };

    let pairs = duplicate_pairs(&pool).await;
    if pairs.is_empty() {
        eprintln!("no duplicated Torvik profiles locally; nothing to guard");
        return;
    }

    // The two call sites spell the collapse differently -- LATERAL `ORDER BY
    // torvik_pid LIMIT 1` on the roster, `DISTINCT ON (player_id, season)
    // ORDER BY player_id, season, torvik_pid` on the list -- and both are
    // meant to be min(torvik_pid). Compare what the two functions actually
    // return rather than re-running either idiom here: a test that restated
    // the SQL would keep passing while a call site drifted away from it.
    //
    // Only players a duplicated profile actually reaches are checked. The
    // listing gates on GP>=5 / MPG>=10 and the roster does not, so a bench
    // player is legitimately absent from one side.
    let mut compared = 0usize;
    for (player_id, season) in &pairs {
        let Some(team_id) =
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT team_id FROM players WHERE id = $1")
                .bind(player_id)
                .fetch_one(&pool)
                .await
                .unwrap()
        else {
            continue;
        };

        let roster = queries::get_team_roster(&pool, team_id, *season)
            .await
            .unwrap();
        let Some(from_roster) = roster.iter().find(|r| r.player_id == *player_id) else {
            continue;
        };

        let (listed, _) = queries::search_players(
            &pool,
            None,
            Some(team_id),
            *season,
            PlayerSortField::Campom,
            Some(SortOrder::Desc),
            None,
            false,
            500,
            0,
        )
        .await
        .unwrap();
        let Some(from_list) = listed.iter().find(|r| r.player_id == *player_id) else {
            continue;
        };

        assert_eq!(
            from_roster.campom, from_list.campom,
            "player {player_id} season {season}: CAM is {:?} on the team page \
             and {:?} in the player list — the two collapses picked different \
             Torvik profiles",
            from_roster.campom, from_list.campom,
        );
        assert_eq!(
            from_roster.campom_pct, from_list.campom_pct,
            "player {player_id} season {season}: CAM percentile disagrees \
             between the team page and the player list",
        );
        assert_eq!(
            (from_roster.campom_o, from_roster.campom_d),
            (from_list.campom_o, from_list.campom_d),
            "player {player_id} season {season}: CAMO/CAMD disagree between \
             the team page and the player list",
        );
        compared += 1;
    }

    assert!(
        compared > 0,
        "{} duplicated pairs, none of which reached both a roster and the \
         listing — nothing was actually compared",
        pairs.len(),
    );
    eprintln!("{compared} duplicated players agree across both served paths");
}
