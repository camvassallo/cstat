//! `source-parity`: game-by-game comparison of the NatStat-fed tables against
//! the CollegeBasketballData shadow tables (`cbbd_*`), over Eastern dates.
//! The acceptance gate for moving any feed (`docs/natstat_migration_plan.md`
//! §5). Read-only.
//!
//! Games join through `game_broadcasts.source_event_id`, the ESPN event id,
//! which is CBBD's `sourceId`. `source_home_is_away` flips CBBD's home side
//! into our `games` row's frame where ESPN lists the game the other way round.
//!
//! Players are paired within each game side by normalized name, then by a
//! unique last name, with [`crate::name_match::pair_box_names`]; an
//! ambiguous name is left unpaired rather than guessed.
//!
//! "Ours" is NatStat's own data only. The gap-fill (migration 077) writes
//! CBBD's box rows (`source = 'cbbd'`), final scores (`games.score_source`) and
//! even whole games into the same tables, and comparing those with the shadow
//! copy they came from would read as perfect agreement. They are left out of
//! every comparison and counted as `filled_by_cbbd` instead; play-by-play is
//! read the same way (`play_by_play.source`, migration 078).
//!
//! The nightly's `source_parity` step ([`nightly_step`]) runs this over the
//! last [`PARITY_DAYS`] days and stores the result per day
//! (`source_parity_days`, `source_parity_games`, migration 079), so the
//! acceptance window (`docs/natstat_migration_plan.md` §5) reads back with
//! `source-parity --summary-days N` ([`trend`]).

use crate::name_match::pair_box_names;
use crate::run_ledger::{RunLedger, StepStatus};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use uuid::Uuid;

/// The team box columns compared, as named in both `team_game_stats` and
/// `cbbd_team_game_stats`.
pub const TEAM_FIELDS: [&str; 15] = [
    "points",
    "fgm",
    "fga",
    "tpm",
    "tpa",
    "ftm",
    "fta",
    "off_rebounds",
    "def_rebounds",
    "total_rebounds",
    "assists",
    "steals",
    "blocks",
    "turnovers",
    "fouls",
];

/// One of our games in the window, with what each source has for it.
#[derive(Debug, Default, Clone)]
pub struct GameParity {
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    pub matchup: String,
    pub espn_event_id: Option<String>,
    pub cbbd_game_id: Option<i32>,
    /// Final in NatStat: a final status with a score NatStat sent, not one
    /// the gap-fill set.
    pub ours_final: bool,
    pub cbbd_final: bool,
    /// The game carries something the gap-fill wrote from CBBD: box rows, its
    /// final score, or the game itself. None of it is compared.
    pub filled_by_cbbd: bool,
    /// Our score is CBBD's, set by the gap-fill, so it is not compared.
    pub score_from_cbbd: bool,
    /// `None` unless both sides have a score, and ours is NatStat's.
    pub score_match: Option<bool>,
    /// Final score as "home-away", ours and CBBD's in our frame.
    pub our_score: Option<String>,
    pub cbbd_score: Option<String>,
    pub natstat_box: bool,
    pub cbbd_box: bool,
    /// Hours CBBD's first box score came after NatStat's; negative when it was
    /// earlier. `None` unless both have one.
    pub cbbd_box_lag_hours: Option<f64>,
    /// CBBD lists the game's home and away the other way round from ours.
    pub sides_swapped: bool,
    /// Team box fields that differ, over both sides, as
    /// `side.field ours/cbbd` (e.g. `home.assists 15/17`).
    pub team_diffs: Vec<String>,
    /// Paired players whose points or rebounds differ, as
    /// `name pts ours/cbbd` / `name reb ours/cbbd`.
    pub player_diffs: Vec<String>,
    /// Team box fields one source leaves empty and the other fills.
    pub team_missing: Vec<String>,
    pub our_players: usize,
    pub cbbd_players: usize,
    pub players_paired: usize,
    pub minutes_abs_diff: f64,
    pub starter_mismatches: usize,
    pub points_mismatches: usize,
    pub rebound_mismatches: usize,
    pub our_plays: i64,
    pub cbbd_plays: Option<i32>,
    pub cbbd_shots: Option<i32>,
    pub cbbd_shots_with_location: Option<i32>,
    pub cbbd_plays_full_on_floor: Option<i32>,
}

