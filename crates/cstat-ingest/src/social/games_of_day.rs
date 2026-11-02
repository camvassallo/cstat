//! The morning slate: tonight's best games by watchability, and a closer look
//! at the best one. Also keeps every game's pregame numbers for the recaps.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde_json::{Value, json};
use tracing::info;
use uuid::Uuid;

use super::site::{SlateGame, TeamProjection, format_tip};
use super::{Ctx, Image, Item, pct, round1, store};

/// How many games the slate post lists.
const SLATE_SIZE: usize = 4;

pub(super) async fn build(ctx: &Ctx<'_>, date: NaiveDate) -> anyhow::Result<Vec<Item>> {
    let slate = ctx.site.schedule(date).await?;
    if !ctx.opts.dry_run {
        let n = store::save_snapshots(ctx.pool, &slate.games).await?;
        info!(%date, snapshots = n, "kept pregame numbers");
    }

    let mut upcoming: Vec<&SlateGame> = slate
        .games
        .iter()
        .filter(|g| !g.is_final() && g.watchability.is_some())
        .collect();
    upcoming.sort_by(|a, b| {
        let w = |g: &SlateGame| g.watchability.unwrap_or(0.0);
        w(b).total_cmp(&w(a))
    });
    if upcoming.is_empty() {
        return Ok(vec![]);
    }

    // The Future page's numbers, for context on teams that have barely played.
    let season = crate::season_for_date(date);
    let projections = index_projections(ctx.site.projections(season).await.unwrap_or_default());
    let ranked = slate.ranked_teams;

    let top: Vec<Value> = upcoming
        .iter()
        .take(SLATE_SIZE)
        .map(|g| game_facts(g, None, ranked))
        .collect();
    let best = upcoming[0];
    let card = Image {
        url: ctx.site.matchup_card(best.game_id, date),
        alt: format!("{} vs {}: our projection", best.away(), best.home()),
    };
    let day = date.format("%a %b %-d").to_string();

    let slate_item = Item {
        kind: "games_of_day",
        source_key: date.to_string(),
        title: format!("Games of the day · {day}"),
        summary: format!(
            "{} games on the slate; top {} by watchability",
            slate.games.len(),
            top.len()
        ),
        brief: format!(
            "A \"games to watch tonight\" list tweet for {day}. One short line per game: the \
             matchup with our ranks, the tip time and network, and a few words on why it is worth \
             watching (two good teams, a close projection, or both). Keep the whole tweet within \
             270 characters, dropping the last game if needed. Watchability is our 0-100 score for \
             how good and how close a game should be; mention it at most once."
        ),
        facts: json!({ "date": day, "ranked_teams": ranked, "games": top }),
        images: vec![card.clone()],
        links: vec![("Full schedule".into(), ctx.site.schedule_url(date))],
        fallback: vec![slate_template(&upcoming)],
    };

    let mut best_facts = game_facts(best, Some(&projections), ranked);
    best_facts["date"] = json!(day);
    let mut links = vec![("Schedule".into(), ctx.site.schedule_url(date))];
    for (id, name) in [
        (best.away_team_id, best.away()),
        (best.home_team_id, best.home()),
    ] {
        if let Some(id) = id {
            links.push((name.to_string(), ctx.site.team_url(id)));
        }
    }
    let best_item = Item {
        kind: "game_of_day",
        source_key: date.to_string(),
        title: format!("Game of the day · {}", matchup(best)),
        summary: [
            best.tip_time.map(format_tip),
            best.network.clone(),
            best.watchability.map(|w| format!("watchability {w:.0}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · "),
        brief: "A single tweet previewing tonight's best game. Lead with our projection (the \
                favorite and their win probability, and the projected score when there is one), \
                then one specific reason it is worth watching from the facts: the ranks, a \
                preseason projection band, a new head coach, or how many players return. Include \
                the tip time and network."
            .into(),
        facts: best_facts,
        images: vec![card],
        links,
        fallback: vec![best_template(best)],
    };

    Ok(vec![slate_item, best_item])
}

/// Future-page rows keyed by team id and by name. The page can key a team by
/// last season's id until this season's rows are ingested, so the name is the
/// fallback that still finds it.
pub(super) fn index_projections(rows: Vec<TeamProjection>) -> HashMap<String, TeamProjection> {
    let mut out = HashMap::with_capacity(rows.len() * 2);
    for p in rows {
        out.insert(p.team_name.clone(), p.clone());
        out.insert(p.team_id.to_string(), p);
    }
    out
}

/// What a draft may say about one game. With `projections`, adds each team's
/// preseason band and roster turnover from the Future page.
pub(super) fn game_facts(
    g: &SlateGame,
    projections: Option<&HashMap<String, TeamProjection>>,
    ranked_teams: usize,
) -> Value {
    let side = |id: Option<Uuid>, name: &str, rank: Option<usize>| {
        let mut v = json!({ "team": name, "our_rank": rank, "of_ranked_teams": ranked_teams });
        let p = projections.and_then(|m| {
            id.and_then(|id| m.get(&id.to_string()))
                .or_else(|| m.get(name))
        });
        if let Some(p) = p {
            v["conference"] = json!(p.conference);
            v["preseason_projected_adj_em"] = p.midpoint_adj_em.map(round1).into();
            v["coach"] = json!(p.coach_name);
            v["first_year_head_coach"] = json!(p.coach_is_new_hc);
            v["returning_players"] = json!(p.returning_count);
            v["incoming_transfers"] = json!(p.arrivals_count);
            v["departures"] = json!(p.departures_count);
            v["freshman_recruits"] = json!(p.recruits_count);
            v["adj_em_means"] = json!("points per 100 possessions better than an average D-I team");
        }
        v
    };
    let mut v = json!({
        "matchup": matchup(g),
        "away": side(g.away_team_id, g.away(), g.away_rank),
        "home": side(g.home_team_id, g.home(), g.home_rank),
        "neutral_site": g.is_neutral_site,
        "conference_game": g.is_conference,
        "venue": g.venue,
        "tip": g.tip_time.map(format_tip),
        "network": g.network,
        "watchability": g.watchability.map(|w| w.round()),
    });
    if let (Some(p), Some(m)) = (g.home_win_probability, g.predicted_margin) {
        let (fav, fav_p) = if p >= 0.5 {
            (g.home(), p)
        } else {
            (g.away(), 1.0 - p)
        };
        v["our_favorite"] = json!(fav);
        v["favorite_win_probability"] = json!(pct(fav_p));
        v["projected_margin"] = round1(m.abs());
        v["projection_basis"] = json!(match g.prediction_basis.as_deref() {
            Some("preseason") => "preseason ratings (one or both teams have not played)",
            _ => "our game model",
        });
    }
    if let (Some(h), Some(a)) = (g.predicted_home_score, g.predicted_away_score) {
        v["projected_score"] = json!(format!("{} {a}, {} {h}", g.away(), g.home()));
    }
    v
}

/// "Purdue vs Gonzaga" on a neutral floor, "Purdue at Gonzaga" otherwise.
pub(super) fn matchup(g: &SlateGame) -> String {
    let sep = if g.is_neutral_site { "vs" } else { "at" };
    format!("{} {sep} {}", g.away(), g.home())
}

fn ranked_name(name: &str, rank: Option<usize>) -> String {
    match rank {
        Some(r) if r <= 25 => format!("No. {r} {name}"),
        _ => name.to_string(),
    }
}

fn slate_template(games: &[&SlateGame]) -> String {
    let mut out = String::from("Tonight's best games by our watchability score (ranks are ours):");
    for g in games.iter().take(SLATE_SIZE) {
        let sep = if g.is_neutral_site { "vs" } else { "at" };
        let when = [g.tip_time.map(format_tip), g.network.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\n• {} {sep} {}{}",
            ranked_name(g.away(), g.away_rank),
            ranked_name(g.home(), g.home_rank),
            if when.is_empty() {
                String::new()
            } else {
                format!(" ({when})")
            }
        ));
    }
    out
}

fn best_template(g: &SlateGame) -> String {
    let mut out = format!("Game of the day: {}.", matchup(g));
    if let Some(p) = g.home_win_probability {
        let (fav, fp) = if p >= 0.5 {
            (g.home(), p)
        } else {
            (g.away(), 1.0 - p)
        };
        out.push_str(&format!(" We give {fav} a {} chance.", pct(fp)));
    }
    if let Some(t) = g.tip_time {
        out.push_str(&format!(" {}", format_tip(t)));
        if let Some(n) = &g.network {
            out.push_str(&format!(", {n}"));
        }
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn game() -> SlateGame {
        serde_json::from_value(json!({
            "game_id": "d0fa49ea-a2fc-4c75-8671-6dd86ad8e770",
            "game_date": "2026-11-02",
            "tip_time": "19:30:00",
            "network": "TNT",
            "home_team_id": "44209968-e320-4655-a7e1-d30188ae78ab",
            "home_team_name": "Gonzaga",
            "away_team_id": "e6dfdbd0-555c-4b43-88af-9bb80bf28005",
            "away_team_name": "Purdue",
            "home_score": null, "away_score": null,
            "is_neutral_site": true, "is_conference": false,
            "venue": "T-Mobile Arena", "overtime": null,
            "home_rank": 5, "away_rank": 12,
            "predicted_margin": 2.2, "home_win_probability": 0.579,
            "predicted_home_score": null, "predicted_away_score": null,
            "prediction_basis": "preseason", "watchability": 85.2
        }))
        .unwrap()
    }

    #[test]
    fn facts_name_the_favorite_from_either_side() {
        let g = game();
        let f = game_facts(&g, None, 293);
        assert_eq!(f["matchup"], "Purdue vs Gonzaga");
        assert_eq!(f["our_favorite"], "Gonzaga");
        assert_eq!(f["favorite_win_probability"], "58%");
        assert_eq!(f["tip"], "7:30 PM ET");

        let mut away_fav = game();
        away_fav.home_win_probability = Some(0.3);
        assert_eq!(game_facts(&away_fav, None, 293)["our_favorite"], "Purdue");
    }

    #[test]
    fn templates_pass_the_fact_check() {
        let g = game();
        let facts = game_facts(&g, None, 293);
        let t = best_template(&g);
        assert!(
            super::super::factcheck::unverified_numbers(&t, &facts).is_empty(),
            "{t}"
        );
        let s = slate_template(&[&g]);
        assert!(
            s.contains("No. 12 Purdue vs No. 5 Gonzaga (7:30 PM ET, TNT)"),
            "{s}"
        );
    }
}
