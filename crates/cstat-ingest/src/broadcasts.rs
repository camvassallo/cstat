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
//! **The event id is load-bearing.** Since migration 076, `espn_ids` copies
//! the matched event onto `games.espn_id`, and the CBBD steps join through it:
//! `gap_fill` writes box scores and final scores, `cbbd_plays` writes plays,
//! and `espn_lines` reads the record panel's betting lines by the event. A
//! game matched to another game's event gets that game's box, plays and lines.
//!
//! **Both teams must agree.** A listing matches the one game of ours that day
//! whose two teams it positively identifies, in either orientation (a neutral
//! site's "home" side is a coin toss between sources). A listing side is
//! identified, in order, by:
//!
//! 1. its ESPN team id, which is `teams.espn_id` (or the id the same program
//!    carries in another season; ESPN's team ids do not change);
//! 2. its name, when the name keys to exactly one of our teams and no other
//!    ESPN id is tied to that team (its stored id, or another listing's
//!    name, in this window);
//! 3. what one-sided listings taught: an id nothing else places, in listings
//!    whose other side is one of our teams with one game that day, is that
//!    game's opponent, but only when at least two such listings agree, none
//!    disagree, and the team has no ESPN id of its own;
//! 4. as a non-Division I program, when CBBD's cached team list carries the id
//!    with no conference. That side matches our side with no team, so a game
//!    against a non-D1 opponent matches only a listing whose other side is
//!    positively non-D1.
//!
//! One identified side used to be enough when that team had one game that
//! day, and it taught the other side's identity from that one listing. A game
//! the primary feed lists against the wrong opponent then took the real
//! opponent's event: Towson vs Morgan State on 2026-11-14 carried the event of
//! Loyola Maryland at Morgan State.
//!
//! **A match must be unambiguous or it is dropped.** A name key shared by two
//! programs resolves neither, a listing two games fit matches neither, and a
//! game claimed by two listings keeps neither. When both sides have a final
//! score, the two must be the same pair of numbers. An unmatched game shows
//! "TBA" and no network, which is what it showed before; a wrong match would
//! show another game's time, box score, plays and lines.
//!
//! **A wrong tie is undone.** A game's stored event (its `game_broadcasts`
//! row, or `games.espn_id`) is wrong when the event matched another game, the
//! game matched another event, or the event is listed and a side is
//! positively another team, or the final scores differ. Such a game is
//! unlinked ([`unlink`]): the row, the id, its ESPN lines, and the CBBD box,
//! plays and score written through the id all go, for the later steps to
//! redo from the right event. A game the gap-fill created from its event is
//! never unlinked, only reported. `broadcasts --dry-run` reports all of it
//! without writing.
//!
//! **A stale row is pruned, but only on evidence.** On a day fetched
//! successfully, a row goes when its listing is no longer there (the source
//! withdrew it). A row whose listing is still listed but went unmatched this
//! run, with nothing refuting it, is kept: how much the teaching step can
//! resolve depends on the window, and the nightly's window slides, so "not
//! matched tonight" alone would flap rows in and out. A day whose fetch failed
//! is left untouched, and so are the days either side of it, since a listing
//! can match a game dated the neighbouring day.
//!
//! **Midnight sits on a boundary.** The listing writes a midnight tip as 11:59
//! PM the night before, and NatStat dates it the next day, so a listing whose
//! own day has no candidate near midnight is retried on the neighbouring day.
//! Its Eastern date is stored with its time (`tip_date`), and the Schedule tab
//! files each game under the night it tips.
//!
//! **The match also carries the live scores.** The Schedule tab and the
//! scorebug read running scores from this same scoreboard (`crate::live`),
//! joined through the row's event id and `source_home_is_away`, which records
//! which way round the listing has the two teams. A game with no row, or a row
//! kept from an earlier run without an orientation, shows no live score.
//!
//! Best-effort throughout: the nightly records a failure here in the ledger and
//! moves on. What it serves directly is display (the network and tip time
//! columns, and the live score); what it feeds is the event id above. All of
//! a run's writes are one transaction.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime, Timelike, Utc};
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
    #[serde(default)]
    status: Option<RawStatus>,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "type")]
    kind: RawStatusType,
}

#[derive(Deserialize)]
struct RawStatusType {
    /// `pre`, `in` or `post`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    completed: bool,
    /// `2:19 - 2nd`, `Halftime`, `Final/OT`, `Postponed`.
    #[serde(rename = "shortDetail", default)]
    short_detail: String,
}

#[derive(Deserialize)]
struct Competitor {
    #[serde(rename = "homeAway")]
    home_away: String,
    team: RawTeam,
    /// A string ("74"); absent or "0" before tipoff.
    #[serde(default)]
    score: Option<String>,
}

#[derive(Deserialize)]
struct RawTeam {
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(default)]
    location: String,
    // `shortDisplayName` is deliberately not read. It is abbreviated to fit a
    // scoreboard tile, and the abbreviations collide with real programs:
    // Saint Mary-of-the-Woods is "St Mary's", which keys to Saint Mary's
    // (CA). Dropping it matched more games, not fewer (799 of 811 against 797
    // over two weeks), because the teaching pass covers what it used to.
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
    /// The source's full name and its location name, in that order.
    pub names: Vec<String>,
}

/// One game as the source lists it.
#[derive(Debug, Clone)]
pub struct Listing {
    pub event_id: String,
    /// The Eastern day the listing was requested under, which is the Eastern
    /// date of its tip.
    pub date: NaiveDate,
    /// Eastern wall clock; `None` when the source flags the time unconfirmed.
    pub tip_time: Option<NaiveTime>,
    pub network: Option<String>,
    pub home: SourceTeam,
    pub away: SourceTeam,
    /// Where the game stands, in the listing's own home/away orientation.
    pub status: SourceStatus,
}

/// A listing's game state and running score, for the Schedule tab's live
/// scores (`crate::live`). Home and away are the listing's, not ours.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct SourceStatus {
    /// `pre`, `in` or `post`; empty when the listing carries none.
    pub state: String,
    pub completed: bool,
    pub detail: String,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
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
    let names = [t.display_name, t.location]
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
        let mut status = comp
            .status
            .map(|s| SourceStatus {
                state: s.kind.state,
                completed: s.kind.completed,
                detail: s.kind.short_detail,
                ..SourceStatus::default()
            })
            .unwrap_or_default();
        let (mut home, mut away) = (None, None);
        for c in comp.competitors {
            let score = c.score.as_deref().and_then(|s| s.trim().parse().ok());
            match c.home_away.as_str() {
                "home" => {
                    status.home_score = score;
                    home = Some(source_team(c.team));
                }
                "away" => {
                    status.away_score = score;
                    away = Some(source_team(c.team));
                }
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
            status,
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
    /// "Away at Home", for the logs and the report.
    pub label: String,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub status: Option<String>,
    /// The ESPN event `game_broadcasts` holds for it.
    pub stored_event: Option<String>,
    /// `games.espn_id`, which `espn_ids` copied from that event.
    pub espn_id: Option<String>,
    /// Created by the gap-fill from a CBBD game (`natstat_id` `espn:<event>`):
    /// the event is what the row is, so it is never unlinked from it here.
    pub created_from_event: bool,
}

impl OurGame {
    /// Our final score, when the game is over and has one.
    fn final_score(&self) -> Option<(i32, i32)> {
        if !crate::ingest::games::is_settled_status(self.status.as_deref()) {
            return None;
        }
        Some((self.home_score?, self.away_score?))
    }
}

/// One of our teams in the season being matched.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OurTeam {
    pub id: Uuid,
    pub name: String,
    pub short_name: Option<String>,
    /// ESPN's id for the program: `teams.espn_id`, or the one the same
    /// program carries in another season.
    pub espn_id: Option<String>,
    /// `espn_id` is this season's row's own, not borrowed from another
    /// season. Only an own id refutes a stored tie: ESPN re-ids a program
    /// now and then (LIU's 2019 listings carry an id the 2027 LIU does not).
    pub espn_id_own: bool,
}

/// What CBBD's cached team list says about ESPN team ids. Only used to
/// recognise a non-Division I opponent, which we store as no team at all.
#[derive(Debug, Clone, Default)]
pub struct Divisions {
    /// ESPN team ids CBBD lists with no conference: not Division I.
    pub non_d1: HashSet<String>,
    /// The list describes the season being matched. CBBD's list is today's,
    /// so for an earlier season it only matches a non-D1 side to our side
    /// with no team, and never refutes a team of ours: St. Francis Brooklyn
    /// was Division I in 2019 and is not now.
    pub current: bool,
}

/// Who one side of a listing is, as far as the evidence goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideIs {
    Team(Uuid),
    /// A program outside Division I, which our row has as no team.
    NonD1,
    Unknown,
}

