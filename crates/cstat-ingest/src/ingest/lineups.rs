//! NatStat per-game `lineups`-object capture — the Tier-2 cross-season
//! lineup-membership source (docs/pbp_utilization_scope.md).
//!
//! Fetches the `games;lineups` hydrate one game at a time and persists the
//! server-computed 5-man units into the durable `natstat_lineups` /
//! `natstat_lineup_games` capture tables (migration 037). The tables are the
//! source of truth: compute derives from them and never regenerates them, so
//! the ~130-hr 12-season backfill is spent exactly once.
//!
//! De-risk findings (2026-06-10) baked into this loader:
//! - Units carry NatStat player codes, but the code SERIES does not always
//!   match the one our box ingest stored (NatStat re-issues player ids — a
//!   game's lineup codes can be an entirely different block from its box
//!   codes). Resolution is therefore **two-tier and game-scoped**: exact code
//!   match against the game's box roster first, then abbreviated-name match
//!   (`lineupplayers`, `"F. Abee · J. Banks · …"`) against the same game-team
//!   roster. Ambiguous abbreviations (1-2.7% of team-games have a colliding
//!   initial+lastname pair) stay NULL rather than guessing.
//! - The hydrate must be **lineups-only**: `games;playbyplay,lineups`
//!   returns HTTP 500 for 2026 games, `games;lineups` works for all seasons.
//! - **v4-only**: api3 does not support per-game hydrates (302-redirects
//!   away), so this loader never falls back to v3 ([`NatStatClient::get_v4_only`]).
//! - Not every game has the object (~12% absent in the de-risk sample); those
//!   are recorded as `status='no_lineups'` so reruns skip them for free.

use crate::NatStatClient;
use crate::client::NatStatError;
use crate::ingest::playbyplay::team_abbrev_map;
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};
use uuid::Uuid;

/// Cache TTL for the hydrate responses. The durable `natstat_lineups` table
/// is the real store — once a game persists, its cache row is redundant — so
/// this only needs to cover short-horizon re-runs (dev iteration, a resolver
/// fix re-applied to recent fetches). Kept deliberately short: `clean-cache`
/// deletes only *expired* rows, and a season's hydrates are ~GBs of JSONB
/// that shouldn't be pinned for months.
const LINEUPS_TTL_SECS: i64 = 7 * 24 * 3600;

/// Progress log cadence (games).
const LOG_EVERY: u64 = 100;

#[derive(Debug, Default)]
pub struct LineupsReport {
    /// Games fetched and persisted this run (any status).
    pub games_fetched: u64,
    /// Games skipped because the ledger already covers them.
    pub games_skipped: u64,
    /// Subset of `games_fetched` where the API has no lineups object.
    pub games_no_lineups: u64,
    /// Games that errored (recorded in the ledger; rerun with --retry-errors).
    pub games_errored: u64,
    pub units: u64,
    pub player_slots: u64,
    /// Slots resolved by exact code match against the game's box roster.
    pub resolved_by_code: u64,
    /// Slots resolved by abbreviated-name fallback.
    pub resolved_by_name: u64,
    /// Slots left NULL (no match, or an ambiguous abbreviation).
    pub unresolved_slots: u64,
    /// Units whose `team-code` was wrong in the feed — the opposing game
    /// roster resolved strictly more slots, so attribution was flipped
    /// (observed live: NatStat swaps the two team codes on some games).
    pub team_swapped_units: u64,
}

impl std::fmt::Display for LineupsReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lineups: fetched={} skipped={} no_lineups={} errored={} units={} slots={} by_code={} by_name={} unresolved={} team_swapped={}",
            self.games_fetched,
            self.games_skipped,
            self.games_no_lineups,
            self.games_errored,
            self.units,
            self.player_slots,
            self.resolved_by_code,
            self.resolved_by_name,
            self.unresolved_slots,
            self.team_swapped_units
        )
    }
}

