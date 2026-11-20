//! `cbbd-plays`: CollegeBasketballData play-by-play into `play_by_play`, for
//! the current season (step 4 of `docs/natstat_migration_plan.md` §4.5).
//!
//! NatStat's November 2026 format change left its plays with no clock, no
//! tags, no on-floor players and no player ids; the loader rebuilds the first
//! three from the text and the lineup chain falls back to substitution replay.
//! CBBD's `/plays/date` carries all of them, including **all ten on-floor
//! players on ~98% of plays**, so `compute_pbp_lineups` reads lineups straight
//! from the rows (`pbp_replay`'s exact on-floor path).
//!
//! **CBBD replaces NatStat's plays for every game it has**, from
//! [`FIRST_SEASON`] on; NatStat's loader only fills the games CBBD lacks, and
//! skips any game holding CBBD rows (`play_by_play.source`, migration 078), so
//! the two never double up. Earlier seasons are never read or written: they
//! stay the data the models were trained on. The comparison behind the
//! decision is in the migration plan, §4.5 step 4.
//!
//! **Ids, not names.** A game joins on `games.espn_id` (the play's
//! `gameSourceId`). A player joins CBBD athlete id → `cbbd_player_game_stats`
//! → ESPN athlete id → `players.espn_id` (migration 076); a participant that
//! does not resolve keeps a NULL `player_id`, and an on-floor player that does
//! not resolve is stored as `cbbd:<athlete id>`, so the lineup builder still
//! sees five players change and only the unknown one is left out of the unit.
//!
//! **Sides follow our `games` row**, not CBBD's: CBBD's home and away do not
//! always follow ESPN's, so the sides are oriented by the final score exactly
//! as `source-parity` does ([`crate::source_parity::cbbd_swapped`]).
//!
//! **Days.** `/plays/date` takes a UTC day keyed on game start and returns
//! each game whole. A window of Eastern dates reads UTC days `from..=to+1`
//! and keeps the games whose `games.game_date` is inside it; a midnight-UTC
//! tip comes back in both days and is taken once. `utcOffset` is not used
//! (with -5 it returned a six-hour slice). One call per UTC day.

use crate::cbbd::{CbbdClient, CbbdError, Play};
use crate::cbbd_shadow::{Coverage, write_summaries};
use crate::ingest::playbyplay::{PbpRow, points_from_tags};
use crate::source_parity::cbbd_swapped;
use anyhow::{Context, Result, bail};
use chrono::{Duration, NaiveDate};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use tracing::{info, warn};
use uuid::Uuid;

/// The first season whose plays come from CBBD. Every earlier season keeps
/// NatStat's plays, which the trained models were fitted on.
pub const FIRST_SEASON: i32 = 2027;

/// `play_by_play.source` for rows this module writes.
pub const SOURCE: &str = "cbbd";

/// Prefix of an on-floor token for a CBBD athlete with no `players` row yet;
/// the lineup builder reads it from the same constant.
pub const UNRESOLVED_PREFIX: &str = cstat_core::pbp_replay::UNRESOLVED_ONFLOOR_PREFIX;

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Read and report; write nothing.
    pub dry_run: bool,
    /// Keep each day's raw response as `plays_date_<day>.json` here, and read
    /// it back instead of calling CBBD when it exists. For development: a
    /// re-run costs no quota.
    pub cache_dir: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct PlaysReport {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub dry_run: bool,
    /// UTC days read, how many came from `cache_dir`, and how many failed.
    pub days: usize,
    pub days_cached: usize,
    pub days_failed: usize,
    /// Our games in the window from [`FIRST_SEASON`] on, and those of them
    /// with an ESPN id to join on.
    pub games_in_window: usize,
    pub games_with_espn_id: usize,
    /// Distinct CBBD games in the responses, and those that are ours in the
    /// window.
    pub cbbd_games: usize,
    pub games_matched: usize,
    /// Our games with an ESPN id that CBBD returned no plays for.
    pub games_missing: usize,
    /// What happened to each matched game.
    pub filled: usize,
    pub replaced_natstat: usize,
    pub refreshed: usize,
    pub unchanged: usize,
    pub rows_written: usize,
    pub plays: usize,
    /// Plays listing exactly five players for each side.
    pub plays_full_on_floor: usize,
    /// Plays naming an actor, and how many of those resolved to a player.
    pub actors: usize,
    pub actors_resolved: usize,
    /// On-floor player slots, and how many resolved to a player.
    pub on_floor_slots: usize,
    pub on_floor_resolved: usize,
    /// Games whose sides were oriented by ESPN's flag because the scores
    /// agreed in neither order.
    pub sides_by_flag: usize,
    pub calls_remaining: Option<i64>,
}

impl PlaysReport {
    pub fn written(&self) -> usize {
        self.filled + self.replaced_natstat + self.refreshed
    }

