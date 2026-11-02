//! Recaps of one night's biggest results: upsets against our pregame numbers,
//! games between two good teams, and finishes that went down to the wire.
//!
//! The pregame number comes from `pregame_snapshots`, captured the morning of
//! the game, never from the Schedule tab's projection for a finished game: the
//! nightly rebuilds that from season aggregates that already include the
//! result. A game with no snapshot can still be a recap for being a big game,
//! but never an upset call.

use chrono::NaiveDate;
use cstat_core::queries;
use serde_json::{Value, json};
use uuid::Uuid;

use super::site::SlateGame;
use super::store::{self, Snapshot};
use super::{Ctx, Image, Item, pct, round1};

/// Recaps per night, best first.
const MAX_RECAPS: usize = 3;
/// A winner we gave less than this chance is an upset.
const UPSET_BELOW: f64 = 0.25;
/// Below this interest score a game is not worth a post.
const MIN_INTEREST: f64 = 20.0;
/// "Both teams good" means both inside this rank.
const GOOD_RANK: usize = 50;

pub(super) async fn build(ctx: &Ctx<'_>, date: NaiveDate) -> anyhow::Result<Vec<Item>> {
    let season = crate::season_for_date(date);
    let slate = ctx.site.schedule(date).await?;
    let finals: Vec<&SlateGame> = slate.games.iter().filter(|g| g.is_final()).collect();
    let ids: Vec<Uuid> = finals.iter().map(|g| g.game_id).collect();
    let snaps = store::load_snapshots(ctx.pool, &ids).await?;

    let mut scored: Vec<(f64, &SlateGame, Option<&Snapshot>)> = finals
        .iter()
        .map(|g| {
            let snap = snaps.get(&g.game_id);
            (interest(g, snap), *g, snap)
        })
        .filter(|(s, _, _)| *s >= MIN_INTEREST)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut items = Vec::new();
    for (score, g, snap) in scored.into_iter().take(MAX_RECAPS) {
        let boxes = queries::get_player_game_boxes(ctx.pool, &[g.game_id]).await?;
        let mut top = boxes;
        top.sort_by(|a, b| {
            b.game_score
                .unwrap_or(f64::MIN)
                .total_cmp(&a.game_score.unwrap_or(f64::MIN))
        });
        let standouts: Vec<Value> = top
            .iter()
            .take(2)
            .map(|b| {
                let team = if Some(b.team_id) == g.home_team_id {
                    g.home()
                } else {
                    g.away()
                };
                json!({
                    "player": b.player_name,
                    "player_id": b.player_id,
                    "team": team,
                    "points": b.points,
                    "rebounds": b.total_rebounds,
                    "assists": b.assists,
                })
            })
            .collect();

        let mut facts = recap_facts(g, snap);
        facts["standout_players"] = json!(standouts);
        let upset = facts["upset"].as_bool().unwrap_or(false);
        let (winner, loser) = winner_loser(g);

        let mut links = vec![("Schedule".to_string(), ctx.site.schedule_url(date))];
        for (id, name) in [(g.away_team_id, g.away()), (g.home_team_id, g.home())] {
            if let Some(id) = id {
                links.push((name.to_string(), ctx.site.team_url(id)));
            }
        }
        items.push(Item {
            kind: "recap",
            source_key: g.game_id.to_string(),
            title: format!(
                "{} · {winner} over {loser}",
                if upset { "Upset" } else { "Recap" }
            ),
            summary: format!("{} · interest {score:.0}", final_line(g)),
            brief: if upset {
                "A single tweet on an upset. Give the final and how unlikely it was by our \
                 pregame numbers. Research why it happened: predict_matchup for what our model \
                 weighed going in, team_profile for both sides (was the winner quietly playing \
                 better than its rank, has the loser been slipping against our numbers), \
                 player_profile for the standout (a career night, or a habit). Dry, not gloating."
                    .into()
            } else {
                "A single tweet recapping a big result. Give the final and the one thing that \
                 made it notable. Research it: team_profile for both sides (streaks, how they did \
                 against our pregame numbers lately), player_profile for the standout (new high, \
                 streak), predict_matchup for prior meetings if there is a series."
                    .into()
            },
            facts,
            images: vec![Image {
                url: ctx.site.matchup_card(g.game_id, date),
                alt: format!("{}: final and our pregame projection", final_line(g)),
            }],
            links,
            fallback: vec![template(g, snap)],
            season,
        });
    }
    Ok(items)
}

/// How post-worthy a final is, 0-100ish: an upset counts most, then a game
/// between two good teams, then a close or overtime finish.
fn interest(g: &SlateGame, snap: Option<&Snapshot>) -> f64 {
    let mut s = 0.0;
    if let Some(p) = snap.and_then(|s| winner_prob(g, s)) {
        // A coin flip scores 0; a 10% winner scores 64.
        s += ((0.5 - p).max(0.0) * 160.0).min(80.0);
    }
    let rank = |r: Option<usize>| r.is_some_and(|r| r <= GOOD_RANK);
    if rank(g.home_rank) && rank(g.away_rank) {
        s += 30.0;
    } else if rank(g.home_rank) || rank(g.away_rank) {
        s += 8.0;
    }
    let margin = (g.home_score.unwrap_or(0) - g.away_score.unwrap_or(0)).abs();
    if g.overtime.as_deref().is_some_and(|o| !o.is_empty()) {
        s += 15.0;
    } else if margin <= 3 {
        s += 10.0;
    }
    s
}

