//! Bulk-load a season from NatStat dashboard CSV exports.
//!
//! Use case: backfill historical seasons in seconds instead of hours. The
//! `season --year YYYY` API path is rate-limited to ~2 hrs/season; this
//! loader runs `COPY`-equivalent batch inserts and finishes a season in
//! under a minute (excluding `compute_all`, which still takes ~5 min).
//!
//! Expected layout: `data/natstat_csv/YYYY/NatStat-MBB{YYYY}-{Kind}-*.csv`
//! for kinds `Teams`, `Games`, `Team_Statlines`, `Player_Statlines`.
//! `Players.csv` and `Play-by-Play.csv` are intentionally skipped —
//! Players uses a different ID space (registration IDs ≠ player ids in
//! box scores) so the box-score path is authoritative as in the API
//! ingest. PBP lives in its own future loader.
//!
//! ID mapping caveat: most CSVs use NatStat's numeric `TeamID`
//! (e.g. `2031759`) but our DB stores `teams.natstat_id` as the short
//! code (`WGEO`). The Teams.csv `Abbrev` column is the bridge — we build
//! a `numeric_team_id -> abbrev` map up front and translate every
//! downstream CSV through it. PlayerIDs match between CSV and DB
//! (verified against 2026 sample), so no translation is needed there.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;
use csv::ReaderBuilder;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::{info, warn};
use uuid::Uuid;

use super::team_aliases;

/// Per-table row counts from a bootstrap run. Surfaced to the CLI so the
/// user can sanity-check against expected season volume.
#[derive(Debug, Default)]
pub struct BootstrapCsvReport {
    pub teams: u64,
    pub games: u64,
    pub players: u64,
    pub team_game_stats: u64,
    pub player_game_stats: u64,
}

impl std::fmt::Display for BootstrapCsvReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bootstrap_csv: teams={} games={} players={} team_game_stats={} player_game_stats={}",
            self.teams, self.games, self.players, self.team_game_stats, self.player_game_stats
        )
    }
}

/// Top-level orchestrator. Loads the four required CSVs in dependency
/// order: Teams (also builds the numeric→abbrev map), Games,
/// Team_Statlines, Player_Statlines (also auto-creates player rows).
pub async fn bootstrap_from_csv_dir(
    pool: &PgPool,
    year: i32,
    dir: &Path,
) -> Result<BootstrapCsvReport> {
    info!(year, dir = %dir.display(), "starting CSV bootstrap");

    let teams_csv = find_csv(dir, year, "Teams")?;
    let games_csv = find_csv(dir, year, "Games")?;
    let team_statlines_csv = find_csv(dir, year, "Team_Statlines")?;
    let player_statlines_csv = find_csv(dir, year, "Player_Statlines")?;

    let mut report = BootstrapCsvReport::default();

    let (teams_upserted, team_id_map) = load_teams(pool, year, &teams_csv).await?;
    report.teams = teams_upserted;
    info!(count = teams_upserted, "loaded teams");

    report.games = load_games(pool, year, &games_csv, &team_id_map).await?;
    info!(count = report.games, "loaded games");

    report.team_game_stats =
        load_team_statlines(pool, year, &team_statlines_csv, &team_id_map).await?;
    info!(count = report.team_game_stats, "loaded team_game_stats");

    let (players_upserted, pgs_upserted) =
        load_player_statlines(pool, year, &player_statlines_csv, &team_id_map).await?;
    report.players = players_upserted;
    report.player_game_stats = pgs_upserted;
    info!(
        players = report.players,
        player_game_stats = report.player_game_stats,
        "loaded player_game_stats"
    );

    info!(?report, "CSV bootstrap complete");
    Ok(report)
}

// ---------------------------------------------------------------------------
// File discovery
// ---------------------------------------------------------------------------

