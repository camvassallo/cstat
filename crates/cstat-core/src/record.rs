//! Grading the Schedule tab's predictions: how far off the projected margins
//! were, and how the picks they imply fared straight up, against the spread,
//! on the moneyline and on the total.
//!
//! Pure, so every rule is pinned by a test without a database. The inputs come
//! from `pregame_projections` (migration 063), the projection each game carried
//! at tipoff, and never from `game_projections`, whose numbers for a played
//! game are recomputed from aggregates that already include it.
//!
//! The rules, all from the home side:
//! - **Straight up**: the side the projected margin favours. A zero margin
//!   falls back to the win probability.
//! - **Against the spread**: the model takes the home side when its margin
//!   beats the line (`margin + home_spread > 0`), the away side when it falls
//!   short, and passes when the two agree exactly. A push is neither a win nor
//!   a loss. Units at the listed price, or the standard -110 without one.
//! - **Moneyline**: the side whose win probability most exceeds the price's
//!   implied probability, and only when one does. The implied probability
//!   keeps the vig, so a bet needs a real edge over the price actually offered.
//!   One unit staked per bet.
//! - **Total**: over when the projected score sums above the line, under when
//!   below. Needs a projected score, which the preseason basis does not have.
//!   Units as for the spread.
//!
//! A game whose two sources dispute the spread ([`lines_disputed`]) loses its
//! spread and moneyline before grading: both are stated from the same side, so
//! a reversed line reverses both. Its total stays, which has no side.

use serde::Serialize;

