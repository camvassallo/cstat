//! Regression guards for the `torvik_player_stats` fan-out (issue #307).
//!
//! `torvik_player_stats` is UNIQUE on `(torvik_pid, season)`, **not** on
//! `(player_id, season)`: a few hundred `(player, season)` pairs carry two or
//! three Torvik profiles. Any `LEFT JOIN torvik_player_stats ... ON player_id
//! = ... AND season = ...` therefore multiplies its row, and three read paths
//! joined exactly that way -- `get_team_roster` (served a duplicated player,
//! e.g. Mercyhurst 2026 at 15 rows for 14 players), `search_players`
//! (duplicated season-stat rows), and `pick_or_pin_daily_puzzle` (inflated
//! the candidate pool).
//!
//! Two different things produce those pairs, and only one of them is what it
//! looks like:
//!
//!   * **262 of the 287 locally are one human with two Torvik profiles at one
//!     school**, carrying byte-identical stat lines -- Bernie Blunt 2026 is
//!     the same GP, minutes and CAM under pids 74649 and 127223. Genuine
//!     upstream duplication; collapsing loses nothing at all.
//!   * **~19 are two DIFFERENT humans** who share a name and were both linked
//!     to one `player_id` by `ingest/torvik.rs`. 2017 Jared Harper is
//!     Auburn's (32 GP) and Fairfield's (12 GP) at once. `min(torvik_pid)`
//!     picks the wrong one about half the time, and no query-side tiebreak
//!     can do better -- the repair belongs in the linker (#313), after which
//!     these pairs stop existing. Collapsing is still right for them: the
//!     pre-fix behaviour was two indistinguishable rows, one equally wrong.
//!
//! `duplicate_pairs` deliberately does not separate the two. Both fan out,
//! both must collapse, and the tests here are about the fan-out.
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
    //
    // The pool counts don't depend on the date, so they are checked once per
    // season and only the pinned answer is swept across dates.
    let mut exercised = 0usize;
    for &season in &seasons {
        let probe = chrono::NaiveDate::from_ymd_opt(2999, 1, 1).unwrap();
        let (bare_rows, bare_pairs, _) = portle_pool(&pool, JOIN_BARE, season, probe).await;
        let (rows, pairs, _) = portle_pool(&pool, JOIN_COLLAPSED, season, probe).await;

        // The collapse removes duplicate rows and nothing else.
        assert_eq!(
            rows, pairs,
            "season {season}: pool holds {rows} rows for {pairs} (player, team) candidates",
        );
        assert_eq!(
            bare_pairs, pairs,
            "season {season}: the collapse dropped a candidate rather than a duplicate",
        );

        // Whether this season had anything to collapse at all. A season can
        // hold duplicated profiles that never clear the pool's GP/MPG or
        // answerable gates, and that is a legitimate data state — count it
        // instead of asserting, and require that some season did.
        if bare_rows > rows {
            exercised += 1;
        } else {
            eprintln!(
                "season {season}: no duplicated profile reaches the Portle \
                 pool; nothing to exercise here"
            );
        }

        // And no live puzzle moves. Both copies of a duplicated candidate
        // carry the same `natstat_id`, so they hash identically and the
        // minimum is unchanged — the fan-out inflated the pool without ever
        // skewing the draw. Asserted rather than assumed, because it is the
        // reason this call site needed no data fixup.
        for day in 1..=5 {
            let date = chrono::NaiveDate::from_ymd_opt(2999, 1, day).unwrap();
            let (_, _, bare_pick) = portle_pool(&pool, JOIN_BARE, season, date).await;
            let (_, _, pick) = portle_pool(&pool, JOIN_COLLAPSED, season, date).await;
            assert_eq!(
                bare_pick, pick,
                "season {season} {date}: the collapse moved the pinned answer",
            );
        }
    }

    assert!(
        exercised > 0,
        "no season's Portle pool held a duplicate — every assertion above \
         passed vacuously",
    );

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

// ---------------------------------------------------------------------------
// Issue #312 — the four display-list call sites in `cstat-api::routes`.
//
// Same root cause as the three above, but these live in route handlers rather
// than in `queries.rs`, so a `cstat-core` test cannot call them and has to
// reproduce the join skeleton. That restatement is the known weakness of the
// tests below, and it is why `tests/torvik_join_shape.rs` exists alongside
// them: the shape guard reads the real source and fails on a bare join
// wherever it appears, including a call site nobody has thought to reproduce
// here. The two are complementary — the shape guard proves the collapse is
// still spelled in the source, these prove the spelling actually collapses.
//
// The four sites do NOT share a symptom, which is worth stating because the
// issue reported one symptom for all of them:
//
//   * `recruits.rs` is the only REPEATED ROW. Its list is built from this
//     query's rows, so a duplicated profile rendered the recruit twice.
//   * `transfers.rs` and `draft.rs` bucket candidates by name and pick one.
//     Both copies carry the same name and team, so the disambiguators cannot
//     separate them and the winner was whichever the plan emitted first — a
//     non-deterministic CAM, not a repeated row.
//   * `projections.rs`'s departure enrichment collapses into a `HashMap` keyed
//     on player id, so the fan-out was resolved by last-write-wins. Also a
//     coin flip, also not a repeated row.
// ---------------------------------------------------------------------------