/// How a listing fits one of our games, scores aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fit {
    /// Both sides positively ours. `flipped`: the listing's home side is our
    /// away side.
    Confirmed { flipped: bool },
    /// Nothing contradicts it, but a side is unidentified.
    Possible,
    /// A side is positively someone else.
    Contradicted,
}

#[derive(Debug, Default)]
pub struct MatchOutcome {
    /// (our game, index into the listings).
    pub matched: Vec<(Uuid, usize)>,
    /// Matched games whose listing has the two teams the other way round: its
    /// home team is our away team.
    pub flipped: HashSet<Uuid>,
    /// Games two listings both claimed; neither is kept.
    pub conflicts: Vec<Uuid>,
    /// Listings two of our games both fit; none is kept.
    pub ambiguous: Vec<usize>,
    /// (our game, listing) pairs whose teams agree but whose final scores do
    /// not; not kept.
    pub score_mismatches: Vec<(Uuid, usize)>,
    /// Matched pairs whose final scores differ although both teams are
    /// identified by ESPN id: one source has the score wrong. Kept.
    pub score_disputes: Vec<(Uuid, usize)>,
}

/// A spelling reduced to what the two sources agree on: no accents, case,
/// punctuation or hyphens, and "State" / "Saint" abbreviated, so
/// "San José State" and "San Jose St." meet.
pub(crate) fn name_key(s: &str) -> String {
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

/// The neighbouring day the primary feed may file a listing under. The
/// listing writes a midnight tip as 11:59 PM the night before, which the feed
/// dates the next day; and an after-midnight tip is the end of the night
/// before, which the feed sometimes dates that way. Only tried when the
/// listing's own day has no candidate, so a team's game that day always wins.
fn boundary_day(l: &Listing) -> Option<NaiveDate> {
    let t = l.tip_time?;
    if t.hour() >= 23 {
        l.date.succ_opt()
    } else if i64::from(t.hour()) < crate::SCHEDULE_ROLLOVER_HOUR {
        l.date.pred_opt()
    } else {
        None
    }
}

/// How many one-sided listings must agree before an ESPN team id that
/// neither its id nor its name places is taken to be a team of ours. One is
/// never enough: a game of ours against the wrong opponent (the primary feed
/// lists some) would teach the wrong team.
const MIN_LEARN_VOTES: usize = 2;

/// Who each listing side is, for one season's teams over one window of
/// listings. Pure, so the rules are tested without a network or a database.
pub struct Matcher {
    names: HashMap<String, HashSet<Uuid>>,
    /// ESPN team id → our team, for ids exactly one of our teams carries.
    by_espn: HashMap<String, Uuid>,
    /// Our team → every ESPN id that names it directly: its stored id, and
    /// the ids whose names resolve to it in this window. A name is trusted
    /// only while this holds exactly that one id.
    ties: HashMap<Uuid, HashSet<String>>,
    /// ESPN team id → our team, from one-sided listings that all agree.
    learned: HashMap<String, Uuid>,
    non_d1: HashSet<String>,
    /// Our team → its own-season ESPN id, when one of our teams alone holds it.
    own_id: HashMap<Uuid, String>,
    /// A non-D1 side proves our side is not a team of ours only when CBBD's
    /// list is the season's own (the current season).
    non_d1_proves: bool,
}

impl Matcher {
    pub fn new(
        listings: &[Listing],
        games: &[OurGame],
        teams: &[OurTeam],
        div: &Divisions,
    ) -> Self {
        let mut names: HashMap<String, HashSet<Uuid>> = HashMap::new();
        let mut holders: HashMap<&str, Vec<Uuid>> = HashMap::new();
        let own: HashSet<Uuid> = teams
            .iter()
            .filter(|t| t.espn_id_own)
            .map(|t| t.id)
            .collect();
        for t in teams {
            for n in std::iter::once(&t.name).chain(t.short_name.as_ref()) {
                names.entry(name_key(n)).or_default().insert(t.id);
            }
            if let Some(e) = t.espn_id.as_deref() {
                holders.entry(e).or_default().push(t.id);
            }
        }
        let mut by_espn = HashMap::new();
        let mut own_id = HashMap::new();
        let mut ties: HashMap<Uuid, HashSet<String>> = HashMap::new();
        for (e, ids) in holders {
            // An id two of our teams carry names neither.
            if let [id] = ids[..] {
                by_espn.insert(e.to_string(), id);
                ties.entry(id).or_default().insert(e.to_string());
                if own.contains(&id) {
                    own_id.insert(id, e.to_string());
                }
            }
        }
        let mut m = Matcher {
            names,
            by_espn,
            ties,
            learned: HashMap::new(),
            non_d1: div.non_d1.clone(),
            own_id,
            non_d1_proves: div.current,
        };
        for l in listings {
            for side in [&l.home, &l.away] {
                if m.by_espn.contains_key(&side.id) {
                    continue;
                }
                if let Some(t) = m.by_name(side) {
                    m.ties.entry(t).or_default().insert(side.id.clone());
                }
            }
        }
        m.learn(listings, games);
        m
    }

    /// The one team a side's names key to, if exactly one.
    fn by_name(&self, side: &SourceTeam) -> Option<Uuid> {
        side.names.iter().find_map(|n| {
            let ids = self.names.get(&name_key(n))?;
            (ids.len() == 1).then(|| *ids.iter().next().expect("one id"))
        })
    }

    /// A side by its id, then by its name, then as a known non-D1 program:
    /// everything except what one-sided listings taught.
    fn direct(&self, side: &SourceTeam) -> SideIs {
        if let Some(&t) = self.by_espn.get(&side.id) {
            return SideIs::Team(t);
        }
        if let Some(t) = self.by_name(side)
            && self
                .ties
                .get(&t)
                .is_some_and(|ids| ids.len() == 1 && ids.contains(&side.id))
        {
            return SideIs::Team(t);
        }
        if self.non_d1.contains(&side.id) {
            return SideIs::NonD1;
        }
        SideIs::Unknown
    }

    pub fn side(&self, side: &SourceTeam) -> SideIs {
        match self.direct(side) {
            SideIs::Unknown => self
                .learned
                .get(&side.id)
                .map_or(SideIs::Unknown, |&t| SideIs::Team(t)),
            s => s,
        }
    }

    /// Teach an ESPN id nothing else places: in a listing whose other side is
    /// one of our teams with exactly one game that day, it is that game's
    /// opponent. Kept only when [`MIN_LEARN_VOTES`] such listings all agree,
    /// the team they name has no ESPN id of its own, and no other id was
    /// taught the same team.
    fn learn(&mut self, listings: &[Listing], games: &[OurGame]) {
        let by_date = games_by_date(games);
        let mut votes: HashMap<&str, Vec<Uuid>> = HashMap::new();
        for l in listings {
            let (anchor, unknown) = match (self.direct(&l.home), self.direct(&l.away)) {
                (SideIs::Team(t), SideIs::Unknown) => (t, &l.away),
                (SideIs::Unknown, SideIs::Team(t)) => (t, &l.home),
                _ => continue,
            };
            let involves = |g: &&OurGame| g.home == Some(anchor) || g.away == Some(anchor);
            let on = |d: NaiveDate| -> Vec<&OurGame> {
                by_date
                    .get(&d)
                    .map(|v| v.iter().copied().filter(involves).collect())
                    .unwrap_or_default()
            };
            let mut day = on(l.date);
            if day.is_empty()
                && let Some(alt) = boundary_day(l)
            {
                day = on(alt);
            }
            let [g] = day[..] else { continue };
            let other = if g.home == Some(anchor) {
                g.away
            } else {
                g.home
            };
            if let Some(o) = other {
                votes.entry(unknown.id.as_str()).or_default().push(o);
            }
        }
        let mut taught: HashMap<Uuid, Vec<&str>> = HashMap::new();
        for (id, v) in &votes {
            let o = v[0];
            if v.len() >= MIN_LEARN_VOTES
                && v.iter().all(|x| *x == o)
                && !self.non_d1.contains(*id)
                && self.ties.get(&o).is_none_or(HashSet::is_empty)
            {
                taught.entry(o).or_default().push(id);
            }
        }
        for (o, ids) in taught {
            if let [id] = ids[..] {
                self.learned.insert(id.to_string(), o);
            }
        }
    }

    /// Every ESPN id anything ties to our team `t`.
    fn known_ids(&self, t: Uuid) -> HashSet<&str> {
        let mut out: HashSet<&str> = self
            .ties
            .get(&t)
            .map(|s| s.iter().map(String::as_str).collect())
            .unwrap_or_default();
        out.extend(
            self.learned
                .iter()
                .filter(|(_, o)| **o == t)
                .map(|(id, _)| id.as_str()),
        );
        out
    }

    /// One listing side against one of our sides.
    fn side_fit(&self, side: &SourceTeam, ours: Option<Uuid>) -> Fit {
        match (self.side(side), ours) {
            (SideIs::Team(t), Some(o)) if t == o => Fit::Confirmed { flipped: false },
            (SideIs::NonD1, None) => Fit::Confirmed { flipped: false },
            // A name that keys to one of our teams while CBBD lists the id as
            // non-D1: either a program that has since left Division I, or a
            // non-D1 school spelled like one of ours. Neither proves our
            // non-D1 side is someone else.
            (SideIs::Team(_), None)
                if !self.by_espn.contains_key(&side.id) && self.non_d1.contains(&side.id) =>
            {
                Fit::Possible
            }
            (SideIs::Team(_) | SideIs::NonD1, _) => Fit::Contradicted,
            // Unidentified: someone else only when our team is known by
            // another ESPN id.
            (SideIs::Unknown, Some(o)) => {
                let known = self.known_ids(o);
                if known.is_empty() || known.contains(side.id.as_str()) {
                    Fit::Possible
                } else {
                    Fit::Contradicted
                }
            }
            (SideIs::Unknown, None) => Fit::Possible,
        }
    }

    /// How `l` fits `g`, in whichever orientation fits it best. A neutral
    /// site's "home" side is a coin toss between sources, so both are tried.
    pub fn fit(&self, g: &OurGame, l: &Listing) -> Fit {
        // A game with no team of ours on either side ("? at ?") has nothing a
        // listing could confirm: two non-D1 sides would fit any such game.
        if g.home.is_none() && g.away.is_none() {
            return Fit::Possible;
        }
        let rank = |f: &Fit| match f {
            Fit::Confirmed { .. } => 2,
            Fit::Possible => 1,
            Fit::Contradicted => 0,
        };
        let both = |a: Fit, b: Fit| -> Fit {
            match (a, b) {
                (Fit::Contradicted, _) | (_, Fit::Contradicted) => Fit::Contradicted,
                (Fit::Confirmed { .. }, Fit::Confirmed { .. }) => Fit::Confirmed { flipped: false },
                _ => Fit::Possible,
            }
        };
        let straight = both(
            self.side_fit(&l.home, g.home),
            self.side_fit(&l.away, g.away),
        );
        let reversed = match both(
            self.side_fit(&l.home, g.away),
            self.side_fit(&l.away, g.home),
        ) {
            Fit::Confirmed { .. } => Fit::Confirmed { flipped: true },
            f => f,
        };
        if rank(&reversed) > rank(&straight) {
            reversed
        } else {
            straight
        }
    }

    /// Match listings to games: a listing takes the one game of ours on its
    /// day (or, failing any, the boundary day) whose two teams it confirms,
    /// when the final scores, where both have one, agree.
    pub fn match_listings(&self, listings: &[Listing], games: &[OurGame]) -> MatchOutcome {
        let by_date = games_by_date(games);
        let mut out = MatchOutcome::default();
        let mut claimed: HashMap<Uuid, (usize, bool)> = HashMap::new();
        let mut contested: HashSet<Uuid> = HashSet::new();
        for (i, l) in listings.iter().enumerate() {
            let on = |d: NaiveDate| -> Vec<(&OurGame, bool)> {
                by_date
                    .get(&d)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|g| match self.fit(g, l) {
                        Fit::Confirmed { flipped } => Some((*g, flipped)),
                        _ => None,
                    })
                    .collect()
            };
            let mut cands = on(l.date);
            if cands.is_empty()
                && let Some(alt) = boundary_day(l)
            {
                cands = on(alt);
            }
            let (game, flipped) = match cands[..] {
                [] => continue,
                [one] => one,
                _ => {
                    out.ambiguous.push(i);
                    continue;
                }
            };
            if scores_disagree(game, l) {
                if !self.ids_settle(game, l) {
                    out.score_mismatches.push((game.game_id, i));
                    continue;
                }
                out.score_disputes.push((game.game_id, i));
            }
            if contested.contains(&game.game_id) {
                continue;
            }
            if claimed.insert(game.game_id, (i, flipped)).is_some() {
                claimed.remove(&game.game_id);
                contested.insert(game.game_id);
            }
        }
        for (g, (i, flipped)) in claimed {
            if flipped {
                out.flipped.insert(g);
            }
            out.matched.push((g, i));
        }
        out.matched.sort_by_key(|&(_, i)| i);
        out.conflicts = contested.into_iter().collect();
        out.conflicts.sort();
        out
    }

    /// True when the side is identified by an ESPN id: one of our teams'
    /// ids, or one CBBD lists as non-D1.
    fn by_id(&self, side: &SourceTeam) -> bool {
        self.by_espn.contains_key(&side.id) || self.direct(side) == SideIs::NonD1
    }

    /// True when `l` confirms `g` on ESPN ids alone, for both sides. A final
    /// score is the independent check on a match; when both teams are
    /// settled by id, a score that differs is one source's mistake about the
    /// right game (a point off is the usual shape), not a different game.
    fn ids_settle(&self, g: &OurGame, l: &Listing) -> bool {
        matches!(self.fit(g, l), Fit::Confirmed { .. })
            && self.by_id(&l.home)
            && self.by_id(&l.away)
    }

    /// Why the stored link between `g` and `l` is wrong, if the evidence
    /// says it is: ESPN ids that prove a side is another team
    /// ([`Matcher::id_denies`]), or, when a side is unidentified and nothing
    /// contradicts the rest, final scores that differ. `None` when it fits or
    /// cannot be told.
    ///
    /// A score never refutes a link whose two teams are both identified: the
    /// same two teams on the same day are the same game, and the differing
    /// score is one source's mistake about it. Such a link is not made anew
    /// on names alone ([`Matcher::match_listings`]), but one already made
    /// is kept, since unlinking would also drop the right game's CBBD box and
    /// plays.
    pub fn refutes(&self, g: &OurGame, l: &Listing) -> Option<String> {
        let fit = self.fit(g, l);
        if self.ids_refute(g, l) {
            return Some(format!(
                "teams differ: ESPN lists {} at {}",
                l.away.names.first().map_or("?", String::as_str),
                l.home.names.first().map_or("?", String::as_str),
            ));
        }
        if fit == Fit::Possible && score_gap(g, l).is_some_and(|d| d >= SCORE_REFUTE_POINTS) {
            let (h, a) = g.final_score().expect("checked by scores_disagree");
            return Some(format!(
                "final scores differ: ours {h}-{a}, ESPN {}-{}",
                l.status.home_score.unwrap_or_default(),
                l.status.away_score.unwrap_or_default(),
            ));
        }
        None
    }

    /// True when ESPN ids prove a listing side is not our side: the side
    /// carries one of our teams' own-season ids and our side is not that
    /// team; or it is a non-D1 program and our side is a team; or our side's
    /// team has an own-season id and the listing side's is a different one.
    /// Names, borrowed ids and taught ids never prove it: they decide what
    /// to match, but a mistake in them must not unlink a right game.
    fn id_denies(&self, side: &SourceTeam, ours: Option<Uuid>) -> bool {
        if let Some(&t) = self.by_espn.get(&side.id)
            && self.own_id.get(&t) == Some(&side.id)
        {
            return ours != Some(t);
        }
        if self.non_d1_proves && self.direct(side) == SideIs::NonD1 {
            return ours.is_some();
        }
        ours.and_then(|o| self.own_id.get(&o))
            .is_some_and(|id| *id != side.id)
    }

    /// True when ids refute `l` as `g` in both orientations.
    fn ids_refute(&self, g: &OurGame, l: &Listing) -> bool {
        let straight = self.id_denies(&l.home, g.home) || self.id_denies(&l.away, g.away);
        let reversed = self.id_denies(&l.home, g.away) || self.id_denies(&l.away, g.home);
        straight && reversed
    }
}

