//! Headlines from the NatStat news feed, each paired with what our numbers say
//! about the team or players in it.
//!
//! The post is our take, not the story: the article is linked for the
//! proofreader, and the drafter is told not to restate it. One triage call per
//! run picks the few headlines worth a take; the rest are recorded as skipped
//! so the hourly cron does not weigh them again.
//!
//! **The feed's `team-code` is not used.** Checked against the headlines on
//! 2026-11-01 it named the wrong team for 16 of 40 stories: a catch-all code
//! on league-wide pieces, and football and soccer stories filed under
//! basketball programs. The team is read from the headline instead, by our
//! own team names, and stories from other sports are dropped.

use anyhow::Context;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

use super::games_of_day::game_facts;
use super::site::TeamProjection;
use super::{Ctx, Image, Item, record_skip, round1};

/// Newest headlines considered per run.
const CANDIDATES: usize = 25;
/// Story text handed to the drafter for understanding only. Short on purpose:
/// it is someone else's writing, and the draft must not lean on it.
const EXCERPT_CHARS: usize = 600;
const CACHE_TTL_SECS: i64 = 1800;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Headlines drafted per run, and per Eastern day. Each researched draft costs
/// a few cents, and the feed would happily fill every hourly run, so the day
/// cap is what keeps news from dominating the bill (docs/social_drafts.md).
fn max_per_run() -> usize {
    env_usize("SOCIAL_NEWS_MAX_PER_RUN", 3)
}

fn max_per_day() -> usize {
    env_usize("SOCIAL_NEWS_MAX_PER_DAY", 6)
}

#[derive(Debug, Clone, Deserialize)]
struct NewsItem {
    id: String,
    headline: String,
    #[serde(default)]
    description: Option<String>,
    url: Option<String>,
    provider: Option<String>,
    published: Option<String>,
}

/// Headline words that mark a story about another sport or the women's game.
const OTHER_SPORTS: [&str; 9] = [
    "football",
    "soccer",
    "volleyball",
    "baseball",
    "softball",
    "hockey",
    "women's",
    "womens",
    "wbb",
];