/// Find a CSV by kind in `dir`, expecting filename pattern
/// `NatStat-MBB{year}-{kind}-*.csv` (NatStat dashboard's export naming).
/// Tries both `dir/` and `dir/{year}/` so the caller can pass either the
/// per-season subdir or the parent collection root. Errors when zero or
/// >1 file matches.
fn find_csv(dir: &Path, year: i32, kind: &str) -> Result<PathBuf> {
    let prefix = format!("NatStat-MBB{year}-{kind}-");
    let search_dirs = [dir.to_path_buf(), dir.join(year.to_string())];
    for d in &search_dirs {
        if !d.is_dir() {
            continue;
        }
        let mut matches: Vec<PathBuf> = std::fs::read_dir(d)
            .with_context(|| format!("reading dir {}", d.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix) && n.ends_with(".csv"))
                    .unwrap_or(false)
            })
            .collect();
        matches.sort();
        match matches.len() {
            0 => continue,
            1 => return Ok(matches.pop().unwrap()),
            n => {
                return Err(anyhow!(
                    "{n} files matched {prefix}*.csv in {} — keep only one",
                    d.display()
                ));
            }
        }
    }
    Err(anyhow!(
        "no CSV found for {kind}: searched {} and {} for {prefix}*.csv",
        search_dirs[0].display(),
        search_dirs[1].display()
    ))
}

// ---------------------------------------------------------------------------
// Cell parsers — CSV strings are all quoted; missing values are empty strings.
// ---------------------------------------------------------------------------

fn cell(row: &csv::StringRecord, idx: usize) -> &str {
    row.get(idx).unwrap_or("").trim()
}

fn cell_owned(row: &csv::StringRecord, idx: usize) -> String {
    cell(row, idx).to_string()
}

fn parse_i32(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() { None } else { s.parse().ok() }
}