fn games_by_date(games: &[OurGame]) -> HashMap<NaiveDate, Vec<&OurGame>> {
    let mut by_date: HashMap<NaiveDate, Vec<&OurGame>> = HashMap::new();
    for g in games {
        by_date.entry(g.date).or_default().push(g);
    }
    by_date
}

/// The listing's final score, when it is over and has one.
fn listing_final(l: &Listing) -> Option<(i32, i32)> {
    let s = &l.status;
    if !s.completed || s.state != "post" {
        return None;
    }
    let (h, a) = (s.home_score?, s.away_score?);
    (h + a > 0).then_some((h, a))
}

/// How far apart the two final scores are, in total points over both sides,
/// in whichever orientation brings them closest; `None` unless both have one.
/// Either orientation counts: which way round is the team check's job, and
/// the two sources do not always agree on it.
fn score_gap(g: &OurGame, l: &Listing) -> Option<i32> {
    let (h, a) = g.final_score()?;
    let (lh, la) = listing_final(l)?;
    Some(((h - lh).abs() + (a - la).abs()).min((h - la).abs() + (a - lh).abs()))
}

/// True when both sides have a final score and they are not the same two
/// numbers. A new match must agree exactly.
fn scores_disagree(g: &OurGame, l: &Listing) -> bool {
    score_gap(g, l).is_some_and(|d| d > 0)
}

