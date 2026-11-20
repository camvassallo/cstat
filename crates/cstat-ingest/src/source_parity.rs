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
//! unique last name, with [`crate::name_match::pair_unambiguous`]; an
//! ambiguous name is left unpaired rather than guessed.

use crate::name_match::pair_unambiguous;
use chrono::{DateTime, NaiveDate, Utc};
use cstat_core::roster_projection::normalize_player_name;
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
    pub ours_final: bool,
    pub cbbd_final: bool,
    /// `None` unless both sides have a score.
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
                g.status, b.source_event_id AS espn_event_id, \
                COALESCE(b.source_home_is_away, false) AS flip, \
                c.cbbd_game_id, c.status AS cbbd_status, \
                c.home_points AS cbbd_home_points, c.away_points AS cbbd_away_points, \
                c.first_box_at AS cbbd_first_box_at, \
                (SELECT min(t.created_at) AT TIME ZONE 'UTC' FROM team_game_stats t \
                  WHERE t.game_id = g.id) AS natstat_box_at, \
                (SELECT count(*) FROM play_by_play p WHERE p.game_id = g.id) AS our_plays, \
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
            ours_final: r
                .status
                .as_deref()
                .is_some_and(|s| s.to_ascii_lowercase().starts_with("final")),
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
        if let (Some(h), Some(a), Some(ch), Some(ca)) =
            (r.home_score, r.away_score, c_home_pts, c_away_pts)
        {
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
            for (oi, ti) in pair_players(&ours, &theirs) {
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
fn cbbd_swapped(
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

/// Pair two box scores' players: exact normalized name first, then a last name
/// unique on both sides among those left. Returns (ours, theirs) indexes.
fn pair_players(ours: &[PlayerRow], theirs: &[PlayerRow]) -> Vec<(usize, usize)> {
    let norm_o: Vec<String> = ours
        .iter()
        .map(|p| normalize_player_name(&p.name))
        .collect();
    let norm_t: Vec<String> = theirs
        .iter()
        .map(|p| normalize_player_name(&p.name))
        .collect();
    let mut pairs = pair_unambiguous(&norm_o, &norm_t, |_, _| true);

    let taken_o: std::collections::HashSet<usize> = pairs.iter().map(|(o, _)| *o).collect();
    let taken_t: std::collections::HashSet<usize> = pairs.iter().map(|(_, t)| *t).collect();
    let left_o: Vec<usize> = (0..ours.len()).filter(|i| !taken_o.contains(i)).collect();
    let left_t: Vec<usize> = (0..theirs.len()).filter(|i| !taken_t.contains(i)).collect();
    let last = |s: &String| s.split_whitespace().last().unwrap_or("").to_string();
    let last_o: Vec<String> = left_o.iter().map(|&i| last(&norm_o[i])).collect();
    let last_t: Vec<String> = left_t.iter().map(|&i| last(&norm_t[i])).collect();
    pairs.extend(
        pair_unambiguous(&last_o, &last_t, |a, _| !last_o[a].is_empty())
            .into_iter()
            .map(|(a, b)| (left_o[a], left_t[b])),
    );
    pairs.sort_unstable();
    pairs
}

async fn our_team_boxes(
    pool: &PgPool,
    game_ids: &[Uuid],
) -> Result<HashMap<(Uuid, Uuid), TeamBox>, sqlx::Error> {
    let sql = format!(
        "SELECT game_id, team_id, {} FROM team_game_stats WHERE game_id = ANY($1)",
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
         WHERE s.game_id = ANY($1)",
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
            "  score disagreements: {} of {}",
            scored
                .iter()
                .filter(|p| p.score_match == Some(false))
                .count(),
            scored.len()
        );

        let finals: Vec<&GameParity> = matched.iter().copied().filter(|p| p.cbbd_final).collect();
        let _ = writeln!(s, "\nbox scores (games CBBD lists final: {})", finals.len());
        let _ = writeln!(
            s,
            "  both: {}; CBBD only: {}; NatStat only: {}; neither: {}",
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
            "  games with plays: ours {}, CBBD {}",
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
            let _ = writeln!(s, "  median plays per game: ours {o:.0}, CBBD {c:.0}");
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
                let _ = writeln!(s, "  {} {}", p.game_date, p.matchup);
            }
        }
        s
    }

    /// One CSV row per game, with a header.
    pub fn csv(&self) -> String {
        let mut s = String::from(
            "game_date,matchup,game_id,espn_event_id,cbbd_game_id,ours_final,cbbd_final,\
             sides_swapped,our_score,cbbd_score,score_match,natstat_box,cbbd_box,cbbd_box_lag_hours,team_diffs,team_missing,\
             our_players,cbbd_players,players_paired,minutes_abs_diff,starter_mismatches,\
             points_mismatches,rebound_mismatches,our_plays,cbbd_plays,cbbd_shots,\
             cbbd_shots_with_location,cbbd_plays_full_on_floor,player_diffs\n",
        );
        let opt = |v: Option<String>| v.unwrap_or_default();
        for p in &self.games {
            let _ = writeln!(
                s,
                "{},\"{}\",{},{},{},{},{},{},{},{},{},{},{},{},\"{}\",\"{}\",{},{},{},{:.0},{},{},{},{},{},{},{},{},\"{}\"",
                p.game_date,
                p.matchup.replace('"', "'"),
                p.game_id,
                opt(p.espn_event_id.clone()),
                opt(p.cbbd_game_id.map(|v| v.to_string())),
                p.ours_final,
                p.cbbd_final,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> PlayerRow {
        PlayerRow {
            name: name.to_string(),
            starter: None,
            minutes: None,
            points: None,
            rebounds: None,
        }
    }

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

    #[test]
    fn pairs_by_name_then_unique_last_name() {
        // NatStat drops the suffix ("Abdi Bashir"); CBBD keeps it. Two Smiths
        // on one side make "smith" ambiguous, so neither pairs by last name.
        let ours = [
            row("Abdi Bashir"),
            row("DK Thorn"),
            row("Al Smith"),
            row("Bo Smith"),
        ];
        let theirs = [
            row("Abdi Bashir Jr."),
            row("Dekedran Thorn"),
            row("Alan Smith"),
            row("Bob Smith"),
        ];
        assert_eq!(pair_players(&ours, &theirs), vec![(0, 0), (1, 1)]);
    }
}