#[derive(Debug, Default)]
pub struct ParityReport {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub games: Vec<GameParity>,
    /// CBBD games in the window that no game of ours is matched to, as
    /// "date home vs away".
    pub cbbd_unmatched: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct GameRow {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Option<Uuid>,
    away_team_id: Option<Uuid>,
    home_name: Option<String>,
    away_name: Option<String>,
    home_score: Option<i32>,
    away_score: Option<i32>,
    status: Option<String>,
    score_source: Option<String>,
    cbbd_rows: bool,
    created: bool,
    espn_event_id: Option<String>,
    flip: bool,
    cbbd_game_id: Option<i32>,
    cbbd_status: Option<String>,
    cbbd_home_points: Option<i32>,
    cbbd_away_points: Option<i32>,
    cbbd_first_box_at: Option<DateTime<Utc>>,
    natstat_box_at: Option<DateTime<Utc>>,
    our_plays: i64,
    cbbd_plays: Option<i32>,
    cbbd_shots: Option<i32>,
    cbbd_shots_with_location: Option<i32>,
    cbbd_plays_full_on_floor: Option<i32>,
}

/// A team box row from either source: the [`TEAM_FIELDS`] in order.
type TeamBox = [Option<i32>; 15];

#[derive(Debug, Clone)]
struct PlayerRow {
    name: String,
    starter: Option<bool>,
    minutes: Option<f64>,
    points: Option<i32>,
    rebounds: Option<i32>,
}

pub async fn run(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<ParityReport, sqlx::Error> {
    let rows: Vec<GameRow> = sqlx::query_as(
        "SELECT g.id, g.game_date, g.home_team_id, g.away_team_id, \
                ht.name AS home_name, at.name AS away_name, g.home_score, g.away_score, \
                g.status, g.score_source, \
                (EXISTS (SELECT 1 FROM team_game_stats t \
                          WHERE t.game_id = g.id AND t.source = 'cbbd') \
                 OR EXISTS (SELECT 1 FROM player_game_stats p \
                             WHERE p.game_id = g.id AND p.source = 'cbbd')) AS cbbd_rows, \
                COALESCE(g.natstat_id LIKE 'espn:%', false) AS created, \
                b.source_event_id AS espn_event_id, \
                COALESCE(b.source_home_is_away, false) AS flip, \
                c.cbbd_game_id, c.status AS cbbd_status, \
                c.home_points AS cbbd_home_points, c.away_points AS cbbd_away_points, \
                c.first_box_at AS cbbd_first_box_at, \
                (SELECT min(t.created_at) AT TIME ZONE 'UTC' FROM team_game_stats t \
                  WHERE t.game_id = g.id AND t.source = 'natstat') AS natstat_box_at, \
                (SELECT count(*) FROM play_by_play p \
                  WHERE p.game_id = g.id AND p.source = 'natstat') AS our_plays, \
                s.plays AS cbbd_plays, s.shots AS cbbd_shots, \
                s.shots_with_location AS cbbd_shots_with_location, \
                s.plays_full_on_floor AS cbbd_plays_full_on_floor \
         FROM games g \
         LEFT JOIN teams ht ON ht.id = g.home_team_id \
         LEFT JOIN teams at ON at.id = g.away_team_id \
         LEFT JOIN game_broadcasts b ON b.game_id = g.id \
         LEFT JOIN cbbd_games c ON c.espn_event_id = b.source_event_id \
         LEFT JOIN cbbd_game_play_summary s ON s.cbbd_game_id = c.cbbd_game_id \
         WHERE g.game_date BETWEEN $1 AND $2 \
         ORDER BY g.game_date, ht.name",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    let game_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let cbbd_ids: Vec<i32> = rows.iter().filter_map(|r| r.cbbd_game_id).collect();

    let ours_team = our_team_boxes(pool, &game_ids).await?;
    let cbbd_team = cbbd_team_boxes(pool, &cbbd_ids).await?;
    let ours_players = our_players(pool, &game_ids).await?;
    let cbbd_players = cbbd_players(pool, &cbbd_ids).await?;

    let mut games = Vec::with_capacity(rows.len());
    for r in &rows {
        let score_from_cbbd = r.score_source.as_deref() == Some("cbbd");
        let mut p = GameParity {
            game_id: r.id,
            game_date: r.game_date,
            matchup: format!(
                "{} vs {}",
                r.home_name.as_deref().unwrap_or("(non-D1)"),
                r.away_name.as_deref().unwrap_or("(non-D1)")
            ),
            espn_event_id: r.espn_event_id.clone(),
            cbbd_game_id: r.cbbd_game_id,
            ours_final: !score_from_cbbd
                && r.status
                    .as_deref()
                    .is_some_and(|s| s.to_ascii_lowercase().starts_with("final")),
            filled_by_cbbd: score_from_cbbd || r.cbbd_rows || r.created,
            score_from_cbbd,
            cbbd_final: r.cbbd_status.as_deref() == Some("final"),
            our_plays: r.our_plays,
            cbbd_plays: r.cbbd_plays,
            cbbd_shots: r.cbbd_shots,
            cbbd_shots_with_location: r.cbbd_shots_with_location,
            cbbd_plays_full_on_floor: r.cbbd_plays_full_on_floor,
            ..Default::default()
        };
        let swapped = cbbd_swapped(
            (r.home_score, r.away_score),
            (r.cbbd_home_points, r.cbbd_away_points),
            r.flip,
        );
        p.sides_swapped = r.cbbd_game_id.is_some() && swapped;
        let (c_home_pts, c_away_pts) = if swapped {
            (r.cbbd_away_points, r.cbbd_home_points)
        } else {
            (r.cbbd_home_points, r.cbbd_away_points)
        };
        let score = |h: Option<i32>, a: Option<i32>| match (h, a) {
            (Some(h), Some(a)) => Some(format!("{h}-{a}")),
            _ => None,
        };
        p.our_score = score(r.home_score, r.away_score);
        p.cbbd_score = score(c_home_pts, c_away_pts);
        if let (false, Some(h), Some(a), Some(ch), Some(ca)) = (
            score_from_cbbd,
            r.home_score,
            r.away_score,
            c_home_pts,
            c_away_pts,
        ) {
            p.score_match = Some(h == ch && a == ca);
        }
        if let (Some(c), Some(n)) = (r.cbbd_first_box_at, r.natstat_box_at) {
            p.cbbd_box_lag_hours = Some((c - n).num_minutes() as f64 / 60.0);
        }

        for (side, team_id, cbbd_is_home) in [
            ("home", r.home_team_id, !swapped),
            ("away", r.away_team_id, swapped),
        ] {
            // A non-D1 side: we store no team or player rows for it by design,
            // so CBBD's rows there are not a gap in ours.
            let Some(team_id) = team_id else {
                continue;
            };
            let team_id = Some(team_id);
            let ours = team_id.and_then(|t| ours_team.get(&(r.id, t)));
            let theirs = r
                .cbbd_game_id
                .and_then(|c| cbbd_team.get(&(c, cbbd_is_home)));
            p.natstat_box |= ours.is_some();
            p.cbbd_box |= theirs.is_some();
            if let (Some(o), Some(t)) = (ours, theirs) {
                for (i, field) in TEAM_FIELDS.iter().enumerate() {
                    match (o[i], t[i]) {
                        (Some(a), Some(b)) if a != b => {
                            p.team_diffs.push(format!("{side}.{field} {a}/{b}"))
                        }
                        (None, Some(_)) | (Some(_), None) => {
                            p.team_missing.push(format!("{side}.{field}"))
                        }
                        _ => {}
                    }
                }
            }

            let ours = team_id
                .and_then(|t| ours_players.get(&(r.id, t)))
                .cloned()
                .unwrap_or_default();
            let theirs = r
                .cbbd_game_id
                .and_then(|c| cbbd_players.get(&(c, cbbd_is_home)))
                .cloned()
                .unwrap_or_default();
            p.our_players += ours.len();
            p.cbbd_players += theirs.len();
            let names_o: Vec<&str> = ours.iter().map(|p| p.name.as_str()).collect();
            let names_t: Vec<&str> = theirs.iter().map(|p| p.name.as_str()).collect();
            for (oi, ti) in pair_box_names(&names_o, &names_t) {
                let (o, t) = (&ours[oi], &theirs[ti]);
                p.players_paired += 1;
                if let (Some(a), Some(b)) = (o.minutes, t.minutes) {
                    p.minutes_abs_diff += (a - b).abs();
                }
                if o.starter.is_some() && t.starter.is_some() && o.starter != t.starter {
                    p.starter_mismatches += 1;
                }
                let show = |v: Option<i32>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
                if o.points != t.points {
                    p.points_mismatches += 1;
                    p.player_diffs.push(format!(
                        "{} pts {}/{}",
                        t.name,
                        show(o.points),
                        show(t.points)
                    ));
                }
                if o.rebounds != t.rebounds {
                    p.rebound_mismatches += 1;
                    p.player_diffs.push(format!(
                        "{} reb {}/{}",
                        t.name,
                        show(o.rebounds),
                        show(t.rebounds)
                    ));
                }
            }
        }
        games.push(p);
    }

    let cbbd_unmatched: Vec<String> = sqlx::query_scalar(
        "SELECT to_char(c.start_date AT TIME ZONE 'America/New_York', 'YYYY-MM-DD') \
                || ' ' || coalesce(c.home_team, '?') || ' vs ' || coalesce(c.away_team, '?') \
         FROM cbbd_games c \
         WHERE (c.start_date AT TIME ZONE 'America/New_York')::date BETWEEN $1 AND $2 \
           AND NOT EXISTS (SELECT 1 FROM game_broadcasts b \
                           WHERE b.source_event_id = c.espn_event_id) \
         ORDER BY 1",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    Ok(ParityReport {
        from: Some(from),
        to: Some(to),
        games,
        cbbd_unmatched,
    })
}

/// Whether CBBD lists a game's sides the other way round from our `games` row.
/// The scores decide when they can: identical means the same sides, reversed
/// means swapped (a final score is never tied). CBBD's sides do not always
/// follow ESPN's, so `source_home_is_away` (ESPN's flip) is only the fallback,
/// for a game whose scores agree in neither order.
pub(crate) fn cbbd_swapped(
    ours: (Option<i32>, Option<i32>),
    cbbd: (Option<i32>, Option<i32>),
    espn_flip: bool,
) -> bool {
    match (ours, cbbd) {
        ((Some(h), Some(a)), (Some(ch), Some(ca))) if h == ch && a == ca => false,
        ((Some(h), Some(a)), (Some(ch), Some(ca))) if h == ca && a == ch => true,
        _ => espn_flip,
    }
}

async fn our_team_boxes(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<HashMap<(Uuid, Uuid), TeamBox>, sqlx::Error> {
    let sql = format!(
        "SELECT game_id, team_id, {} FROM team_game_stats \
         WHERE game_id = ANY($1) AND source = 'natstat'",
        TEAM_FIELDS.join(", ")
    );
    let rows = sqlx::query(&sql).bind(game_ids).fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        use sqlx::Row;
        let mut b: TeamBox = [None; 15];
        for (i, f) in TEAM_FIELDS.iter().enumerate() {
            b[i] = row.try_get(*f)?;
        }
        out.insert((row.try_get("game_id")?, row.try_get("team_id")?), b);
    }
    Ok(out)
}

/// Team fields where CBBD's team line and ours count different things: CBBD's
/// adds team rebounds, team turnovers and bench technicals (credited to no
/// player), while `team_game_stats` is the sum of the players. These are
/// compared on CBBD's player sums instead, which matched ours on every game
/// checked (2026-11-12..18).
const PLAYER_SUM_FIELDS: [&str; 5] = [
    "off_rebounds",
    "def_rebounds",
    "total_rebounds",
    "turnovers",
    "fouls",
];

async fn cbbd_team_boxes(
    pool: &PgPool,
    cbbd_ids: &[i32],
) -> Result<HashMap<(i32, bool), TeamBox>, sqlx::Error> {
    let cols: Vec<String> = TEAM_FIELDS
        .iter()
        .map(|f| {
            if PLAYER_SUM_FIELDS.contains(f) {
                format!(
                    "(SELECT sum(p.{f})::int FROM cbbd_player_game_stats p \
                      WHERE p.cbbd_game_id = t.cbbd_game_id AND p.is_home = t.is_home) AS {f}"
                )
            } else {
                format!("t.{f}")
            }
        })
        .collect();
    let sql = format!(
        "SELECT t.cbbd_game_id, t.is_home, {} FROM cbbd_team_game_stats t \
         WHERE t.cbbd_game_id = ANY($1)",
        cols.join(", ")
    );
    let rows = sqlx::query(&sql).bind(cbbd_ids).fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        use sqlx::Row;
        let mut b: TeamBox = [None; 15];
        for (i, f) in TEAM_FIELDS.iter().enumerate() {
            b[i] = row.try_get(*f)?;
        }
        out.insert((row.try_get("cbbd_game_id")?, row.try_get("is_home")?), b);
    }
    Ok(out)
}

/// One player row from either source, read by column name so both queries
/// share it. `minutes` is a float on our side and an integer on CBBD's.
fn player_row(row: &sqlx::postgres::PgRow) -> Result<PlayerRow, sqlx::Error> {
    use sqlx::Row;
    Ok(PlayerRow {
        name: row.try_get("name")?,
        starter: row.try_get("starter")?,
        minutes: row.try_get("minutes")?,
        points: row.try_get("points")?,
        rebounds: row.try_get("total_rebounds")?,
    })
}

async fn our_players(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<HashMap<(Uuid, Uuid), Vec<PlayerRow>>, sqlx::Error> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT s.game_id, s.team_id, p.name, s.starter, s.minutes, s.points, \
                s.total_rebounds \
         FROM player_game_stats s JOIN players p ON p.id = s.player_id \
         WHERE s.game_id = ANY($1) AND s.source = 'natstat'",
    )
    .bind(game_ids)
    .fetch_all(pool)
    .await?;
    let mut out: HashMap<(Uuid, Uuid), Vec<PlayerRow>> = HashMap::new();
    for row in &rows {
        out.entry((row.try_get("game_id")?, row.try_get("team_id")?))
            .or_default()
            .push(player_row(row)?);
    }
    Ok(out)
}

async fn cbbd_players(
    pool: &PgPool,
    cbbd_ids: &[i32],
) -> Result<HashMap<(i32, bool), Vec<PlayerRow>>, sqlx::Error> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT cbbd_game_id, is_home, name, starter, minutes::float8 AS minutes, points, \
                total_rebounds \
         FROM cbbd_player_game_stats WHERE cbbd_game_id = ANY($1)",
    )
    .bind(cbbd_ids)
    .fetch_all(pool)
    .await?;
    let mut out: HashMap<(i32, bool), Vec<PlayerRow>> = HashMap::new();
    for row in &rows {
        out.entry((row.try_get("cbbd_game_id")?, row.try_get("is_home")?))
            .or_default()
            .push(player_row(row)?);
    }
    Ok(out)
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "-".to_string()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / d as f64)
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

