//! Official school-published roster ingest.
//!
//! `players` is box-score-derived, so cstat's whole roster picture only ever
//! looks backwards: a row exists once somebody has played a game. That leaves
//! the preseason projection blind to four populations — redshirts staying at
//! the same school, D2/D3 up-transfers, JuCo arrivals, and direct international
//! signings — and they concentrate in exactly the teams the projection is
//! already weakest on. The schools publish all four, months before tipoff, on
//! their own athletics sites. This module reads them.
//!
//! # What it is for, and what it is deliberately not for
//!
//! Nothing here feeds the roster projection's scored roster. That restraint is
//! load-bearing rather than conservative: `train_roster_impact_model.py` builds
//! every training roster from `player_season_stats ... games_played >= 5`, so
//! the calibrator has never seen a roster carrying players with no `cam_v3`.
//! Handing it roster-confirmed-but-statless bodies is the same train/serve
//! mismatch that got the returner-redshirt exclusion built, measured and
//! reverted (raw MAE 6.13 → 6.20, bias +0.22 → +0.54, 91 team-seasons of
//! coverage lost). See `docs/roster_impact_retrain_plan.md`.
//!
//! What this data does instead is inform the two curated captures the
//! projection *already* reads — `player_departures` and `player_returns` — by
//! turning `departures-audit` from a worklist into a detector. See
//! [`crate::departures_audit`].
//!
//! # Absence is not a fact
//!
//! A player's presence on a fetched roster is a fact from a single row. A
//! player's absence is a claim about the whole page, and most pages cannot
//! support it:
//!
//! - Gonzaga published "2026-27 Men's Basketball Roster (Returners)" with four
//!   players on 2026-08-23. Diffing against that marks nine returners departed.
//! - Campbell and Navy were still serving 2025-26 rosters in late August.
//!
//! Both look identical to a clean fetch from the outside. So every fetch
//! records a [`FetchStatus`] alongside its players, and only [`FetchStatus::Ok`]
//! licenses an absence-based inference.
//!
//! # Platforms
//!
//! Three vendors and one fallback cover Division I, verified against all 364
//! cstat teams (301 usable, 2026-08-23):
//!
//! - **Sidearm nextgen** (143 teams) — an unauthenticated JSON API. Two calls:
//!   `/api/v2/Sports` to find the men's-basketball `sportId` (it is per-site —
//!   Duke 7, BU 3, LA Tech 5 — so it cannot be cached across schools), then
//!   `/api/v2/Rosters?sportId={id}`.
//! - **Sidearm legacy** (183 teams) — server-rendered HTML with stable vendor
//!   CSS classes (`sidearm-roster-player-*`).
//! - **WMT Digital** (~36 teams, skewed to majors — Purdue, Virginia, Oklahoma
//!   St., New Mexico) — server-rendered HTML, card and list DOMs.
//! - **Plain roster table** (~7) — not a vendor. The WordPress-based sites
//!   (Arkansas, Kentucky) share no markup with each other but all emit a real
//!   `<table>` with a labelled header row, so columns are mapped by header
//!   text. [`detect_html_platform`] falls through to this LAST, so a vendor
//!   page is never read through it by accident — wire any new vendor ahead of
//!   it, not after.

use anyhow::{Context, Result, anyhow};
use cstat_core::roster_projection::normalize_player_name;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::{debug, warn};
use uuid::Uuid;

/// Identify honestly. These are small public pages and we fetch each one once
/// per run; a browser UA would be a lie told for no gain.
const USER_AGENT: &str = "cstat-ingest/0.1 (+https://camalytics.org)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

/// Below this headcount a roster is recorded as [`FetchStatus::Partial`] rather
/// than `Ok`, so nothing downstream reads absence from it.
///
/// Nine is chosen from what actually goes wrong, not from a rulebook. A D-I
/// scholarship roster is 13-18; the failure this guards against is a school
/// publishing a placeholder mid-offseason, which lands far lower (Gonzaga
/// served four). Real-but-small rosters do exist in August — Grambling served
/// five, Morehead St. six — and marking those `Partial` is the correct
/// outcome, because a five-man page genuinely cannot tell you who left.
pub const MIN_TRUSTED_ROSTER: usize = 9;

/// Words that mean the page is showing a *subset* by construction.
///
/// Matched as whole words, case-insensitively, by [`title_subset_marker`] —
/// NOT as substrings. The HTML platforms hand us the full document `<title>`,
/// which carries the school's site branding and averages 63-78 characters
/// ("… - Official Athletics Website"), so a bare `contains("commit")` also
/// fires on "Commitment" and `contains("recruit")` on "Recruiting". That would
/// demote a complete roster to [`FetchStatus::Partial`], silently drop the team
/// out of the audit's trusted set, and give no hint why.
const PARTIAL_TITLE_MARKERS: &[&str] = &[
    "returner",
    "returners",
    "incoming",
    "newcomer",
    "newcomers",
    "signee",
    "signees",
    "commit",
    "commits",
    "recruit",
    "recruits",
];

/// The subset marker a roster title states, if any. Whole-word match so a
/// school's marketing tagline cannot disable its own absence detection.
fn title_subset_marker(title: &str) -> Option<&'static str> {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    PARTIAL_TITLE_MARKERS
        .iter()
        .copied()
        .find(|m| words.iter().any(|w| w == m))
}

/// Roster paths to try, in order, when probing an HTML platform. Sidearm and
/// WMT both key the sport off the URL and there is no discovery endpoint, so
/// the sport slug has to be guessed from the handful the vendors actually use.
const HTML_ROSTER_PATHS: &[&str] = &[
    "/sports/mens-basketball/roster",
    "/sports/mbball/roster",
    "/sports/mbkb/roster",
    "/sports/mbasketball/roster",
    "/sports/m-baskbl/roster",
    // WMT's WordPress sites use a singular "/sport/" segment (Arkansas).
    "/sport/m-baskbl/roster/",
];

/// What happened when we asked one school for one season's roster. Mirrors the
/// `team_roster_fetches.status` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FetchStatus {
    /// Full roster for the requested season, plausible size. The **only**
    /// status from which a player's absence may be read as a departure.
    Ok,
    /// Right season, but a subset — too few players, or the page says so
    /// itself. Players are stored and individually true; absence means nothing.
    Partial,
    /// The page served a different season. No players stored: they describe
    /// the wrong year, and storing them would silently resurrect last year's
    /// roster.
    StaleSeason,
    /// Reachable, but not a platform we can parse.
    Unsupported,
    /// DNS/TLS/HTTP failure, or no roster page found at any known path.
    Unreachable,
}

impl FetchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FetchStatus::Ok => "ok",
            FetchStatus::Partial => "partial",
            FetchStatus::StaleSeason => "stale_season",
            FetchStatus::Unsupported => "unsupported",
            FetchStatus::Unreachable => "unreachable",
        }
    }

    /// May a *missing* player on this fetch be read as evidence he is gone?
    /// The single question the whole status enum exists to answer.
    pub fn licenses_absence(self) -> bool {
        matches!(self, FetchStatus::Ok)
    }
}

/// One player as the school lists him. Every field except `name` is optional —
/// coverage varies by platform and by school, and a blank is not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterPlayer {
    pub name: String,
    pub jersey: Option<String>,
    /// Verbatim school label — "Fr.", "R-Jr.", "5th", "Gr.", "2nd Year".
    /// Never coerced into cstat's Fr/So/Jr/Sr vocabulary: the redshirt and
    /// fifth-year markers are precisely what that coercion would destroy, and
    /// they are the signal the 5-in-5 capture needs.
    pub class_year_raw: Option<String>,
    pub position: Option<String>,
    pub height_inches: Option<i32>,
    pub weight_lbs: Option<i32>,
    pub hometown: Option<String>,
    pub high_school: Option<String>,
    /// "Tyler Junior College", "Concordia-Irvine", "BC Zalgiris". The field
    /// that makes a D2/JuCo/international arrival identifiable as one.
    pub previous_school: Option<String>,
}

/// The result of one team-season fetch: the verdict, its provenance, and
/// whatever players were on the page.
#[derive(Debug, Clone)]
pub struct TeamRosterFetch {
    pub team_short_name: String,
    pub status: FetchStatus,
    pub source_url: Option<String>,
    pub platform: Option<&'static str>,
    pub roster_title: Option<String>,
    pub players: Vec<RosterPlayer>,
    pub note: Option<String>,
}

/// One entry of `data/team_sites.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct TeamSite {
    /// Bare athletics hostname, no scheme and no `www.`.
    pub host: String,
    /// Optional platform hint from the last verified probe. Saves a request
    /// when right; ignored and re-probed when it doesn't produce a roster, so
    /// a stale hint degrades to slow rather than to wrong.
    #[serde(default)]
    pub platform: Option<String>,
    /// Optional roster path hint for the HTML platforms.
    #[serde(default)]
    pub path: Option<String>,
}

/// `data/team_sites.json` — keyed by `teams.short_name`.
pub type TeamSites = BTreeMap<String, TeamSite>;

/// Load and validate the site map.
pub fn load_sites(path: &std::path::Path) -> Result<TeamSites> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading team-site map {}", path.display()))?;
    let sites: TeamSites = serde_json::from_str(&raw)
        .with_context(|| format!("parsing team-site map {}", path.display()))?;
    for (team, site) in &sites {
        if site.host.trim().is_empty() {
            return Err(anyhow!("{team}: empty host in {}", path.display()));
        }
        if site.host.contains("://") || site.host.contains('/') {
            return Err(anyhow!(
                "{team}: host must be a bare hostname, got {:?} in {}",
                site.host,
                path.display()
            ));
        }
    }
    Ok(sites)
}

// ---------------------------------------------------------------------------
// Season gating
// ---------------------------------------------------------------------------

/// What season a roster page claims to be, as stated by the page itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSeason {
    /// An explicit span — "2026-27", "2026-2027". Carries the cstat season
    /// (the later year), and is unambiguous.
    Span(i32),
    /// A single year — "2026 Men's Basketball Roster". Ambiguous by
    /// construction: schools use it for both the opening and closing calendar
    /// year of a winter season.
    Bare(i32),
    /// No year anywhere in the title.
    Unknown,
}

/// Read the season a roster title claims. Span form wins wherever both could
/// match, since it is the one that cannot be misread.
pub fn parse_title_season(title: &str) -> TitleSeason {
    let digits: Vec<char> = title.chars().collect();
    let mut bare: Option<i32> = None;
    let mut i = 0usize;
    while i + 4 <= digits.len() {
        let is_year = digits[i..i + 4].iter().all(|c| c.is_ascii_digit())
            && (i == 0 || !digits[i - 1].is_ascii_digit());
        if !is_year {
            i += 1;
            continue;
        }
        let start: i32 = digits[i..i + 4].iter().collect::<String>().parse().unwrap();
        if !(1900..=2200).contains(&start) {
            i += 4;
            continue;
        }
        // "2026-27" / "2026-2027" / "2026–27" (en dash) / "2026/27"
        let after = &digits[i + 4..];
        if let Some(sep) = after.first()
            && matches!(sep, '-' | '\u{2013}' | '/')
        {
            let tail: String = after[1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            match tail.len() {
                2 => {
                    // Century-carry: "1999-00" is 2000, not 1900.
                    let two: i32 = tail.parse().unwrap();
                    let end = start - start % 100 + two;
                    let end = if end < start { end + 100 } else { end };
                    return TitleSeason::Span(end);
                }
                4 => return TitleSeason::Span(tail.parse().unwrap()),
                _ => {}
            }
        }
        bare.get_or_insert(start);
        i += 4;
    }
    bare.map_or(TitleSeason::Unknown, TitleSeason::Bare)
}

/// How well a page's own title supports the season it is being read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeasonEvidence {
    /// The title states the target season as an unambiguous span.
    Confirmed,
    /// The title names a bare year consistent with the target. Real evidence,
    /// but it could denote either end of the span.
    Inferred(String),
    /// The title says nothing about any season. No evidence at all.
    Absent,
}

