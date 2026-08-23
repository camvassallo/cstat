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
pub fn season_gate(claimed: TitleSeason, target: i32) -> Result<Option<String>, String> {
    match claimed {
        TitleSeason::Span(y) if y == target => Ok(None),
        TitleSeason::Span(y) => Err(format!("page serves the {} season, not {target}", span(y))),
        TitleSeason::Bare(y) if y == target || y == target - 1 => Ok(Some(format!(
            "season inferred from a bare year ({y}) rather than a span"
        ))),
        TitleSeason::Bare(y) => Err(format!("page serves {y}, not {target}")),
        TitleSeason::Unknown => Ok(Some("roster title carries no season".to_string())),
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
    let position = sel(".sidearm-roster-player-position-long-short");
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
            class_year_raw: pick(&el, &year),
            position: pick(&el, &position),
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
         .roster-player-list-profile-field--hometown");
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
            hometown: pick(&el, &hometown),
            high_school: pick(&el, &school),
            previous_school: pick(&el, &previous),
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
                if v.is_empty() {
                    continue;
                }
                let v = v.clone();
                // Order matters: the combined "High School/Previous School"
                // column must be tested before either single-field spelling,
                // or it would be filed as a high school and the transfer
                // origin — the whole point of reading these pages — lost.
                if h.contains("high school") && h.contains("previous") {
                    let (hs, prev) = split_combined_school(&v);
                    p.high_school = hs;
                    p.previous_school = prev;
                } else if h.contains("previous") {
                    p.previous_school = Some(v);
                } else if h.contains("high school") || h.contains("last school") {
                    p.high_school = Some(v);
                } else if h.contains("hometown") {
                    p.hometown = Some(v);
                } else if h.contains("name") {
                    p.name = v;
                } else if h.contains("pos") {
                    p.position = Some(v);
                } else if h.contains("ht") || h.contains("height") {
                    p.height_inches = parse_height_inches(&v);
                } else if h.contains("wt") || h.contains("weight") {
                    p.weight_lbs = parse_weight_lbs(&v);
                } else if h.contains("yr") || h.contains("class") || h.contains("cl.") {
                    p.class_year_raw = Some(v);
                } else if h.contains("num") || h.contains('#') || h.contains("no.") {
                    p.jersey = Some(v);
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

/// Split a combined "High School / Previous School" cell.
///
/// Schools pack both into one column with no fixed separator: Arkansas prints
/// `Sunrise Christian Academy (Kan.) / Furman` but also `The Skill Factory ||
/// Georgia` and, for a player with no college stop, a bare
/// `Little Rock Christian Academy`. With no separator the value is a high
/// school and nothing else — inventing a previous school from it would
/// manufacture exactly the transfer signal this ingest exists to measure.
fn split_combined_school(v: &str) -> (Option<String>, Option<String>) {
    for sep in ["||", "/"] {
        if let Some((hs, prev)) = v.split_once(sep) {
            return (clean(hs), clean(prev));
        }
    }
    (clean(v), None)
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
            let claimed = parse_title_season(title.as_deref().unwrap_or_default());
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
    match season_gate(claimed, target) {
        Err(why) => {
            return TeamRosterFetch {
                status: FetchStatus::StaleSeason,
                note: Some(why),
                ..base
            };
        }
        Ok(Some(caveat)) => notes.push(caveat),
        Ok(None) => {}
    }

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
    /// Rows carrying a `previous_school` — the D2/JuCo/international signal.
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
        assert!(season_gate(TitleSeason::Span(2027), 2027).is_ok());
    }

    #[test]
    fn a_bare_year_is_accepted_for_either_end_of_the_span_but_flagged() {
        // Lipscomb titles the 2026-27 roster "2026 Men's Basketball Roster".
        for y in [2026, 2027] {
            let caveat = season_gate(TitleSeason::Bare(y), 2027).expect("accepted");
            assert!(caveat.is_some(), "bare year {y} must carry a caveat");
        }
        assert!(season_gate(TitleSeason::Bare(2025), 2027).is_err());
    }

    #[test]
    fn a_title_with_no_season_is_accepted_with_a_caveat() {
        // Refusing outright would drop schools whose page simply says
        // "Roster"; accepting silently would let a stale one through unnoticed.
        assert_eq!(
            season_gate(TitleSeason::Unknown, 2027),
            Ok(Some("roster title carries no season".to_string()))
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
    fn table_parser_ignores_tables_that_are_not_rosters() {
        // Every athletics page carries schedule and sponsor tables. A table
        // without both a name and an origin column is not a roster.
        let schedule = r#"<table><thead><tr><th>Date</th><th>Opponent</th><th>Result</th></tr>
          </thead><tbody><tr><td>Nov 4</td><td>Duke</td><td>W 80-70</td></tr></tbody></table>"#;
        assert!(parse_roster_table(schedule).is_empty());
    }

    // --- verdicts -----------------------------------------------------

    fn dummy(n: usize) -> Vec<RosterPlayer> {
        (0..n)
            .map(|i| RosterPlayer {
                name: format!("Player {i}"),
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
