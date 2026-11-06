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
//!   beats the line (`margin + home_spread > 0`) and the away side when it
//!   falls short, and only when its chance of covering beats what the price
//!   needs. At -110 that is a gap of about 0.6 points. A push is neither a win
//!   nor a loss. Units at the listed price, or the standard -110 without one.
//! - **Moneyline**: the side whose win probability most exceeds the price's
//!   implied probability, and only when one does. The implied probability
//!   keeps the vig, so a bet needs a real edge over the price actually offered.
//!   A price longer than +999 is never taken. One unit staked per bet.
//! - **Total**: over when the projected score sums above the line, under when
//!   below, with the same price check, at about 1 point at -110. Needs a
//!   projected score, which the preseason basis does not have. Units as for
//!   the spread.

use crate::projection::{PREDICT_SIGMA_PROD, win_prob_at_sigma};
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

impl GradedGame {
    pub fn lines(&self) -> Lines {
        Lines {
            home_spread: self.home_spread,
            home_spread_odds: self.home_spread_odds,
            away_spread_odds: self.away_spread_odds,
            home_moneyline: self.home_moneyline,
            away_moneyline: self.away_moneyline,
            total: self.total_line,
            over_odds: self.over_odds,
            under_odds: self.under_odds,
        }
    }
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
    /// Games with a line whose chance of covering did not beat the price.
    pub passes: u32,
    /// Profit in units staked, one unit per bet.
    pub units: f64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct MoneylineRecord {
    pub wins: u32,
    pub losses: u32,
    /// Games with a moneyline the model found no edge on, or only one on a
    /// side priced past +999.
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

/// The price assumed where none is listed.
const STANDARD_ODDS: i32 = -110;

/// The longest moneyline price the model will take. Out here the edge is an
/// artifact of the win probability's tail rather than a read on the game: over
/// the first week of 2026-27, 53 bets at +1000 or longer carried 4.3 projected
/// wins against 2.2 implied by their prices, and won 1. A stopgap until those
/// tail probabilities are calibrated against an honest history.
const LONGEST_MONEYLINE: i32 = 999;

/// Residual error of a projected total, from the total models' backtests
/// (`backtest_total.rmse`, 17.2 to 17.3). The spread uses the margin models'.
const TOTAL_SIGMA: f64 = 17.2;

/// Is a pick `edge` points off the line worth its price? The chance of covering
/// is the same curve that turns a margin into a win probability, with the line
/// in place of zero, and it must beat what the price implies.
fn worth_the_price(edge: f64, sigma: f64, price: Option<i32>) -> bool {
    // No edge is no side to take, whatever a plus-money price would allow.
    edge != 0.0
        && win_prob_at_sigma(edge.abs() as f32, sigma) > implied(price.unwrap_or(STANDARD_ODDS))
}

/// The price of the side `pick` takes.
fn picked_price(pick: f64, prices: (Option<i32>, Option<i32>)) -> Option<i32> {
    if pick > 0.0 { prices.0 } else { prices.1 }
}

/// The lines a game's picks are made against, from the home side. Prices are
/// `None` where none is listed, which grades at -110.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Lines {
    /// Sportsbook convention: -4.5 means the home team is favoured by 4.5.
    pub home_spread: Option<f64>,
    pub home_spread_odds: Option<i32>,
    pub away_spread_odds: Option<i32>,
    pub home_moneyline: Option<i32>,
    pub away_moneyline: Option<i32>,
    pub total: Option<f64>,
    pub over_odds: Option<i32>,
    pub under_odds: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Home,
    Away,
    Over,
    Under,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Win,
    Loss,
    Push,
}

/// One market's decision on one game.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Pick {
    /// `None` is a pass: the chance was not worth the price.
    pub side: Option<Side>,
    /// The price of the side taken.
    pub price: Option<i32>,
    /// `None` until the game is graded, and on a pass.
    pub outcome: Option<Outcome>,
    /// Profit on a one-unit stake; 0 on a pass or a push.
    pub units: f64,
}

impl Pick {
    fn pass() -> Self {
        Pick {
            side: None,
            price: None,
            outcome: None,
            units: 0.0,
        }
    }