    pub fn notes(&self) -> String {
        format!(
            "{} games matched of {} with an ESPN id ({} missing from CBBD); {} filled, {} \
             replaced NatStat, {} refreshed, {} unchanged; {} rows; actors resolved {}, \
             on-floor resolved {}, full on-floor {}; {} days ({} cached, {} failed); \
             {} calls left this month",
            self.games_matched,
            self.games_with_espn_id,
            self.games_missing,
            self.filled,
            self.replaced_natstat,
            self.refreshed,
            self.unchanged,
            self.rows_written,
            pct(self.actors_resolved, self.actors),
            pct(self.on_floor_resolved, self.on_floor_slots),
            pct(self.plays_full_on_floor, self.plays),
            self.days,
            self.days_cached,
            self.days_failed,
            self.calls_remaining
                .map_or_else(|| "unknown".to_string(), |n| n.to_string()),
        )
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "cbbd plays {}..{}{}\n",
            self.from.map(|d| d.to_string()).unwrap_or_default(),
            self.to.map(|d| d.to_string()).unwrap_or_default(),
            if self.dry_run {
                " (dry run, nothing written)"
            } else {
                ""
            }
        );
        s += &format!(
            "  our games: {} in window (season {FIRST_SEASON}+), {} with an ESPN id\n",
            self.games_in_window, self.games_with_espn_id
        );
        s += &format!(
            "  CBBD: {} games in {} UTC days ({} from cache, {} failed); {} ours, {} of ours \
             missing\n",
            self.cbbd_games,
            self.days,
            self.days_cached,
            self.days_failed,
            self.games_matched,
            self.games_missing
        );
        s += &format!(
            "  games: {} filled (no plays before), {} replaced NatStat plays, {} refreshed, \
             {} unchanged; {} rows {}\n",
            self.filled,
            self.replaced_natstat,
            self.refreshed,
            self.unchanged,
            self.rows_written,
            if self.dry_run {
                "would be written"
            } else {
                "written"
            }
        );
        s += &format!(
            "  plays: {}; all ten on the floor on {}\n",
            self.plays,
            pct(self.plays_full_on_floor, self.plays)
        );
        s += &format!(
            "  players: actors resolved {} ({} of {}), on-floor slots resolved {} ({} of {})\n",
            pct(self.actors_resolved, self.actors),
            self.actors_resolved,
            self.actors,
            pct(self.on_floor_resolved, self.on_floor_slots),
            self.on_floor_resolved,
            self.on_floor_slots
        );
        if self.sides_by_flag > 0 {
            s += &format!(
                "  {} games oriented by ESPN's home flag (scores matched neither way)\n",
                self.sides_by_flag
            );
        }
        match self.calls_remaining {
            Some(n) => s += &format!("  {n} CBBD calls left this month\n"),
            None => s += "  no CBBD call made\n",
        }
        s
    }
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "-".to_string()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / d as f64)
    }
}

/// One of our games, as the plays are mapped onto it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GameRef {
    pub id: Uuid,
    pub season: i32,
    pub espn_id: String,
    pub home_team_id: Option<Uuid>,
    pub away_team_id: Option<Uuid>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    /// ESPN lists the game the other way round (`game_broadcasts`).
    pub espn_flip: bool,
}

/// Load CBBD play-by-play for the games on Eastern dates `from..=to`.
pub async fn run(
    pool: &PgPool,
    client: Option<&CbbdClient>,
    from: NaiveDate,
    to: NaiveDate,
    opts: &Options,
) -> Result<PlaysReport> {
    let mut report = PlaysReport {
        from: Some(from),
        to: Some(to),
        dry_run: opts.dry_run,
        ..Default::default()
    };

    let (in_window, games): (i64, Vec<GameRef>) = window_games(pool, from, to).await?;
    report.games_in_window = in_window as usize;
    report.games_with_espn_id = games.len();
    let by_event: HashMap<&str, &GameRef> = games.iter().map(|g| (g.espn_id.as_str(), g)).collect();
    if by_event.is_empty() {
        info!(%from, %to, "cbbd plays: no games with an ESPN id in the window");
        return Ok(report);
    }

    let mut seen: HashSet<i32> = HashSet::new();
    let mut matched: HashSet<Uuid> = HashSet::new();
    let mut day = from;
    while day <= to + Duration::days(1) {
        report.days += 1;
        let plays = match load_day(client, day, opts, &mut report).await {
            Ok(p) => p,
            // Out of quota or a revoked key: no later day can succeed.
            Err(e)
                if e.downcast_ref::<CbbdError>()
                    .is_some_and(|e| matches!(e, CbbdError::Refused(_))) =>
            {
                return Err(e);
            }
            Err(e) => {
                warn!(%day, error = %format!("{e:#}"), "cbbd plays: day failed; continuing");
                report.days_failed += 1;
                day += Duration::days(1);
                continue;
            }
        };
        let mut by_game: BTreeMap<i32, Vec<Play>> = BTreeMap::new();
        for p in plays {
            by_game.entry(p.game_id).or_default().push(p);
        }
        // A midnight-UTC tip is listed whole in both days: take it once.
        by_game.retain(|id, _| seen.insert(*id));
        report.cbbd_games += by_game.len();

        let coverage: BTreeMap<i32, Coverage> =
            by_game.iter().map(|(id, p)| (*id, coverage(p))).collect();
        if !opts.dry_run {
            write_summaries(pool, &coverage)
                .await
                .context("writing play coverage")?;
        }

        let ours: Vec<(&GameRef, Vec<Play>)> = by_game
            .into_values()
            .filter_map(|plays| {
                let event = plays.first()?.game_source_id.as_deref()?;
                let game = *by_event.get(event)?;
                Some((game, plays))
            })
            .collect();
        let athlete_ids: Vec<i32> = ours
            .iter()
            .flat_map(|(_, plays)| plays.iter().flat_map(athletes_on))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let seasons: Vec<i32> = ours
            .iter()
            .map(|(g, _)| g.season)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let athletes = athlete_map(pool, &athlete_ids, &seasons).await?;
        let none = HashMap::new();

        let mut built: Vec<(Uuid, Vec<PbpRow>)> = Vec::with_capacity(ours.len());
        for (game, plays) in ours {
            matched.insert(game.id);
            let b = build_game(game, plays, athletes.get(&game.season).unwrap_or(&none));
            report.plays += b.rows.len();
            report.plays_full_on_floor += b.full_on_floor;
            report.actors += b.actors;
            report.actors_resolved += b.actors_resolved;
            report.on_floor_slots += b.on_floor_slots;
            report.on_floor_resolved += b.on_floor_resolved;
            report.sides_by_flag += usize::from(b.sides_by_flag);
            if !b.rows.is_empty() {
                built.push((game.id, b.rows));
            }
        }
        write_games(pool, &built, opts.dry_run, &mut report).await?;
        day += Duration::days(1);
    }
    report.games_matched = matched.len();
    report.games_missing = games.len() - matched.len();
    report.calls_remaining = client.and_then(|c| c.calls_remaining());
    info!(?report, "cbbd plays done");
    Ok(report)
}