/// Does a page claiming `claimed` describe the roster for cstat season
/// `target`? Returns `Err(reason)` when it demonstrably does not.
///
/// A [`TitleSeason::Bare`] year is accepted for both `target` and `target - 1`,
/// because a bare year names either end of the span and both readings put it on
/// the right roster. That leniency has a known residual: a stale page using the
/// closing-year convention ("2026" for the 2025-26 team) is indistinguishable
/// from a current one using the opening-year convention, and slips through. It
/// is narrow — the HTML platforms overwhelmingly print spans, and the Sidearm
/// JSON API is gated on its structured `season.title` instead of this — and the
/// verbatim title is stored on every row so the case is auditable rather than
/// invisible.
///
/// [`SeasonEvidence::Absent`] is NOT treated as acceptance. Four schools
/// publish a roster titled only "Roster | Arkansas Razorbacks", and granting
/// those the same standing as a page that names its season would hand the
/// strongest verdict to the pages we know least about — see [`finish`].
pub fn season_gate(claimed: TitleSeason, target: i32) -> Result<SeasonEvidence, String> {
    match claimed {
        TitleSeason::Span(y) if y == target => Ok(SeasonEvidence::Confirmed),
        TitleSeason::Span(y) => Err(format!("page serves the {} season, not {target}", span(y))),
        TitleSeason::Bare(y) if y == target || y == target - 1 => Ok(SeasonEvidence::Inferred(
            format!("season inferred from a bare year ({y}) rather than a span"),
        )),
        TitleSeason::Bare(y) => Err(format!("page serves {y}, not {target}")),
        TitleSeason::Unknown => Ok(SeasonEvidence::Absent),
    }
}

/// Render a cstat season as the school's own span form, for messages.
fn span(season: i32) -> String {
    format!("{}-{:02}", season - 1, season % 100)
}

// ---------------------------------------------------------------------------
// Small field parsers
// ---------------------------------------------------------------------------

/// Height in inches from any of the forms the platforms print: `6-5`, `6'5"`,
/// `6′5″`, `6-5 ft`. Returns `None` for anything that doesn't yield a
/// plausible basketball height, so a stray "2026" can't become a height.
pub fn parse_height_inches(raw: &str) -> Option<i32> {
    let mut nums = raw
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<i32>().ok());
    let feet = nums.next()?;
    let inches = nums.next().unwrap_or(0);
    if !(3..=8).contains(&feet) || !(0..=11).contains(&inches) {
        return None;
    }
    Some(feet * 12 + inches)
}

/// Weight in pounds from `180 lbs` / `180` / `180.0`.
pub fn parse_weight_lbs(raw: &str) -> Option<i32> {
    let n: i32 = raw
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()?;
    (100..=400).contains(&n).then_some(n)
}

/// Strip a parenthesised or double-quoted nickname from a roster name.
///
/// Schools print these inline — Georgia lists `Marcus "Smurf" Millender` and
/// `Kemauri "Kemo" Millender` — and cstat carries the plain form, so leaving
/// them in makes the normalized keys disagree and reports a rostered player as
/// departed. That is not a hypothetical: it was the first false positive the
/// roster-backed audit produced.
///
/// Single quotes are deliberately **not** treated as nickname delimiters. They
/// are load-bearing inside real given names — Sei'Mir, O'Neal, Ja'Kobe — and
/// stripping between them would mangle far more names than it repaired.
pub fn strip_nickname(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0i32;
    let mut in_quote = false;
    for c in name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            '"' | '\u{201C}' | '\u{201D}' => in_quote = !in_quote,
            _ if depth == 0 && !in_quote => out.push(c),
            _ => {}
        }
    }
    out
}

