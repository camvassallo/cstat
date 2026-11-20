//! Headlines from ESPN's men's college basketball news feed, each paired with
//! what our numbers say about the team or players in it.
//!
//! The post is our take on the story, and it links our page, not the article:
//! each draft paraphrases the news in a clause, credits the outlet with
//! "via <outlet>", and spends the rest on what our numbers say. The article is
//! linked in Slack for the proofreader only. One triage call per
//! run picks the few headlines worth a take; the rest are recorded as skipped
//! so the hourly cron does not weigh them again.
//!
//! **Only written stories are read.** The feed is mostly wire game recaps and
//! highlight videos (46 of the 50 items on 2026-11-20); the recaps and lines
//! jobs cover games from our own numbers, so only `HeadlineNews` and `Story`
//! items are candidates, and a story tagged only with another league (a
//! football or NBA piece filed under a basketball program) is dropped.
//!
//! **The team comes from the story's team tags, checked against the
//! headline.** Each story carries the men's basketball teams it is about. On
//! 282 stories checked on 2026-11-20, the tags of 281 resolved to our teams
//! (with the few spellings in [`OTHER_NAMES`]; the other tagged a non-Division
//! I opponent), and of the 147 whose headline named one of our teams, the tags
//! included it 140 times. The 7 others were the headline
//! match being wrong ("Dale Brown" read as Brown, "W. Michigan" as Michigan,
//! "All-American" as American). So the team is the earliest tagged
//! team the headline names; a story tagged with one team that the headline
//! does not name (a player's name alone) takes that team; a story tagged with
//! several the headline does not name is a league-wide piece and is skipped.
//! A story with no tags, or a tag that matches none of our teams, falls back
//! to reading the team from the headline alone, as the bot did before tags.
//!
//! Story ids are recorded under an `espn:` prefix, so they cannot meet the
//! bare NatStat ids the ledger holds from before.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

use super::games_of_day::game_facts;
use super::site::TeamProjection;
use super::{Ctx, Image, Item, record_skip, round1};
use crate::broadcasts::name_key;

const NEWS_URL: &str =
    "https://site.api.espn.com/apis/site/v2/sports/basketball/mens-college-basketball/news";
/// The most the feed returns in one call; a larger `limit` still gets 50, and
/// it has no paging. The hourly cron reads it often enough that a story does
/// not scroll past between runs.
const FEED_LIMIT: usize = 50;
/// The feed's id for men's college basketball, as a team tag's `sportId` and a
/// league tag's `leagueId`.
const MBB: i64 = 41;
/// Item types that are written stories. The rest are game recaps, previews
/// and highlight videos.
const STORY_TYPES: [&str; 2] = ["HeadlineNews", "Story"];
/// The outlet every story in this feed comes from, for the "via" credit.
const PROVIDER: &str = "ESPN";
/// Ledger key prefix, keeping these ids apart from the NatStat story ids
/// recorded before the switch.
const KEY_PREFIX: &str = "espn:";

/// Newest stories considered per run.
const CANDIDATES: usize = 25;
/// Story text handed to the drafter for understanding only. Short on purpose:
/// it is someone else's writing, and the draft must not lean on it.
const EXCERPT_CHARS: usize = 600;

/// Names the feed's tags and headlines use for a program that match none of
/// our names for it, keyed by our `short_name`. Without these the feed's tag
/// for the program resolves to nothing, which costs its stories the tag check,
/// and a headline saying "Sam Houston" reads as Houston. With them every one of
/// the feed's 365 Division I teams resolved to a distinct team of ours
/// (2026-11-20).
const OTHER_NAMES: [(&str, &str); 20] = [
    ("Connecticut", "UConn"),
    ("Mississippi", "Ole Miss"),
    ("Appalachian St.", "App State"),
    ("Miami FL", "Miami Hurricanes"),
    ("Penn", "Pennsylvania Quakers"),
    ("Albany", "UAlbany"),
    ("Southeastern Louisiana", "SE Louisiana"),
    ("McNeese St.", "McNeese"),
    ("Nicholls St.", "Nicholls"),
    ("LIU", "Long Island University"),
    ("Louisiana Monroe", "UL Monroe"),
    ("Tennessee Martin", "UT Martin"),
    ("IU Indy", "IU Indianapolis"),
    ("Sam Houston St.", "Sam Houston"),
    ("Illinois Chicago", "UIC"),
    ("FIU", "Florida International"),
    // The tag's full name, where its school part is one of our names for a
    // different program and the mascot check rightly refuses it.
    ("Grand Canyon", "Grand Canyon Lopes"),
    ("UMKC", "Kansas City Roos"),
    ("New Orleans", "LSU New Orleans Privateers"),
    ("Long Beach St.", "Long Beach State Beach"),
];

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