/// One parsed 5-man unit, ready to resolve and insert.
struct UnitRow {
    natstat_lineup_id: String,
    team_code: Option<String>,
    player_codes: Vec<String>,
    /// Abbreviated display names from `lineupplayers`, aligned with
    /// `player_codes` by slot order; may be empty when the field is absent.
    player_names: Vec<String>,
    possessions: Option<f32>,
    points: Option<i32>,
    points_d: Option<i32>,
    plusminus: Option<i32>,
    raw: Value,
}

/// One box-roster entry of a (game, team), pre-normalized for matching.
struct RosterSlot {
    /// `players.natstat_id` — the box-era player code.
    code: String,
    /// Lowercased first initial.
    initial: Option<char>,
    /// Suffix-stripped tokens after the first name, joined (`"van dyke"`).
    rest: String,
    /// Last token of `rest` (`"dyke"`).
    last: String,
    id: Uuid,
}

/// Capture the lineups object for every Final game of a season not already in
/// the ledger. Restart-safe: the ledger is the done-set. `limit` bounds the
/// number of API fetches this run (budget control); `retry_errors` re-attempts
/// games previously recorded as `status='error'`.
///
/// `window` scopes the candidate games to `game_date BETWEEN from AND to`
/// (`YYYY-MM-DD`). The nightly passes its ingest window so the sweep stays
/// bounded to the night's games — without it, the FIRST nightly against a
/// season whose `natstat_lineup_games` ledger is empty (e.g. prod, which never
/// receives this local-only table via `sync_to_prod.sh`) would try to backfill
/// the ENTIRE season's lineups in one run. `None` restores the full-season
/// sweep, which is what the `lineups` CLI subcommand (backfill) wants.
/// Ordering is `game_date` ascending, so a windowed+limited run processes the
/// oldest un-covered games in the window first.
pub async fn ingest_lineups_for_season(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    window: Option<(&str, &str)>,
    limit: Option<u64>,
    retry_errors: bool,
) -> Result<LineupsReport, NatStatError> {
    let done: HashSet<Uuid> = {
        let exclude_errors = if retry_errors {
            " AND status <> 'error'"
        } else {
            ""
        };
        let q =
            format!("SELECT game_id FROM natstat_lineup_games WHERE season = $1{exclude_errors}");
        sqlx::query_as::<_, (Uuid,)>(&q)
            .bind(season)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(g,)| g)
            .collect()
    };

    // Date-window filter is optional: `$2::date`/`$3::date` are only referenced
    // when `window` is Some, and bound in the same branch, so the placeholder
    // numbering stays consistent with the binds.
    let date_filter = if window.is_some() {
        " AND game_date BETWEEN $2::date AND $3::date"
    } else {
        ""
    };
    let games_sql = format!(
        "SELECT id, natstat_id, home_team_id, away_team_id FROM games \
         WHERE season = $1 AND status = 'Final' AND natstat_id IS NOT NULL{date_filter} \
         ORDER BY game_date, natstat_id"
    );
    let mut games_q = sqlx::query_as::<_, (Uuid, String, Option<Uuid>, Option<Uuid>)>(&games_sql)
        .bind(season);
    if let Some((from, to)) = window {
        games_q = games_q.bind(from).bind(to);
    }
    let games: Vec<(Uuid, String, Option<Uuid>, Option<Uuid>)> =
        games_q.fetch_all(pool).await?;

    let teams = team_abbrev_map(pool, season).await?;
    let rosters = game_rosters(pool, season).await?;

    let mut report = LineupsReport::default();
    let total = games.len() as u64;

    for (game_id, gamecode, home_team, away_team) in games {
        if done.contains(&game_id) {
            report.games_skipped += 1;
            continue;
        }
        if let Some(max) = limit
            && report.games_fetched + report.games_errored >= max
        {
            info!(season, max, "lineups: fetch limit reached — stopping");
            break;
        }

        match fetch_game_lineups(client, &gamecode).await {
            Ok(units) => {
                persist_game(
                    pool,
                    game_id,
                    season,
                    &units,
                    (home_team, away_team),
                    &teams,
                    &rosters,
                    &mut report,
                )
                .await?;
                report.games_fetched += 1;
                if units.is_empty() {
                    report.games_no_lineups += 1;
                }
            }
            // An exhausted call budget is account-wide, not per-game: abort the
            // sweep cleanly (ledger intact, resumable) instead of churning
            // through every remaining game recording bogus 'error' rows.
            Err(e) if is_rate_limit_error(&e) => {
                tracing::error!(season, gamecode, error = %e, "lineups: rate limit hit — aborting sweep (resume later; ledger is intact)");
                return Err(e);
            }
            Err(e) => {
                // Other per-game errors must not kill a multi-hour sweep:
                // record in the ledger (so the default rerun skips it) and
                // keep going.
                warn!(season, gamecode, error = %e, "lineups: game errored — recorded, continuing");
                record_status(pool, game_id, season, "error", 0).await?;
                report.games_errored += 1;
            }
        }

        let processed = report.games_fetched + report.games_errored;
        if processed % LOG_EVERY == 0 && processed > 0 {
            info!(
                season,
                done = report.games_skipped + processed,
                total,
                %report,
                "lineups: progress"
            );
        }
    }

    info!(season, %report, "lineups capture finished");
    Ok(report)
}