impl ParityReport {
    /// The human summary `source-parity` prints.
    pub fn summary(&self) -> String {
        let g = &self.games;
        let mut s = String::new();
        let window = match (self.from, self.to) {
            (Some(f), Some(t)) => format!("{f}..{t}"),
            _ => String::new(),
        };
        let matched: Vec<&GameParity> = g.iter().filter(|p| p.cbbd_game_id.is_some()).collect();
        let _ = writeln!(s, "source parity {window}: {} of our games", g.len());
        let _ = writeln!(
            s,
            "  matched to a CBBD game: {} ({}); no ESPN event: {}; event not in CBBD: {}",
            matched.len(),
            pct(matched.len(), g.len()),
            g.iter().filter(|p| p.espn_event_id.is_none()).count(),
            g.iter()
                .filter(|p| p.espn_event_id.is_some() && p.cbbd_game_id.is_none())
                .count(),
        );
        let _ = writeln!(
            s,
            "  CBBD games with no game of ours: {}",
            self.cbbd_unmatched.len()
        );

        let _ = writeln!(s, "\nstatus (matched games)");
        let both_final = matched
            .iter()
            .filter(|p| p.ours_final && p.cbbd_final)
            .count();
        let _ = writeln!(
            s,
            "  final in both: {both_final}; final only in CBBD: {}; final only in ours: {}",
            matched
                .iter()
                .filter(|p| p.cbbd_final && !p.ours_final)
                .count(),
            matched
                .iter()
                .filter(|p| p.ours_final && !p.cbbd_final)
                .count(),
        );
        let _ = writeln!(
            s,
            "  home and away listed the other way round in CBBD: {}",
            matched.iter().filter(|p| p.sides_swapped).count()
        );
        let scored: Vec<&GameParity> = matched
            .iter()
            .copied()
            .filter(|p| p.score_match.is_some())
            .collect();
        let _ = writeln!(
            s,
            "  score disagreements: {} of {}; scores the gap-fill set from CBBD (not compared): {}",
            scored
                .iter()
                .filter(|p| p.score_match == Some(false))
                .count(),
            scored.len(),
            matched.iter().filter(|p| p.score_from_cbbd).count(),
        );

        let finals: Vec<&GameParity> = matched.iter().copied().filter(|p| p.cbbd_final).collect();
        let _ = writeln!(s, "\nbox scores (games CBBD lists final: {})", finals.len());
        let _ = writeln!(
            s,
            "  both: {}; CBBD only: {} ({} filled from CBBD by the gap-fill); NatStat only: {}; \
             neither: {}",
            finals
                .iter()
                .filter(|p| p.natstat_box && p.cbbd_box)
                .count(),
            finals
                .iter()
                .filter(|p| !p.natstat_box && p.cbbd_box)
                .count(),
            finals
                .iter()
                .filter(|p| !p.natstat_box && p.cbbd_box && p.filled_by_cbbd)
                .count(),
            finals
                .iter()
                .filter(|p| p.natstat_box && !p.cbbd_box)
                .count(),
            finals
                .iter()
                .filter(|p| !p.natstat_box && !p.cbbd_box)
                .count(),
        );
        if let Some(m) = median(finals.iter().filter_map(|p| p.cbbd_box_lag_hours).collect()) {
            let _ = writeln!(
                s,
                "  median hours CBBD's box arrived after NatStat's: {m:.1} (negative = earlier)"
            );
        }

        let both: Vec<&GameParity> = finals
            .iter()
            .copied()
            .filter(|p| p.natstat_box && p.cbbd_box)
            .collect();
        let mut field_counts: BTreeMap<String, usize> = BTreeMap::new();
        for p in &both {
            for d in &p.team_diffs {
                let key = d.split_whitespace().next().unwrap_or(d);
                let field = key.split_once('.').map_or(key, |(_, f)| f);
                *field_counts.entry(field.to_string()).or_default() += 1;
            }
        }
        let _ = writeln!(
            s,
            "  games with any team-box difference: {} of {}",
            both.iter().filter(|p| !p.team_diffs.is_empty()).count(),
            both.len()
        );
        if !field_counts.is_empty() {
            let list: Vec<String> = field_counts
                .iter()
                .map(|(f, n)| format!("{f} {n}"))
                .collect();
            let _ = writeln!(s, "  team sides differing, by field: {}", list.join(", "));
        }
        let missing = both.iter().filter(|p| !p.team_missing.is_empty()).count();
        if missing > 0 {
            let _ = writeln!(s, "  games with a field only one source fills: {missing}");
        }

        let ours: usize = both.iter().map(|p| p.our_players).sum();
        let theirs: usize = both.iter().map(|p| p.cbbd_players).sum();
        let paired: usize = both.iter().map(|p| p.players_paired).sum();
        let minutes: f64 = both.iter().map(|p| p.minutes_abs_diff).sum();
        let _ = writeln!(s, "\nplayers (games with both box scores)");
        let _ = writeln!(
            s,
            "  rows: ours {ours}, CBBD {theirs}; paired {paired} ({} of ours, {} of CBBD's)",
            pct(paired, ours),
            pct(paired, theirs)
        );
        if paired > 0 {
            let _ = writeln!(
                s,
                "  mean |minutes difference|: {:.2}; starter mismatches {}; points {}; rebounds {}",
                minutes / paired as f64,
                pct(both.iter().map(|p| p.starter_mismatches).sum(), paired),
                pct(both.iter().map(|p| p.points_mismatches).sum(), paired),
                pct(both.iter().map(|p| p.rebound_mismatches).sum(), paired),
            );
        }

        let with_plays: Vec<&GameParity> = finals
            .iter()
            .copied()
            .filter(|p| p.cbbd_plays.is_some())
            .collect();
        let _ = writeln!(s, "\nplay-by-play (CBBD-final games)");
        let _ = writeln!(
            s,
            "  games with plays: NatStat {}, CBBD {}",
            finals.iter().filter(|p| p.our_plays > 0).count(),
            with_plays.len()
        );
        let shots: i32 = with_plays.iter().filter_map(|p| p.cbbd_shots).sum();
        let located: i32 = with_plays
            .iter()
            .filter_map(|p| p.cbbd_shots_with_location)
            .sum();
        let plays: i32 = with_plays.iter().filter_map(|p| p.cbbd_plays).sum();
        let full: i32 = with_plays
            .iter()
            .filter_map(|p| p.cbbd_plays_full_on_floor)
            .sum();
        let _ = writeln!(
            s,
            "  CBBD shots with a location: {}; plays with all 10 on the floor: {}",
            pct(located as usize, shots as usize),
            pct(full as usize, plays as usize)
        );
        if let (Some(o), Some(c)) = (
            median(
                with_plays
                    .iter()
                    .filter(|p| p.our_plays > 0)
                    .map(|p| p.our_plays as f64)
                    .collect(),
            ),
            median(
                with_plays
                    .iter()
                    .filter_map(|p| p.cbbd_plays.map(f64::from))
                    .collect(),
            ),
        ) {
            let _ = writeln!(s, "  median plays per game: NatStat {o:.0}, CBBD {c:.0}");
        }

        let mismatched: Vec<&GameParity> = scored
            .iter()
            .copied()
            .filter(|p| p.score_match == Some(false))
            .collect();
        if !mismatched.is_empty() {
            let _ = writeln!(s, "\nscore disagreements (home-away: ours / CBBD)");
            for p in mismatched.iter().take(20) {
                let _ = writeln!(
                    s,
                    "  {} {}: {} / {} (ESPN {})",
                    p.game_date,
                    p.matchup,
                    p.our_score.as_deref().unwrap_or("-"),
                    p.cbbd_score.as_deref().unwrap_or("-"),
                    p.espn_event_id.as_deref().unwrap_or("-"),
                );
            }
        }
        let team_differing: Vec<&GameParity> = both
            .iter()
            .copied()
            .filter(|p| !p.team_diffs.is_empty())
            .collect();
        if !team_differing.is_empty() {
            let _ = writeln!(s, "\nteam box differences (ours/CBBD, first 40)");
            for p in team_differing.iter().take(40) {
                let _ = writeln!(
                    s,
                    "  {} {} (ESPN {}): {}",
                    p.game_date,
                    p.matchup,
                    p.espn_event_id.as_deref().unwrap_or("-"),
                    p.team_diffs.join(", ")
                );
            }
        }
        let player_differing: Vec<&GameParity> = both
            .iter()
            .copied()
            .filter(|p| !p.player_diffs.is_empty())
            .collect();
        if !player_differing.is_empty() {
            let _ = writeln!(s, "\nplayer stat differences (ours/CBBD, first 40 games)");
            for p in player_differing.iter().take(40) {
                let _ = writeln!(
                    s,
                    "  {} {} (ESPN {}): {}",
                    p.game_date,
                    p.matchup,
                    p.espn_event_id.as_deref().unwrap_or("-"),
                    p.player_diffs.join(", ")
                );
            }
        }
        let cbbd_only: Vec<&GameParity> = finals
            .iter()
            .copied()
            .filter(|p| !p.natstat_box && p.cbbd_box)
            .collect();
        if !cbbd_only.is_empty() {
            let _ = writeln!(s, "\nbox score in CBBD but not NatStat (first 20)");
            for p in cbbd_only.iter().take(20) {
                let filled = if p.filled_by_cbbd {
                    " (filled by the gap-fill)"
                } else {
                    ""
                };
                let _ = writeln!(s, "  {} {}{filled}", p.game_date, p.matchup);
            }
        }
        s
    }

