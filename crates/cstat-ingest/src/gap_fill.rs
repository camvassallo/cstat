//! `gap-fill`: write CollegeBasketballData's box scores into
//! `player_game_stats` / `team_game_stats` for the games NatStat has no box
//! score for, and finish the games NatStat left stuck at an in-progress status
//! (`docs/natstat_migration_plan.md` §4.5 step 3); create the games and
//! players NatStat never listed (§4.5 step 5).
//!
//! Reads local tables only: the `cbbd_*` shadow tables (migration 075), joined
//! through `games.espn_id` and `players.espn_id` (migration 076). NatStat stays
//! primary wherever it has a game: a game with any box row is left alone, and
//! when NatStat's own box for a game side arrives later the perf ingest deletes
//! that side's CBBD rows first (`ingest::games::clear_cbbd_rows`).
//!
//! **What is written, per game final in CBBD:**
//!
//! - **The score.** Our game not final (no score, or a stuck status such as
//!   `H2 05:11`): its score and `status = 'Final'` from CBBD, with
//!   `games.score_source = 'cbbd'`. Never from a CBBD score with a 0 or NULL
//!   side (CBBD lists some games `final` at 0-0), and never over a score of
//!   ours that disagrees.
//! - **The box.** Only for a game with no `team_game_stats` and no
//!   `player_game_stats` row, and only when **every** CBBD player line on each
//!   Division I side resolves to one of our players and the lines' points add
//!   up to the side's score: a partial box is worse than none. Non-D1 sides get
//!   no rows, as with NatStat.
//! - **New players.** A line no player of ours matches becomes a new `players`
//!   row on that team (`espn_id` set, `natstat_id` the `espn:<id>` placeholder
//!   of `cstat_core::source_ids`) when it carries an ESPN id no player of ours
//!   holds this season, no roster player might be him under another spelling
//!   ([`may_be_on_roster`]), and the side is mostly players we already have
//!   ([`MIN_KNOWN_LINES`]), which is what stops a game carrying the wrong ESPN
//!   event from creating the other team's roster on ours.
//! - **New games.** A CBBD final on no game of ours, between two teams of ours
//!   by ESPN id, becomes a `games` row (`natstat_id` `espn:<event>`, `espn_id`
//!   the event, `score_source = 'cbbd'`) when nothing of ours may already be it
//!   ([`creation_blocker`]), and only together with its box.
//!
//! NatStat adopts either when it lists the same game or player later
//! (`ingest::games::adopt_cbbd_games` / `adopt_cbbd_player`) rather than
//! adding a second copy. A player adopts only on a matching squashed name, so
//! one NatStat spells differently past that (Xabi / Xabier) is still split
//! across two rows: a known gap (docs/natstat_migration_plan.md §4.5 step 5,
//! "Not done").
//!
//! **Sides.** CBBD's home and away do not always follow ours (or ESPN's), so
//! each game is oriented by the score when ours has one, and by which of our
//! teams the CBBD lines' players (resolved by ESPN id) belong to. The two must
//! agree; a game neither can orient is skipped.
//!
//! **Players.** By `players.espn_id` first. A line left over pairs by exact
//! normalized name to a player on that team's season roster who has no ESPN
//! id yet, when the name is unique on both sides. The last-name fallback
//! `name_match::pair_box_names` adds is deliberately not used: it is safe
//! between two box scores of the same game, where both sides list the same
//! people, but a season roster and one night's box are different sets, and a
//! walk-on missing from our roster would take a teammate's last name.
//!
//! **Team rows** are the sums of the CBBD player lines, never CBBD's team line,
//! which adds team rebounds, team turnovers and bench technicals:
//! `team_game_stats` is a sum of players and every rate built on it assumes so.

use crate::ingest::games::is_settled_status;
use crate::name_match::pair_unambiguous;
use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveTime};
use cstat_core::excluded_games::is_excluded;
use cstat_core::roster_projection::normalize_player_name;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use uuid::Uuid;

/// The value `source` carries on a row this module writes.
pub const CBBD_SOURCE: &str = "cbbd";

/// One CBBD box-score line's numbers.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct LineStats {
    pub starter: Option<bool>,
    pub minutes: Option<i32>,
    pub points: Option<i32>,
    pub fgm: Option<i32>,
    pub fga: Option<i32>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub off_rebounds: Option<i32>,
    pub def_rebounds: Option<i32>,
    pub total_rebounds: Option<i32>,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub turnovers: Option<i32>,
    pub fouls: Option<i32>,
}

/// One CBBD player line.
#[derive(Debug, Clone)]
pub struct Line {
    pub cbbd_is_home: bool,
    pub name: String,
    pub espn_id: Option<String>,
    pub stats: LineStats,
}

/// Our game, with its CBBD twin.
#[derive(Debug, Clone)]
pub struct GameIn {
    pub id: Uuid,
    pub natstat_id: Option<String>,
    pub label: String,
    pub home_team_id: Option<Uuid>,
    pub away_team_id: Option<Uuid>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub status: Option<String>,
    pub neutral: bool,
    /// Box rows already stored for the game, by source.
    pub box_sources: Vec<String>,
    pub cbbd_status: Option<String>,
    pub cbbd_home_points: Option<i32>,
    pub cbbd_away_points: Option<i32>,
}

/// A player of ours, as the resolution sees him.
#[derive(Debug, Clone)]
pub struct Player {
    pub id: Uuid,
    pub name: String,
    pub team_id: Option<Uuid>,
    pub espn_id: Option<String>,
}

/// Our players for one game: by ESPN id, and each team's season roster.
#[derive(Debug, Default)]
pub struct Players {
    pub by_espn: HashMap<String, Player>,
    pub rosters: HashMap<Uuid, Vec<Player>>,
}

/// A `team_game_stats` row to write.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamRow {
    pub team_id: Uuid,
    pub opponent_id: Option<Uuid>,
    pub is_home: bool,
    pub win: bool,
    pub stats: LineStats,
}

/// A `players` row to create for a CBBD line no player of ours matches.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPlayer {
    pub name: String,
    pub espn_id: String,
}

/// A `games` row to create for a CBBD game NatStat never listed.
#[derive(Debug, Clone, PartialEq)]
pub struct NewGame {
    pub espn_event_id: String,
    pub game_date: NaiveDate,
    pub tip_time: Option<NaiveTime>,
    pub is_conference: Option<bool>,
}

/// A `player_game_stats` row to write.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerRow {
    /// Our player; for a [`new_player`](Self::new_player), the id the run
    /// assigns it (nil until then).
    pub player_id: Uuid,
    pub team_id: Uuid,
    pub opponent_id: Option<Uuid>,
    pub is_home: bool,
    pub stats: LineStats,
    /// Resolved by name rather than ESPN id (for the report).
    pub by_name: bool,
    /// The line is a player we do not have: created with the box.
    pub new_player: Option<NewPlayer>,
}

/// What to write for one game.
#[derive(Debug, Default, PartialEq)]
pub struct GamePlan {
    /// Our final `(home, away)` score to set, with `status = 'Final'`.
    pub score: Option<(i32, i32)>,
    pub teams: Vec<TeamRow>,
    pub players: Vec<PlayerRow>,
    /// Why the score or the box was not written, when one was not.
    pub skip: Option<String>,
    /// The game already had box rows (NatStat's or an earlier fill's).
    pub already_boxed: bool,
    /// The game is not ours yet: created with its box, or not at all.
    pub new_game: Option<NewGame>,
}

impl GamePlan {
    fn skip(reason: impl Into<String>) -> Self {
        Self {
            skip: Some(reason.into()),
            ..Default::default()
        }
    }
}

/// Whether CBBD's home side is our away side, from the two scores. `None`
/// when ours has no score; `Err` when the scores disagree.
fn orient_by_score(g: &GameIn, ch: i32, ca: i32) -> Result<Option<bool>, String> {
    match (g.home_score, g.away_score) {
        (Some(h), Some(a)) if (h, a) == (ch, ca) => Ok(Some(false)),
        (Some(h), Some(a)) if (h, a) == (ca, ch) => Ok(Some(true)),
        (Some(h), Some(a)) => Err(format!("score disagrees: ours {h}-{a}, CBBD {ch}-{ca}")),
        _ => Ok(None),
    }
}

/// Whether CBBD's home side is our away side, from the teams of the players
/// the lines resolve to by ESPN id. `None` without evidence; `Err` when
/// players point both ways, or when any belongs to a team not in this game:
/// that is a game matched to the wrong ESPN event (a NatStat Towson-Morgan
/// State carried the event of Loyola Maryland-Morgan State), and writing its
/// score would be as wrong as writing its box.
fn orient_by_players(
    g: &GameIn,
    lines: &[Line],
    players: &Players,
) -> Result<Option<bool>, String> {
    let (mut straight, mut swapped, mut foreign) = (0u32, 0u32, 0u32);
    for l in lines {
        let Some(team) = l
            .espn_id
            .as_ref()
            .and_then(|e| players.by_espn.get(e))
            .and_then(|p| p.team_id)
        else {
            continue;
        };
        let (own, other) = if l.cbbd_is_home {
            (g.home_team_id, g.away_team_id)
        } else {
            (g.away_team_id, g.home_team_id)
        };
        if Some(team) == own {
            straight += 1;
        } else if Some(team) == other {
            swapped += 1;
        } else {
            foreign += 1;
        }
    }
    if foreign > 0 {
        return Err(format!(
            "{foreign} lines name players of a team not in this game (wrong ESPN event?)"
        ));
    }
    match (straight, swapped) {
        (0, 0) => Ok(None),
        (_, 0) => Ok(Some(false)),
        (0, _) => Ok(Some(true)),
        (s, w) => Err(format!(
            "players point both ways ({s} lines on our sides, {w} on the opposite ones)"
        )),
    }
}