/// Our games on Eastern dates `from..=to` from [`FIRST_SEASON`] on: how many
/// there are, and those with an ESPN id. An ESPN id held by two games in the
/// window is left out: which one the plays belong to cannot be told.
async fn window_games(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(i64, Vec<GameRef>)> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM games WHERE game_date BETWEEN $1 AND $2 AND season >= $3",
    )
    .bind(from)
    .bind(to)
    .bind(FIRST_SEASON)
    .fetch_one(pool)
    .await?;
    let games: Vec<GameRef> = sqlx::query_as(
        "SELECT g.id, g.season, g.espn_id, g.home_team_id, g.away_team_id, \
                g.home_score, g.away_score, \
                COALESCE((SELECT bool_or(b.source_home_is_away) FROM game_broadcasts b \
                          WHERE b.game_id = g.id), false) AS espn_flip \
         FROM games g \
         WHERE g.game_date BETWEEN $1 AND $2 AND g.season >= $3 AND g.espn_id IS NOT NULL \
           AND (SELECT count(*) FROM games g2 \
                WHERE g2.espn_id = g.espn_id AND g2.game_date BETWEEN $1 AND $2) = 1",
    )
    .bind(from)
    .bind(to)
    .bind(FIRST_SEASON)
    .fetch_all(pool)
    .await
    .context("loading the window's games")?;
    Ok((total, games))
}

/// One UTC day of plays, from `cache_dir` when it holds the day, else CBBD.
async fn load_day(
    client: Option<&CbbdClient>,
    day: NaiveDate,
    opts: &Options,
    report: &mut PlaysReport,
) -> Result<Vec<Play>> {
    let cached = opts
        .cache_dir
        .as_ref()
        .map(|d| d.join(format!("plays_date_{day}.json")));
    let bytes = match cached.as_ref().filter(|p| p.is_file()) {
        Some(path) => {
            report.days_cached += 1;
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
        }
        None => {
            let Some(client) = client else {
                bail!("no cached response for {day} and CBBD_API_KEY is not set");
            };
            let bytes = client
                .get_bytes("/plays/date", &[("date", day.to_string())])
                .await?;
            // The call is spent either way: a cache that cannot be written
            // is a warning, not a lost day.
            if let Some(path) = &cached
                && let Err(e) = std::fs::write(path, &bytes)
            {
                warn!(path = %path.display(), error = %e, "cbbd plays: could not cache the day");
            }
            bytes
        }
    };
    serde_json::from_slice(&bytes).with_context(|| format!("parsing /plays/date for {day}"))
}

/// Every CBBD athlete id a game's plays name, as actor or on the floor.
fn athletes_on(p: &Play) -> impl Iterator<Item = i32> + '_ {
    p.participants
        .iter()
        .filter_map(|x| x.id)
        .chain(p.on_floor.iter().filter_map(|x| x.id))
}

/// Per season, CBBD athlete id → our player, through the ESPN athlete id the
/// shadow box scores carry and `players.espn_id`. Player rows are
/// season-scoped and an ESPN id carries across seasons, so the lookup is by the
/// game's season. An athlete that leads to two players in a season is left
/// out rather than guessed.
async fn athlete_map(
    pool: &PgPool,
    ids: &[i32],
    seasons: &[i32],
) -> Result<HashMap<i32, HashMap<i32, Uuid>>> {
    if ids.is_empty() || seasons.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i32, i32, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT p.season, c.cbbd_athlete_id, p.id \
         FROM cbbd_player_game_stats c \
         JOIN players p ON p.espn_id = c.espn_athlete_id AND p.season = ANY($2) \
         WHERE c.cbbd_athlete_id = ANY($1)",
    )
    .bind(ids)
    .bind(seasons)
    .fetch_all(pool)
    .await
    .context("resolving CBBD athletes")?;
    let mut by_season: HashMap<i32, Vec<(i32, Uuid)>> = HashMap::new();
    for (season, athlete, player) in rows {
        by_season.entry(season).or_default().push((athlete, player));
    }
    Ok(by_season
        .into_iter()
        .map(|(season, rows)| (season, unique_map(rows)))
        .collect())
}

/// Keep the keys that map to exactly one value.
fn unique_map(rows: Vec<(i32, Uuid)>) -> HashMap<i32, Uuid> {
    let mut out: HashMap<i32, Option<Uuid>> = HashMap::new();
    for (k, v) in rows {
        out.entry(k)
            .and_modify(|cur| {
                if *cur != Some(v) {
                    *cur = None;
                }
            })
            .or_insert(Some(v));
    }
    out.into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
}

fn coverage(plays: &[Play]) -> Coverage {
    let mut c = Coverage::default();
    for p in plays {
        c.plays += 1;
        if p.shooting_play {
            c.shots += 1;
            if p.shot_info.as_ref().is_some_and(|s| s.has_location()) {
                c.shots_with_location += 1;
            }
        }
        if p.on_floor.len() == 10 {
            c.plays_full_on_floor += 1;
        }
    }
    c
}

