//! Play-by-play ingestion — the API (intra-season) loader plus the shared
//! normalized row type and batch-insert used by BOTH loaders.
//!
//! Design of record: `docs/pbp_methodology.md`. The CSV bulk loader lives in
//! `bootstrap_csv.rs` (backfill path); it builds the same [`PbpRow`] values and
//! calls [`insert_pbp_rows`], so the normalization contract is defined once,
//! here. The raw `play_by_play` table is local-only (never synced to prod).
//!
//! API vs CSV shape (re-verified 2026-11-06): the API groups plays per game,
//! `playbyplay.playbyplay_<gamecode>`, with the game's teams at the top of each
//! entry and its plays under numeric keys. A play carries the sequence, teams,
//! scores and a description, and nothing else: no tags, no game clock, no
//! on-floor lineup and no play id, and its `player` code matches neither id we
//! store. [`parse_feed_game`] rebuilds all of those. The earlier nested shape
//! (`play.game.*`, with tags and `game.onfloorhome`) is gone for every season.
//! The flat CSV has a Points column and its own tags. Both collapse into the
//! same [`PbpRow`].

use crate::client::NatStatError;
use crate::{NatStatClient, extract_results};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};
use uuid::Uuid;

/// One normalized play-by-play event, the shared contract between the API and
/// CSV loaders. `seq` is assigned by the caller (dense 0..N per game, in source
/// order) because NatStat's `sort_order` collides across same-instant events.
#[derive(Debug, Clone)]
pub struct PbpRow {
    pub game_id: Uuid,
    pub season: i32,
    pub seq: i32,
    pub sort_order: Option<String>,
    pub period: i32,
    pub clock: Option<String>,
    pub team_id: Option<Uuid>,
    pub player_id: Option<Uuid>,
    pub description: Option<String>,
    pub scoring_play: bool,
    pub points: i32,
    pub tags: Vec<String>,
    pub score_home: Option<i32>,
    pub score_vis: Option<i32>,
    pub score_diff: Option<i32>,
    /// Raw NatStat on-floor player-code strings (`game.onfloorhome` /
    /// `onfloorvis`), API-only. `None` for CSV rows (the export omits them);
    /// the lineup derivation falls back to SUB-replay when these are absent.
    pub onfloor_home: Option<String>,
    pub onfloor_vis: Option<String>,
}

/// Counts from a PBP ingest run, surfaced to the CLI for a sanity check
/// against expected volume (~530 rows/game).
#[derive(Debug, Default)]
pub struct PbpReport {
    pub games: u64,
    pub rows: u64,
    pub skipped_games: u64,
}

impl std::fmt::Display for PbpReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "play_by_play: games={} rows={} skipped_games={}",
            self.games, self.rows, self.skipped_games
        )
    }
}

/// Points scored on a play, derived from its tags. The API path carries no
/// explicit points; the CSV path has a Points column and uses it directly.
/// Made 3s are tagged `3FM` (not `FGM`), made 2s `FGM`, free throws `FTM`.
pub fn points_from_tags(tags: &[String]) -> i32 {
    let has = |t: &str| tags.iter().any(|x| x == t);
    if has("FTM") {
        1
    } else if has("3FM") {
        3
    } else if has("FGM") {
        2
    } else {
        0
    }
}

/// Split a NatStat `|`-delimited tag string (`"FGA|paint|offto|"`) into tags,
/// dropping empties. Shared by both loaders.
pub fn parse_tags(raw: &str) -> Vec<String> {
    raw.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Parse NatStat's signed score-diff string (`"+5"`, `"-5"`, `"0"`) to i32.
pub fn parse_score_diff(raw: &str) -> Option<i32> {
    let s = raw.trim().trim_start_matches('+');
    if s.is_empty() { None } else { s.parse().ok() }
}

/// Replace all play-by-play for one game then bulk-insert the supplied rows,
/// in a single transaction. The game is the idempotency unit: PBP for a
/// finished game is immutable, so delete-then-insert is the simplest correct
/// re-ingest. `rows` must all share `game_id`. Used by both loaders.
pub async fn replace_game_pbp(
    pool: &PgPool,
    game_id: Uuid,
    rows: &[PbpRow],
) -> Result<u64, NatStatError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM play_by_play WHERE game_id = $1")
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
    let n = insert_pbp_rows(&mut tx, rows).await?;
    tx.commit().await?;
    Ok(n)
}