/// Row count for the `recruits.rs` list join, Torvik spelled bare or
/// collapsed. Counts rows rather than selecting the payload: the invariant is
/// that the list holds one row per `recruits` record.
async fn recruit_rows(pool: &PgPool, torvik_join: &str, year: i32) -> i64 {
    let sql = format!(
        r#"
        SELECT count(*)
        FROM recruits r
        {torvik_join}
        WHERE r.year = $1
        "#
    );
    sqlx::query_scalar(&sql)
        .bind(year)
        .fetch_one(pool)
        .await
        .unwrap()
}

const RECRUIT_JOIN_BARE: &str = "LEFT JOIN torvik_player_stats tps
            ON tps.player_id = r.cstat_player_id AND tps.season = r.year + 1";

const RECRUIT_JOIN_COLLAPSED: &str = "LEFT JOIN (
            SELECT DISTINCT ON (player_id, season) *
            FROM torvik_player_stats
            WHERE player_id IS NOT NULL
            ORDER BY player_id, season, torvik_pid
        ) tps ON tps.player_id = r.cstat_player_id AND tps.season = r.year + 1";

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn recruit_list_serves_one_row_per_recruit() {
    let Some(pool) = pool().await else { return };

    let years: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT year FROM recruits ORDER BY year")
        .fetch_all(&pool)
        .await
        .unwrap();
    if years.is_empty() {
        eprintln!("no recruits ingested locally; nothing to guard");
        return;
    }

    let mut exercised = 0usize;
    for year in years {
        let recruits: i64 = sqlx::query_scalar("SELECT count(*) FROM recruits WHERE year = $1")
            .bind(year)
            .fetch_one(&pool)
            .await
            .unwrap();

        let collapsed = recruit_rows(&pool, RECRUIT_JOIN_COLLAPSED, year).await;
        assert_eq!(
            collapsed, recruits,
            "class {year}: the list joins out to {collapsed} rows for {recruits} \
             recruits — the Torvik join is fanning rows out again",
        );

        // Not vacuous: the pre-fix join really did exceed the recruit count
        // for this class. A class whose duplicated profiles all belong to
        // recruits who never got a Torvik row in their freshman season is a
        // legitimate data state, so it is noted rather than failed.
        let bare = recruit_rows(&pool, RECRUIT_JOIN_BARE, year).await;
        if bare > recruits {
            eprintln!("class {year}: bare join served {bare} rows for {recruits} recruits");
            exercised += 1;
        }
    }

    assert!(
        exercised > 0,
        "no recruiting class exercised the fan-out — every assertion above \
         passed vacuously",
    );
}

/// Row count and distinct-stint count for the candidate query shared by
/// `transfers.rs::fetch_candidates` and `draft.rs`. `season_lo`/`season_hi`
/// cover both callers: `draft.rs` and pass 1 of `transfers.rs` ask for a
/// single season, pass 2 of `transfers.rs` for a lookback range.
async fn candidate_counts(
    pool: &PgPool,
    torvik_join: &str,
    season_lo: i32,
    season_hi: i32,
) -> (i64, i64) {
    let sql = format!(
        r#"
        WITH candidates AS (
            SELECT p.id AS player_id, pss.team_id, pss.season
            FROM player_season_stats pss
            JOIN players p ON p.id = pss.player_id AND p.season = pss.season
            LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
            {torvik_join}
            WHERE pss.season BETWEEN $1 AND $2
        )
        SELECT
            (SELECT count(*) FROM candidates),
            (SELECT count(*) FROM (
                SELECT DISTINCT player_id, team_id, season FROM candidates
            ) d)
        "#
    );
    sqlx::query(&sql)
        .bind(season_lo)
        .bind(season_hi)
        .fetch_one(pool)
        .await
        .map(|r| (r.get(0), r.get(1)))
        .unwrap()
}

const CANDIDATE_JOIN_BARE: &str =
    "LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = pss.season";

