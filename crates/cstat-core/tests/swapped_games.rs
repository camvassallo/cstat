//! Invariant for `compute::correct_swapped_games` (issue #119).
//!
//! NatStat occasionally swaps a game's two teams onto each other's identity —
//! the 2018 Champions Classic (game 1083775) was stored as Duke 84 / Kentucky 118
//! with Kentucky's roster under "Duke", when Duke actually won 118-84. The fix
//! detects fully-swapped 2-team games (each side >=80% the OTHER team's players by
//! reconciled `players.team_id`) and relabels them. After `compute_all` has run
//! for every season, the invariant is simply: NO such game remains — the detector
//! finds an empty set. A break means a swap slipped through (or a relabel created
//! a fresh inconsistency).
//!
//! Gated `#[ignore]` — needs a local DB with `compute` run for all seasons. Run:
//!   DATABASE_URL=... cargo test -p cstat-core --test swapped_games -- --ignored --nocapture

use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

const MIN_CROSS_SHARE: f64 = 0.80;

#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn no_fully_swapped_games_remain() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // The same detector `correct_swapped_games` uses: 2-team games where BOTH
    // sides are mostly the other team's players (by reconciled season team).
    let rows = sqlx::query(
        r#"
        WITH gt AS (
            SELECT pgs.game_id, pgs.team_id AS labeled, pl.team_id AS real_team, COUNT(*) AS n
            FROM player_game_stats pgs
            JOIN players pl ON pl.id = pgs.player_id
            GROUP BY pgs.game_id, pgs.team_id, pl.team_id
        ),
        two_team AS (
            SELECT game_id FROM team_game_stats GROUP BY game_id HAVING COUNT(DISTINCT team_id) = 2
        ),
        sides AS (
            SELECT game_id, labeled,
                   SUM(n) AS tot,
                   SUM(n) FILTER (WHERE real_team IS DISTINCT FROM labeled) AS mis
            FROM gt
            WHERE game_id IN (SELECT game_id FROM two_team)
            GROUP BY game_id, labeled
        )
        SELECT s.game_id, g.season, g.natstat_id
        FROM sides s
        JOIN games g ON g.id = s.game_id
        GROUP BY s.game_id, g.season, g.natstat_id
        HAVING COUNT(*) = 2 AND MIN(s.mis::float8 / NULLIF(s.tot, 0)) >= $1
        ORDER BY g.season
        "#,
    )
    .bind(MIN_CROSS_SHARE)
    .fetch_all(&pool)
    .await
    .unwrap();

    if !rows.is_empty() {
        for r in &rows {
            let season: i32 = r.get("season");
            let natstat_id: String = r.get("natstat_id");
            eprintln!("  still-swapped: season {season} game {natstat_id}");
        }
    }
    assert_eq!(
        rows.len(),
        0,
        "{} fully-swapped game(s) remain — correct_swapped_games did not run for their \
         season (or a relabel left a side cross-tagged)",
        rows.len()
    );
}

/// Invariant for `compute::repair_phantom_swapped_games` (issue #140).
///
/// The harder swap variant: NatStat delivered four 2024-11-15/16 games (Virginia/
/// Villanova, Virginia Tech/Penn State, Holy Cross/Sacred Heart, UT Rio Grande
/// Valley/Tennessee Tech) with the rosters crossed AND a brand-new per-game phantom
/// id for every player, defeating the cross-tag detector above (each phantom's only
/// game reconciles it to its own wrong label). The repair re-identifies each phantom
/// against the opponent roster and merges it away. After `compute_all` has run for
/// every season the invariant is: NO 2-team game still has a side that is mostly
/// gp=1 phantoms resolving to the opponent roster.
#[tokio::test]
#[ignore = "needs local DB with compute run for all seasons"]
async fn no_phantom_swapped_games_remain() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // The same gate `repair_phantom_swapped_games` uses: 2-team games where BOTH
    // sides have >=80% of box rows being gp=1 phantoms that resolve (exact name, or
    // unique/first-name-disambiguated last name) to a real opponent-roster player.
    let rows = sqlx::query(
        r#"
        WITH np AS (
            SELECT p.id, p.season, p.team_id,
                   regexp_replace(lower(p.name),'[^a-z0-9]','','g') AS nn,
                   lower(regexp_replace(split_part(
                       regexp_replace(p.name,'( Jr\.?| Sr\.?| III| II| IV)$','','i'),' ',
                       array_length(string_to_array(
                           regexp_replace(p.name,'( Jr\.?| Sr\.?| III| II| IV)$','','i'),' '),1)),
                       '[^a-z0-9]','','g')) AS ln,
                   lower(split_part(p.name,' ',1)) AS fn,
                   (SELECT count(*) FROM player_game_stats x WHERE x.player_id=p.id) AS gp
            FROM players p
        ),
        games2 AS (
            SELECT game_id, season FROM team_game_stats
            GROUP BY game_id, season HAVING count(DISTINCT team_id) = 2
        ),
        resolved AS (
            SELECT pgs.id AS pgs_id, pgs.game_id,
                   (SELECT EXISTS (
                       SELECT 1 FROM np r
                       WHERE r.season = np.season AND r.gp > 1
                         AND r.team_id = (SELECT tg.team_id FROM team_game_stats tg
                                          WHERE tg.game_id = pgs.game_id AND tg.team_id <> pgs.team_id)
                         AND (r.nn = np.nn OR r.ln = np.ln))) AS resolves
            FROM player_game_stats pgs
            JOIN np ON np.id = pgs.player_id
            WHERE np.gp = 1 AND pgs.game_id IN (SELECT game_id FROM games2)
        ),
        sides AS (
            SELECT pgs.game_id, pgs.team_id,
                   count(*) AS box,
                   count(*) FILTER (WHERE r.resolves) AS res
            FROM player_game_stats pgs
            LEFT JOIN resolved r ON r.pgs_id = pgs.id
            WHERE pgs.game_id IN (SELECT game_id FROM games2)
            GROUP BY pgs.game_id, pgs.team_id
        )
        SELECT s.game_id, g.season, g.natstat_id
        FROM sides s JOIN games g ON g.id = s.game_id
        GROUP BY s.game_id, g.season, g.natstat_id
        HAVING count(*) = 2 AND min(s.res::float8 / s.box) >= $1 AND min(s.res) >= 3
        ORDER BY g.season
        "#,
    )
    .bind(MIN_CROSS_SHARE)
    .fetch_all(&pool)
    .await
    .unwrap();

    for r in &rows {
        let season: i32 = r.get("season");
        let natstat_id: String = r.get("natstat_id");
        eprintln!("  still-phantom-swapped: season {season} game {natstat_id}");
    }
    assert_eq!(
        rows.len(),
        0,
        "{} phantom-swapped game(s) remain — repair_phantom_swapped_games did not run \
         for their season (or could not re-identify a side's phantoms)",
        rows.len()
    );
}
