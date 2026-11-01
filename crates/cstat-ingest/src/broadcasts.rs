//! TV networks, and the tip times the primary feed leaves blank, for the
//! Schedule tab (issue #430).
//!
//! NatStat's schedule has no broadcast field at all, and about a third of a
//! season's games carry `00:00:00`, meaning "time not set". ESPN's public
//! scoreboard JSON has both: one unauthenticated call per Eastern day returns
//! every Division I game with its outlets and an exact UTC start. This module
//! fetches a window of days, matches each listing to a `games` row, and writes
//! `game_broadcasts`.
//!
//! **Matching is per game, not per team.** The two sources spell teams
//! differently ("UConn" / "Connecticut", "App State" / "Appalachian St.",
//! "San José State" / "San Jose St."), and a name crosswalk would need curating
//! forever. Instead a listing is matched by its date plus whichever of its teams
//! resolve by name: both resolving pins the game outright, and one resolving
//! still pins it when that team has exactly one game that day (a team does not
//! play twice in a day). A one-sided match then *teaches* the other team's
//! identity, so a later listing between two hard-to-spell teams resolves too.
//! On a sample week this matched 514 of 519 games with no conflicting claims,
//! and the misses were games one source simply does not list.
//!
//! **A match must be unambiguous or it is dropped.** A name key shared by two
//! programs resolves neither, a game claimed by two listings keeps neither, and
//! a source team taught two different identities forgets both. An unmatched
//! game shows "TBA" and no network, which is what it showed before; a wrong
//! match would show another game's time.
//!
//! Best-effort throughout: the nightly records a failure here in the ledger and
//! moves on, and nothing served depends on it beyond two display columns.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

const SCOREBOARD_URL: &str =
    "https://site.api.espn.com/apis/site/v2/sports/basketball/mens-college-basketball/scoreboard";

/// How far ahead the nightly looks, in days past today. Times are published
/// months out, but outlets are announced late (two weeks out, most of a
/// slate has one; seven weeks out, almost none do), so a short window
/// re-read every night is what catches them.
pub const NIGHTLY_WINDOW_DAYS: i64 = 14;

/// Most unmatched games named in the log per run. The full count is always
/// reported; this only bounds the noise.
const UNMATCHED_LOG_LIMIT: usize = 20;

// --- source payload (only the fields we read) ---

#[derive(Deserialize)]
struct Scoreboard {
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Deserialize)]
struct Event {
    id: String,
    #[serde(default)]
    competitions: Vec<Competition>,
}

#[derive(Deserialize)]
struct Competition {
    date: String,
    /// False when the start is a placeholder (the source parks those at
    /// midnight Eastern).
    #[serde(rename = "timeValid", default)]
    time_valid: bool,
    #[serde(default)]
    competitors: Vec<Competitor>,
    #[serde(default)]
    broadcasts: Vec<Broadcast>,
}

#[derive(Deserialize)]
struct Competitor {
    #[serde(rename = "homeAway")]
    home_away: String,
    team: RawTeam,
}

#[derive(Deserialize)]
struct RawTeam {
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(default)]
    location: String,
    #[serde(rename = "shortDisplayName", default)]
    short_display_name: String,
}

#[derive(Deserialize)]
struct Broadcast {
    /// A string ("national", "home") here. Read loosely so a shape change
    /// costs the national-first ordering, not the whole day.
    #[serde(default)]
    market: serde_json::Value,
    #[serde(default)]
    names: Vec<String>,
}

// --- parsed listing ---

/// One team as the source names it.
#[derive(Debug, Clone)]
pub struct SourceTeam {
    /// The source's team id, stable across listings; the key a one-sided
    /// match teaches.
    pub id: String,
    /// Every spelling the source gives, most specific first.
    pub names: Vec<String>,
}

/// One game as the source lists it.
#[derive(Debug, Clone)]
pub struct Listing {
    pub event_id: String,
    /// The Eastern day the listing was requested under.
    pub date: NaiveDate,
    /// Eastern wall clock; `None` when the source flags the time unconfirmed.
    pub tip_time: Option<NaiveTime>,
    pub network: Option<String>,
    pub home: SourceTeam,
    pub away: SourceTeam,
}

