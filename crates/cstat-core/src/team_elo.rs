//! In-house Elo over our own `games` (NatStat migration plan §4.4, §4.5 step 6).
//!
//! Every served game model reads an Elo difference. Until now that was
//! NatStat's rating, so a NatStat outage stalled a model input and retiring
//! NatStat meant losing the feature. This is the replacement the walk-forward
//! test in `training/experiments/experiment_elo_source.py` chose (#507): a
//! margin-aware, 538-style Elo that ties NatStat's on margin MAE, total MAE and
//! win log-loss, and beats it alone as a predictor. CollegeBasketballData's Elo
//! lost the same test.
//!
//! The update, for each played game in `(game_date, game_id)` order:
//!
//! ```text
//! expected = 1 / (1 + 10^((away - home - edge) / 400))     edge = 100, 0 at a neutral site
//! mult     = ln(|margin| + 1) * 2.2 / (0.001 * winner_edge + 2.2)
//! delta    = K * mult * (home_won - expected)                K = 20
//! home += delta; away -= delta
//! ```
//!
//! where `winner_edge` is the winner's pre-game edge including the home edge
//! (the autocorrelation damping: a favourite's blowout moves it less). At a
//! team's first game of a season its rating regresses toward the mean,
//! `r <- 1500 + 0.85 * (r - 1500)`, once however many seasons it sat out. A
//! team seen for the first time starts at 1500. A non-Division I opponent (no
//! `teams` row) rates a fixed 1200 and is never updated.
//!
//! The constants were picked by the experiment on 2016-2020 home-win log-loss,
//! before its first walk-forward test season. **This module must reproduce the
//! experiment's `own_elo` exactly** — that is what the walk-forward verdict was
//! measured on. `tests` below pins it on a fixture whose expected values came
//! from running `own_elo` itself.
//!
//! The team identity that carries a rating across seasons is `teams.natstat_id`
//! (season-scoped UUIDs cannot). It is read here and nowhere else in this
//! module, so moving cross-season joins to `espn_id` (§4.7) is a change to
//! [`load_games`] alone.

use chrono::NaiveDate;
use sqlx::{PgPool, QueryBuilder};
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

/// Update size.
pub const ELO_K: f64 = 20.0;
/// Home edge in rating points; zero at a neutral site.
pub const ELO_HOME_EDGE: f64 = 100.0;
/// Share of a rating's distance from the mean kept into a new season.
pub const ELO_CARRY: f64 = 0.85;
/// A team's first rating, and the mean ratings regress to.
pub const ELO_INIT: f64 = 1500.0;
/// The fixed rating of a non-Division I opponent.
pub const ELO_NON_D1: f64 = 1200.0;

/// A rating carried into a new season.
pub fn carry_over(rating: f64) -> f64 {
    ELO_INIT + ELO_CARRY * (rating - ELO_INIT)
}

/// A team's rating entering `season`, given its rating after its latest game
/// and that game's season. The carry-over is applied once, however many
/// seasons the team sat out, as the sequential update does.
pub fn rating_entering(last_after: f64, last_season: i32, season: i32) -> f64 {
    if last_season == season {
        last_after
    } else {
        carry_over(last_after)
    }
}

/// One Division I side of a game.
#[derive(Debug, Clone, PartialEq)]
pub struct EloSide {
    pub team_id: Uuid,
    /// Cross-season identity (`teams.natstat_id`).
    pub key: String,
}

/// One played game.
#[derive(Debug, Clone, PartialEq)]
pub struct EloGame {
    pub game_id: Uuid,
    pub season: i32,
    pub game_date: NaiveDate,
    pub neutral: bool,
    /// `None` for a non-Division I side.
    pub home: Option<EloSide>,
    pub away: Option<EloSide>,
    pub home_score: i32,
    pub away_score: i32,
}

/// One team's rating into and out of one game.
#[derive(Debug, Clone, PartialEq)]
pub struct EloRow {
    pub game_id: Uuid,
    pub team_id: Uuid,
    pub team_key: String,
    pub season: i32,
    pub game_date: NaiveDate,
    pub elo_before: f64,
    pub elo_after: f64,
}