/// Sum a field over lines: `None` if any line lacks it.
fn sum(lines: &[&Line], f: impl Fn(&LineStats) -> Option<i32>) -> Option<i32> {
    lines.iter().map(|l| f(&l.stats)).sum()
}

/// The team row's numbers: the sums of the players' lines.
pub fn team_totals(lines: &[&Line]) -> LineStats {
    LineStats {
        starter: None,
        minutes: sum(lines, |s| s.minutes),
        points: sum(lines, |s| s.points),
        fgm: sum(lines, |s| s.fgm),
        fga: sum(lines, |s| s.fga),
        tpm: sum(lines, |s| s.tpm),
        tpa: sum(lines, |s| s.tpa),
        ftm: sum(lines, |s| s.ftm),
        fta: sum(lines, |s| s.fta),
        off_rebounds: sum(lines, |s| s.off_rebounds),
        def_rebounds: sum(lines, |s| s.def_rebounds),
        total_rebounds: sum(lines, |s| s.total_rebounds),
        assists: sum(lines, |s| s.assists),
        steals: sum(lines, |s| s.steals),
        blocks: sum(lines, |s| s.blocks),
        turnovers: sum(lines, |s| s.turnovers),
        fouls: sum(lines, |s| s.fouls),
    }
}

/// How one CBBD line resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Resolved {
    Open,
    Refused(&'static str),
    /// Our player, and whether by name.
    Player(Uuid, bool),
    /// Not a player of ours: a new one is created.
    Create,
}

/// A side may add new players only when at least this many of its lines, and
/// at least half of them, are players we already have on that team. That is
/// the guard against a game carrying the wrong ESPN event whose players we
/// have never seen: Loyola Maryland's box on our Towson game would otherwise
/// create the whole Loyola roster as Towson players. A real side holds a
/// majority of known players from its first game of the season on.
pub const MIN_KNOWN_LINES: usize = 3;

/// Whether a line named `name`, which no player of ours matched, might still
/// be one of `roster` under a different spelling, which would make a new
/// player a duplicate in waiting. True when a roster player's name agrees once
/// squashed (`Jalen St Clair` / `Jalen StClair`), or, for a roster player with
/// no ESPN id yet (who could be this line's: `espn_ids` maps only the names it
/// can pair), when any part of the two names may be the same word
/// ([`name_parts_overlap`]). The first local run created Xabi Lopez beside
/// Xabier Lopez Rodriguez, Carlos Varela beside Carlos Varela Romero and
/// Daniel Perez-Rouzan beside Daniel Rouzan under a narrower rule, so this one
/// is deliberately wide: it also holds back a real newcomer who shares a first
/// name with an unmapped teammate, whose game then waits for NatStat or for
/// the teammate's ESPN id. A name that normalizes to nothing is never safe.
pub fn may_be_on_roster(name: &str, roster: &[Player]) -> bool {
    let norm = normalize_player_name(name);
    if norm.is_empty() {
        return true;
    }
    let squashed = norm.replace(' ', "");
    roster.iter().any(|p| {
        let pn = normalize_player_name(&p.name);
        if pn.replace(' ', "") == squashed {
            return true;
        }
        p.espn_id.is_none() && !pn.is_empty() && name_parts_overlap(&norm, &pn)
    })
}

/// Whether two normalized names share a part that may be one word: the same
/// part (`Lopez`), one a prefix of the other (`Xabi` / `Xabier`, `Josh` /
/// `Joshua`), or one ending in the other (`Perez-Rouzan` / `Rouzan`). Parts
/// shorter than three letters (initials, `JJ`) are not compared, nor are
/// prefixes and endings shorter than three.
fn name_parts_overlap(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<String> {
        s.split(' ')
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_string)
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    pa.iter().any(|x| {
        pb.iter().any(|y| {
            x == y
                || x.starts_with(y.as_str())
                || y.starts_with(x.as_str())
                || x.ends_with(y.as_str())
                || y.ends_with(x.as_str())
        })
    })
}

/// Decide what to write for one game. Pure: everything it needs is passed in.
pub fn plan_game(g: &GameIn, lines: &[Line], players: &Players) -> GamePlan {
    if g.natstat_id.as_deref().is_some_and(is_excluded) {
        return GamePlan::skip("on the exclusion list");
    }
    let ours_final =
        is_settled_status(g.status.as_deref()) && g.home_score.is_some() && g.away_score.is_some();
    if ours_final && !g.box_sources.is_empty() {
        // Nothing to write: comparing the two is `source-parity`'s job.
        return GamePlan {
            already_boxed: true,
            ..Default::default()
        };
    }
    let (Some(ch), Some(ca)) = (g.cbbd_home_points, g.cbbd_away_points) else {
        return GamePlan::skip("CBBD lists it final with no score");
    };
    if ch <= 0 || ca <= 0 {
        return GamePlan::skip(format!("CBBD lists it final at {ch}-{ca}"));
    }
    let by_score = match orient_by_score(g, ch, ca) {
        Ok(o) => o,
        Err(why) => return GamePlan::skip(why),
    };
    let by_players = match orient_by_players(g, lines, players) {
        Ok(o) => o,
        Err(why) => return GamePlan::skip(why),
    };
    let swapped = match (by_score, by_players) {
        (Some(a), Some(b)) if a != b => {
            return GamePlan::skip("the score and the players put the sides opposite ways");
        }
        (Some(a), _) | (None, Some(a)) => a,
        (None, None) => {
            return GamePlan::skip(
                "cannot tell which CBBD side is which (no score, no known player)",
            );
        }
    };
    let (home_pts, away_pts) = if swapped { (ca, ch) } else { (ch, ca) };

    let mut plan = GamePlan {
        score: (!ours_final).then_some((home_pts, away_pts)),
        ..Default::default()
    };
    if !g.box_sources.is_empty() {
        plan.already_boxed = true;
        return plan;
    }

    // Each Division I side: (our team, opponent, home?, CBBD is_home, points, opponent's points).
    let sides = [
        (
            g.home_team_id,
            g.away_team_id,
            true,
            !swapped,
            home_pts,
            away_pts,
        ),
        (
            g.away_team_id,
            g.home_team_id,
            false,
            swapped,
            away_pts,
            home_pts,
        ),
    ];
    if sides.iter().all(|s| s.0.is_none()) {
        plan.skip = Some("no Division I side".into());
        return plan;
    }
    let mut teams = Vec::new();
    let mut rows = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut claimed: HashSet<Uuid> = HashSet::new();
    let mut new_espn: HashSet<String> = HashSet::new();
    for (team, opp, home, cbbd_home, pts, opp_pts) in sides {
        let Some(team) = team else { continue };
        let side: Vec<&Line> = lines
            .iter()
            .filter(|l| l.cbbd_is_home == cbbd_home)
            .collect();
        if side.is_empty() {
            plan.skip = Some("CBBD has no player lines for a Division I side".into());
            return plan;
        }
        let is_home = home && !g.neutral;
        let mut resolved: Vec<Resolved> = vec![Resolved::Open; side.len()];
        for (i, l) in side.iter().enumerate() {
            if let Some(p) = l.espn_id.as_ref().and_then(|e| players.by_espn.get(e)) {
                resolved[i] = if p.team_id == Some(team) {
                    Resolved::Player(p.id, false)
                } else {
                    Resolved::Refused("ESPN id held by another team's player")
                };
            }
        }
        // Name fallback against the team's roster: players with no ESPN id
        // yet, exact normalized name, unique on both sides.
        let roster: Vec<&Player> = players
            .rosters
            .get(&team)
            .map(|r| r.iter().filter(|p| p.espn_id.is_none()).collect())
            .unwrap_or_default();
        let open: Vec<usize> = (0..side.len())
            .filter(|&i| resolved[i] == Resolved::Open)
            .collect();
        let open_names: Vec<String> = open
            .iter()
            .map(|&i| normalize_player_name(&side[i].name))
            .collect();
        let roster_names: Vec<String> = roster
            .iter()
            .map(|p| normalize_player_name(&p.name))
            .collect();
        for (a, b) in pair_unambiguous(&open_names, &roster_names, |a, _| !open_names[a].is_empty())
        {
            resolved[open[a]] = Resolved::Player(roster[b].id, true);
        }
        // A line still open, with an ESPN id no player of ours holds and a
        // name nobody on the roster might be, is a player we do not have.
        let full_roster: &[Player] = players.rosters.get(&team).map_or(&[], Vec::as_slice);
        for (l, r) in side.iter().zip(resolved.iter_mut()) {
            if *r == Resolved::Open
                && l.espn_id.is_some()
                && !may_be_on_roster(&l.name, full_roster)
            {
                *r = Resolved::Create;
            }
        }
        let known = resolved
            .iter()
            .filter(|r| matches!(r, Resolved::Player(..)))
            .count();
        if (known < MIN_KNOWN_LINES || known * 2 < side.len())
            && resolved.contains(&Resolved::Create)
        {
            for r in resolved.iter_mut().filter(|r| **r == Resolved::Create) {
                *r = Resolved::Refused("too few of the side's players are ours to add a new one");
            }
        }
        for (l, r) in side.iter().zip(&resolved) {
            match *r {
                Resolved::Create => {
                    // `Create` is only reached with an ESPN id.
                    let espn = l.espn_id.clone().unwrap_or_default();
                    if !new_espn.insert(espn.clone()) {
                        unresolved.push(format!("{} (a second line for one player)", l.name));
                        continue;
                    }
                    rows.push(PlayerRow {
                        player_id: Uuid::nil(),
                        team_id: team,
                        opponent_id: opp,
                        is_home,
                        stats: l.stats.clone(),
                        by_name: false,
                        new_player: Some(NewPlayer {
                            name: l.name.clone(),
                            espn_id: espn,
                        }),
                    });
                }
                Resolved::Open => unresolved.push(l.name.clone()),
                Resolved::Refused(why) => unresolved.push(format!("{} ({why})", l.name)),
                Resolved::Player(id, by_name) => {
                    if !claimed.insert(id) {
                        unresolved.push(format!("{} (a second line for one player)", l.name));
                        continue;
                    }
                    rows.push(PlayerRow {
                        player_id: id,
                        team_id: team,
                        opponent_id: opp,
                        is_home,
                        stats: l.stats.clone(),
                        by_name,
                        new_player: None,
                    });
                }
            }
        }
        let totals = team_totals(&side);
        if totals.points != Some(pts) {
            plan.skip = Some(format!(
                "player points add to {} on a side that scored {pts}",
                totals
                    .points
                    .map_or_else(|| "an incomplete sum".into(), |p| p.to_string())
            ));
            return plan;
        }
        teams.push(TeamRow {
            team_id: team,
            opponent_id: opp,
            is_home,
            win: pts > opp_pts,
            stats: totals,
        });
    }
    if !unresolved.is_empty() {
        unresolved.sort();
        plan.skip = Some(format!("unresolved players: {}", unresolved.join(", ")));
        return plan;
    }
    plan.teams = teams;
    plan.players = rows;
    plan
}