/// Collapse runs of whitespace and trim; `None` for an empty result. Every
/// scraped field goes through this — the HTML platforms indent their markup,
/// so raw `.text()` is full of newlines and doubled spaces ("Trevaun  Clark").
fn clean(raw: &str) -> Option<String> {
    let s = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------
// Platform: Sidearm nextgen (JSON API)
// ---------------------------------------------------------------------------

pub const PLATFORM_SIDEARM_NEXTGEN: &str = "sidearm_nextgen";
pub const PLATFORM_SIDEARM_LEGACY: &str = "sidearm_legacy";
pub const PLATFORM_WMT: &str = "wmt";
/// WMT's table layout. Recognised so it can be reported, not parsed: it has
/// no hometown and no previous-school column.
pub const PLATFORM_WMT_TABLE: &str = "wmt_table";
/// Last-resort layout: a plain semantic `<table>` whose header row names the
/// columns. Not a vendor at all — it is what several WordPress-based athletics
/// sites (Arkansas, Kentucky) emit, and it is tried only after every
/// vendor-specific parser has declined, so a vendor page is never read through
/// it by accident.
pub const PLATFORM_ROSTER_TABLE: &str = "roster_table";

#[derive(Deserialize)]
struct NgSport {
    id: i64,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct NgRosterList {
    #[serde(default)]
    items: Vec<NgRoster>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NgRoster {
    #[serde(default)]
    display_title: Option<String>,
    #[serde(default)]
    season: Option<NgSeason>,
    #[serde(default)]
    players: Vec<NgPlayer>,
}

#[derive(Deserialize)]
struct NgSeason {
    /// "2026-27". The authoritative season for this platform — structured
    /// rather than scraped out of a display string, so the bare-year ambiguity
    /// documented on [`season_gate`] never arises here.
    #[serde(default)]
    title: Option<String>,
}

/// A Sidearm field that is a string on some schools and a number on others.
///
/// Not defensive programming for its own sake: `weight` alone is a JSON string
/// at Duke and a JSON integer at ~90 other schools, and a strict `Option<String>`
/// made every one of those teams fail the whole roster parse with
/// `invalid type: integer 220, expected a string`. The platform is one vendor
/// but the per-school data entry behind it is not, so every free-text-ish
/// numeric field is read through this.
#[derive(Deserialize)]
#[serde(untagged)]
enum Flex {
    Text(String),
    Int(i64),
    Float(f64),
}

impl Flex {
    fn text(&self) -> String {
        match self {
            Flex::Text(s) => s.clone(),
            Flex::Int(i) => i.to_string(),
            Flex::Float(f) => f.to_string(),
        }
    }

    fn int(&self) -> Option<i32> {
        match self {
            Flex::Int(i) => i32::try_from(*i).ok(),
            Flex::Float(f) => Some(*f as i32),
            Flex::Text(s) => s.trim().parse().ok(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NgPlayer {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    hometown: Option<String>,
    #[serde(default)]
    high_school: Option<String>,
    #[serde(default)]
    previous_school: Option<String>,
    #[serde(default)]
    position_short: Option<String>,
    #[serde(default)]
    academic_year_short: Option<String>,
    #[serde(default)]
    jersey_number: Option<Flex>,
    #[serde(default)]
    height_feet: Option<Flex>,
    #[serde(default)]
    height_inches: Option<Flex>,
    #[serde(default)]
    weight: Option<Flex>,
}

impl NgPlayer {
    fn into_row(self) -> Option<RosterPlayer> {
        let name = clean(&format!(
            "{} {}",
            self.first_name.unwrap_or_default(),
            self.last_name.unwrap_or_default()
        ))?;
        let feet = self.height_feet.as_ref().and_then(Flex::int);
        let inches = self.height_inches.as_ref().and_then(Flex::int);
        let height_inches = match (feet, inches) {
            (Some(f), i) if (3..=8).contains(&f) => Some(f * 12 + i.unwrap_or(0).clamp(0, 11)),
            _ => None,
        };
        Some(RosterPlayer {
            name,
            jersey: self
                .jersey_number
                .as_ref()
                .map(Flex::text)
                .as_deref()
                .and_then(clean),
            class_year_raw: self.academic_year_short.as_deref().and_then(clean),
            position: self.position_short.as_deref().and_then(clean),
            height_inches,
            weight_lbs: self
                .weight
                .as_ref()
                .map(Flex::text)
                .as_deref()
                .and_then(parse_weight_lbs),
            hometown: self.hometown.as_deref().and_then(clean),
            high_school: self.high_school.as_deref().and_then(clean),
            previous_school: self.previous_school.as_deref().and_then(clean),
        })
    }
}

// ---------------------------------------------------------------------------
// Platform: HTML (Sidearm legacy + WMT)
// ---------------------------------------------------------------------------

/// Does this string read as an eligibility label rather than, say, a major?
///
/// Guards the `custom1` fallback in [`parse_sidearm_legacy`]. That slot is
/// school-configurable — Lamar fills it with "Sr.-TR", others with a course of
/// study — so taking it unconditionally would file "Business Administration" as
/// a class year for every player at those schools.
///
/// Deliberately permissive about SUFFIXES ("Sr.-TR", "Fr.-HS") and prefixes
/// ("R-Jr.", "RS-Fr."), because those carry the redshirt and transfer markers
/// that are the whole reason the raw label is stored instead of a normalised
/// one.
fn looks_like_class_year(v: &str) -> bool {
    let lower = v.trim().to_lowercase();
    if lower.is_empty() || lower.len() > 24 {
        return false;
    }
    const WORDS: &[&str] = &[
        "fr",
        "so",
        "jr",
        "sr",
        "gr",
        "freshman",
        "sophomore",
        "junior",
        "senior",
        "graduate",
        "grad",
        "redshirt",
        "fifth",
        "sixth",
        "1st",
        "2nd",
        "3rd",
        "4th",
        "5th",
        "6th",
    ];
    // Split on the separators these labels use so "r-jr." and "sr.-tr" reduce
    // to their parts.
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|t| WORDS.contains(&t))
}

/// Is this container a STAFF card rather than a player card?
///
/// WMT renders the coaching staff on the same roster page, in cards that reuse
/// the player container class and add a staff modifier
/// (`roster-card-item roster-staff-members-card-item`). Missing this put Ryan
/// Odom and Malcolm Brogdon on Virginia's roster and left LSU's "roster" as 16
/// staff and 4 players — which not only pollutes the table but inflates the
/// headcount past [`MIN_TRUSTED_ROSTER`], so a nearly-empty player list earns an
/// `ok` verdict and starts licensing absence claims.
///
/// Matched on the raw class attribute rather than a `:not()` selector so it
/// covers every layout's spelling of the modifier without needing to enumerate
/// them.
fn is_staff_card(el: &scraper::ElementRef) -> bool {
    el.value()
        .attr("class")
        .is_some_and(|c| c.contains("staff") || c.contains("coach"))
}

/// Does this parsed row carry anything beyond a name?
///
/// The second staff guard, and the one that catches what [`is_staff_card`]
/// cannot. WMT's *list* layout renders coaches in containers that reuse the
/// player class with no staff modifier at all, so San Diego State's roster
/// arrived as 13 players and 13 staff — head coach included — with the staff
/// rows distinguishable only by being empty: no class year, no position, no
/// hometown, no jersey.
///
/// Requiring one substantive field is safe in both directions. A staff card in
/// these layouts has none. A player row that genuinely has none carries no
/// information except its own existence, and admitting it would assert a false
/// presence — which reads as "this senior is coming back" in the audit's
/// eligibility section and hides a real departure in the absence section.
fn has_player_fields(p: &RosterPlayer) -> bool {
    p.class_year_raw.is_some()
        || p.position.is_some()
        || p.hometown.is_some()
        || p.high_school.is_some()
        || p.previous_school.is_some()
        || p.jersey.is_some()
        || p.height_inches.is_some()
}

/// First matching descendant's text, skipping any subtree whose class contains
/// one of `exclude`.
///
/// Sidearm legacy's `-position` container is a grab-bag: it holds the position
/// (sometimes as loose text, sometimes nested inside a `text-bold` span) AND
/// the height, weight and jersey spans as siblings. A plain subtree read
/// returns "G 6'1\" 160 lbs"; an own-direct-text read returns nothing at all on
/// the sites that nest the position. Pruning the fields that have their own
/// selectors is the only reading that works for both shapes.
fn pick_excluding(el: &scraper::ElementRef, sel: &Selector, exclude: &[&str]) -> Option<String> {
    let root = el.select(sel).next()?;
    let mut out = String::new();
    for node in root.descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let inside_excluded = node
            .ancestors()
            .take_while(|a| a.id() != root.id())
            .any(|a| {
                a.value().as_element().is_some_and(|e| {
                    e.attr("class")
                        .is_some_and(|c| exclude.iter().any(|x| c.contains(x)))
                })
            });
        if !inside_excluded {
            out.push_str(text);
        }
    }
    clean(&out)
}

/// First matching descendant's cleaned text.
fn pick(el: &scraper::ElementRef, sel: &Selector) -> Option<String> {
    el.select(sel)
        .next()
        .and_then(|e| clean(&e.text().collect::<String>()))
}

/// Compile a selector known-good at author time.
fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

/// Parse a Sidearm **legacy** roster page.
///
/// Every field lives inside the `li.sidearm-roster-player` container, but each
/// appears *twice* — the platform renders a `hide-on-large` (mobile) and a
/// `hide-on-medium-down` (desktop) copy of the same block. Taking the first
/// match per container both de-duplicates and picks the abbreviated academic
/// year ("Fr.", "R-Jr.") over the desktop long form ("Freshman"), which is the
/// one that preserves the redshirt marker.
fn parse_sidearm_legacy(html: &str) -> Vec<RosterPlayer> {
    let doc = Html::parse_document(html);
    // No element prefix: the vendor emits this container as <li> on most
    // sites and <div> on others, and pinning the tag silently returns an empty
    // roster on the minority. The class token is unambiguous on its own —
    // `sidearm-roster-player-name` is a different token, not a match.
    let container = sel(".sidearm-roster-player");
    // The name block also contains the jersey number, so the name must come
    // from the anchor rather than the block's text.
    let name_link = sel(".sidearm-roster-player-name a");
    let first = sel(".sidearm-roster-player-first-name");
    let last = sel(".sidearm-roster-player-last-name");
    let jersey = sel(".sidearm-roster-player-jersey-number");
    let year = sel(".sidearm-roster-player-academic-year");
    // Schools that configure no academic-year field sometimes put the class in
    // the generic `custom1` slot instead (Lamar: "Sr.-TR"). That slot is
    // school-configurable and holds a major or a nickname elsewhere, so it is
    // only accepted when the value actually looks like a class year.
    let custom1 = sel(".sidearm-roster-player-custom1");
    let position = sel(".sidearm-roster-player-position-long-short");
    // Sites that never render the `-long-short` child put the position inside
    // `-position`, which ALSO wraps the height, weight and jersey spans — so a
    // plain subtree read yields "G 6'1\" 160 lbs". Those siblings are pruned.
    let position_plain = sel(".sidearm-roster-player-position");
    let height = sel(".sidearm-roster-player-height");
    let weight = sel(".sidearm-roster-player-weight");
    let hometown = sel(".sidearm-roster-player-hometown");
    let highschool = sel(".sidearm-roster-player-highschool");
    let previous = sel(".sidearm-roster-player-previous-school");

    let mut out = Vec::new();
    for el in doc.select(&container) {
        if is_staff_card(&el) {
            continue;
        }
        let name = pick(&el, &name_link).or_else(|| {
            let f = pick(&el, &first).unwrap_or_default();
            let l = pick(&el, &last).unwrap_or_default();
            clean(&format!("{f} {l}"))
        });
        let Some(name) = name else { continue };
        out.push(RosterPlayer {
            name,
            jersey: pick(&el, &jersey),
            class_year_raw: pick(&el, &year)
                .or_else(|| pick(&el, &custom1).filter(|v| looks_like_class_year(v))),
            position: pick(&el, &position).or_else(|| {
                pick_excluding(
                    &el,
                    &position_plain,
                    &[
                        "sidearm-roster-player-height",
                        "sidearm-roster-player-weight",
                        "sidearm-roster-player-jersey",
                    ],
                )
            }),
            height_inches: pick(&el, &height).as_deref().and_then(parse_height_inches),
            weight_lbs: pick(&el, &weight).as_deref().and_then(parse_weight_lbs),
            hometown: pick(&el, &hometown),
            high_school: pick(&el, &highschool),
            previous_school: pick(&el, &previous),
        });
    }
    out
}

/// Parse a **WMT Digital** roster page.
///
/// WMT ships four layouts and the same school can switch between them, so
/// every selector below is a group covering all of them rather than a guess:
///
/// | layout             | container            | example              |
/// |--------------------|----------------------|----------------------|
/// | card ("item" BEM)  | `.roster-card-item`  | Virginia, Stanford   |
/// | card (plain BEM)   | `.roster-card`       | Cincinnati, Missouri |
/// | list               | `.roster-list-item`  | San Diego St., UCLA  |
/// | table              | `.roster-table-cell` | Purdue               |
///
/// The table layout is deliberately NOT parsed — it publishes only jersey,
/// name, position, height and weight, dropping both hometown and previous
/// school, which are most of the reason to read these pages. It is detected in
/// [`detect_html_platform`] and reported as `unsupported` so it stays a visible
/// gap rather than an unexplained empty roster.
///
/// Height, weight and class arrive two different ways. The list layout labels
/// them with their own modifier classes; the card layout emits them as an
/// unlabeled ordered triple ("6′1″", "171 lbs", "2nd Year"). The labeled path
/// is tried first, and the triple is read positionally *by content* rather than
/// by index, so a school that omits one field does not shift the other two.
fn parse_wmt(html: &str) -> Vec<RosterPlayer> {
    let doc = Html::parse_document(html);
    // Tag-free for the same reason as the legacy container: the card layouts
    // use <div> and the list layout uses <li>.
    let container = sel(".roster-card-item, .roster-card, .roster-list-item");
    let title = sel(
        ".roster-card-item__title, .roster-card__title, .roster-list-item__title, \
         .roster-card-item__title-link, .roster-card__title-link, .roster-list-item__title-link",
    );
    let jersey = sel(
        ".roster-card-item__jersey-number, .roster-card__jersey-number, \
         .roster-list-item__jersey-number",
    );
    let position = sel(
        ".roster-card-item__position, .roster-card__position, .roster-list-item__position, \
         .roster-player-list-profile-field--position",
    );
    let f_height = sel(".roster-player-list-profile-field--height");
    let f_weight = sel(".roster-player-list-profile-field--weight");
    let f_class = sel(".roster-player-list-profile-field--class-level");
    let basic = sel(".roster-player-card-profile-field__value--basic");
    let hometown = sel(".roster-player-card-profile-field__value--hometown, \
         .roster-player-list-profile-field--hometown, \
         .roster-card__hometown");
    // Iowa's shape: explicit `__label` / `__value` pairs.
    let labelled = sel(".roster-card-profile-field");
    let field_label = sel(".roster-card-profile-field__label");
    let field_value = sel(".roster-card-profile-field__value");
    // Clemson's shape: an unlabelled ordered run of values.
    let info_item = sel(".roster-players-cards-item__info-item");
    let school = sel(".roster-player-card-profile-field__value--school, \
         .roster-player-list-profile-field--high-school");
    let previous = sel(
        ".roster-player-card-profile-field__value--previous-school, \
         .roster-player-list-profile-field--previous-school",
    );

    let mut out = Vec::new();
    for el in doc.select(&container) {
        if is_staff_card(&el) {
            continue;
        }
        let Some(name) = pick(&el, &title) else {
            continue;
        };
        let mut height = pick(&el, &f_height)
            .as_deref()
            .and_then(parse_height_inches);
        let mut weight = pick(&el, &f_weight).as_deref().and_then(parse_weight_lbs);
        let mut class_year = pick(&el, &f_class);
        let mut labelled_hometown: Option<String> = None;
        let mut labelled_school: Option<String> = None;
        let mut labelled_prev: Option<String> = None;

        // Iowa's shape: explicit `__label` / `__value` pairs, reusing
        // `assign_by_label` so the label vocabulary stays shared with the table
        // parser rather than growing a second word list to keep in sync.
        for field in el.select(&labelled) {
            let (Some(l), Some(fv)) = (pick(&field, &field_label), pick(&field, &field_value))
            else {
                continue;
            };
            let mut tmp = RosterPlayer::default();
            assign_by_label(&mut tmp, &l.to_lowercase(), fv);
            class_year = class_year.or(tmp.class_year_raw);
            height = height.or(tmp.height_inches);
            weight = weight.or(tmp.weight_lbs);
            labelled_hometown = labelled_hometown.or(tmp.hometown);
            labelled_school = labelled_school.or(tmp.high_school);
            labelled_prev = labelled_prev.or(tmp.previous_school);
        }

        // Clemson's shape: an unlabelled ORDERED run — height, weight, then
        // hometown, high school and class in that order, the middle two
        // optional (David Fuchs has a hometown and a class but no high school).
        // Height and weight are picked out by content; the remainder is
        // positional, anchored on class always coming last.
        if class_year.is_none() && labelled_hometown.is_none() {
            let mut rest: Vec<String> = Vec::new();
            for v in el
                .select(&info_item)
                .filter_map(|e| clean(&e.text().collect::<String>()))
            {
                if height.is_none() && v.contains(['-', '\'', '\u{2032}']) {
                    height = parse_height_inches(&v);
                } else if weight.is_none() && v.to_lowercase().contains("lb") {
                    weight = parse_weight_lbs(&v);
                } else {
                    rest.push(v);
                }
            }
            class_year = rest.pop();
            labelled_hometown = if rest.is_empty() {
                None
            } else {
                Some(rest.remove(0))
            };
            labelled_school = rest.pop();
        }

        // Card layout: classify the unlabeled triple by what each value looks
        // like. Whatever is neither a height nor a weight is the eligibility
        // label, which is the field we most need and the one with no stable
        // format at all ("2nd Year", "R-So.", "Grad").
        if height.is_none() && weight.is_none() && class_year.is_none() {
            for v in el
                .select(&basic)
                .filter_map(|e| clean(&e.text().collect::<String>()))
            {
                if height.is_none()
                    && v.contains(['-', '\'', '\u{2032}'])
                    && let Some(inches) = parse_height_inches(&v)
                {
                    height = Some(inches);
                } else if weight.is_none()
                    && let Some(lbs) = parse_weight_lbs(&v)
                {
                    weight = Some(lbs);
                } else if class_year.is_none() {
                    class_year = Some(v);
                }
            }
        }
        let row = RosterPlayer {
            name,
            jersey: pick(&el, &jersey).map(|j| j.trim_start_matches('#').to_string()),
            class_year_raw: class_year,
            position: pick(&el, &position),
            height_inches: height,
            weight_lbs: weight,
            hometown: pick(&el, &hometown).or(labelled_hometown),
            high_school: pick(&el, &school).or(labelled_school),
            previous_school: pick(&el, &previous).or(labelled_prev),
        };
        if has_player_fields(&row) {
            out.push(row);
        }
    }
    out
}

/// Parse any roster laid out as a semantic `<table>` with a labelled header row.
///
/// The fallback for sites that are not one of the three vendor DOMs — in
/// practice the WordPress-based athletics sites, whose markup differs per
/// school (Arkansas uses `rost_field_*` cell classes, Kentucky uses
/// `roster-item__*`) but which agree on emitting a real table with real
/// headers. Mapping columns by HEADER TEXT rather than by position or by class
/// means one parser covers both and survives a school reordering its columns,
/// which a positional read would silently scramble.
///
/// Runs last, so a vendor page is never read through it by accident.
fn parse_roster_table(html: &str) -> Vec<RosterPlayer> {
    let doc = Html::parse_document(html);
    let table = sel("table");
    let row = sel("tr");
    let head = sel("th");
    let cell = sel("td");

    for t in doc.select(&table) {
        let headers: Vec<String> = t
            .select(&head)
            .map(|h| {
                clean(&h.text().collect::<String>())
                    .unwrap_or_default()
                    .to_lowercase()
            })
            .collect();
        // A roster table names its players and locates them. Requiring both
        // rejects the schedule, stats and sponsor tables on the same page.
        let has_name = headers.iter().any(|h| h.contains("name"));
        let has_origin = headers
            .iter()
            .any(|h| h.contains("hometown") || h.contains("previous") || h.contains("high school"));
        if !has_name || !has_origin {
            continue;
        }

        let mut out = Vec::new();
        for r in t.select(&row) {
            let cells: Vec<String> = r
                .select(&cell)
                .map(|c| clean(&c.text().collect::<String>()).unwrap_or_default())
                .collect();
            if cells.len() < headers.len().min(3) {
                continue;
            }
            let mut p = RosterPlayer::default();
            for (h, v) in headers.iter().zip(cells.iter()) {
                if !v.is_empty() {
                    assign_by_label(&mut p, h, v.clone());
                }
            }
            if !p.name.is_empty() && has_player_fields(&p) {
                out.push(p);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

/// An origin column a roster table can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OriginField {
    Hometown,
    HighSchool,
    PreviousSchool,
}

/// Which origin columns a header names, in the order it names them.
///
/// Returns more than one for the combined headers these tables are full of:
/// "High School/Previous School" (Arkansas), "Hometown / High School" (Troy).
///
/// "Last School" counts as the previous school, not the high school. Georgia
/// Tech's column under that header holds San Jose State and Washington
/// alongside Lee-Scott Academy — it means "wherever you were before here", and
/// filing it as a high school is what made Georgia Tech contribute nothing to
/// the transfer-origin signal this ingest exists to capture.
fn origin_fields(header: &str) -> Vec<OriginField> {
    let mut out: Vec<(usize, OriginField)> = Vec::new();
    let mut push = |needle: &str, field: OriginField| {
        if let Some(at) = header.find(needle) {
            out.push((at, field));
        }
    };
    push("hometown", OriginField::Hometown);
    push("high school", OriginField::HighSchool);
    if header.contains("previous") {
        push("previous", OriginField::PreviousSchool);
    } else if header.contains("last school") {
        push("last school", OriginField::PreviousSchool);
    }
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, f)| f).collect()
}

/// Assign one labelled value onto a player row.
///
/// Shared by the table parser, which gets its labels from a `<th>` row, and by
/// WMT's label/value card DOM, which gets them from a `__label` span. Both
/// vocabularies are the same words in the same free-text soup, so keeping one
/// implementation means a header spelling fixed for one platform is fixed for
/// the other — the "YEAR"/"Height"/"Last School" cases were each found on one
/// platform and apply to both.
fn assign_by_label(p: &mut RosterPlayer, label: &str, v: String) {
    // Origin columns first, and as a GROUP. Schools combine them
    // in whatever pairing they like — Arkansas ships
    // "High School/Previous School", Troy ships
    // "Hometown / High School" — so the header is read for every
    // origin field it names and the cell is split across them in
    // order. Testing the single-field spellings first would file
    // Troy's whole "Waldorf, Md. / Bullis School" as a high school
    // and lose the hometown entirely.
    let origins = origin_fields(label);
    if !origins.is_empty() {
        for (field, part) in origins.iter().zip(split_origin(&v, origins.len())) {
            match field {
                OriginField::Hometown => p.hometown = part,
                OriginField::HighSchool => p.high_school = part,
                OriginField::PreviousSchool => p.previous_school = part,
            }
        }
    } else if label.contains("name") {
        p.name = v;
    } else if label.contains("pos") {
        p.position = Some(v);
    // WEIGHT BEFORE HEIGHT. "weight" contains the substring "ht",
    // so a `contains("ht")` test claims the weight column first,
    // fails to read "165 lbs." as a height, and — because the
    // assignment is unconditional — overwrites the height already
    // read from the real column with None. Kentucky, Miami,
    // Oklahoma St. and South Carolina lost BOTH measurements to
    // that; Arkansas escaped only because it abbreviates to
    // "Ht"/"Wt", and "height" does not contain "wt".
    //
    // The assignments are also now conditional, so no later column
    // can blank a field an earlier one filled.
    } else if label.contains("wt") || label.contains("weight") {
        if let Some(w) = parse_weight_lbs(&v) {
            p.weight_lbs = Some(w);
        }
    } else if label.contains("ht") || label.contains("height") {
        if let Some(inches) = parse_height_inches(&v) {
            p.height_inches = Some(inches);
        }
    // "year" does NOT contain "yr" — Georgia Tech's header is
    // "YEAR" and Troy's is "Year", and matching only "yr" silently
    // dropped the class for both. That is the field the eligibility
    // audit reads: Georgia Tech alone lists two players as "5th".
    } else if label.contains("yr")
        || label.contains("year")
        || label.contains("class")
        || label.contains("cl.")
    {
        p.class_year_raw = Some(v);
    } else if label.contains("num") || label.contains('#') || label.contains("no.") {
        p.jersey = Some(v);
    }
}

/// Split a combined origin cell into `n` parts.
///
/// Schools pack several origins into one column with no fixed separator:
/// Arkansas prints `Sunrise Christian Academy (Kan.) / Furman` but also
/// `The Skill Factory || Georgia` and, for a player with no college stop, a
/// bare `Little Rock Christian Academy`.
///
/// A cell with fewer parts than the header promises fills from the LEFT and
/// leaves the rest `None`. That is the whole point: with no separator the value
/// is only the first field, and inventing a previous school from it would
/// manufacture exactly the transfer signal this ingest measures.
fn split_origin(v: &str, n: usize) -> Vec<Option<String>> {
    // A header naming ONE field takes the whole cell. Splitting anyway
    // truncates a transfer chain at its first slash — "Emory & Henry/Seton
    // Hall" becomes "Emory & Henry", "Meridian CC/Miss. Valley St." loses the
    // destination. 244 players across the other platforms carry such chains in
    // exactly this field, and the table path was silently the only one that
    // could not.
    if n <= 1 {
        return vec![clean(v)];
    }
    let parts: Vec<&str> = if v.contains("||") {
        v.split("||").collect()
    } else {
        v.split('/').collect()
    };
    (0..n)
        .map(|i| parts.get(i).and_then(|p| clean(p)))
        .collect()
}

/// The season a page's own season PICKER says is selected/// The season a page's own season PICKER says is selected, when it names the
/// target season.
///
/// Fallback for the schools whose `<title>` names no season — Arkansas serves
/// "Roster | Arkansas Razorbacks" — but which do carry a season dropdown with
/// the current one selected:
///
/// ```html
/// <option value="…?season=2026-27" selected>2026-27</option>
/// <span class="selected-option__text">2026-27</span>
/// ```
///
/// Three deliberate restrictions, each from a way this misfired in testing:
///
/// 1. **Only the element's own text**, never `.text()` over its subtree.
///    Athletics pages routinely ship unclosed `<option selected>A<option>B`
///    markup — ramblinwreck.com serves exactly that on one of its pages — and
///    html5ever repairs it by NESTING the unclosed options. A subtree read then
///    picks up a later option's year, so a page stating no season at all can
///    confirm itself from an unrelated dropdown. Own-text is also simply the
///    correct reading of an `<option>`.
/// 2. **Only the selected control**, never the most frequent year on the page —
///    that would confirm a stale roster off a "2026-27 schedule" sidebar link,
///    the exact failure the gate exists for.
/// 3. **Only a match for `target`.** The picker can raise confidence, never
///    destroy a roster. Troy's page carries four `selected-option` widgets, one
///    of which is a "Jersey" sort control, so attribution is not certain enough
///    to justify discarding players — which is what a `stale_season` verdict
///    does. A picker that names some other season simply leaves the page
///    unconfirmed, and the title stays the only thing that can condemn it.
fn selected_season(html: &str, target: i32) -> Option<TitleSeason> {
    let doc = Html::parse_document(html);
    for selector in ["option[selected]", "[class*='selected-option']"] {
        let Ok(sel) = Selector::parse(selector) else {
            continue;
        };
        for el in doc.select(&sel) {
            // Direct text children only — see restriction 1.
            let own: String = el
                .children()
                .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
                .collect();
            let attr = el.value().attr("value").unwrap_or_default();
            for candidate in [own.as_str(), attr] {
                if parse_title_season(candidate) == TitleSeason::Span(target) {
                    return Some(TitleSeason::Span(target));
                }
            }
        }
    }
    None
}

/// Which vendor layout is this page, if any? Cheap substring probes on the
/// raw body: every one of these markers is a vendor CSS class that no other
/// platform emits.
fn detect_html_platform(body: &str) -> Option<&'static str> {
    if body.contains("sidearm-roster-player") {
        Some(PLATFORM_SIDEARM_LEGACY)
    } else if body.contains("roster-card-item")
        || body.contains("roster-card__")
        // Bare `roster-list-item` as well as the BEM-suffixed form: UTSA and
        // Washington St. emit the container class without ever using a
        // suffixed child class, so keying only on `__` missed them entirely.
        || body.contains("roster-list-item")
    {
        Some(PLATFORM_WMT)
    } else if body.contains("roster-table-cell") {
        Some(PLATFORM_WMT_TABLE)
    } else if body.contains("<table") {
        Some(PLATFORM_ROSTER_TABLE)
    } else {
        None
    }
}

/// Read a roster page's own title. Sidearm legacy states the season in the
/// document `<title>`; WMT does too ("Men's Basketball 2026-27 - Virginia …").
fn html_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    doc.select(&sel("title"))
        .next()
        .and_then(|e| clean(&e.text().collect::<String>()))
}

// ---------------------------------------------------------------------------
// Fetch client
// ---------------------------------------------------------------------------

pub struct RosterClient {
    http: Client,
}

impl RosterClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .user_agent(USER_AGENT)
                .timeout(REQUEST_TIMEOUT)
                .build()?,
        })
    }

    async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self.http.get(url).send().await?.error_for_status()?;
        Ok(resp.text().await?)
    }

    /// Sidearm nextgen. Two calls, because `sportId` is per-site (Duke 7, BU 3,
    /// LA Tech 5) and there is no cross-site constant to hard-code.
    async fn try_nextgen(&self, host: &str, target: i32) -> Result<Option<TeamRosterFetch>> {
        let sports_url = format!("https://{host}/api/v2/Sports");
        let body = self.get_text(&sports_url).await?;
        let sports: Vec<NgSport> = serde_json::from_str(&body)?;
        let Some(sport) = sports.into_iter().find(|s| {
            s.title
                .as_deref()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("men's basketball"))
        }) else {
            return Ok(None);
        };
        let url = format!("https://{host}/api/v2/Rosters?sportId={}", sport.id);
        let body = self.get_text(&url).await?;
        let list: NgRosterList = serde_json::from_str(&body)?;
        let Some(roster) = list.items.into_iter().next() else {
            return Ok(None);
        };
        // Prefer the structured season over the display title.
        let claimed = roster
            .season
            .as_ref()
            .and_then(|s| s.title.as_deref())
            .map(parse_title_season)
            .filter(|t| !matches!(t, TitleSeason::Unknown))
            .unwrap_or_else(|| {
                parse_title_season(roster.display_title.as_deref().unwrap_or_default())
            });
        let players = roster
            .players
            .into_iter()
            .filter_map(NgPlayer::into_row)
            .collect();
        Ok(Some(finish(
            claimed,
            target,
            roster.display_title,
            players,
            PLATFORM_SIDEARM_NEXTGEN,
            url,
        )))
    }

    /// The two server-rendered platforms, distinguished by the vendor markers
    /// their own CSS classes leave in the body.
    async fn try_html(
        &self,
        host: &str,
        target: i32,
        hint: Option<&str>,
    ) -> Result<Option<TeamRosterFetch>> {
        let mut paths: Vec<&str> = Vec::new();
        if let Some(h) = hint {
            paths.push(h);
        }
        paths.extend(
            HTML_ROSTER_PATHS
                .iter()
                .copied()
                .filter(|p| Some(*p) != hint),
        );
        for path in paths {
            let url = format!("https://{host}{path}");
            let Ok(body) = self.get_text(&url).await else {
                continue;
            };
            let title = html_title(&body);
            let (players, platform) = match detect_html_platform(&body) {
                Some(PLATFORM_SIDEARM_LEGACY) => {
                    (parse_sidearm_legacy(&body), PLATFORM_SIDEARM_LEGACY)
                }
                Some(PLATFORM_WMT) => (parse_wmt(&body), PLATFORM_WMT),
                Some(PLATFORM_ROSTER_TABLE) => (parse_roster_table(&body), PLATFORM_ROSTER_TABLE),
                Some(other) => {
                    // A layout we recognise but cannot read. Reported as its
                    // own status so it reads as a known gap in the platform
                    // coverage rather than as an unreachable school.
                    return Ok(Some(TeamRosterFetch {
                        team_short_name: String::new(),
                        status: FetchStatus::Unsupported,
                        source_url: Some(url),
                        platform: Some(other),
                        roster_title: title,
                        players: Vec::new(),
                        note: Some(format!(
                            "{other} layout carries no hometown or previous school"
                        )),
                    }));
                }
                None => continue,
            };
            if players.is_empty() {
                continue;
            }
            // Title first; fall back to the page's own season picker only when
            // the title says nothing, so a page that names a season is never
            // second-guessed by a control elsewhere on it.
            let mut claimed = parse_title_season(title.as_deref().unwrap_or_default());
            if matches!(claimed, TitleSeason::Unknown)
                && let Some(picked) = selected_season(&body, target)
            {
                claimed = picked;
            }
            return Ok(Some(finish(claimed, target, title, players, platform, url)));
        }
        Ok(None)
    }

    /// Fetch one team's roster for `target`. Never returns `Err`: a failure to
    /// reach a school is itself a recordable outcome, and one dead athletics
    /// site must not abort a 364-team sweep.
    pub async fn fetch_team(&self, team: &str, site: &TeamSite, target: i32) -> TeamRosterFetch {
        let host = site.host.trim();
        // Every platform that is NOT the JSON API. Listing them positively
        // rather than negating the one nextgen value is deliberate: when a new
        // HTML platform is added, forgetting it here silently reverts its teams
        // to probing `/api/v2/Sports` first, which 404s, wastes a request, and
        // prepends that 404 to the note on a fetch that then succeeds.
        let prefer_html = matches!(
            site.platform.as_deref(),
            Some(PLATFORM_SIDEARM_LEGACY)
                | Some(PLATFORM_WMT)
                | Some(PLATFORM_WMT_TABLE)
                | Some(PLATFORM_ROSTER_TABLE)
        );
        let mut notes: Vec<String> = Vec::new();

        // Ordered by the hint, but both are always tried: a hint that has gone
        // stale (a school replatformed) degrades to an extra request, never to
        // a wrong answer.
        let attempts: [bool; 2] = if prefer_html {
            [false, true]
        } else {
            [true, false]
        };
        for nextgen_first in attempts {
            let res = if nextgen_first {
                self.try_nextgen(host, target).await
            } else {
                self.try_html(host, target, site.path.as_deref()).await
            };
            match res {
                Ok(Some(mut f)) => {
                    f.team_short_name = team.to_string();
                    if !notes.is_empty() {
                        notes.extend(f.note.take());
                        f.note = Some(notes.join("; "));
                    }
                    return f;
                }
                Ok(None) => {}
                Err(e) => {
                    debug!(team, host, error = %e, "roster fetch attempt failed");
                    notes.push(shorten_err(&e));
                }
            }
        }
        TeamRosterFetch {
            team_short_name: team.to_string(),
            status: FetchStatus::Unreachable,
            source_url: Some(format!("https://{host}/")),
            platform: None,
            roster_title: None,
            players: Vec::new(),
            note: Some(if notes.is_empty() {
                "no roster page found at any known path".to_string()
            } else {
                notes.join("; ")
            }),
        }
    }
}