/// Chunked multi-row INSERT into `play_by_play`. Postgres caps bind params at
/// 65535; at 17 columns/row we chunk well under that (1000 rows/statement).
/// Does NOT delete first — callers manage idempotency (per-game
/// [`replace_game_pbp`], or a season-wide delete in the CSV path).
pub async fn insert_pbp_rows(
    tx: &mut Transaction<'_, Postgres>,
    rows: &[PbpRow],
) -> Result<u64, NatStatError> {
    let mut total = 0u64;
    for chunk in rows.chunks(1000) {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "INSERT INTO play_by_play (game_id, season, seq, sort_order, period, clock, \
             team_id, player_id, description, scoring_play, points, tags, \
             score_home, score_vis, score_diff, onfloor_home, onfloor_vis) ",
        );
        qb.push_values(chunk, |mut b, r| {
            b.push_bind(r.game_id)
                .push_bind(r.season)
                .push_bind(r.seq)
                .push_bind(r.sort_order.clone())
                .push_bind(r.period)
                .push_bind(r.clock.clone())
                .push_bind(r.team_id)
                .push_bind(r.player_id)
                .push_bind(r.description.clone())
                .push_bind(r.scoring_play)
                .push_bind(r.points)
                .push_bind(r.tags.clone())
                .push_bind(r.score_home)
                .push_bind(r.score_vis)
                .push_bind(r.score_diff)
                .push_bind(r.onfloor_home.clone())
                .push_bind(r.onfloor_vis.clone());
        });
        qb.build().execute(&mut **tx).await?;
        total += chunk.len() as u64;
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// API loader (intra-season)
// ---------------------------------------------------------------------------

/// Ingest play-by-play for a single date (`YYYY-MM-DD`). This is the
/// intra-season path: a nightly job fetches just the prior day's finished
/// games. The in-scope game set is the games on that date.
pub async fn ingest_play_by_play_by_date(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    date: &str,
) -> Result<PbpReport, NatStatError> {
    let scope = game_codes_for_dates(pool, season, date, date).await?;
    ingest_pbp_scoped(client, pool, season, date, &scope).await
}

/// Ingest play-by-play for a date range (`start` and `end` are `YYYY-MM-DD`).
pub async fn ingest_play_by_play_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<PbpReport, NatStatError> {
    let scope = game_codes_for_dates(pool, season, start, end).await?;
    let range = format!("{start},{end}");
    ingest_pbp_scoped(client, pool, season, &range, &scope).await
}

/// Ingest play-by-play for a single game by NatStat gamecode.
pub async fn ingest_play_by_play_by_gamecode(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    gamecode: &str,
) -> Result<PbpReport, NatStatError> {
    let scope: HashSet<String> = std::iter::once(gamecode.to_string()).collect();
    ingest_pbp_scoped(client, pool, season, gamecode, &scope).await
}

/// Core API ingest, **scope-aware**. Pages `playbyplay/{range}`, keeps only
/// games in `scope`, and stops as soon as a page yields zero in-scope games (or
/// is empty / NO_DATA). A page holds up to `results-max` GAMES, each with all of
/// its plays.
///
/// Returns an error when the feed answers and nothing can be stored from it.
/// That is what a format change looks like, and an empty success hides it: the
/// self-heal can re-pull a night, but it gets nothing back, and the step still
/// records `ok`.
///
/// Why scope-aware: NatStat only honors the `range` filter on **page 1**. Past
/// offset 0 a `gamecode` query silently returns the GLOBAL season stream
/// (verified 2026-06-05: offset 23000 of gamecode 1511104 returned a different
/// game). A naive "paginate until empty" loop therefore runs away through all
/// ~6,700 pages of the season. Bounding by the in-scope game set caps any
/// query at ~1 page past its real end and guarantees we never write a game we
/// didn't ask for. The date/season filters do appear to compose with offset
/// (like `playerperfs`), so for the nightly date path this is mostly a safety
/// belt; for `gamecode` it's load-bearing.
async fn ingest_pbp_scoped(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    range: &str,
    scope: &HashSet<String>,
) -> Result<PbpReport, NatStatError> {
    if scope.is_empty() {
        warn!(
            range,
            "pbp: no in-scope games for this query — nothing to fetch"
        );
        return Ok(PbpReport::default());
    }

    // Backstop only — scope termination should stop us long before this. A full
    // season is ~6,700 pages at 500/page; we never want to reach that here.
    const MAX_PAGES: u64 = 4000;

    let games = game_map(pool, season).await?;
    let teams = team_abbrev_map(pool, season).await?;

    // In-scope game entries (owned, since each page Value is dropped between
    // iterations).
    let mut by_game: HashMap<String, Value> = HashMap::new();
    let mut feed_entries = 0u64;
    let mut recognized_entries = 0u64;
    let mut offset: u64 = 0;
    let mut page: u64 = 1;
    let mut step: u64 = 500; // playbyplay page size; refined from meta below

    loop {
        let response = match client
            .get("playbyplay", Some(range), Some(offset), None)
            .await
        {
            Ok(r) => r,
            // NatStat's normal end-of-results signal.
            Err(NatStatError::ApiError { code, .. }) if code == "NO_DATA" => break,
            Err(e) => return Err(e),
        };
        if let Some(m) = response
            .get("meta")
            .and_then(|m| m.get("results-max"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            && m > 0
        {
            step = m;
        }

        let entries = extract_results(&response);
        if entries.is_empty() {
            break;
        }
        feed_entries += entries.len() as u64;
        let mut in_scope = 0u64;
        for entry in entries {
            let Some(code) = entry.get("game-code").and_then(Value::as_str) else {
                continue;
            };
            recognized_entries += 1;
            if scope.contains(code) {
                in_scope += 1;
                by_game.insert(code.to_string(), entry.clone());
            }
        }

        // Termination. A page with no in-scope games is NOT a sufficient stop
        // signal on its own: a date can include non-D1 games we don't ingest,
        // and if a full page lands entirely on those (out of scope) *between*
        // two of our games, an eager break would silently drop the later games.
        // So we keep paging while any scope game is still unseen — the
        // date/daterange filter composes, so the real end arrives as an empty /
        // NO_DATA page. Once every scope game has been seen, the first
        // out-of-scope page is the tail and we stop.
        let all_seen = by_game.len() == scope.len();
        if in_scope == 0 && all_seen {
            break;
        }
        // Single-target queries (`gamecode`, or a one-game date) do NOT compose
        // past page 1 for a gamecode, so `all_seen` can never trip if that game
        // simply has no PBP (postponed / feed gap / bad code) — without this the
        // loop would walk the global season stream to the MAX_PAGES backstop.
        // Page 1 (offset 0) is always filter-honored, so if a sole target hasn't
        // shown up by the time its first page yields nothing in scope, it has no
        // plays: stop. Guarded to size 1 so multi-game dates (which may legitimately
        // open with an out-of-scope non-D1 game) keep paging.
        if scope.len() == 1 && in_scope == 0 && by_game.is_empty() {
            break;
        }
        if page >= MAX_PAGES {
            warn!(range, page, "pbp: hit MAX_PAGES backstop — stopping");
            break;
        }
        offset += step;
        page += 1;
    }

    if feed_entries > 0 && recognized_entries == 0 {
        return Err(feed_shape_error(format!(
            "{feed_entries} entries for {range}, none with a game-code"
        )));
    }

    let game_ids: Vec<Uuid> = by_game
        .keys()
        .filter_map(|code| games.get(code).map(|&(id, _)| id))
        .collect();
    let rosters = game_rosters(pool, &game_ids).await?;

    let mut report = PbpReport::default();
    let mut unresolved_players = 0u64;
    for (game_code, entry) in &by_game {
        let Some(&(game_id, game_season)) = games.get(game_code) else {
            report.skipped_games += 1;
            continue;
        };
        let parsed = parse_feed_game(entry, game_id, game_season, &teams, &rosters);
        // A game listed with no plays yet must not wipe what is stored for it.
        if parsed.rows.is_empty() {
            report.skipped_games += 1;
            continue;
        }
        unresolved_players += parsed.unresolved_players;
        report.rows += replace_game_pbp(pool, game_id, &parsed.rows).await?;
        report.games += 1;
    }

    if !by_game.is_empty() && report.rows == 0 {
        return Err(feed_shape_error(format!(
            "{} in-scope games for {range}, no plays parsed",
            by_game.len()
        )));
    }
    if unresolved_players > 0 {
        info!(
            unresolved_players,
            "pbp: rostered-team plays with no box-score name match"
        );
    }
    if report.skipped_games > 0 {
        warn!(
            skipped = report.skipped_games,
            "pbp: games skipped (not ingested for this season)"
        );
    }
    info!(%report, "play-by-play ingested (API, scope-aware)");
    Ok(report)
}

fn feed_shape_error(message: String) -> NatStatError {
    NatStatError::ApiError {
        code: "PBP_FEED_SHAPE".to_string(),
        message,
    }
}

/// Seconds left in the period, decoded from a play's `sequence`. The number is
/// `(20 - minutes) * 100 + (100 - seconds)`, counted from a 20-minute period
/// even in overtime, and printed with a quirk: a multiple of ten loses its
/// trailing zero behind a dot, so 19:30 is `1-01.7`, not `1-0170`. On the hour
/// the seconds part is `60` with no dot, so 19:00 is `1-0160` while 19:40 is
/// `1-01.6`. Mapping fitted against the stored clock of every 2026 play.
fn seconds_remaining(sequence: &str) -> Option<i32> {
    let body = sequence.split_once('-')?.1;
    let dotted = body.contains('.');
    let digits: String = body.chars().filter(char::is_ascii_digit).collect();
    let n: i32 = format!("{digits:0<4}").parse().ok()?;
    let (minutes_gone, lo) = (n / 100, n % 100);
    let seconds = if lo == 60 && !dotted { 0 } else { 100 - lo };
    Some(((20 - minutes_gone) * 60 + seconds).max(0))
}

/// Box-event tags for a play, derived from its description in the vocabulary
/// the rest of the pipeline reads. The contextual tags (`paint`, `brk`, `2ch`,
/// `offto`) cannot be rebuilt from the text, so this leaves them out and
/// `pbp_lacks_context_tags` clears that season's tag-derived aggregates rather
/// than serving zeros.
pub fn tags_from_description(description: &str) -> Vec<String> {
    let text = format!(
        " {} ",
        description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    );
    let has = |needle: &str| text.contains(needle);
    let tags: &[&str] = if has(" sub in ") || has(" sub out ") {
        &["SUB"]
    } else if has(" made free throw") {
        &["FTA", "FTM"]
    } else if has(" missed free throw") {
        &["FTA"]
    } else if has(" made ") && has("3-point") {
        &["3FA", "3FM"]
    } else if has(" missed ") && has("3-point") {
        &["3FA"]
    } else if has(" made ") {
        &["FGA", "FGM"]
    } else if has(" missed ") {
        &["FGA"]
    } else if has("defensivedeadball") {
        // "Rebound Team defensivedeadball" is a team bookkeeping row, not a board.
        &[]
    } else if has("deadball rebound") {
        &["REB", "TREB"]
    } else if has("offensive rebound") {
        &["ORB", "REB"]
    } else if has(" rebound") {
        &["REB"]
    } else if has("timeout") {
        &["TIMEOUT"]
    } else if has("fouled on the play") {
        &["FOULED"]
    } else if has(" turnover") {
        &["TO"]
    } else if text.ends_with(" assist ") {
        &["AST"]
    } else if text.ends_with(" steal ") {
        &["STL"]
    } else if text.ends_with(" block ") {
        &["BLK"]
    } else if has(" personal") || has(" foul") || has(" technical") || has(" flagrant") {
        &["PF"]
    } else if has("start of ") || has("end of ") || has("jump ball") {
        &["MISC"]
    } else {
        &[]
    };
    tags.iter().map(|t| t.to_string()).collect()
}

/// Order of plays that share a second and a score. The feed lists plays grouped
/// by event type, so this is the sequence a dead ball actually runs in. Made
/// free throws sit before subs of the same score because a sub row's score
/// already includes them.
fn instant_rank(tags: &[String]) -> u8 {
    let has = |t: &str| tags.iter().any(|x| x == t);
    if has("FGA") || has("3FA") {
        0
    } else if has("AST") || has("BLK") {
        1
    } else if has("TO") {
        2
    } else if has("STL") {
        3
    } else if has("PF") {
        5
    } else if has("FOULED") {
        6
    } else if has("TIMEOUT") {
        7
    } else if has("FTA") {
        8
    } else if has("REB") {
        9
    } else if has("SUB") {
        10
    } else {
        4
    }
}

/// -1 opens a period, 1 closes it, 0 is everything between. These rows can
/// carry a 0-0 score, so they are placed by phase rather than by score.
fn period_phase(description: &str) -> i8 {
    let text = description.to_lowercase();
    if text == "jump ball" || text.starts_with("start of ") || text.ends_with(" start") {
        -1
    } else if text.starts_with("end of ") || text.ends_with(" confirmed") {
        1
    } else {
        0
    }
}

/// A box-score roster for one (game, team), keyed for name matching. `None`
/// marks a key two players share, which resolves to nobody.
#[derive(Debug, Default)]
struct RosterNames {
    full: HashMap<String, Option<Uuid>>,
    last: HashMap<String, Option<Uuid>>,
}

impl RosterNames {
    fn add(&mut self, name: &str, id: Uuid) {
        let key = normalize_name(name);
        let last = key.rsplit(' ').next().unwrap_or_default().to_string();
        for (map, k) in [(&mut self.full, key), (&mut self.last, last)] {
            map.entry(k)
                .and_modify(|v| {
                    if *v != Some(id) {
                        *v = None;
                    }
                })
                .or_insert(Some(id));
        }
    }

    /// Exact name, then a last name only one player on the roster has (the feed
    /// and the box score disagree on forms like "Tobi" and "Toibu").
    fn resolve(&self, name: &str) -> Option<Uuid> {
        let key = normalize_name(name);
        if let Some(found) = self.full.get(&key) {
            return *found;
        }
        let last = key.rsplit(' ').next()?;
        self.last.get(last).copied().flatten()
    }
}

/// Lowercased, punctuation and name suffix dropped, so the feed's "Chris Davis
/// Jr" and "Roddy Gayle" meet the box score's "Chris Davis Jr." and "Roddy
/// Gayle Jr.".
fn normalize_name(name: &str) -> String {
    let cleaned: String = name
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '.' | '\'' | '’'))
        .collect();
    let mut tokens: Vec<&str> = cleaned.split_whitespace().collect();
    while tokens.len() > 1
        && matches!(
            tokens.last().copied(),
            Some("jr" | "sr" | "ii" | "iii" | "iv" | "v")
        )
    {
        tokens.pop();
    }
    tokens.join(" ")
}

/// The name a description opens with, for rows that leave `player_name` blank.
fn name_from_description(description: &str) -> Option<&str> {
    const VERBS: &[&str] = &[
        " sub in",
        " sub out",
        " made ",
        " missed ",
        " defensive ",
        " offensive ",
        " turnover",
        " steal",
        " block",
        " assist",
        " personal",
        " fouled",
    ];
    let end = VERBS.iter().filter_map(|v| description.find(v)).min()?;
    let name = description[..end].trim();
    (!name.is_empty()).then_some(name)
}

/// One game's normalized rows, plus how many plays on a rostered team named a
/// player the box score could not place.
struct ParsedGame {
    rows: Vec<PbpRow>,
    unresolved_players: u64,
}

fn int_field(play: &Value, key: &str) -> Option<i32> {
    play.get(key)?.as_str()?.trim().parse().ok()
}

fn str_field<'a>(play: &'a Value, key: &str) -> Option<&'a str> {
    play.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Normalize one `playbyplay_<gamecode>` entry into chronological [`PbpRow`]s.