fn parse_start(s: &str) -> Option<DateTime<Utc>> {
    // The scoreboard writes minutes-only UTC ("2026-11-03T23:00Z"), which
    // RFC 3339 does not allow; accept both forms.
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%MZ")
                .ok()
                .map(|n| n.and_utc())
        })
}

/// Every outlet, national first, de-duplicated, joined with " / ".
fn network_label(broadcasts: &[Broadcast]) -> Option<String> {
    let mut ordered: Vec<&Broadcast> = broadcasts.iter().collect();
    ordered.sort_by_key(|b| b.market.as_str() != Some("national"));
    let mut seen = HashSet::new();
    let names: Vec<&str> = ordered
        .iter()
        .flat_map(|b| b.names.iter())
        .map(|n| n.trim())
        .filter(|n| !n.is_empty() && seen.insert(n.to_string()))
        .collect();
    (!names.is_empty()).then(|| names.join(" / "))
}

fn source_team(t: RawTeam) -> SourceTeam {
    let names = [t.display_name, t.location, t.short_display_name]
        .into_iter()
        .filter(|n| !n.trim().is_empty())
        .collect();
    SourceTeam { id: t.id, names }
}

/// The listings in one day's scoreboard. A malformed event is skipped rather
/// than failing the day.
fn parse_scoreboard(date: NaiveDate, board: Scoreboard) -> Vec<Listing> {
    let mut out = Vec::with_capacity(board.events.len());
    for event in board.events {
        let Some(comp) = event.competitions.into_iter().next() else {
            continue;
        };
        let tip_time = parse_start(&comp.date)
            .filter(|_| comp.time_valid)
            .map(|utc| crate::eastern_datetime(utc).time());
        let network = network_label(&comp.broadcasts);
        let (mut home, mut away) = (None, None);
        for c in comp.competitors {
            match c.home_away.as_str() {
                "home" => home = Some(source_team(c.team)),
                "away" => away = Some(source_team(c.team)),
                _ => {}
            }
        }
        let (Some(home), Some(away)) = (home, away) else {
            continue;
        };
        out.push(Listing {
            event_id: event.id,
            date,
            tip_time,
            network,
            home,
            away,
        });
    }
    out
}

// --- matching ---

/// One of our games in the window.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OurGame {
    pub game_id: Uuid,
    pub date: NaiveDate,
    pub home: Option<Uuid>,
    pub away: Option<Uuid>,
    /// The primary feed's time, for reporting how many TBAs this fills.
    pub tip_time: Option<NaiveTime>,
    /// "Away at Home", for the unmatched log.
    pub label: String,
}

/// One of our teams in the season being matched.
#[derive(Debug, Clone)]
pub struct OurTeam {
    pub id: Uuid,
    pub name: String,
    pub short_name: Option<String>,
}

#[derive(Debug, Default)]
pub struct MatchOutcome {
    /// (our game, index into the listings).
    pub matched: Vec<(Uuid, usize)>,
    /// Games two listings both claimed; neither is kept.
    pub conflicts: usize,
}