#[derive(Debug, Clone, Deserialize)]
struct ProjectedPlayer {
    player_id: Uuid,
    name: String,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    campom: Option<f64>,
    class_year: Option<String>,
    primary_archetype: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectedPlayers {
    players: Vec<ProjectedPlayer>,
}

pub(super) async fn build(ctx: &Ctx<'_>, date: NaiveDate) -> anyhow::Result<Vec<Item>> {
    let natstat = ctx
        .natstat
        .context("news needs a NatStat client (NATSTAT_API_KEY)")?;
    // The newest page. A date in the range is not a filter on this endpoint
    // (it returns older stories), so `date` only sets the season here, and
    // the dedupe ledger is what keeps a story from coming back.
    let raw = natstat
        .get("news", None, None, Some(CACHE_TTL_SECS))
        .await?;
    let mut items = parse_news(&raw);
    // The feed is newest first; keep it that way and bound the work.
    items.truncate(CANDIDATES);

    let season = crate::season_for_date(date);
    let teams = team_aliases(ctx, season).await?;
    let mut fresh = Vec::new();
    for n in items {
        if is_other_sport(&n.headline) {
            continue;
        }
        let Some(team) = team_in_headline(&n.headline, &teams) else {
            continue;
        };
        if !ctx.opts.force
            && !ctx.opts.dry_run
            && super::store::already_handled(ctx.pool, "news", &n.id).await?
        {
            continue;
        }
        fresh.push((n, team.clone()));
    }
    if fresh.is_empty() {
        return Ok(vec![]);
    }
    let posted_today = if ctx.opts.dry_run || ctx.opts.force {
        0
    } else {
        super::store::posted_today(ctx.pool, "news").await? as usize
    };
    let max = max_per_run().min(max_per_day().saturating_sub(posted_today));
    if max == 0 {
        info!(posted_today, "news day cap reached");
        return Ok(vec![]);
    }

    let projections = ctx.site.projections(season).await.unwrap_or_default();
    let players: Vec<ProjectedPlayer> = ctx
        .site
        .get_json::<ProjectedPlayers>(&format!("/api/projected-players/{season}"))
        .await
        .map(|p| p.players)
        .unwrap_or_default();
    let slate = ctx.site.schedule(date).await.ok();

    let mut candidates: Vec<(NewsItem, Value, Option<Uuid>, Uuid)> = Vec::new();
    for (n, (team_id, team_name)) in fresh {
        let roster: Vec<&ProjectedPlayer> = players
            .iter()
            .filter(|p| p.team_id == Some(team_id) || p.team_name.as_deref() == Some(&team_name))
            .collect();
        let description = n.description.clone().unwrap_or_default();
        let text = format!("{} {description}", n.headline);
        let by_cam = |a: &&&ProjectedPlayer, b: &&&ProjectedPlayer| {
            b.campom
                .unwrap_or(f64::MIN)
                .total_cmp(&a.campom.unwrap_or(f64::MIN))
        };
        // A roster preview names everyone; keep the few that matter most.
        let mut mentioned: Vec<&&ProjectedPlayer> =
            roster.iter().filter(|p| mentions(&text, &p.name)).collect();
        mentioned.sort_by(by_cam);
        mentioned.truncate(4);
        let mut top: Vec<&&ProjectedPlayer> = roster.iter().collect();
        top.sort_by(by_cam);

        let proj = projections
            .iter()
            .find(|p| p.team_id == team_id || p.team_name == team_name);
        let tonight = slate.as_ref().and_then(|s| {
            s.games
                .iter()
                .find(|g| g.home_team_id == Some(team_id) || g.away_team_id == Some(team_id))
                .map(|g| game_facts(g, None, s.ranked_teams))
        });
        let excerpt: String = description.chars().take(EXCERPT_CHARS).collect();
        let facts = json!({
            "headline": n.headline,
            "source": n.provider,
            "published": n.published,
            "story_excerpt_for_context_only": excerpt,
            "team": team_facts(&team_name, proj, &projections),
            "players_in_the_story": mentioned.iter().map(|p| player_facts(p)).collect::<Vec<_>>(),
            "team_top_players_by_projected_cam": top.iter().take(5).map(|p| player_facts(p)).collect::<Vec<_>>(),
            "team_game_today": tonight,
            "season": season,
            "last_completed_season": season - 1,
        });
        let mut facts = facts;
        // This season's id for the team, which the research tools take.
        facts["team"]["team_id"] = json!(team_id);
        let featured = mentioned.first().map(|p| p.player_id);
        candidates.push((n, facts, featured, team_id));
    }

    let picks: Vec<String> = match &ctx.drafter {
        Some(d) => {
            let menu: Vec<Value> = candidates
                .iter()
                .map(|(n, f, _, _)| {
                    json!({
                        "id": n.id,
                        "headline": n.headline,
                        "team": f["team"]["team"],
                        "players_in_the_story": f["players_in_the_story"],
                    })
                })
                .collect();
            match d
                .triage(
                    "Pick the college basketball headlines where our numbers add a genuine, \
                     specific take: a team's projection or ranking, a player's CAM value or \
                     archetype, or tonight's game. Skip routine items (ticket sales, TV schedules, \
                     generic previews, recruiting rumors without a player we rate) and anything \
                     sensitive (injuries described in detail, legal trouble, deaths).",
                    &Value::Array(menu),
                    max,
                )
                .await
            {
                Ok((p, _)) => p,
                Err(e) => {
                    warn!(error = %e, "news triage failed; trying again next run");
                    return Ok(vec![]);
                }
            }
        }
        // Without a model, a headline naming a player we rate is the best
        // cheap signal that the data has something to add.
        None => candidates
            .iter()
            .filter(|(_, f, _, _)| {
                f["players_in_the_story"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
            })
            .take(max)
            .map(|(n, _, _, _)| n.id.clone())
            .collect(),
    };
    info!(
        candidates = candidates.len(),
        picked = picks.len(),
        "news triage"
    );

    let mut out = Vec::new();
    for (n, facts, featured, team_id) in candidates {
        if !picks.contains(&n.id) {
            record_skip(ctx, "news", &n.id, &facts).await;
            continue;
        }
        let image = match featured {
            Some(pid) => Image {
                url: ctx.site.radar_card(pid, season, date),
                alt: "Skill profile of the player in the story".into(),
            },
            None => Image {
                url: ctx.site.team_card(team_id, season, date),
                alt: "Team outlook card".into(),
            },
        };
        let mut links = Vec::new();
        if let Some(u) = &n.url {
            links.push((
                n.provider.clone().unwrap_or_else(|| "Article".into()),
                u.clone(),
            ));
        }
        links.push(("Team page".into(), ctx.site.team_url(team_id)));
        out.push(Item {
            kind: "news",
            source_key: n.id.clone(),
            title: format!("News · {}", n.headline),
            summary: n.provider.clone().unwrap_or_default(),
            brief: "Our take on a news story, to be posted as a quote of the article. Do not \
                    summarize, restate or quote the story; the reader will see it. Add what our \
                    numbers say about the team or player in it, and research to find it: \
                    team_profile or team_coach for the program, player_profile, player_career or \
                    similar_players for a player named, predict_matchup for an upcoming game. If \
                    our numbers cut against the story's framing, say so plainly. Early in a \
                    season, the last completed season is where the history is."
                .into(),
            fallback: vec![template(&facts)],
            facts,
            images: vec![image],
            links,
            season,
        });
    }
    Ok(out)
}

/// The feed's items, whether it returns them as an array or keyed by id.
fn parse_news(raw: &Value) -> Vec<NewsItem> {
    let node = &raw["news"];
    let values: Vec<&Value> = match node {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        _ => vec![],
    };
    values
        .into_iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

/// A team and the names a headline might call it by.
struct TeamNames {
    id: Uuid,
    display: String,
    aliases: Vec<String>,
}

async fn team_aliases(ctx: &Ctx<'_>, season: i32) -> sqlx::Result<Vec<TeamNames>> {
    let rows: Vec<(Uuid, Option<String>, String)> =
        sqlx::query_as("SELECT id, short_name, name FROM teams WHERE season = $1")
            .bind(season)
            .fetch_all(ctx.pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, short, name)| {
            let display = short.clone().unwrap_or_else(|| name.clone());
            let mut aliases = vec![name];
            if let Some(s) = short {
                // "Iowa St." is "Iowa State" in a headline.
                if let Some(base) = s.strip_suffix(" St.") {
                    aliases.push(format!("{base} State"));
                }
                aliases.push(s);
            }
            TeamNames {
                id,
                display,
                aliases,
            }
        })
        .collect())
}

/// The team a headline is about: the earliest whole-word mention of any of
/// our team names, preferring the longest name at a position so "Michigan
/// State" is not read as "Michigan".
fn team_in_headline(headline: &str, teams: &[TeamNames]) -> Option<(Uuid, String)> {
    let mut best: Option<(usize, usize, &TeamNames)> = None;
    for t in teams {
        for alias in &t.aliases {
            let Some(pos) = find_word(headline, alias) else {
                continue;
            };
            let better = match best {
                None => true,
                Some((bp, blen, _)) => pos < bp || (pos == bp && alias.len() > blen),
            };
            if better {
                best = Some((pos, alias.len(), t));
            }
        }
    }
    best.map(|(_, _, t)| (t.id, t.display.clone()))
}

/// Byte offset of `needle` in `hay` as whole words (not inside a longer word).
fn find_word(hay: &str, needle: &str) -> Option<usize> {
    if needle.len() < 3 {
        return None;
    }
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        if boundary(hay[..start].chars().next_back()) && boundary(hay[end..].chars().next()) {
            return Some(start);
        }
        from = start + needle.len().max(1);
    }
    None
}