fn parse_f64(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() { None } else { s.parse().ok() }
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

fn maybe(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.is_empty() { None } else { Some(s) }
}

/// (made, attempts) → percentage on the **0–100 scale** to match the API
/// path's convention (NatStat's `fgpct`/`ftpct`/etc. are percents, not
/// fractions, and the API ingester stores them as-is). Downstream compute
/// pipeline and frontends key off this scale — using fractions here would
/// shift CSV-loaded seasons into a different range and break percentiles
/// + rankings across the cohort.
fn pct(made: Option<i32>, attempts: Option<i32>) -> Option<f64> {
    match (made, attempts) {
        (Some(m), Some(a)) if a > 0 => Some(100.0 * m as f64 / a as f64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Step 1: Teams.csv → teams + numeric_id→abbrev map
// ---------------------------------------------------------------------------

/// Columns: TeamID, Name, Nickname, FullName, Abbrev (5 cols).
async fn load_teams(
    pool: &PgPool,
    year: i32,
    path: &Path,
) -> Result<(u64, HashMap<String, String>)> {
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?);

    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut tx = pool.begin().await?;
    let mut count = 0u64;

    for result in rdr.records() {
        let row = result?;
        let team_id_csv = cell_owned(&row, 0);
        let full_name = cell(&row, 3);
        let abbrev = cell(&row, 4);
        if team_id_csv.is_empty() || abbrev.is_empty() {
            continue;
        }
        // Map for downstream translation. Insert even if upsert fails.
        id_map.insert(team_id_csv.clone(), abbrev.to_string());

        // Populate `short_name` from the bundled Torvik-style alias map,
        // matching `teams.rs::upsert_team`. Without this, `bootstrap-csv`
        // rows arrived with NULL short_name and the frontend fell back to
        // `name` ("Hartford Hawks") for older seasons while 2021+ rows
        // rendered as "Hartford" — the inconsistency flagged on the
        // team-detail page.
        //
        // Conflict behavior intentionally diverges from the live-API
        // path: API uses `EXCLUDED.short_name` (always overwrite), this
        // path uses `COALESCE(teams.short_name, EXCLUDED.short_name)`
        // (preserve existing). Reason: bootstrap-csv is a backfill loop
        // and any non-null existing value was set by the API or by
        // migration 017, both of which are higher-trust sources. Trade-
        // off: a typo fix in `data/team_short_names.json` won't
        // propagate to already-stamped rows via bootstrap alone — for
        // that case, re-execute migration 017's UPDATE block via psql
        // (`psql ... -f migrations/017_team_short_names.sql`); SQLx's
        // checksum gate prevents the migration framework from re-running
        // it automatically, but the SQL itself is an idempotent UPDATE.
        let short_name = team_aliases::short_name(abbrev);
        sqlx::query(
            "INSERT INTO teams (id, natstat_id, name, short_name, season)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (natstat_id, season) DO UPDATE
             SET name = EXCLUDED.name,
                 short_name = COALESCE(teams.short_name, EXCLUDED.short_name),
                 updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(abbrev)
        .bind(full_name)
        .bind(short_name)
        .bind(year)
        .execute(&mut *tx)
        .await?;
        count += 1;
    }
    tx.commit().await?;

    // Seed empty team_season_stats rows. The compute pipeline's
    // four-factors / adj-efficiency / wins-losses steps are all UPDATEs
    // and silently no-op when the row is missing. The API path's TCR
    // enrichment normally creates these rows — without that step we have
    // to seed them ourselves or compute returns zero everywhere.
    sqlx::query(
        "INSERT INTO team_season_stats (id, team_id, season)
         SELECT gen_random_uuid(), t.id, t.season
         FROM teams t
         WHERE t.season = $1
         ON CONFLICT (team_id, season) DO NOTHING",
    )
    .bind(year)
    .execute(pool)
    .await?;

    Ok((count, id_map))
}

// ---------------------------------------------------------------------------
// Step 2: Games.csv → games
// ---------------------------------------------------------------------------

/// Columns: ID, GameDay, GameTime, Home, HomeID, Visitor, VisitorID,
/// ScoreVis, ScoreHome, GameStatus, Venue, CityState, Neutral, Division,
/// Conference, Playoffs (16 cols).
async fn load_games(
    pool: &PgPool,
    year: i32,
    path: &Path,
    team_id_map: &HashMap<String, String>,
) -> Result<u64> {
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?);

    // Pre-resolve all team UUIDs we'll need (the season's full roster).
    let team_uuids = team_uuid_map(pool, year, team_id_map).await?;

    let mut tx = pool.begin().await?;
    let mut count = 0u64;
    let mut unresolved = 0u64;
    let mut bad_dates = 0u64;

    for result in rdr.records() {
        let row = result?;
        let natstat_id = cell_owned(&row, 0);
        if natstat_id.is_empty() {
            continue;
        }
        let game_date = match parse_date(cell(&row, 1)) {
            Some(d) => d,
            None => {
                bad_dates += 1;
                continue;
            }
        };
        let home_team_csv_id = cell(&row, 4);
        let away_team_csv_id = cell(&row, 6);
        let away_score = parse_i32(cell(&row, 7));
        let home_score = parse_i32(cell(&row, 8));
        let status = maybe(cell(&row, 9));
        let venue = maybe(cell(&row, 10));
        let is_neutral = cell(&row, 12) == "Y";
        // Playoffs column uses "Y" for postseason games; tighter than
        // !is_empty() (which would treat any rogue value as postseason).
        let is_postseason = cell(&row, 15) == "Y";

        let home_team_id = team_id_map
            .get(home_team_csv_id)
            .and_then(|abbrev| team_uuids.get(abbrev))
            .copied();
        let away_team_id = team_id_map
            .get(away_team_csv_id)
            .and_then(|abbrev| team_uuids.get(abbrev))
            .copied();

        if home_team_id.is_none() || away_team_id.is_none() {
            // Non-D1 opponent (early-season exhibitions etc.) — skip
            // silently like the API path does; box scores for non-D1
            // opponents are also dropped.
            unresolved += 1;
            continue;
        }

        sqlx::query(
            "INSERT INTO games (id, natstat_id, season, game_date, home_team_id, away_team_id,
             home_score, away_score, is_neutral_site, is_postseason, venue, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (natstat_id) DO UPDATE
             SET home_score = COALESCE(EXCLUDED.home_score, games.home_score),
                 away_score = COALESCE(EXCLUDED.away_score, games.away_score),
                 home_team_id = COALESCE(EXCLUDED.home_team_id, games.home_team_id),
                 away_team_id = COALESCE(EXCLUDED.away_team_id, games.away_team_id),
                 is_neutral_site = EXCLUDED.is_neutral_site,
                 is_postseason = COALESCE(EXCLUDED.is_postseason, games.is_postseason),
                 venue = COALESCE(EXCLUDED.venue, games.venue),
                 status = COALESCE(EXCLUDED.status, games.status),
                 updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(&natstat_id)
        .bind(year)
        .bind(game_date)
        .bind(home_team_id)
        .bind(away_team_id)
        .bind(home_score)
        .bind(away_score)
        .bind(is_neutral)
        .bind(is_postseason)
        .bind(venue)
        .bind(status)
        .execute(&mut *tx)
        .await?;
        count += 1;
    }
    tx.commit().await?;
    if unresolved > 0 {
        warn!(unresolved, "games skipped: non-D1 opponent (no team in DB)");
    }
    if bad_dates > 0 {
        warn!(bad_dates, "games skipped: unparseable GameDay");
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Step 3: Team_Statlines.csv → team_game_stats
// ---------------------------------------------------------------------------

/// Columns: GameDay, GameID, TeamID, Team, OpponentID, Opponent, Location,
/// Division, Conference, Playoffs, WinOrLoss, PlayerType, MIN, PTS, FGM,
/// FGA, 3FM, 3FA, FTM, FTA, REB, AST, STL, BLK, OREB, TO, PF (27 cols).
async fn load_team_statlines(
    pool: &PgPool,
    year: i32,
    path: &Path,
    team_id_map: &HashMap<String, String>,
) -> Result<u64> {
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?);

    let team_uuids = team_uuid_map(pool, year, team_id_map).await?;
    let game_uuids = game_uuid_map(pool, year).await?;

    let mut tx = pool.begin().await?;
    let mut count = 0u64;
    let mut skipped = 0u64;

    for result in rdr.records() {
        let row = result?;
        let game_natstat = cell(&row, 1);
        let team_csv_id = cell(&row, 2);
        let opponent_csv_id = cell(&row, 4);
        let location = cell(&row, 6);
        let win_loss = cell(&row, 10);

        let game_info = game_uuids.get(game_natstat);
        let team_id = team_id_map
            .get(team_csv_id)
            .and_then(|a| team_uuids.get(a))
            .copied();
        let opponent_id = team_id_map
            .get(opponent_csv_id)
            .and_then(|a| team_uuids.get(a))
            .copied();
        let Some((game_id, game_date)) = game_info else {
            skipped += 1;
            continue;
        };
        let Some(team_id) = team_id else {
            skipped += 1;
            continue;
        };

        let minutes = parse_i32(cell(&row, 12));
        let points = parse_i32(cell(&row, 13));
        let fgm = parse_i32(cell(&row, 14));
        let fga = parse_i32(cell(&row, 15));
        let tpm = parse_i32(cell(&row, 16));
        let tpa = parse_i32(cell(&row, 17));
        let ftm = parse_i32(cell(&row, 18));
        let fta = parse_i32(cell(&row, 19));
        let total_rebounds = parse_i32(cell(&row, 20));
        let assists = parse_i32(cell(&row, 21));
        let steals = parse_i32(cell(&row, 22));
        let blocks = parse_i32(cell(&row, 23));
        let off_rebounds = parse_i32(cell(&row, 24));
        let turnovers = parse_i32(cell(&row, 25));
        let fouls = parse_i32(cell(&row, 26));

        // dreb from CSV: derive only when both reb and oreb are present
        // and reb > oreb (mirrors API path's NULL-guard).
        let def_rebounds = total_rebounds
            .zip(off_rebounds)
            .map(|(t, o)| t - o)
            .filter(|&d| d >= 0);

        let is_home = match location {
            "H" => Some(true),
            "V" | "A" => Some(false),
            "N" => None, // neutral — game has is_neutral_site set
            _ => None,
        };
        let win = match win_loss {
            "W" => Some(true),
            "L" => Some(false),
            _ => None,
        };

        sqlx::query(
            "INSERT INTO team_game_stats (
                id, team_id, game_id, season, game_date, opponent_id, is_home, win,
                minutes, points, fgm, fga, tpm, tpa, ftm, fta,
                off_rebounds, def_rebounds, total_rebounds,
                assists, steals, blocks, turnovers, fouls
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                     $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19, $20, $21, $22, $23, $24)
             ON CONFLICT (team_id, game_id) DO UPDATE
             SET minutes = COALESCE(EXCLUDED.minutes, team_game_stats.minutes),
                 points = COALESCE(EXCLUDED.points, team_game_stats.points),
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
                 win = COALESCE(EXCLUDED.win, team_game_stats.win)",
        )
        .bind(Uuid::new_v4())
        .bind(team_id)
        .bind(*game_id)
        .bind(year)
        .bind(*game_date)
        .bind(opponent_id)
        .bind(is_home)
        .bind(win)
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
        .execute(&mut *tx)
        .await?;
        count += 1;

        // Commit every 1000 rows so a partial failure isn't catastrophic.
        if count.is_multiple_of(1000) {
            tx.commit().await?;
            tx = pool.begin().await?;
        }
    }
    tx.commit().await?;
    if skipped > 0 {
        warn!(skipped, "team_statlines skipped: missing team or game");
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Step 4: Player_Statlines.csv → players (auto-create) + player_game_stats
// ---------------------------------------------------------------------------

/// Columns: GameDay, GameID, Player, PlayerID, PlayerCode, TeamID, Team,
/// OpponentID, Opponent, Location, Division, Conference, Playoffs,
/// WinOrLoss, Starter, PlayerType, PerfScore, MIN, PTS, FGM, FGA, 3FM,
/// 3FA, FTM, FTA, REB, AST, STL, BLK, OREB, TO, PF (32 cols).
async fn load_player_statlines(
    pool: &PgPool,
    year: i32,
    path: &Path,
    team_id_map: &HashMap<String, String>,
) -> Result<(u64, u64)> {
    // First pass: collect unique (player_natstat_id, name, team_csv_id) tuples
    // for batch player-row creation. This avoids per-row existence checks
    // during the main insert loop.
    let mut players_seen: HashMap<String, (String, String)> = HashMap::new();
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?);
    for result in rdr.records() {
        let row = result?;
        let player_id = cell_owned(&row, 3);
        if player_id.is_empty() {
            continue;
        }
        // Fall back to player_id when Name is empty so the players row
        // has *something* in name (NOT NULL constraint upstream). The API
        // path similarly stubs to "Unknown" when name is missing.
        let name = {
            let n = cell(&row, 2);
            if n.is_empty() {
                player_id.clone()
            } else {
                n.to_string()
            }
        };
        let team_csv = cell_owned(&row, 5);
        // or_insert (not insert) keeps the FIRST team seen for a player —
        // chronological CSV order means this is the pre-transfer team for
        // mid-season movers. Matches the API path's `upsert_player`,
        // which also never overwrites team_id on conflict. The DB's
        // UNIQUE (natstat_id, season) constraint only allows one team per
        // player-season anyway.
        players_seen.entry(player_id).or_insert((name, team_csv));
    }
    info!(
        unique_players = players_seen.len(),
        "first pass complete; batch-creating player rows"
    );

    let team_uuids = team_uuid_map(pool, year, team_id_map).await?;
    let players_upserted =
        upsert_players_batch(pool, year, &players_seen, team_id_map, &team_uuids).await?;

    // Pre-load player_id lookup for the perf insert loop.
    let player_uuids = player_uuid_map(pool, year).await?;
    let game_uuids = game_uuid_map(pool, year).await?;

    // Second pass: insert player_game_stats.
    let mut rdr = ReaderBuilder::new()
        .has_headers(true)
        .from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?);

    let mut tx = pool.begin().await?;
    let mut count = 0u64;
    let mut skipped = 0u64;

    for result in rdr.records() {
        let row = result?;
        let game_natstat = cell(&row, 1);
        let player_natstat = cell(&row, 3);
        let team_csv = cell(&row, 5);
        let opponent_csv = cell(&row, 7);
        let location = cell(&row, 9);
        let starter_raw = cell(&row, 14);

        let game_info = game_uuids.get(game_natstat);
        let player_id = player_uuids.get(player_natstat).copied();
        let team_id = team_id_map
            .get(team_csv)
            .and_then(|a| team_uuids.get(a))
            .copied();
        let opponent_id = team_id_map
            .get(opponent_csv)
            .and_then(|a| team_uuids.get(a))
            .copied();
        let Some((game_id, game_date)) = game_info else {
            skipped += 1;
            continue;
        };
        let (Some(player_id), Some(team_id)) = (player_id, team_id) else {
            skipped += 1;
            continue;
        };

        let perf_score = parse_f64(cell(&row, 16));
        let minutes = parse_f64(cell(&row, 17));
        let points = parse_i32(cell(&row, 18));
        let fgm = parse_i32(cell(&row, 19));
        let fga = parse_i32(cell(&row, 20));
        let tpm = parse_i32(cell(&row, 21));
        let tpa = parse_i32(cell(&row, 22));
        let ftm = parse_i32(cell(&row, 23));
        let fta = parse_i32(cell(&row, 24));
        let total_rebounds = parse_i32(cell(&row, 25));
        let assists = parse_i32(cell(&row, 26));
        let steals = parse_i32(cell(&row, 27));
        let blocks = parse_i32(cell(&row, 28));
        let off_rebounds = parse_i32(cell(&row, 29));
        let turnovers = parse_i32(cell(&row, 30));
        let fouls = parse_i32(cell(&row, 31));

        let fg_pct = pct(fgm, fga);
        let tp_pct = pct(tpm, tpa);
        let ft_pct = pct(ftm, fta);
        let two_fg_pct = pct(
            fgm.zip(tpm).map(|(m, t)| m - t),
            fga.zip(tpa).map(|(a, t)| a - t),
        );
        let def_rebounds = total_rebounds
            .zip(off_rebounds)
            .map(|(t, o)| t - o)
            .filter(|&d| d >= 0);

        let is_home = match location {
            "H" => Some(true),
            "V" | "A" => Some(false),
            "N" => None, // neutral — games.is_neutral_site carries the flag
            _ => None,
        };
        let starter = match starter_raw {
            "Y" => Some(true),
            "" => None,
            _ => Some(false),
        };

        sqlx::query(
            "INSERT INTO player_game_stats (
                id, player_id, game_id, team_id, season, game_date, opponent_id, is_home,
                minutes, points, fgm, fga, fg_pct, tpm, tpa, tp_pct,
                ftm, fta, ft_pct, off_rebounds, def_rebounds, total_rebounds,
                assists, turnovers, steals, blocks, fouls,
                starter, two_fg_pct, perf_score
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30)
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
                 starter = COALESCE(EXCLUDED.starter, player_game_stats.starter),
                 two_fg_pct = COALESCE(EXCLUDED.two_fg_pct, player_game_stats.two_fg_pct),
                 perf_score = COALESCE(EXCLUDED.perf_score, player_game_stats.perf_score),
                 is_home = COALESCE(EXCLUDED.is_home, player_game_stats.is_home)",
        )
        .bind(Uuid::new_v4())
        .bind(player_id)
        .bind(*game_id)
        .bind(team_id)
        .bind(year)
        .bind(*game_date)
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
        .bind(starter)
        .bind(two_fg_pct)
        .bind(perf_score)
        .execute(&mut *tx)
        .await?;
        count += 1;

        if count.is_multiple_of(2000) {
            tx.commit().await?;
            tx = pool.begin().await?;
            info!(progress = count, "player_game_stats inserted");
        }
    }
    tx.commit().await?;
    if skipped > 0 {
        warn!(
            skipped,
            "player_statlines skipped: missing player/team/game (mostly non-D1 opponents)"
        );
    }
    Ok((players_upserted, count))
}

// ---------------------------------------------------------------------------
// Helpers: in-memory ID maps to avoid per-row lookups during insert loops.
// ---------------------------------------------------------------------------

/// Build `Abbrev -> teams.id` map for the season. Called after load_teams
/// has populated the rows.
async fn team_uuid_map(
    pool: &PgPool,
    year: i32,
    team_id_map: &HashMap<String, String>,
) -> Result<HashMap<String, Uuid>> {
    let abbrevs: Vec<String> = team_id_map.values().cloned().collect();
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, natstat_id FROM teams WHERE season = $1 AND natstat_id = ANY($2)",
    )
    .bind(year)
    .bind(&abbrevs)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id, code)| (code, id)).collect())
}