    /// One CSV row per game, with a header.
    pub fn csv(&self) -> String {
        let mut s = String::from(
            "game_date,matchup,game_id,espn_event_id,cbbd_game_id,ours_final,cbbd_final,\
             filled_by_cbbd,score_from_cbbd,sides_swapped,our_score,cbbd_score,score_match,natstat_box,cbbd_box,cbbd_box_lag_hours,team_diffs,team_missing,\
             our_players,cbbd_players,players_paired,minutes_abs_diff,starter_mismatches,\
             points_mismatches,rebound_mismatches,our_plays,cbbd_plays,cbbd_shots,\
             cbbd_shots_with_location,cbbd_plays_full_on_floor,player_diffs\n",
        );
        let opt = |v: Option<String>| v.unwrap_or_default();
        for p in &self.games {
            let _ = writeln!(
                s,
                "{},\"{}\",{},{},{},{},{},{},{},{},{},{},{},{},{},{},\"{}\",\"{}\",{},{},{},{:.0},{},{},{},{},{},{},{},{},\"{}\"",
                p.game_date,
                p.matchup.replace('"', "'"),
                p.game_id,
                opt(p.espn_event_id.clone()),
                opt(p.cbbd_game_id.map(|v| v.to_string())),
                p.ours_final,
                p.cbbd_final,
                p.filled_by_cbbd,
                p.score_from_cbbd,
                p.sides_swapped,
                opt(p.our_score.clone()),
                opt(p.cbbd_score.clone()),
                opt(p.score_match.map(|v| v.to_string())),
                p.natstat_box,
                p.cbbd_box,
                opt(p.cbbd_box_lag_hours.map(|v| format!("{v:.1}"))),
                p.team_diffs.join(" "),
                p.team_missing.join(" "),
                p.our_players,
                p.cbbd_players,
                p.players_paired,
                p.minutes_abs_diff,
                p.starter_mismatches,
                p.points_mismatches,
                p.rebound_mismatches,
                p.our_plays,
                opt(p.cbbd_plays.map(|v| v.to_string())),
                opt(p.cbbd_shots.map(|v| v.to_string())),
                opt(p.cbbd_shots_with_location.map(|v| v.to_string())),
                opt(p.cbbd_plays_full_on_floor.map(|v| v.to_string())),
                p.player_diffs.join("; ").replace('"', "'"),
            );
        }
        s
    }
}