/// The feed gives no play id and lists plays by event type, so order comes from
/// the clock, then the running score, then [`instant_rank`].
fn parse_feed_game(
    entry: &Value,
    game_id: Uuid,
    season: i32,
    teams: &HashMap<String, Uuid>,
    rosters: &HashMap<(Uuid, Uuid), RosterNames>,
) -> ParsedGame {
    let home_team = str_field(entry, "home-code")
        .and_then(|c| teams.get(c))
        .copied();
    let vis_team = str_field(entry, "visitor-code")
        .and_then(|c| teams.get(c))
        .copied();

    struct Keyed<'a> {
        play: &'a Value,
        description: &'a str,
        tags: Vec<String>,
        key: (i32, i8, i32, i32, u8, u64),
    }

    let mut plays: Vec<Keyed> = entry
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, play)| {
            let index: u64 = k.parse().ok()?;
            // The feed opens each game with a blank header row.
            let description = str_field(play, "description")?;
            let period = int_field(play, "period").unwrap_or(0);
            let remaining = str_field(play, "sequence")
                .and_then(seconds_remaining)
                .unwrap_or(0);
            let score = int_field(play, "score_home").unwrap_or(0)
                + int_field(play, "score_vis").unwrap_or(0);
            let tags = tags_from_description(description);
            let key = (
                period,
                period_phase(description),
                -remaining,
                score,
                instant_rank(&tags),
                index,
            );
            Some(Keyed {
                play,
                description,
                tags,
                key,
            })
        })
        .collect();
    plays.sort_by_key(|p| p.key);

    let mut rows = Vec::with_capacity(plays.len());
    let mut unresolved_players = 0u64;
    let (mut running_home, mut running_vis) = (0, 0);
    for (seq, p) in plays.into_iter().enumerate() {
        let play = p.play;
        let team_id = str_field(play, "team_code")
            .and_then(|c| teams.get(c))
            .copied();
        let name = str_field(play, "player_name").or_else(|| name_from_description(p.description));
        let roster = team_id.and_then(|t| rosters.get(&(game_id, t)));
        let player_id = match (name, roster) {
            (Some(n), Some(r)) => {
                let found = r.resolve(n);
                unresolved_players += u64::from(found.is_none() && n != "Team");
                found
            }
            _ => None,
        };

        // A running score only rises. Timeouts and period markers report 0-0
        // mid-game and some late rows lag the true score, so a row can only
        // raise it; otherwise a stint could end on a stale score.
        running_home = running_home.max(int_field(play, "score_home").unwrap_or(0));
        running_vis = running_vis.max(int_field(play, "score_vis").unwrap_or(0));
        let score_diff = match team_id {
            Some(t) if Some(t) == home_team => Some(running_home - running_vis),
            Some(t) if Some(t) == vis_team => Some(running_vis - running_home),
            _ => None,
        };

        let points = points_from_tags(&p.tags);
        rows.push(PbpRow {
            game_id,
            season,
            seq: seq as i32,
            sort_order: str_field(play, "sequence").map(String::from),
            period: p.key.0,
            clock: str_field(play, "sequence")
                .and_then(seconds_remaining)
                // 2027 sequences run up to 20:39 in the opening seconds. They still
                // sort correctly, but no clock reads above the top of a half.
                .map(|s| s.min(20 * 60))
                .map(|s| format!("{:02}:{:02}", s / 60, s % 60)),
            team_id,
            player_id,
            description: Some(p.description.to_string()),
            // Derived from points rather than the feed's flag so the field means
            // the same thing on both loaders.
            scoring_play: points > 0,
            points,
            tags: p.tags,
            score_home: Some(running_home),
            score_vis: Some(running_vis),
            score_diff,
            onfloor_home: None,
            onfloor_vis: None,
        });
    }
    ParsedGame {
        rows,
        unresolved_players,
    }
}