/// A spelling reduced to what the two sources agree on: no accents, case,
/// punctuation or hyphens, and "State" / "Saint" abbreviated, so
/// "San José State" and "San Jose St." meet.
fn name_key(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            '-' | '/' => ' ',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    folded
        .split_whitespace()
        .map(|w| match w {
            "state" => "st",
            "saint" => "st",
            w => w,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Match listings to games. Pure, so the rules above are tested without a
/// network or a database.
pub fn match_listings(listings: &[Listing], games: &[OurGame], teams: &[OurTeam]) -> MatchOutcome {
    // Name key → every team it could mean. Only a key with one team resolves.
    let mut index: HashMap<String, HashSet<Uuid>> = HashMap::new();
    for t in teams {
        for n in std::iter::once(&t.name).chain(t.short_name.as_ref()) {
            index.entry(name_key(n)).or_default().insert(t.id);
        }
    }
    let by_name = |team: &SourceTeam| -> Option<Uuid> {
        team.names.iter().find_map(|n| {
            let ids = index.get(&name_key(n))?;
            (ids.len() == 1).then(|| *ids.iter().next().expect("one id"))
        })
    };

    let mut by_date: HashMap<NaiveDate, Vec<&OurGame>> = HashMap::new();
    for g in games {
        by_date.entry(g.date).or_default().push(g);
    }

    // Source team id → what one-sided matches taught; `None` once two
    // matches disagreed.
    let mut learned: HashMap<String, Option<Uuid>> = HashMap::new();
    let mut claimed: HashMap<Uuid, usize> = HashMap::new();
    let mut contested: HashSet<Uuid> = HashSet::new();
    let mut done: HashSet<usize> = HashSet::new();

    // Two passes: the second retries listings that only became resolvable
    // through what the first taught.
    for _ in 0..2 {
        for (i, l) in listings.iter().enumerate() {
            if done.contains(&i) {
                continue;
            }
            let resolve = |t: &SourceTeam| by_name(t).or_else(|| learned.get(&t.id).copied()?);
            let (home, away) = (resolve(&l.home), resolve(&l.away));
            let day = by_date.get(&l.date).map(Vec::as_slice).unwrap_or(&[]);
            let involves = |g: &OurGame, t: Uuid| g.home == Some(t) || g.away == Some(t);
            let candidates: Vec<&OurGame> = match (home, away) {
                // Either orientation: a neutral-site game's "home" side is
                // a coin toss between sources.
                (Some(h), Some(a)) => day
                    .iter()
                    .copied()
                    .filter(|g| involves(g, h) && involves(g, a))
                    .collect(),
                (Some(t), None) | (None, Some(t)) => {
                    day.iter().copied().filter(|g| involves(g, t)).collect()
                }
                (None, None) => continue,
            };
            let [game] = candidates[..] else {
                continue;
            };
            done.insert(i);
            if contested.contains(&game.game_id) {
                continue;
            }
            if claimed.insert(game.game_id, i).is_some() {
                claimed.remove(&game.game_id);
                contested.insert(game.game_id);
                continue;
            }

            // Teach the side that did not resolve.
            let unresolved = match (home, away) {
                (Some(t), None) => Some((t, &l.away)),
                (None, Some(t)) => Some((t, &l.home)),
                _ => None,
            };
            if let Some((anchor, source)) = unresolved {
                let other = if game.home == Some(anchor) {
                    game.away
                } else {
                    game.home
                };
                if let Some(other) = other {
                    learned
                        .entry(source.id.clone())
                        .and_modify(|v| {
                            if *v != Some(other) {
                                *v = None;
                            }
                        })
                        .or_insert(Some(other));
                }
            }
        }
    }

    let mut matched: Vec<(Uuid, usize)> = claimed.into_iter().collect();
    matched.sort_by_key(|&(_, i)| i);
    MatchOutcome {
        matched,
        conflicts: contested.len(),
    }
}

// --- fetch ---

pub struct BroadcastClient {
    http: reqwest::Client,
}

impl BroadcastClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("cstat/0.1")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("building the broadcast-listing HTTP client")?;
        Ok(BroadcastClient { http })
    }

    /// Every listing for one Eastern day. `groups=50` is Division I, and the
    /// limit is well above the busiest day (~180 games on opening night).
    pub async fn fetch_day(&self, date: NaiveDate) -> Result<Vec<Listing>> {
        let resp = self
            .http
            .get(SCOREBOARD_URL)
            .query(&[
                ("dates", date.format("%Y%m%d").to_string()),
                ("groups", "50".to_string()),
                ("limit", "500".to_string()),
            ])
            .send()
            .await
            .with_context(|| format!("fetching broadcast listings for {date}"))?
            .error_for_status()
            .with_context(|| format!("broadcast listings for {date}"))?;
        let board: Scoreboard = resp
            .json()
            .await
            .with_context(|| format!("parsing broadcast listings for {date}"))?;
        Ok(parse_scoreboard(date, board))
    }
}

// --- run ---

#[derive(Debug, Default)]
pub struct BroadcastReport {
    pub days: usize,
    /// Days whose fetch failed; their games simply go unmatched this run.
    pub failed_days: usize,
    pub listings: usize,
    pub games_in_window: usize,
    pub matched: usize,
    /// Our games in the window with no listing matched. Most are games the
    /// source does not carry; a jump means matching broke.
    pub unmatched_games: usize,
    pub conflicts: usize,
    pub with_network: usize,
    /// Matched games the primary feed has as TBA that now have a time.
    pub times_filled: usize,
    pub written: usize,
}

impl BroadcastReport {
    /// One line for the ledger's notes and the CLI.
    pub fn summary(&self) -> String {
        format!(
            "{} of {} games matched ({} unmatched, {} conflicts), {} with a network, \
             {} TBA times filled; {} listings over {} days ({} failed)",
            self.matched,
            self.games_in_window,
            self.unmatched_games,
            self.conflicts,
            self.with_network,
            self.times_filled,
            self.listings,
            self.days,
            self.failed_days,
        )
    }
}

async fn load_games(
    pool: &PgPool,
    season: i32,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<OurGame>> {
    sqlx::query_as::<_, OurGame>(
        r#"
        SELECT g.id AS game_id, g.game_date AS date, g.home_team_id AS home,
               g.away_team_id AS away, g.tip_time,
               COALESCE(at.short_name, at.name, '?') || ' at '
                   || COALESCE(ht.short_name, ht.name, '?') AS label
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id
        LEFT JOIN teams at ON at.id = g.away_team_id
        WHERE g.season = $1 AND g.game_date BETWEEN $2 AND $3
        "#,
    )
    .bind(season)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("loading games for broadcast matching")
}

async fn load_teams(pool: &PgPool, season: i32) -> Result<Vec<OurTeam>> {
    let rows: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, short_name FROM teams WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await
            .context("loading teams for broadcast matching")?;
    Ok(rows
        .into_iter()
        .map(|(id, name, short_name)| OurTeam {
            id,
            name,
            short_name,
        })
        .collect())
}

async fn write_rows(pool: &PgPool, season: i32, rows: &[(Uuid, &Listing)]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let game_ids: Vec<Uuid> = rows.iter().map(|(g, _)| *g).collect();
    let networks: Vec<Option<String>> = rows.iter().map(|(_, l)| l.network.clone()).collect();
    let times: Vec<Option<NaiveTime>> = rows.iter().map(|(_, l)| l.tip_time).collect();
    let events: Vec<String> = rows.iter().map(|(_, l)| l.event_id.clone()).collect();
    // Only changed rows are rewritten, so `updated_at` says when the source
    // last moved, and the count says how much it did.
    let res = sqlx::query(
        r#"
        INSERT INTO game_broadcasts (game_id, season, network, tip_time, source_event_id)
        SELECT u.game_id, $1, u.network, u.tip_time, u.event_id
        FROM UNNEST($2::uuid[], $3::text[], $4::time[], $5::text[])
            AS u(game_id, network, tip_time, event_id)
        ON CONFLICT (game_id) DO UPDATE SET
            season = EXCLUDED.season,
            network = EXCLUDED.network,
            tip_time = EXCLUDED.tip_time,
            source_event_id = EXCLUDED.source_event_id,
            updated_at = now()
        WHERE (game_broadcasts.season, game_broadcasts.network, game_broadcasts.tip_time,
               game_broadcasts.source_event_id)
            IS DISTINCT FROM
              (EXCLUDED.season, EXCLUDED.network, EXCLUDED.tip_time, EXCLUDED.source_event_id)
        "#,
    )
    .bind(season)
    .bind(&game_ids)
    .bind(&networks)
    .bind(&times)
    .bind(&events)
    .execute(pool)
    .await
    .context("writing game_broadcasts rows")?;
    Ok(res.rows_affected() as usize)
}

/// Fetch, match and write every Eastern day in `from..=to`.
///
/// Days are grouped by season so each is matched against that season's
/// teams: team ids are season-scoped, and one window can straddle the
/// November rollover. Fails only when no day could be fetched; a few failed
/// days are counted and their games left for the next run.
pub async fn run(
    pool: &PgPool,
    client: &BroadcastClient,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<BroadcastReport> {
    if to < from {
        bail!("broadcast window ends ({to}) before it starts ({from})");
    }
    let mut report = BroadcastReport::default();
    let mut by_season: HashMap<i32, Vec<Listing>> = HashMap::new();
    let mut seasons: Vec<i32> = Vec::new();
    let mut last_error = None;
    for date in from.iter_days().take_while(|d| *d <= to) {
        report.days += 1;
        let season = crate::season_for_date(date);
        if !seasons.contains(&season) {
            seasons.push(season);
        }
        match client.fetch_day(date).await {
            Ok(listings) => {
                report.listings += listings.len();
                by_season.entry(season).or_default().extend(listings);
            }
            Err(e) => {
                warn!(%date, error = %format!("{e:#}"), "broadcast listings fetch failed");
                report.failed_days += 1;
                last_error = Some(e);
            }
        }
    }
    if report.failed_days == report.days {
        return Err(last_error
            .expect("a failed day recorded its error")
            .context("every broadcast listings fetch failed"));
    }

    let mut unmatched_labels: Vec<String> = Vec::new();
    for season in seasons {
        let season_from = from.max(season_start(season));
        let season_to = to.min(season_start(season + 1).pred_opt().expect("valid date"));
        let games = load_games(pool, season, season_from, season_to).await?;
        let teams = load_teams(pool, season).await?;
        let listings = by_season.remove(&season).unwrap_or_default();
        let outcome = match_listings(&listings, &games, &teams);

        let game_by_id: HashMap<Uuid, &OurGame> = games.iter().map(|g| (g.game_id, g)).collect();
        let rows: Vec<(Uuid, &Listing)> = outcome
            .matched
            .iter()
            .map(|&(g, i)| (g, &listings[i]))
            .collect();
        let matched_ids: HashSet<Uuid> = rows.iter().map(|(g, _)| *g).collect();

        report.games_in_window += games.len();
        report.matched += rows.len();
        report.conflicts += outcome.conflicts;
        report.with_network += rows.iter().filter(|(_, l)| l.network.is_some()).count();
        report.times_filled += rows
            .iter()
            .filter(|(g, l)| {
                l.tip_time.is_some() && game_by_id.get(g).is_some_and(|g| g.tip_time.is_none())
            })
            .count();
        for g in games.iter().filter(|g| !matched_ids.contains(&g.game_id)) {
            report.unmatched_games += 1;
            unmatched_labels.push(format!("{} {}", g.date, g.label));
        }
        report.written += write_rows(pool, season, &rows).await?;
    }

    unmatched_labels.sort();
    if !unmatched_labels.is_empty() {
        info!(
            count = unmatched_labels.len(),
            sample = %unmatched_labels
                .iter()
                .take(UNMATCHED_LOG_LIMIT)
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
            "games with no broadcast listing matched"
        );
    }
    Ok(report)
}

/// First day of `season` under [`crate::season_for_date`]: November 1 of the
/// prior calendar year.
fn season_start(season: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(season - 1, 11, 1).expect("valid date")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn team(id: u128, name: &str, short: &str) -> OurTeam {
        OurTeam {
            id: Uuid::from_u128(id),
            name: name.into(),
            short_name: Some(short.into()),
        }
    }

    fn game(id: u128, date: &str, home: u128, away: u128) -> OurGame {
        OurGame {
            game_id: Uuid::from_u128(1000 + id),
            date: d(date),
            home: Some(Uuid::from_u128(home)),
            away: Some(Uuid::from_u128(away)),
            tip_time: None,
            label: String::new(),
        }
    }

    fn src(id: &str, names: &[&str]) -> SourceTeam {
        SourceTeam {
            id: id.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn listing(event: &str, date: &str, home: SourceTeam, away: SourceTeam) -> Listing {
        Listing {
            event_id: event.into(),
            date: d(date),
            tip_time: None,
            network: None,
            home,
            away,
        }
    }

    fn teams() -> Vec<OurTeam> {
        vec![
            team(1, "Duke Blue Devils", "Duke"),
            team(2, "Army Black Knights", "Army"),
            team(3, "Connecticut Huskies", "Connecticut"),
            team(4, "Appalachian State Mountaineers", "Appalachian St."),
            team(5, "San Jose State Spartans", "San Jose St."),
            team(6, "Miami (Fla.) Hurricanes", "Miami FL"),
            team(7, "Miami (Ohio) Redhawks", "Miami OH"),
        ]
    }

    #[test]
    fn name_keys_meet_across_spellings() {
        assert_eq!(name_key("San José State"), name_key("San Jose St."));
        assert_eq!(name_key("Gardner-Webb"), name_key("Gardner Webb"));
        assert_eq!(name_key("Saint Mary's"), name_key("St. Mary's"));
    }

    #[test]
    fn both_teams_resolving_matches_either_orientation() {
        let games = [game(1, "2026-11-02", 1, 2)];
        // Listed with home and away the other way round, as a neutral site
        // often is.
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("a", &["Army Black Knights", "Army"]),
            src("d", &["Duke Blue Devils", "Duke"]),
        )];
        let out = match_listings(&ls, &games, &teams());
        assert_eq!(out.matched, vec![(Uuid::from_u128(1001), 0)]);
    }

    #[test]
    fn a_one_sided_match_teaches_the_other_team() {
        let games = [
            // UConn vs Duke: Duke resolves, UConn does not by name.
            game(1, "2026-11-02", 3, 1),
            // Later, UConn vs App State: neither resolves by name alone.
            game(2, "2026-11-05", 4, 3),
            game(3, "2026-11-04", 4, 2),
        ];
        let uconn = || src("41", &["UConn Huskies", "UConn"]);
        let app = || src("2026", &["App State Mountaineers", "App State"]);
        let ls = [
            listing("e2", "2026-11-05", app(), uconn()),
            listing("e1", "2026-11-02", uconn(), src("d", &["Duke"])),
            listing("e3", "2026-11-04", app(), src("a", &["Army"])),
        ];
        let out = match_listings(&ls, &games, &teams());
        let got: HashMap<usize, Uuid> = out.matched.iter().map(|&(g, i)| (i, g)).collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[&0], Uuid::from_u128(1002));
    }

    #[test]
    fn an_ambiguous_name_resolves_neither_team() {
        // Bare "Miami" could be either; with no anchor the game is unmatched.
        let games = [game(1, "2026-11-02", 6, 5)];
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("m", &["Miami"]),
            src("x", &["Somewhere Else"]),
        )];
        assert!(match_listings(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn a_team_with_two_games_that_day_needs_both_sides() {
        let games = [game(1, "2026-11-02", 1, 2), game(2, "2026-11-02", 1, 3)];
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("d", &["Duke"]),
            src("?", &["Unknown"]),
        )];
        assert!(match_listings(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn a_game_claimed_twice_keeps_neither() {
        let games = [game(1, "2026-11-02", 1, 2)];
        let ls = [
            listing("e1", "2026-11-02", src("d", &["Duke"]), src("a", &["Army"])),
            listing(
                "e2",
                "2026-11-02",
                src("d", &["Duke"]),
                src("q", &["Unknown"]),
            ),
        ];
        let out = match_listings(&ls, &games, &teams());
        assert!(out.matched.is_empty());
        assert_eq!(out.conflicts, 1);
    }

    #[test]
    fn a_different_day_never_matches() {
        let games = [game(1, "2026-11-03", 1, 2)];
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("d", &["Duke"]),
            src("a", &["Army"]),
        )];
        assert!(match_listings(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn parses_a_scoreboard_event() {
        let json = r#"{"events":[{"id":"401906848","competitions":[{
            "date":"2026-11-03T23:00Z","timeValid":true,
            "broadcasts":[{"market":"home","names":["ESPN+"]},
                          {"market":"national","names":["SEC Network","ESPN+"]}],
            "competitors":[
              {"homeAway":"home","team":{"id":"2633","displayName":"Tennessee Volunteers",
                "location":"Tennessee","shortDisplayName":"Tennessee"}},
              {"homeAway":"away","team":{"id":"2747","displayName":"Wofford Terriers",
                "location":"Wofford","shortDisplayName":"Wofford"}}]}]},
          {"id":"2","competitions":[{"date":"2026-12-05T05:00Z","timeValid":false,
            "competitors":[
              {"homeAway":"home","team":{"id":"1","displayName":"A"}},
              {"homeAway":"away","team":{"id":"2","displayName":"B"}}]}]}]}"#;
        let board: Scoreboard = serde_json::from_str(json).unwrap();
        let ls = parse_scoreboard(d("2026-11-03"), board);
        assert_eq!(ls.len(), 2);
        // 23:00 UTC in November is 6pm Eastern.
        assert_eq!(ls[0].tip_time, NaiveTime::from_hms_opt(18, 0, 0));
        assert_eq!(ls[0].network.as_deref(), Some("SEC Network / ESPN+"));
        assert_eq!(ls[0].home.id, "2633");
        assert_eq!(ls[0].away.names[0], "Wofford Terriers");
        // An unconfirmed time is no time, not midnight.
        assert_eq!(ls[1].tip_time, None);
        assert_eq!(ls[1].network, None);
    }
}