/// Run the ratings over `games`, in `(game_date, game_id)` order whatever order
/// they arrive in. Returns one row per Division I side of every game.
pub fn run_elo(games: &[EloGame]) -> Vec<EloRow> {
    let mut order: Vec<&EloGame> = games.iter().collect();
    // Same-day games of one team are rare but real; `game_id` breaks the tie
    // the way the experiment's `ORDER BY game_date, id` did (uuid order is
    // byte order in both Postgres and `Uuid`'s `Ord`).
    order.sort_by_key(|g| (g.game_date, g.game_id));

    let mut rating: HashMap<&str, f64> = HashMap::new();
    let mut seen_season: HashMap<&str, i32> = HashMap::new();
    let mut rows = Vec::with_capacity(games.len() * 2);

    for g in order {
        let h = pre_game(&mut rating, &mut seen_season, g.home.as_ref(), g.season);
        let a = pre_game(&mut rating, &mut seen_season, g.away.as_ref(), g.season);

        let edge = if g.neutral { 0.0 } else { ELO_HOME_EDGE };
        let expected_home = 1.0 / (1.0 + 10f64.powf((a - h - edge) / 400.0));
        let margin = g.home_score - g.away_score;
        let home_won = if margin > 0 { 1.0 } else { 0.0 };
        let winner_edge = if margin > 0 {
            h + edge - a
        } else {
            a - h - edge
        };
        let mult = (f64::from(margin.abs()) + 1.0).ln() * 2.2 / (winner_edge * 0.001 + 2.2);
        let delta = ELO_K * mult * (home_won - expected_home);

        for (side, before, after) in [(&g.home, h, h + delta), (&g.away, a, a - delta)] {
            if let Some(s) = side {
                rating.insert(s.key.as_str(), after);
                rows.push(EloRow {
                    game_id: g.game_id,
                    team_id: s.team_id,
                    team_key: s.key.clone(),
                    season: g.season,
                    game_date: g.game_date,
                    elo_before: before,
                    elo_after: after,
                });
            }
        }
    }
    rows
}

/// A side's rating going into a game, applying the carry-over at the team's
/// first game of a season (`own_elo`'s `seen_season` rule).
fn pre_game<'a>(
    rating: &mut HashMap<&'a str, f64>,
    seen_season: &mut HashMap<&'a str, i32>,
    side: Option<&'a EloSide>,
    season: i32,
) -> f64 {
    let Some(s) = side else {
        return ELO_NON_D1;
    };
    let key = s.key.as_str();
    if seen_season.get(key) != Some(&season) {
        if let Some(r) = rating.get_mut(key) {
            *r = carry_over(*r);
        }
        seen_season.insert(key, season);
    }
    *rating.get(key).unwrap_or(&ELO_INIT)
}

type GameRecord = (
    Uuid,
    i32,
    NaiveDate,
    bool,
    Option<Uuid>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    i32,
    i32,
);

/// Every finished game of every season, with each side's cross-season key.
pub async fn load_games(pool: &PgPool) -> Result<Vec<EloGame>, sqlx::Error> {
    let rows: Vec<GameRecord> = sqlx::query_as(
        "SELECT g.id, g.season, g.game_date, g.is_neutral_site,
                g.home_team_id, ht.natstat_id, g.away_team_id, at.natstat_id,
                g.home_score, g.away_score
           FROM games g
           LEFT JOIN teams ht ON ht.id = g.home_team_id
           LEFT JOIN teams at ON at.id = g.away_team_id
          WHERE g.status = 'Final'
            AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let side = |id: Option<Uuid>, key: Option<String>| match (id, key) {
        (Some(team_id), Some(key)) => Some(EloSide { team_id, key }),
        _ => None,
    };
    Ok(rows
        .into_iter()
        .map(
            |(game_id, season, game_date, neutral, hid, hk, aid, ak, hs, as_)| EloGame {
                game_id,
                season,
                game_date,
                neutral,
                home: side(hid, hk),
                away: side(aid, ak),
                home_score: hs,
                away_score: as_,
            },
        )
        .collect())
}

/// What one refresh did.
#[derive(Debug, Default, Clone, Copy)]
pub struct TeamEloReport {
    pub games: usize,
    pub rows: usize,
    pub written: usize,
    pub deleted: usize,
}

impl std::fmt::Display for TeamEloReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "in-house Elo: {} games, {} team-game rows, {} written, {} deleted",
            self.games, self.rows, self.written, self.deleted
        )
    }
}

type StoredRow = (Uuid, Uuid, String, i32, NaiveDate, f64, f64);