/// Our pregame chance for the team that won.
fn winner_prob(g: &SlateGame, snap: &Snapshot) -> Option<f64> {
    let home_won = g.home_score? > g.away_score?;
    Some(if home_won {
        snap.home_win_prob
    } else {
        1.0 - snap.home_win_prob
    })
}

fn winner_loser(g: &SlateGame) -> (&str, &str) {
    if g.home_score > g.away_score {
        (g.home(), g.away())
    } else {
        (g.away(), g.home())
    }
}

/// "Purdue 81, Gonzaga 74 (OT)", winner first.
fn final_line(g: &SlateGame) -> String {
    let (h, a) = (g.home_score.unwrap_or(0), g.away_score.unwrap_or(0));
    let (w, ws, l, ls) = if h > a {
        (g.home(), h, g.away(), a)
    } else {
        (g.away(), a, g.home(), h)
    };
    let ot = g
        .overtime
        .as_deref()
        .filter(|o| !o.is_empty())
        .map(|o| format!(" ({o})"))
        .unwrap_or_default();
    format!("{w} {ws}, {l} {ls}{ot}")
}

fn recap_facts(g: &SlateGame, snap: Option<&Snapshot>) -> Value {
    let (winner, loser) = winner_loser(g);
    let (h, a) = (g.home_score.unwrap_or(0), g.away_score.unwrap_or(0));
    let mut v = json!({
        "final": final_line(g),
        "winner": winner,
        "loser": loser,
        "winning_margin": (h - a).abs(),
        "overtime": g.overtime.as_deref().filter(|o| !o.is_empty()),
        "neutral_site": g.is_neutral_site,
        "home_team": g.home(),
        "home_team_id": g.home_team_id,
        "away_team_id": g.away_team_id,
        "conference_game": g.is_conference,
        "venue": g.venue,
        "upset": false,
    });
    if let Some(s) = snap {
        let rank_of = |team: &str| {
            if team == g.home() {
                s.home_rank
            } else {
                s.away_rank
            }
        };
        v["winner_our_rank_before_the_game"] = json!(rank_of(winner));
        v["loser_our_rank_before_the_game"] = json!(rank_of(loser));
        if let Some(p) = winner_prob(g, s) {
            v["our_pregame_win_probability_for_winner"] = json!(pct(p));
            v["upset"] = json!(p < UPSET_BELOW);
        }
        let home_fav = s.home_win_prob >= 0.5;
        v["our_pregame_favorite"] = json!(if home_fav { g.home() } else { g.away() });
        v["our_projected_margin"] = round1(s.projected_margin.abs());
        if let (Some(ph), Some(pa)) = (s.projected_home_score, s.projected_away_score) {
            v["our_projected_score"] = json!(format!("{} {pa}, {} {ph}", g.away(), g.home()));
        }
    }
    v
}

fn template(g: &SlateGame, snap: Option<&Snapshot>) -> String {
    let mut out = format!("Final: {}.", final_line(g));
    if let Some(p) = snap.and_then(|s| winner_prob(g, s)) {
        let (w, _) = winner_loser(g);
        out.push_str(&format!(" We gave {w} a {} chance before tipoff.", pct(p)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(home: i32, away: i32) -> SlateGame {
        serde_json::from_value(json!({
            "game_id": "d0fa49ea-a2fc-4c75-8671-6dd86ad8e770",
            "game_date": "2026-11-02", "tip_time": null, "network": null,
            "home_team_id": null, "home_team_name": "Gonzaga",
            "away_team_id": null, "away_team_name": "Purdue",
            "home_score": home, "away_score": away,
            "is_neutral_site": true, "is_conference": false,
            "venue": null, "overtime": null,
            "home_rank": 5, "away_rank": 12,
            "predicted_margin": null, "home_win_probability": null,
            "predicted_home_score": null, "predicted_away_score": null,
            "prediction_basis": null, "watchability": null
        }))
        .unwrap()
    }

    fn snap(home_win_prob: f64) -> Snapshot {
        Snapshot {
            game_id: Uuid::nil(),
            home_win_prob,
            projected_margin: 8.0,
            projected_home_score: Some(80),
            projected_away_score: Some(72),
            home_rank: Some(5),
            away_rank: Some(12),
        }
    }

    #[test]
    fn upsets_outscore_expected_results() {
        let g = game(70, 74); // Purdue won
        let upset = interest(&g, Some(&snap(0.85)));
        let chalk = interest(&g, Some(&snap(0.15)));
        assert!(upset > chalk + 30.0, "{upset} vs {chalk}");

        let f = recap_facts(&g, Some(&snap(0.85)));
        assert_eq!(f["upset"], true);
        assert_eq!(f["our_pregame_win_probability_for_winner"], "15%");
        assert_eq!(f["final"], "Purdue 74, Gonzaga 70");
    }

    #[test]
    fn no_snapshot_means_no_upset_call() {
        let g = game(70, 74);
        let f = recap_facts(&g, None);
        assert_eq!(f["upset"], false);
        assert!(f.get("our_pregame_win_probability_for_winner").is_none());
        // Still a recap: two top-50 teams, a one-possession game.
        assert!(interest(&g, None) >= MIN_INTEREST);
    }

    #[test]
    fn template_passes_the_fact_check() {
        let g = game(70, 74);
        let s = snap(0.85);
        let t = template(&g, Some(&s));
        let f = recap_facts(&g, Some(&s));
        assert!(
            super::super::factcheck::unverified_numbers(&t, &f).is_empty(),
            "{t}"
        );
    }
}