fn is_other_sport(headline: &str) -> bool {
    let h = headline.to_lowercase();
    OTHER_SPORTS.iter().any(|w| {
        h.split(|c: char| !c.is_alphanumeric() && c != '\'')
            .any(|tok| tok == *w)
    })
}

/// Whole-name match, or a distinctive last name when the full name is absent
/// (stories often say "Smith" after the first mention, never only the first).
fn mentions(text: &str, name: &str) -> bool {
    if text.contains(name) {
        return true;
    }
    let last = name.split_whitespace().last().unwrap_or("");
    let first = name.split_whitespace().next().unwrap_or("");
    last.len() >= 4
        && text.contains(first)
        && text
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == last)
}

fn team_facts(name: &str, p: Option<&TeamProjection>, all: &[TeamProjection]) -> Value {
    let mut v = json!({ "team": name });
    if let Some(p) = p {
        // Rank among every projected team, best first.
        let rank = p.midpoint_adj_em.map(|m| {
            1 + all
                .iter()
                .filter(|o| o.midpoint_adj_em.is_some_and(|x| x > m))
                .count()
        });
        v["conference"] = json!(p.conference);
        v["our_preseason_projection_rank"] = json!(rank);
        v["of_projected_teams"] = json!(all.len());
        v["preseason_projected_adj_em"] = p.midpoint_adj_em.map(round1).into();
        v["adj_em_means"] = json!("points per 100 possessions better than an average D-I team");
        v["coach"] = json!(p.coach_name);
        v["first_year_head_coach"] = json!(p.coach_is_new_hc);
        v["returning_players"] = json!(p.returning_count);
        v["incoming_transfers"] = json!(p.arrivals_count);
        v["freshman_recruits"] = json!(p.recruits_count);
    }
    v
}

