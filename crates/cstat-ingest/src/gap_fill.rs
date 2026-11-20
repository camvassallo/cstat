//! `gap-fill`: write CollegeBasketballData's box scores into
//! `player_game_stats` / `team_game_stats` for the games NatStat has no box
//! score for, and finish the games NatStat left stuck at an in-progress status
//! (`docs/natstat_migration_plan.md` §4.5 step 3).
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
//!   no rows, as with NatStat. No `players` row is created.
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
use chrono::NaiveDate;
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

/// A `player_game_stats` row to write.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerRow {
    pub player_id: Uuid,
    pub team_id: Uuid,
    pub opponent_id: Option<Uuid>,
    pub is_home: bool,
    pub stats: LineStats,
    /// Resolved by name rather than ESPN id (for the report).
    pub by_name: bool,
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
        for (l, r) in side.iter().zip(&resolved) {
            match *r {
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
    /// `(game, reason)` for each final game whose box or score was not written.
    pub skipped: Vec<(String, String)>,
}

impl GapFillReport {
    /// Rows written (or that would be, on a dry run).
    pub fn written(&self) -> usize {
        self.team_rows + self.player_rows + self.scores_set
    }

    /// One line for the nightly ledger's notes.
    pub fn notes(&self) -> String {
        format!(
            "{} games filled ({} team rows, {} player rows, {} by name), {} scores set, \
             {} skipped, {} already boxed by NatStat, {} by CBBD",
            self.games_filled,
            self.team_rows,
            self.player_rows,
            self.players_by_name,
            self.scores_set,
            self.skipped.len(),
            self.already_natstat,
            self.already_cbbd
        )
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
    let games: Vec<GameRow> = games
        .into_iter()
        .filter(|g| g.cbbd_status.as_deref() == Some("final"))
        .collect();
    report.cbbd_final = games.len();
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
        let plan = plan_game(&input, lines, by_season.get(&g.season).unwrap_or(&empty));
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

    let filled: Vec<&(&GameRow, GamePlan)> = writes
        .iter()
        .filter(|(_, p)| !p.players.is_empty())
        .collect();
    report.games_filled = filled.len();
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
    Ok(report)
}

#[derive(Default)]
struct Written {
    games_filled: usize,
    team_rows: usize,
    player_rows: usize,
    scores_set: usize,
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
    let fills: Vec<(&GameRow, &GamePlan)> = writes
        .iter()
        .filter(|(g, p)| !p.players.is_empty() && !taken.contains(&g.id))
        .map(|(g, p)| (*g, p))
        .collect();
    out.games_filled = fills.len();

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

    #[test]
    fn a_neutral_site_has_no_home_side() {
        let mut g = game(Some((10, 7)), Some("Final"), (10, 7));
        g.neutral = true;
        let plan = plan_game(&g, &lines(), &players());
        assert!(plan.teams.iter().all(|t| !t.is_home));
        assert!(plan.players.iter().all(|r| !r.is_home));
    }
}