// ---------------------------------------------------------------------------
// Season resolution maps
// ---------------------------------------------------------------------------

/// `games.natstat_id -> (games.id, season)` for the season. The game's own
/// season stamps its PBP rows (the `--year` arg only bounds this lookup).
async fn game_map(
    pool: &PgPool,
    season: i32,
) -> Result<HashMap<String, (Uuid, i32)>, NatStatError> {
    let rows: Vec<(Uuid, Option<String>, i32)> =
        sqlx::query_as("SELECT id, natstat_id, season FROM games WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, nat, s)| nat.map(|n| (n, (id, s))))
        .collect())
}

/// In-scope game natstat ids for a date span (inclusive), from our own `games`
/// table. Drives scope-aware pagination so a bounded PBP query can't run away
/// into the global season stream. `start == end` for a single date.
async fn game_codes_for_dates(
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<HashSet<String>, NatStatError> {
    // Bind the date strings and let Postgres cast — avoids a chrono parse and
    // surfaces a malformed date as a clear DB error.
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT natstat_id FROM games \
         WHERE season = $1 AND game_date BETWEEN $2::date AND $3::date \
           AND natstat_id IS NOT NULL",
    )
    .bind(season)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().filter_map(|(n,)| n).collect())
}

/// `teams.natstat_id (abbrev) -> teams.id` for the season.
pub(crate) async fn team_abbrev_map(
    pool: &PgPool,
    season: i32,
) -> Result<HashMap<String, Uuid>, NatStatError> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, natstat_id FROM teams WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, code)| (code, id)).collect())
}