/// How many Eastern days back the nightly's `source_parity` step looks, ending
/// yesterday. Each day is rewritten on every night it falls in the window, so
/// its stored row is the look from [`PARITY_DAYS`] nights later, when a box
/// score either source posted a day or two late has landed. The CBBD shadow
/// copy reaches back two days (`CBBD_SHADOW_DAYS`), so the third night sees
/// each day once more after CBBD's copy of it has settled.
pub const PARITY_DAYS: i64 = 3;

/// The step's bound. It reads and writes local tables only (a handful of
/// batched statements over a few hundred games), so this guards against a
/// stalled database rather than a budget it is expected to use.
pub const PARITY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Longest ledger note the step writes, and how many disagreeing games it
/// names there. The full list is in `source_parity_games`.
const NOTE_MAX_CHARS: usize = 2000;
const NOTE_MAX_GAMES: usize = 10;

impl GameParity {
    /// CBBD lists the game final and both sources have a box score for it:
    /// the games the box comparison covers.
    pub fn compared(&self) -> bool {
        self.cbbd_final && self.natstat_box && self.cbbd_box
    }

    /// A value disagreement: the final scores differ, or a compared game's
    /// team box or player stats do.
    pub fn disagrees(&self) -> bool {
        self.score_match == Some(false)
            || (self.compared() && (!self.team_diffs.is_empty() || !self.player_diffs.is_empty()))
    }
}

/// One day's counts, as stored in `source_parity_days` (migration 079 says
/// what each column means).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct DayCounts {
    pub games: i32,
    pub matched: i32,
    pub missing_in_cbbd: i32,
    pub compared: i32,
    pub identical: i32,
    pub team_box_diff: i32,
    pub player_diff: i32,
    pub score_compared: i32,
    pub score_disagreements: i32,
    pub cbbd_only: i32,
    pub natstat_only: i32,
    pub filled_by_cbbd: i32,
    pub cbbd_unmatched: i32,
}

impl DayCounts {
    fn add_game(&mut self, p: &GameParity) {
        let n = i32::from;
        let compared = p.compared();
        self.games += 1;
        self.matched += n(p.cbbd_game_id.is_some());
        self.missing_in_cbbd += n(p.ours_final && !p.cbbd_final);
        self.compared += n(compared);
        self.identical += n(compared && !p.disagrees());
        self.team_box_diff += n(compared && !p.team_diffs.is_empty());
        self.player_diff += n(compared && !p.player_diffs.is_empty());
        self.score_compared += n(p.score_match.is_some());
        self.score_disagreements += n(p.score_match == Some(false));
        self.cbbd_only += n(p.cbbd_final && p.cbbd_box && !p.natstat_box);
        self.natstat_only += n(p.cbbd_final && p.natstat_box && !p.cbbd_box);
        self.filled_by_cbbd += n(p.filled_by_cbbd);
    }

    fn add(&mut self, o: &DayCounts) {
        self.games += o.games;
        self.matched += o.matched;
        self.missing_in_cbbd += o.missing_in_cbbd;
        self.compared += o.compared;
        self.identical += o.identical;
        self.team_box_diff += o.team_box_diff;
        self.player_diff += o.player_diff;
        self.score_compared += o.score_compared;
        self.score_disagreements += o.score_disagreements;
        self.cbbd_only += o.cbbd_only;
        self.natstat_only += o.natstat_only;
        self.filled_by_cbbd += o.filled_by_cbbd;
        self.cbbd_unmatched += o.cbbd_unmatched;
    }

    /// The nightly summary's counts, e.g. `92/95 identical · 2 team-box diffs ·
    /// 0 player diffs · 1/120 score disagreements`, with the presence gaps
    /// appended only when there are any.
    pub fn slack_summary(&self) -> String {
        let mut s = format!(
            "{}/{} identical · {} team-box diffs · {} player diffs · {}/{} score disagreements",
            self.identical,
            self.compared,
            self.team_box_diff,
            self.player_diff,
            self.score_disagreements,
            self.score_compared,
        );
        for (n, what) in [
            (self.cbbd_only, "CBBD-only"),
            (self.filled_by_cbbd, "filled by CBBD"),
            (self.natstat_only, "NatStat-only"),
            (self.missing_in_cbbd, "missing in CBBD"),
        ] {
            if n > 0 {
                let _ = write!(s, " · {n} {what}");
            }
        }
        s
    }