/// One story, as the job uses it.
#[derive(Debug, Clone)]
struct NewsItem {
    /// The ledger key: the feed's id under [`KEY_PREFIX`].
    id: String,
    headline: String,
    description: Option<String>,
    url: Option<String>,
    provider: Option<String>,
    published: Option<String>,
    /// The men's basketball teams the story is tagged with, by the feed's
    /// full name ("Houston Cougars"), one per team, in the feed's order.
    teams: Vec<String>,
    /// Players the story is tagged with, by name.
    athletes: Vec<String>,
}

// --- source payload (only the fields read) ---

#[derive(Deserialize)]
struct Feed {
    #[serde(default)]
    articles: Vec<Value>,
}

#[derive(Deserialize)]
struct RawArticle {
    /// A number in the feed; read loosely.
    id: Value,
    #[serde(rename = "type", default)]
    kind: String,
    headline: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    published: Option<String>,
    #[serde(default)]
    links: Value,
    #[serde(default)]
    categories: Vec<RawCategory>,
}

#[derive(Deserialize)]
struct RawCategory {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "sportId", default)]
    sport_id: Option<i64>,
    #[serde(rename = "leagueId", default)]
    league_id: Option<i64>,
    #[serde(default)]
    team: Option<RawTagged>,
    #[serde(default)]
    athlete: Option<RawTagged>,
}