/// The NatStat tag vocabulary the compute reads (`FGA` is a two-point
/// attempt, `3FA` a three, `TO` a turnover; `pbp_replay::PossCount`), as the
/// NatStat loader's `tags_from_description` emits it for the same events.
/// Contextual tags (`paint`, `brk`, `2ch`, `offto`, `FOULED`) are not
/// derived: NatStat's current plays carry none, and a season holding them on
/// only some games would pass `pbp_lacks_context_tags` and show zeros for the
/// rest. CBBD folds an assist into its shot, so there are no `AST` rows.
pub fn tags_for(p: &Play) -> Vec<&'static str> {
    let shot = p.shot_info.as_ref();
    let range = shot.and_then(|s| s.range.as_deref());
    let made = shot.and_then(|s| s.made).unwrap_or(p.scoring_play);
    let t = p.play_type.as_str();
    if t == "Substitution" {
        return vec!["SUB"];
    }
    if t == "MadeFreeThrow" || range == Some("free_throw") {
        return if made {
            vec!["FTA", "FTM"]
        } else {
            vec!["FTA"]
        };
    }
    if p.shooting_play {
        let three = range == Some("three_pointer") || p.score_value == Some(3);
        return match (three, made) {
            (true, true) => vec!["3FA", "3FM"],
            (true, false) => vec!["3FA"],
            (false, true) => vec!["FGA", "FGM"],
            (false, false) => vec!["FGA"],
        };
    }
    match t {
        "Offensive Rebound" => vec!["ORB", "REB"],
        "Defensive Rebound" => vec!["REB"],
        "Dead Ball Rebound" => vec!["REB", "TREB"],
        "Steal" => vec!["STL"],
        "Block Shot" => vec!["BLK"],
        "Jumpball" | "End Period" | "End Game" => vec!["MISC"],
        _ if t.contains("Turnover") => vec!["TO"],
        // Coach's challenges name a foul-free review; check them first.
        _ if t.contains("Challenge") => vec![],
        _ if t.contains("Foul") => vec!["PF"],
        _ if t.to_ascii_lowercase().contains("timeout") => vec!["TIMEOUT"],
        _ => vec![],
    }
}

/// A substitution's description in the NatStat form the replay parser reads
/// (`pbp_replay::parse_sub`): `"<name> sub in"` / `"<name> sub out"`.
fn sub_description(p: &Play) -> Option<String> {
    let text = p.play_text.as_deref()?;
    let (name, dir) = match text.find(" subbing in") {
        Some(i) => (&text[..i], "in"),
        None => (&text[..text.find(" subbing out")?], "out"),
    };
    let name = p
        .participants
        .first()
        .and_then(|x| x.name.as_deref())
        .unwrap_or(name)
        .trim();
    Some(format!("{name} sub {dir}"))
}

/// A game's plays mapped onto `play_by_play` rows, with resolution counts.
#[derive(Debug, Default)]
pub struct BuiltGame {
    pub rows: Vec<PbpRow>,
    pub full_on_floor: usize,
    pub actors: usize,
    pub actors_resolved: usize,
    pub on_floor_slots: usize,
    pub on_floor_resolved: usize,
    pub sides_by_flag: bool,
}

/// Play order: period, then the clock running down, then ESPN's play id.
/// The response itself is not in game order, and neither CBBD's nor ESPN's
/// id alone is (a free throw is often numbered after the next possession).
fn play_order(p: &Play) -> (i32, i32, u128, i64) {
    (
        p.period,
        -p.seconds_remaining.unwrap_or(0),
        p.source_id
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u128::MAX),
        p.id,
    )
}

/// Map one game's plays onto `play_by_play` rows in our game's frame.
pub fn build_game(
    game: &GameRef,
    mut plays: Vec<Play>,
    athletes: &HashMap<i32, Uuid>,
) -> BuiltGame {
    plays.sort_by_key(play_order);
    let mut out = BuiltGame::default();

    // CBBD's final score in its own frame decides whether its sides are ours.
    let cbbd_final = (
        plays.iter().filter_map(|p| p.home_score).max(),
        plays.iter().filter_map(|p| p.away_score).max(),
    );
    let ours = (game.home_score, game.away_score);
    let scores_decide = matches!((ours, cbbd_final), ((Some(h), Some(a)), (Some(ch), Some(ca)))
        if (h == ch && a == ca) || (h == ca && a == ch));
    out.sides_by_flag = !scores_decide;
    let swapped = cbbd_swapped(ours, cbbd_final, game.espn_flip);

    // School name → CBBD home flag, from the plays that carry both. The
    // on-floor entries name only the school.
    let mut side_of: HashMap<&str, Option<bool>> = HashMap::new();
    for p in &plays {
        if let (Some(team), Some(home)) = (p.team.as_deref(), p.is_home_team) {
            side_of
                .entry(team)
                .and_modify(|v| {
                    if *v != Some(home) {
                        *v = None;
                    }
                })
                .or_insert(Some(home));
        }
    }
    // `true` = our home side.
    let ours_home = |cbbd_home: bool| cbbd_home != swapped;
    let team_of = |home: bool| {
        if home {
            game.home_team_id
        } else {
            game.away_team_id
        }
    };

    let (mut run_home, mut run_away) = (0, 0);
    let mut rows = Vec::with_capacity(plays.len());
    for (seq, p) in plays.iter().enumerate() {
        let home_side = p.is_home_team.map(ours_home);
        let team_id = home_side.and_then(team_of);

        let actor = p.participants.first().and_then(|x| x.id);
        let player_id = actor.and_then(|a| athletes.get(&a).copied());
        if actor.is_some() {
            out.actors += 1;
            out.actors_resolved += usize::from(player_id.is_some());
        }

        // Scores in our frame. A running score only rises: a few rows (a
        // timeout logged mid-free-throws) carry the score from a moment later,
        // and the next row would otherwise lower it.
        let (h, a) = if swapped {
            (p.away_score, p.home_score)
        } else {
            (p.home_score, p.away_score)
        };
        run_home = run_home.max(h.unwrap_or(0));
        run_away = run_away.max(a.unwrap_or(0));
        let score_diff = match home_side {
            Some(true) if team_id.is_some() => Some(run_home - run_away),
            Some(false) if team_id.is_some() => Some(run_away - run_home),
            _ => None,
        };

        // On-floor tokens per our side, sorted so a re-run writes the same
        // string.
        let (mut on_home, mut on_away): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for f in &p.on_floor {
            let (Some(id), Some(team)) = (f.id, f.team.as_deref()) else {
                continue;
            };
            let Some(Some(cbbd_home)) = side_of.get(team).copied() else {
                continue;
            };
            out.on_floor_slots += 1;
            let token = match athletes.get(&id) {
                Some(u) => {
                    out.on_floor_resolved += 1;
                    u.to_string()
                }
                None => format!("{UNRESOLVED_PREFIX}{id}"),
            };
            if ours_home(cbbd_home) {
                on_home.push(token);
            } else {
                on_away.push(token);
            }
        }
        if on_home.len() == 5 && on_away.len() == 5 {
            out.full_on_floor += 1;
        }
        let join = |mut v: Vec<String>| {
            (!v.is_empty()).then(|| {
                v.sort();
                v.join(",")
            })
        };

        let tags = tags_for(p);
        let points = points_from_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        let description = if p.play_type == "Substitution" {
            sub_description(p)
        } else {
            None
        }
        .or_else(|| {
            p.play_text
                .as_deref()
                .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        });
        let clock = p
            .seconds_remaining
            .filter(|s| *s >= 0)
            .map(|s| format!("{:02}:{:02}", s / 60, s % 60));
        rows.push(PbpRow {
            game_id: game.id,
            season: game.season,
            seq: seq as i32,
            sort_order: p.source_id.clone(),
            period: p.period,
            clock,
            team_id,
            player_id,
            description,
            scoring_play: points > 0,
            points,
            tags: tags.into_iter().map(String::from).collect(),
            score_home: Some(run_home),
            score_vis: Some(run_away),
            score_diff,
            onfloor_home: join(on_home),
            onfloor_vis: join(on_away),
        });
    }
    out.rows = rows;
    out
}