/// Rate-limit-shaped errors that should abort a sweep rather than be recorded
/// per game. NatStat signals exhaustion as `OUT_OF_CALLS` (API-level) or
/// HTTP 429 (the client retries 429 with backoff first, so reaching here
/// means the budget is genuinely gone).
fn is_rate_limit_error(e: &NatStatError) -> bool {
    match e {
        NatStatError::ApiError { code, .. } => {
            let c = code.to_uppercase();
            c.contains("OUT_OF_CALLS") || c.contains("RATE")
        }
        NatStatError::HttpStatus { status, .. } => *status == 429,
        _ => false,
    }
}

/// Fetch one game's hydrate and parse its lineup units. An absent lineups
/// object (or a NO_DATA response) is a normal outcome, returned as empty.
async fn fetch_game_lineups(
    client: &NatStatClient,
    gamecode: &str,
) -> Result<Vec<UnitRow>, NatStatError> {
    let response = match client
        .get_v4_only(
            "games;lineups",
            Some(gamecode),
            None,
            Some(LINEUPS_TTL_SECS),
        )
        .await
    {
        Ok(r) => r,
        Err(NatStatError::ApiError { code, .. }) if code == "NO_DATA" => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let Some(game) = response
        .get("games")
        .and_then(|g| g.as_object())
        .and_then(|g| g.values().next())
    else {
        return Ok(Vec::new());
    };
    let Some(lineups) = game.get("lineups").and_then(|l| l.as_object()) else {
        return Ok(Vec::new());
    };

    Ok(lineups
        .iter()
        .filter_map(|(key, unit)| parse_unit(key, unit))
        .collect())
}

/// Parse one `lineup_<id>` entry. Numeric fields arrive as strings
/// (`"8"`, `"2.250"`, `"-3"`). Player slots are `players.player-1..player-5`,
/// each `{code, player}`; the per-slot `player` name is empty for most
/// historical seasons, so the display string `lineupplayers` (slot-ordered,
/// `·`-separated) is parsed as the name source instead.
fn parse_unit(key: &str, unit: &Value) -> Option<UnitRow> {
    let natstat_lineup_id = unit
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| key.trim_start_matches("lineup_").to_string());
    if natstat_lineup_id.is_empty() {
        return None;
    }

    let players_obj = unit.get("players").and_then(|p| p.as_object());
    let mut player_codes: Vec<String> = Vec::with_capacity(5);
    if let Some(slots) = players_obj {
        // Slot keys are player-1..player-5; sort for a stable unit order.
        let mut keys: Vec<&String> = slots.keys().collect();
        keys.sort();
        for k in keys {
            if let Some(code) = slots[k].get("code").and_then(|c| c.as_str())
                && !code.is_empty()
            {
                player_codes.push(code.to_string());
            }
        }
    }
    if player_codes.is_empty() {
        return None;
    }

    let player_names: Vec<String> = unit
        .get("lineupplayers")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split('·')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    Some(UnitRow {
        natstat_lineup_id,
        team_code: unit
            .get("team-code")
            .and_then(|v| v.as_str())
            .map(String::from),
        player_codes,
        player_names,
        possessions: str_num::<f32>(unit, "possessions"),
        points: str_num::<i32>(unit, "points"),
        points_d: str_num::<i32>(unit, "points-d"),
        plusminus: str_num::<i32>(unit, "plusminus"),
        raw: unit.clone(),
    })
}