/// Build `games.natstat_id -> (games.id, game_date)` map for the season.
async fn game_uuid_map(pool: &PgPool, year: i32) -> Result<HashMap<String, (Uuid, NaiveDate)>> {
    let rows: Vec<(Uuid, String, NaiveDate)> =
        sqlx::query_as("SELECT id, natstat_id, game_date FROM games WHERE season = $1")
            .bind(year)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, nat, date)| (nat, (id, date)))
        .collect())
}

/// Build `players.natstat_id -> players.id` map for the season.
async fn player_uuid_map(pool: &PgPool, year: i32) -> Result<HashMap<String, Uuid>> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, natstat_id FROM players WHERE season = $1")
            .bind(year)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, code)| (code, id)).collect())
}

/// Batch-insert player rows discovered in the Player_Statlines first pass.
/// Idempotent: `ON CONFLICT (natstat_id, season) DO UPDATE` only refreshes
/// `name`, never `team_id` (matches the API path's
/// `upsert_player` behavior — box-score path is authoritative).
async fn upsert_players_batch(
    pool: &PgPool,
    year: i32,
    players_seen: &HashMap<String, (String, String)>,
    team_id_map: &HashMap<String, String>,
    team_uuids: &HashMap<String, Uuid>,
) -> Result<u64> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;
    let mut count = 0u64;
    for (player_natstat, (name, team_csv)) in players_seen {
        let team_id = team_id_map
            .get(team_csv)
            .and_then(|a| team_uuids.get(a))
            .copied();
        let Some(team_id) = team_id else {
            // Non-D1 opponent player — skip
            continue;
        };
        sqlx::query(
            "INSERT INTO players (id, natstat_id, name, team_id, season)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (natstat_id, season) DO UPDATE
             SET name = EXCLUDED.name, updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(player_natstat)
        .bind(name)
        .bind(team_id)
        .bind(year)
        .execute(&mut *tx)
        .await?;
        count += 1;
        if count.is_multiple_of(2000) {
            tx.commit().await?;
            tx = pool.begin().await?;
        }
    }
    tx.commit().await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_returns_percent_scale_not_fraction() {
        // Match the API ingester convention so downstream rate-stat
        // aggregations don't end up bimodal across CSV-loaded vs
        // API-loaded seasons. See ROADMAP "CSV bulk-bootstrap" Shipped
        // notes for why this is load-bearing.
        assert_eq!(pct(Some(50), Some(100)), Some(50.0));
        assert_eq!(pct(Some(0), Some(10)), Some(0.0));
        assert_eq!(pct(Some(10), Some(10)), Some(100.0));
    }

    #[test]
    fn pct_returns_none_for_missing_or_zero_attempts() {
        assert_eq!(pct(None, Some(10)), None);
        assert_eq!(pct(Some(5), None), None);
        assert_eq!(pct(Some(5), Some(0)), None);
        assert_eq!(pct(None, None), None);
    }

    #[test]
    fn parse_i32_handles_blanks_and_whitespace() {
        assert_eq!(parse_i32(""), None);
        assert_eq!(parse_i32("   "), None);
        assert_eq!(parse_i32("42"), Some(42));
        assert_eq!(parse_i32(" 42 "), Some(42));
        assert_eq!(parse_i32("not a number"), None);
    }

    #[test]
    fn parse_date_accepts_iso_format_only() {
        assert_eq!(
            parse_date("2026-03-15"),
            Some(NaiveDate::from_ymd_opt(2026, 3, 15).unwrap())
        );
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("3/15/2026"), None);
    }

    #[test]
    fn find_csv_errors_when_no_match() {
        // Empty temp dir — no CSVs of any kind. Both search paths
        // (dir/ and dir/{year}/) miss; expect a single useful error.
        let tmp = std::env::temp_dir().join(format!("cstat-bootstrap-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let err = find_csv(&tmp, 2026, "Teams").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no CSV found"), "got: {msg}");
        assert!(msg.contains("Teams"), "got: {msg}");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