/// One-line error text for the `note` column — the full reqwest chain is long
/// and mostly boilerplate.
fn shorten_err(e: &anyhow::Error) -> String {
    let s = e.to_string();
    s.chars().take(160).collect()
}

/// Apply the season gate and the partial-roster rules to a parsed page.
///
/// Order matters. The season check comes first and **discards the players** on
/// failure: a page serving last season is not a small roster, it is the wrong
/// roster, and keeping its names would quietly reinstate players who left a
/// year ago. Only once the season is right do the subset rules apply, and those
/// keep their players — a four-man "(Returners)" page states four true facts.
fn finish(
    claimed: TitleSeason,
    target: i32,
    title: Option<String>,
    players: Vec<RosterPlayer>,
    platform: &'static str,
    url: String,
) -> TeamRosterFetch {
    let mut notes: Vec<String> = Vec::new();
    let base = TeamRosterFetch {
        team_short_name: String::new(),
        status: FetchStatus::Ok,
        source_url: Some(url),
        platform: Some(platform),
        roster_title: title.clone(),
        players: Vec::new(),
        note: None,
    };
    let mut season_unconfirmed = false;
    match season_gate(claimed, target) {
        Err(why) => {
            return TeamRosterFetch {
                status: FetchStatus::StaleSeason,
                note: Some(why),
                ..base
            };
        }
        Ok(SeasonEvidence::Inferred(caveat)) => notes.push(caveat),
        Ok(SeasonEvidence::Absent) => {
            season_unconfirmed = true;
            notes.push(
                "roster title names no season, so this page cannot be confirmed as the \
                 target one — players kept, absence not trusted"
                    .to_string(),
            );
        }
        Ok(SeasonEvidence::Confirmed) => {}
    }

    // De-duplicate on the stored key BEFORE the headcount gate, so
    // `player_count`, the `MIN_TRUSTED_ROSTER` decision and the rows actually
    // written all agree. `persist` keys players on the normalized name, and
    // two roster entries can share one — Missouri briefly listed "Jason Crowe
    // Jr." and "Jason Crowe Sr.", which normalize identically. Counting the
    // raw parse would report a 9-man roster as `ok` while storing 8.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut players = players;
    players.retain(|p| {
        let key = normalize_player_name(&strip_nickname(&p.name));
        if key.is_empty() || !seen.insert(key) {
            warn!(player = %p.name, "dropping duplicate roster entry");
            return false;
        }
        true
    });

    let marker = title_subset_marker(&title.unwrap_or_default());
    let status = if let Some(m) = marker {
        notes.push(format!(
            "roster title says {m:?} — this page is a subset by design"
        ));
        FetchStatus::Partial
    } else if players.len() < MIN_TRUSTED_ROSTER {
        notes.push(format!(
            "only {} player(s), below the {MIN_TRUSTED_ROSTER} needed to trust an absence",
            players.len()
        ));
        FetchStatus::Partial
    } else if season_unconfirmed {
        // `Ok` means "a full roster FOR THE REQUESTED SEASON", and that is
        // exactly the half we cannot assert here. Absence is the only thing
        // withheld: the players are still stored and still true, so the
        // eligibility section — which reads presence — is unaffected.
        FetchStatus::Partial
    } else {
        FetchStatus::Ok
    };
    TeamRosterFetch {
        status,
        players,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
        ..base
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Write one team-season fetch. Idempotent: the fetch row is upserted on
/// `(season, team_short_name)` and its players are replaced wholesale, so a
/// player who has since left the published roster disappears rather than
/// lingering from an earlier run.
pub async fn persist(pool: &PgPool, season: i32, fetch: &TeamRosterFetch) -> Result<()> {
    let mut tx = pool.begin().await?;
    let fetch_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO team_roster_fetches
            (season, team_short_name, status, source_url, platform,
             roster_title, player_count, note, fetched_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
        ON CONFLICT (season, team_short_name) DO UPDATE SET
            status       = EXCLUDED.status,
            source_url   = EXCLUDED.source_url,
            platform     = EXCLUDED.platform,
            roster_title = EXCLUDED.roster_title,
            player_count = EXCLUDED.player_count,
            note         = EXCLUDED.note,
            fetched_at   = now()
        RETURNING id
        "#,
    )
    .bind(season)
    .bind(&fetch.team_short_name)
    .bind(fetch.status.as_str())
    .bind(&fetch.source_url)
    .bind(fetch.platform)
    .bind(&fetch.roster_title)
    .bind(fetch.players.len() as i32)
    .bind(&fetch.note)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM team_roster_players WHERE season = $1 AND team_short_name = $2")
        .bind(season)
        .bind(&fetch.team_short_name)
        .execute(&mut *tx)
        .await?;

    // Two distinct humans can normalize to one key (accent folding plus the
    // Jr/III strip). The UNIQUE index would reject the second, failing the
    // whole team's write over a display-name collision, so collapse here and
    // say so rather than losing the roster.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in &fetch.players {
        let norm = normalize_player_name(&strip_nickname(&p.name));
        if norm.is_empty() || !seen.insert(norm.clone()) {
            warn!(
                team = %fetch.team_short_name,
                player = %p.name,
                "skipping roster row whose normalized name collides with an earlier one"
            );
            continue;
        }
        sqlx::query(
            r#"
            INSERT INTO team_roster_players
                (fetch_id, season, team_short_name, player_name, normalized_name,
                 jersey, class_year_raw, position, height_inches, weight_lbs,
                 hometown, high_school, previous_school)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
            "#,
        )
        .bind(fetch_id)
        .bind(season)
        .bind(&fetch.team_short_name)
        .bind(&p.name)
        .bind(&norm)
        .bind(&p.jersey)
        .bind(&p.class_year_raw)
        .bind(&p.position)
        .bind(p.height_inches)
        .bind(p.weight_lbs)
        .bind(&p.hometown)
        .bind(&p.high_school)
        .bind(&p.previous_school)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

/// Knobs for one `cstat-ingest rosters` run.
pub struct SweepOptions {
    /// Target cstat season (2027 = the 2026-27 rosters).
    pub season: i32,
    /// Restrict to these `teams.short_name` values; empty means every mapped team.
    pub only: Vec<String>,
    /// Hosts fetched at once. These are 364 different servers, so the cap is
    /// about our own egress and politeness, not any one site's load.
    pub concurrency: usize,
    /// Fetch and report without writing.
    pub dry_run: bool,
}

#[derive(Default)]
pub struct SweepReport {
    pub ok: usize,
    pub partial: usize,
    pub stale_season: usize,
    pub unsupported: usize,
    pub unreachable: usize,
    pub players: usize,
    /// Rows carrying a `previous_school`.
    ///
    /// Where a D2/JuCo/overseas arrival becomes identifiable — but not a
    /// transfer count. Schools whose column is headed "Last School" file a
    /// high school there for a true freshman, and that is the honest reading
    /// of their data rather than something to filter: Georgia Tech's column
    /// holds San Jose State and Lee-Scott Academy side by side.
    pub with_previous_school: usize,
    /// Teams whose verdict is not `ok`, with the reason, for the CLI summary.
    pub problems: Vec<(String, FetchStatus, String)>,
    /// Every team's verdict, for `--verbose`. Kept separate from `problems`
    /// so the default summary stays short.
    pub verdicts: Vec<(String, FetchStatus, usize)>,
}

impl SweepReport {
    fn record(&mut self, f: &TeamRosterFetch) {
        match f.status {
            FetchStatus::Ok => self.ok += 1,
            FetchStatus::Partial => self.partial += 1,
            FetchStatus::StaleSeason => self.stale_season += 1,
            FetchStatus::Unsupported => self.unsupported += 1,
            FetchStatus::Unreachable => self.unreachable += 1,
        }
        self.players += f.players.len();
        self.with_previous_school += f
            .players
            .iter()
            .filter(|p| p.previous_school.is_some())
            .count();
        self.verdicts
            .push((f.team_short_name.clone(), f.status, f.players.len()));
        if f.status != FetchStatus::Ok {
            self.problems.push((
                f.team_short_name.clone(),
                f.status,
                f.note.clone().unwrap_or_default(),
            ));
        }
    }
}

/// Reject a `--teams` name that matches no site-map key.
///
/// An unmatched name is an error, not a skip. Filtering alone would fetch the
/// names that did match, exit 0, and leave the operator believing the typo'd
/// one refreshed too — the same silent no-op `departures-audit` exists to catch
/// in the curated captures. Called by the CLI *before* it prints the run header
/// so the header can't claim a count it is about to fail, and again by
/// [`sweep`] so a non-CLI caller gets the same guarantee.
pub fn validate_teams(sites: &TeamSites, only: &[String]) -> Result<()> {
    let unknown: Vec<&str> = only
        .iter()
        .filter(|t| !sites.contains_key(*t))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "--teams named {} team(s) that are not in the site map: {}. \
         Keys are `teams.short_name` exactly (e.g. \"Miami FL\", \"N.C. State\").",
        unknown.len(),
        unknown.join(", ")
    ))
}

/// Fetch every mapped team's roster for `opts.season` and persist the results.
pub async fn sweep(pool: &PgPool, sites: &TeamSites, opts: &SweepOptions) -> Result<SweepReport> {
    let client = std::sync::Arc::new(RosterClient::new()?);
    validate_teams(sites, &opts.only)?;
    let targets: Vec<(String, TeamSite)> = sites
        .iter()
        .filter(|(team, _)| opts.only.is_empty() || opts.only.iter().any(|t| t == *team))
        .map(|(t, s)| (t.clone(), s.clone()))
        .collect();
    if targets.is_empty() {
        return Err(anyhow!(
            "no teams selected — check --teams against teams.short_name"
        ));
    }

    let mut report = SweepReport::default();
    let season = opts.season;
    for chunk in targets.chunks(opts.concurrency.max(1)) {
        let mut set = tokio::task::JoinSet::new();
        for (team, site) in chunk {
            let (client, team, site) = (client.clone(), team.clone(), site.clone());
            set.spawn(async move { client.fetch_team(&team, &site, season).await });
        }
        while let Some(joined) = set.join_next().await {
            let fetch = joined?;
            if !opts.dry_run {
                persist(pool, season, &fetch).await?;
            }
            report.record(&fetch);
        }
    }
    report.problems.sort();
    report.verdicts.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- season parsing / gating -------------------------------------

    #[test]
    fn span_titles_resolve_to_the_closing_year() {
        assert_eq!(
            parse_title_season("2026-27 Men's Basketball Roster"),
            TitleSeason::Span(2027)
        );
        assert_eq!(
            parse_title_season("Men's Basketball 2026-2027"),
            TitleSeason::Span(2027)
        );
        assert_eq!(
            parse_title_season("2026\u{2013}27 Roster"),
            TitleSeason::Span(2027)
        );
    }

    #[test]
    fn span_across_a_century_boundary_does_not_go_backwards() {
        // "1999-00" is 2000. Without the carry it reads as 1900 and every
        // historical page would look stale by a century.
        assert_eq!(
            parse_title_season("1999-00 Roster"),
            TitleSeason::Span(2000)
        );
    }

    #[test]
    fn a_lone_year_is_reported_as_ambiguous_not_as_a_span() {
        assert_eq!(
            parse_title_season("2026 Men's Basketball Roster"),
            TitleSeason::Bare(2026)
        );
        assert_eq!(
            parse_title_season("Men's Basketball Roster"),
            TitleSeason::Unknown
        );
    }

    #[test]
    fn last_seasons_page_is_rejected_rather_than_ingested() {
        // The Campbell/Navy failure: still serving 2025-26 in late August.
        // This must be an error, not a small-roster warning.
        assert!(season_gate(TitleSeason::Span(2026), 2027).is_err());
        assert_eq!(
            season_gate(TitleSeason::Span(2027), 2027),
            Ok(SeasonEvidence::Confirmed)
        );
    }

    #[test]
    fn a_bare_year_is_accepted_for_either_end_of_the_span_but_flagged() {
        // Lipscomb titles the 2026-27 roster "2026 Men's Basketball Roster".
        for y in [2026, 2027] {
            let ev = season_gate(TitleSeason::Bare(y), 2027).expect("accepted");
            assert!(
                matches!(ev, SeasonEvidence::Inferred(_)),
                "bare year {y} must be inferred, not confirmed"
            );
        }
        assert!(season_gate(TitleSeason::Bare(2025), 2027).is_err());
    }

    #[test]
    fn a_title_with_no_season_is_accepted_but_never_confirmed() {
        // Refusing outright would drop schools whose page simply says
        // "Roster"; accepting silently would let a stale one through unnoticed.
        assert_eq!(
            season_gate(TitleSeason::Unknown, 2027),
            Ok(SeasonEvidence::Absent)
        );
    }

    // --- field parsing -----------------------------------------------

    #[test]
    fn heights_parse_from_every_form_the_platforms_print() {
        for raw in ["6-5", "6'5\"", "6\u{2032}5\u{2033}", "6-5 ft"] {
            assert_eq!(parse_height_inches(raw), Some(77), "{raw}");
        }
        assert_eq!(parse_height_inches("7-1"), Some(85));
    }

    #[test]
    fn implausible_numbers_are_not_mistaken_for_heights_or_weights() {
        // A stray year or jersey number must not become a measurement.
        assert_eq!(parse_height_inches("2026"), None);
        assert_eq!(parse_height_inches(""), None);
        assert_eq!(parse_weight_lbs("2026"), None);
        assert_eq!(parse_weight_lbs("180 lbs"), Some(180));
    }

    #[test]
    fn nicknames_are_stripped_but_apostrophes_in_names_survive() {
        assert_eq!(
            strip_nickname("Marcus \"Smurf\" Millender")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "Marcus Millender"
        );
        assert_eq!(
            strip_nickname("Bob (BJ) Smith")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "Bob Smith"
        );
        // Sei'Mir and O'Neal must come through untouched — treating the
        // apostrophe as a nickname delimiter would mangle more names than it
        // repaired.
        assert_eq!(strip_nickname("Sei'Mir Roberson"), "Sei'Mir Roberson");
        assert_eq!(strip_nickname("Shaquille O'Neal"), "Shaquille O'Neal");
    }

    // --- HTML parsers -------------------------------------------------

    const LEGACY: &str = r#"
      <ul class="sidearm-roster-players">
        <li class="sidearm-roster-player">
          <span class="sidearm-roster-player-height">6'7"</span>
          <span class="sidearm-roster-player-weight">180 lbs</span>
          <div class="sidearm-roster-player-name">
            <span class="sidearm-roster-player-jersey-number">0</span>
            <h3><a href="/x">Trevaun  Clark</a></h3>
          </div>
          <div class="sidearm-roster-player-other hide-on-large">
            <span class="sidearm-roster-player-academic-year">Fr.</span>
            <span class="sidearm-roster-player-hometown">Lithuania</span>
            <span class="sidearm-roster-player-previous-school">BC Zalgiris</span>
          </div>
          <div class="sidearm-roster-player-other hide-on-medium-down">
            <span class="sidearm-roster-player-academic-year">Freshman</span>
            <span class="sidearm-roster-player-hometown">Lithuania</span>
            <span class="sidearm-roster-player-previous-school">BC Zalgiris</span>
          </div>
        </li>
      </ul>"#;

    #[test]
    fn legacy_reads_one_player_from_the_platforms_doubled_markup() {
        let rows = parse_sidearm_legacy(LEGACY);
        assert_eq!(
            rows.len(),
            1,
            "mobile + desktop copies must not double the roster"
        );
        let p = &rows[0];
        // The name block also contains the jersey, so a naive text grab yields
        // "0 Trevaun Clark".
        assert_eq!(p.name, "Trevaun Clark");
        assert_eq!(p.jersey.as_deref(), Some("0"));
        // First match wins, which is the abbreviated form — the one that keeps
        // an "R-Jr." legible instead of flattening it to "Redshirt Junior".
        assert_eq!(p.class_year_raw.as_deref(), Some("Fr."));
        assert_eq!(p.previous_school.as_deref(), Some("BC Zalgiris"));
        assert_eq!(p.height_inches, Some(79));
        assert_eq!(p.weight_lbs, Some(180));
    }

    #[test]
    fn legacy_reads_a_position_that_shares_its_container_with_height() {
        // Siena and 16 other legacy sites never render the `-long-short` child,
        // and their `-position` container also wraps the height and weight
        // spans — so a subtree read yields "G 6'1\" 160 lbs" and a selector
        // requiring `-long-short` yields nothing at all.
        let html = r#"
          <li class="sidearm-roster-player">
            <div class="sidearm-roster-player-position"><span class="text-bold">G</span>
              <span class="sidearm-roster-player-height">6'1"</span>
              <span class="sidearm-roster-player-weight">160 lbs</span>
            </div>
            <div class="sidearm-roster-player-name"><h3><a href="/x">Owen Schlager</a></h3></div>
          </li>"#;
        let rows = parse_sidearm_legacy(html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].position.as_deref(), Some("G"));
        assert_eq!(rows[0].height_inches, Some(73));
        assert_eq!(rows[0].weight_lbs, Some(160));
    }

    #[test]
    fn custom1_is_read_as_a_class_only_when_it_looks_like_one() {
        // Lamar configures no academic-year field and puts "Sr.-TR" in the
        // generic custom1 slot. Other schools put a major there, so taking it
        // unconditionally would file "Business Administration" as a class year.
        assert!(looks_like_class_year("Sr.-TR"));
        assert!(looks_like_class_year("R-Jr."));
        assert!(looks_like_class_year("RS-Fr."));
        assert!(looks_like_class_year("Fifth Year"));
        assert!(looks_like_class_year("5th"));
        assert!(!looks_like_class_year("Business Administration"));
        assert!(!looks_like_class_year("History, Technology and Society"));
        assert!(!looks_like_class_year(""));

        let html = r#"
          <li class="sidearm-roster-player">
            <span class="sidearm-roster-player-custom1">Sr.-TR</span>
            <div class="sidearm-roster-player-name"><h3><a href="/x">Caden Hinker</a></h3></div>
          </li>
          <li class="sidearm-roster-player">
            <span class="sidearm-roster-player-custom1">Business Administration</span>
            <div class="sidearm-roster-player-name"><h3><a href="/y">Other Guy</a></h3></div>
          </li>"#;
        let rows = parse_sidearm_legacy(html);
        assert_eq!(rows[0].class_year_raw.as_deref(), Some("Sr.-TR"));
        assert_eq!(rows[1].class_year_raw, None, "a major is not a class year");
    }

    const WMT_CARD: &str = r#"
      <div class="roster-card-item">
        <strong class="roster-card-item__jersey-number">#00</strong>
        <h3 class="roster-card-item__title"><a class="roster-card-item__title-link">Owen Odom</a></h3>
        <div class="roster-card-item__position">Guard</div>
        <span class="roster-player-card-profile-field__value roster-player-card-profile-field__value--basic">6&prime;1&Prime;</span>
        <span class="roster-player-card-profile-field__value roster-player-card-profile-field__value--basic">171 lbs</span>
        <span class="roster-player-card-profile-field__value roster-player-card-profile-field__value--basic">2nd Year</span>
        <span class="roster-player-card-profile-field__value roster-player-card-profile-field__value--hometown">Annapolis, Md.</span>
        <span class="roster-player-card-profile-field__value roster-player-card-profile-field__value--school">Collegiate School</span>
      </div>
      <div class="roster-card-item roster-staff-members-card-item">
        <h3 class="roster-card-item__title"><a class="roster-card-item__title-link">Ryan Odom</a></h3>
        <div class="roster-card-item__position">Head Coach</div>
      </div>"#;

    #[test]
    fn wmt_card_reads_the_unlabeled_triple_and_drops_the_coach() {
        let rows = parse_wmt(WMT_CARD);
        assert_eq!(rows.len(), 1, "the staff card must not become a player");
        let p = &rows[0];
        assert_eq!(p.name, "Owen Odom");
        assert_eq!(p.jersey.as_deref(), Some("00"));
        assert_eq!(p.height_inches, Some(73));
        assert_eq!(p.weight_lbs, Some(171));
        assert_eq!(p.class_year_raw.as_deref(), Some("2nd Year"));
        assert_eq!(p.hometown.as_deref(), Some("Annapolis, Md."));
    }

    const WMT_LIST: &str = r#"
      <li class="roster-list-item">
        <span class="roster-list-item__jersey-number">3</span>
        <h3 class="roster-list-item__title">Chance Gladden</h3>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--class-level">Sophomore</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--height">6&prime;4&Prime;</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--weight">185 lbs</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--position">Guard</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--hometown">Raleigh, N.C.</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--high-school">Ravenscroft High</span>
        <span class="roster-player-list-profile-field roster-player-list-profile-field--previous-school">Boston University</span>
      </li>
      <li class="roster-list-item">
        <h3 class="roster-list-item__title">Brian Dutcher</h3>
      </li>"#;

    #[test]
    fn wmt_list_reads_labeled_fields_and_drops_the_fieldless_coach() {
        // San Diego State's list layout gives its coaches no staff class at
        // all — the only thing separating them from players is that they carry
        // no roster fields.
        let rows = parse_wmt(WMT_LIST);
        assert_eq!(
            rows.len(),
            1,
            "a name-only container is staff, not a player"
        );
        let p = &rows[0];
        assert_eq!(p.name, "Chance Gladden");
        assert_eq!(p.class_year_raw.as_deref(), Some("Sophomore"));
        assert_eq!(p.height_inches, Some(76));
        assert_eq!(p.previous_school.as_deref(), Some("Boston University"));
    }

    const TABLE_COMBINED: &str = r#"
      <table><thead><tr>
        <th>Num</th><th>Name</th><th>Pos</th><th>Yr</th><th>Ht</th><th>Wt</th>
        <th>Hometown</th><th>High School/Previous School</th>
      </tr></thead><tbody>
        <tr><td>21</td><td>Cooper Bowser</td><td>F/C</td><td>Sr.</td><td>6-11</td><td>225</td>
            <td>Woodbridge, Va.</td><td>Sunrise Christian Academy (Kan.) / Furman</td></tr>
        <tr><td>7</td><td>JJ Andrews</td><td>W</td><td>Fr.</td><td>6-6</td><td>225</td>
            <td>Little Rock, Ark.</td><td>Little Rock Christian Academy</td></tr>
      </tbody></table>"#;

    #[test]
    fn table_parser_maps_columns_by_header_and_splits_the_combined_school() {
        let rows = parse_roster_table(TABLE_COMBINED);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Cooper Bowser");
        assert_eq!(rows[0].class_year_raw.as_deref(), Some("Sr."));
        assert_eq!(rows[0].height_inches, Some(83));
        assert_eq!(
            rows[0].high_school.as_deref(),
            Some("Sunrise Christian Academy (Kan.)")
        );
        assert_eq!(rows[0].previous_school.as_deref(), Some("Furman"));
        // No separator means high school and nothing else. Treating the whole
        // cell as a previous school would manufacture the transfer signal this
        // ingest exists to measure.
        assert_eq!(
            rows[1].high_school.as_deref(),
            Some("Little Rock Christian Academy")
        );
        assert_eq!(rows[1].previous_school, None);
    }

    #[test]
    fn table_parser_reads_year_and_last_school_headers() {
        // Georgia Tech's header row. "YEAR" does not contain "yr", and "LAST
        // SCHOOL" is a transfer origin, not a high school — matching only
        // "yr"/"high school" dropped the class for every player and filed San
        // Jose State as a high school, so the school contributed nothing to the
        // transfer signal.
        let html = r#"
          <table><thead><tr>
            <th>Number</th><th>Name</th><th>Position</th><th>HT.</th><th>WT.</th>
            <th>YEAR</th><th>HOMETOWN</th><th>LAST SCHOOL</th>
          </tr></thead><tbody>
            <tr><td>7</td><td>Jackson Fields</td><td>Forward</td><td>6-9</td><td>220</td>
                <td>5th</td><td>Missouri City, Texas</td><td>Troy</td></tr>
            <tr><td>0</td><td>Colby Garland</td><td>Guard</td><td>6-1</td><td>195</td>
                <td>Senior</td><td>Magnolia, Ark.</td><td>San Jose State</td></tr>
          </tbody></table>"#;
        let rows = parse_roster_table(html);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].class_year_raw.as_deref(), Some("5th"));
        assert_eq!(rows[1].class_year_raw.as_deref(), Some("Senior"));
        assert_eq!(rows[1].previous_school.as_deref(), Some("San Jose State"));
        assert_eq!(rows[1].high_school, None);
        assert_eq!(rows[1].hometown.as_deref(), Some("Magnolia, Ark."));
    }

    #[test]
    fn table_parser_splits_a_combined_hometown_and_high_school() {
        // Troy pairs the columns the other way round. Testing the single-field
        // spellings first filed the whole cell as a high school and lost the
        // hometown.
        let html = r#"
          <table><thead><tr>
            <th>#</th><th>Full Name</th><th>Pos.</th><th>Year</th>
            <th>Hometown / High School</th><th>Previous School</th>
          </tr></thead><tbody>
            <tr><td>1</td><td>Caden Diggs</td><td>G</td><td>Junior</td>
                <td>Waldorf, Md. / Bullis School</td><td>UMBC</td></tr>
            <tr><td>2</td><td>Afonso Pacheco</td><td>F</td><td>Sophomore</td>
                <td>Rio de Janeiro, Brazil</td><td>E.C. Pinheiros</td></tr>
          </tbody></table>"#;
        let rows = parse_roster_table(html);
        assert_eq!(rows[0].hometown.as_deref(), Some("Waldorf, Md."));
        assert_eq!(rows[0].high_school.as_deref(), Some("Bullis School"));
        assert_eq!(rows[0].previous_school.as_deref(), Some("UMBC"));
        assert_eq!(rows[0].class_year_raw.as_deref(), Some("Junior"));
        // No separator: the cell is only the hometown, and no high school is
        // invented from it.
        assert_eq!(rows[1].hometown.as_deref(), Some("Rio de Janeiro, Brazil"));
        assert_eq!(rows[1].high_school, None);
    }

    #[test]
    fn table_parser_reads_a_discrete_previous_school_column() {
        // Kentucky's shape: separate columns, and eligibility spelled out.
        let html = r#"
          <table><thead><tr>
            <th>Number</th><th>Name</th><th>Position</th><th>Height</th><th>Weight</th>
            <th>Class</th><th>Hometown</th><th>High school</th><th>Previous School</th>
          </tr></thead><tbody>
            <tr><td>3</td><td>Franck Kepnang</td><td>C</td><td>6-11</td><td>250</td>
                <td>Graduate Student</td><td>Yaound&eacute;, Cameroon</td><td>Ledyard</td>
                <td>Washington</td></tr>
          </tbody></table>"#;
        let rows = parse_roster_table(html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].class_year_raw.as_deref(), Some("Graduate Student"));
        assert_eq!(rows[0].previous_school.as_deref(), Some("Washington"));
        assert_eq!(rows[0].high_school.as_deref(), Some("Ledyard"));
    }

    #[test]
    fn a_weight_column_does_not_eat_the_height_column() {
        // "weight" contains the substring "ht". Kentucky's headers spell both
        // out, so the weight column matched the height branch, failed to read
        // "165 lbs." as a height, and blanked the height already read from the
        // real column. Both measurements were lost for four schools.
        let html = r#"
          <table><thead><tr>
            <th>Number</th><th>Name</th><th>Position</th><th>Height</th><th>Weight</th><th>Hometown</th>
          </tr></thead><tbody>
            <tr><td>0</td><td>Zyon Hawthorne</td><td>Guard</td><td>6-2</td><td>165 lbs.</td>
                <td>Louisville, Ky.</td></tr>
          </tbody></table>"#;
        let rows = parse_roster_table(html);
        assert_eq!(rows[0].height_inches, Some(74));
        assert_eq!(rows[0].weight_lbs, Some(165));
    }

    #[test]
    fn a_discrete_previous_school_column_keeps_the_whole_transfer_chain() {
        // Splitting a single-field cell truncates the chain at its first slash
        // and drops every school after the first — which is most of the
        // information in a multi-stop transfer.
        let html = r#"
          <table><thead><tr>
            <th>Name</th><th>Hometown</th><th>Previous School</th>
          </tr></thead><tbody>
            <tr><td>Mike James</td><td>Meridian, Miss.</td>
                <td>Meridian CC/Miss. Valley St.</td></tr>
          </tbody></table>"#;
        let rows = parse_roster_table(html);
        assert_eq!(
            rows[0].previous_school.as_deref(),
            Some("Meridian CC/Miss. Valley St.")
        );
        assert_eq!(rows[0].hometown.as_deref(), Some("Meridian, Miss."));
    }

    #[test]
    fn table_parser_ignores_tables_that_are_not_rosters() {
        // Every athletics page carries schedule and sponsor tables. A table
        // without both a name and an origin column is not a roster.
        let schedule = r#"<table><thead><tr><th>Date</th><th>Opponent</th><th>Result</th></tr>
          </thead><tbody><tr><td>Nov 4</td><td>Duke</td><td>W 80-70</td></tr></tbody></table>"#;
        assert!(parse_roster_table(schedule).is_empty());
    }

    // --- verdicts -----------------------------------------------------

    /// `n` distinct players.
    ///
    /// Names must be alphabetically distinct, not "Player 0"/"Player 1":
    /// `normalize_player_name` strips digits, so numbered names all collapse to
    /// the single key `player` and the de-duplication in `finish` would reduce
    /// any such roster to one man.
    fn dummy(n: usize) -> Vec<RosterPlayer> {
        (0..n)
            .map(|i| RosterPlayer {
                name: format!(
                    "Player {}",
                    [
                        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
                        "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa",
                        "Quebec", "Romeo", "Sierra", "Tango"
                    ][i % 20]
                ),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn subset_markers_match_whole_words_not_substrings() {
        // The HTML platforms hand over the full document title, branding and
        // all. "Recruiting" must not read as "recruit", or a complete roster is
        // demoted to Partial and the team silently leaves the audit's trusted
        // set.
        assert_eq!(
            title_subset_marker("2026-27 Men's Basketball Roster - Recruiting Questionnaire"),
            None
        );
        assert_eq!(
            title_subset_marker("Men's Basketball Roster - Commitment to Excellence"),
            None
        );
        // The real thing still trips it, with or without the parentheses.
        assert_eq!(
            title_subset_marker("2026-27 Men's Basketball Roster (Returners)"),
            Some("returners")
        );
        assert_eq!(
            title_subset_marker("2026-27 Incoming Class"),
            Some("incoming")
        );
        assert_eq!(title_subset_marker("2026-27 Men's Basketball Roster"), None);
    }

    #[test]
    fn a_full_current_roster_is_the_only_thing_that_licenses_absence() {
        let f = finish(
            TitleSeason::Span(2027),
            2027,
            Some("2026-27 Men's Basketball Roster".into()),
            dummy(15),
            PLATFORM_SIDEARM_NEXTGEN,
            "u".into(),
        );
        assert_eq!(f.status, FetchStatus::Ok);
        assert!(f.status.licenses_absence());
    }

    #[test]
    fn a_returners_only_page_keeps_its_players_but_forfeits_absence() {
        // The Gonzaga case: "2026-27 Men's Basketball Roster (Returners)" with
        // four names. Those four are true; the nine missing are not departures.
        let f = finish(
            TitleSeason::Span(2027),
            2027,
            Some("2026-27 Men's Basketball Roster (Returners)".into()),
            dummy(4),
            PLATFORM_SIDEARM_NEXTGEN,
            "u".into(),
        );
        assert_eq!(f.status, FetchStatus::Partial);
        assert!(!f.status.licenses_absence());
        assert_eq!(f.players.len(), 4, "presence is still a fact");
        assert!(f.note.unwrap().contains("returner"));
    }

    #[test]
    fn a_short_roster_forfeits_absence_even_with_an_innocent_title() {
        let f = finish(
            TitleSeason::Span(2027),
            2027,
            Some("2026-27 Men's Basketball Roster".into()),
            dummy(5),
            PLATFORM_SIDEARM_LEGACY,
            "u".into(),
        );
        assert_eq!(f.status, FetchStatus::Partial);
        assert_eq!(f.players.len(), 5);
    }

    #[test]
    fn duplicate_names_are_dropped_before_the_headcount_gate() {
        // `persist` keys on the normalized name, so two entries sharing one
        // store as a single row. Counting the raw parse would grade a roster
        // `ok` on a headcount it does not actually have.
        let mut players = dummy(8);
        players.push(RosterPlayer {
            name: "Player Alpha".into(),
            ..Default::default()
        });
        assert_eq!(players.len(), 9, "9 raw entries, one a duplicate");
        let f = finish(
            TitleSeason::Span(2027),
            2027,
            Some("2026-27 Men's Basketball Roster".into()),
            players,
            PLATFORM_SIDEARM_LEGACY,
            "u".into(),
        );
        assert_eq!(f.players.len(), 8, "the duplicate must not be counted");
        assert_eq!(
            f.status,
            FetchStatus::Partial,
            "8 real players is below the trust threshold, even though 9 parsed"
        );
    }

    #[test]
    fn a_season_picker_supplies_what_the_title_omits() {
        // Arkansas titles its page "Roster | Arkansas Razorbacks" but ships a
        // season dropdown with the current season selected.
        let html = r#"<html><body>
          <select>
            <option value="/roster/?season=2025-26">2025-26</option>
            <option value="/roster/?season=2026-27" selected>2026-27</option>
          </select></body></html>"#;
        assert_eq!(selected_season(html, 2027), Some(TitleSeason::Span(2027)));

        // Troy's widget shape.
        let widget = r#"<div class="selected-option__text">2026-27</div>"#;
        assert_eq!(selected_season(widget, 2027), Some(TitleSeason::Span(2027)));
    }

    #[test]
    fn a_picker_naming_another_season_confirms_nothing_and_destroys_nothing() {
        // The picker may only raise confidence. Letting it condemn would mean
        // discarding a roster on the say-so of a control we cannot always
        // attribute — Troy's page carries four such widgets, one of them a
        // jersey-sort. Unconfirmed (players kept, absence withheld) is the
        // right answer, and the title remains the only thing that can declare a
        // page stale.
        let html = r#"<select>
            <option value="/roster/?season=2025-26" selected>2025-26</option>
            <option value="/roster/?season=2026-27">2026-27</option>
          </select>
          <a href="/schedule/2026-27">2026-27 Schedule</a>"#;
        assert_eq!(selected_season(html, 2027), None);
    }

    #[test]
    fn unclosed_option_markup_cannot_confirm_by_accident() {
        // Unclosed options are common in this markup, and html5ever repairs
        // them by nesting, so reading the selected element's SUBTREE picks up a
        // later option's year and lets a page stating no season confirm itself
        // off an unrelated dropdown.
        let gt = r#"<select><option selected>All Types<option value="x">2026-27</select>"#;
        assert_eq!(selected_season(gt, 2027), None);
    }

    #[test]
    fn an_unselected_picker_confirms_nothing() {
        assert_eq!(selected_season("<option>2026-27</option>", 2027), None);
        assert_eq!(
            selected_season("<option selected>All Types</option>", 2027),
            None
        );
        // A bare year in a picker is no more decidable than one in a title.
        assert_eq!(
            selected_season("<option selected>2026</option>", 2027),
            None
        );
    }

    #[test]
    fn a_title_with_no_season_keeps_players_but_forfeits_absence() {
        // Four schools (Arkansas, Georgia Tech, Miami, Troy) publish a roster
        // titled only "Roster | <School>". Marking those `Ok` would hand the
        // strongest verdict — the one that licenses reading a departure from a
        // missing name — to the pages carrying the least evidence, and nothing
        // would surface it if one of them started serving last season.
        let f = finish(
            TitleSeason::Unknown,
            2027,
            Some("Roster | Arkansas Razorbacks".into()),
            dummy(15),
            PLATFORM_ROSTER_TABLE,
            "u".into(),
        );
        assert_eq!(f.status, FetchStatus::Partial);
        assert!(!f.status.licenses_absence());
        assert_eq!(f.players.len(), 15, "presence is still a fact");
        assert!(f.note.unwrap().contains("names no season"));
    }

    #[test]
    fn a_stale_page_discards_its_players_entirely() {
        // Unlike the partial cases, last season's names are not true facts
        // about this season — keeping them would reinstate players who left.
        let f = finish(
            TitleSeason::Span(2026),
            2027,
            Some("2025-26 Men's Basketball Roster".into()),
            dummy(15),
            PLATFORM_SIDEARM_LEGACY,
            "u".into(),
        );
        assert_eq!(f.status, FetchStatus::StaleSeason);
        assert!(f.players.is_empty());
    }

    // --- site map -----------------------------------------------------

    #[test]
    fn the_committed_site_map_parses_and_holds_bare_hostnames() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/team_sites.json");
        let sites = load_sites(&path).expect("data/team_sites.json must parse");
        assert!(
            sites.len() > 300,
            "expected the full D-I map, got {}",
            sites.len()
        );
        for (team, s) in &sites {
            assert!(
                !s.host.starts_with("http"),
                "{team}: {} carries a scheme",
                s.host
            );
            assert!(!s.host.contains('/'), "{team}: {} carries a path", s.host);
        }
        assert_eq!(sites["Duke"].host, "goduke.com");
    }
}