/// The comparable content of a stored or built row: every column the
/// loader writes except the ones derived from these.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RowKey {
    seq: i32,
    sort_order: Option<String>,
    period: i32,
    clock: Option<String>,
    team_id: Option<Uuid>,
    player_id: Option<Uuid>,
    description: Option<String>,
    points: i32,
    tags: Vec<String>,
    score_home: Option<i32>,
    score_vis: Option<i32>,
    score_diff: Option<i32>,
    onfloor_home: Option<String>,
    onfloor_vis: Option<String>,
}

fn row_key(r: &PbpRow) -> RowKey {
    RowKey {
        seq: r.seq,
        sort_order: r.sort_order.clone(),
        period: r.period,
        clock: r.clock.clone(),
        team_id: r.team_id,
        player_id: r.player_id,
        description: r.description.clone(),
        points: r.points,
        tags: r.tags.clone(),
        score_home: r.score_home,
        score_vis: r.score_vis,
        score_diff: r.score_diff,
        onfloor_home: r.onfloor_home.clone(),
        onfloor_vis: r.onfloor_vis.clone(),
    }
}

/// What to do with one game's built rows, given what `play_by_play` holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// No plays stored.
    Fill,
    /// NatStat's plays stored; CBBD replaces them.
    ReplaceNatstat,
    /// CBBD's plays stored and different (a late correction, or a player
    /// resolved since).
    Refresh,
    Unchanged,
}

pub fn decide(
    stored_source: Option<&str>,
    stored_cbbd: Option<&[RowKey]>,
    new: &[RowKey],
) -> Action {
    match stored_source {
        None => Action::Fill,
        Some(SOURCE) if stored_cbbd == Some(new) => Action::Unchanged,
        Some(SOURCE) => Action::Refresh,
        Some(_) => Action::ReplaceNatstat,
    }
}

/// Write the changed games: each game's rows are replaced whole, in one
/// transaction for the day's batch, with `UNNEST` arrays.
async fn write_games(
    pool: &PgPool,
    built: &[(Uuid, Vec<PbpRow>)],
    dry_run: bool,
    report: &mut PlaysReport,
) -> Result<()> {
    if built.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = built.iter().map(|(id, _)| *id).collect();
    // A game holding both sources (it should not happen) counts as NatStat's,
    // so it is replaced whole.
    let sources: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT game_id, CASE WHEN bool_and(source = $2) THEN $2 ELSE min(source) END \
         FROM play_by_play WHERE game_id = ANY($1) GROUP BY game_id",
    )
    .bind(&ids)
    .bind(SOURCE)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let cbbd_ids: Vec<Uuid> = sources
        .iter()
        .filter(|(_, s)| s.as_str() == SOURCE)
        .map(|(id, _)| *id)
        .collect();
    let mut stored: HashMap<Uuid, Vec<RowKey>> = HashMap::new();
    if !cbbd_ids.is_empty() {
        let rows: Vec<StoredRow> = sqlx::query_as(
            "SELECT game_id, seq, sort_order, period, clock, team_id, player_id, description, \
                    points, tags, score_home, score_vis, score_diff, onfloor_home, onfloor_vis \
             FROM play_by_play WHERE game_id = ANY($1) ORDER BY game_id, seq",
        )
        .bind(&cbbd_ids)
        .fetch_all(pool)
        .await?;
        for r in rows {
            stored.entry(r.game_id).or_default().push(r.key);
        }
    }

    let mut changed: Vec<&(Uuid, Vec<PbpRow>)> = Vec::new();
    for g in built {
        let new: Vec<RowKey> = g.1.iter().map(row_key).collect();
        match decide(
            sources.get(&g.0).map(String::as_str),
            stored.get(&g.0).map(Vec::as_slice),
            &new,
        ) {
            Action::Fill => report.filled += 1,
            Action::ReplaceNatstat => report.replaced_natstat += 1,
            Action::Refresh => report.refreshed += 1,
            Action::Unchanged => {
                report.unchanged += 1;
                continue;
            }
        }
        changed.push(g);
    }
    let rows: Vec<&PbpRow> = changed.iter().flat_map(|(_, r)| r.iter()).collect();
    report.rows_written += rows.len();
    if dry_run || changed.is_empty() {
        return Ok(());
    }
    let changed_ids: Vec<Uuid> = changed.iter().map(|(id, _)| *id).collect();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM play_by_play WHERE game_id = ANY($1)")
        .bind(&changed_ids)
        .execute(&mut *tx)
        .await?;
    // Tags travel as one `|`-joined string per row: UNNEST cannot take a
    // ragged two-dimensional array.
    sqlx::query(
        "INSERT INTO play_by_play (game_id, season, seq, sort_order, period, clock, team_id, \
            player_id, description, scoring_play, points, tags, score_home, score_vis, \
            score_diff, onfloor_home, onfloor_vis, source) \
         SELECT g, s, q, so, pe, c, t, pl, d, sp, pt, \
                COALESCE(string_to_array(NULLIF(tg, ''), '|'), '{}'::text[]), \
                sh, sv, sd, oh, ov, $18 \
         FROM UNNEST($1::uuid[], $2::int[], $3::int[], $4::text[], $5::int[], $6::text[], \
            $7::uuid[], $8::uuid[], $9::text[], $10::bool[], $11::int[], $12::text[], \
            $13::int[], $14::int[], $15::int[], $16::text[], $17::text[]) \
            AS u(g, s, q, so, pe, c, t, pl, d, sp, pt, tg, sh, sv, sd, oh, ov)",
    )
    .bind(rows.iter().map(|r| r.game_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.season).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.seq).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|r| r.sort_order.clone())
            .collect::<Vec<_>>(),
    )
    .bind(rows.iter().map(|r| r.period).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.clock.clone()).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.team_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.player_id).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|r| r.description.clone())
            .collect::<Vec<_>>(),
    )
    .bind(rows.iter().map(|r| r.scoring_play).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.points).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.tags.join("|")).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.score_home).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.score_vis).collect::<Vec<_>>())
    .bind(rows.iter().map(|r| r.score_diff).collect::<Vec<_>>())
    .bind(
        rows.iter()
            .map(|r| r.onfloor_home.clone())
            .collect::<Vec<_>>(),
    )
    .bind(
        rows.iter()
            .map(|r| r.onfloor_vis.clone())
            .collect::<Vec<_>>(),
    )
    .bind(SOURCE)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct StoredRow {
    game_id: Uuid,
    #[sqlx(flatten)]
    key: RowKey,
}