/// `(game, team) -> box-score names` for the games being loaded. Names are the
/// only join to the feed: its `player` codes match neither `players.natstat_id`
/// nor anything else we store.
async fn game_rosters(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<HashMap<(Uuid, Uuid), RosterNames>, NatStatError> {
    let rows: Vec<(Uuid, Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT pgs.game_id, pgs.team_id, p.name, pgs.player_id \
         FROM player_game_stats pgs JOIN players p ON p.id = pgs.player_id \
         WHERE pgs.game_id = ANY($1)",
    )
    .bind(game_ids)
    .fetch_all(pool)
    .await?;
    let mut map: HashMap<(Uuid, Uuid), RosterNames> = HashMap::new();
    for (game_id, team_id, name, player_id) in rows {
        map.entry((game_id, team_id))
            .or_default()
            .add(&name, player_id);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_from_tags_made_three_is_three() {
        assert_eq!(points_from_tags(&["3FA".into(), "3FM".into()]), 3);
    }

    #[test]
    fn points_from_tags_made_two_is_two() {
        assert_eq!(
            points_from_tags(&["FGA".into(), "FGM".into(), "paint".into()]),
            2
        );
    }

    #[test]
    fn points_from_tags_made_ft_is_one() {
        assert_eq!(points_from_tags(&["FTA".into(), "FTM".into()]), 1);
    }

    #[test]
    fn points_from_tags_miss_is_zero() {
        assert_eq!(points_from_tags(&["FGA".into(), "paint".into()]), 0);
        assert_eq!(points_from_tags(&["REB".into(), "ORB".into()]), 0);
    }

    #[test]
    fn parse_tags_drops_trailing_empty() {
        assert_eq!(
            parse_tags("FGA|paint|offto|"),
            vec!["FGA".to_string(), "paint".to_string(), "offto".to_string()]
        );
        assert!(parse_tags("").is_empty());
    }

    #[test]
    fn parse_score_diff_handles_signs() {
        assert_eq!(parse_score_diff("+5"), Some(5));
        assert_eq!(parse_score_diff("-5"), Some(-5));
        assert_eq!(parse_score_diff("0"), Some(0));
        assert_eq!(parse_score_diff(""), None);
    }

    #[test]
    fn seconds_remaining_decodes_every_sequence_form() {
        assert_eq!(seconds_remaining("1-0060"), Some(20 * 60));
        assert_eq!(seconds_remaining("1-0182"), Some(19 * 60 + 18));
        assert_eq!(seconds_remaining("1-0159"), Some(19 * 60 + 41));
        // A trailing zero hides behind a dot, which is the only thing telling
        // 19:40 apart from 19:00.
        assert_eq!(seconds_remaining("1-01.7"), Some(19 * 60 + 30));
        assert_eq!(seconds_remaining("1-01.6"), Some(19 * 60 + 40));
        assert_eq!(seconds_remaining("1-0160"), Some(19 * 60));
        assert_eq!(seconds_remaining("2-2060"), Some(0));
        // Overtime counts on from a 20-minute period.
        assert_eq!(seconds_remaining("3-1560"), Some(5 * 60));
        assert_eq!(seconds_remaining("3-1641"), Some(4 * 60 + 59));
        assert_eq!(seconds_remaining("3-20.9"), Some(10));
        assert_eq!(seconds_remaining("garbled"), None);
    }

    fn tags(d: &str) -> Vec<String> {
        tags_from_description(d)
    }

    #[test]
    fn tags_cover_the_box_events() {
        assert_eq!(tags("Aday Mara sub in"), ["SUB"]);
        assert_eq!(tags("Morez Johnson made layup"), ["FGA", "FGM"]);
        assert_eq!(tags("Tarris Reed missed hook shot"), ["FGA"]);
        assert_eq!(
            tags("Elliot Cadeau made 3-point field goal"),
            ["3FA", "3FM"]
        );
        assert_eq!(tags("X missed 3-point jump shot"), ["3FA"]);
        assert_eq!(tags("X made free throw 1 of 2"), ["FTA", "FTM"]);
        assert_eq!(tags("X missed free throw 2 of 2"), ["FTA"]);
        assert_eq!(tags("X offensive rebound"), ["ORB", "REB"]);
        assert_eq!(tags("Team offensive rebound team"), ["ORB", "REB"]);
        assert_eq!(tags("X defensive rebound"), ["REB"]);
        assert_eq!(tags("X offensive deadball rebound"), ["REB", "TREB"]);
        assert!(tags("Rebound Team defensivedeadball").is_empty());
        assert_eq!(tags("X personal foul  two shots"), ["PF"]);
        assert_eq!(tags("X offensive  foul"), ["PF"]);
        assert_eq!(tags("X fouled on the play"), ["FOULED"]);
        assert_eq!(tags("X turnover travel"), ["TO"]);
        assert_eq!(tags("X steal"), ["STL"]);
        assert_eq!(tags("X block"), ["BLK"]);
        assert_eq!(tags("X assist"), ["AST"]);
        assert_eq!(tags("media timeout"), ["TIMEOUT"]);
        assert_eq!(tags("Start of period"), ["MISC"]);
    }

    #[test]
    fn a_shot_off_a_turnover_is_a_shot() {
        assert_eq!(tags("X made jump shot off turnover"), ["FGA", "FGM"]);
    }

    #[test]
    fn a_name_containing_a_verb_does_not_change_the_event() {
        assert_eq!(tags("Blockton Stealey assist"), ["AST"]);
    }

    #[test]
    fn names_meet_across_suffix_case_and_punctuation() {
        let mut roster = RosterNames::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        roster.add("Roddy Gayle Jr.", a);
        roster.add("Trey McKenney", b);
        assert_eq!(roster.resolve("Roddy Gayle"), Some(a));
        assert_eq!(roster.resolve("Trey Mckenney"), Some(b));
        assert_eq!(roster.resolve("T. McKenney"), Some(b));
    }

    #[test]
    fn a_shared_last_name_resolves_to_nobody() {
        let mut roster = RosterNames::default();
        roster.add("Jalen Smith", Uuid::from_u128(1));
        roster.add("Jaden Smith", Uuid::from_u128(2));
        assert_eq!(roster.resolve("J. Smith"), None);
        assert_eq!(roster.resolve("Jaden Smith"), Some(Uuid::from_u128(2)));
    }

    #[test]
    fn a_blank_player_name_falls_back_to_the_description() {
        assert_eq!(
            name_from_description("Adam Painter missed layup"),
            Some("Adam Painter")
        );
        assert_eq!(
            name_from_description("Nbaye Thiam sub in"),
            Some("Nbaye Thiam")
        );
        assert_eq!(name_from_description("media timeout"), None);
    }

    fn play(seq: &str, team: &str, name: &str, desc: &str, vis: &str, home: &str) -> Value {
        serde_json::json!({
            "period": &seq[..1], "sequence": seq, "team": "", "team_code": team,
            "player": "1", "player_name": name, "description": desc,
            "score_vis": vis, "score_home": home, "scoring_play": ""
        })
    }

    fn parse(entry: &Value) -> ParsedGame {
        let (h, v) = (Uuid::from_u128(10), Uuid::from_u128(20));
        let teams = HashMap::from([("HOME".to_string(), h), ("VIS".to_string(), v)]);
        let mut roster = RosterNames::default();
        roster.add("Ann Hoop", Uuid::from_u128(100));
        let rosters = HashMap::from([((Uuid::nil(), h), roster)]);
        parse_feed_game(entry, Uuid::nil(), 2027, &teams, &rosters)
    }

    #[test]
    fn plays_are_rebuilt_in_game_order_from_a_grouped_feed() {
        // The feed's grouping: shots first, subs later, rebounds last, with the
        // blank header row up front.
        let entry = serde_json::json!({
            "game-code": "1", "home-code": "HOME", "visitor-code": "VIS",
            "0": play("1-2060", "", "", "", "0", "0"),
            "1": play("1-0060", "VIS", "", "Start of game", "0", "0"),
            "2": play("1-0182", "HOME", "Ann Hoop", "Ann Hoop missed layup", "0", "0"),
            "3": play("1-0199", "HOME", "Ann Hoop", "Ann Hoop made layup", "0", "2"),
            "4": play("1-0199", "HOME", "", "Bea Bench sub in", "0", "2"),
            "5": play("1-0199", "VIS", "", "media timeout", "0", "0"),
            "6": play("1-01.9", "HOME", "Ann Hoop", "Ann Hoop offensive rebound", "0", "0"),
            "7": play("1-2060", "VIS", "", "End of period", "0", "0"),
        });
        let parsed = parse(&entry);
        let order: Vec<&str> = parsed
            .rows
            .iter()
            .map(|r| r.description.as_deref().unwrap())
            .collect();
        assert_eq!(
            order,
            [
                "Start of game",
                "Ann Hoop missed layup",
                "Ann Hoop offensive rebound",
                "media timeout",
                "Ann Hoop made layup",
                "Bea Bench sub in",
                "End of period",
            ]
        );
        assert_eq!(
            parsed.rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
        // The closing row's 0-0 does not undo the running score.
        let last = parsed.rows.last().unwrap();
        assert_eq!((last.score_home, last.score_vis), (Some(2), Some(0)));
        assert_eq!(parsed.rows[1].clock.as_deref(), Some("19:18"));
    }

    #[test]
    fn a_lagging_row_cannot_lower_the_running_score() {
        let entry = serde_json::json!({
            "game-code": "1", "home-code": "HOME", "visitor-code": "VIS",
            "0": play("2-1950", "HOME", "Ann Hoop", "Ann Hoop made layup", "50", "86"),
            "1": play("2-1990", "VIS", "", "Cy Road missed layup", "48", "84"),
        });
        let last = parse(&entry).rows.pop().unwrap();
        assert_eq!((last.score_home, last.score_vis), (Some(86), Some(50)));
    }

    #[test]
    fn made_shots_score_and_players_resolve_by_name() {
        let entry = serde_json::json!({
            "game-code": "1", "home-code": "HOME", "visitor-code": "VIS",
            "0": play("1-0199", "HOME", "Ann Hoop", "Ann Hoop made 3-point field goal", "0", "3"),
            "1": play("1-0255", "VIS", "", "Cy Road missed layup", "0", "3"),
            "2": play("1-0300", "HOME", "Nobody Known", "Nobody Known missed layup", "0", "3"),
        });
        let parsed = parse(&entry);
        let made = &parsed.rows[0];
        assert_eq!((made.points, made.scoring_play), (3, true));
        assert_eq!(made.player_id, Some(Uuid::from_u128(100)));
        assert_eq!(made.score_diff, Some(3));
        assert_eq!(parsed.rows[1].score_diff, Some(-3));
        // A visiting team with no box roster is not a resolution failure; a
        // rostered team naming a stranger is.
        assert_eq!(parsed.unresolved_players, 1);
    }

    #[test]
    fn a_game_with_no_plays_yet_parses_empty() {
        let entry = serde_json::json!({"game-code": "1", "home-code": "HOME"});
        assert!(parse(&entry).rows.is_empty());
    }
}