// ---------------------------------------------------------------- the run

#[derive(Debug, Default)]
pub struct GapFillReport {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub dry_run: bool,
    /// Our games in the window with exactly one CBBD game on their ESPN id.
    pub games_in_cbbd: usize,
    /// Of those, CBBD lists this many as final.
    pub cbbd_final: usize,
    pub games_filled: usize,
    pub team_rows: usize,
    pub player_rows: usize,
    /// Player rows resolved by name rather than ESPN id.
    pub players_by_name: usize,
    pub scores_set: usize,
    /// Games final in CBBD that already had box rows, by source.
    pub already_natstat: usize,
    pub already_cbbd: usize,
    /// Of the games filled, this many were created (NatStat never listed
    /// them), and this many players were created with their boxes.
    pub games_created: usize,
    pub players_created: usize,
    /// CBBD finals in the window on no game of ours, and how many of those
    /// could not be considered for creation because a side is not a team of
    /// ours by ESPN id (non-D1 opponents, mostly).
    pub cbbd_unmatched: usize,
    pub unmatched_unmapped: usize,
    /// Why no game was considered for creation at all, when none was.
    pub creation_off: Option<String>,
    /// `(game, reason)` for each final game whose box or score was not written.
    pub skipped: Vec<(String, String)>,
}

impl GapFillReport {
    /// Rows written (or that would be, on a dry run).
    pub fn written(&self) -> usize {
        self.team_rows
            + self.player_rows
            + self.scores_set
            + self.games_created
            + self.players_created
    }

    /// One line for the nightly ledger's notes.
    pub fn notes(&self) -> String {
        let mut s = format!(
            "{} games filled ({} team rows, {} player rows, {} by name), {} scores set, \
             {} games and {} players created, {} skipped, {} already boxed by NatStat, {} by CBBD",
            self.games_filled,
            self.team_rows,
            self.player_rows,
            self.players_by_name,
            self.scores_set,
            self.games_created,
            self.players_created,
            self.skipped.len(),
            self.already_natstat,
            self.already_cbbd
        );
        if let Some(why) = &self.creation_off {
            let _ = write!(s, "; no games created: {why}");
        }
        s
    }

    /// The report `gap-fill` prints.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let window = match (self.from, self.to) {
            (Some(f), Some(t)) => format!("{f}..{t}"),
            _ => String::new(),
        };
        let mode = if self.dry_run {
            " (dry run: nothing written)"
        } else {
            ""
        };
        let _ = writeln!(s, "gap fill {window}{mode}");
        let _ = writeln!(
            s,
            "  {} games matched to CBBD, {} final there",
            self.games_in_cbbd, self.cbbd_final
        );
        let _ = writeln!(
            s,
            "  games filled: {} ({} team rows, {} player rows, {} players resolved by name)",
            self.games_filled, self.team_rows, self.player_rows, self.players_by_name
        );
        let _ = writeln!(
            s,
            "  final scores/statuses set from CBBD: {}",
            self.scores_set
        );
        let _ = writeln!(
            s,
            "  already boxed: {} by NatStat, {} by an earlier fill",
            self.already_natstat, self.already_cbbd
        );
        let _ = writeln!(
            s,
            "  CBBD finals on no game of ours: {} ({} with a side that is not a team of ours)",
            self.cbbd_unmatched, self.unmatched_unmapped
        );
        let _ = writeln!(
            s,
            "  created: {} games, {} players{}",
            self.games_created,
            self.players_created,
            self.creation_off
                .as_deref()
                .map(|w| format!(" (no games considered: {w})"))
                .unwrap_or_default()
        );
        let _ = writeln!(s, "  skipped: {}", self.skipped.len());
        for (game, why) in &self.skipped {
            let _ = writeln!(s, "    {game}: {why}");
        }
        s
    }
}