/// The score gap at which a stored tie with an unidentified side is taken
/// to be the wrong game. Smaller gaps are one source a point or two off on
/// the right game (2025-11-03 Kennesaw State: 105-30 here, 105-29 at ESPN),
/// and unlinking would drop that game's box and plays; two different games
/// landing this close on both sides is rare.
const SCORE_REFUTE_POINTS: i32 = 4;

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

/// One game unlinked from an ESPN event it was wrongly tied to.
#[derive(Debug, Clone)]
pub struct Unlink {
    pub game_id: Uuid,
    pub date: NaiveDate,
    pub label: String,
    pub event_id: String,
    pub reason: String,
    /// The event is the game's `game_broadcasts` row, which `espn_lines`
    /// fetched its lines through.
    pub via_row: bool,
    /// The event is the game's `games.espn_id`, which the CBBD steps wrote
    /// its box, plays and score through.
    pub via_espn_id: bool,
}

/// What unlinking removed, in rows.
#[derive(Debug, Default, Clone, Copy)]
pub struct Removed {
    pub broadcast_rows: u64,
    pub espn_ids: u64,
    pub espn_lines: u64,
    pub cbbd_team_rows: u64,
    pub cbbd_player_rows: u64,
    pub cbbd_plays: u64,
    pub cbbd_scores: u64,
}

#[derive(Debug, Default)]
pub struct BroadcastReport {
    pub days: usize,
    /// Days whose fetch failed. Their games are left out of every count below
    /// and keep whatever rows they had.
    pub failed_days: usize,
    pub listings: usize,
    pub games_in_window: usize,
    pub matched: usize,
    /// Our games in the window with no listing matched. Most are games the
    /// source does not carry; a jump means matching broke.
    pub unmatched_games: usize,
    /// Games two listings both claimed; nothing written for them.
    pub conflicts: Vec<String>,
    /// Listings that fit two of our games; nothing written for them.
    pub ambiguous: Vec<String>,
    /// Listings whose teams fit one of our games but whose final score does
    /// not; nothing written for them.
    pub score_mismatches: Vec<String>,
    /// Matched games whose final score differs from the listing's although
    /// ESPN ids settle both teams; matched, and named for a look.
    pub score_disputes: Vec<String>,
    pub with_network: usize,
    /// Matched games the primary feed has as TBA that now have a time.
    pub times_filled: usize,
    pub written: usize,
    /// Rows removed because their listing is gone.
    pub pruned: usize,
    /// Games taken off an ESPN event that is not theirs, and what went with it.
    pub unlinked: Vec<Unlink>,
    pub removed: Removed,
    /// Games created from an ESPN event that this run would have unlinked;
    /// left alone and named instead.
    pub created_kept: Vec<String>,
    /// CBBD's team list was not cached, so no non-D1 opponent was recognised.
    pub no_divisions: bool,
    /// Nothing was written: the run's writes were rolled back.
    pub dry_run: bool,
}

impl BroadcastReport {
    /// One line for the ledger's notes and the CLI.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} of {} games matched ({} unmatched, {} conflicts, {} ambiguous, {} score \
             mismatches), {} with a network, {} TBA times filled, {} stale rows pruned, {} \
             games unlinked from a wrong ESPN event; {} listings over {} days ({} failed)",
            self.matched,
            self.games_in_window,
            self.unmatched_games,
            self.conflicts.len(),
            self.ambiguous.len(),
            self.score_mismatches.len(),
            self.with_network,
            self.times_filled,
            self.pruned,
            self.unlinked.len(),
            self.listings,
            self.days,
            self.failed_days,
        );
        if self.no_divisions {
            s.push_str("; CBBD team list not cached, so no non-D1 opponent matched");
        }
        if self.dry_run {
            s.push_str(" [dry run: nothing written]");
        }
        s
    }

    /// Every unlink and refusal, one per line, for the CLI.
    pub fn details(&self) -> String {
        let mut out = String::new();
        let r = &self.removed;
        if !self.unlinked.is_empty() {
            out.push_str(&format!(
                "unlinked {} games: {} broadcast rows, {} games.espn_id, {} ESPN line rows, \
                 {} CBBD team box rows, {} CBBD player box rows, {} CBBD plays, {} CBBD \
                 scores cleared\n",
                self.unlinked.len(),
                r.broadcast_rows,
                r.espn_ids,
                r.espn_lines,
                r.cbbd_team_rows,
                r.cbbd_player_rows,
                r.cbbd_plays,
                r.cbbd_scores,
            ));
            for u in &self.unlinked {
                out.push_str(&format!(
                    "  {} {} [{}] event {}: {}\n",
                    u.date, u.label, u.game_id, u.event_id, u.reason
                ));
            }
        }
        for (title, lines) in [
            ("created from an event, kept", &self.created_kept),
            ("games two listings claim", &self.conflicts),
            ("ambiguous listings", &self.ambiguous),
            ("score mismatches, not matched", &self.score_mismatches),
            ("score disputes, matched on ESPN ids", &self.score_disputes),
        ] {
            if !lines.is_empty() {
                out.push_str(&format!("{title} ({}):\n", lines.len()));
                for l in lines {
                    out.push_str(&format!("  {l}\n"));
                }
            }
        }
        out
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
                   || COALESCE(ht.short_name, ht.name, '?') AS label,
               g.home_score, g.away_score, g.status,
               b.source_event_id AS stored_event, g.espn_id,
               starts_with(g.natstat_id, $4) AS created_from_event
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id
        LEFT JOIN teams at ON at.id = g.away_team_id
        LEFT JOIN game_broadcasts b ON b.game_id = g.id
        WHERE g.season = $1 AND g.game_date BETWEEN $2 AND $3
        "#,
    )
    .bind(season)
    .bind(from)
    .bind(to)
    .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
    .fetch_all(pool)
    .await
    .context("loading games for broadcast matching")
}

