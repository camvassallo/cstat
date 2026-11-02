//! Pregame watchability score for the Schedule tab (issue #429).
//!
//! One number per game, 0–100, answering "is this worth watching?" before
//! tipoff. It rewards two things and needs both: **quality** (both teams are
//! good) and **competitiveness** (the result is in doubt). A coin flip between
//! two bad teams is not watchable, and neither is a top-five team against a
//! cupcake.
//!
//! **The scale is absolute, not normalized per slate.** The best game on a
//! thin Tuesday does not read 100 just because it tops the day: two top-ten
//! teams in a coin flip approach 100, and a night of mid-majors tops out in
//! the 30s or 40s. The number therefore says whether tonight is worth it, not
//! only which game is the least bad.
//!
//! Display-only and deliberately simple, so a tooltip can say what it means
//! in a sentence. Nothing reads it back; tune the constants by eye on real
//! slates.

/// Mean team rating (AdjEM, points per 100 possessions) at which the quality
/// term is 0.5. On the 2026 board the median team is about −3, the
/// 75th-percentile +10 and the 90th +23, so +5 is a solid mid-major: two of
/// them in a coin flip are a 50, which is what the absolute scale promises
/// a good mid-major night tops out at.
const QUALITY_MIDPOINT: f64 = 5.0;

/// Logistic width of the quality term. At 11 points, two top-ten teams (mean
/// ≈ +32) score ~0.92, two median teams (≈ −3) ~0.33, and two bottom-50
/// teams (≈ −15) ~0.14, so most of the board's range stays readable.
///
/// It was 5 points around a midpoint of +15, which put two median teams at
/// 0.03: on the 2026 opening night the median game scored 1.8 and only four
/// of 78 cleared 20, so the number separated the marquee games from the
/// rest and nothing within the rest.
const QUALITY_WIDTH: f64 = 11.0;

/// Exponent on the closeness term. Below 1 so a moderate favourite (70%) keeps
/// most of the game's quality, while a lock (97%) still falls away: at 0.5 a
/// 70/30 game keeps 77% of its quality and a 97/3 game keeps 24%.
const CLOSENESS_EXPONENT: f64 = 0.5;

/// Quality of a matchup in `[0, 1]` from the two teams' ratings.
///
/// The plain mean, not a min-weighted one: a mismatch is already priced by
/// the closeness term, and counting it twice would bury good-vs-very-good
/// games that are genuinely worth a look.
pub fn quality(home_rating: f64, away_rating: f64) -> f64 {
    let mean = (home_rating + away_rating) / 2.0;
    1.0 / (1.0 + (-(mean - QUALITY_MIDPOINT) / QUALITY_WIDTH).exp())
}

/// Competitiveness in `[0, 1]`: 1 at a coin flip, 0 at a certainty. Symmetric,
/// so it does not matter which side's probability is passed.
pub fn closeness(home_win_prob: f64) -> f64 {
    (1.0 - (2.0 * home_win_prob.clamp(0.0, 1.0) - 1.0).abs()).max(0.0)
}

/// Watchability, 0–100, rounded to one decimal.
///
/// `home_win_prob` is the pregame probability, so a finished game keeps the
/// score it had before tipoff.
pub fn watchability(home_rating: f64, away_rating: f64, home_win_prob: f64) -> f64 {
    let score = 100.0
        * quality(home_rating, away_rating)
        * closeness(home_win_prob).powf(CLOSENESS_EXPONENT);
    (score * 10.0).round() / 10.0
}

