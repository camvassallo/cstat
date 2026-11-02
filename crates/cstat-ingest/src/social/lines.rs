//! Lines of the night: the best individual box scores from one date, by game
//! score, as a list post and a spotlight on the best one.

use chrono::NaiveDate;
use cstat_core::queries::{self, TopPerformance};
use serde_json::{Value, json};

use super::{Ctx, Image, Item, round1};

const LIST_SIZE: usize = 5;

pub(super) async fn build(ctx: &Ctx<'_>, date: NaiveDate) -> anyhow::Result<Vec<Item>> {
    let season = crate::season_for_date(date);
    let perfs = queries::get_top_performances(ctx.pool, season, date, LIST_SIZE as i64).await?;
    let Some(best) = perfs.first() else {
        return Ok(vec![]);
    };
    let day = date.format("%a %b %-d").to_string();

    let list = Item {
        kind: "lines",
        source_key: date.to_string(),
        title: format!("Lines of the night · {day}"),
        summary: format!("Top {} by game score", perfs.len()),
        brief: format!(
            "A \"lines of the night\" list tweet for {day}: one line per player, best first, with \
             the name, team and the stat line (points, rebounds, assists, plus steals or blocks \
             when notable), and W or L. Keep the whole tweet within 270 characters, dropping the \
             last player if needed. No commentary beyond a short opener."
        ),
        facts: json!({ "date": day, "lines": perfs.iter().map(facts).collect::<Vec<_>>() }),
        images: vec![],
        links: perfs
            .iter()
            .take(3)
            .map(|p| (p.player_name.clone(), ctx.site.player_url(p.player_id)))
            .collect(),
        fallback: vec![list_template(&perfs)],
    };

    let spotlight = Item {
        kind: "line_of_night",
        source_key: date.to_string(),
        title: format!("Line of the night · {}", best.player_name),
        summary: format!(
            "{} · {} · game score {:.1}",
            best.team_name,
            stat_line(best),
            best.game_score
        ),
        brief: "A single tweet on the best individual performance of the night. Give the stat \
                line and the result, then one piece of context from the facts: a new season high, \
                the player's CAM value or archetype, or the shooting. The card attached shows the \
                player's skill profile."
            .into(),
        facts: facts(best),
        images: vec![Image {
            url: ctx.site.radar_card(best.player_id, season, date),
            alt: format!("{} skill profile", best.player_name),
        }],
        links: vec![(
            best.player_name.clone(),
            ctx.site.player_url(best.player_id),
        )],
        fallback: vec![format!(
            "Line of the night: {} ({}) with {} in a {}.",
            best.player_name,
            best.team_name,
            stat_line(best),
            result(best).unwrap_or_else(|| "game".into())
        )],
    };

    Ok(vec![list, spotlight])
}

fn facts(p: &TopPerformance) -> Value {
    let new_high = match (p.points, p.prior_season_high_points) {
        (Some(pts), Some(prev)) => Some(pts > prev),
        _ => None,
    };
    json!({
        "player": p.player_name,
        "class": p.class_year,
        "position": p.position,
        "team": p.team_name,
        "opponent": p.opponent_name,
        "result": result(p),
        "stat_line": stat_line(p),
        "shooting": shooting(p),
        "minutes": p.minutes.map(|m| m.round()),
        "game_score": round1(p.game_score),
        "season_high_points_before_tonight": p.prior_season_high_points,
        "new_season_high_in_points": new_high,
        "first_game_of_season": p.prior_season_high_points.is_none(),
        "cam_this_season": p.campom.map(round1),
        "archetype": p.archetype,
    })
}

/// "31 pts, 9 reb, 5 ast, 3 stl" — steals and blocks only when they stand out.
fn stat_line(p: &TopPerformance) -> String {
    let mut parts = vec![format!("{} pts", p.points.unwrap_or(0))];
    if let Some(r) = p.total_rebounds.filter(|&r| r > 0) {
        parts.push(format!("{r} reb"));
    }
    if let Some(a) = p.assists.filter(|&a| a > 0) {
        parts.push(format!("{a} ast"));
    }
    if let Some(s) = p.steals.filter(|&s| s >= 3) {
        parts.push(format!("{s} stl"));
    }
    if let Some(b) = p.blocks.filter(|&b| b >= 3) {
        parts.push(format!("{b} blk"));
    }
    parts.join(", ")
}

/// "11-18 FG, 4-7 3PT, 5-6 FT", leaving out zero-attempt categories.
fn shooting(p: &TopPerformance) -> String {
    [
        (p.fgm, p.fga, "FG"),
        (p.tpm, p.tpa, "3PT"),
        (p.ftm, p.fta, "FT"),
    ]
    .into_iter()
    .filter_map(|(m, a, label)| match (m, a) {
        (Some(m), Some(a)) if a > 0 => Some(format!("{m}-{a} {label}")),
        _ => None,
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// "W 81-74 vs Purdue".
fn result(p: &TopPerformance) -> Option<String> {
    let (us, them) = (p.team_score?, p.opponent_score?);
    let wl = if us > them { "W" } else { "L" };
    Some(match &p.opponent_name {
        Some(o) => format!("{wl} {us}-{them} vs {o}"),
        None => format!("{wl} {us}-{them}"),
    })
}

fn list_template(perfs: &[TopPerformance]) -> String {
    let mut out = String::from("Lines of the night:");
    for p in perfs.iter().take(4) {
        out.push_str(&format!(
            "\n• {} ({}): {}",
            p.player_name,
            p.team_name,
            stat_line(p)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn perf() -> TopPerformance {
        TopPerformance {
            game_id: Uuid::nil(),
            game_date: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            player_id: Uuid::nil(),
            player_name: "Braden Smith".into(),
            class_year: Some("Sr".into()),
            position: Some("G".into()),
            team_id: Uuid::nil(),
            team_name: "Purdue".into(),
            opponent_id: None,
            opponent_name: Some("Gonzaga".into()),
            team_score: Some(81),
            opponent_score: Some(74),
            minutes: Some(36.0),
            points: Some(31),
            total_rebounds: Some(9),
            assists: Some(5),
            steals: Some(3),
            blocks: Some(0),
            turnovers: Some(2),
            fgm: Some(11),
            fga: Some(18),
            tpm: Some(4),
            tpa: Some(7),
            ftm: Some(5),
            fta: Some(6),
            game_score: 32.14,
            prior_season_high_points: Some(28),
            campom: Some(14.27),
            archetype: Some("Wizard".into()),
        }
    }

    #[test]
    fn stat_lines_read_like_a_box_score() {
        let p = perf();
        assert_eq!(stat_line(&p), "31 pts, 9 reb, 5 ast, 3 stl");
        assert_eq!(shooting(&p), "11-18 FG, 4-7 3PT, 5-6 FT");
        assert_eq!(result(&p).unwrap(), "W 81-74 vs Gonzaga");
        let f = facts(&p);
        assert_eq!(f["new_season_high_in_points"], true);
        assert_eq!(f["cam_this_season"], 14.3);
    }

    #[test]
    fn templates_pass_the_fact_check() {
        let p = perf();
        let t = list_template(std::slice::from_ref(&p));
        let f = json!({ "lines": [facts(&p)] });
        assert!(
            super::super::factcheck::unverified_numbers(&t, &f).is_empty(),
            "{t}"
        );
    }
}