    fn take(side: Side, price: Option<i32>) -> Self {
        Pick {
            side: Some(side),
            price,
            outcome: None,
            units: 0.0,
        }
    }

    /// Settle against the actual value minus the line, positive for the home
    /// or over side. A moneyline passes the margin, which is never zero.
    fn settled(mut self, result: f64) -> Self {
        let Some(side) = self.side else {
            return self;
        };
        let took_positive = matches!(side, Side::Home | Side::Over);
        let odds = self.price.unwrap_or(STANDARD_ODDS);
        if result == 0.0 {
            self.outcome = Some(Outcome::Push);
        } else if (result > 0.0) == took_positive {
            self.outcome = Some(Outcome::Win);
            self.units = payout(odds);
        } else {
            self.outcome = Some(Outcome::Loss);
            self.units = -1.0;
        }
        self
    }
}

/// Every decision on one game. A market with no line is `None`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct GamePicks {
    /// Whether the projected winner won; `None` until graded.
    pub straight_up: Option<bool>,
    pub spread: Option<Pick>,
    pub moneyline: Option<Pick>,
    pub total: Option<Pick>,
}

impl GamePicks {
    /// Grade the picks against a result.
    pub fn settled(
        mut self,
        picks_home: bool,
        actual_margin: f64,
        actual_total: f64,
        l: &Lines,
    ) -> Self {
        self.straight_up = Some(picks_home == (actual_margin > 0.0));
        if let Some(spread) = l.home_spread {
            self.spread = self.spread.map(|p| p.settled(actual_margin + spread));
        }
        self.moneyline = self.moneyline.map(|p| p.settled(actual_margin));
        if let Some(total) = l.total {
            self.total = self.total.map(|p| p.settled(actual_total - total));
        }
        self
    }
}

/// The side the projected margin favours; a zero margin falls back to the win
/// probability.
pub fn picks_home(projected_margin: f64, home_win_prob: f64) -> bool {
    if projected_margin != 0.0 {
        projected_margin > 0.0
    } else {
        home_win_prob >= 0.5
    }
}

/// What the model takes on each market of a game, before the result. The one
/// definition of a pick: the record panel totals these, and the Schedule tab
/// shows them row by row.
pub fn picks(
    projected_margin: f64,
    home_win_prob: f64,
    projected_total: Option<f64>,
    l: &Lines,
) -> GamePicks {
    let spread = l.home_spread.map(|spread| {
        let edge = projected_margin + spread;
        let prices = (l.home_spread_odds, l.away_spread_odds);
        let price = picked_price(edge, prices);
        if worth_the_price(edge, PREDICT_SIGMA_PROD, price) {
            let side = if edge > 0.0 { Side::Home } else { Side::Away };
            Pick::take(side, price)
        } else {
            Pick::pass()
        }
    });

    let moneyline = l.home_moneyline.zip(l.away_moneyline).map(|(h, a)| {
        let edge = |chance: f64, odds: i32| {
            if odds > LONGEST_MONEYLINE {
                0.0
            } else {
                chance - implied(odds)
            }
        };
        let edge_home = edge(home_win_prob, h);
        let edge_away = edge(1.0 - home_win_prob, a);
        if edge_home > 0.0 && edge_home >= edge_away {
            Pick::take(Side::Home, Some(h))
        } else if edge_away > 0.0 {
            Pick::take(Side::Away, Some(a))
        } else {
            Pick::pass()
        }
    });

    let total = l.total.zip(projected_total).map(|(line, projected)| {
        let edge = projected - line;
        let prices = (l.over_odds, l.under_odds);
        let price = picked_price(edge, prices);
        if worth_the_price(edge, TOTAL_SIGMA, price) {
            let side = if edge > 0.0 { Side::Over } else { Side::Under };
            Pick::take(side, price)
        } else {
            Pick::pass()
        }
    });

    GamePicks {
        straight_up: None,
        spread,
        moneyline,
        total,
    }
}