/// A team's rating for the schedule (AdjEM scale), from its inputs and the
/// live preseason blend weight.
///
/// The same early-season mix the game predictions use: until a team has
/// played, its preseason projection is the only rating it has; once it has,
/// the season's adjusted margin takes over as the preseason weight decays
/// (0.70 at the November open, zero six weeks in). Past the window this is
/// exactly the AdjEM the Rankings page sorts by, so the two agree on rank.
///
/// `None` when there is nothing to rate: a team that has not played and has
/// no projection (a roster too thin to project). A team that has not played
/// has no meaningful adjusted margin even if a row exists, so it is ignored.
pub fn team_rating(
    adj_em: Option<f64>,
    preseason_adj_em: Option<f64>,
    has_played: bool,
    preseason_weight: f64,
) -> Option<f64> {
    if !has_played {
        return preseason_adj_em;
    }
    match (adj_em, preseason_adj_em) {
        (Some(a), Some(p)) => {
            let w = preseason_weight.clamp(0.0, 1.0);
            Some(w * p + (1.0 - w) * a)
        }
        (Some(a), None) => Some(a),
        (None, p) => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_uses_the_projection_until_a_team_plays_then_blends_it_out() {
        // Not played: the projection, even if a (blank) adjusted margin exists.
        assert_eq!(team_rating(Some(-40.0), Some(20.0), false, 0.7), Some(20.0));
        assert_eq!(team_rating(Some(5.0), None, false, 0.7), None);
        // Played, early: mostly the projection.
        let r = team_rating(Some(0.0), Some(20.0), true, 0.7).unwrap();
        assert!((r - 14.0).abs() < 1e-9);
        // Played, past the window: the adjusted margin alone.
        assert_eq!(team_rating(Some(8.0), Some(20.0), true, 0.0), Some(8.0));
        // Played with no projection, or no margin yet.
        assert_eq!(team_rating(Some(8.0), None, true, 0.7), Some(8.0));
        assert_eq!(team_rating(None, Some(20.0), true, 0.7), Some(20.0));
    }

    #[test]
    fn two_elite_teams_in_a_coin_flip_approach_but_do_not_reach_100() {
        let w = watchability(33.0, 31.0, 0.52);
        assert!(w > 88.0 && w < 100.0, "got {w}");
    }

    #[test]
    fn the_scale_is_absolute_so_a_weak_slate_tops_out_low() {
        // Two median teams in a pure coin flip: worth a look, not a marquee.
        let median = watchability(-3.0, -3.0, 0.5);
        assert!(median > 25.0 && median < 40.0, "got {median}");
        // Two solid mid-majors, close game: the 40s-50s, well short of
        // two elite teams.
        let w = watchability(10.0, 8.0, 0.55);
        assert!(w > 45.0 && w < 65.0, "got {w}");
        // Two bottom-50 teams, even a coin flip, stay low.
        assert!(watchability(-15.0, -15.0, 0.5) < 20.0);
    }

    #[test]
    fn a_close_mid_major_game_outranks_a_top_team_against_a_cupcake() {
        // The ordering the old constants inverted: McNeese-UAB (+5 v +5,
        // 60/40) scored 10.6 and Kansas-Fordham (+32 v 0, 96/4) 15.4.
        assert!(watchability(5.0, 5.0, 0.6) > watchability(32.0, 0.0, 0.96));
    }

    #[test]
    fn a_blowout_is_unwatchable_whatever_the_quality() {
        assert!(watchability(35.0, -5.0, 0.995) < 10.0);
    }

    #[test]
    fn closeness_is_symmetric_and_bounded() {
        for p in [0.0, 0.1, 0.3, 0.5, 0.73, 0.99, 1.0] {
            let c = closeness(p);
            assert!((0.0..=1.0).contains(&c));
            assert!((c - closeness(1.0 - p)).abs() < 1e-12);
        }
        assert_eq!(closeness(0.5), 1.0);
        assert_eq!(closeness(1.0), 0.0);
        // Out-of-range input is clamped rather than producing a negative.
        assert_eq!(closeness(1.4), 0.0);
    }

    #[test]
    fn monotone_in_quality_and_in_closeness() {
        assert!(watchability(20.0, 20.0, 0.6) > watchability(15.0, 15.0, 0.6));
        assert!(watchability(20.0, 20.0, 0.55) > watchability(20.0, 20.0, 0.75));
        // Order of the teams does not matter to quality.
        assert_eq!(quality(30.0, 5.0), quality(5.0, 30.0));
    }
}