/// Recompute the ratings over every season and bring `team_game_elo` in line.
///
/// The whole history is recomputed every time: a late box score, a corrected
/// swap or a deleted game anywhere moves every later rating of the teams
/// involved, and the run is about 70k games of arithmetic. Only rows that
/// changed are written, so a nightly run touches the night's games and little
/// else, and the first run is the backfill.
pub async fn refresh_team_elo(pool: &PgPool) -> Result<TeamEloReport, sqlx::Error> {
    let games = load_games(pool).await?;
    let rows = run_elo(&games);

    let stored: Vec<StoredRow> = sqlx::query_as(
        "SELECT game_id, team_id, team_key, season, game_date, elo_before, elo_after
           FROM team_game_elo",
    )
    .fetch_all(pool)
    .await?;
    let stored: HashMap<(Uuid, Uuid), StoredRow> =
        stored.into_iter().map(|r| ((r.0, r.1), r)).collect();

    let changed: Vec<&EloRow> = rows
        .iter()
        .filter(|r| match stored.get(&(r.game_id, r.team_id)) {
            None => true,
            Some(s) => {
                s.2 != r.team_key
                    || s.3 != r.season
                    || s.4 != r.game_date
                    || s.5.to_bits() != r.elo_before.to_bits()
                    || s.6.to_bits() != r.elo_after.to_bits()
            }
        })
        .collect();
    let live: std::collections::HashSet<(Uuid, Uuid)> =
        rows.iter().map(|r| (r.game_id, r.team_id)).collect();
    let (gone_games, gone_teams): (Vec<Uuid>, Vec<Uuid>) =
        stored.keys().filter(|k| !live.contains(k)).copied().unzip();

    let mut tx = pool.begin().await?;
    if !gone_games.is_empty() {
        sqlx::query(
            "DELETE FROM team_game_elo e
              USING UNNEST($1::uuid[], $2::uuid[]) AS d(game_id, team_id)
              WHERE e.game_id = d.game_id AND e.team_id = d.team_id",
        )
        .bind(&gone_games)
        .bind(&gone_teams)
        .execute(&mut *tx)
        .await?;
    }
    for chunk in changed.chunks(2000) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO team_game_elo \
             (game_id, team_id, team_key, season, game_date, elo_before, elo_after) ",
        );
        qb.push_values(chunk, |mut b, r| {
            b.push_bind(r.game_id)
                .push_bind(r.team_id)
                .push_bind(&r.team_key)
                .push_bind(r.season)
                .push_bind(r.game_date)
                .push_bind(r.elo_before)
                .push_bind(r.elo_after);
        });
        qb.push(
            " ON CONFLICT (game_id, team_id) DO UPDATE SET \
             team_key = EXCLUDED.team_key, season = EXCLUDED.season, \
             game_date = EXCLUDED.game_date, elo_before = EXCLUDED.elo_before, \
             elo_after = EXCLUDED.elo_after",
        );
        qb.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;

    let report = TeamEloReport {
        games: games.len(),
        rows: rows.len(),
        written: changed.len(),
        deleted: gone_games.len(),
    };
    info!("{report}");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn side(n: u128, key: &str) -> Option<EloSide> {
        Some(EloSide {
            team_id: Uuid::from_u128(n),
            key: key.to_string(),
        })
    }

    /// Pinned to the experiment's `own_elo`: these `(game, home before, away
    /// before, delta)` values are what `own_elo(df, 20, 100, 0.85)` returns on
    /// this fixture (training/experiments/experiment_elo_source.py, #507). It
    /// covers a non-D1 side at home and away, neutral sites, an away win, a
    /// team's second game on one day (ordered by game id), the carry-over at a
    /// new season, a team that sits out seasons (one carry-over, not two) and a
    /// team first seen mid-history.
    #[test]
    fn reproduces_the_experiments_own_elo() {
        #[allow(clippy::type_complexity)]
        let fixture: [(u128, i32, &str, bool, Option<&str>, Option<&str>, i32, i32); 12] = [
            (
                1,
                2015,
                "2014-11-14",
                false,
                Some("AAA"),
                Some("BBB"),
                80,
                70,
            ),
            (2, 2015, "2014-11-14", true, Some("CCC"), None, 95, 60),
            (
                3,
                2015,
                "2014-11-20",
                false,
                Some("BBB"),
                Some("CCC"),
                61,
                75,
            ),
            (4, 2015, "2014-11-20", false, None, Some("DDD"), 70, 71),
            (
                5,
                2015,
                "2014-12-01",
                true,
                Some("AAA"),
                Some("CCC"),
                66,
                64,
            ),
            (
                6,
                2015,
                "2014-12-01",
                false,
                Some("DDD"),
                Some("AAA"),
                90,
                55,
            ),
            (
                7,
                2015,
                "2015-03-10",
                false,
                Some("BBB"),
                Some("DDD"),
                50,
                52,
            ),
            (
                8,
                2016,
                "2015-11-13",
                false,
                Some("AAA"),
                Some("BBB"),
                70,
                88,
            ),
            (
                9,
                2016,
                "2015-11-13",
                true,
                Some("CCC"),
                Some("EEE"),
                77,
                78,
            ),
            (10, 2016, "2016-01-05", false, Some("CCC"), None, 100, 40),
            (
                11,
                2018,
                "2017-11-10",
                false,
                Some("DDD"),
                Some("AAA"),
                60,
                59,
            ),
            (
                12,
                2018,
                "2017-11-12",
                true,
                Some("EEE"),
                Some("DDD"),
                81,
                80,
            ),
        ];
        let expected: [(u128, f64, f64, f64); 12] = [
            (1, 1500.0, 1500.0, 16.51121876584481),
            (2, 1500.0, 1200.0, 9.522270605522309),
            (3, 1483.488781234155, 1509.5222706055224, -33.89994969245198),
            (4, 1200.0, 1500.0, -3.0530635742718784),
            (5, 1516.511218765845, 1543.4222202979745, 11.981933630740235),
            (6, 1503.0530635742718, 1528.4931523965852, 27.33446416016969),
            (
                7,
                1449.588831541703,
                1530.3875277344416,
                -11.694732356576813,
            ),
            (8, 1500.984885000953, 1447.2099843073572, -44.8201012360216),
            (9, 1526.724243667149, 1500.0, -7.555359469971191),
            (10, 1519.168884197178, 1200.0, 5.676138017428412),
            (
                11,
                1535.7699210773658,
                1462.7400662001917,
                3.466484547257779,
            ),
            (
                12,
                1506.4220555494755,
                1539.2364056246236,
                7.699026589542292,
            ),
        ];
        let team = |k: Option<&str>| {
            k.map(|k| EloSide {
                team_id: Uuid::from_u128(u128::from(k.as_bytes()[0])),
                key: k.to_string(),
            })
        };
        let mut games: Vec<EloGame> = fixture
            .iter()
            .map(|&(id, season, date, neutral, h, a, hs, as_)| EloGame {
                game_id: Uuid::from_u128(id),
                season,
                game_date: d(date),
                neutral,
                home: team(h),
                away: team(a),
                home_score: hs,
                away_score: as_,
            })
            .collect();
        // Arrival order must not matter.
        games.reverse();
        let rows = run_elo(&games);

        let close = |x: f64, y: f64| (x - y).abs() < 1e-9;
        let mut checked = 0;
        for (id, h, a, delta) in expected {
            let g = &fixture[(id - 1) as usize];
            let get = |k: Option<&str>| {
                k.map(|k| {
                    rows.iter()
                        .find(|r| r.game_id == Uuid::from_u128(id) && r.team_key == k)
                        .expect("row for a D1 side")
                })
            };
            if let Some(r) = get(g.4) {
                assert!(
                    close(r.elo_before, h),
                    "game {id} home before {}",
                    r.elo_before
                );
                assert!(close(r.elo_after, h + delta), "game {id} home after");
                checked += 1;
            }
            if let Some(r) = get(g.5) {
                assert!(
                    close(r.elo_before, a),
                    "game {id} away before {}",
                    r.elo_before
                );
                assert!(close(r.elo_after, a - delta), "game {id} away after");
                checked += 1;
            }
        }
        // The three non-D1 sides write no row.
        assert_eq!(checked, 21);
        assert_eq!(rows.len(), 21);
    }

    #[test]
    fn a_rating_entering_a_season_matches_the_sequential_update() {
        // AAA's last 2015 game leaves it at x; its first 2016 game must start
        // from carry_over(x), which is what the point-in-time reader computes
        // from the stored row.
        let games = vec![
            EloGame {
                game_id: Uuid::from_u128(1),
                season: 2015,
                game_date: d("2015-03-01"),
                neutral: false,
                home: side(1, "AAA"),
                away: None,
                home_score: 90,
                away_score: 50,
            },
            EloGame {
                game_id: Uuid::from_u128(2),
                season: 2016,
                game_date: d("2015-11-10"),
                neutral: false,
                home: side(1, "AAA"),
                away: side(2, "BBB"),
                home_score: 60,
                away_score: 70,
            },
        ];
        let rows = run_elo(&games);
        let last_2015 = rows.iter().find(|r| r.season == 2015).unwrap();
        let first_2016 = rows
            .iter()
            .find(|r| r.season == 2016 && r.team_key == "AAA")
            .unwrap();
        assert_eq!(
            rating_entering(last_2015.elo_after, 2015, 2016),
            first_2016.elo_before
        );
    }

    #[test]
    fn carry_over_regresses_toward_the_mean() {
        assert_eq!(carry_over(1500.0), 1500.0);
        assert!((carry_over(1700.0) - 1670.0).abs() < 1e-12);
        assert!((carry_over(1300.0) - 1330.0).abs() < 1e-12);
        assert_eq!(rating_entering(1700.0, 2026, 2026), 1700.0);
        assert!((rating_entering(1700.0, 2024, 2026) - 1670.0).abs() < 1e-12);
    }
}