fn player_facts(p: &ProjectedPlayer) -> Value {
    json!({
        "player": p.name,
        "player_id": p.player_id,
        "class": p.class_year,
        "projected_cam_this_season": p.campom.map(round1),
        "archetype": p.primary_archetype,
    })
}

fn template(facts: &Value) -> String {
    let team = facts["team"]["team"].as_str().unwrap_or("This team");
    match facts["team"]["our_preseason_projection_rank"].as_u64() {
        Some(r) => {
            format!("For context: {team} came into the season at No. {r} in our projections.")
        }
        None => format!("Our numbers on {team}:"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_feed_shape() {
        let raw = json!({"news": [{
            "id": "1621576", "headline": "TCU opens", "description": "FORT WORTH - ...",
            "url": "https://example.com/a", "provider": "Yahoo College Basketball",
            "category": "", "thumbnail": "", "image": "",
            "team-code": "508", "game-code": "1516307", "published": "2026-11-02 01:24:05"
        }]});
        let items = parse_news(&raw);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].headline, "TCU opens");
    }

    fn teams() -> Vec<TeamNames> {
        let t = |n: &str, short: &str| TeamNames {
            id: Uuid::new_v4(),
            display: short.into(),
            aliases: vec![n.into(), short.into()],
        };
        let mut v = vec![
            t("Michigan Wolverines", "Michigan"),
            t("Michigan State Spartans", "Michigan St."),
            t("Duke Blue Devils", "Duke"),
            t("UCLA Bruins", "UCLA"),
        ];
        v[1].aliases.push("Michigan State".into());
        v
    }

    #[test]
    fn headlines_name_their_team() {
        let ts = teams();
        let name = |h: &str| team_in_headline(h, &ts).map(|(_, n)| n);
        assert_eq!(
            name("Michigan State opens at home").as_deref(),
            Some("Michigan St.")
        );
        assert_eq!(
            name("Duke Gets Ranking Even UCLA Fans Agree With").as_deref(),
            Some("Duke")
        );
        assert_eq!(
            name("UCLA Basketball Projected Rotation").as_deref(),
            Some("UCLA")
        );
        assert_eq!(name("Dukes of Hazzard reboot"), None, "whole words only");
        assert_eq!(name("College Basketball Eligibility Crisis Grows"), None);
    }

    #[test]
    fn other_sports_are_dropped() {
        assert!(is_other_sport(
            "How to watch no. 8 BYU Football vs. Arizona State"
        ));
        assert!(is_other_sport("Marquette Women's Soccer Can't Make A Dent"));
        assert!(!is_other_sport("Kentucky Basketball vs. Manhattan preview"));
    }

    #[test]
    fn name_matching_needs_more_than_a_common_word() {
        let text = "Smith scored 20 as Braden Smith led Purdue";
        assert!(mentions(text, "Braden Smith"));
        assert!(mentions(
            "Braden led the way; Smith hit 5 threes",
            "Braden Smith"
        ));
        // A last name alone, without the first name anywhere, is not enough.
        assert!(!mentions("Smith hit 5 threes", "Braden Smith"));
        // Short last names never match alone.
        assert!(!mentions("Ty went off; Lee hit 5 threes", "Ty Lee"));
    }
}