/// One graded game, everything from the home side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradedGame {
    pub projected_margin: f64,
    pub home_win_prob: f64,
    /// Projected combined points; `None` under the preseason basis.
    pub projected_total: Option<f64>,
    pub is_preseason: bool,
    /// Home score minus away score.
    pub actual_margin: f64,
    pub actual_total: f64,
    /// Sportsbook convention: -4.5 means the home team is favoured by 4.5.
    pub home_spread: Option<f64>,
    /// Prices on the spread and total; `None` grades at -110.
    pub home_spread_odds: Option<i32>,
    pub away_spread_odds: Option<i32>,
    pub home_moneyline: Option<i32>,
    pub away_moneyline: Option<i32>,
    pub total_line: Option<f64>,
    pub over_odds: Option<i32>,
    pub under_odds: Option<i32>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct WinLoss {
    pub wins: u32,
    pub losses: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct WinLossPush {
    pub wins: u32,
    pub losses: u32,
    pub pushes: u32,
    /// Profit in units staked, one unit per bet.
    pub units: f64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct MoneylineRecord {
    pub wins: u32,
    pub losses: u32,
    /// Games with a moneyline the model found no edge on.
    pub passes: u32,
    /// Profit in units, one unit staked per bet.
    pub units: f64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Record {
    /// Games graded.
    pub games: u32,
    /// How many of them were projected from preseason expectations.
    pub preseason_games: u32,
    /// Mean absolute error of the projected margin, all graded games.
    pub mae: Option<f64>,
    /// Games with a spread, and the model's and the spread's MAE on just
    /// those, so the two are compared on the same games.
    pub lined_games: u32,
    pub mae_lined: Option<f64>,
    pub line_mae: Option<f64>,
    pub straight_up: WinLoss,
    pub ats: WinLossPush,
    pub moneyline: MoneylineRecord,
    pub totals: WinLossPush,
}

/// How far apart, in points, two sources must be to call a line wrong.
///
/// Measured on 3,313 games of 2026 against a second book's line: the two named
/// opposite favourites 127 times, nearly always near a pick'em, and those were
/// ordinary disagreements, the feed's favourite winning as often as the other.
/// None was 7 or more apart. Four opening-week 2027 games were (7 to 57).
pub const LINE_DISPUTE_POINTS: f64 = 7.0;

/// Do two spreads name opposite favourites by a wide margin? One of them then
/// has the sides reversed, and a game with a reversed line cannot be graded.
/// Which one is not decided here: the result would be a judgement made after
/// the fact, and our projection would flatter the record.
pub fn lines_disputed(home_spread: f64, check_spread: f64) -> bool {
    home_spread * check_spread < 0.0 && (home_spread - check_spread).abs() >= LINE_DISPUTE_POINTS
}

/// Profit on a winning one-unit bet at American odds.
fn payout(odds: i32) -> f64 {
    if odds > 0 {
        odds as f64 / 100.0
    } else {
        100.0 / (-odds) as f64
    }
}

/// The win probability a price implies, vig included.
fn implied(odds: i32) -> f64 {
    if odds > 0 {
        100.0 / (odds as f64 + 100.0)
    } else {
        let o = (-odds) as f64;
        o / (o + 100.0)
    }
}

/// Profit on a winning bet at -110.
const STANDARD_PAYOUT: f64 = 100.0 / 110.0;

/// Settle a pick against a line. `pick` is the side taken (+1 over/home, -1
/// under/away), `result` the actual value minus the line, same sign, and
/// `prices` the two sides' odds, the +1 side's first.
fn settle(r: &mut WinLossPush, pick: f64, result: f64, prices: (Option<i32>, Option<i32>)) {
    let price = if pick > 0.0 { prices.0 } else { prices.1 };
    if result == 0.0 {
        r.pushes += 1;
    } else if result.signum() == pick.signum() {
        r.wins += 1;
        r.units += price.map_or(STANDARD_PAYOUT, payout);
    } else {
        r.losses += 1;
        r.units -= 1.0;
    }
}

pub fn grade(games: &[GradedGame]) -> Record {
    let mut r = Record::default();
    let mut abs_err = 0.0;
    let (mut abs_err_lined, mut abs_err_line) = (0.0, 0.0);

    for g in games {
        r.games += 1;
        r.preseason_games += g.is_preseason as u32;
        abs_err += (g.projected_margin - g.actual_margin).abs();

        let picks_home = if g.projected_margin != 0.0 {
            g.projected_margin > 0.0
        } else {
            g.home_win_prob >= 0.5
        };
        if picks_home == (g.actual_margin > 0.0) {
            r.straight_up.wins += 1;
        } else {
            r.straight_up.losses += 1;
        }

        if let Some(spread) = g.home_spread {
            r.lined_games += 1;
            abs_err_lined += (g.projected_margin - g.actual_margin).abs();
            abs_err_line += (-spread - g.actual_margin).abs();
            let edge = g.projected_margin + spread;
            if edge != 0.0 {
                settle(
                    &mut r.ats,
                    edge,
                    g.actual_margin + spread,
                    (g.home_spread_odds, g.away_spread_odds),
                );
            }
        }

        if let (Some(h), Some(a)) = (g.home_moneyline, g.away_moneyline) {
            let edge_home = g.home_win_prob - implied(h);
            let edge_away = (1.0 - g.home_win_prob) - implied(a);
            let bet = if edge_home > 0.0 && edge_home >= edge_away {
                Some((true, h))
            } else if edge_away > 0.0 {
                Some((false, a))
            } else {
                None
            };
            match bet {
                None => r.moneyline.passes += 1,
                Some((home, odds)) => {
                    if home == (g.actual_margin > 0.0) {
                        r.moneyline.wins += 1;
                        r.moneyline.units += payout(odds);
                    } else {
                        r.moneyline.losses += 1;
                        r.moneyline.units -= 1.0;
                    }
                }
            }
        }

        if let (Some(line), Some(projected)) = (g.total_line, g.projected_total) {
            let edge = projected - line;
            if edge != 0.0 {
                settle(
                    &mut r.totals,
                    edge,
                    g.actual_total - line,
                    (g.over_odds, g.under_odds),
                );
            }
        }
    }

    if r.games > 0 {
        r.mae = Some(abs_err / r.games as f64);
    }
    if r.lined_games > 0 {
        r.mae_lined = Some(abs_err_lined / r.lined_games as f64);
        r.line_mae = Some(abs_err_line / r.lined_games as f64);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(projected_margin: f64, actual_margin: f64) -> GradedGame {
        GradedGame {
            projected_margin,
            home_win_prob: if projected_margin > 0.0 { 0.7 } else { 0.3 },
            projected_total: None,
            is_preseason: false,
            actual_margin,
            actual_total: 140.0,
            home_spread: None,
            home_spread_odds: None,
            away_spread_odds: None,
            home_moneyline: None,
            away_moneyline: None,
            total_line: None,
            over_odds: None,
            under_odds: None,
        }
    }

    #[test]
    fn mae_and_straight_up() {
        let r = grade(&[game(5.0, 9.0), game(-3.0, 2.0)]);
        assert_eq!(r.games, 2);
        assert_eq!(r.mae, Some((4.0 + 5.0) / 2.0));
        assert_eq!(r.straight_up, WinLoss { wins: 1, losses: 1 });
        assert_eq!(r.lined_games, 0);
        assert_eq!(r.line_mae, None);
    }

    #[test]
    fn ats_takes_the_side_the_margin_beats_the_line_on() {
        // Home -4.5. Model says home by 7: takes home. Home wins by 6: covers.
        let mut g = game(7.0, 6.0);
        g.home_spread = Some(-4.5);
        // Model says home by 2: takes the away side. Home wins by 6: loses.
        let mut h = game(2.0, 6.0);
        h.home_spread = Some(-4.5);
        // Model says home by 3, line home -3: no pick.
        let mut pass = game(3.0, 10.0);
        pass.home_spread = Some(-3.0);
        // Home -3, wins by 3: a push.
        let mut push = game(8.0, 3.0);
        push.home_spread = Some(-3.0);

        let r = grade(&[g, h, pass, push]);
        assert_eq!((r.ats.wins, r.ats.losses, r.ats.pushes), (1, 1, 1));
        assert!((r.ats.units - (STANDARD_PAYOUT - 1.0)).abs() < 1e-12);
        assert_eq!(r.lined_games, 4);
        // The line's own error: |4.5-6| + |4.5-6| + |3-10| + |3-3|.
        assert_eq!(r.line_mae, Some((1.5 + 1.5 + 7.0 + 0.0) / 4.0));
    }

    #[test]
    fn a_listed_price_replaces_the_standard_one() {
        // Takes the home side at -125 and wins: 0.8 units, not 0.909.
        let mut g = game(7.0, 6.0);
        g.home_spread = Some(-4.5);
        g.home_spread_odds = Some(-125);
        g.away_spread_odds = Some(105);
        assert!((grade(&[g]).ats.units - 0.8).abs() < 1e-12);

        // Takes the under at +105 and wins.
        let mut t = game(3.0, 5.0);
        t.projected_total = Some(140.0);
        t.total_line = Some(145.5);
        t.actual_total = 141.0;
        t.over_odds = Some(-125);
        t.under_odds = Some(105);
        assert!((grade(&[t]).totals.units - 1.05).abs() < 1e-12);
    }

    #[test]
    fn moneyline_bets_only_an_edge_over_the_price() {
        // Home -200 implies 0.667. Model 0.70: bet home, win pays 0.5.
        let mut g = game(5.0, 4.0);
        g.home_win_prob = 0.70;
        g.home_moneyline = Some(-200);
        g.away_moneyline = Some(170);
        // Model 0.60 on the same prices: away needs 0.37 (implied of +170),
        // has 0.40 — bet the underdog, which loses.
        let mut h = g;
        h.home_win_prob = 0.60;
        // Model 0.65 on -200 / +170: neither side clears its price.
        let mut pass = g;
        pass.home_win_prob = 0.65;

        let r = grade(&[g, h, pass]);
        assert_eq!(
            (r.moneyline.wins, r.moneyline.losses, r.moneyline.passes),
            (1, 1, 1)
        );
        assert!((r.moneyline.units - (0.5 - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn totals_need_a_projected_score() {
        let mut over = game(3.0, 5.0);
        over.projected_total = Some(150.0);
        over.total_line = Some(145.5);
        over.actual_total = 151.0;
        let mut preseason = over;
        preseason.projected_total = None;
        preseason.is_preseason = true;

        let r = grade(&[over, preseason]);
        assert_eq!((r.totals.wins, r.totals.losses, r.totals.pushes), (1, 0, 0));
        assert_eq!(r.preseason_games, 1);
    }

    #[test]
    fn implied_and_payout_are_the_american_conventions() {
        assert!((implied(-200) - 2.0 / 3.0).abs() < 1e-12);
        assert!((implied(150) - 0.4).abs() < 1e-12);
        assert_eq!(payout(150), 1.5);
        assert_eq!(payout(-200), 0.5);
    }

    #[test]
    fn only_opposite_favourites_by_a_wide_margin_are_disputed() {
        // Florida A&M -28.5 against Florida St. -28.5.
        assert!(lines_disputed(-28.5, 28.5));
        assert!(lines_disputed(-3.5, 3.5));
        // A pick'em either side of zero is an ordinary disagreement.
        assert!(!lines_disputed(-1.5, 1.5));
        assert!(!lines_disputed(-3.0, 2.5));
        // The same favourite, however far apart.
        assert!(!lines_disputed(-30.0, -38.5));
        assert!(!lines_disputed(0.0, 9.0));
    }

    #[test]
    fn empty_is_all_zero() {
        let r = grade(&[]);
        assert_eq!(r.games, 0);
        assert_eq!(r.mae, None);
    }
}