#[derive(Deserialize)]
struct RawTagged {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    description: String,
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

/// The player a story is about: name and projected team, for finding his
/// page this season.
type Featured = (String, Option<String>);

#[derive(Debug, Deserialize)]
struct ProjectedPlayers {
    players: Vec<ProjectedPlayer>,
}

pub(super) async fn build(ctx: &Ctx<'_>, date: NaiveDate) -> anyhow::Result<Vec<Item>> {
    // The newest stories. The feed takes no date, so `date` only sets the
    // season here, and the dedupe ledger is what keeps a story from coming
    // back.
    let raw = fetch_feed().await?;
    let mut items = parse_feed(&raw);
    // The feed is newest first; keep it that way and bound the work.
    items.truncate(CANDIDATES);

    let season = crate::season_for_date(date);
    let teams = team_aliases(ctx, season).await?;
    let index = NameIndex::new(&teams);
    let mut fresh = Vec::new();
    for n in items {
        if is_other_sport(&n.headline) {
            continue;
        }
        let Some(team) = story_team(&n, &teams, &index) else {
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

    let mut candidates: Vec<(NewsItem, Value, Option<Featured>, Uuid)> = Vec::new();
    for (n, (team_id, team_name)) in fresh {
        let roster: Vec<&ProjectedPlayer> = players
            .iter()
            .filter(|p| p.team_id == Some(team_id) || p.team_name.as_deref() == Some(&team_name))
            .collect();
        let description = n.description.clone().unwrap_or_default();
        // The story's player tags count as mentions too: a headline often
        // names the player only by a nickname or not at all.
        let text = format!("{} {description} {}", n.headline, n.athletes.join(", "));
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
            "via": n.provider.as_deref().map(via_name),
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
        let featured = mentioned
            .first()
            .map(|p| (p.name.clone(), p.team_name.clone()));
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
        // The post links the player in the story, else the team, and that
        // page's preview card is the image. The projected-player list keys a
        // returner or transfer by last season's id and a freshman by his
        // recruit id, which has no player page, so the player is looked up
        // again in this season's list: one with no row this season (a
        // freshman, someone who has not played) gets the team page instead
        // of a link to nothing. A player found carries his team's id too.
        let player = match &featured {
            Some((name, team)) => ctx.site.find_player(name, team.as_deref(), season).await,
            None => None,
        };
        let team_id = player.and_then(|(_, t)| t).unwrap_or(team_id);
        let player = player.map(|(p, _)| p);
        let (share, image) = match player {
            Some(pid) => (
                ctx.site.player_url(pid),
                Image {
                    url: ctx.site.player_page_card(pid, season),
                    alt: "Season card of the player in the story".into(),
                },
            ),
            None => (
                ctx.site.team_url(team_id),
                Image {
                    url: ctx.site.team_page_card(team_id, season),
                    alt: "Team season card".into(),
                },
            ),
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
            brief: "Our take on a news story. The tweet links our page, not the article, so a \
                    reader who has not seen the story must understand it from the draft alone. \
                    Each draft says what happened in a short paraphrase of your own (never the \
                    headline word for word) and credits the outlet as \"via <via>\" using the \
                    `via` fact, in the shape \"<what happened, in your words> (via <via>).\" \
                    Then most of the draft is what our numbers say about the team or player in \
                    it. Vary the shape across the three drafts: not every draft opens with the \
                    news; one can lead with our number and bring the news in after it. Research \
                    to find the take: team_profile or team_coach for the program, \
                    player_profile, player_career or similar_players for a player named, \
                    predict_matchup for an upcoming game. If our numbers cut against the story's \
                    framing, say so plainly. Crediting the outlet is the one place another \
                    source is named, and only the outlet in `via`. Early in a season, the last \
                    completed season is where the history is."
                .into(),
            fallback: vec![template(&facts)],
            facts,
            share: Some(share),
            images: vec![image],
            links,
            season,
        });
    }
    Ok(out)
}

/// The newest page of the feed. One unauthenticated call, read fresh each run
/// (the hourly cron is the only caller, so there is nothing to cache for). A
/// failure fails the job for this run, which the ledger records, and the next
/// run tries again.
async fn fetch_feed() -> anyhow::Result<Value> {
    let http = reqwest::Client::builder()
        .user_agent("cstat/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the news HTTP client")?;
    http.get(NEWS_URL)
        .query(&[("limit", FEED_LIMIT.to_string())])
        .send()
        .await
        .context("fetching the news feed")?
        .error_for_status()
        .context("news feed")?
        .json()
        .await
        .context("parsing the news feed")
}

/// The feed's written stories about men's basketball, newest first. A
/// malformed item is skipped rather than failing the run.
fn parse_feed(raw: &Value) -> Vec<NewsItem> {
    let Ok(feed) = serde_json::from_value::<Feed>(raw.clone()) else {
        return vec![];
    };
    feed.articles
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawArticle>(v).ok())
        .filter_map(story)
        .collect()
}

/// One feed item as a candidate, or `None` when it is not a written story,
/// has no usable id, or is tagged only with leagues other than ours.
fn story(a: RawArticle) -> Option<NewsItem> {
    if !STORY_TYPES.contains(&a.kind.as_str()) {
        return None;
    }
    let id = id_string(&a.id)?;
    let leagues: Vec<i64> = a
        .categories
        .iter()
        .filter(|c| c.kind == "league")
        .filter_map(|c| c.league_id)
        .collect();
    if !leagues.is_empty() && !leagues.contains(&MBB) {
        return None;
    }
    let mut seen = HashSet::new();
    let mut teams = Vec::new();
    let mut athletes = Vec::new();
    for c in &a.categories {
        match (c.kind.as_str(), &c.team, &c.athlete) {
            // A team is tagged twice, once as the basketball program and once
            // as the university (another sport id); only the first counts.
            ("team", Some(t), _) if c.sport_id == Some(MBB) => {
                let key = id_string(&t.id).unwrap_or_else(|| t.description.clone());
                if !t.description.trim().is_empty() && seen.insert(key) {
                    teams.push(t.description.trim().to_string());
                }
            }
            ("athlete", _, Some(p)) if !p.description.trim().is_empty() => {
                athletes.push(p.description.trim().to_string());
            }
            _ => {}
        }
    }
    let url = a.links["web"]["href"].as_str().map(str::to_string);
    Some(NewsItem {
        id: format!("{KEY_PREFIX}{id}"),
        headline: a.headline,
        description: a.description,
        url,
        provider: Some(PROVIDER.to_string()),
        published: a.published,
        teams,
        athletes,
    })
}

/// An id the feed writes as a number or a string.
fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// A team and the names a headline might call it by.
struct TeamNames {
    id: Uuid,
    display: String,
    /// The full name, mascot last ("Houston Cougars").
    name: String,
    aliases: Vec<String>,
}

/// Every alias of every team by [`name_key`], for resolving a tag's name.
struct NameIndex {
    names: HashMap<String, HashSet<Uuid>>,
    /// The last word of each team's full name: its mascot, or the end of it.
    mascots: HashMap<Uuid, String>,
}

impl NameIndex {
    fn new(teams: &[TeamNames]) -> Self {
        let mut names: HashMap<String, HashSet<Uuid>> = HashMap::new();
        let mut mascots = HashMap::new();
        for t in teams {
            for a in &t.aliases {
                names.entry(name_key(a)).or_default().insert(t.id);
            }
            if let Some(last) = name_key(&t.name).split_whitespace().last() {
                mascots.insert(t.id, last.to_string());
            }
        }
        NameIndex { names, mascots }
    }