fn tally(r: &mut WinLossPush, pick: Pick) {
    match pick.outcome {
        None => r.passes += 1,
        Some(Outcome::Push) => r.pushes += 1,
        Some(Outcome::Win) => r.wins += 1,
        Some(Outcome::Loss) => r.losses += 1,
    }
    r.units += pick.units;
}

pub fn grade(games: &[GradedGame]) -> Record {
    let mut r = Record::default();
    let mut abs_err = 0.0;
    let (mut abs_err_lined, mut abs_err_line) = (0.0, 0.0);

    for g in games {
        r.games += 1;
        r.preseason_games += g.is_preseason as u32;
        abs_err += (g.projected_margin - g.actual_margin).abs();

        let l = g.lines();
        let home = picks_home(g.projected_margin, g.home_win_prob);
        let p = picks(g.projected_margin, g.home_win_prob, g.projected_total, &l).settled(
            home,
            g.actual_margin,
            g.actual_total,
            &l,
        );

        if p.straight_up == Some(true) {
            r.straight_up.wins += 1;
        } else {
            r.straight_up.losses += 1;
        }

        if let Some(spread) = l.home_spread {
            r.lined_games += 1;
            abs_err_lined += (g.projected_margin - g.actual_margin).abs();
            abs_err_line += (-spread - g.actual_margin).abs();
        }
        if let Some(pick) = p.spread {
            tally(&mut r.ats, pick);
        }
        if let Some(pick) = p.moneyline {
            match pick.outcome {
                None => r.moneyline.passes += 1,
                Some(Outcome::Win) => r.moneyline.wins += 1,
                Some(_) => r.moneyline.losses += 1,
            }
            r.moneyline.units += pick.units;
        }
        if let Some(pick) = p.total {
            tally(&mut r.totals, pick);
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
        assert_eq!(r.ats.passes, 1);
        assert!((r.ats.units - (payout(STANDARD_ODDS) - 1.0)).abs() < 1e-12);
        assert_eq!(r.lined_games, 4);
        // The line's own error: |4.5-6| + |4.5-6| + |3-10| + |3-3|.
        assert_eq!(r.line_mae, Some((1.5 + 1.5 + 7.0 + 0.0) / 4.0));
    }

    #[test]
    fn a_small_edge_does_not_clear_the_standard_price() {
        // 0.5 points off the line is about 52%; -110 needs 52.4%.
        let mut small = game(5.0, 9.0);
        small.home_spread = Some(-4.5);
        // 1 point is about 54%.
        let mut enough = game(5.5, 9.0);
        enough.home_spread = Some(-4.5);
        let r = grade(&[small, enough]);
        assert_eq!((r.ats.wins, r.ats.passes), (1, 1));
    }

    #[test]
    fn no_edge_is_a_pass_even_at_plus_money() {
        let mut g = game(3.0, 10.0);
        g.home_spread = Some(-3.0);
        g.away_spread_odds = Some(105);
        let r = grade(&[g]);
        assert_eq!((r.ats.wins, r.ats.losses, r.ats.passes), (0, 0, 1));
    }

    #[test]
    fn a_dearer_price_needs_a_bigger_edge() {
        // 1 point clears -110 above, but not -125, which needs 55.6%.
        let mut g = game(5.5, 9.0);
        g.home_spread = Some(-4.5);
        g.home_spread_odds = Some(-125);
        g.away_spread_odds = Some(105);
        let r = grade(&[g]);
        assert_eq!((r.ats.wins, r.ats.passes), (0, 1));
        // The price checked is the side taken: the away side here, at +105,
        // needs only 48.8%.
        let mut h = game(3.5, 2.0);
        h.home_spread = Some(-4.5);
        h.home_spread_odds = Some(-125);
        h.away_spread_odds = Some(105);
        assert_eq!(grade(&[h]).ats.wins, 1);
    }

    #[test]
    fn a_total_needs_a_bigger_edge_than_a_spread() {
        // 0.8 clears nothing on a total, whose error is wider.
        let mut g = game(3.0, 5.0);
        g.projected_total = Some(146.3);
        g.total_line = Some(145.5);
        g.actual_total = 151.0;
        assert_eq!(grade(&[g]).totals.passes, 1);
        g.projected_total = Some(147.0);
        assert_eq!(grade(&[g]).totals.wins, 1);
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
    fn a_pick_is_decided_before_the_result_and_settled_after() {
        let l = Lines {
            home_spread: Some(-4.5),
            home_moneyline: Some(-200),
            away_moneyline: Some(170),
            total: Some(145.5),
            ..Default::default()
        };
        let p = picks(7.0, 0.70, Some(150.0), &l);
        assert_eq!(p.straight_up, None);
        assert_eq!(p.spread.map(|x| x.side), Some(Some(Side::Home)));
        assert_eq!(p.spread.and_then(|x| x.outcome), None);
        assert_eq!(p.moneyline.and_then(|x| x.price), Some(-200));
        assert_eq!(p.total.map(|x| x.side), Some(Some(Side::Over)));

        // Home wins by 3: right straight up, short of the spread, under.
        let s = p.settled(true, 3.0, 141.0, &l);
        assert_eq!(s.straight_up, Some(true));
        assert_eq!(s.spread.and_then(|x| x.outcome), Some(Outcome::Loss));
        assert_eq!(s.moneyline.and_then(|x| x.outcome), Some(Outcome::Win));
        assert_eq!(s.moneyline.map(|x| x.units), Some(0.5));
        assert_eq!(s.total.and_then(|x| x.outcome), Some(Outcome::Loss));
    }

    #[test]
    fn a_value_bet_can_win_while_the_projected_winner_lost() {
        let l = Lines {
            home_moneyline: Some(-1200),
            away_moneyline: Some(800),
            ..Default::default()
        };
        let p = picks(15.0, 0.85, None, &l).settled(true, -4.0, 132.0, &l);
        assert_eq!(p.straight_up, Some(false));
        assert_eq!(p.moneyline.and_then(|x| x.outcome), Some(Outcome::Win));
        assert_eq!(p.moneyline.and_then(|x| x.side), Some(Side::Away));
    }

    /// The East Texas A&M line: 0.023 clears the 0.020 that +5000 implies, and
    /// it is still a pass, however the game went.
    #[test]
    fn a_price_past_the_longest_is_a_pass() {
        let l = Lines {
            home_moneyline: Some(-100000),
            away_moneyline: Some(5000),
            ..Default::default()
        };
        let p = picks(26.0, 0.977, None, &l).settled(true, -4.0, 132.0, &l);
        assert_eq!(p.moneyline.map(|x| (x.side, x.outcome)), Some((None, None)));
    }

    #[test]
    fn the_longest_price_is_inclusive() {
        let lines = |away| Lines {
            home_moneyline: Some(-2000),
            away_moneyline: Some(away),
            ..Default::default()
        };
        let side = |away| {
            picks(20.0, 0.90, None, &lines(away))
                .moneyline
                .map(|x| x.side)
        };
        assert_eq!(side(LONGEST_MONEYLINE), Some(Some(Side::Away)));
        assert_eq!(side(LONGEST_MONEYLINE + 1), Some(None));
    }

    /// A long price does not stop the other side being taken, even when the
    /// long side has the bigger edge: away 0.323 over its price, home 0.076.
    #[test]
    fn a_capped_side_leaves_the_other_side_open() {
        let l = Lines {
            home_moneyline: Some(-110),
            away_moneyline: Some(1200),
            ..Default::default()
        };
        let p = picks(5.0, 0.60, None, &l);
        assert_eq!(p.moneyline.map(|x| x.side), Some(Some(Side::Home)));
    }

    #[test]
    fn a_pass_stays_a_pass_after_the_result() {
        let l = Lines {
            home_spread: Some(-3.0),
            ..Default::default()
        };
        let p = picks(3.0, 0.6, None, &l).settled(true, 10.0, 140.0, &l);
        assert_eq!(p.spread.map(|x| (x.side, x.outcome)), Some((None, None)));
    }

    #[test]
    fn empty_is_all_zero() {
        let r = grade(&[]);
        assert_eq!(r.games, 0);
        assert_eq!(r.mae, None);
    }
}