/// The season's teams, each with ESPN's id for it: its own, or failing that
/// the one the same program carries in another season.
async fn load_teams(pool: &PgPool, season: i32) -> Result<Vec<OurTeam>> {
    sqlx::query_as::<_, OurTeam>(
        "SELECT t.id, t.name, t.short_name, \
                COALESCE(t.espn_id, ( \
                    SELECT o.espn_id FROM teams o \
                    WHERE o.natstat_id = t.natstat_id AND o.espn_id IS NOT NULL \
                    ORDER BY abs(o.season - t.season) LIMIT 1)) AS espn_id, \
                t.espn_id IS NOT NULL AS espn_id_own \
         FROM teams t WHERE t.season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .context("loading teams for broadcast matching")
}

/// Non-D1 ESPN team ids from CBBD's cached team list (no call). `None` when
/// nothing is cached.
async fn load_divisions(pool: &PgPool) -> Result<Option<Divisions>> {
    let Some(teams) = crate::cbbd::cached_teams(pool)
        .await
        .context("reading CBBD's cached team list")?
    else {
        return Ok(None);
    };
    Ok(Some(Divisions {
        non_d1: teams
            .into_iter()
            .filter(|t| t.conference.as_deref().is_none_or(|c| c.trim().is_empty()))
            .filter_map(|t| t.source_id)
            .collect(),
        current: false,
    }))
}

async fn write_rows(
    tx: &mut sqlx::PgConnection,
    season: i32,
    rows: &[(Uuid, &Listing)],
    flipped: &HashSet<Uuid>,
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let game_ids: Vec<Uuid> = rows.iter().map(|(g, _)| *g).collect();
    let networks: Vec<Option<String>> = rows.iter().map(|(_, l)| l.network.clone()).collect();
    let times: Vec<Option<NaiveTime>> = rows.iter().map(|(_, l)| l.tip_time).collect();
    let dates: Vec<Option<NaiveDate>> = rows
        .iter()
        .map(|(_, l)| l.tip_time.map(|_| l.date))
        .collect();
    let events: Vec<String> = rows.iter().map(|(_, l)| l.event_id.clone()).collect();
    let reversed: Vec<bool> = rows.iter().map(|(g, _)| flipped.contains(g)).collect();
    // Only changed rows are rewritten, so `updated_at` says when the source
    // last moved, and the count says how much it did.
    let res = sqlx::query(
        r#"
        INSERT INTO game_broadcasts
            (game_id, season, network, tip_time, tip_date, source_event_id,
             source_home_is_away)
        SELECT u.game_id, $1, u.network, u.tip_time, u.tip_date, u.event_id, u.reversed
        FROM UNNEST($2::uuid[], $3::text[], $4::time[], $5::date[], $6::text[], $7::bool[])
            AS u(game_id, network, tip_time, tip_date, event_id, reversed)
        ON CONFLICT (game_id) DO UPDATE SET
            season = EXCLUDED.season,
            network = EXCLUDED.network,
            tip_time = EXCLUDED.tip_time,
            tip_date = EXCLUDED.tip_date,
            source_event_id = EXCLUDED.source_event_id,
            source_home_is_away = EXCLUDED.source_home_is_away,
            updated_at = now()
        WHERE (game_broadcasts.season, game_broadcasts.network, game_broadcasts.tip_time,
               game_broadcasts.tip_date, game_broadcasts.source_event_id,
               game_broadcasts.source_home_is_away)
            IS DISTINCT FROM
              (EXCLUDED.season, EXCLUDED.network, EXCLUDED.tip_time, EXCLUDED.tip_date,
               EXCLUDED.source_event_id, EXCLUDED.source_home_is_away)
        "#,
    )
    .bind(season)
    .bind(&game_ids)
    .bind(&networks)
    .bind(&times)
    .bind(&dates)
    .bind(&events)
    .bind(&reversed)
    .execute(&mut *tx)
    .await
    .context("writing game_broadcasts rows")?;
    Ok(res.rows_affected() as usize)
}

/// Delete the rows of `games` (all on successfully fetched days) that this
/// run did not match, unless their listing is among `unmatched_events`: still
/// listed, just not resolved tonight. A listing that matched another game is
/// not in that set, so its old row goes.
async fn prune_unmatched(
    tx: &mut sqlx::PgConnection,
    games: &[OurGame],
    matched: &HashSet<Uuid>,
    unmatched_events: &[String],
) -> Result<usize> {
    let stale: Vec<Uuid> = games
        .iter()
        .map(|g| g.game_id)
        .filter(|id| !matched.contains(id))
        .collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let res = sqlx::query(
        "DELETE FROM game_broadcasts \
         WHERE game_id = ANY($1) AND NOT (source_event_id = ANY($2))",
    )
    .bind(&stale)
    .bind(unmatched_events)
    .execute(&mut *tx)
    .await
    .context("pruning unmatched game_broadcasts rows")?;
    Ok(res.rows_affected() as usize)
}

/// The ESPN events tied to our games that are not theirs.
///
/// A game is tied to an event by its `game_broadcasts` row and by
/// `games.espn_id` (copied from that row by `espn_ids`). A tie is wrong when
/// the event matched another game this run, when the game matched a
/// different event this run, or when the event is listed and its teams or
/// final score refute the game ([`Matcher::refutes`]). A tie whose event is
/// listed but only unconfirmed (a side nothing identifies) is left alone, as
/// is one whose event is not in the window.
pub fn wrong_ties(
    matcher: &Matcher,
    listings: &[Listing],
    games: &[OurGame],
    outcome: &MatchOutcome,
) -> Vec<Unlink> {
    let listing_of: HashMap<&str, &Listing> =
        listings.iter().map(|l| (l.event_id.as_str(), l)).collect();
    let event_of: HashMap<Uuid, &str> = outcome
        .matched
        .iter()
        .map(|&(g, i)| (g, listings[i].event_id.as_str()))
        .collect();
    let game_of: HashMap<&str, Uuid> = event_of.iter().map(|(g, e)| (*e, *g)).collect();
    let label_of: HashMap<Uuid, &str> = games
        .iter()
        .map(|g| (g.game_id, g.label.as_str()))
        .collect();
    let mut out = Vec::new();
    for g in games {
        let new = event_of.get(&g.game_id).copied();
        let mut olds: Vec<&str> = [g.stored_event.as_deref(), g.espn_id.as_deref()]
            .into_iter()
            .flatten()
            .filter(|e| Some(*e) != new)
            .collect();
        olds.dedup();
        for old in olds {
            let reason = if let Some(&other) = game_of.get(old) {
                Some(format!(
                    "event belongs to {}",
                    label_of.get(&other).copied().unwrap_or("another game")
                ))
            } else if let Some(new) = new {
                Some(format!("game matched event {new} instead"))
            } else {
                listing_of.get(old).and_then(|l| matcher.refutes(g, l))
            };
            if let Some(reason) = reason {
                out.push(Unlink {
                    game_id: g.game_id,
                    date: g.date,
                    label: g.label.clone(),
                    event_id: old.to_string(),
                    reason,
                    via_row: g.stored_event.as_deref() == Some(old),
                    via_espn_id: g.espn_id.as_deref() == Some(old),
                });
            }
        }
    }
    out
}

/// Take each game off the ESPN event it was wrongly tied to, and remove what
/// that event brought onto it: the `game_broadcasts` row, `games.espn_id`,
/// the event's betting lines (`espn_game_lines` has one row per game, read
/// through the event), and anything the CBBD steps wrote through the id
/// (box rows and plays with `source = 'cbbd'`, and a score the gap-fill set).
/// The next `espn_ids`, `espn_lines`, `gap-fill` and `cbbd-plays` runs
/// redo them from the right event, if one is found.
async fn unlink(tx: &mut sqlx::PgConnection, ties: &[Unlink]) -> Result<Removed> {
    let mut r = Removed::default();
    if ties.is_empty() {
        return Ok(r);
    }
    let games: Vec<Uuid> = ties.iter().map(|u| u.game_id).collect();
    let events: Vec<String> = ties.iter().map(|u| u.event_id.clone()).collect();
    // Lines go only with the event they were fetched through, and CBBD rows
    // only with the id they were written through: a game whose other tie is
    // right keeps what that tie brought.
    let lined: Vec<Uuid> = ties
        .iter()
        .filter(|u| u.via_row)
        .map(|u| u.game_id)
        .collect();
    let filled: Vec<Uuid> = ties
        .iter()
        .filter(|u| u.via_espn_id)
        .map(|u| u.game_id)
        .collect();
    let src = crate::gap_fill::CBBD_SOURCE;
    r.broadcast_rows = sqlx::query(
        "DELETE FROM game_broadcasts b USING UNNEST($1::uuid[], $2::text[]) AS u(g, e) \
         WHERE b.game_id = u.g AND b.source_event_id = u.e",
    )
    .bind(&games)
    .bind(&events)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    r.espn_ids = sqlx::query(
        "UPDATE games g SET espn_id = NULL, updated_at = now() \
         FROM UNNEST($1::uuid[], $2::text[]) AS u(g, e) \
         WHERE g.id = u.g AND g.espn_id = u.e",
    )
    .bind(&games)
    .bind(&events)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    r.espn_lines = sqlx::query("DELETE FROM espn_game_lines WHERE game_id = ANY($1)")
        .bind(&lined)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    r.cbbd_player_rows =
        sqlx::query("DELETE FROM player_game_stats WHERE game_id = ANY($1) AND source = $2")
            .bind(&filled)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    r.cbbd_team_rows =
        sqlx::query("DELETE FROM team_game_stats WHERE game_id = ANY($1) AND source = $2")
            .bind(&filled)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    r.cbbd_plays = sqlx::query("DELETE FROM play_by_play WHERE game_id = ANY($1) AND source = $2")
        .bind(&filled)
        .bind(crate::cbbd_plays::SOURCE)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    // The score came from the wrong game, so the game goes back to unplayed
    // until a source with the right one reports it.
    r.cbbd_scores = sqlx::query(
        "UPDATE games SET home_score = NULL, away_score = NULL, status = NULL, \
                score_source = NULL, updated_at = now() \
         WHERE id = ANY($1) AND score_source = $2",
    )
    .bind(&filled)
    .bind(src)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(r)
}

impl Removed {
    fn add(&mut self, o: Removed) {
        self.broadcast_rows += o.broadcast_rows;
        self.espn_ids += o.espn_ids;
        self.espn_lines += o.espn_lines;
        self.cbbd_team_rows += o.cbbd_team_rows;
        self.cbbd_player_rows += o.cbbd_player_rows;
        self.cbbd_plays += o.cbbd_plays;
        self.cbbd_scores += o.cbbd_scores;
    }
}

/// Fetch, match and write every Eastern day in `from..=to`, and take any game
/// in it off an ESPN event that is not its own ([`wrong_ties`], [`unlink`]).
/// With `dry_run` everything is computed and counted inside a transaction
/// that is rolled back.
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
    dry_run: bool,
) -> Result<BroadcastReport> {
    if to < from {
        bail!("broadcast window ends ({to}) before it starts ({from})");
    }
    let mut report = BroadcastReport {
        dry_run,
        ..Default::default()
    };
    let mut by_season: HashMap<i32, Vec<Listing>> = HashMap::new();
    let mut fetched: HashSet<NaiveDate> = HashSet::new();
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
                fetched.insert(date);
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
    let divisions = load_divisions(pool).await?;
    report.no_divisions = divisions.is_none();
    let divisions = divisions.unwrap_or_default();

    let mut tx = pool.begin().await.context("opening the broadcasts write")?;
    let mut unmatched_labels: Vec<String> = Vec::new();
    for season in seasons {
        let season_from = from.max(season_start(season));
        let season_to = to.min(season_start(season + 1).pred_opt().expect("valid date"));
        // A day either side too: a listing at the edge of the window can
        // belong to a game the feed dates on the neighbouring day.
        let nearby = load_games(
            pool,
            season,
            season_from.pred_opt().expect("valid date"),
            season_to.succ_opt().expect("valid date"),
        )
        .await?;
        let teams = load_teams(pool, season).await?;
        let mut listings = by_season.remove(&season).unwrap_or_default();
        // One event is one game; a repeat would only ever conflict with itself.
        let mut seen = HashSet::new();
        listings.retain(|l| seen.insert(l.event_id.clone()));
        let season_div = Divisions {
            current: season == crate::current_natstat_season(),
            ..divisions.clone()
        };
        let matcher = Matcher::new(&listings, &nearby, &teams, &season_div);
        let outcome = matcher.match_listings(&listings, &nearby);
        let mut games = nearby;
        games.retain(|g| fetched.contains(&g.date));

        let game_by_id: HashMap<Uuid, &OurGame> = games.iter().map(|g| (g.game_id, g)).collect();
        let rows: Vec<(Uuid, &Listing)> = outcome
            .matched
            .iter()
            .map(|&(g, i)| (g, &listings[i]))
            .collect();
        let matched_ids: HashSet<Uuid> = rows.iter().map(|(g, _)| *g).collect();

        report.games_in_window += games.len();
        report.matched += rows.len();
        let label = |g: &Uuid| game_by_id.get(g).map_or("?", |g| g.label.as_str());
        for g in &outcome.conflicts {
            report
                .conflicts
                .push(format!("{label} [{g}]", label = label(g)));
        }
        for &i in &outcome.ambiguous {
            let l = &listings[i];
            report.ambiguous.push(format!(
                "{} event {}: {} at {}",
                l.date,
                l.event_id,
                l.away.names.first().map_or("?", String::as_str),
                l.home.names.first().map_or("?", String::as_str),
            ));
        }
        for (pairs, into) in [
            (&outcome.score_mismatches, &mut report.score_mismatches),
            (&outcome.score_disputes, &mut report.score_disputes),
        ] {
            for &(g, i) in pairs {
                let l = &listings[i];
                let ours = game_by_id
                    .get(&g)
                    .and_then(|g| g.final_score())
                    .map_or("?".to_string(), |(h, a)| format!("{h}-{a}"));
                into.push(format!(
                    "{} {} [{g}] event {}: ours {ours}, ESPN {}-{}",
                    l.date,
                    label(&g),
                    l.event_id,
                    l.status.home_score.unwrap_or_default(),
                    l.status.away_score.unwrap_or_default(),
                ));
            }
        }
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

        // Wrong ties go first, so the rows written below land on clean games.
        let (created, ties): (Vec<Unlink>, Vec<Unlink>) =
            wrong_ties(&matcher, &listings, &games, &outcome)
                .into_iter()
                .partition(|u| {
                    game_by_id
                        .get(&u.game_id)
                        .is_some_and(|g| g.created_from_event)
                });
        for u in created {
            report.created_kept.push(format!(
                "{} {} [{}] event {}: {}",
                u.date, u.label, u.game_id, u.event_id, u.reason
            ));
        }
        // A created game takes only the event it was created from.
        let rows: Vec<(Uuid, &Listing)> = rows
            .into_iter()
            .filter(|(g, l)| {
                game_by_id.get(g).is_none_or(|g| {
                    !g.created_from_event || g.espn_id.as_deref() == Some(l.event_id.as_str())
                })
            })
            .collect();
        report.removed.add(unlink(&mut tx, &ties).await?);
        report.unlinked.extend(ties);

        report.written += write_rows(&mut tx, season, &rows, &outcome.flipped).await?;
        let matched_listings: HashSet<usize> = outcome.matched.iter().map(|&(_, i)| i).collect();
        let still_listed: Vec<String> = listings
            .iter()
            .enumerate()
            .filter(|(i, _)| !matched_listings.contains(i))
            .map(|(_, l)| l.event_id.clone())
            .collect();
        // Only a game whose neighbouring days were fetched too: either could
        // hold the listing it matches.
        let settled: Vec<OurGame> = games
            .iter()
            .filter(|g| {
                [g.date.pred_opt(), g.date.succ_opt()]
                    .into_iter()
                    .all(|d| d.is_some_and(|d| fetched.contains(&d)))
            })
            .cloned()
            .collect();
        report.pruned += prune_unmatched(&mut tx, &settled, &matched_ids, &still_listed).await?;
    }
    if dry_run {
        tx.rollback().await.context("rolling back the dry run")?;
    } else {
        tx.commit()
            .await
            .context("committing the broadcasts write")?;
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
    for u in &report.unlinked {
        warn!(date = %u.date, game = %u.label, event = %u.event_id, reason = %u.reason,
              dry_run, "game unlinked from an ESPN event that is not its own");
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

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn team(id: u128, name: &str, short: &str) -> OurTeam {
        OurTeam {
            id: u(id),
            name: name.into(),
            short_name: Some(short.into()),
            espn_id: None,
            espn_id_own: false,
        }
    }

    fn with_espn(mut t: OurTeam, espn: &str) -> OurTeam {
        t.espn_id = Some(espn.into());
        t.espn_id_own = true;
        t
    }

    fn game_opt(id: u128, date: &str, home: Option<u128>, away: Option<u128>) -> OurGame {
        OurGame {
            game_id: u(1000 + id),
            date: d(date),
            home: home.map(u),
            away: away.map(u),
            tip_time: None,
            label: format!("game {id}"),
            home_score: None,
            away_score: None,
            status: None,
            stored_event: None,
            espn_id: None,
            created_from_event: false,
        }
    }

    fn game(id: u128, date: &str, home: u128, away: u128) -> OurGame {
        game_opt(id, date, Some(home), Some(away))
    }

    fn scored(mut g: OurGame, h: i32, a: i32) -> OurGame {
        g.home_score = Some(h);
        g.away_score = Some(a);
        g.status = Some("Final".into());
        g
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
            status: SourceStatus::default(),
        }
    }

    fn final_listing(mut l: Listing, h: i32, a: i32) -> Listing {
        l.status = SourceStatus {
            state: "post".into(),
            completed: true,
            detail: "Final".into(),
            home_score: Some(h),
            away_score: Some(a),
        };
        l
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

    fn run_match(ls: &[Listing], games: &[OurGame], teams: &[OurTeam]) -> MatchOutcome {
        run_match_div(ls, games, teams, &Divisions::default())
    }

    fn run_match_div(
        ls: &[Listing],
        games: &[OurGame],
        teams: &[OurTeam],
        div: &Divisions,
    ) -> MatchOutcome {
        Matcher::new(ls, games, teams, div).match_listings(ls, games)
    }

    fn duke() -> SourceTeam {
        src("150", &["Duke Blue Devils", "Duke"])
    }

    fn army() -> SourceTeam {
        src("349", &["Army Black Knights", "Army"])
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
        let ls = [listing("e1", "2026-11-02", army(), duke())];
        let out = run_match(&ls, &games, &teams());
        assert_eq!(out.matched, vec![(u(1001), 0)]);
        // Recorded, so the listing's score lands on the right team.
        assert_eq!(out.flipped, HashSet::from([u(1001)]));
    }

    #[test]
    fn one_identified_team_is_not_enough() {
        // Towson vs Morgan State on 2026-11-14: our feed had Towson against
        // Morgan State, ESPN listed Loyola Maryland at Morgan State. Morgan
        // State had one game that day, which used to pin the match.
        let ts = vec![
            team(10, "Towson Tigers", "Towson"),
            team(11, "Morgan State Bears", "Morgan St."),
            team(12, "Loyola Maryland Greyhounds", "Loyola MD"),
        ];
        let games = [game(1, "2026-11-14", 10, 11)];
        let morgan = || src("2415", &["Morgan State Bears", "Morgan State"]);
        // Whether or not the other side's name resolves, nothing matches.
        for loyola in [
            src("2352", &["Loyola Maryland Greyhounds", "Loyola Maryland"]),
            src("2352", &["Some Unknown Name"]),
        ] {
            let ls = [listing("401920567", "2026-11-14", morgan(), loyola)];
            assert!(run_match(&ls, &games, &ts).matched.is_empty());
        }
    }

    #[test]
    fn a_stored_tie_to_another_teams_event_is_refuted() {
        let ts = vec![
            with_espn(team(10, "Towson Tigers", "Towson"), "119"),
            with_espn(team(11, "Morgan State Bears", "Morgan St."), "2415"),
            with_espn(team(12, "Loyola Maryland Greyhounds", "Loyola MD"), "2352"),
        ];
        let mut g = game(1, "2026-11-14", 10, 11);
        g.stored_event = Some("401920567".into());
        g.espn_id = Some("401920567".into());
        let ls = [listing(
            "401920567",
            "2026-11-14",
            src("2415", &["Morgan State Bears"]),
            src("2352", &["Loyola Maryland Greyhounds"]),
        )];
        let games = [g];
        let m = Matcher::new(&ls, &games, &ts, &Divisions::default());
        let out = m.match_listings(&ls, &games);
        assert!(out.matched.is_empty());
        let ties = wrong_ties(&m, &ls, &games, &out);
        assert_eq!(ties.len(), 1, "{ties:?}");
        assert_eq!(ties[0].event_id, "401920567");
        assert!(
            ties[0].reason.starts_with("teams differ"),
            "{}",
            ties[0].reason
        );
    }

    #[test]
    fn an_unidentified_side_refutes_only_a_team_known_by_another_id() {
        let games = [game(1, "2026-11-14", 1, 2)];
        let l = listing("e1", "2026-11-14", duke(), src("777", &["Nobody"]));
        // Army has no ESPN id: the tie cannot be told, and is kept.
        let m = Matcher::new(
            std::slice::from_ref(&l),
            &games,
            &teams(),
            &Divisions::default(),
        );
        assert_eq!(m.fit(&games[0], &l), Fit::Possible);
        assert_eq!(m.refutes(&games[0], &l), None);
        // Army's id is borrowed from another season: it keeps the listing
        // from matching, but proves nothing, so the tie is kept.
        let mut ts = teams();
        ts[1].espn_id = Some("349".into());
        let m = Matcher::new(std::slice::from_ref(&l), &games, &ts, &Divisions::default());
        assert_eq!(m.fit(&games[0], &l), Fit::Contradicted);
        assert_eq!(m.refutes(&games[0], &l), None);
        // Army's own id this season is 349, so 777 is someone else.
        ts[1].espn_id_own = true;
        let m = Matcher::new(std::slice::from_ref(&l), &games, &ts, &Divisions::default());
        assert!(m.refutes(&games[0], &l).is_some());
    }

    #[test]
    fn a_name_never_refutes() {
        // A listing side whose name keys to one of our teams (here Army,
        // with no ESPN id) is matched on, but never proves a stored tie
        // wrong: generic keys collide ("St. Francis").
        let games = [game(1, "2026-11-14", 1, 3)];
        let l = listing("e1", "2026-11-14", duke(), army());
        let m = Matcher::new(
            std::slice::from_ref(&l),
            &games,
            &teams(),
            &Divisions::default(),
        );
        assert_eq!(m.fit(&games[0], &l), Fit::Contradicted);
        assert_eq!(m.refutes(&games[0], &l), None);
    }

    #[test]
    fn an_espn_id_beats_an_ambiguous_name() {
        let mut ts = teams();
        ts[5].espn_id = Some("2390".into());
        let games = [game(1, "2026-11-02", 6, 1)];
        let ls = [listing("e1", "2026-11-02", src("2390", &["Miami"]), duke())];
        assert_eq!(run_match(&ls, &games, &ts).matched, vec![(u(1001), 0)]);
    }

    #[test]
    fn a_name_is_not_trusted_for_a_team_known_by_another_id() {
        // Our Duke is ESPN 150; a listing side named "Duke" with id 999 is
        // not Duke, and a game of Duke's does not match it.
        let mut ts = teams();
        ts[0].espn_id = Some("150".into());
        let games = [game(1, "2026-11-02", 1, 2)];
        let ls = [listing("e1", "2026-11-02", src("999", &["Duke"]), army())];
        assert!(run_match(&ls, &games, &ts).matched.is_empty());
    }

    #[test]
    fn two_ids_named_alike_trust_neither_name() {
        let games = [game(1, "2026-11-02", 1, 2), game(2, "2026-11-03", 1, 3)];
        let ls = [
            listing("e1", "2026-11-02", src("a", &["Duke"]), army()),
            listing(
                "e2",
                "2026-11-03",
                src("b", &["Duke"]),
                src("c", &["Connecticut"]),
            ),
        ];
        assert!(run_match(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn agreeing_one_sided_listings_teach_the_other_team() {
        // UConn's name never resolves. Duke's and Army's games against it each
        // vote that ESPN 41 is our Connecticut; with two agreeing votes the
        // listings match, and so does UConn's game against App State.
        let uconn = || src("41", &["UConn Huskies", "UConn"]);
        let app = || src("2026", &["Appalachian State Mountaineers"]);
        let games = [
            game(1, "2026-11-02", 3, 1),
            game(2, "2026-11-04", 2, 3),
            game(3, "2026-11-06", 4, 3),
        ];
        let ls = [
            listing("e1", "2026-11-02", uconn(), duke()),
            listing("e2", "2026-11-04", army(), uconn()),
            listing("e3", "2026-11-06", app(), uconn()),
        ];
        let out = run_match(&ls, &games, &teams());
        let got: HashMap<usize, Uuid> = out.matched.iter().map(|&(g, i)| (i, g)).collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[&2], u(1003));
        assert!(out.flipped.is_empty(), "{:?}", out.flipped);
    }

    #[test]
    fn one_listing_teaches_nothing() {
        let games = [game(1, "2026-11-02", 3, 1)];
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("41", &["UConn Huskies"]),
            duke(),
        )];
        assert!(run_match(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn disagreeing_listings_teach_nothing() {
        // The Towson shape: ESPN 2352 is the opponent of a team whose one
        // game that day is against Towson (the wrong game), and of two teams
        // whose one game is against Loyola. Nothing is learned.
        let ts = vec![
            team(10, "Towson Tigers", "Towson"),
            team(11, "Morgan State Bears", "Morgan St."),
            team(12, "Loyola (MD) Greyhounds", "Loyola (MD)"),
            team(13, "Navy Midshipmen", "Navy"),
            team(14, "Army Black Knights", "Army"),
        ];
        let loyola = || src("2352", &["Loyola Maryland Greyhounds"]);
        let games = [
            game(1, "2026-11-14", 10, 11),
            game(2, "2026-11-16", 13, 12),
            game(3, "2026-11-18", 12, 14),
        ];
        let ls = [
            listing("e1", "2026-11-14", src("m", &["Morgan State"]), loyola()),
            listing("e2", "2026-11-16", src("n", &["Navy"]), loyola()),
            listing("e3", "2026-11-18", loyola(), src("a", &["Army"])),
        ];
        assert!(run_match(&ls, &games, &ts).matched.is_empty());
    }

    #[test]
    fn a_non_d1_opponent_needs_a_known_non_d1_side() {
        let games = [game_opt(1, "2026-11-02", Some(1), None)];
        let ls = [listing(
            "e1",
            "2026-11-02",
            duke(),
            src("3001", &["Wilson College"]),
        )];
        // Unknown: not matched.
        assert!(run_match(&ls, &games, &teams()).matched.is_empty());
        // Known non-D1 (CBBD lists it with no conference): matched.
        let div = Divisions {
            non_d1: HashSet::from(["3001".to_string()]),
            current: true,
        };
        let out = run_match_div(&ls, &games, &teams(), &div);
        assert_eq!(out.matched, vec![(u(1001), 0)]);
        assert!(out.flipped.is_empty());
        // Listed the other way round.
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("3001", &["Wilson"]),
            duke(),
        )];
        let out = run_match_div(&ls, &games, &teams(), &div);
        assert_eq!(out.flipped, HashSet::from([u(1001)]));
    }

    #[test]
    fn a_non_d1_side_never_matches_a_d1_game() {
        let games = [game(1, "2026-11-02", 1, 2)];
        let div = Divisions {
            non_d1: HashSet::from(["3001".to_string()]),
            current: true,
        };
        let l = listing("e1", "2026-11-02", duke(), src("3001", &["Wilson"]));
        let m = Matcher::new(std::slice::from_ref(&l), &games, &teams(), &div);
        assert_eq!(m.fit(&games[0], &l), Fit::Contradicted);
        assert!(m.refutes(&games[0], &l).is_some());
        assert!(
            m.match_listings(std::slice::from_ref(&l), &games)
                .matched
                .is_empty()
        );
        // For an earlier season CBBD's list is not that season's: no match,
        // but no proof either (St. Francis Brooklyn, D1 in 2019).
        let past = Divisions {
            current: false,
            ..div
        };
        let m = Matcher::new(std::slice::from_ref(&l), &games, &teams(), &past);
        assert_eq!(m.fit(&games[0], &l), Fit::Contradicted);
        assert_eq!(m.refutes(&games[0], &l), None);
    }

    #[test]
    fn a_small_score_gap_does_not_refute_a_stored_tie() {
        // Kennesaw State vs a non-D1 side ESPN's id does not place, 105-30
        // here and 105-29 at ESPN: one source a point off, not another game.
        let games = [scored(game_opt(1, "2025-11-03", Some(1), None), 105, 30)];
        let close = final_listing(
            listing("e1", "2025-11-03", duke(), src("x", &["Somebody"])),
            105,
            29,
        );
        let far = final_listing(
            listing("e1", "2025-11-03", duke(), src("x", &["Somebody"])),
            88,
            61,
        );
        let m = Matcher::new(
            &[close.clone(), far.clone()],
            &games,
            &teams(),
            &Divisions::default(),
        );
        assert_eq!(m.refutes(&games[0], &close), None);
        assert!(m.refutes(&games[0], &far).is_some());
    }

    #[test]
    fn a_game_with_no_team_of_ours_never_matches() {
        let games = [game_opt(1, "2026-11-02", None, None)];
        let div = Divisions {
            non_d1: HashSet::from(["3001".to_string(), "3002".to_string()]),
            current: true,
        };
        let ls = [listing(
            "e1",
            "2026-11-02",
            src("3001", &["Wilson"]),
            src("3002", &["Paine"]),
        )];
        assert!(
            run_match_div(&ls, &games, &teams(), &div)
                .matched
                .is_empty()
        );
    }

    #[test]
    fn final_scores_must_agree_in_either_order() {
        let games = [scored(game(1, "2026-11-02", 1, 2), 80, 70)];
        let agree = final_listing(listing("e1", "2026-11-02", army(), duke()), 70, 80);
        assert_eq!(run_match(&[agree], &games, &teams()).matched.len(), 1);
        let either = final_listing(listing("e1", "2026-11-02", army(), duke()), 80, 70);
        assert_eq!(run_match(&[either], &games, &teams()).matched.len(), 1);
        let differ = final_listing(listing("e1", "2026-11-02", army(), duke()), 61, 63);
        let out = run_match(std::slice::from_ref(&differ), &games, &teams());
        assert!(out.matched.is_empty());
        assert_eq!(out.score_mismatches, vec![(u(1001), 0)]);
        // A game of ours not yet final is not checked.
        let open = [game(1, "2026-11-02", 1, 2)];
        assert_eq!(run_match(&[differ], &open, &teams()).matched.len(), 1);
    }

    #[test]
    fn a_score_dispute_on_espn_ids_is_still_a_match() {
        // Florida State at Florida A&M, 2026-11-02: ours 69-112, ESPN 69-113.
        // Both teams settled by id: one source has the score wrong.
        let mut ts = teams();
        ts[0].espn_id = Some("150".into());
        ts[1].espn_id = Some("349".into());
        let games = [scored(game(1, "2026-11-02", 1, 2), 112, 69)];
        let l = final_listing(listing("e1", "2026-11-02", duke(), army()), 113, 69);
        let m = Matcher::new(std::slice::from_ref(&l), &games, &ts, &Divisions::default());
        let out = m.match_listings(std::slice::from_ref(&l), &games);
        assert_eq!(out.matched, vec![(u(1001), 0)]);
        assert_eq!(out.score_disputes, vec![(u(1001), 0)]);
        assert_eq!(m.refutes(&games[0], &l), None);
        // On names alone the score is the check, and it refuses.
        let m = Matcher::new(
            std::slice::from_ref(&l),
            &games,
            &teams(),
            &Divisions::default(),
        );
        assert!(
            m.match_listings(std::slice::from_ref(&l), &games)
                .matched
                .is_empty()
        );
        // ... but an existing link between the two named teams is kept.
        assert_eq!(m.refutes(&games[0], &l), None);
        // With a side unidentified, a clearly different score refutes it.
        let odd = final_listing(
            listing("e1", "2026-11-02", duke(), src("x", &["Nobody"])),
            90,
            69,
        );
        let m = Matcher::new(
            std::slice::from_ref(&odd),
            &games,
            &teams(),
            &Divisions::default(),
        );
        assert!(m.refutes(&games[0], &odd).is_some());
    }

    #[test]
    fn a_listing_two_games_fit_is_ambiguous() {
        // The primary feed lists some games twice.
        let games = [game(1, "2026-11-02", 1, 2), game(2, "2026-11-02", 2, 1)];
        let ls = [listing("e1", "2026-11-02", duke(), army())];
        let out = run_match(&ls, &games, &teams());
        assert!(out.matched.is_empty());
        assert_eq!(out.ambiguous, vec![0]);
    }

    #[test]
    fn a_game_claimed_twice_keeps_neither() {
        let games = [game(1, "2026-11-02", 1, 2)];
        let ls = [
            listing("e1", "2026-11-02", duke(), army()),
            listing("e2", "2026-11-02", army(), duke()),
        ];
        let out = run_match(&ls, &games, &teams());
        assert!(out.matched.is_empty());
        assert_eq!(out.conflicts, vec![u(1001)]);
    }

    #[test]
    fn a_different_day_never_matches() {
        let games = [game(1, "2026-11-03", 1, 2)];
        let ls = [listing("e1", "2026-11-02", duke(), army())];
        assert!(run_match(&ls, &games, &teams()).matched.is_empty());
    }

    #[test]
    fn a_midnight_listing_matches_the_next_days_game() {
        // Listed 11:59 PM Nov 1, dated Nov 2 by the primary feed.
        let games = [game(1, "2026-11-02", 1, 2)];
        let mut l = listing("e1", "2026-11-01", duke(), army());
        l.tip_time = NaiveTime::from_hms_opt(23, 59, 0);
        let out = run_match(&[l], &games, &teams());
        assert_eq!(out.matched, vec![(u(1001), 0)]);
    }

    #[test]
    fn a_boundary_listing_prefers_its_own_day() {
        let games = [game(1, "2026-11-01", 1, 2), game(2, "2026-11-02", 1, 2)];
        let mut l = listing("e1", "2026-11-01", duke(), army());
        l.tip_time = NaiveTime::from_hms_opt(23, 30, 0);
        let out = run_match(&[l], &games, &teams());
        assert_eq!(out.matched, vec![(u(1001), 0)]);
    }

    #[test]
    fn wrong_ties_name_each_kind() {
        // Game 1 matches e1 itself; its stored row says e2 (which matches
        // game 2) and its espn_id says e9 (not listed).
        let mut g1 = game(1, "2026-11-02", 1, 2);
        g1.stored_event = Some("e2".into());
        g1.espn_id = Some("e9".into());
        let mut g2 = game(2, "2026-11-02", 3, 4);
        g2.stored_event = Some("e2".into());
        // Game 3 is stored on e3, listed but unidentifiable: kept.
        let mut g3 = game(3, "2026-11-02", 5, 6);
        g3.stored_event = Some("e3".into());
        let games = [g1, g2, g3];
        let ls = [
            listing("e1", "2026-11-02", duke(), army()),
            listing(
                "e2",
                "2026-11-02",
                src("41", &["Connecticut"]),
                src("2026", &["Appalachian St."]),
            ),
            listing(
                "e3",
                "2026-11-02",
                src("x", &["San Jose St."]),
                src("y", &["?"]),
            ),
        ];
        let m = Matcher::new(&ls, &games, &teams(), &Divisions::default());
        let out = m.match_listings(&ls, &games);
        assert_eq!(out.matched.len(), 2);
        let mut ties: Vec<(Uuid, String)> = wrong_ties(&m, &ls, &games, &out)
            .into_iter()
            .map(|t| (t.game_id, t.event_id))
            .collect();
        ties.sort();
        assert_eq!(
            ties,
            vec![(u(1001), "e2".to_string()), (u(1001), "e9".to_string())]
        );
    }

    #[test]
    fn a_listing_carries_its_state_and_scores() {
        let board: Scoreboard = serde_json::from_value(serde_json::json!({
            "events": [{
                "id": "401",
                "competitions": [{
                    "date": "2026-11-03T02:00Z",
                    "timeValid": true,
                    "status": { "type": {
                        "state": "in", "completed": false, "shortDetail": "2:19 - 2nd"
                    }},
                    "competitors": [
                        { "homeAway": "home", "score": "94",
                          "team": { "id": "1", "displayName": "Duke Blue Devils", "location": "Duke" } },
                        { "homeAway": "away", "score": "52",
                          "team": { "id": "2", "displayName": "Army Black Knights", "location": "Army" } }
                    ]
                }]
            }]
        }))
        .unwrap();
        let l = &parse_scoreboard(d("2026-11-02"), board)[0];
        assert_eq!(l.status.state, "in");
        assert_eq!(l.status.detail, "2:19 - 2nd");
        assert_eq!(
            (l.status.home_score, l.status.away_score),
            (Some(94), Some(52))
        );
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
