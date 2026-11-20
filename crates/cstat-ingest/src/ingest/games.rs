use super::utils::{decode_html_entities, get_f64, get_i32, parse_i32};
use crate::NatStatClient;
use crate::client::NatStatError;
use crate::{extract_results, team_id_by_code_and_season};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use cstat_core::excluded_games::is_excluded;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// Why an upsert declined to write a row.
///
/// Every variant is a row the feed handed us that never reached the database.
/// Before #202 each of these was a bare `Ok(false)` the caller counted as "not
/// written" and then forgot: no log, no counter, no ledger note. An
/// under-fetched `games` step therefore orphaned every perf referencing a game
/// we had not stored, while the nightly still recorded `player_perfs` as `ok`,
/// posted a green Slack heartbeat, and reset the `/api/health/ingest`
/// staleness clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The payload carried no usable player / team / game code to key on.
    /// Malformed upstream record; always anomalous.
    MissingIdentifier,
    /// The game payload had no parseable `gameday`. Always anomalous.
    MissingDate,
    /// The referenced game has no `games` row. **This is the #202 alarm.** In a
    /// date-range run the `games` step covers the same window and runs first,
    /// so every perf's game should already be stored; a non-zero count means
    /// box scores were dropped on the floor.
    UnknownGame,
    /// The team code resolved to no `teams` row for this season. Routine and
    /// expected — NatStat's perf feeds carry the non-D1 side of a game and we
    /// only store D1 teams. Counted for completeness but deliberately **not**
    /// alarmed on: a warning that is non-zero every single night is how #232
    /// happened.
    UnresolvedTeam,
    /// The game is still being played. Its box score is a running total, and
    /// season aggregates must only ever see a finished game, so the line waits
    /// for the run after the final whistle. Routine on any run that overlaps a
    /// slate, so not alarmed on.
    GameInProgress,
    /// The game is on the curated exclusion list (`data/excluded_games.json`):
    /// the feed publishes it but it is not part of anyone's season. Deliberate,
    /// so not alarmed on.
    Excluded,
    /// NatStat lists, against a different opponent, a game we already hold as
    /// one CollegeBasketballData created (same team, same final score, within
    /// a couple of days): NatStat's wrong-opponent listing of that game. The
    /// created game, whose teams come from ESPN's ids, is kept, and NatStat's
    /// copy is not written, which would count the game twice for the team the
    /// two share. Anomalous: it names a disagreement worth a look.
    CbbdTwin,
}

/// Per-reason tally of rows an ingest loop declined to write.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SkipCounts {
    pub missing_identifier: u64,
    pub missing_date: u64,
    pub unknown_game: u64,
    pub unresolved_team: u64,
    pub game_in_progress: u64,
    pub excluded: u64,
    pub cbbd_twin: u64,
}

impl SkipCounts {
    pub(crate) fn record(&mut self, reason: SkipReason) {
        match reason {
            SkipReason::MissingIdentifier => self.missing_identifier += 1,
            SkipReason::MissingDate => self.missing_date += 1,
            SkipReason::UnknownGame => self.unknown_game += 1,
            SkipReason::UnresolvedTeam => self.unresolved_team += 1,
            SkipReason::GameInProgress => self.game_in_progress += 1,
            SkipReason::Excluded => self.excluded += 1,
            SkipReason::CbbdTwin => self.cbbd_twin += 1,
        }
    }

    pub fn total(&self) -> u64 {
        self.missing_identifier
            + self.missing_date
            + self.unknown_game
            + self.unresolved_team
            + self.game_in_progress
            + self.excluded
            + self.cbbd_twin
    }

    /// Skips that mean **we** lost data, as opposed to the routine non-D1
    /// filtering. Only these earn a `warn!` — see [`SkipReason::UnresolvedTeam`].
    pub fn anomalous(&self) -> u64 {
        self.missing_identifier + self.missing_date + self.unknown_game + self.cbbd_twin
    }

    /// Compact `reason=n` breakdown for `ingest_runs.notes`, or `None` when
    /// nothing was skipped. Reasons that did not fire are omitted, so the note
    /// is only ever as long as it needs to be.
    pub fn summary(&self) -> Option<String> {
        if self.total() == 0 {
            return None;
        }
        let parts: Vec<String> = [
            ("missing_identifier", self.missing_identifier),
            ("missing_date", self.missing_date),
            ("unknown_game", self.unknown_game),
            ("unresolved_team", self.unresolved_team),
            ("game_in_progress", self.game_in_progress),
            ("excluded", self.excluded),
            ("cbbd_twin", self.cbbd_twin),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(label, n)| format!("{label}={n}"))
        .collect();
        Some(format!("skipped {} ({})", self.total(), parts.join(", ")))
    }
}

/// What one ingest loop wrote, and what it threw away doing so.
///
/// Replaces the bare `u64` these functions used to return. Callers that only
/// need the count read [`IngestOutcome::written`]; the nightly additionally
/// stamps [`IngestOutcome::skipped`] into `ingest_runs.notes`, so the loss
/// survives log retention and is answerable from SQL months later.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub written: u64,
    pub skipped: SkipCounts,
}

impl IngestOutcome {
    fn record(&mut self, upsert: Upsert) {
        match upsert {
            Upsert::Wrote => self.written += 1,
            Upsert::Skipped(r) => self.skipped.record(r),
        }
    }

    /// The standard audit pair for a finished loop: the full breakdown always
    /// goes to `info!` next to the write count, and a `warn!` fires only when
    /// an anomalous reason is non-zero. `scope` is the caller's own context
    /// (season, window, team) rendered for the message.
    fn log(&self, what: &str, scope: &str) {
        let s = self.skipped;
        info!(
            written = self.written,
            skipped = s.total(),
            missing_identifier = s.missing_identifier,
            missing_date = s.missing_date,
            unknown_game = s.unknown_game,
            unresolved_team = s.unresolved_team,
            game_in_progress = s.game_in_progress,
            excluded = s.excluded,
            cbbd_twin = s.cbbd_twin,
            scope,
            "{what} ingested"
        );
        if s.anomalous() > 0 {
            warn!(
                missing_identifier = s.missing_identifier,
                missing_date = s.missing_date,
                unknown_game = s.unknown_game,
                cbbd_twin = s.cbbd_twin,
                scope,
                "{what}: rows dropped before write — the feed referenced records we could not store"
            );
        }
    }
}

/// Outcome of a single upsert: either the row was written, or it was declined
/// for a named reason. Replaces the `bool` these helpers used to return, whose
/// `false` arm carried no information at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upsert {
    Wrote,
    Skipped(SkipReason),
}

/// Shorthand for the three upsert helpers' return type.
type UpsertResult = Result<Upsert, NatStatError>;

/// Ingest all MBB games for a season, including player box scores via hydration.
///
/// Uses `games;boxscores` hydration to get game results + box scores in fewer API calls.
pub async fn ingest_games(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<IngestOutcome, NatStatError> {
    let pages = client
        .get_all_pages("games", Some(&season.to_string()), None)
        .await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let games = extract_results(page);
        for game in games {
            outcome.record(upsert_game(game, pool, season).await?);
        }
    }

    outcome.log("games", &format!("season={season}"));
    Ok(outcome)
}

/// Ingest games for a specific date range.
pub async fn ingest_games_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<IngestOutcome, NatStatError> {
    let range = format!("{},{}", start, end);
    let pages = client.get_all_pages("games", Some(&range), None).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let games = extract_results(page);
        for game in games {
            outcome.record(upsert_game(game, pool, season).await?);
        }
    }

    outcome.log("games", &format!("season={season} {start}..{end}"));
    Ok(outcome)
}