/// The season predicate is repeated inside the subquery, which the two
/// `queries.rs` season-wide sites do not need to do. They filter on
/// `pss.season = $1`, and Postgres pushes an equality qual through the join
/// equivalence into the subquery. It does not do that for a range, so without
/// the repeat `transfers.rs`'s `BETWEEN` de-duplicates all twelve seasons on
/// every call. Measured locally at 85 ms against 17 ms.
const CANDIDATE_JOIN_COLLAPSED: &str = "LEFT JOIN (
                SELECT DISTINCT ON (player_id, season) *
                FROM torvik_player_stats
                WHERE player_id IS NOT NULL
                  AND season BETWEEN $1 AND $2
                ORDER BY player_id, season, torvik_pid
            ) tps ON tps.player_id = p.id AND tps.season = pss.season";

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn name_bucket_candidates_hold_each_stint_once() {
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

    // One stint per (player, team, season) is the exact invariant here, not
    // one row per player: `player_season_stats` is UNIQUE on that triple, and
    // a mid-season transfer legitimately contributes a candidate per school.
    // The bucket index below is built to hold exactly those.
    let mut exercised = 0usize;
    for &season in &seasons {
        let (rows, stints) =
            candidate_counts(&pool, CANDIDATE_JOIN_COLLAPSED, season, season).await;
        assert_eq!(
            rows, stints,
            "season {season}: {rows} candidate rows for {stints} player-team \
             stints — the Torvik join is fanning rows out again",
        );

        let (bare, bare_stints) =
            candidate_counts(&pool, CANDIDATE_JOIN_BARE, season, season).await;
        assert_eq!(
            bare_stints, stints,
            "season {season}: the collapse dropped a stint rather than a duplicate",
        );
        if bare > stints {
            eprintln!("season {season}: bare join served {bare} candidates for {stints} stints");
            exercised += 1;
        }
    }

    // Pass 2 of `transfers.rs` asks for a multi-season range, which is the
    // case the equality-propagation shortcut does not cover. Exercise it as a
    // range rather than only season by season.
    let (lo, hi) = (*seasons.first().unwrap(), *seasons.last().unwrap());
    if hi > lo {
        let (rows, stints) = candidate_counts(&pool, CANDIDATE_JOIN_COLLAPSED, lo, hi).await;
        assert_eq!(
            rows, stints,
            "seasons {lo}..={hi}: {rows} candidate rows for {stints} stints — \
             the collapse does not survive a season range",
        );
    }

    assert!(
        exercised > 0,
        "no season exercised the fan-out — every assertion above passed vacuously",
    );
}

/// The most rows `projections.rs`'s departure enrichment returns for any one
/// player id. That query collapses into a `HashMap` keyed on the id, so
/// anything above 1 is a silent last-write-wins over two Torvik profiles.
async fn max_departure_rows_per_player(pool: &PgPool, torvik_join: &str, season: i32) -> i64 {
    let sql = format!(
        r#"
        SELECT COALESCE(max(n), 0) FROM (
            SELECT count(*) AS n
            FROM players p
            LEFT JOIN player_archetypes pa
                ON pa.player_id = p.id AND pa.season = $1
            {torvik_join}
            WHERE p.season = $1
            GROUP BY p.id
        ) x
        "#
    );
    sqlx::query_scalar(&sql)
        .bind(season)
        .fetch_one(pool)
        .await
        .unwrap()
}

const DEPARTURE_JOIN_BARE: &str = "LEFT JOIN torvik_player_stats tps
                 ON tps.player_id = p.id AND tps.season = $1";

const DEPARTURE_JOIN_COLLAPSED: &str = "LEFT JOIN LATERAL (
                     SELECT * FROM torvik_player_stats t
                     WHERE t.player_id = p.id AND t.season = $1
                     ORDER BY t.torvik_pid
                     LIMIT 1
                 ) tps ON TRUE";

#[tokio::test]
#[ignore = "needs a populated local DB; run: DATABASE_URL=... cargo test -p cstat-core \
            --test torvik_duplicate_profiles -- --ignored"]
async fn departure_enrichment_resolves_one_profile_per_player() {
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

    // `player_archetypes` is UNIQUE on (player_id, season) and
    // `player_season_stats` is joined on the same key the HashMap is built
    // from, so Torvik was the only join here that could put two rows under one
    // id. The projection's departure list is a subset of the base season's
    // players, so guarding the whole season is a superset of what the route
    // actually asks for.
    let mut exercised = 0usize;
    for &season in &seasons {
        let collapsed =
            max_departure_rows_per_player(&pool, DEPARTURE_JOIN_COLLAPSED, season).await;
        assert_eq!(
            collapsed, 1,
            "season {season}: departure enrichment returns up to {collapsed} rows \
             for one player id — the HashMap it feeds resolves that by \
             last-write-wins, so the served CAM is a coin flip",
        );

        if max_departure_rows_per_player(&pool, DEPARTURE_JOIN_BARE, season).await > 1 {
            exercised += 1;
        }
    }

    assert!(
        exercised > 0,
        "no season exercised the fan-out — every assertion above passed vacuously",
    );
}