/// NatStat numeric-as-string field (`"8"`, `"2.250"`, `"-3"`); tolerates a
/// genuine JSON number too.
fn str_num<T: std::str::FromStr>(unit: &Value, field: &str) -> Option<T> {
    let v = unit.get(field)?;
    match v {
        Value::String(s) => s.trim().parse().ok(),
        other => other.to_string().parse().ok(),
    }
}

/// `(game_id, team_id) -> box roster` for the season, pre-normalized for the
/// two-tier slot resolution. ~110k rows/season; loaded once per run.
async fn game_rosters(
    pool: &PgPool,
    season: i32,
) -> Result<HashMap<(Uuid, Uuid), Vec<RosterSlot>>, NatStatError> {
    let rows: Vec<(Uuid, Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT pgs.game_id, pgs.team_id, p.natstat_id, p.name, pgs.player_id \
         FROM player_game_stats pgs JOIN players p ON p.id = pgs.player_id \
         WHERE pgs.season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    let mut map: HashMap<(Uuid, Uuid), Vec<RosterSlot>> = HashMap::new();
    for (game_id, team_id, code, name, player_id) in rows {
        let (initial, rest, last) = name_parts(&name);
        map.entry((game_id, team_id)).or_default().push(RosterSlot {
            code,
            initial,
            rest,
            last,
            id: player_id,
        });
    }
    Ok(map)
}

/// Suffix tokens dropped from both abbreviation and full name before matching.
fn is_suffix(token: &str) -> bool {
    matches!(
        token.trim_end_matches('.'),
        "jr" | "sr" | "ii" | "iii" | "iv" | "v"
    )
}

/// Normalize a full box-score name into (first initial, rest-of-name, last
/// token), lowercased and suffix-stripped. `"Mike Mitchell Jr."` ->
/// `('m', "mitchell", "mitchell")`; `"Jake Van Dyke"` -> `('j', "van dyke", "dyke")`.
///
/// The first token is exempt from suffix filtering: an initial like `"V."`
/// (V.J. Edgecombe's `"V. Edgecombe"`) would otherwise be eaten by the
/// Roman-numeral rule and lose the name entirely.
fn name_parts(name: &str) -> (Option<char>, String, String) {
    let lower = name.to_lowercase();
    let mut iter = lower.split_whitespace();
    let first = iter.next();
    let initial = first.and_then(|t| t.chars().next());
    let rest_tokens: Vec<&str> = iter.filter(|t| !is_suffix(t)).collect();
    let rest = rest_tokens.join(" ");
    let last = rest_tokens
        .last()
        .map(|t| t.to_string())
        .unwrap_or_default();
    (initial, rest, last)
}

/// Normalize an abbreviated display name (`"F. Abee"`, `"M. Mitchell Jr."`)
/// into (initial, rest, last token). Same first-token exemption as
/// [`name_parts`].
fn abbrev_parts(abbrev: &str) -> (Option<char>, String, String) {
    name_parts(abbrev)
}

/// Two-tier slot resolution against the game-team box roster: exact code
/// match first (safe — game-scoped, so a cross-era code can't false-positive),
/// then the abbreviated name, accepted only when it matches exactly one box
/// player. Returns the tier used for report bookkeeping.
fn resolve_slot(
    code: &str,
    abbrev: Option<&str>,
    roster: &[RosterSlot],
) -> (Option<Uuid>, ResolvedBy) {
    if let Some(slot) = roster.iter().find(|s| s.code == code) {
        return (Some(slot.id), ResolvedBy::Code);
    }
    if let Some(a) = abbrev {
        let (initial, rest, last) = abbrev_parts(a);
        if initial.is_some() && !last.is_empty() {
            let matches: Vec<&RosterSlot> = roster
                .iter()
                .filter(|s| {
                    s.initial == initial && (s.rest == rest || (!rest.is_empty() && s.last == last))
                })
                .collect();
            if let [only] = matches.as_slice() {
                return (Some(only.id), ResolvedBy::Name);
            }
        }
    }
    (None, ResolvedBy::None)
}

enum ResolvedBy {
    Code,
    Name,
    None,
}

/// A unit flips to the *other* team only when that roster resolves a clear
/// majority of its five slots, not merely strictly more. Without the floor, a
/// game whose coded-team box roster is missing entirely (observed: Bellarmine
/// 2022 has box rows for only one side) would flip units to the opponent off a
/// single coincidental name match (1 > 0).
const SWAP_MIN_RESOLVED: usize = 3;

fn alternate_overturns(primary: usize, alternate: usize) -> bool {
    alternate > primary && alternate >= SWAP_MIN_RESOLVED
}

/// Resolve every slot of a unit against one candidate roster.
fn resolve_unit(u: &UnitRow, roster: &[RosterSlot]) -> Vec<(Option<Uuid>, ResolvedBy)> {
    u.player_codes
        .iter()
        .enumerate()
        .map(|(i, code)| {
            let abbrev = u.player_names.get(i).map(String::as_str);
            resolve_slot(code, abbrev, roster)
        })
        .collect()
}

/// Persist one game's units + ledger row in a single transaction. Delete-then-
/// insert per game (units for a finished game are immutable, same idempotency
/// unit as `replace_game_pbp`).
///
/// Each unit is resolved against BOTH game rosters and attributed to whichever
/// resolves strictly more slots (ties keep the feed's `team-code`). NatStat
/// swaps the two team codes on some games (observed live on a 2025 game where
/// every FAU unit listed Indiana State players and vice versa), so the feed's
/// attribution is a hint, not ground truth.
#[allow(clippy::too_many_arguments)]
async fn persist_game(
    pool: &PgPool,
    game_id: Uuid,
    season: i32,
    units: &[UnitRow],
    (home_team, away_team): (Option<Uuid>, Option<Uuid>),
    teams: &HashMap<String, Uuid>,
    rosters: &HashMap<(Uuid, Uuid), Vec<RosterSlot>>,
    report: &mut LineupsReport,
) -> Result<(), NatStatError> {
    let empty: Vec<RosterSlot> = Vec::new();
    let roster_of = |t: Option<Uuid>| -> &Vec<RosterSlot> {
        t.and_then(|t| rosters.get(&(game_id, t))).unwrap_or(&empty)
    };
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM natstat_lineups WHERE game_id = $1")
        .bind(game_id)
        .execute(&mut *tx)
        .await?;

    for u in units {
        let coded_team = u.team_code.as_deref().and_then(|c| teams.get(c)).copied();
        // Primary = the feed's team, alternate = the other team in this game.
        let (primary, alternate) = match (coded_team, home_team, away_team) {
            (Some(c), Some(h), Some(a)) if c == h => (Some(c), Some(a)),
            (Some(c), Some(h), Some(a)) if c == a => (Some(c), Some(h)),
            (Some(c), _, _) => (Some(c), None),
            (None, h, a) => (h, a),
        };

        let primary_res = resolve_unit(u, roster_of(primary));
        let alternate_res = alternate.map(|t| resolve_unit(u, roster_of(Some(t))));
        let count =
            |r: &[(Option<Uuid>, ResolvedBy)]| r.iter().filter(|(id, _)| id.is_some()).count();

        let (team_id, slots) = match alternate_res {
            Some(alt) if alternate_overturns(count(&primary_res), count(&alt)) => {
                if coded_team.is_some() {
                    report.team_swapped_units += 1;
                }
                (alternate, alt)
            }
            _ => (primary, primary_res),
        };

        let player_ids: Vec<Option<Uuid>> = slots
            .into_iter()
            .map(|(id, by)| {
                match by {
                    ResolvedBy::Code => report.resolved_by_code += 1,
                    ResolvedBy::Name => report.resolved_by_name += 1,
                    ResolvedBy::None => report.unresolved_slots += 1,
                }
                id
            })
            .collect();
        let resolved = player_ids.len() == 5 && player_ids.iter().all(Option::is_some);

        report.units += 1;
        report.player_slots += player_ids.len() as u64;

        sqlx::query(
            "INSERT INTO natstat_lineups \
             (game_id, natstat_lineup_id, season, team_id, team_code, player_codes, \
              player_ids, resolved, possessions, points, points_d, plusminus, raw) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(game_id)
        .bind(&u.natstat_lineup_id)
        .bind(season)
        .bind(team_id)
        .bind(&u.team_code)
        .bind(&u.player_codes)
        .bind(&player_ids)
        .bind(resolved)
        .bind(u.possessions)
        .bind(u.points)
        .bind(u.points_d)
        .bind(u.plusminus)
        .bind(&u.raw)
        .execute(&mut *tx)
        .await?;
    }

    let status = if units.is_empty() { "no_lineups" } else { "ok" };
    sqlx::query(
        "INSERT INTO natstat_lineup_games (game_id, season, status, units, fetched_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (game_id) DO UPDATE \
         SET status = EXCLUDED.status, units = EXCLUDED.units, fetched_at = now()",
    )
    .bind(game_id)
    .bind(season)
    .bind(status)
    .bind(units.len() as i32)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Upsert a ledger row outside the per-game transaction (error bookkeeping).
async fn record_status(
    pool: &PgPool,
    game_id: Uuid,
    season: i32,
    status: &str,
    units: i32,
) -> Result<(), NatStatError> {
    sqlx::query(
        "INSERT INTO natstat_lineup_games (game_id, season, status, units, fetched_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (game_id) DO UPDATE \
         SET status = EXCLUDED.status, units = EXCLUDED.units, fetched_at = now()",
    )
    .bind(game_id)
    .bind(season)
    .bind(status)
    .bind(units)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_unit() -> Value {
        json!({
            "id": "10612954",
            "team-code": "MINN",
            "lineupplayers": "M. Mitchell Jr. · K. Betts · F. Odukale · P. Fox · L. Patterson",
            "possessions": "8",
            "oppp": "2.250",
            "dppp": "1.500",
            "points": "18",
            "points-d": "15",
            "plusminus": "-3",
            "players": {
                "player-1": {"code": "57989206", "player": "Mike Mitchell"},
                "player-2": {"code": "57989207", "player": {}},
                "player-3": {"code": "57989214", "player": "Femi Odukale"},
                "player-4": {"code": "57989215", "player": "Parker Fox"},
                "player-5": {"code": "58002555", "player": "Lu'cye Patterson"}
            }
        })
    }

    fn slot(code: &str, name: &str, id_byte: u8) -> RosterSlot {
        let (initial, rest, last) = name_parts(name);
        RosterSlot {
            code: code.to_string(),
            initial,
            rest,
            last,
            id: Uuid::from_bytes([id_byte; 16]),
        }
    }

    #[test]
    fn parse_unit_extracts_codes_in_slot_order() {
        let u = parse_unit("lineup_10612954", &sample_unit()).unwrap();
        assert_eq!(u.natstat_lineup_id, "10612954");
        assert_eq!(u.team_code.as_deref(), Some("MINN"));
        assert_eq!(
            u.player_codes,
            vec!["57989206", "57989207", "57989214", "57989215", "58002555"]
        );
    }

    #[test]
    fn parse_unit_splits_lineupplayers_aligned_with_slots() {
        let u = parse_unit("lineup_10612954", &sample_unit()).unwrap();
        assert_eq!(
            u.player_names,
            vec![
                "M. Mitchell Jr.",
                "K. Betts",
                "F. Odukale",
                "P. Fox",
                "L. Patterson"
            ]
        );
    }

    #[test]
    fn parse_unit_parses_string_numerics_including_negative() {
        let u = parse_unit("lineup_10612954", &sample_unit()).unwrap();
        assert_eq!(u.possessions, Some(8.0));
        assert_eq!(u.points, Some(18));
        assert_eq!(u.points_d, Some(15));
        assert_eq!(u.plusminus, Some(-3));
    }

    #[test]
    fn parse_unit_without_players_is_skipped() {
        let mut unit = sample_unit();
        unit.as_object_mut().unwrap().remove("players");
        assert!(parse_unit("lineup_1", &unit).is_none());
    }

    #[test]
    fn resolve_slot_prefers_exact_code_match() {
        let roster = vec![
            slot("100", "Greg Gantt", 1),
            slot("200", "Fletcher Abee", 2),
        ];
        let (id, _) = resolve_slot("200", Some("G. Gantt"), &roster);
        // Code wins even though the abbreviation points elsewhere.
        assert_eq!(id, Some(Uuid::from_bytes([2; 16])));
    }

    #[test]
    fn resolve_slot_falls_back_to_unique_abbrev_name() {
        let roster = vec![
            slot("100", "Greg Gantt", 1),
            slot("200", "Fletcher Abee", 2),
        ];
        let (id, _) = resolve_slot("999999", Some("F. Abee"), &roster);
        assert_eq!(id, Some(Uuid::from_bytes([2; 16])));
    }

    #[test]
    fn resolve_slot_handles_suffix_mismatch_between_sources() {
        // Lineup string says "M. Mitchell Jr.", box name has no suffix.
        let roster = vec![slot("100", "Mike Mitchell", 7)];
        let (id, _) = resolve_slot("999999", Some("M. Mitchell Jr."), &roster);
        assert_eq!(id, Some(Uuid::from_bytes([7; 16])));
    }

    #[test]
    fn resolve_slot_leaves_ambiguous_abbreviation_null() {
        let roster = vec![slot("100", "Jalen Smith", 1), slot("200", "Jaden Smith", 2)];
        let (id, _) = resolve_slot("999999", Some("J. Smith"), &roster);
        assert_eq!(id, None);
    }

    #[test]
    fn alternate_overturns_needs_a_clear_majority_not_just_strictly_more() {
        // The real swapped game resolves ~5 v 0 — flips.
        assert!(alternate_overturns(0, 5));
        assert!(alternate_overturns(2, 3));
        // A missing primary box roster + one coincidental opponent name
        // match must NOT flip the unit.
        assert!(!alternate_overturns(0, 1));
        assert!(!alternate_overturns(0, 2));
        // Ties keep the feed's attribution.
        assert!(!alternate_overturns(3, 3));
    }

    #[test]
    fn resolve_slot_initial_v_is_not_a_roman_numeral_suffix() {
        // Live failures from the 2025 smoke: "V." was stripped by the suffix
        // rule, orphaning both names.
        let roster = vec![
            slot("100", "V.J. Edgecombe", 4),
            slot("200", "Jeremy Roach", 5),
        ];
        let (id, _) = resolve_slot("999999", Some("V. Edgecombe"), &roster);
        assert_eq!(id, Some(Uuid::from_bytes([4; 16])));

        let roster = vec![slot("100", "Viktor Lakhin", 6)];
        let (id, _) = resolve_slot("999999", Some("V. Lakhin"), &roster);
        assert_eq!(id, Some(Uuid::from_bytes([6; 16])));
    }

    #[test]
    fn resolve_slot_matches_multiword_last_name() {
        let roster = vec![slot("100", "Jake Van Dyke", 3)];
        let (id, _) = resolve_slot("999999", Some("J. Van Dyke"), &roster);
        assert_eq!(id, Some(Uuid::from_bytes([3; 16])));
    }
}