    fn notes(&self) -> String {
        format!(
            "{} games, {} matched to CBBD, {} compared, {} identical, {} team-box diff, \
             {} player diff, {}/{} score disagreements, {} CBBD-only, {} NatStat-only, \
             {} filled by CBBD, {} final in NatStat but not in CBBD, {} CBBD games unmatched",
            self.games,
            self.matched,
            self.compared,
            self.identical,
            self.team_box_diff,
            self.player_diff,
            self.score_disagreements,
            self.score_compared,
            self.cbbd_only,
            self.natstat_only,
            self.filled_by_cbbd,
            self.missing_in_cbbd,
            self.cbbd_unmatched,
        )
    }
}

impl ParityReport {
    /// Counts for every day of the window, days with no games included, in
    /// date order. Empty when the report has no window.
    pub fn days(&self) -> Vec<(NaiveDate, DayCounts)> {
        let (Some(from), Some(to)) = (self.from, self.to) else {
            return Vec::new();
        };
        let mut days: BTreeMap<NaiveDate, DayCounts> = from
            .iter_days()
            .take_while(|d| *d <= to)
            .map(|d| (d, DayCounts::default()))
            .collect();
        for p in &self.games {
            if let Some(c) = days.get_mut(&p.game_date) {
                c.add_game(p);
            }
        }
        // `cbbd_unmatched` entries lead with their Eastern date.
        for u in &self.cbbd_unmatched {
            let date = u.get(..10).and_then(|d| d.parse::<NaiveDate>().ok());
            if let Some(c) = date.and_then(|d| days.get_mut(&d)) {
                c.cbbd_unmatched += 1;
            }
        }
        days.into_iter().collect()
    }

    pub fn totals(&self) -> DayCounts {
        let mut t = DayCounts::default();
        for (_, c) in self.days() {
            t.add(&c);
        }
        t
    }

    /// The ledger note: the window's totals, then the disagreeing games with
    /// both values, bounded to [`NOTE_MAX_GAMES`] games and
    /// [`NOTE_MAX_CHARS`] characters.
    pub fn ledger_notes(&self) -> String {
        let window = match (self.from, self.to) {
            (Some(f), Some(t)) => format!("{f}..{t}: "),
            _ => String::new(),
        };
        let mut s = format!("{window}{}", self.totals().notes());
        let disagreeing: Vec<&GameParity> = self.games.iter().filter(|p| p.disagrees()).collect();
        if !disagreeing.is_empty() {
            let _ = write!(s, "; disagreements (ours/CBBD):");
            for p in disagreeing.iter().take(NOTE_MAX_GAMES) {
                let _ = write!(s, " [{}]", game_disagreement(p));
            }
            if disagreeing.len() > NOTE_MAX_GAMES {
                let _ = write!(
                    s,
                    " and {} more in source_parity_games",
                    disagreeing.len() - NOTE_MAX_GAMES
                );
            }
        }
        truncate_chars(s, NOTE_MAX_CHARS)
    }
}