    /// The team a tag's full name means: an exact name, else the longest of
    /// our names that starts it when the mascot agrees too (the tag spells
    /// the school differently: "Miami (OH) RedHawks" is our "Miami OH",
    /// Redhawks). The mascot check is what stops "Florida International
    /// Panthers" reading as Florida and "Texas Wesleyan Rams" as Texas.
    /// `None` when nothing matches or the match is shared by two teams.
    fn resolve(&self, tag: &str) -> Option<Uuid> {
        let key = name_key(tag);
        let words: Vec<&str> = key.split_whitespace().collect();
        let only = |ids: &HashSet<Uuid>| match ids.len() {
            1 => ids.iter().next().copied(),
            _ => None,
        };
        if let Some(ids) = self.names.get(&key) {
            return only(ids);
        }
        let mascot = *words.last()?;
        for n in (1..words.len()).rev() {
            if let Some(ids) = self.names.get(&words[..n].join(" ")) {
                let id = only(ids)?;
                return (self.mascots.get(&id).map(String::as_str) == Some(mascot)).then_some(id);
            }
        }
        None
    }
}

/// The team a story is about (see the module docs for the rule), or `None`
/// to skip it.
fn story_team(n: &NewsItem, teams: &[TeamNames], index: &NameIndex) -> Option<(Uuid, String)> {
    let tagged: Vec<Option<Uuid>> = n.teams.iter().map(|t| index.resolve(t)).collect();
    if !tagged.is_empty() && tagged.iter().all(Option::is_some) {
        let ids: HashSet<Uuid> = tagged.iter().flatten().copied().collect();
        if let Some(t) = team_in_headline(&n.headline, teams.iter().filter(|t| ids.contains(&t.id)))
        {
            return Some(t);
        }
        if let [Some(only)] = tagged.as_slice() {
            return teams
                .iter()
                .find(|t| t.id == *only)
                .map(|t| (t.id, t.display.clone()));
        }
        return None;
    }
    team_in_headline(&n.headline, teams)
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
            TeamNames {
                id,
                display,
                aliases: aliases(name.clone(), short),
                name,
            }
        })
        .collect())
}

/// Every name a headline or a tag might use for a team.
fn aliases(name: String, short: Option<String>) -> Vec<String> {
    let mut aliases = Vec::new();
    // "Miami (Ohio) Redhawks" is "Miami (Ohio)" in a headline.
    if let Some(end) = name.find(')') {
        aliases.push(name[..=end].to_string());
    }
    aliases.push(name);
    if let Some(s) = short {
        // "Iowa St." is "Iowa State" in a headline.
        if let Some(base) = s.strip_suffix(" St.") {
            aliases.push(format!("{base} State"));
        }
        aliases.extend(
            OTHER_NAMES
                .iter()
                .filter(|(ours, _)| *ours == s)
                .map(|(_, other)| other.to_string()),
        );
        aliases.push(s);
    }
    aliases
}

/// The team a headline is about: the earliest whole-word mention of any of
/// our team names, preferring the longest name at a position so "Michigan
/// State" is not read as "Michigan".
fn team_in_headline<'a>(
    headline: &str,
    teams: impl IntoIterator<Item = &'a TeamNames>,
) -> Option<(Uuid, String)> {
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
    let news = match (facts["headline"].as_str(), facts["via"].as_str()) {
        (Some(h), Some(v)) => format!("{h} (via {v}). "),
        (Some(h), None) => format!("{h}. "),
        _ => String::new(),
    };
    match facts["team"]["our_preseason_projection_rank"].as_u64() {
        Some(r) => {
            format!("{news}For context: {team} came into the season at No. {r} in our projections.")
        }
        None => format!("{news}Our numbers on {team}:"),
    }
}

