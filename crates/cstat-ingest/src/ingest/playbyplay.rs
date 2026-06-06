//! Play-by-play ingestion — the API (intra-season) loader plus the shared
//! normalized row type and batch-insert used by BOTH loaders.
//!
//! Design of record: `docs/pbp_methodology.md`. The CSV bulk loader lives in
//! `bootstrap_csv.rs` (backfill path); it builds the same [`PbpRow`] values and
//! calls [`insert_pbp_rows`], so the normalization contract is defined once,
//! here. The raw `play_by_play` table is local-only (never synced to prod).
//!
//! API vs CSV shape (verified 2026-06-05): the API JSON is deeply nested
//! (`play.game.*`, `play.team.*`, `play.players.primary.*`) and carries the
//! on-floor lineup per row (`game.onfloorhome`/`onfloorvis`) but **no explicit
//! points** — points are derived from tags. The flat CSV has a Points column
//! but no lineup. Both collapse into the same [`PbpRow`].

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
/// 65535; at 15 columns/row we chunk well under that (1000 rows/statement).
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
             score_home, score_vis, score_diff) ",
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
                .push_bind(r.score_diff);
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
/// plays whose game is in `scope`, and stops as soon as a page yields zero
/// in-scope plays (or is empty / NO_DATA).
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
    let players = player_map(pool, season).await?;

    // Accumulate in-scope plays grouped by game (owned, since each page Value is
    // dropped between iterations).
    let mut by_game: HashMap<String, Vec<Value>> = HashMap::new();
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

        let plays = extract_results(&response);
        if plays.is_empty() {
            break;
        }
        let mut in_scope = 0u64;
        for play in plays {
            if let Some(code) = nested_str(play, &["game", "code"])
                && scope.contains(&code)
            {
                in_scope += 1;
                by_game.entry(code).or_default().push(play.clone());
            }
        }

        // Termination. A page with no in-scope plays is NOT a sufficient stop
        // signal on its own: a date can include non-D1 games we don't ingest,
        // and if a full page lands entirely on those (out of scope) *between*
        // two of our games, an eager break would silently drop the later games.
        // So we keep paging while any scope game is still unseen — the
        // date/daterange filter composes, so the real end arrives as an empty /
        // NO_DATA page. Once every scope game has been seen, the first
        // out-of-scope page is the tail and we stop. For a `gamecode` query
        // (scope size 1, filter does NOT compose past page 1) this trips the
        // moment that game's plays end, bounding the global-stream runaway.
        let all_seen = by_game.len() == scope.len();
        if in_scope == 0 && all_seen {
            break;
        }
        if page >= MAX_PAGES {
            warn!(range, page, "pbp: hit MAX_PAGES backstop — stopping");
            break;
        }
        offset += step;
        page += 1;
    }

    let mut report = PbpReport::default();
    for (game_code, mut plays) in by_game {
        let Some(&(game_id, game_season)) = games.get(&game_code) else {
            report.skipped_games += 1;
            continue;
        };
        // Chronological order: play `id` is a monotonically increasing int.
        // serde_json has no preserve_order feature here, so sort explicitly.
        plays.sort_by_key(|p| play_id(p).unwrap_or(i64::MAX));

        let rows: Vec<PbpRow> = plays
            .iter()
            .enumerate()
            .map(|(i, p)| parse_api_play(p, game_id, game_season, i as i32, &teams, &players))
            .collect();

        report.rows += replace_game_pbp(pool, game_id, &rows).await?;
        report.games += 1;
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

/// Parse one nested API play into a normalized [`PbpRow`].
fn parse_api_play(
    play: &Value,
    game_id: Uuid,
    season: i32,
    seq: i32,
    teams: &HashMap<String, Uuid>,
    players: &HashMap<String, Uuid>,
) -> PbpRow {
    let game = play.get("game");
    let period = nested_str(play, &["game", "period"])
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let sort_order = nested_str(play, &["game", "sequence"]);
    let clock = nonempty_str(game.and_then(|g| g.get("time")));
    let description = nonempty_str(play.get("explanation"));
    let tags = play
        .get("tags")
        .and_then(Value::as_str)
        .map(parse_tags)
        .unwrap_or_default();
    let points = points_from_tags(&tags);
    // Derive scoring_play from points rather than the source flag so the field
    // means the same thing on both loaders. The CSV `ScoringPlay` column omits
    // made free throws (and the API `scoringplay` field can disagree); points>0
    // is the consistent, source-agnostic definition.
    let scoring_play = points > 0;
    let score_home = nested_str(play, &["game", "score-home"]).and_then(|s| s.parse().ok());
    let score_vis = nested_str(play, &["game", "score-vis"]).and_then(|s| s.parse().ok());
    let score_diff = nested_str(play, &["thediff"]).and_then(|s| parse_score_diff(&s));

    // Acting team: the API identifies it by numeric id under `team.code`, but
    // our teams.natstat_id is the short abbrev. Bridge by matching the acting
    // team NAME to the game's home/visitor name, then take that side's code.
    let team_id = acting_team_abbrev(play).and_then(|abbrev| teams.get(&abbrev).copied());
    let player_id = nested_str(play, &["players", "primary", "code"])
        .and_then(|code| players.get(&code).copied());

    PbpRow {
        game_id,
        season,
        seq,
        sort_order,
        period,
        clock,
        team_id,
        player_id,
        description,
        scoring_play,
        points,
        tags,
        score_home,
        score_vis,
        score_diff,
    }
}

/// Resolve the acting team's short abbrev by matching its name against the
/// game's home/visitor names (the API gives us a numeric `team.code` that
/// doesn't match our abbrev-keyed teams table).
fn acting_team_abbrev(play: &Value) -> Option<String> {
    let team_name = nested_str(play, &["team", "team"])?;
    let home = nested_str(play, &["game", "home"]);
    let visitor = nested_str(play, &["game", "visitor"]);
    if visitor.as_deref() == Some(team_name.as_str()) {
        nested_str(play, &["game", "visitor-code"])
    } else if home.as_deref() == Some(team_name.as_str()) {
        nested_str(play, &["game", "home-code"])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Small JSON helpers (local to the API path)
// ---------------------------------------------------------------------------

/// Follow a nested key path and return the leaf as an owned String when it's a
/// non-empty string (NatStat returns scalars as strings).
fn nested_str(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str().map(str::to_string).filter(|s| !s.is_empty())
}

/// Coerce a value that NatStat sometimes returns as an empty object `{}` (for
/// absent `time`/`explanation`) into `Some(text)` only when it's a real string.
fn nonempty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Numeric play id used as the chronological sort key within a game.
fn play_id(play: &Value) -> Option<i64> {
    play.get("id").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
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
async fn team_abbrev_map(
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

/// `players.natstat_id -> players.id` for the season.
async fn player_map(pool: &PgPool, season: i32) -> Result<HashMap<String, Uuid>, NatStatError> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, natstat_id FROM players WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, code)| (code, id)).collect())
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
    fn acting_team_matches_visitor_side() {
        let play = serde_json::json!({
            "team": {"team": "Connecticut Huskies", "code": "255"},
            "game": {
                "home": "Michigan Wolverines", "home-code": "MICH",
                "visitor": "Connecticut Huskies", "visitor-code": "CONN"
            }
        });
        assert_eq!(acting_team_abbrev(&play), Some("CONN".to_string()));
    }

    #[test]
    fn acting_team_matches_home_side() {
        let play = serde_json::json!({
            "team": {"team": "Michigan Wolverines"},
            "game": {
                "home": "Michigan Wolverines", "home-code": "MICH",
                "visitor": "Connecticut Huskies", "visitor-code": "CONN"
            }
        });
        assert_eq!(acting_team_abbrev(&play), Some("MICH".to_string()));
    }

    #[test]
    fn nonempty_str_rejects_empty_object_and_string() {
        assert_eq!(nonempty_str(Some(&serde_json::json!({}))), None);
        assert_eq!(nonempty_str(Some(&serde_json::json!(""))), None);
        assert_eq!(
            nonempty_str(Some(&serde_json::json!("15:41.41"))),
            Some("15:41.41".to_string())
        );
    }
}