/// One disagreeing game as `date matchup (ESPN id): score 70-60/70-61;
/// home.fouls 26/27; Jo Doe pts 12/14`.
fn game_disagreement(p: &GameParity) -> String {
    let mut parts = Vec::new();
    if p.score_match == Some(false) {
        parts.push(format!(
            "score {}/{}",
            p.our_score.as_deref().unwrap_or("-"),
            p.cbbd_score.as_deref().unwrap_or("-")
        ));
    }
    if p.compared() {
        parts.extend(p.team_diffs.iter().cloned());
        parts.extend(p.player_diffs.iter().cloned());
    }
    format!(
        "{} {} (ESPN {}): {}",
        p.game_date,
        p.matchup,
        p.espn_event_id.as_deref().unwrap_or("-"),
        parts.join("; ")
    )
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Store a report's window: one `source_parity_days` row per day, replacing
/// what an earlier run stored for it, and one `source_parity_games` row per
/// disagreeing game, after deleting the window's earlier game rows so a
/// disagreement that has since resolved does not linger. One transaction.
/// Returns the disagreement rows written.
pub async fn record(
    pool: &PgPool,
    report: &ParityReport,
    run_id: Option<Uuid>,
) -> Result<usize, sqlx::Error> {
    let (Some(from), Some(to)) = (report.from, report.to) else {
        return Ok(0);
    };
    let days = report.days();
    let dates: Vec<NaiveDate> = days.iter().map(|(d, _)| *d).collect();
    let col = |f: fn(&DayCounts) -> i32| -> Vec<i32> { days.iter().map(|(_, c)| f(c)).collect() };

    // Diff lists travel as one newline-joined string per game (no name or
    // field holds a newline), so the insert is one statement however many
    // games disagree. A score-only disagreement stores empty lists.
    const SEP: &str = "\n";
    let disagreeing: Vec<&GameParity> = report.games.iter().filter(|p| p.disagrees()).collect();
    let list = |p: &GameParity, v: &[String]| {
        if p.compared() {
            v.join(SEP)
        } else {
            String::new()
        }
    };
    let ids: Vec<Uuid> = disagreeing.iter().map(|p| p.game_id).collect();
    let game_dates: Vec<NaiveDate> = disagreeing.iter().map(|p| p.game_date).collect();
    let matchups: Vec<String> = disagreeing.iter().map(|p| p.matchup.clone()).collect();
    let espn: Vec<Option<String>> = disagreeing
        .iter()
        .map(|p| p.espn_event_id.clone())
        .collect();
    let cbbd: Vec<Option<i32>> = disagreeing.iter().map(|p| p.cbbd_game_id).collect();
    let ours: Vec<Option<String>> = disagreeing.iter().map(|p| p.our_score.clone()).collect();
    let theirs: Vec<Option<String>> = disagreeing.iter().map(|p| p.cbbd_score.clone()).collect();
    let score_match: Vec<Option<bool>> = disagreeing.iter().map(|p| p.score_match).collect();
    let team: Vec<String> = disagreeing.iter().map(|p| list(p, &p.team_diffs)).collect();
    let player: Vec<String> = disagreeing
        .iter()
        .map(|p| list(p, &p.player_diffs))
        .collect();

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM source_parity_games WHERE game_date BETWEEN $1 AND $2")
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    // `string_to_array('', …)` is the empty array.
    sqlx::query(
        "INSERT INTO source_parity_games \
         (game_id, game_date, run_id, matchup, espn_event_id, cbbd_game_id, our_score, \
          cbbd_score, score_match, team_diffs, player_diffs) \
         SELECT u.game_id, u.game_date, $1, u.matchup, u.espn_event_id, u.cbbd_game_id, \
                u.our_score, u.cbbd_score, u.score_match, \
                string_to_array(u.team, $12), string_to_array(u.player, $12) \
         FROM UNNEST($2::uuid[], $3::date[], $4::text[], $5::text[], $6::int[], $7::text[], \
                     $8::text[], $9::bool[], $10::text[], $11::text[]) \
              AS u(game_id, game_date, matchup, espn_event_id, cbbd_game_id, our_score, \
                   cbbd_score, score_match, team, player) \
         ON CONFLICT (game_id) DO UPDATE SET \
           game_date = EXCLUDED.game_date, computed_at = now(), run_id = EXCLUDED.run_id, \
           matchup = EXCLUDED.matchup, espn_event_id = EXCLUDED.espn_event_id, \
           cbbd_game_id = EXCLUDED.cbbd_game_id, our_score = EXCLUDED.our_score, \
           cbbd_score = EXCLUDED.cbbd_score, score_match = EXCLUDED.score_match, \
           team_diffs = EXCLUDED.team_diffs, player_diffs = EXCLUDED.player_diffs",
    )
    .bind(run_id)
    .bind(&ids)
    .bind(&game_dates)
    .bind(&matchups)
    .bind(&espn)
    .bind(&cbbd)
    .bind(&ours)
    .bind(&theirs)
    .bind(&score_match)
    .bind(&team)
    .bind(&player)
    .bind(SEP)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO source_parity_days \
         (game_date, computed_at, run_id, games, matched, missing_in_cbbd, compared, identical, \
          team_box_diff, player_diff, score_compared, score_disagreements, cbbd_only, \
          natstat_only, filled_by_cbbd, cbbd_unmatched) \
         SELECT u.d, now(), $1, u.games, u.matched, u.missing, u.compared, u.identical, \
                u.team, u.player, u.score_compared, u.score_dis, u.cbbd_only, u.natstat_only, \
                u.filled, u.unmatched \
         FROM UNNEST($2::date[], $3::int[], $4::int[], $5::int[], $6::int[], $7::int[], \
                     $8::int[], $9::int[], $10::int[], $11::int[], $12::int[], $13::int[], \
                     $14::int[], $15::int[]) \
              AS u(d, games, matched, missing, compared, identical, team, player, \
                   score_compared, score_dis, cbbd_only, natstat_only, filled, unmatched) \
         ON CONFLICT (game_date) DO UPDATE SET \
           computed_at = EXCLUDED.computed_at, run_id = EXCLUDED.run_id, \
           games = EXCLUDED.games, matched = EXCLUDED.matched, \
           missing_in_cbbd = EXCLUDED.missing_in_cbbd, compared = EXCLUDED.compared, \
           identical = EXCLUDED.identical, team_box_diff = EXCLUDED.team_box_diff, \
           player_diff = EXCLUDED.player_diff, score_compared = EXCLUDED.score_compared, \
           score_disagreements = EXCLUDED.score_disagreements, cbbd_only = EXCLUDED.cbbd_only, \
           natstat_only = EXCLUDED.natstat_only, filled_by_cbbd = EXCLUDED.filled_by_cbbd, \
           cbbd_unmatched = EXCLUDED.cbbd_unmatched",
    )
    .bind(run_id)
    .bind(&dates)
    .bind(col(|c| c.games))
    .bind(col(|c| c.matched))
    .bind(col(|c| c.missing_in_cbbd))
    .bind(col(|c| c.compared))
    .bind(col(|c| c.identical))
    .bind(col(|c| c.team_box_diff))
    .bind(col(|c| c.player_diff))
    .bind(col(|c| c.score_compared))
    .bind(col(|c| c.score_disagreements))
    .bind(col(|c| c.cbbd_only))
    .bind(col(|c| c.natstat_only))
    .bind(col(|c| c.filled_by_cbbd))
    .bind(col(|c| c.cbbd_unmatched))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(disagreeing.len())
}

/// The nightly's `source_parity` step: compare the last [`PARITY_DAYS`]
/// Eastern days and [`record`] them, under [`PARITY_DEADLINE`]. Local tables
/// only, no API call. Best-effort: a failure is a failed ledger row and never
/// degrades the run. Returns the line for the nightly's Slack summary, or
/// `None` when the step was skipped (the simulate harness, which has no CBBD
/// shadow rows).
pub async fn nightly_step(pool: &PgPool, ledger: &RunLedger<'_>) -> Option<String> {
    let t0 = Utc::now();
    if crate::simulated_today().is_some() {
        ledger
            .record(
                "source_parity",
                StepStatus::Skipped,
                None,
                t0,
                Some("simulated clock"),
            )
            .await;
        return None;
    }
    let to = crate::today_eastern() - chrono::Duration::days(1);
    let from = to - chrono::Duration::days(PARITY_DAYS - 1);
    let work = async {
        let report = run(pool, from, to).await?;
        let written = record(pool, &report, Some(ledger.run_id())).await?;
        Ok::<_, sqlx::Error>((report, written))
    };
    let label = format!(
        "*Parity ({} → {}):*",
        from.format("%m-%d"),
        to.format("%m-%d")
    );
    let msg = match tokio::time::timeout(PARITY_DEADLINE, work).await {
        Ok(Ok((report, written))) => {
            ledger
                .record_with_notes(
                    "source_parity",
                    StepStatus::Ok,
                    Some(written as i64),
                    t0,
                    None,
                    Some(&report.ledger_notes()),
                )
                .await;
            return Some(format!("{label}  {}", report.totals().slack_summary()));
        }
        Ok(Err(e)) => e.to_string(),
        Err(_) => format!("abandoned after {}s", PARITY_DEADLINE.as_secs()),
    };
    tracing::warn!(error = %msg, "source parity failed; continuing");
    ledger
        .record("source_parity", StepStatus::Failed, None, t0, Some(&msg))
        .await;
    Some(format!(
        "{label}  not recorded ({})",
        truncate_chars(msg, 120)
    ))
}

#[derive(sqlx::FromRow)]
struct StoredDay {
    game_date: NaiveDate,
    #[sqlx(flatten)]
    counts: DayCounts,
}

#[derive(sqlx::FromRow)]
struct StoredGame {
    game_date: NaiveDate,
    matchup: String,
    espn_event_id: Option<String>,
    our_score: Option<String>,
    cbbd_score: Option<String>,
    score_match: Option<bool>,
    team_diffs: Vec<String>,
    player_diffs: Vec<String>,
}