#[derive(sqlx::FromRow)]
struct GameRow {
    id: Uuid,
    season: i32,
    natstat_id: Option<String>,
    label: String,
    home_team_id: Option<Uuid>,
    away_team_id: Option<Uuid>,
    home_score: Option<i32>,
    away_score: Option<i32>,
    status: Option<String>,
    is_neutral_site: bool,
    box_sources: Vec<String>,
    cbbd_game_id: i32,
    cbbd_status: Option<String>,
    cbbd_home_points: Option<i32>,
    cbbd_away_points: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct LineRow {
    cbbd_game_id: i32,
    is_home: bool,
    name: String,
    espn_athlete_id: Option<String>,
    #[sqlx(flatten)]
    stats: LineStats,
}

#[derive(sqlx::FromRow)]
struct PlayerDb {
    id: Uuid,
    season: i32,
    name: String,
    team_id: Option<Uuid>,
    espn_id: Option<String>,
}

/// Fill the games played on Eastern dates `from..=to`. `dry_run` plans and
/// reports without writing.
pub async fn run(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
    dry_run: bool,
) -> Result<GapFillReport> {
    let mut report = GapFillReport {
        from: Some(from),
        to: Some(to),
        dry_run,
        ..Default::default()
    };
    // A game whose ESPN event matches two CBBD games is left out: which box is
    // its own cannot be told (the rule `map-espn-ids` follows).
    let games: Vec<GameRow> = sqlx::query_as(
        "SELECT g.id, g.season, g.natstat_id, \
                g.game_date::text || ' ' || coalesce(ht.name, '(non-D1)') || ' vs ' \
                  || coalesce(at.name, '(non-D1)') AS label, \
                g.home_team_id, g.away_team_id, g.home_score, g.away_score, g.status, \
                g.is_neutral_site, \
                ARRAY(SELECT source FROM team_game_stats WHERE game_id = g.id \
                      UNION SELECT source FROM player_game_stats WHERE game_id = g.id) \
                  AS box_sources, \
                c.cbbd_game_id, c.status AS cbbd_status, \
                c.home_points AS cbbd_home_points, c.away_points AS cbbd_away_points \
         FROM games g \
         JOIN cbbd_games c ON c.espn_event_id = g.espn_id \
         LEFT JOIN teams ht ON ht.id = g.home_team_id \
         LEFT JOIN teams at ON at.id = g.away_team_id \
         WHERE g.game_date BETWEEN $1 AND $2 AND g.espn_id IS NOT NULL \
           AND (SELECT count(*) FROM cbbd_games c2 WHERE c2.espn_event_id = g.espn_id) = 1 \
         ORDER BY g.game_date, label",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("loading the window's games")?;
    report.games_in_cbbd = games.len();
    let mut games: Vec<GameRow> = games
        .into_iter()
        .filter(|g| g.cbbd_status.as_deref() == Some("final"))
        .collect();
    report.cbbd_final = games.len();

    // CBBD's finals on no game of ours: the ones safe to create join the
    // games planned, as games whose score is CBBD's in CBBD's orientation.
    let mut new_games: HashMap<Uuid, NewGame> = HashMap::new();
    for (row, new) in creatable_games(pool, from, to, &mut report).await? {
        new_games.insert(row.id, new);
        games.push(row);
    }
    if games.is_empty() {
        return Ok(report);
    }

    let cbbd_ids: Vec<i32> = games.iter().map(|g| g.cbbd_game_id).collect();
    let lines: Vec<LineRow> = sqlx::query_as(
        "SELECT cbbd_game_id, is_home, name, espn_athlete_id, starter, minutes, points, fgm, \
                fga, tpm, tpa, ftm, fta, off_rebounds, def_rebounds, total_rebounds, assists, \
                steals, blocks, turnovers, fouls \
         FROM cbbd_player_game_stats WHERE cbbd_game_id = ANY($1)",
    )
    .bind(&cbbd_ids)
    .fetch_all(pool)
    .await
    .context("loading the CBBD shadow box rows")?;
    let mut lines_by_game: HashMap<i32, Vec<Line>> = HashMap::new();
    for l in lines {
        lines_by_game.entry(l.cbbd_game_id).or_default().push(Line {
            cbbd_is_home: l.is_home,
            name: l.name,
            espn_id: l.espn_athlete_id.filter(|s| !s.is_empty()),
            stats: l.stats,
        });
    }

    // Our players: every one an ESPN id in the window names, and the season
    // rosters of every team in it.
    let seasons: Vec<i32> = games
        .iter()
        .map(|g| g.season)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let espn_ids: Vec<&str> = lines_by_game
        .values()
        .flatten()
        .filter_map(|l| l.espn_id.as_deref())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let team_ids: Vec<Uuid> = games
        .iter()
        .flat_map(|g| [g.home_team_id, g.away_team_id])
        .flatten()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let ours: Vec<PlayerDb> = sqlx::query_as(
        "SELECT id, season, name, team_id, espn_id FROM players \
         WHERE (season = ANY($1) AND espn_id = ANY($2)) OR team_id = ANY($3)",
    )
    .bind(&seasons)
    .bind(&espn_ids)
    .bind(&team_ids)
    .fetch_all(pool)
    .await
    .context("loading our players")?;
    let mut by_season: HashMap<i32, Players> = HashMap::new();
    for p in ours {
        let entry = by_season.entry(p.season).or_default();
        let player = Player {
            id: p.id,
            name: p.name,
            team_id: p.team_id,
            espn_id: p.espn_id.clone(),
        };
        if let Some(e) = p.espn_id {
            entry.by_espn.insert(e, player.clone());
        }
        if let Some(t) = player.team_id {
            entry.rosters.entry(t).or_default().push(player);
        }
    }

    let empty = Players::default();
    let mut writes: Vec<(&GameRow, GamePlan)> = Vec::new();
    for g in &games {
        let input = GameIn {
            id: g.id,
            natstat_id: g.natstat_id.clone(),
            label: g.label.clone(),
            home_team_id: g.home_team_id,
            away_team_id: g.away_team_id,
            home_score: g.home_score,
            away_score: g.away_score,
            status: g.status.clone(),
            neutral: g.is_neutral_site,
            box_sources: g.box_sources.clone(),
            cbbd_status: g.cbbd_status.clone(),
            cbbd_home_points: g.cbbd_home_points,
            cbbd_away_points: g.cbbd_away_points,
        };
        let lines = lines_by_game
            .get(&g.cbbd_game_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut plan = plan_game(&input, lines, by_season.get(&g.season).unwrap_or(&empty));
        plan.new_game = new_games.get(&g.id).cloned();
        if plan.already_boxed {
            if g.box_sources.iter().any(|s| s == CBBD_SOURCE) {
                report.already_cbbd += 1;
            } else {
                report.already_natstat += 1;
            }
        }
        if let Some(why) = &plan.skip {
            report.skipped.push((input.label.clone(), why.clone()));
        }
        if plan.score.is_some() || !plan.players.is_empty() {
            writes.push((g, plan));
        }
    }
    for (label, why) in assign_new_player_ids(&mut writes) {
        report.skipped.push((label, why));
    }

    let filled: Vec<&(&GameRow, GamePlan)> = writes
        .iter()
        .filter(|(_, p)| !p.players.is_empty())
        .collect();
    report.games_filled = filled.len();
    report.games_created = filled.iter().filter(|(_, p)| p.new_game.is_some()).count();
    report.players_created = filled
        .iter()
        .flat_map(|(_, p)| &p.players)
        .filter(|r| r.new_player.is_some())
        .map(|r| r.player_id)
        .collect::<HashSet<_>>()
        .len();
    report.team_rows = filled.iter().map(|(_, p)| p.teams.len()).sum();
    report.player_rows = filled.iter().map(|(_, p)| p.players.len()).sum();
    report.players_by_name = filled
        .iter()
        .map(|(_, p)| p.players.iter().filter(|r| r.by_name).count())
        .sum();
    report.scores_set = writes.iter().filter(|(_, p)| p.score.is_some()).count();
    if dry_run || writes.is_empty() {
        return Ok(report);
    }
    let done = write(pool, &writes)
        .await
        .context("writing the gap-filled games")?;
    report.games_filled = done.games_filled;
    report.team_rows = done.team_rows;
    report.player_rows = done.player_rows;
    report.scores_set = done.scores_set;
    report.games_created = done.games_created;
    report.players_created = done.players_created;
    Ok(report)
}

/// Give each new player one id, shared by every game in the run he appears
/// in. A new ESPN id two teams' lines claim this season cannot be one person
/// on both: the later game is dropped, box and all. Returns `(game, reason)`
/// for each game dropped.
fn assign_new_player_ids(writes: &mut [(&GameRow, GamePlan)]) -> Vec<(String, String)> {
    let mut minted: HashMap<(i32, String), (Uuid, Uuid)> = HashMap::new();
    let mut dropped = Vec::new();
    for (g, plan) in writes.iter_mut() {
        let clash = plan.players.iter().any(|r| {
            r.new_player.as_ref().is_some_and(|n| {
                minted
                    .get(&(g.season, n.espn_id.clone()))
                    .is_some_and(|(team, _)| *team != r.team_id)
            })
        });
        if clash {
            plan.players.clear();
            plan.teams.clear();
            let why = "a new player's ESPN id is on another team's line this run";
            plan.skip = Some(why.into());
            dropped.push((g.label.clone(), why.to_string()));
            continue;
        }
        for r in plan.players.iter_mut() {
            if let Some(n) = &r.new_player {
                r.player_id = minted
                    .entry((g.season, n.espn_id.clone()))
                    .or_insert((r.team_id, Uuid::new_v4()))
                    .1;
            }
        }
    }
    dropped
}

/// The window's CBBD finals that are on no game of ours, as games to plan,
/// with what creating each takes. A candidate needs:
///
/// - an ESPN event on no game of ours (`games.espn_id`) and not matched to
///   one by the broadcasts matcher (`game_broadcasts`), and the only CBBD
///   game on that event;
/// - both sides teams of ours this season by ESPN id (CBBD's team → ESPN's
///   from CBBD's cached `/teams` → `teams.espn_id`). A non-D1 opponent has no
///   ESPN id of ours, and neither does a D1 team not mapped yet; the two
///   cannot be told apart, so neither is created;
/// - nothing of ours that may already be this game ([`creation_blocker`]):
///   NatStat lists some games on the wrong date, and some against the wrong
///   opponent, and a copy of either would double-count it.
///
/// CBBD's orientation is kept: the game is created as CBBD lists it, and
/// NatStat's orientation replaces it if NatStat later lists the game
/// (`ingest::games::adopt_cbbd_games`).
async fn creatable_games(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
    report: &mut GapFillReport,
) -> Result<Vec<(GameRow, NewGame)>> {
    let cands: Vec<Candidate> = sqlx::query_as(
        "SELECT c.cbbd_game_id, c.espn_event_id, c.season, \
                (c.start_date AT TIME ZONE 'America/New_York')::date AS game_date, \
                (c.start_date AT TIME ZONE 'America/New_York')::time AS tip_time, \
                c.neutral_site, c.conference_game, c.home_team_id, c.away_team_id, \
                c.home_team, c.away_team, c.home_points, c.away_points \
         FROM cbbd_games c \
         WHERE c.status = 'final' AND c.espn_event_id IS NOT NULL \
           AND (c.start_date AT TIME ZONE 'America/New_York')::date BETWEEN $1 AND $2 \
           AND NOT EXISTS (SELECT 1 FROM games g WHERE g.espn_id = c.espn_event_id) \
           AND NOT EXISTS (SELECT 1 FROM game_broadcasts b \
                           WHERE b.source_event_id = c.espn_event_id) \
           AND (SELECT count(*) FROM cbbd_games c2 \
                WHERE c2.espn_event_id = c.espn_event_id) = 1 \
         ORDER BY game_date, c.cbbd_game_id",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("loading CBBD finals on no game of ours")?;
    report.cbbd_unmatched = cands.len();
    if cands.is_empty() {
        return Ok(Vec::new());
    }
    let Some(cbbd_teams) = crate::cbbd::cached_teams(pool)
        .await
        .context("reading CBBD's cached team list")?
    else {
        report.creation_off = Some("CBBD's team list is not cached".into());
        return Ok(Vec::new());
    };
    let espn_of: HashMap<i32, String> = cbbd_teams
        .into_iter()
        .filter_map(|t| Some((t.id, t.source_id?)))
        .collect();
    let seasons: Vec<i32> = cands
        .iter()
        .map(|c| c.season)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let ours: HashMap<(i32, String), Uuid> = sqlx::query_as::<_, (Uuid, i32, String)>(
        "SELECT id, season, espn_id FROM teams WHERE season = ANY($1) AND espn_id IS NOT NULL",
    )
    .bind(&seasons)
    .fetch_all(pool)
    .await
    .context("loading our teams' ESPN ids")?
    .into_iter()
    .map(|(id, season, espn)| ((season, espn), id))
    .collect();
    let team = |season: i32, cbbd: Option<i32>| {
        cbbd.and_then(|c| espn_of.get(&c))
            .and_then(|e| ours.get(&(season, e.clone())))
            .copied()
    };

    // Our games near the candidates, each with whether its ESPN event (when
    // it has one CBBD also lists) names the same two teams it does.
    let pad = chrono::Duration::days(CREATE_SAME_PAIR_DAYS);
    let near: Vec<NearRow> = sqlx::query_as(
        "SELECT g.id, g.season, g.game_date, g.home_team_id, g.away_team_id, \
                c.home_team_id AS cbbd_home, c.away_team_id AS cbbd_away \
         FROM games g LEFT JOIN cbbd_games c ON c.espn_event_id = g.espn_id \
         WHERE g.season = ANY($1) AND g.game_date BETWEEN $2 AND $3",
    )
    .bind(&seasons)
    .bind(from - pad)
    .bind(to + pad)
    .fetch_all(pool)
    .await
    .context("loading our games near the CBBD finals")?;
    let mut neighbors: HashMap<Uuid, Neighbor> = HashMap::new();
    for n in near {
        let proven = n.cbbd_home.is_some() && {
            let mut theirs = [team(n.season, n.cbbd_home), team(n.season, n.cbbd_away)];
            let mut mine = [n.home_team_id, n.away_team_id];
            theirs.sort();
            mine.sort();
            theirs == mine
        };
        let e = neighbors.entry(n.id).or_insert(Neighbor {
            game_date: n.game_date,
            home_team_id: n.home_team_id,
            away_team_id: n.away_team_id,
            proven_distinct: false,
        });
        e.proven_distinct |= proven;
    }
    let neighbors: Vec<Neighbor> = neighbors.into_values().collect();

    let mut out = Vec::new();
    for c in cands {
        let (Some(home), Some(away)) = (
            team(c.season, c.home_team_id),
            team(c.season, c.away_team_id),
        ) else {
            report.unmatched_unmapped += 1;
            continue;
        };
        let label = format!(
            "{} {} vs {} (not listed by NatStat)",
            c.game_date,
            c.home_team.as_deref().unwrap_or("?"),
            c.away_team.as_deref().unwrap_or("?")
        );
        if home == away {
            report
                .skipped
                .push((label, "CBBD's two sides are one team of ours".into()));
            continue;
        }
        // The run's own earlier candidates count as games of ours too, not
        // yet pinned down: two CBBD events for one meeting must not both be
        // created. (A team's second missing game within two days waits a
        // night, until the first is a game of ours on its ESPN event.)
        let taken: Vec<Neighbor> = out
            .iter()
            .map(|(g, n): &(GameRow, NewGame)| Neighbor {
                game_date: n.game_date,
                home_team_id: g.home_team_id,
                away_team_id: g.away_team_id,
                proven_distinct: false,
            })
            .collect();
        if let Some(why) = creation_blocker(c.game_date, home, away, &neighbors)
            .or_else(|| creation_blocker(c.game_date, home, away, &taken))
        {
            report.skipped.push((label, why));
            continue;
        }
        out.push((
            GameRow {
                id: Uuid::new_v4(),
                season: c.season,
                natstat_id: None,
                label,
                home_team_id: Some(home),
                away_team_id: Some(away),
                home_score: c.home_points,
                away_score: c.away_points,
                status: Some("Final".into()),
                is_neutral_site: c.neutral_site.unwrap_or(false),
                box_sources: Vec::new(),
                cbbd_game_id: c.cbbd_game_id,
                cbbd_status: Some("final".into()),
                cbbd_home_points: c.home_points,
                cbbd_away_points: c.away_points,
            },
            NewGame {
                espn_event_id: c.espn_event_id,
                game_date: c.game_date,
                tip_time: c.tip_time,
                is_conference: c.conference_game,
            },
        ));
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct Candidate {
    cbbd_game_id: i32,
    espn_event_id: String,
    season: i32,
    game_date: NaiveDate,
    tip_time: Option<NaiveTime>,
    neutral_site: Option<bool>,
    conference_game: Option<bool>,
    home_team_id: Option<i32>,
    away_team_id: Option<i32>,
    home_team: Option<String>,
    away_team: Option<String>,
    home_points: Option<i32>,
    away_points: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct NearRow {
    id: Uuid,
    season: i32,
    game_date: NaiveDate,
    home_team_id: Option<Uuid>,
    away_team_id: Option<Uuid>,
    cbbd_home: Option<i32>,
    cbbd_away: Option<i32>,
}

/// A game of ours near a CBBD final being considered for creation.
#[derive(Debug, Clone)]
pub struct Neighbor {
    pub game_date: NaiveDate,
    pub home_team_id: Option<Uuid>,
    pub away_team_id: Option<Uuid>,
    /// Its ESPN event is one CBBD lists between the same two teams, so it is
    /// a different game from any CBBD final not on that event.
    pub proven_distinct: bool,
}

/// No game is created when we already have one between the same two teams
/// within this many days: NatStat's date for a game can be off by two, and a
/// pair does not meet twice in a week.
pub const CREATE_SAME_PAIR_DAYS: i64 = 7;
/// ...nor when either team has a game of ours within this many days that no
/// ESPN event pins down as a different game: it may be this game under a
/// wrong date or a wrong opponent (NatStat listed UNC Wilmington's 100-41 win
/// over Montreat as a game against Louisiana).
pub const CREATE_NEARBY_DAYS: i64 = 2;

/// Why a CBBD final between our teams `a` and `b` on `date` must not be
/// created, if it must not.
pub fn creation_blocker(
    date: NaiveDate,
    a: Uuid,
    b: Uuid,
    neighbors: &[Neighbor],
) -> Option<String> {
    for n in neighbors {
        let teams = [n.home_team_id, n.away_team_id];
        let (has_a, has_b) = (teams.contains(&Some(a)), teams.contains(&Some(b)));
        if !has_a && !has_b {
            continue;
        }
        let days = (n.game_date - date).num_days().abs();
        if has_a && has_b && days <= CREATE_SAME_PAIR_DAYS {
            return Some(format!(
                "we have a game between the same teams on {}",
                n.game_date
            ));
        }
        if days <= CREATE_NEARBY_DAYS && !n.proven_distinct {
            return Some(format!(
                "a game of ours on {} that no ESPN event pins down may be this one",
                n.game_date
            ));
        }
    }
    None
}

#[derive(Default)]
struct Written {
    games_filled: usize,
    team_rows: usize,
    player_rows: usize,
    scores_set: usize,
    games_created: usize,
    players_created: usize,
}

/// One transaction: scores, then team rows, then player rows, each one
/// batched statement. A game that gained box rows since it was planned is
/// dropped from the box writes, so a fill never lands beside another source.
async fn write(pool: &PgPool, writes: &[(&GameRow, GamePlan)]) -> Result<Written, sqlx::Error> {
    let mut out = Written::default();
    let mut tx = pool.begin().await?;

    let scores: Vec<(Uuid, i32, i32)> = writes
        .iter()
        .filter_map(|(g, p)| p.score.map(|(h, a)| (g.id, h, a)))
        .collect();
    if !scores.is_empty() {
        // Only over no score, or the same score (a stuck status). Never over a
        // different score written since the plan read the row.
        let done = sqlx::query(
            "UPDATE games g SET home_score = u.h, away_score = u.a, status = 'Final', \
                score_source = $4, updated_at = now() \
             FROM UNNEST($1::uuid[], $2::int[], $3::int[]) AS u(id, h, a) \
             WHERE g.id = u.id \
               AND ((g.home_score IS NULL AND g.away_score IS NULL) \
                    OR (g.home_score = u.h AND g.away_score = u.a)) \
               AND (g.status IS DISTINCT FROM 'Final' OR g.home_score IS NULL)",
        )
        .bind(scores.iter().map(|s| s.0).collect::<Vec<_>>())
        .bind(scores.iter().map(|s| s.1).collect::<Vec<_>>())
        .bind(scores.iter().map(|s| s.2).collect::<Vec<_>>())
        .bind(CBBD_SOURCE)
        .execute(&mut *tx)
        .await?;
        out.scores_set = done.rows_affected() as usize;
    }

    let boxed: Vec<Uuid> = writes
        .iter()
        .filter(|(_, p)| !p.players.is_empty())
        .map(|(g, _)| g.id)
        .collect();
    if boxed.is_empty() {
        tx.commit().await?;
        return Ok(out);
    }
    let taken: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT game_id FROM team_game_stats WHERE game_id = ANY($1) \
         UNION SELECT game_id FROM player_game_stats WHERE game_id = ANY($1)",
    )
    .bind(&boxed)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let mut fills: Vec<(&GameRow, &GamePlan)> = writes
        .iter()
        .filter(|(g, p)| !p.players.is_empty() && !taken.contains(&g.id))
        .map(|(g, p)| (*g, p))
        .collect();

    // A game or player to create whose ESPN id, or placeholder natstat id, a
    // row has taken since the plan read the tables (or a hand-cleared
    // `espn_id` left behind) takes its game out, so the inserts below cannot
    // fail the transaction.
    let new_events: Vec<String> = fills
        .iter()
        .filter_map(|(_, p)| p.new_game.as_ref().map(|n| n.espn_event_id.clone()))
        .collect();
    let events_held: HashSet<String> = if new_events.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT e FROM UNNEST($1::text[]) AS e \
             WHERE EXISTS (SELECT 1 FROM games g WHERE g.espn_id = e OR g.natstat_id = $2 || e)",
        )
        .bind(&new_events)
        .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect()
    };
    let new_people: Vec<(i32, String)> = fills
        .iter()
        .flat_map(|(g, p)| {
            p.players
                .iter()
                .filter_map(move |r| r.new_player.as_ref().map(|n| (g.season, n.espn_id.clone())))
        })
        .collect();
    let people_held: HashSet<(i32, String)> = if new_people.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_as::<_, (i32, String)>(
            "SELECT u.season, u.espn FROM UNNEST($1::int[], $2::text[]) AS u(season, espn) \
             WHERE EXISTS (SELECT 1 FROM players p WHERE p.season = u.season \
                           AND (p.espn_id = u.espn OR p.natstat_id = $3 || u.espn))",
        )
        .bind(new_people.iter().map(|p| p.0).collect::<Vec<_>>())
        .bind(new_people.iter().map(|p| p.1.clone()).collect::<Vec<_>>())
        .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect()
    };
    fills.retain(|(g, p)| {
        p.new_game
            .as_ref()
            .is_none_or(|n| !events_held.contains(&n.espn_event_id))
            && p.players.iter().all(|r| {
                r.new_player
                    .as_ref()
                    .is_none_or(|n| !people_held.contains(&(g.season, n.espn_id.clone())))
            })
    });
    out.games_filled = fills.len();

    let created: Vec<(&GameRow, &NewGame)> = fills
        .iter()
        .filter_map(|(g, p)| p.new_game.as_ref().map(|n| (*g, n)))
        .collect();
    if !created.is_empty() {
        out.games_created = sqlx::query(
            "INSERT INTO games (id, natstat_id, season, game_date, home_team_id, away_team_id, \
                home_score, away_score, is_neutral_site, is_conference, status, tip_time, \
                espn_id, score_source) \
             SELECT u.id, $12 || u.espn, u.season, u.game_date, u.home, u.away, u.hs, u.aws, \
                u.neutral, u.conf, 'Final', u.tip, u.espn, $13 \
             FROM UNNEST($1::uuid[], $2::int[], $3::date[], $4::uuid[], $5::uuid[], \
                $6::int[], $7::int[], $8::bool[], $9::bool[], $10::time[], $11::text[]) \
                AS u(id, season, game_date, home, away, hs, aws, neutral, conf, tip, espn)",
        )
        .bind(created.iter().map(|(g, _)| g.id).collect::<Vec<_>>())
        .bind(created.iter().map(|(g, _)| g.season).collect::<Vec<_>>())
        .bind(created.iter().map(|(_, n)| n.game_date).collect::<Vec<_>>())
        .bind(
            created
                .iter()
                .map(|(g, _)| g.home_team_id)
                .collect::<Vec<_>>(),
        )
        .bind(
            created
                .iter()
                .map(|(g, _)| g.away_team_id)
                .collect::<Vec<_>>(),
        )
        .bind(
            created
                .iter()
                .map(|(g, _)| g.home_score)
                .collect::<Vec<_>>(),
        )
        .bind(
            created
                .iter()
                .map(|(g, _)| g.away_score)
                .collect::<Vec<_>>(),
        )
        .bind(
            created
                .iter()
                .map(|(g, _)| g.is_neutral_site)
                .collect::<Vec<_>>(),
        )
        .bind(
            created
                .iter()
                .map(|(_, n)| n.is_conference)
                .collect::<Vec<_>>(),
        )
        .bind(created.iter().map(|(_, n)| n.tip_time).collect::<Vec<_>>())
        .bind(
            created
                .iter()
                .map(|(_, n)| n.espn_event_id.clone())
                .collect::<Vec<_>>(),
        )
        .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
        .bind(CBBD_SOURCE)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;
    }

    let mut people: HashMap<Uuid, (i32, Uuid, &NewPlayer)> = HashMap::new();
    for (g, p) in &fills {
        for r in &p.players {
            if let Some(n) = &r.new_player {
                people.insert(r.player_id, (g.season, r.team_id, n));
            }
        }
    }
    if !people.is_empty() {
        let people: Vec<(&Uuid, &(i32, Uuid, &NewPlayer))> = people.iter().collect();
        out.players_created = sqlx::query(
            "INSERT INTO players (id, natstat_id, name, team_id, season, espn_id) \
             SELECT u.id, $6 || u.espn, u.name, u.team, u.season, u.espn \
             FROM UNNEST($1::uuid[], $2::text[], $3::uuid[], $4::int[], $5::text[]) \
                AS u(id, name, team, season, espn)",
        )
        .bind(people.iter().map(|(id, _)| **id).collect::<Vec<_>>())
        .bind(
            people
                .iter()
                .map(|(_, v)| v.2.name.clone())
                .collect::<Vec<_>>(),
        )
        .bind(people.iter().map(|(_, v)| v.1).collect::<Vec<_>>())
        .bind(people.iter().map(|(_, v)| v.0).collect::<Vec<_>>())
        .bind(
            people
                .iter()
                .map(|(_, v)| v.2.espn_id.clone())
                .collect::<Vec<_>>(),
        )
        .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;
    }

    let teams: Vec<(&GameRow, &TeamRow)> = fills
        .iter()
        .flat_map(|(g, p)| p.teams.iter().map(move |t| (*g, t)))
        .collect();
    let ts = |f: fn(&LineStats) -> Option<i32>| {
        teams.iter().map(|(_, t)| f(&t.stats)).collect::<Vec<_>>()
    };
    out.team_rows = sqlx::query(
        "INSERT INTO team_game_stats (team_id, game_id, season, game_date, opponent_id, is_home, \
            win, minutes, points, fgm, fga, tpm, tpa, ftm, fta, off_rebounds, def_rebounds, \
            total_rebounds, assists, steals, blocks, turnovers, fouls, source) \
         SELECT u.team_id, u.game_id, g.season, g.game_date, u.opponent_id, u.is_home, u.win, \
            u.minutes, u.points, u.fgm, u.fga, u.tpm, u.tpa, u.ftm, u.fta, u.orb, u.drb, u.reb, \
            u.ast, u.stl, u.blk, u.tov, u.pf, $22 \
         FROM UNNEST($1::uuid[], $2::uuid[], $3::uuid[], $4::bool[], $5::bool[], $6::int[], \
            $7::int[], $8::int[], $9::int[], $10::int[], $11::int[], $12::int[], $13::int[], \
            $14::int[], $15::int[], $16::int[], $17::int[], $18::int[], $19::int[], $20::int[], \
            $21::int[]) \
            AS u(team_id, game_id, opponent_id, is_home, win, minutes, points, fgm, fga, tpm, \
                 tpa, ftm, fta, orb, drb, reb, ast, stl, blk, tov, pf) \
         JOIN games g ON g.id = u.game_id",
    )
    .bind(teams.iter().map(|(_, t)| t.team_id).collect::<Vec<_>>())
    .bind(teams.iter().map(|(g, _)| g.id).collect::<Vec<_>>())
    .bind(teams.iter().map(|(_, t)| t.opponent_id).collect::<Vec<_>>())
    .bind(teams.iter().map(|(_, t)| t.is_home).collect::<Vec<_>>())
    .bind(teams.iter().map(|(_, t)| t.win).collect::<Vec<_>>())
    .bind(ts(|s| s.minutes))
    .bind(ts(|s| s.points))
    .bind(ts(|s| s.fgm))
    .bind(ts(|s| s.fga))
    .bind(ts(|s| s.tpm))
    .bind(ts(|s| s.tpa))
    .bind(ts(|s| s.ftm))
    .bind(ts(|s| s.fta))
    .bind(ts(|s| s.off_rebounds))
    .bind(ts(|s| s.def_rebounds))
    .bind(ts(|s| s.total_rebounds))
    .bind(ts(|s| s.assists))
    .bind(ts(|s| s.steals))
    .bind(ts(|s| s.blocks))
    .bind(ts(|s| s.turnovers))
    .bind(ts(|s| s.fouls))
    .bind(CBBD_SOURCE)
    .execute(&mut *tx)
    .await?
    .rows_affected() as usize;

    let players: Vec<(&GameRow, &PlayerRow)> = fills
        .iter()
        .flat_map(|(g, p)| p.players.iter().map(move |r| (*g, r)))
        .collect();
    let ps = |f: fn(&LineStats) -> Option<i32>| {
        players.iter().map(|(_, r)| f(&r.stats)).collect::<Vec<_>>()
    };
    out.player_rows = sqlx::query(
        "INSERT INTO player_game_stats (player_id, game_id, team_id, season, game_date, \
            opponent_id, is_home, starter, minutes, points, fgm, fga, tpm, tpa, ftm, fta, \
            off_rebounds, def_rebounds, total_rebounds, assists, steals, blocks, turnovers, \
            fouls, source) \
         SELECT u.player_id, u.game_id, u.team_id, g.season, g.game_date, u.opponent_id, \
            u.is_home, u.starter, u.minutes, u.points, u.fgm, u.fga, u.tpm, u.tpa, u.ftm, u.fta, \
            u.orb, u.drb, u.reb, u.ast, u.stl, u.blk, u.tov, u.pf, $23 \
         FROM UNNEST($1::uuid[], $2::uuid[], $3::uuid[], $4::uuid[], $5::bool[], $6::bool[], \
            $7::float8[], $8::int[], $9::int[], $10::int[], $11::int[], $12::int[], $13::int[], \
            $14::int[], $15::int[], $16::int[], $17::int[], $18::int[], $19::int[], $20::int[], \
            $21::int[], $22::int[]) \
            AS u(player_id, game_id, team_id, opponent_id, is_home, starter, minutes, points, \
                 fgm, fga, tpm, tpa, ftm, fta, orb, drb, reb, ast, stl, blk, tov, pf) \
         JOIN games g ON g.id = u.game_id",
    )
    .bind(players.iter().map(|(_, r)| r.player_id).collect::<Vec<_>>())
    .bind(players.iter().map(|(g, _)| g.id).collect::<Vec<_>>())
    .bind(players.iter().map(|(_, r)| r.team_id).collect::<Vec<_>>())
    .bind(
        players
            .iter()
            .map(|(_, r)| r.opponent_id)
            .collect::<Vec<_>>(),
    )
    .bind(players.iter().map(|(_, r)| r.is_home).collect::<Vec<_>>())
    .bind(
        players
            .iter()
            .map(|(_, r)| r.stats.starter)
            .collect::<Vec<_>>(),
    )
    .bind(
        players
            .iter()
            .map(|(_, r)| r.stats.minutes.map(f64::from))
            .collect::<Vec<_>>(),
    )
    .bind(ps(|s| s.points))
    .bind(ps(|s| s.fgm))
    .bind(ps(|s| s.fga))
    .bind(ps(|s| s.tpm))
    .bind(ps(|s| s.tpa))
    .bind(ps(|s| s.ftm))
    .bind(ps(|s| s.fta))
    .bind(ps(|s| s.off_rebounds))
    .bind(ps(|s| s.def_rebounds))
    .bind(ps(|s| s.total_rebounds))
    .bind(ps(|s| s.assists))
    .bind(ps(|s| s.steals))
    .bind(ps(|s| s.blocks))
    .bind(ps(|s| s.turnovers))
    .bind(ps(|s| s.fouls))
    .bind(CBBD_SOURCE)
    .execute(&mut *tx)
    .await?
    .rows_affected() as usize;

    tx.commit().await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: u128 = 10;
    const AWAY: u128 = 20;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Our game, home team 10 vs away team 20, with no box rows.
    fn game(ours: Option<(i32, i32)>, status: Option<&str>, cbbd: (i32, i32)) -> GameIn {
        GameIn {
            id: id(1),
            natstat_id: Some("1".into()),
            label: "g".into(),
            home_team_id: Some(id(HOME)),
            away_team_id: Some(id(AWAY)),
            home_score: ours.map(|s| s.0),
            away_score: ours.map(|s| s.1),
            status: status.map(str::to_string),
            neutral: false,
            box_sources: vec![],
            cbbd_status: Some("final".into()),
            cbbd_home_points: Some(cbbd.0),
            cbbd_away_points: Some(cbbd.1),
        }
    }

    fn line(cbbd_home: bool, name: &str, espn: Option<&str>, pts: i32) -> Line {
        Line {
            cbbd_is_home: cbbd_home,
            name: name.into(),
            espn_id: espn.map(str::to_string),
            stats: LineStats {
                starter: Some(true),
                minutes: Some(20),
                points: Some(pts),
                fgm: Some(pts / 2),
                fga: Some(pts),
                tpm: Some(0),
                tpa: Some(1),
                ftm: Some(pts % 2),
                fta: Some(pts % 2),
                off_rebounds: Some(1),
                def_rebounds: Some(2),
                total_rebounds: Some(3),
                assists: Some(1),
                steals: Some(0),
                blocks: Some(0),
                turnovers: Some(2),
                fouls: Some(3),
            },
        }
    }

    /// Players 101/102 on team 10 and 201/202 on team 20, ESPN ids "e101"…
    fn players() -> Players {
        let mut p = Players::default();
        for (pid, team) in [(101, HOME), (102, HOME), (201, AWAY), (202, AWAY)] {
            let player = Player {
                id: id(pid),
                name: format!("Player {pid}"),
                team_id: Some(id(team)),
                espn_id: Some(format!("e{pid}")),
            };
            p.by_espn.insert(format!("e{pid}"), player.clone());
            p.rosters.entry(id(team)).or_default().push(player);
        }
        p
    }

    /// A 10-7 game: CBBD home lines are players 101+102, away 201+202.
    fn lines() -> Vec<Line> {
        vec![
            line(true, "Player 101", Some("e101"), 6),
            line(true, "Player 102", Some("e102"), 4),
            line(false, "Player 201", Some("e201"), 5),
            line(false, "Player 202", Some("e202"), 2),
        ]
    }

    /// The same lines with CBBD's sides flipped: our away team is CBBD's home.
    fn flipped_lines() -> Vec<Line> {
        lines()
            .into_iter()
            .map(|mut l| {
                l.cbbd_is_home = !l.cbbd_is_home;
                l
            })
            .collect()
    }

    fn team_points(plan: &GamePlan, team: u128) -> Option<i32> {
        plan.teams
            .iter()
            .find(|t| t.team_id == id(team))
            .and_then(|t| t.stats.points)
    }

    #[test]
    fn fills_a_game_with_no_box_and_keeps_our_final_score() {
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (10, 7)),
            &lines(),
            &players(),
        );
        assert_eq!(plan.skip, None);
        assert_eq!(plan.score, None, "our score is already final");
        assert_eq!(plan.players.len(), 4);
        assert_eq!(team_points(&plan, HOME), Some(10));
        assert_eq!(team_points(&plan, AWAY), Some(7));
        let home = plan.teams.iter().find(|t| t.team_id == id(HOME)).unwrap();
        assert!(home.is_home && home.win && home.opponent_id == Some(id(AWAY)));
    }

    #[test]
    fn orients_by_the_score_when_cbbds_sides_are_swapped() {
        // CBBD lists our away team as its home side, at 7-10.
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (7, 10)),
            &flipped_lines(),
            &players(),
        );
        assert_eq!(plan.skip, None);
        assert_eq!(team_points(&plan, HOME), Some(10));
        assert_eq!(team_points(&plan, AWAY), Some(7));
        let p101 = plan
            .players
            .iter()
            .find(|r| r.player_id == id(101))
            .unwrap();
        assert_eq!(p101.team_id, id(HOME));
        assert!(p101.is_home);
    }

    #[test]
    fn orients_by_the_players_when_ours_has_no_score_and_sets_it() {
        // Stuck at an in-progress status with no score; CBBD's sides swapped.
        let plan = plan_game(
            &game(None, Some("H2 05:11"), (7, 10)),
            &flipped_lines(),
            &players(),
        );
        assert_eq!(plan.skip, None);
        assert_eq!(plan.score, Some((10, 7)), "home first, in our orientation");
        assert_eq!(team_points(&plan, HOME), Some(10));
    }

    #[test]
    fn a_stuck_status_with_our_score_is_finished_too() {
        let plan = plan_game(
            &game(Some((10, 7)), Some("H2 05:11"), (10, 7)),
            &lines(),
            &players(),
        );
        assert_eq!(plan.score, Some((10, 7)));
    }

    #[test]
    fn score_and_players_disagreeing_skips_the_game() {
        // Our score says CBBD's sides match ours; the players say swapped.
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (10, 7)),
            &flipped_lines(),
            &players(),
        );
        assert!(plan.skip.is_some());
        assert!(plan.players.is_empty() && plan.score.is_none());
    }

    #[test]
    fn a_player_of_a_third_team_means_the_wrong_event() {
        let mut p = players();
        p.by_espn.get_mut("e102").unwrap().team_id = Some(id(99));
        let plan = plan_game(&game(None, None, (10, 7)), &lines(), &p);
        assert!(plan.skip.unwrap().contains("not in this game"));
        assert_eq!(
            plan.score, None,
            "never score a game matched to another event"
        );
    }

    #[test]
    fn no_score_and_no_known_player_cannot_be_oriented() {
        let mut p = players();
        p.by_espn.clear();
        let plan = plan_game(&game(None, None, (10, 7)), &lines(), &p);
        assert!(plan.skip.is_some());
        assert_eq!(plan.score, None);
    }

    #[test]
    fn team_rows_are_the_player_sums_not_cbbds_team_line() {
        // CBBD's team line would carry team rebounds and turnovers; the plan
        // never sees it, and the team row adds up the players.
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (10, 7)),
            &lines(),
            &players(),
        );
        let home = plan.teams.iter().find(|t| t.team_id == id(HOME)).unwrap();
        assert_eq!(home.stats.total_rebounds, Some(6));
        assert_eq!(home.stats.off_rebounds, Some(2));
        assert_eq!(home.stats.def_rebounds, Some(4));
        assert_eq!(home.stats.turnovers, Some(4));
        assert_eq!(home.stats.fouls, Some(6));
        assert_eq!(home.stats.minutes, Some(40));
        assert_eq!(home.stats.starter, None);
        // A line missing a field leaves the sum unknown rather than short.
        let mut ls = lines();
        ls[0].stats.steals = None;
        let refs: Vec<&Line> = ls.iter().filter(|l| l.cbbd_is_home).collect();
        assert_eq!(team_totals(&refs).steals, None);
    }

    #[test]
    fn a_zero_zero_final_is_never_written() {
        for ours in [None, Some((10, 7))] {
            let plan = plan_game(&game(ours, None, (0, 0)), &lines(), &players());
            assert!(plan.skip.unwrap().contains("0-0"));
            assert!(plan.players.is_empty() && plan.score.is_none());
        }
        let mut g = game(None, None, (10, 7));
        g.cbbd_home_points = None;
        let plan = plan_game(&g, &lines(), &players());
        assert!(plan.skip.is_some() && plan.score.is_none());
    }

    #[test]
    fn a_score_that_disagrees_with_our_final_is_skipped() {
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (11, 7)),
            &lines(),
            &players(),
        );
        assert!(plan.skip.unwrap().contains("disagrees"));
        assert!(plan.players.is_empty() && plan.score.is_none());
    }

    #[test]
    fn one_unresolved_player_skips_the_whole_box_but_not_the_score() {
        let mut ls = lines();
        ls[3].espn_id = Some("e999".into());
        ls[3].name = "Walk On".into();
        let plan = plan_game(&game(None, Some("H2 05:11"), (10, 7)), &ls, &players());
        assert!(plan.skip.unwrap().contains("Walk On"));
        assert!(plan.players.is_empty() && plan.teams.is_empty());
        // The sides are known from the other players, so the score still lands.
        assert_eq!(plan.score, Some((10, 7)));
    }

    #[test]
    fn a_line_resolves_by_exact_name_to_an_unmapped_roster_player() {
        let mut p = players();
        // Player 202 has no ESPN id yet; his line carries one we do not know.
        p.by_espn.remove("e202");
        for r in p.rosters.values_mut() {
            for pl in r.iter_mut().filter(|pl| pl.id == id(202)) {
                pl.espn_id = None;
            }
        }
        let mut ls = lines();
        ls[3].espn_id = Some("e202-new".into());
        let plan = plan_game(&game(Some((10, 7)), Some("Final"), (10, 7)), &ls, &p);
        assert_eq!(plan.skip, None);
        let r = plan
            .players
            .iter()
            .find(|r| r.player_id == id(202))
            .unwrap();
        assert!(r.by_name);
        // A last name alone is not enough against a roster.
        ls[3].name = "Someone 202".into();
        let plan = plan_game(&game(Some((10, 7)), Some("Final"), (10, 7)), &ls, &p);
        assert!(plan.skip.unwrap().contains("Someone 202"));
    }

    #[test]
    fn player_points_must_add_up_to_the_score() {
        // A line missing from CBBD's box: the side's points fall short.
        let mut ls = lines();
        ls.remove(1);
        let plan = plan_game(
            &game(Some((10, 7)), Some("Final"), (10, 7)),
            &ls,
            &players(),
        );
        assert!(plan.skip.unwrap().contains("add to 6"));
        assert!(plan.players.is_empty());
    }

    #[test]
    fn a_boxed_game_is_left_alone() {
        let mut g = game(Some((10, 7)), Some("Final"), (10, 7));
        g.box_sources = vec!["natstat".into()];
        let plan = plan_game(&g, &lines(), &players());
        assert!(plan.already_boxed && plan.players.is_empty() && plan.skip.is_none());
        // A stuck status on a boxed game still gets its final, and no rows.
        g.status = Some("H2 05:11".into());
        let plan = plan_game(&g, &lines(), &players());
        assert!(plan.already_boxed && plan.players.is_empty());
        assert_eq!(plan.score, Some((10, 7)));
    }

    #[test]
    fn a_non_d1_side_gets_no_rows() {
        let mut g = game(Some((10, 7)), Some("Final"), (10, 7));
        g.away_team_id = None;
        let ls: Vec<Line> = lines()
            .into_iter()
            .map(|mut l| {
                if !l.cbbd_is_home {
                    l.espn_id = None;
                    l.name = format!("Visitor {}", l.name);
                }
                l
            })
            .collect();
        let plan = plan_game(&g, &ls, &players());
        assert_eq!(plan.skip, None);
        assert_eq!(plan.teams.len(), 1);
        assert_eq!(plan.players.len(), 2);
        assert!(plan.teams[0].opponent_id.is_none());
    }

    /// Players 101–103 and 201–203, ESPN-mapped, and a 15-11 game whose
    /// CBBD box adds a newcomer to each side ("Fresh Face", "New Guy").
    fn bigger_game() -> (GameIn, Vec<Line>, Players) {
        let mut p = Players::default();
        let mut ls = Vec::new();
        for (pid, team, home) in [
            (101, HOME, true),
            (102, HOME, true),
            (103, HOME, true),
            (201, AWAY, false),
            (202, AWAY, false),
            (203, AWAY, false),
        ] {
            let player = Player {
                id: id(pid),
                name: format!("Player {pid}"),
                team_id: Some(id(team)),
                espn_id: Some(format!("e{pid}")),
            };
            p.by_espn.insert(format!("e{pid}"), player.clone());
            p.rosters.entry(id(team)).or_default().push(player);
            ls.push(line(
                home,
                &format!("Player {pid}"),
                Some(&format!("e{pid}")),
                3,
            ));
        }
        ls.push(line(true, "Fresh Face", Some("e901"), 6));
        ls.push(line(false, "New Guy", Some("e902"), 2));
        (game(Some((15, 11)), Some("Final"), (15, 11)), ls, p)
    }

    #[test]
    fn a_line_no_player_of_ours_matches_creates_one() {
        let (g, ls, p) = bigger_game();
        let plan = plan_game(&g, &ls, &p);
        assert_eq!(plan.skip, None);
        assert_eq!(plan.players.len(), 8);
        let new: Vec<&PlayerRow> = plan
            .players
            .iter()
            .filter(|r| r.new_player.is_some())
            .collect();
        assert_eq!(new.len(), 2);
        let fresh = new
            .iter()
            .find(|r| r.new_player.as_ref().unwrap().espn_id == "e901")
            .unwrap();
        assert_eq!(fresh.team_id, id(HOME));
        assert_eq!(fresh.new_player.as_ref().unwrap().name, "Fresh Face");
        assert_eq!(team_points(&plan, HOME), Some(15));
    }

    #[test]
    fn a_side_mostly_of_strangers_creates_nobody() {
        // The wrong-event guard: a side whose lines are mostly players we have
        // never seen is not evidence they are this team's.
        let (g, mut ls, p) = bigger_game();
        for l in ls
            .iter_mut()
            .filter(|l| l.name == "Player 102" || l.name == "Player 103")
        {
            l.espn_id = Some(format!("x-{}", l.name));
            l.name = format!("Stranger {}", &l.name[7..]);
        }
        let plan = plan_game(&g, &ls, &p);
        assert!(plan.players.is_empty());
        assert!(plan.skip.unwrap().contains("too few of the side's players"));
    }

    #[test]
    fn a_name_a_roster_player_might_have_is_not_created() {
        let roster = |name: &str, espn: Option<&str>| {
            vec![Player {
                id: id(1),
                name: name.into(),
                team_id: Some(id(HOME)),
                espn_id: espn.map(str::to_string),
            }]
        };
        // Spelled apart: squashed names agree, whatever the ESPN id.
        assert!(may_be_on_roster(
            "Jalen StClair",
            &roster("Jalen St Clair", Some("e1"))
        ));
        // Diacritics normalization misses: the first name is shared.
        assert!(may_be_on_roster(
            "Roko Prkacin",
            &roster("Roko Prkačin", None)
        ));
        // The duplicates the first local run created under a narrower rule.
        for (line, ours) in [
            ("Xabi Lopez", "Xabier Lopez Rodriguez"),
            ("Carlos Varela", "Carlos Varela Romero"),
            ("Daniel Perez-Rouzan", "Daniel Rouzan"),
            ("Ahmed Nedal", "Ahmed Nedal Abdelrahman"),
        ] {
            assert!(may_be_on_roster(line, &roster(ours, None)), "{line}");
        }
        // Initials and two-letter parts are not compared.
        assert!(!may_be_on_roster("JJ Laboy", &roster("JJ Smith", None)));
        // Same last name, a roster player with no ESPN id yet.
        assert!(may_be_on_roster(
            "Josh Clark",
            &roster("Joshua Clark", None)
        ));
        // ...but one with an ESPN id is a different person by ESPN's word.
        assert!(!may_be_on_roster(
            "Josh Clark",
            &roster("Joshua Clark", Some("e1"))
        ));
        assert!(!may_be_on_roster("Fresh Face", &roster("Player 101", None)));
        assert!(may_be_on_roster("", &[]));
    }

    #[test]
    fn a_new_player_seen_in_two_games_gets_one_id_and_two_teams_drop_the_later_game() {
        let (g, ls, p) = bigger_game();
        let row = |label: &str| GameRow {
            id: Uuid::new_v4(),
            season: 2027,
            natstat_id: None,
            label: label.into(),
            home_team_id: g.home_team_id,
            away_team_id: g.away_team_id,
            home_score: g.home_score,
            away_score: g.away_score,
            status: g.status.clone(),
            is_neutral_site: false,
            box_sources: vec![],
            cbbd_game_id: 1,
            cbbd_status: None,
            cbbd_home_points: None,
            cbbd_away_points: None,
        };
        let (a, b) = (row("a"), row("b"));
        let mut writes = vec![(&a, plan_game(&g, &ls, &p)), (&b, plan_game(&g, &ls, &p))];
        assert!(assign_new_player_ids(&mut writes).is_empty());
        let fresh = |plan: &GamePlan| {
            plan.players
                .iter()
                .find(|r| r.new_player.as_ref().is_some_and(|n| n.espn_id == "e901"))
                .unwrap()
                .player_id
        };
        assert_ne!(fresh(&writes[0].1), Uuid::nil());
        assert_eq!(fresh(&writes[0].1), fresh(&writes[1].1));

        // The same newcomer on the other team in a later game: dropped.
        let mut flipped = ls.clone();
        for l in flipped.iter_mut() {
            if l.name == "Fresh Face" {
                l.espn_id = Some("e902".into());
            } else if l.name == "New Guy" {
                l.espn_id = Some("e901".into());
            }
        }
        let mut writes = vec![
            (&a, plan_game(&g, &ls, &p)),
            (&b, plan_game(&g, &flipped, &p)),
        ];
        let dropped = assign_new_player_ids(&mut writes);
        assert_eq!(dropped.len(), 1);
        assert!(writes[1].1.players.is_empty() && writes[1].1.teams.is_empty());
        assert!(!writes[0].1.players.is_empty());
    }

    #[test]
    fn a_cbbd_final_that_may_be_a_game_of_ours_is_not_created() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let n = |date: &str, home: u128, away: Option<u128>, proven: bool| Neighbor {
            game_date: d(date),
            home_team_id: Some(id(home)),
            away_team_id: away.map(id),
            proven_distinct: proven,
        };
        let on = d("2026-11-18");
        // Nothing near either team: created.
        assert_eq!(creation_blocker(on, id(HOME), id(AWAY), &[]), None);
        // The same two teams on another date within a week (NatStat's
        // Arkansas-Indiana sat two days off ESPN's), even pinned down.
        assert!(
            creation_blocker(
                on,
                id(HOME),
                id(AWAY),
                &[n("2026-11-16", AWAY, Some(HOME), true)]
            )
            .unwrap()
            .contains("same teams")
        );
        // A nearby game of one team that no ESPN event pins down may be this
        // one against a wrong opponent.
        assert!(
            creation_blocker(
                on,
                id(HOME),
                id(AWAY),
                &[n("2026-11-17", HOME, Some(99), false)]
            )
            .is_some()
        );
        // Pinned down as a different game, or far enough away: no bar.
        assert_eq!(
            creation_blocker(
                on,
                id(HOME),
                id(AWAY),
                &[n("2026-11-17", HOME, Some(99), true)]
            ),
            None
        );
        assert_eq!(
            creation_blocker(
                on,
                id(HOME),
                id(AWAY),
                &[n("2026-11-14", HOME, Some(99), false)]
            ),
            None
        );
        // Another pair's game is nothing to do with it.
        assert_eq!(
            creation_blocker(
                on,
                id(HOME),
                id(AWAY),
                &[n("2026-11-18", 98, Some(99), false)]
            ),
            None
        );
    }

    #[test]
    fn a_neutral_site_has_no_home_side() {
        let mut g = game(Some((10, 7)), Some("Final"), (10, 7));
        g.neutral = true;
        let plan = plan_game(&g, &lines(), &players());
        assert!(plan.teams.iter().all(|t| !t.is_home));
        assert!(plan.players.iter().all(|r| !r.is_home));
    }
}