/// Those of `game_ids` holding CBBD plays. The NatStat loaders skip them, so
/// NatStat's plays never replace or join CBBD's.
pub async fn cbbd_games(pool: &PgPool, game_ids: &[Uuid]) -> Result<HashSet<Uuid>, sqlx::Error> {
    if game_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT game_id FROM play_by_play WHERE game_id = ANY($1) AND source = $2",
    )
    .bind(game_ids)
    .bind(SOURCE)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> GameRef {
        GameRef {
            id: Uuid::from_u128(1),
            season: 2027,
            espn_id: "401".into(),
            home_team_id: Some(Uuid::from_u128(10)),
            away_team_id: Some(Uuid::from_u128(20)),
            home_score: Some(4),
            away_score: Some(3),
            espn_flip: false,
        }
    }

    /// Five CBBD athletes per side, `base..base+5`, with school names.
    fn floor(home: &str, hb: i32, away: &str, ab: i32) -> serde_json::Value {
        let mut v: Vec<serde_json::Value> = (hb..hb + 5)
            .map(|i| serde_json::json!({"id": i, "name": format!("H{i}"), "team": home}))
            .collect();
        v.extend(
            (ab..ab + 5)
                .map(|i| serde_json::json!({"id": i, "name": format!("A{i}"), "team": away})),
        );
        serde_json::Value::Array(v)
    }

    fn plays(json: serde_json::Value) -> Vec<Play> {
        serde_json::from_value(json).unwrap()
    }

    /// A short game as CBBD lists it: out of order, with CBBD's home team
    /// "Road U" being our AWAY team (CBBD's sides are reversed; the scores
    /// say so: our 4-3 home win is CBBD's 3-4). The timeout is logged with
    /// the final score and the turnover after it with an older one.
    fn swapped_game() -> Vec<Play> {
        let f1 = floor("Road U", 100, "Home St", 200);
        let f2 = floor("Road U", 101, "Home St", 200); // 100 out, 105 in
        plays(serde_json::json!([
            {"gameId": 9, "gameSourceId": "401", "id": 3, "sourceId": "401000003",
             "playType": "JumpShot", "isHomeTeam": false, "team": "Home St",
             "homeScore": 0, "awayScore": 3, "period": 1, "secondsRemaining": 1100,
             "scoringPlay": true, "shootingPlay": true, "scoreValue": 3,
             "playText": "A200 makes three point jumper (A201 assists)",
             "participants": [{"id": 200, "name": "A200"}, {"id": 201, "name": "A201"}],
             "shotInfo": {"made": true, "range": "three_pointer"}, "onFloor": f1},
            {"gameId": 9, "gameSourceId": "401", "id": 1, "sourceId": "401000001",
             "playType": "Jumpball", "isHomeTeam": null, "team": null,
             "homeScore": 0, "awayScore": 0, "period": 1, "secondsRemaining": 1200,
             "playText": "Start game", "participants": [], "shotInfo": null, "onFloor": f1},
            {"gameId": 9, "gameSourceId": "401", "id": 4, "sourceId": "401000004",
             "playType": "Substitution", "isHomeTeam": true, "team": "Road U",
             "homeScore": 0, "awayScore": 3, "period": 1, "secondsRemaining": 1000,
             "playText": "H105 subbing in for Road U",
             "participants": [{"id": 105, "name": "H105"}], "onFloor": f1},
            {"gameId": 9, "gameSourceId": "401", "id": 5, "sourceId": "401000005",
             "playType": "Substitution", "isHomeTeam": true, "team": "Road U",
             "homeScore": 0, "awayScore": 3, "period": 1, "secondsRemaining": 1000,
             "playText": "H100 subbing out for Road U",
             "participants": [{"id": 100, "name": "H100"}], "onFloor": f1},
            {"gameId": 9, "gameSourceId": "401", "id": 6, "sourceId": "401000006",
             "playType": "MadeFreeThrow", "isHomeTeam": true, "team": "Road U",
             "homeScore": 1, "awayScore": 3, "period": 1, "secondsRemaining": 1000,
             "scoringPlay": true, "shootingPlay": true, "scoreValue": 1,
             "playText": "H105 makes free throw 1 of 2",
             "participants": [{"id": 105, "name": "H105"}],
             "shotInfo": {"made": true, "range": "free_throw"}, "onFloor": f2},
            {"gameId": 9, "gameSourceId": "401", "id": 7, "sourceId": "401000007",
             "playType": "MadeFreeThrow", "isHomeTeam": true, "team": "Road U",
             "homeScore": 1, "awayScore": 3, "period": 1, "secondsRemaining": 1000,
             "scoringPlay": false, "shootingPlay": true, "scoreValue": 1,
             "playText": "H105 misses free throw 2 of 2",
             "participants": [{"id": 105, "name": "H105"}],
             "shotInfo": {"made": false, "range": "free_throw"}, "onFloor": f2},
            {"gameId": 9, "gameSourceId": "401", "id": 8, "sourceId": "401000008",
             "playType": "Offensive Rebound", "isHomeTeam": true, "team": "Road U",
             "homeScore": 1, "awayScore": 3, "period": 1, "secondsRemaining": 998,
             "playText": "Road U Offensive Rebound.", "participants": [], "onFloor": f2},
            {"gameId": 9, "gameSourceId": "401", "id": 2, "sourceId": "401000009",
             "playType": "LayUpShot", "isHomeTeam": true, "team": "Road U",
             "homeScore": 3, "awayScore": 3, "period": 1, "secondsRemaining": 990,
             "scoringPlay": true, "shootingPlay": true, "scoreValue": 2,
             "playText": "H101 makes layup", "participants": [{"id": 101, "name": "H101"}],
             "shotInfo": {"made": true, "range": "rim"}, "onFloor": f2},
            {"gameId": 9, "gameSourceId": "401", "id": 10, "sourceId": "401000010",
             "playType": "OfficialTVTimeOut", "isHomeTeam": null, "team": null,
             "homeScore": 3, "awayScore": 4, "period": 1, "secondsRemaining": 990,
             "playText": "Official TV Timeout", "participants": [], "onFloor": f2},
            {"gameId": 9, "gameSourceId": "401", "id": 11, "sourceId": "401000011",
             "playType": "Lost Ball Turnover", "isHomeTeam": false, "team": "Home St",
             "homeScore": 3, "awayScore": 3, "period": 2, "secondsRemaining": 5,
             "playText": "A202 bad pass\nturnover",
             "participants": [{"id": 202, "name": "A202"}], "onFloor": f2}
        ]))
    }

    fn athletes() -> HashMap<i32, Uuid> {
        // Every athlete resolves except 104 (Road U) and 204 (Home St).
        (100..106)
            .chain(200..206)
            .filter(|i| *i != 104 && *i != 204)
            .map(|i| (i, Uuid::from_u128(i as u128)))
            .collect()
    }

    #[test]
    fn plays_map_onto_our_frame_in_game_order() {
        let b = build_game(&game(), swapped_game(), &athletes());
        let order: Vec<&str> = b
            .rows
            .iter()
            .map(|r| r.sort_order.as_deref().unwrap())
            .collect();
        assert_eq!(
            order,
            [
                "401000001",
                "401000003",
                "401000004",
                "401000005",
                "401000006",
                "401000007",
                "401000008",
                "401000009",
                "401000010",
                "401000011"
            ]
        );
        assert!(!b.sides_by_flag, "the scores decide the orientation");
        let (home, away) = (Uuid::from_u128(10), Uuid::from_u128(20));
        // CBBD's away "Home St" three is our home team's.
        let three = &b.rows[1];
        assert_eq!(three.team_id, Some(home));
        assert_eq!(three.tags, ["3FA", "3FM"]);
        assert_eq!((three.points, three.scoring_play), (3, true));
        assert_eq!(three.player_id, Some(Uuid::from_u128(200)));
        assert_eq!((three.score_home, three.score_vis), (Some(3), Some(0)));
        assert_eq!(three.score_diff, Some(3));
        assert_eq!(three.clock.as_deref(), Some("18:20"));
        // Substitutions read as NatStat's, for the replay fallback.
        assert_eq!(b.rows[2].description.as_deref(), Some("H105 sub in"));
        assert_eq!(b.rows[3].description.as_deref(), Some("H100 sub out"));
        assert_eq!(b.rows[2].tags, ["SUB"]);
        assert_eq!(b.rows[2].team_id, Some(away));
        // Free throws: made and missed.
        assert_eq!(b.rows[4].tags, ["FTA", "FTM"]);
        assert_eq!(b.rows[5].tags, ["FTA"]);
        assert_eq!(b.rows[5].points, 0);
        // A team rebound has a team and no player.
        assert_eq!(b.rows[6].tags, ["ORB", "REB"]);
        assert_eq!((b.rows[6].team_id, b.rows[6].player_id), (Some(away), None));
        // The timeout logged with a later score raises the running score; a
        // following row with the older score cannot lower it.
        assert_eq!(
            (b.rows[8].score_home, b.rows[8].score_vis),
            (Some(4), Some(3))
        );
        assert_eq!(b.rows[8].team_id, None);
        assert_eq!(b.rows[8].tags, ["TIMEOUT"]);
        let last = b.rows.last().unwrap();
        assert_eq!((last.score_home, last.score_vis), (Some(4), Some(3)));
        assert_eq!(last.tags, ["TO"]);
        assert_eq!(last.description.as_deref(), Some("A202 bad pass turnover"));
        assert_eq!(last.period, 2);
        assert_eq!(
            b.rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            (0..10).collect::<Vec<_>>()
        );
    }

    #[test]
    fn on_floor_players_resolve_by_id_and_the_unknown_keep_a_token() {
        let b = build_game(&game(), swapped_game(), &athletes());
        let first = &b.rows[0];
        // Our home side is CBBD's "Home St": 200..205, 204 unresolved.
        let home: Vec<&str> = first.onfloor_home.as_deref().unwrap().split(',').collect();
        assert_eq!(home.len(), 5);
        assert!(home.contains(&"cbbd:204"));
        assert!(home.contains(&Uuid::from_u128(200).to_string().as_str()));
        let away: Vec<&str> = first.onfloor_vis.as_deref().unwrap().split(',').collect();
        assert!(away.contains(&"cbbd:104"));
        assert!(away.contains(&Uuid::from_u128(100).to_string().as_str()));
        // After the substitution, 100 is off and 105 on.
        let after = b.rows[4].onfloor_vis.as_deref().unwrap();
        assert!(!after.contains(&Uuid::from_u128(100).to_string()));
        assert!(after.contains(&Uuid::from_u128(105).to_string()));
        assert_eq!(b.full_on_floor, 10);
        assert_eq!(b.on_floor_slots, 100);
        assert_eq!(b.on_floor_resolved, 80);
        // Actors: every play naming one; none of them is 104 or 204.
        assert_eq!((b.actors, b.actors_resolved), (7, 7));
    }

    #[test]
    fn an_unknown_participant_stays_null_rather_than_guessed() {
        let mut a = athletes();
        a.remove(&200);
        let b = build_game(&game(), swapped_game(), &a);
        assert_eq!(b.rows[1].player_id, None);
        assert_eq!(b.actors_resolved, b.actors - 1);
    }

    #[test]
    fn unmatched_scores_fall_back_to_espns_flag() {
        let mut g = game();
        g.home_score = None;
        let b = build_game(&g, swapped_game(), &athletes());
        assert!(b.sides_by_flag);
        // Without the flag CBBD's sides are taken as ours.
        assert_eq!(b.rows[1].team_id, Some(Uuid::from_u128(20)));
        g.espn_flip = true;
        let b = build_game(&g, swapped_game(), &athletes());
        assert_eq!(b.rows[1].team_id, Some(Uuid::from_u128(10)));
    }

    #[test]
    fn a_game_listed_in_two_days_is_taken_once() {
        // `run` keeps a game only the first time its CBBD id is seen; the
        // retain below is the same expression.
        let mut seen: HashSet<i32> = HashSet::new();
        let day1: BTreeMap<i32, ()> = [(1, ()), (2, ())].into_iter().collect();
        let day2: BTreeMap<i32, ()> = [(2, ()), (3, ())].into_iter().collect();
        let mut taken = vec![];
        for mut day in [day1, day2] {
            day.retain(|id, _| seen.insert(*id));
            taken.extend(day.into_keys());
        }
        assert_eq!(taken, [1, 2, 3]);
    }

    #[test]
    fn natstat_plays_are_replaced_and_cbbd_plays_only_when_they_change() {
        let b = build_game(&game(), swapped_game(), &athletes());
        let new: Vec<RowKey> = b.rows.iter().map(row_key).collect();
        assert_eq!(decide(None, None, &new), Action::Fill);
        assert_eq!(decide(Some("natstat"), None, &new), Action::ReplaceNatstat);
        assert_eq!(decide(Some(SOURCE), Some(&new), &new), Action::Unchanged);
        let mut old = new.clone();
        old[1].player_id = None; // a player resolved since the last run
        assert_eq!(decide(Some(SOURCE), Some(&old), &new), Action::Refresh);
    }

    #[test]
    fn tags_follow_the_natstat_vocabulary() {
        let p = |t: &str, extra: serde_json::Value| -> Play {
            let mut v = serde_json::json!({"gameId": 1, "id": 1, "playType": t, "period": 1});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(v).unwrap()
        };
        let none = serde_json::json!({});
        assert_eq!(
            tags_for(&p(
                "JumpShot",
                serde_json::json!({"shootingPlay": true, "scoreValue": 2,
                    "shotInfo": {"made": false, "range": "jumper"}})
            )),
            ["FGA"]
        );
        assert_eq!(
            tags_for(&p(
                "DunkShot",
                serde_json::json!({"shootingPlay": true, "scoringPlay": true, "scoreValue": 2,
                    "shotInfo": {"made": true, "range": "rim"}})
            )),
            ["FGA", "FGM"]
        );
        assert_eq!(
            tags_for(&p("Dead Ball Rebound", none.clone())),
            ["REB", "TREB"]
        );
        assert_eq!(tags_for(&p("Defensive Rebound", none.clone())), ["REB"]);
        assert_eq!(tags_for(&p("Steal", none.clone())), ["STL"]);
        assert_eq!(tags_for(&p("Block Shot", none.clone())), ["BLK"]);
        assert_eq!(tags_for(&p("PersonalFoul", none.clone())), ["PF"]);
        assert_eq!(tags_for(&p("Technical Foul", none.clone())), ["PF"]);
        assert_eq!(tags_for(&p("ShortTimeOut", none.clone())), ["TIMEOUT"]);
        assert_eq!(tags_for(&p("RegularTimeOut", none.clone())), ["TIMEOUT"]);
        assert_eq!(tags_for(&p("End Period", none.clone())), ["MISC"]);
        assert!(tags_for(&p("Coach's Challenge (Stands)", none.clone())).is_empty());
    }

    #[test]
    fn an_athlete_leading_to_two_players_is_dropped() {
        let m = unique_map(vec![
            (1, Uuid::from_u128(1)),
            (2, Uuid::from_u128(2)),
            (2, Uuid::from_u128(3)),
            (1, Uuid::from_u128(1)),
        ]);
        assert_eq!(m.get(&1), Some(&Uuid::from_u128(1)));
        assert_eq!(m.get(&2), None);
    }
}