/// The stored record over the last `days` days that have one (ending at the
/// latest stored day): a row per day, the totals, and every stored
/// disagreement. What `source-parity --summary-days N` prints, so the
/// acceptance call in `docs/natstat_migration_plan.md` §5 is one command.
pub async fn trend(pool: &PgPool, days: i64) -> Result<String, sqlx::Error> {
    let rows: Vec<StoredDay> = sqlx::query_as(
        "SELECT game_date, games, matched, missing_in_cbbd, compared, identical, \
                team_box_diff, player_diff, score_compared, score_disagreements, cbbd_only, \
                natstat_only, filled_by_cbbd, cbbd_unmatched \
         FROM source_parity_days \
         WHERE game_date > (SELECT max(game_date) FROM source_parity_days) - $1::int \
         ORDER BY game_date",
    )
    .bind(days.clamp(1, 3660) as i32)
    .fetch_all(pool)
    .await?;
    let mut s = String::new();
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        let _ = writeln!(s, "no stored source parity (source_parity_days is empty)");
        return Ok(s);
    };
    let (first, last) = (first.game_date, last.game_date);
    let _ = writeln!(
        s,
        "stored source parity {first}..{last} ({} days)\n",
        rows.len()
    );
    let _ = writeln!(
        s,
        "{:<10} {:>5} {:>7} {:>8} {:>9} {:>8} {:>6} {:>11} {:>9} {:>6} {:>12} {:>13}",
        "date",
        "games",
        "matched",
        "compared",
        "identical",
        "team-box",
        "player",
        "score dis.",
        "CBBD-only",
        "filled",
        "NatStat-only",
        "missing CBBD"
    );
    let mut total = DayCounts::default();
    let line = |s: &mut String, label: &str, c: &DayCounts| {
        let _ = writeln!(
            s,
            "{:<10} {:>5} {:>7} {:>8} {:>9} {:>8} {:>6} {:>11} {:>9} {:>6} {:>12} {:>13}",
            label,
            c.games,
            c.matched,
            c.compared,
            c.identical,
            c.team_box_diff,
            c.player_diff,
            format!("{}/{}", c.score_disagreements, c.score_compared),
            c.cbbd_only,
            c.filled_by_cbbd,
            c.natstat_only,
            c.missing_in_cbbd,
        );
    };
    for r in &rows {
        line(&mut s, &r.game_date.to_string(), &r.counts);
        total.add(&r.counts);
    }
    line(&mut s, "total", &total);
    let _ = writeln!(
        s,
        "\nidentical: {} of {} compared games ({}); CBBD games with no game of ours: {}",
        total.identical,
        total.compared,
        pct(total.identical as usize, total.compared as usize),
        total.cbbd_unmatched
    );

    let games: Vec<StoredGame> = sqlx::query_as(
        "SELECT game_date, matchup, espn_event_id, our_score, cbbd_score, score_match, \
                team_diffs, player_diffs \
         FROM source_parity_games WHERE game_date BETWEEN $1 AND $2 \
         ORDER BY game_date, matchup",
    )
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;
    if !games.is_empty() {
        let _ = writeln!(s, "\ndisagreements (ours/CBBD): {}", games.len());
        for g in &games {
            let mut parts = Vec::new();
            if g.score_match == Some(false) {
                parts.push(format!(
                    "score {}/{}",
                    g.our_score.as_deref().unwrap_or("-"),
                    g.cbbd_score.as_deref().unwrap_or("-")
                ));
            }
            parts.extend(g.team_diffs.iter().cloned());
            parts.extend(g.player_diffs.iter().cloned());
            let _ = writeln!(
                s,
                "  {} {} (ESPN {}): {}",
                g.game_date,
                g.matchup,
                g.espn_event_id.as_deref().unwrap_or("-"),
                parts.join("; ")
            );
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_names_each_disagreement_with_both_values() {
        let game = |score_match: bool, team: &[&str], players: &[&str]| GameParity {
            game_date: NaiveDate::from_ymd_opt(2026, 11, 14).unwrap(),
            matchup: "A vs B".into(),
            espn_event_id: Some("401".into()),
            cbbd_game_id: Some(1),
            ours_final: true,
            cbbd_final: true,
            score_match: Some(score_match),
            our_score: Some("100-41".into()),
            cbbd_score: Some(if score_match { "100-41" } else { "0-0" }.into()),
            natstat_box: true,
            cbbd_box: true,
            team_diffs: team.iter().map(|s| s.to_string()).collect(),
            player_diffs: players.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let report = ParityReport {
            games: vec![
                game(false, &[], &[]),
                game(
                    true,
                    &["home.assists 15/17", "away.assists 9/8"],
                    &["Jo Doe pts 12/14"],
                ),
            ],
            ..Default::default()
        };
        let s = report.summary();
        assert!(s.contains("A vs B: 100-41 / 0-0 (ESPN 401)"), "{s}");
        assert!(
            s.contains("team sides differing, by field: assists 2"),
            "{s}"
        );
        assert!(
            s.contains("(ESPN 401): home.assists 15/17, away.assists 9/8"),
            "{s}"
        );
        assert!(s.contains("(ESPN 401): Jo Doe pts 12/14"), "{s}");
    }

    #[test]
    fn ledger_notes_and_slack_line_stay_bounded() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let game = |i: usize| GameParity {
            game_date: d,
            matchup: format!("Team {i} vs Other {i}"),
            espn_event_id: Some(i.to_string()),
            cbbd_game_id: Some(i as i32),
            ours_final: true,
            cbbd_final: true,
            score_match: Some(true),
            natstat_box: true,
            cbbd_box: true,
            team_diffs: (0..15).map(|f| format!("home.field{f} 1/2")).collect(),
            player_diffs: (0..20).map(|p| format!("Player {p} pts 1/2")).collect(),
            ..Default::default()
        };
        let mut games: Vec<GameParity> = (0..40).map(game).collect();
        // One CBBD-filled game, which is counted but never compared.
        games.push(GameParity {
            game_date: d,
            cbbd_game_id: Some(99),
            cbbd_final: true,
            cbbd_box: true,
            filled_by_cbbd: true,
            ..Default::default()
        });
        let report = ParityReport {
            from: Some(d),
            to: Some(d),
            games,
            cbbd_unmatched: vec!["2026-11-14 X vs Y".into()],
        };
        let notes = report.ledger_notes();
        assert!(notes.chars().count() <= NOTE_MAX_CHARS, "{}", notes.len());
        assert!(notes.starts_with(
            "2026-11-14..2026-11-14: 41 games, 41 matched to CBBD, 40 compared, 0 identical, \
             40 team-box diff, 40 player diff, 0/40 score disagreements, 1 CBBD-only"
        ));
        assert!(notes.contains("1 CBBD games unmatched"), "{notes}");
        assert_eq!(
            report.totals().slack_summary(),
            "0/40 identical · 40 team-box diffs · 40 player diffs · 0/40 score disagreements \
             · 1 CBBD-only · 1 filled by CBBD"
        );
    }

    #[test]
    fn sides_follow_the_score_before_espn() {
        // Iowa 69, Creighton 70 at a neutral site: CBBD lists Creighton home,
        // ESPN does not flip.
        assert!(cbbd_swapped(
            (Some(69), Some(70)),
            (Some(70), Some(69)),
            false
        ));
        // ESPN flips, CBBD does not (Cleveland State vs West Florida).
        assert!(!cbbd_swapped(
            (Some(69), Some(67)),
            (Some(69), Some(67)),
            true
        ));
        // CBBD's 0-0 "final": no score to decide by, so ESPN's flag stands.
        assert!(cbbd_swapped(
            (Some(100), Some(41)),
            (Some(0), Some(0)),
            true
        ));
        assert!(!cbbd_swapped((Some(100), Some(41)), (None, None), false));
    }
}