/// The outlet's name for a "via" credit: the feed's provider without the
/// section it appends ("Yahoo College Basketball" → "Yahoo").
fn via_name(provider: &str) -> String {
    let mut name = provider.trim();
    for suffix in [
        " Men's College Basketball",
        " College Basketball",
        " NCAA Basketball",
        " Men's Basketball",
        " NCAAB",
    ] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.trim();
            break;
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn via_credits_name_the_outlet() {
        assert_eq!(via_name("Yahoo College Basketball"), "Yahoo");
        assert_eq!(via_name("CBS Sports"), "CBS Sports");
        assert_eq!(via_name("ESPN Men's College Basketball"), "ESPN");
        let facts = json!({
            "headline": "Purdue guard out 4-6 weeks", "via": "ESPN",
            "team": {"team": "Purdue", "our_preseason_projection_rank": 7},
        });
        assert_eq!(
            template(&facts),
            "Purdue guard out 4-6 weeks (via ESPN). For context: Purdue came into the season at No. 7 in our projections."
        );
    }

    /// A trimmed real response (2026-11-20; story text replaced): two tagged
    /// headline stories, a recap and a highlight video, a single-tag story
    /// whose player is tagged under another sport, a story tagged with eight
    /// teams, and a football story filed in the basketball feed.
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/espn_mbb_news.json"))
            .expect("fixture parses")
    }

    #[test]
    fn parses_the_feed_shape() {
        let items = parse_feed(&fixture());
        let headlines: Vec<&str> = items.iter().map(|n| n.headline.as_str()).collect();
        // The recap, the video and the football story are gone.
        assert_eq!(items.len(), 4, "{headlines:?}");
        assert!(!headlines.iter().any(|h| h.contains("Mercy Miller")));
        assert!(!headlines.iter().any(|h| h.contains("Cignetti")));

        let first = &items[0];
        assert_eq!(first.id, "espn:50539115");
        assert_eq!(first.provider.as_deref(), Some("ESPN"));
        assert_eq!(
            first.url.as_deref(),
            Some(
                "https://www.espn.com/mens-college-basketball/story/_/id/50539115/houston-muscles-kansas-stamps-claim-no-1-ranking"
            )
        );
        // Each team once, though the feed tags it as program and university.
        assert_eq!(first.teams, ["Houston Cougars", "Kansas Jayhawks"]);
        assert_eq!(first.published.as_deref(), Some("2026-11-20T07:43:46Z"));

        let stokes = items.iter().find(|n| n.id == "espn:50537388").unwrap();
        assert_eq!(stokes.teams, ["Kansas Jayhawks"]);
        assert_eq!(stokes.athletes, ["Tyran Stokes"]);
        let luis = items.iter().find(|n| n.id == "espn:50536621").unwrap();
        assert_eq!(luis.athletes, ["RJ Luis Jr"]);
        let wide = items.iter().find(|n| n.id == "espn:50505898").unwrap();
        assert_eq!(wide.teams.len(), 8);
    }

    #[test]
    fn a_broken_feed_is_empty_not_an_error() {
        assert!(parse_feed(&json!({})).is_empty());
        assert!(parse_feed(&json!({"articles": "nope"})).is_empty());
        // One malformed item does not cost the rest.
        let raw = json!({"articles": [
            {"type": "Story"},
            {"id": 7, "type": "Story", "headline": "Duke wins", "categories": []},
        ]});
        let items = parse_feed(&raw);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "espn:7");
    }

    fn team(name: &str, short: &str) -> TeamNames {
        TeamNames {
            id: Uuid::new_v4(),
            display: short.into(),
            name: name.into(),
            aliases: aliases(name.into(), Some(short.into())),
        }
    }

    fn teams() -> Vec<TeamNames> {
        vec![
            team("Michigan Wolverines", "Michigan"),
            team("Michigan State Spartans", "Michigan St."),
            team("Duke Blue Devils", "Duke"),
            team("UCLA Bruins", "UCLA"),
        ]
    }

    /// Our names for the teams in the fixture, and a few that trip a headline.
    fn fixture_teams() -> Vec<TeamNames> {
        [
            ("Houston Cougars", "Houston"),
            ("Houston Christian Huskies", "Houston Christian"),
            ("Sam Houston State Bearkats", "Sam Houston St."),
            ("Kansas Jayhawks", "Kansas"),
            ("LSU Tigers", "LSU"),
            ("Rutgers Scarlet Knights", "Rutgers"),
            ("Florida Gators", "Florida"),
            ("Notre Dame Fighting Irish", "Notre Dame"),
            ("Auburn Tigers", "Auburn"),
            ("UNLV Rebels", "UNLV"),
            ("West Virginia Mountaineers", "West Virginia"),
            ("Connecticut Huskies", "Connecticut"),
            ("Miami (Ohio) Redhawks", "Miami OH"),
            ("Miami (Fla.) Hurricanes", "Miami FL"),
            ("SMU Mustangs", "SMU"),
            ("Brown Bears", "Brown"),
            ("Texas Longhorns", "Texas"),
        ]
        .into_iter()
        .map(|(n, s)| team(n, s))
        .collect()
    }

    fn pick(n: &NewsItem, ts: &[TeamNames]) -> Option<String> {
        story_team(n, ts, &NameIndex::new(ts)).map(|(_, d)| d)
    }

    fn item(headline: &str, teams: &[&str]) -> NewsItem {
        NewsItem {
            id: "espn:1".into(),
            headline: headline.into(),
            description: None,
            url: None,
            provider: Some(PROVIDER.into()),
            published: None,
            teams: teams.iter().map(|t| t.to_string()).collect(),
            athletes: vec![],
        }
    }

    #[test]
    fn fixture_stories_get_their_team() {
        let ts = fixture_teams();
        let items = parse_feed(&fixture());
        let by_id = |id: &str| items.iter().find(|n| n.id == id).unwrap();
        // Both tagged; the headline names Houston first.
        assert_eq!(
            pick(by_id("espn:50539115"), &ts).as_deref(),
            Some("Houston")
        );
        assert_eq!(pick(by_id("espn:50537388"), &ts).as_deref(), Some("Kansas"));
        assert_eq!(pick(by_id("espn:50536621"), &ts).as_deref(), Some("LSU"));
        // Eight teams tagged; the headline names two of them, Houston first.
        assert_eq!(
            pick(by_id("espn:50505898"), &ts).as_deref(),
            Some("Houston")
        );
    }

    #[test]
    fn tags_resolve_to_our_teams() {
        let ts = fixture_teams();
        let index = NameIndex::new(&ts);
        let name = |tag: &str| {
            index
                .resolve(tag)
                .and_then(|id| ts.iter().find(|t| t.id == id))
                .map(|t| t.display.as_str())
        };
        assert_eq!(name("Houston Cougars"), Some("Houston"));
        assert_eq!(name("Houston Christian Huskies"), Some("Houston Christian"));
        assert_eq!(name("Sam Houston Bearkats"), Some("Sam Houston St."));
        assert_eq!(name("UConn Huskies"), Some("Connecticut"));
        assert_eq!(name("Miami (OH) RedHawks"), Some("Miami OH"));
        assert_eq!(name("Miami Hurricanes"), Some("Miami FL"));
        assert_eq!(name("Northwest Indian RedHawks"), None);
        // A school part that is one of our names, with another mascot, is
        // another program.
        assert_eq!(name("Florida International Panthers"), None);
        assert_eq!(name("Texas Wesleyan Rams"), None);
    }

    #[test]
    fn tags_overrule_a_headline_misread() {
        let ts = fixture_teams();
        // The headline's first team name is a person's.
        assert_eq!(
            pick(
                &item(
                    "Dale Brown, LSU's winningest basketball coach, dies at 90",
                    &["LSU Tigers"]
                ),
                &ts
            )
            .as_deref(),
            Some("LSU")
        );
        // A player's name alone, with one team tagged.
        assert_eq!(
            pick(
                &item("Tyran Stokes shines in return", &["Kansas Jayhawks"]),
                &ts
            )
            .as_deref(),
            Some("Kansas")
        );
        // Several tagged, none named: skipped.
        assert_eq!(
            pick(
                &item(
                    "Ranking men's college basketball All-American candidates",
                    &["Florida Gators", "Auburn Tigers"]
                ),
                &ts
            ),
            None
        );
        assert_eq!(
            pick(
                &item(
                    "Miami (Ohio) downs SMU in First Four",
                    &["Miami (OH) RedHawks", "SMU Mustangs"]
                ),
                &ts
            )
            .as_deref(),
            Some("Miami OH")
        );
    }

    #[test]
    fn untagged_or_unknown_tags_fall_back_to_the_headline() {
        let ts = fixture_teams();
        assert_eq!(
            pick(&item("Sam Houston visits Texas", &[]), &ts).as_deref(),
            Some("Sam Houston St.")
        );
        // A tag we cannot resolve (a non-D1 opponent) means the tags cannot be
        // trusted to be complete, so the headline decides.
        assert_eq!(
            pick(
                &item(
                    "UConn routs Fisk in opener",
                    &["Fisk Bulldogs", "UConn Huskies"]
                ),
                &ts
            )
            .as_deref(),
            Some("Connecticut")
        );
        assert_eq!(pick(&item("College hoops storylines", &[]), &ts), None);
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