/// Ingest player performances (box scores) for all games in a season.
///
/// Uses the `playerperfs` endpoint filtered by season.
pub async fn ingest_player_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<IngestOutcome, NatStatError> {
    let pages = client
        .get_all_pages("playerperfs", Some(&season.to_string()), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Players, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_player_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log("player performances", &format!("season={season}"));
    Ok(outcome)
}

/// Ingest player performances for a specific team and season.
pub async fn ingest_player_performances_by_team(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<IngestOutcome, NatStatError> {
    let range = format!("{},{}", season, team_code);
    let pages = client
        .get_all_pages("playerperfs", Some(&range), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Players, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_player_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log(
        "player performances",
        &format!("season={season} team={team_code}"),
    );
    Ok(outcome)
}

/// Ingest player performances for a specific date range.
pub async fn ingest_player_performances_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<IngestOutcome, NatStatError> {
    let range = format!("{},{}", start, end);
    let pages = client
        .get_all_pages("playerperfs", Some(&range), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Players, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_player_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log(
        "player performances",
        &format!("season={season} {start}..{end}"),
    );
    Ok(outcome)
}

/// The two box-score tables a perf ingest writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxTable {
    Players,
    Teams,
}

impl BoxTable {
    fn name(self) -> &'static str {
        match self {
            BoxTable::Players => "player_game_stats",
            BoxTable::Teams => "team_game_stats",
        }
    }
}

/// Every distinct `(game code, team code)` the perf lines in `pages` carry.
pub fn perf_sides(pages: &[Value]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = pages
        .iter()
        .flat_map(extract_results)
        .filter_map(|perf| {
            let game = perf_game_code(perf)?;
            let team = perf
                .get("team-code")
                .or_else(|| perf.get("team_code"))
                .or_else(|| perf.get("team").and_then(|t| t.get("code")))
                .and_then(|c| c.as_str())?;
            Some((game, team.to_string()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Delete the CBBD gap-fill's rows (`source = 'cbbd'`, migration 077) for
/// every game side NatStat is about to write, so NatStat's box replaces the
/// fill instead of landing beside it: a player NatStat ids differently from
/// the fill's would otherwise get two lines, and the team two rows' worth of
/// minutes. Per side rather than per game, so a feed carrying one team's lines
/// leaves the other team's fill in place. Only games whose status lets the
/// lines through (`is_settled_status`), so a game the lines will be skipped
/// for keeps its fill. One statement for the whole batch.
pub async fn clear_cbbd_rows(
    pool: &PgPool,
    table: BoxTable,
    season: i32,
    pages: &[Value],
) -> Result<u64, sqlx::Error> {
    let sides = perf_sides(pages);
    if sides.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM {} s \
         USING games g, teams t, UNNEST($1::text[], $2::text[]) AS u(game, team) \
         WHERE g.natstat_id = u.game AND t.natstat_id = u.team AND t.season = $3 \
           AND s.game_id = g.id AND s.team_id = t.id AND s.source = 'cbbd' \
           AND (g.status IS NULL OR btrim(g.status) = '' \
                OR lower(btrim(g.status)) LIKE 'final%')",
        table.name()
    );
    let done = sqlx::query(&sql)
        .bind(sides.iter().map(|s| s.0.as_str()).collect::<Vec<_>>())
        .bind(sides.iter().map(|s| s.1.as_str()).collect::<Vec<_>>())
        .bind(season)
        .execute(pool)
        .await?;
    if done.rows_affected() > 0 {
        info!(
            table = table.name(),
            rows = done.rows_affected(),
            "NatStat box replaces CBBD gap-filled rows"
        );
    }
    Ok(done.rows_affected())
}

/// How far apart, in days, NatStat's date for a game and the date a
/// CBBD-created row carries may be for one to adopt the other. NatStat lists
/// some games a day or two off (Arkansas–Indiana on Nov 16 against ESPN's
/// Nov 18), and a pair of teams does not meet twice inside the window.
pub const ADOPT_DATE_SLACK_DAYS: i32 = 2;

/// The same for a NatStat game that shares only one team with a created game
/// ([`cbbd_twins`]). Tighter, because one team and a score is weaker evidence
/// than both teams: a team does not post the same final two days running
/// often, but it can.
pub const TWIN_DATE_SLACK_DAYS: i32 = 1;

/// One NatStat game about to be written, as [`adopt_cbbd_games`] matches it.
#[derive(Debug, Clone)]
pub struct FeedGame {
    pub natstat_id: String,
    pub game_date: NaiveDate,
    pub home_team_id: Option<Uuid>,
    pub away_team_id: Option<Uuid>,
    /// The final score, when the feed has one (only [`cbbd_twins`] reads it).
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
}

/// Hand CBBD-created games (docs/natstat_migration_plan.md §4.5 step 5,
/// `cstat_core::source_ids`) to the NatStat games about to be written, so
/// NatStat's row updates them instead of landing beside them as a second copy
/// of the game. Call before inserting `feed`; returns the games adopted.
///
/// A feed game adopts a CBBD-created game of the same season between the same
/// two teams, either way round, within [`ADOPT_DATE_SLACK_DAYS`]; only when the
/// match is one-to-one, and only for a natstat id not already stored. The
/// adopted row takes NatStat's id and NatStat's orientation: when NatStat lists
/// the teams the other way round, the game's teams and scores are swapped, and
/// so is everything stored in the game's home/away frame (box rows' `is_home`,
/// play-by-play scores and on-floor lists), so the NatStat upserts that follow
/// see a row already in their own frame.
pub async fn adopt_cbbd_games(
    conn: &mut sqlx::PgConnection,
    season: i32,
    feed: &[FeedGame],
) -> Result<u64, sqlx::Error> {
    let feed: Vec<&FeedGame> = feed
        .iter()
        .filter(|f| f.home_team_id.is_some() && f.away_team_id.is_some())
        .collect();
    if feed.is_empty() {
        return Ok(0);
    }
    let adopted: Vec<(Uuid, bool)> = sqlx::query_as(
        "WITH feed AS ( \
             SELECT * FROM UNNEST($2::text[], $3::date[], $4::uuid[], $5::uuid[]) \
                 AS f(natstat_id, game_date, home, away) \
             WHERE NOT EXISTS (SELECT 1 FROM games x WHERE x.natstat_id = f.natstat_id) \
         ), \
         pairs AS ( \
             SELECT f.natstat_id, g.id, (g.home_team_id = f.away) AS swapped \
             FROM feed f \
             JOIN games g ON g.season = $1 AND starts_with(g.natstat_id, $6) \
              AND abs(g.game_date - f.game_date) <= $7 \
              AND ((g.home_team_id = f.home AND g.away_team_id = f.away) \
                   OR (g.home_team_id = f.away AND g.away_team_id = f.home)) \
         ), \
         uniq AS ( \
             SELECT p.* FROM pairs p \
             WHERE (SELECT count(*) FROM pairs q WHERE q.natstat_id = p.natstat_id) = 1 \
               AND (SELECT count(*) FROM pairs q WHERE q.id = p.id) = 1 \
         ) \
         UPDATE games g SET \
             natstat_id = u.natstat_id, \
             home_team_id = CASE WHEN u.swapped THEN g.away_team_id ELSE g.home_team_id END, \
             away_team_id = CASE WHEN u.swapped THEN g.home_team_id ELSE g.away_team_id END, \
             home_score = CASE WHEN u.swapped THEN g.away_score ELSE g.home_score END, \
             away_score = CASE WHEN u.swapped THEN g.home_score ELSE g.away_score END, \
             home_half1 = CASE WHEN u.swapped THEN g.away_half1 ELSE g.home_half1 END, \
             home_half2 = CASE WHEN u.swapped THEN g.away_half2 ELSE g.home_half2 END, \
             away_half1 = CASE WHEN u.swapped THEN g.home_half1 ELSE g.away_half1 END, \
             away_half2 = CASE WHEN u.swapped THEN g.home_half2 ELSE g.away_half2 END, \
             updated_at = now() \
         FROM uniq u WHERE g.id = u.id \
         RETURNING g.id, u.swapped",
    )
    .bind(season)
    .bind(
        feed.iter()
            .map(|f| f.natstat_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind(feed.iter().map(|f| f.game_date).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.home_team_id).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.away_team_id).collect::<Vec<_>>())
    .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
    .bind(ADOPT_DATE_SLACK_DAYS)
    .fetch_all(&mut *conn)
    .await?;
    let swapped: Vec<Uuid> = adopted.iter().filter(|a| a.1).map(|a| a.0).collect();
    if !swapped.is_empty() {
        for table in ["team_game_stats", "player_game_stats"] {
            sqlx::query(&format!(
                "UPDATE {table} s SET is_home = (s.team_id = g.home_team_id \
                                                 AND NOT g.is_neutral_site) \
                 FROM games g WHERE g.id = s.game_id AND s.game_id = ANY($1)"
            ))
            .bind(&swapped)
            .execute(&mut *conn)
            .await?;
        }
        sqlx::query(
            "UPDATE play_by_play SET score_home = score_vis, score_vis = score_home, \
                 score_diff = -score_diff, onfloor_home = onfloor_vis, onfloor_vis = onfloor_home \
             WHERE game_id = ANY($1)",
        )
        .bind(&swapped)
        .execute(&mut *conn)
        .await?;
        // The ESPN event's orientation relative to ours flips with ours.
        sqlx::query(
            "UPDATE game_broadcasts SET source_home_is_away = NOT source_home_is_away \
             WHERE game_id = ANY($1) AND source_home_is_away IS NOT NULL",
        )
        .bind(&swapped)
        .execute(&mut *conn)
        .await?;
    }
    if !adopted.is_empty() {
        info!(
            season,
            adopted = adopted.len(),
            swapped = swapped.len(),
            "NatStat adopted CBBD-created games"
        );
    }
    Ok(adopted.len() as u64)
}

/// The feed games, among those not stored yet, that are a CBBD-created game
/// under a different opponent ([`SkipReason::CbbdTwin`]): a created game shares
/// one of their teams, within [`TWIN_DATE_SLACK_DAYS`], with that team's
/// final score and its opponent's the same. Call after [`adopt_cbbd_games`],
/// which has already taken every feed game that names both teams; what is
/// left matching one team and the score is NatStat naming the wrong opponent
/// (as it listed UNC Wilmington's 100-41 win over Montreat against
/// Louisiana). The caller does not write these.
pub async fn cbbd_twins(
    conn: &mut sqlx::PgConnection,
    season: i32,
    feed: &[FeedGame],
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let feed: Vec<&FeedGame> = feed
        .iter()
        .filter(|f| f.home_score.is_some() && f.away_score.is_some())
        .collect();
    if feed.is_empty() {
        return Ok(Default::default());
    }
    let twins: Vec<String> = sqlx::query_scalar(
        "SELECT f.natstat_id \
         FROM UNNEST($2::text[], $3::date[], $4::uuid[], $5::uuid[], $6::int[], $7::int[]) \
             AS f(natstat_id, game_date, home, away, hs, aws) \
         WHERE NOT EXISTS (SELECT 1 FROM games x WHERE x.natstat_id = f.natstat_id \
                           AND (x.home_score IS NOT NULL OR x.away_score IS NOT NULL)) \
           AND EXISTS ( \
               SELECT 1 FROM games g \
               WHERE g.season = $1 AND starts_with(g.natstat_id, $8) \
                 AND abs(g.game_date - f.game_date) <= $9 \
                 AND ((g.home_team_id = f.home AND g.home_score = f.hs AND g.away_score = f.aws) \
                   OR (g.away_team_id = f.home AND g.away_score = f.hs AND g.home_score = f.aws) \
                   OR (g.home_team_id = f.away AND g.home_score = f.aws AND g.away_score = f.hs) \
                   OR (g.away_team_id = f.away AND g.away_score = f.aws AND g.home_score = f.hs)))",
    )
    .bind(season)
    .bind(
        feed.iter()
            .map(|f| f.natstat_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind(feed.iter().map(|f| f.game_date).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.home_team_id).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.away_team_id).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.home_score).collect::<Vec<_>>())
    .bind(feed.iter().map(|f| f.away_score).collect::<Vec<_>>())
    .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
    .bind(TWIN_DATE_SLACK_DAYS)
    .fetch_all(&mut *conn)
    .await?;
    if !twins.is_empty() {
        // A twin NatStat first listed unscored (scheduled, or stuck in
        // progress) is already stored: its score arriving now is what shows
        // it is the created game, so the row goes, unless something already
        // hangs off it, which then stays and is reported.
        let blocked = super::schedule::PRUNE_BLOCKERS
            .iter()
            .filter(|t| **t != "game_forecasts")
            .map(|t| format!("EXISTS (SELECT 1 FROM {t} b WHERE b.game_id = g.id)"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let removed = sqlx::query_scalar::<_, i64>(&format!(
            "WITH doomed AS ( \
                 SELECT g.id FROM games g \
                 WHERE g.natstat_id = ANY($1) AND g.home_score IS NULL AND g.away_score IS NULL \
                   AND NOT ({blocked}) \
             ), \
             s AS (DELETE FROM schedules WHERE game_id IN (SELECT id FROM doomed)), \
             f AS (DELETE FROM game_forecasts WHERE game_id IN (SELECT id FROM doomed)), \
             d AS (DELETE FROM games WHERE id IN (SELECT id FROM doomed) RETURNING 1) \
             SELECT count(*) FROM d"
        ))
        .bind(&twins)
        .fetch_one(&mut *conn)
        .await?;
        warn!(
            season,
            ?twins,
            removed,
            "NatStat lists CBBD-created games against a different opponent; not written"
        );
    }
    Ok(twins.into_iter().collect())
}

/// A name reduced to compare a NatStat roster name with a CBBD box name:
/// [`crate::name_match::name_key`], so `Jalen St Clair` and `Jalen StClair`
/// agree, and so do `Roko Prkačin` and `Roko Prkacin`.
pub fn squashed_name(name: &str) -> String {
    crate::name_match::name_key(name)
}

/// Hand a CBBD-created player (`cstat_core::source_ids`) to the NatStat player
/// about to be inserted, so NatStat's id lands on the row that already holds
/// his CBBD box lines instead of minting the person twice. Call only when no
/// row carries `natstat_id` this season. Matches a CBBD-created player on the
/// same team and season whose name agrees once squashed ([`squashed_name`]),
/// and only when exactly one does. Returns the adopted player's id.
pub async fn adopt_cbbd_player(
    pool: &PgPool,
    natstat_id: &str,
    season: i32,
    team_id: Uuid,
    name: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let candidates: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM players \
         WHERE season = $1 AND team_id = $2 AND starts_with(natstat_id, $3)",
    )
    .bind(season)
    .bind(team_id)
    .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
    .fetch_all(pool)
    .await?;
    let key = squashed_name(name);
    if key.is_empty() {
        return Ok(None);
    }
    let mut hits = candidates.iter().filter(|(_, n)| squashed_name(n) == key);
    let (Some((id, _)), None) = (hits.next(), hits.next()) else {
        return Ok(None);
    };
    let done = sqlx::query(
        "UPDATE players SET natstat_id = $2, updated_at = now() \
         WHERE id = $1 AND starts_with(natstat_id, $4) \
           AND NOT EXISTS (SELECT 1 FROM players x WHERE x.natstat_id = $2 AND x.season = $3)",
    )
    .bind(id)
    .bind(natstat_id)
    .bind(season)
    .bind(cstat_core::source_ids::CBBD_CREATED_PREFIX)
    .execute(pool)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(None);
    }
    info!(natstat_id, season, %id, name, "NatStat adopted a CBBD-created player");
    Ok(Some(*id))
}

/// One `games` feed record, parsed but not yet resolved against `teams`.
///
/// Shared by the per-row [`upsert_game`] and the batched season refresh in
/// `ingest::schedule`, so the two cannot read the same payload differently.
#[derive(Debug, Clone, PartialEq)]
pub struct GameRow {
    pub natstat_id: String,
    pub game_date: NaiveDate,
    pub home_code: Option<String>,
    pub away_code: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    pub is_neutral: bool,
    pub is_conference: Option<bool>,
    pub is_postseason: Option<bool>,
    pub venue: Option<String>,
    pub venue_code: Option<String>,
    /// City and state (or country) of the venue, from `venue.location`.
    pub venue_location: Option<String>,
    /// ET wall-clock tipoff from `gamedate`. `None` when the feed has no time.
    pub tip_time: Option<NaiveTime>,
    pub overtime: Option<String>,
    pub attendance: Option<i32>,
    pub status: Option<String>,
    pub home_half1: Option<i32>,
    pub home_half2: Option<i32>,
    pub away_half1: Option<i32>,
    pub away_half2: Option<i32>,
}

/// Whether a `gamestatus` describes a game whose score is final.
///
/// The feed lists a game in progress with its running score and a clock
/// (`H2 12:28`, `Half`), so a run that overlaps a slate would otherwise store
/// a half-time score as the result, and everything downstream — W-L, four
/// factors, AdjEM, player season stats — would aggregate it as a finished
/// game. Only `Final` (and its overtime spellings) is settled. No status at
/// all, or a blank one, is also treated as settled: older payload shapes and
/// the replay harness's fixtures carry a score with no status, a scheduled
/// game carries neither, and some stored rows hold `''` rather than NULL, so
/// nothing is lost by trusting the score there.
pub fn is_settled_status(status: Option<&str>) -> bool {
    status
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none_or(|s| s.to_ascii_lowercase().starts_with("final"))
}

/// Parse one `games` record, or name why it cannot be stored.
pub fn parse_game(game: &Value) -> Result<GameRow, SkipReason> {
    let natstat_id = match game
        .get("id")
        .or_else(|| game.get("code"))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Err(SkipReason::MissingIdentifier),
    };
    if is_excluded(&natstat_id) {
        return Err(SkipReason::Excluded);
    }

    let game_date = match game
        .get("gameday")
        .or_else(|| game.get("gamedate"))
        .or_else(|| game.get("date"))
        .and_then(|d| d.as_str())
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    {
        Some(d) => d,
        None => {
            warn!(natstat_id, "game missing date, skipping");
            return Err(SkipReason::MissingDate);
        }
    };

    // Resolve home/away team IDs — NatStat v4 uses "home-code" / "visitor-code"
    let home_code = game
        .get("home-code")
        .or_else(|| game.get("home_code"))
        .and_then(|c| c.as_str());
    let away_code = game
        .get("visitor-code")
        .or_else(|| game.get("away-code"))
        .or_else(|| game.get("away_code"))
        .and_then(|c| c.as_str());

    let home_score = game
        .get("score-home")
        .or_else(|| game.get("home_score"))
        .and_then(|s| {
            s.as_i64()
                .or_else(|| s.as_str().and_then(|v| v.parse().ok()))
        })
        .map(|s| s as i32);

    let away_score = game
        .get("score-vis")
        .or_else(|| game.get("away_score"))
        .and_then(|s| {
            s.as_i64()
                .or_else(|| s.as_str().and_then(|v| v.parse().ok()))
        })
        .map(|s| s as i32);

    let is_neutral = game
        .get("neutral")
        .or_else(|| game.get("is_neutral"))
        .and_then(|n| {
            n.as_bool()
                .or_else(|| n.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| n.as_i64().map(|i| i != 0))
        })
        .unwrap_or(false);

    let is_conference = game
        .get("conference")
        .or_else(|| game.get("is_conference"))
        .and_then(|c| {
            c.as_bool()
                .or_else(|| c.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| c.as_i64().map(|i| i != 0))
        });

    let is_postseason = game
        .get("postseason")
        .or_else(|| game.get("is_postseason"))
        .and_then(|p| {
            p.as_bool()
                .or_else(|| p.as_str().map(|s| s == "Y" || s == "1"))
                .or_else(|| p.as_i64().map(|i| i != 0))
        });

    let venue = game
        .get("venue")
        .and_then(|v| v.get("name").or(Some(v)))
        .and_then(|v| v.as_str())
        .or_else(|| game.get("venue-name").and_then(|v| v.as_str()));

    let venue_code = game
        .get("venue-code")
        .or_else(|| game.get("venue").and_then(|v| v.get("code")))
        .and_then(|c| c.as_str());

    let venue_location = game
        .get("venue")
        .and_then(|v| v.get("location"))
        .and_then(|l| l.as_str())
        .map(str::trim)
        .filter(|l| !l.is_empty());

    let tip_time = game
        .get("gamedate")
        .and_then(|d| d.as_str())
        .and_then(parse_tip_time);

    let overtime = game
        .get("overtime")
        .and_then(|o| o.as_str())
        .filter(|o| *o != "N");

    let attendance = game
        .get("attendance")
        .and_then(|a| {
            a.as_i64()
                .or_else(|| a.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|a| a as i32);

    // Blank on every scheduled game. Treated as absent, so a refresh carrying
    // "" cannot blank a stored status through the `COALESCE` below.
    let status = game
        .get("gamestatus")
        .or_else(|| game.get("status"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.trim().is_empty());

    // Half scores: line-home/line-vis contain {p1: "32", p2: "35"}
    let home_half1 = game
        .get("line-home")
        .and_then(|l| l.get("p1"))
        .and_then(parse_i32);
    let home_half2 = game
        .get("line-home")
        .and_then(|l| l.get("p2"))
        .and_then(parse_i32);
    let away_half1 = game
        .get("line-vis")
        .and_then(|l| l.get("p1"))
        .and_then(parse_i32);
    let away_half2 = game
        .get("line-vis")
        .and_then(|l| l.get("p2"))
        .and_then(parse_i32);

    // A running score is not a result: it is left out, so the row stays
    // unplayed until a run sees the game final. The status is still stored,
    // which is what holds the game's box lines back (`GameInProgress`).
    let settled = is_settled_status(status);
    let (home_score, away_score) = if settled {
        (home_score, away_score)
    } else {
        (None, None)
    };
    let (home_half1, home_half2, away_half1, away_half2) = if settled {
        (home_half1, home_half2, away_half1, away_half2)
    } else {
        (None, None, None, None)
    };

    Ok(GameRow {
        natstat_id,
        game_date,
        home_code: home_code.map(String::from),
        away_code: away_code.map(String::from),
        home_score,
        away_score,
        is_neutral,
        is_conference,
        is_postseason,
        venue: venue.map(String::from),
        venue_code: venue_code.map(String::from),
        venue_location: venue_location.map(String::from),
        tip_time,
        overtime: overtime.map(String::from),
        attendance,
        status: status.map(String::from),
        home_half1,
        home_half2,
        away_half1,
        away_half2,
    })
}

/// The tipoff time in a NatStat `gamedate` (`2026-11-02 19:00:00`, ET).
///
/// `00:00:00` is NatStat's "time not set", not a midnight tip, so it is `None`:
/// 2,023 of the 5,510 games on the 2026-27 schedule carry it, Duke–Army and
/// Kansas–Fordham on opening night among them. Real midnight tips do not occur
/// in a D-I schedule; reading the sentinel as one would put a third of the
/// season at 12:00 AM. A bare date (no time part) is also `None`.
pub fn parse_tip_time(gamedate: &str) -> Option<NaiveTime> {
    let dt = NaiveDateTime::parse_from_str(gamedate.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    let t = dt.time();
    (t != NaiveTime::MIN).then_some(t)
}

async fn upsert_game(game: &Value, pool: &PgPool, season: i32) -> UpsertResult {
    let g = match parse_game(game) {
        Ok(g) => g,
        Err(reason) => return Ok(Upsert::Skipped(reason)),
    };
    let home_team_id = team_id_by_code_and_season(pool, g.home_code.as_deref(), season).await?;
    let away_team_id = team_id_by_code_and_season(pool, g.away_code.as_deref(), season).await?;

    // A game CBBD created while NatStat lacked it becomes this one; one
    // NatStat lists against a different opponent is not written twice.
    if home_team_id.is_some() || away_team_id.is_some() {
        let feed = [FeedGame {
            natstat_id: g.natstat_id.clone(),
            game_date: g.game_date,
            home_team_id,
            away_team_id,
            home_score: g.home_score,
            away_score: g.away_score,
        }];
        let mut conn = pool.acquire().await?;
        adopt_cbbd_games(&mut conn, season, &feed).await?;
        if !cbbd_twins(&mut conn, season, &feed).await?.is_empty() {
            return Ok(Upsert::Skipped(SkipReason::CbbdTwin));
        }
    }

    sqlx::query(
        "INSERT INTO games (id, natstat_id, season, game_date, home_team_id, away_team_id,
         home_score, away_score, is_neutral_site, is_conference, is_postseason, venue,
         venue_code, overtime, attendance, status,
         home_half1, home_half2, away_half1, away_half2, venue_location, tip_time)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)
         ON CONFLICT (natstat_id) DO UPDATE
         SET home_score = COALESCE(EXCLUDED.home_score, games.home_score),
             away_score = COALESCE(EXCLUDED.away_score, games.away_score),
             home_team_id = COALESCE(EXCLUDED.home_team_id, games.home_team_id),
             away_team_id = COALESCE(EXCLUDED.away_team_id, games.away_team_id),
             -- Promote-only, like the schedule refresh. The feed does not
             -- flag a scheduled game's neutral site (nor, on the night, a
             -- finished one's), so taking its value reset the neutral sites
             -- `compute_all` derives (#400) on every game the nightly window
             -- touched: the 2026 opening night lost all fifteen,
             -- Purdue-Gonzaga in Las Vegas among them.
             is_neutral_site = games.is_neutral_site OR EXCLUDED.is_neutral_site,
             venue = COALESCE(EXCLUDED.venue, games.venue),
             venue_code = COALESCE(EXCLUDED.venue_code, games.venue_code),
             overtime = COALESCE(EXCLUDED.overtime, games.overtime),
             attendance = COALESCE(EXCLUDED.attendance, games.attendance),
             -- A game the CBBD gap-fill finished (`score_source = 'cbbd'`)
             -- keeps its Final until NatStat sends a final score of its own:
             -- the feed lags by days, and its stale in-progress status
             -- (`H2 05:11`) would otherwise undo the fill every night.
             status = CASE
                 WHEN games.score_source IS NOT NULL AND EXCLUDED.home_score IS NULL
                     THEN games.status
                 ELSE COALESCE(EXCLUDED.status, games.status)
             END,
             score_source = CASE
                 WHEN EXCLUDED.home_score IS NOT NULL THEN NULL
                 ELSE games.score_source
             END,
             home_half1 = COALESCE(EXCLUDED.home_half1, games.home_half1),
             home_half2 = COALESCE(EXCLUDED.home_half2, games.home_half2),
             away_half1 = COALESCE(EXCLUDED.away_half1, games.away_half1),
             away_half2 = COALESCE(EXCLUDED.away_half2, games.away_half2),
             venue_location = COALESCE(EXCLUDED.venue_location, games.venue_location),
             tip_time = COALESCE(EXCLUDED.tip_time, games.tip_time),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(&g.natstat_id)
    .bind(season)
    .bind(g.game_date)
    .bind(home_team_id)
    .bind(away_team_id)
    .bind(g.home_score)
    .bind(g.away_score)
    .bind(g.is_neutral)
    .bind(g.is_conference)
    .bind(g.is_postseason)
    .bind(&g.venue)
    .bind(&g.venue_code)
    .bind(&g.overtime)
    .bind(g.attendance)
    .bind(&g.status)
    .bind(g.home_half1)
    .bind(g.home_half2)
    .bind(g.away_half1)
    .bind(g.away_half2)
    .bind(&g.venue_location)
    .bind(g.tip_time)
    .execute(pool)
    .await?;

    Ok(Upsert::Wrote)
}

// Box-score field names. The live v4 feed (verified 2026-11-02 on both the
// season-wide and the date-range form of `playerperfs` and `teamperfs`) spells
// them as UPPERCASE glossary abbreviations — `MIN`, `3FM`, `OREB`, `TO`, `PF` —
// and nests a team's line under `groups.ALL.stats`. This parser was written
// against the lowercase codes (`min`, `threefm`, `oreb`) the v4 beta returned
// in April, which is also what `simulate` synthesizes, so the replay harness
// kept passing while the live feed drifted: the first in-season nightly wrote
// every player line with NULL stats and dropped every team line. Both
// spellings are read, live first.
const K_MIN: &[&str] = &["MIN", "min", "minutes", "mp"];
const K_PTS: &[&str] = &["PTS", "pts", "points"];
const K_FGM: &[&str] = &["FGM", "fgm", "fg"];
const K_FGA: &[&str] = &["FGA", "fga"];
const K_3FM: &[&str] = &["3FM", "threefm", "tpm", "3pm", "fg3m"];
const K_3FA: &[&str] = &["3FA", "threefa", "tpa", "3pa", "fg3a"];
const K_FTM: &[&str] = &["FTM", "ftm", "ft"];
const K_FTA: &[&str] = &["FTA", "fta"];
const K_OREB: &[&str] = &["OREB", "oreb", "orb"];
const K_REB: &[&str] = &["REB", "reb"];
const K_DREB: &[&str] = &["DREB", "dreb", "drb"];
const K_AST: &[&str] = &["AST", "ast", "assists"];
const K_TO: &[&str] = &["TO", "to", "tov"];
const K_STL: &[&str] = &["STL", "stl", "steals"];
const K_BLK: &[&str] = &["BLK", "blk", "blocks"];
const K_PF: &[&str] = &["PF", "pf", "f", "fouls"];

/// The game code a box-score row belongs to: `game-code` in the live feed,
/// `game.id` / `game.code` in the shapes the replay harness synthesizes.
fn perf_game_code(perf: &Value) -> Option<String> {
    perf.get("game-code")
        .or_else(|| perf.get("game_code"))
        .or_else(|| perf.get("game").and_then(|g| g.get("code")))
        .or_else(|| perf.get("game").and_then(|g| g.get("id")))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
        .map(|s| s.trim_matches('"').to_string())
}

/// Where a team's line keeps its numbers: `groups.ALL.stats` in the live
/// feed, a `stats` object or the row itself in older shapes.
fn team_perf_stats(perf: &Value) -> &Value {
    perf.get("groups")
        .and_then(|g| g.get("ALL"))
        .and_then(|a| a.get("stats"))
        .or_else(|| perf.get("stats"))
        .unwrap_or(perf)
}

/// The `games` row a box-score line is checked against, with what it takes
/// to derive the context the live feed no longer sends on the line itself.
#[derive(sqlx::FromRow)]
struct GameSides {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Option<Uuid>,
    away_team_id: Option<Uuid>,
    home_score: Option<i32>,
    away_score: Option<i32>,
    is_neutral_site: Option<bool>,
    status: Option<String>,
}

async fn game_sides(pool: &PgPool, natstat_id: &str) -> Result<Option<GameSides>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, game_date, home_team_id, away_team_id, home_score, away_score, is_neutral_site,
                status
         FROM games WHERE natstat_id = $1",
    )
    .bind(natstat_id)
    .fetch_optional(pool)
    .await
}

/// `(opponent, is_home, win)` for `team` in `game`, from the game row. The
/// live box-score rows carry no `opponent`, no `game.loc` and no
/// `game.winorloss`; without this every player line lost its opponent, which
/// player SOS joins on. `None` where the row cannot say (the team is not one
/// of the two sides, or the score is not final). A neutral-site game is
/// `is_home = false` for both sides, as `loc = "N"` was.
fn side_context(game: &GameSides, team: Uuid) -> (Option<Uuid>, Option<bool>, Option<bool>) {
    let (opp, own_score, opp_score, home) = if game.home_team_id == Some(team) {
        (game.away_team_id, game.home_score, game.away_score, true)
    } else if game.away_team_id == Some(team) {
        (game.home_team_id, game.away_score, game.home_score, false)
    } else {
        return (None, None, None);
    };
    let is_home = Some(home && game.is_neutral_site != Some(true));
    let win = own_score.zip(opp_score).map(|(a, b)| a > b);
    (opp, is_home, win)
}

async fn upsert_player_game_stats(perf: &Value, pool: &PgPool, season: i32) -> UpsertResult {
    // Get player code — NatStat v4 uses "player-code" or nested player.code
    let player_natstat_id = match perf
        .get("player-code")
        .or_else(|| perf.get("player_code"))
        .or_else(|| perf.get("player").and_then(|p| p.get("code")))
        .and_then(|c| c.as_str().map(String::from).or_else(|| Some(c.to_string())))
    {
        Some(id) => id.trim_matches('"').to_string(),
        None => return Ok(Upsert::Skipped(SkipReason::MissingIdentifier)),
    };

    let Some(game_natstat_id) = perf_game_code(perf) else {
        return Ok(Upsert::Skipped(SkipReason::MissingIdentifier));
    };

    if is_excluded(&game_natstat_id) {
        return Ok(Upsert::Skipped(SkipReason::Excluded));
    }

    // Resolve game first — if game is missing we can't store anything.
    let Some(game) = game_sides(pool, &game_natstat_id).await? else {
        return Ok(Upsert::Skipped(SkipReason::UnknownGame));
    };
    if !is_settled_status(game.status.as_deref()) {
        return Ok(Upsert::Skipped(SkipReason::GameInProgress));
    }
    let (game_id, game_date) = (game.id, game.game_date);

    // Resolve team — NatStat v4 uses "team-code" or nested team.code.
    //
    // The two failure modes are split deliberately. `team_id_by_code_and_season`
    // answers `None` both for "no code in the payload" and for "code resolves to
    // no D1 team", and collapsing them would file a MALFORMED payload under the
    // one class that is never alarmed on. That is not hypothetical at the scale
    // that matters: if NatStat renamed this field, every row would lose its code
    // at once, the step would write nothing, and the run summary would report
    // the whole outage as routine non-D1 filtering.
    let team_code = perf
        .get("team-code")
        .or_else(|| perf.get("team_code"))
        .or_else(|| perf.get("team").and_then(|t| t.get("code")))
        .and_then(|c| c.as_str());
    let Some(team_code) = team_code else {
        return Ok(Upsert::Skipped(SkipReason::MissingIdentifier));
    };
    let Some(team_id) = team_id_by_code_and_season(pool, Some(team_code), season).await? else {
        return Ok(Upsert::Skipped(SkipReason::UnresolvedTeam));
    };

    // Resolve player — auto-create a minimal record if we've never seen them before.
    // The NatStat /players endpoint can't give us historical rosters (it returns
    // current season only), so box scores are the authoritative source for who
    // played in a given season. The perf payload carries enough to stub out the
    // row; richer metadata can backfill later from other endpoints.
    let player_id: Uuid = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM players WHERE natstat_id = $1 AND season = $2",
    )
    .bind(&player_natstat_id)
    .bind(season)
    .fetch_optional(pool)
    .await?
    {
        Some((id,)) => id,
        None => {
            // Decoded for the same reason as `players.rs::upsert_player` —
            // NatStat HTML-escapes some records (#243).
            let name = decode_html_entities(
                perf.get("player")
                    .and_then(|n| n.as_str())
                    .or_else(|| {
                        perf.get("player-name")
                            .or_else(|| perf.get("player_name"))
                            .and_then(|n| n.as_str())
                    })
                    .unwrap_or("Unknown"),
            );
            // A player CBBD created while NatStat lacked him becomes this one.
            if let Some(id) =
                adopt_cbbd_player(pool, &player_natstat_id, season, team_id, &name).await?
            {
                id
            } else {
                let new_id = Uuid::new_v4();
                let (id,): (Uuid,) = sqlx::query_as(
                    "INSERT INTO players (id, natstat_id, name, team_id, season)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (natstat_id, season) DO UPDATE
                 SET team_id = COALESCE(EXCLUDED.team_id, players.team_id),
                     updated_at = now()
                 RETURNING id",
                )
                .bind(new_id)
                .bind(&player_natstat_id)
                .bind(&name)
                .bind(team_id)
                .bind(season)
                .fetch_one(pool)
                .await?;
                id
            }
        }
    };

    // playerperfs returns stats flat on the perf object (not nested under "stats")
    let minutes = get_f64(perf, K_MIN);
    let points = get_i32(perf, K_PTS);
    let fgm = get_i32(perf, K_FGM);
    let fga = get_i32(perf, K_FGA);
    let fg_pct = get_f64(perf, &["fgpct", "fg_pct", "fgp"]);
    let tpm = get_i32(perf, K_3FM);
    let tpa = get_i32(perf, K_3FA);
    let tp_pct = get_f64(perf, &["threefgpct", "tp_pct", "3p_pct"]);
    let ftm = get_i32(perf, K_FTM);
    let fta = get_i32(perf, K_FTA);
    let ft_pct = get_f64(perf, &["ftpct", "ft_pct", "ftp"]);
    let off_rebounds = get_i32(perf, K_OREB);
    // NatStat playerperfs: "reb" = total rebounds, "dreb" = defensive rebounds (when available).
    // ~68% of games have reb=0 (missing data) — NatStat simply doesn't have it for those games.
    // When reb=0 but oreb>0, treat as missing (NULL). Use "dreb" directly when present.
    let reb_raw = get_i32(perf, K_REB);
    let total_rebounds = match (reb_raw, off_rebounds) {
        (Some(0), Some(o)) if o > 0 => None, // missing data at source
        (r, _) => r,
    };
    let dreb_direct = get_i32(perf, K_DREB);
    let def_rebounds = dreb_direct.or_else(|| {
        total_rebounds
            .zip(off_rebounds)
            .map(|(t, o)| t - o)
            .filter(|&d| d >= 0)
    });
    let assists = get_i32(perf, K_AST);
    let turnovers = get_i32(perf, K_TO);
    let steals = get_i32(perf, K_STL);
    let blocks = get_i32(perf, K_BLK);
    let fouls = get_i32(perf, K_PF);
    let plus_minus = get_i32(perf, &["plus_minus", "pm"]);

    // NatStat advanced metrics
    let starter = perf
        .get("starter")
        .and_then(|s| s.as_str())
        .map(|s| s == "Y");
    let efficiency = get_f64(perf, &["eff", "efficiency"]);
    let usage_rate = get_f64(perf, &["usgpct", "usage_rate"]).map(|v| v / 100.0);
    let two_fg_pct = get_f64(perf, &["twofgpct"]);
    let presence_rate = get_f64(perf, &["presencerate"]);
    let adj_presence_rate = get_f64(perf, &["adjpresencerate"]);
    let perf_score = get_f64(perf, &["perfscore"]);
    let perf_score_season_avg = get_f64(perf, &["perfscoreseasonavg"]);
    let team_possessions = get_i32(perf, &["teamposs"]);

    // Team context stats (needed for rate stat calculations)
    let team_fga = get_i32(perf, &["teamfga"]);
    let team_fta = get_i32(perf, &["teamfta"]);
    let team_turnovers = get_i32(perf, &["teamto"]);
    let team_fgm = get_i32(perf, &["teamfgm"]);

    // Update player jersey number if available
    let jersey_number = perf
        .get("player-number")
        .and_then(|n| n.as_str().map(String::from).or_else(|| Some(n.to_string())))
        .map(|s| s.trim_matches('"').to_string());
    if let Some(ref jersey) = jersey_number {
        sqlx::query("UPDATE players SET jersey_number = $1, updated_at = now() WHERE id = $2 AND jersey_number IS NULL")
            .bind(jersey)
            .bind(player_id)
            .execute(pool)
            .await?;
    }

    // Game context: is_home from game.loc, else from the game row.
    let (row_opponent, row_is_home, _) = side_context(&game, team_id);
    let is_home = perf
        .get("game")
        .and_then(|g| g.get("loc"))
        .and_then(|l| l.as_str())
        .map(|l| l == "H")
        .or(row_is_home);

    // Resolve opponent: the payload's code when it sends one, else the game row.
    let opponent_code = perf
        .get("opponent")
        .and_then(|o| o.get("code"))
        .or_else(|| perf.get("opponent-code"))
        .and_then(|c| c.as_str());
    let opponent_id = match opponent_code {
        Some(_) => team_id_by_code_and_season(pool, opponent_code, season).await?,
        None => row_opponent,
    };

    sqlx::query(
        "INSERT INTO player_game_stats (
            id, player_id, game_id, team_id, season, game_date, opponent_id, is_home,
            minutes, points, fgm, fga, fg_pct, tpm, tpa, tp_pct,
            ftm, fta, ft_pct, off_rebounds, def_rebounds, total_rebounds,
            assists, turnovers, steals, blocks, fouls, plus_minus,
            starter, efficiency, usage_rate, two_fg_pct,
            presence_rate, adj_presence_rate, perf_score, perf_score_season_avg,
            team_possessions, team_fga, team_fta, team_turnovers, team_fgm
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28,
                 $29, $30, $31, $32, $33, $34, $35, $36, $37, $38, $39, $40, $41)
         ON CONFLICT (player_id, game_id) DO UPDATE
         SET minutes = COALESCE(EXCLUDED.minutes, player_game_stats.minutes),
             points = COALESCE(EXCLUDED.points, player_game_stats.points),
             fgm = COALESCE(EXCLUDED.fgm, player_game_stats.fgm),
             fga = COALESCE(EXCLUDED.fga, player_game_stats.fga),
             fg_pct = COALESCE(EXCLUDED.fg_pct, player_game_stats.fg_pct),
             tpm = COALESCE(EXCLUDED.tpm, player_game_stats.tpm),
             tpa = COALESCE(EXCLUDED.tpa, player_game_stats.tpa),
             tp_pct = COALESCE(EXCLUDED.tp_pct, player_game_stats.tp_pct),
             ftm = COALESCE(EXCLUDED.ftm, player_game_stats.ftm),
             fta = COALESCE(EXCLUDED.fta, player_game_stats.fta),
             ft_pct = COALESCE(EXCLUDED.ft_pct, player_game_stats.ft_pct),
             off_rebounds = EXCLUDED.off_rebounds,
             def_rebounds = EXCLUDED.def_rebounds,
             total_rebounds = EXCLUDED.total_rebounds,
             assists = COALESCE(EXCLUDED.assists, player_game_stats.assists),
             turnovers = COALESCE(EXCLUDED.turnovers, player_game_stats.turnovers),
             steals = COALESCE(EXCLUDED.steals, player_game_stats.steals),
             blocks = COALESCE(EXCLUDED.blocks, player_game_stats.blocks),
             fouls = COALESCE(EXCLUDED.fouls, player_game_stats.fouls),
             plus_minus = COALESCE(EXCLUDED.plus_minus, player_game_stats.plus_minus),
             starter = COALESCE(EXCLUDED.starter, player_game_stats.starter),
             efficiency = COALESCE(EXCLUDED.efficiency, player_game_stats.efficiency),
             usage_rate = EXCLUDED.usage_rate,
             two_fg_pct = COALESCE(EXCLUDED.two_fg_pct, player_game_stats.two_fg_pct),
             presence_rate = COALESCE(EXCLUDED.presence_rate, player_game_stats.presence_rate),
             adj_presence_rate = COALESCE(EXCLUDED.adj_presence_rate, player_game_stats.adj_presence_rate),
             perf_score = COALESCE(EXCLUDED.perf_score, player_game_stats.perf_score),
             perf_score_season_avg = COALESCE(EXCLUDED.perf_score_season_avg, player_game_stats.perf_score_season_avg),
             team_possessions = COALESCE(EXCLUDED.team_possessions, player_game_stats.team_possessions),
             team_fga = COALESCE(EXCLUDED.team_fga, player_game_stats.team_fga),
             team_fta = COALESCE(EXCLUDED.team_fta, player_game_stats.team_fta),
             team_turnovers = COALESCE(EXCLUDED.team_turnovers, player_game_stats.team_turnovers),
             team_fgm = COALESCE(EXCLUDED.team_fgm, player_game_stats.team_fgm),
             is_home = COALESCE(EXCLUDED.is_home, player_game_stats.is_home),
             -- Repairs a line first written without one: before the live
             -- shape was read, every player line landed with no opponent,
             -- and player SOS joins on it.
             opponent_id = COALESCE(EXCLUDED.opponent_id, player_game_stats.opponent_id),
             -- NatStat's line replaces a gap-filled one (see `clear_cbbd_rows`).
             source = 'natstat'",
    )
    .bind(Uuid::new_v4())
    .bind(player_id)
    .bind(game_id)
    .bind(team_id)
    .bind(season)
    .bind(game_date)
    .bind(opponent_id)
    .bind(is_home)
    .bind(minutes)
    .bind(points)
    .bind(fgm)
    .bind(fga)
    .bind(fg_pct)
    .bind(tpm)
    .bind(tpa)
    .bind(tp_pct)
    .bind(ftm)
    .bind(fta)
    .bind(ft_pct)
    .bind(off_rebounds)
    .bind(def_rebounds)
    .bind(total_rebounds)
    .bind(assists)
    .bind(turnovers)
    .bind(steals)
    .bind(blocks)
    .bind(fouls)
    .bind(plus_minus)
    .bind(starter)
    .bind(efficiency)
    .bind(usage_rate)
    .bind(two_fg_pct)
    .bind(presence_rate)
    .bind(adj_presence_rate)
    .bind(perf_score)
    .bind(perf_score_season_avg)
    .bind(team_possessions)
    .bind(team_fga)
    .bind(team_fta)
    .bind(team_turnovers)
    .bind(team_fgm)
    .execute(pool)
    .await?;

    Ok(Upsert::Wrote)
}

/// Ingest team performances for every team in a season.
///
/// Uses the season-wide `teamperfs/mbb/{season}` endpoint (paginated) — one
/// loop instead of 367 per-team calls. Cuts ingest time and rate-limit
/// pressure dramatically; matches the shape of `ingest_player_performances`.
pub async fn ingest_all_team_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<IngestOutcome, NatStatError> {
    let pages = client
        .get_all_pages("teamperfs", Some(&season.to_string()), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Teams, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_team_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log(
        "team performances (season-wide)",
        &format!("season={season}"),
    );
    Ok(outcome)
}

/// Ingest team performances (team-level box scores) for a specific date range.
///
/// Mirrors [`ingest_player_performances_by_date_range`] so the incremental
/// `update` path can populate `team_game_stats` (four-factors / AdjEM / W-L
/// inputs) for newly-ingested games — not just `player_game_stats`.
pub async fn ingest_team_performances_by_date_range(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    start: &str,
    end: &str,
) -> Result<IngestOutcome, NatStatError> {
    let range = format!("{},{}", start, end);
    let pages = client
        .get_all_pages("teamperfs", Some(&range), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Teams, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_team_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log(
        "team performances",
        &format!("season={season} {start}..{end}"),
    );
    Ok(outcome)
}

/// Ingest team performances (team-level box scores) for a specific team and season.
pub async fn ingest_team_performances(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_code: &str,
) -> Result<IngestOutcome, NatStatError> {
    let range = format!("{},{}", season, team_code);
    let pages = client
        .get_all_pages("teamperfs", Some(&range), None)
        .await?;

    clear_cbbd_rows(pool, BoxTable::Teams, season, &pages).await?;

    let mut outcome = IngestOutcome::default();
    for page in &pages {
        let perfs = extract_results(page);
        for perf in perfs {
            outcome.record(upsert_team_game_stats(perf, pool, season).await?);
        }
    }

    outcome.log(
        "team performances",
        &format!("season={season} team={team_code}"),
    );
    Ok(outcome)
}

async fn upsert_team_game_stats(perf: &Value, pool: &PgPool, season: i32) -> UpsertResult {
    let Some(game_id_str) = perf_game_code(perf) else {
        return Ok(Upsert::Skipped(SkipReason::MissingIdentifier));
    };

    if is_excluded(&game_id_str) {
        return Ok(Upsert::Skipped(SkipReason::Excluded));
    }

    // Game before team, matching `upsert_player_game_stats`. The order decides
    // which bucket a row lands in when BOTH are true — an orphaned game whose
    // side is also non-D1 — and the two steps have to agree, because the whole
    // use of these counters is reading `team_perfs` against `player_perfs` for
    // the same window. Checking the team first here meant a `games` under-fetch
    // showed up as `unknown_game` on one step and `unresolved_team` on the
    // other: the same event, one of the two spellings silent.
    let Some(game) = game_sides(pool, &game_id_str).await? else {
        return Ok(Upsert::Skipped(SkipReason::UnknownGame));
    };
    if !is_settled_status(game.status.as_deref()) {
        return Ok(Upsert::Skipped(SkipReason::GameInProgress));
    }
    let (game_id, game_date) = (game.id, game.game_date);

    // Absent code vs unresolvable code — see the note in
    // `upsert_player_game_stats`; the same collapse would hide the same outage.
    let team_code = perf
        .get("team-code")
        .or_else(|| perf.get("team").and_then(|t| t.get("code")))
        .and_then(|c| c.as_str());
    let Some(team_code) = team_code else {
        return Ok(Upsert::Skipped(SkipReason::MissingIdentifier));
    };
    let Some(team_id) = team_id_by_code_and_season(pool, Some(team_code), season).await? else {
        return Ok(Upsert::Skipped(SkipReason::UnresolvedTeam));
    };

    // Opponent, venue and result: from the payload when it sends them (the
    // older shapes), else from the game row (the live feed sends none).
    let (row_opponent, row_is_home, row_win) = side_context(&game, team_id);
    let opponent_code = perf
        .get("opponent")
        .and_then(|o| o.get("code"))
        .and_then(|c| c.as_str());
    let opponent_id = match opponent_code {
        Some(_) => team_id_by_code_and_season(pool, opponent_code, season).await?,
        None => row_opponent,
    };

    let is_home = perf
        .get("game")
        .and_then(|g| g.get("location"))
        .and_then(|l| l.as_str())
        .map(|l| l == "H")
        .or(row_is_home);

    let win = perf
        .get("game")
        .and_then(|g| g.get("winorloss"))
        .and_then(|w| w.as_str())
        .map(|w| w == "W")
        .or(row_win);

    let league = perf.get("league").and_then(|l| l.as_str());

    let stats = team_perf_stats(perf);

    let minutes = get_i32(stats, K_MIN);
    let points = get_i32(stats, K_PTS);
    let fgm = get_i32(stats, K_FGM);
    let fga = get_i32(stats, K_FGA);
    let tpm = get_i32(stats, K_3FM);
    let tpa = get_i32(stats, K_3FA);
    let ftm = get_i32(stats, K_FTM);
    let fta = get_i32(stats, K_FTA);
    let off_rebounds = get_i32(stats, K_OREB);
    // NatStat teamperfs: "reb" = total rebounds. No "dreb" field at team level (unlike playerperfs).
    // ~68% of games have reb=0 (missing at source). When reb=0 but oreb>0, treat as NULL.
    let reb_raw = get_i32(stats, K_REB);
    let total_rebounds = match (reb_raw, off_rebounds) {
        (Some(0), Some(o)) if o > 0 => None, // missing data
        (r, _) => r,
    };
    let def_rebounds = total_rebounds
        .zip(off_rebounds)
        .map(|(t, o)| t - o)
        .filter(|&d| d >= 0);
    let assists = get_i32(stats, K_AST);
    let steals = get_i32(stats, K_STL);
    let blocks = get_i32(stats, K_BLK);
    let turnovers = get_i32(stats, K_TO);
    let fouls = get_i32(stats, K_PF);

    sqlx::query(
        "INSERT INTO team_game_stats (
            id, team_id, game_id, season, game_date, opponent_id, is_home, win, league,
            minutes, points, fgm, fga, tpm, tpa, ftm, fta,
            off_rebounds, def_rebounds, total_rebounds, assists, steals, blocks, turnovers, fouls
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
        ON CONFLICT (team_id, game_id) DO UPDATE
        SET points = COALESCE(EXCLUDED.points, team_game_stats.points),
            fgm = COALESCE(EXCLUDED.fgm, team_game_stats.fgm),
            fga = COALESCE(EXCLUDED.fga, team_game_stats.fga),
            tpm = COALESCE(EXCLUDED.tpm, team_game_stats.tpm),
            tpa = COALESCE(EXCLUDED.tpa, team_game_stats.tpa),
            ftm = COALESCE(EXCLUDED.ftm, team_game_stats.ftm),
            fta = COALESCE(EXCLUDED.fta, team_game_stats.fta),
            off_rebounds = EXCLUDED.off_rebounds,
            def_rebounds = EXCLUDED.def_rebounds,
            total_rebounds = EXCLUDED.total_rebounds,
            assists = COALESCE(EXCLUDED.assists, team_game_stats.assists),
            steals = COALESCE(EXCLUDED.steals, team_game_stats.steals),
            blocks = COALESCE(EXCLUDED.blocks, team_game_stats.blocks),
            turnovers = COALESCE(EXCLUDED.turnovers, team_game_stats.turnovers),
            fouls = COALESCE(EXCLUDED.fouls, team_game_stats.fouls),
            is_home = COALESCE(EXCLUDED.is_home, team_game_stats.is_home),
            win = COALESCE(EXCLUDED.win, team_game_stats.win),
            opponent_id = COALESCE(EXCLUDED.opponent_id, team_game_stats.opponent_id),
            source = 'natstat'",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(game_id)
    .bind(season)
    .bind(game_date)
    .bind(opponent_id)
    .bind(is_home)
    .bind(win)
    .bind(league)
    .bind(minutes)
    .bind(points)
    .bind(fgm)
    .bind(fga)
    .bind(tpm)
    .bind(tpa)
    .bind(ftm)
    .bind(fta)
    .bind(off_rebounds)
    .bind(def_rebounds)
    .bind(total_rebounds)
    .bind(assists)
    .bind(steals)
    .bind(blocks)
    .bind(turnovers)
    .bind(fouls)
    .execute(pool)
    .await?;

    Ok(Upsert::Wrote)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `teamperfs` row exactly as the live feed returned it for
    /// Villanova-Notre Dame on 2026-11-01 (the first game of 2027).
    fn live_teamperf() -> Value {
        serde_json::json!({
            "game-code": "1512563", "team": "Villanova", "team-code": "NOVA",
            "groups": {"ALL": {"player-count": "8", "stats": {
                "MIN": "197", "PTS": "72", "FGM": "24", "FGA": "52", "3FM": "11",
                "3FA": "31", "FTM": "13", "FTA": "18", "REB": "33", "AST": "12",
                "STL": "6", "BLK": "4", "OREB": "7", "TO": "20", "PF": "17"
            }}}
        })
    }

    /// A `playerperfs` row exactly as the live feed returned it for the same game.
    fn live_playerperf() -> Value {
        serde_json::json!({
            "game-code": "1512563", "player": "Tommy Ahneman", "player-code": "104985517",
            "player-number": "15", "position": "C", "playertype": "", "starter": "Y",
            "team": "Notre Dame", "team-code": "ND", "statline": "13m 4p 6r",
            "MIN": "13", "PTS": "4", "FGM": "1", "FGA": "6", "3FM": "0", "3FA": "0",
            "FTM": "2", "FTA": "2", "REB": "6", "AST": "0", "STL": "1", "BLK": "0",
            "OREB": "3", "TO": "1", "PF": "0"
        })
    }

    #[test]
    fn perf_sides_names_each_game_side_once() {
        // Two lines for one side, one for another game, and a line with no
        // team code (skipped: it cannot be written, so it clears nothing).
        let mut other = live_playerperf();
        other["game-code"] = "1512999".into();
        other["team-code"] = "UK".into();
        let mut no_team = live_playerperf();
        no_team.as_object_mut().unwrap().remove("team-code");
        let page = serde_json::json!({
            "meta": {},
            "performances": {
                "a": live_playerperf(), "b": live_playerperf(), "c": other, "d": no_team
            }
        });
        assert_eq!(
            perf_sides(&[page]),
            vec![
                ("1512563".to_string(), "ND".to_string()),
                ("1512999".to_string(), "UK".to_string())
            ]
        );
    }

    #[test]
    fn the_live_team_line_is_read_not_dropped() {
        // The regression: this row was skipped as `missing_identifier` (no
        // `game.id`) and, had it got past that, would have stored every stat
        // NULL (lowercase keys, read from the row rather than `groups.ALL`).
        let perf = live_teamperf();
        assert_eq!(perf_game_code(&perf).as_deref(), Some("1512563"));
        let stats = team_perf_stats(&perf);
        assert_eq!(get_i32(stats, K_PTS), Some(72));
        assert_eq!(get_i32(stats, K_FGA), Some(52));
        assert_eq!(get_i32(stats, K_3FM), Some(11));
        assert_eq!(get_i32(stats, K_OREB), Some(7));
        assert_eq!(get_i32(stats, K_TO), Some(20));
        assert_eq!(get_i32(stats, K_FTA), Some(18));
        assert_eq!(get_i32(stats, K_PF), Some(17));
    }

    #[test]
    fn the_live_player_line_carries_its_stats() {
        // The regression: this row was written with every stat NULL, so the
        // player had a game but no minutes, and no roster showed him.
        let perf = live_playerperf();
        assert_eq!(perf_game_code(&perf).as_deref(), Some("1512563"));
        assert_eq!(get_f64(&perf, K_MIN), Some(13.0));
        assert_eq!(get_i32(&perf, K_PTS), Some(4));
        assert_eq!(get_i32(&perf, K_REB), Some(6));
        assert_eq!(get_i32(&perf, K_OREB), Some(3));
        assert_eq!(get_i32(&perf, K_FTM), Some(2));
        assert_eq!(get_i32(&perf, K_TO), Some(1));
    }

    #[test]
    fn the_older_shapes_still_parse() {
        // What `simulate` synthesizes and the v4 beta returned in April: the
        // replay harness depends on it.
        let team = serde_json::json!({
            "game": {"id": "CFG1", "location": "H"}, "team": {"code": "TA"},
            "stats": {"pts": 80, "fga": 60, "threefm": 9, "oreb": 11, "to": 12, "f": 15}
        });
        assert_eq!(perf_game_code(&team).as_deref(), Some("CFG1"));
        let stats = team_perf_stats(&team);
        assert_eq!(get_i32(stats, K_PTS), Some(80));
        assert_eq!(get_i32(stats, K_3FM), Some(9));
        assert_eq!(get_i32(stats, K_PF), Some(15));
        let player = serde_json::json!({"game-code": "CFG1", "min": "31", "pts": 12, "threefm": 2});
        assert_eq!(get_f64(&player, K_MIN), Some(31.0));
        assert_eq!(get_i32(&player, K_3FM), Some(2));
    }

    fn sides(
        neutral: Option<bool>,
        home_score: Option<i32>,
        away_score: Option<i32>,
    ) -> (GameSides, Uuid, Uuid) {
        let (h, a) = (Uuid::new_v4(), Uuid::new_v4());
        let g = GameSides {
            id: Uuid::new_v4(),
            game_date: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            home_team_id: Some(h),
            away_team_id: Some(a),
            home_score,
            away_score,
            is_neutral_site: neutral,
            status: None,
        };
        (g, h, a)
    }

    #[test]
    fn opponent_venue_and_result_come_from_the_game_row() {
        // Villanova (home slot) 72, Notre Dame 74, at a neutral site.
        let (g, nova, nd) = sides(Some(true), Some(72), Some(74));
        assert_eq!(side_context(&g, nova), (Some(nd), Some(false), Some(false)));
        assert_eq!(side_context(&g, nd), (Some(nova), Some(false), Some(true)));

        let (g, h, a) = sides(Some(false), Some(80), Some(70));
        assert_eq!(side_context(&g, h), (Some(a), Some(true), Some(true)));
        assert_eq!(side_context(&g, a), (Some(h), Some(false), Some(false)));

        // Not final yet: opponent and venue, no result.
        let (g, h, _) = sides(None, None, None);
        assert_eq!(side_context(&g, h).2, None);
        // A team that is not in the game gets nothing rather than a guess.
        assert_eq!(side_context(&g, Uuid::new_v4()), (None, None, None));
    }

    #[test]
    fn an_excluded_game_is_refused_and_not_alarmed_on() {
        let game = |id: &str| serde_json::json!({ "id": id, "gameday": "2026-11-05" });
        assert_eq!(parse_game(&game("1517094")), Err(SkipReason::Excluded));
        assert!(parse_game(&game("1517095")).is_ok());

        let mut counts = SkipCounts::default();
        counts.record(SkipReason::Excluded);
        assert_eq!((counts.total(), counts.anomalous()), (1, 0));
        assert_eq!(counts.summary().unwrap(), "skipped 1 (excluded=1)");
    }

    #[test]
    fn tip_time_reads_the_clock_and_treats_midnight_as_unset() {
        assert_eq!(
            parse_tip_time("2026-11-02 19:30:00"),
            NaiveTime::from_hms_opt(19, 30, 0)
        );
        // NatStat's "time not set" sentinel, not a midnight tip.
        assert_eq!(parse_tip_time("2026-11-02 00:00:00"), None);
        assert_eq!(parse_tip_time("2026-11-02"), None);
        assert_eq!(parse_tip_time(""), None);
    }

    #[test]
    fn only_a_final_status_is_settled() {
        assert!(is_settled_status(Some("Final")));
        assert!(is_settled_status(Some("Final/OT")));
        assert!(is_settled_status(Some(" final ")));
        assert!(is_settled_status(None));
        assert!(is_settled_status(Some("")));
        assert!(!is_settled_status(Some("H2 12:28")));
        assert!(!is_settled_status(Some("Half")));
        assert!(!is_settled_status(Some("Postponed")));
    }

    #[test]
    fn a_game_in_progress_stores_no_score() {
        let game = serde_json::json!({
            "id": "1516751",
            "gameday": "2026-11-02",
            "home-code": "SOMS",
            "visitor-code": "TOUG",
            "gamestatus": "H2 12:28",
            "score-home": "61",
            "score-vis": "30",
            "line-home": { "p1": "45" },
            "line-vis": { "p1": "12" },
        });
        let row = parse_game(&game).unwrap();
        assert_eq!((row.home_score, row.away_score), (None, None));
        assert_eq!(row.home_half1, None);
        assert_eq!(row.status.as_deref(), Some("H2 12:28"));

        let mut finished = game.clone();
        finished["gamestatus"] = "Final".into();
        let row = parse_game(&finished).unwrap();
        assert_eq!((row.home_score, row.away_score), (Some(61), Some(30)));
        assert_eq!(row.home_half1, Some(45));
    }

    #[test]
    fn parse_game_carries_tip_time_and_venue_location() {
        let game = serde_json::json!({
            "id": "1511105",
            "gameday": "2026-11-02",
            "gamedate": "2026-11-02 18:00:00",
            "home-code": "MICH",
            "visitor-code": "OAK",
            "venue": { "code": "789", "name": "Crisler Center", "location": "Ann Arbor, MI" },
        });
        let row = parse_game(&game).unwrap();
        assert_eq!(row.tip_time, NaiveTime::from_hms_opt(18, 0, 0));
        assert_eq!(row.venue_location.as_deref(), Some("Ann Arbor, MI"));
        assert_eq!(row.venue.as_deref(), Some("Crisler Center"));
    }

    #[test]
    fn silent_loop_reports_nothing() {
        let o = IngestOutcome::default();
        assert_eq!(o.skipped.summary(), None);
        assert_eq!(o.skipped.total(), 0);
        assert_eq!(o.skipped.anomalous(), 0);
    }

    #[test]
    fn summary_names_only_the_reasons_that_fired() {
        let mut o = IngestOutcome::default();
        o.record(Upsert::Wrote);
        o.record(Upsert::Skipped(SkipReason::UnknownGame));
        o.record(Upsert::Skipped(SkipReason::UnknownGame));
        o.record(Upsert::Skipped(SkipReason::UnresolvedTeam));

        assert_eq!(o.written, 1);
        assert_eq!(
            o.skipped.summary().unwrap(),
            "skipped 3 (unknown_game=2, unresolved_team=1)"
        );
    }

    /// The distinction the warn/degrade decision hangs on: a non-D1 opponent is
    /// routine and must not raise an alarm, while an orphaned perf is #202's
    /// whole point. Getting this backwards recreates #232 in a new channel.
    #[test]
    fn non_d1_skips_are_counted_but_not_anomalous() {
        let mut o = IngestOutcome::default();
        for _ in 0..34 {
            o.record(Upsert::Skipped(SkipReason::UnresolvedTeam));
        }
        assert_eq!(o.skipped.total(), 34);
        assert_eq!(o.skipped.anomalous(), 0);
        assert_eq!(
            o.skipped.summary().unwrap(),
            "skipped 34 (unresolved_team=34)"
        );

        o.record(Upsert::Skipped(SkipReason::UnknownGame));
        assert_eq!(o.skipped.anomalous(), 1);
    }

    #[test]
    fn every_reason_lands_in_its_own_bucket() {
        let mut o = IngestOutcome::default();
        for r in [
            SkipReason::MissingIdentifier,
            SkipReason::MissingDate,
            SkipReason::UnknownGame,
            SkipReason::UnresolvedTeam,
            SkipReason::CbbdTwin,
        ] {
            o.record(Upsert::Skipped(r));
        }
        let s = o.skipped;
        assert_eq!(
            (
                s.missing_identifier,
                s.missing_date,
                s.unknown_game,
                s.unresolved_team,
                s.cbbd_twin
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(s.total(), 5);
        assert_eq!(s.anomalous(), 4);
    }
}
