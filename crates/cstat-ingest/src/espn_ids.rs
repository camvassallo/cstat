//! `map-espn-ids`: write ESPN's ids onto our `games`, `players` and `teams`
//! rows (migration 076), so CollegeBasketballData rows, which carry ESPN's
//! ids, join to ours without name matching every night
//! (`docs/natstat_migration_plan.md` §3).
//!
//! Reads local tables only, plus one cached CBBD `/teams` call for the teams:
//!
//! - **Games**: `game_broadcasts.source_event_id` is already the ESPN event id
//!   of each game the broadcasts matcher placed.
//! - **Players**: within each game side, our `player_game_stats` rows pair to
//!   the CBBD shadow box (`cbbd_player_game_stats`) exactly as `source-parity`
//!   pairs them ([`crate::name_match::pair_box_names`]), with the sides
//!   oriented by the score ([`crate::source_parity::cbbd_swapped`]). Each
//!   paired row is a vote for the CBBD line's `espn_athlete_id`.
//! - **Teams**: each game side with at least one paired player is a vote for
//!   that side's CBBD team id, which CBBD's `/teams` turns into ESPN's.
//!
//! **Never guess, never overwrite.** An entity takes the id more than half
//! its votes name; a split vote is a conflict. An id two entities would take
//! in one season is a conflict for both. An entity that already has an id
//! keeps it, and a different id found for it is reported, not written; so is
//! an id another entity already holds. Re-running a window is a no-op. A
//! wrong id is cleared by hand (`UPDATE … SET espn_id = NULL`) and re-mapped.

use crate::cbbd::{CbbdClient, TeamInfo};
use crate::name_match::pair_box_names;
use crate::source_parity::cbbd_swapped;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use uuid::Uuid;

/// How long the `/teams` response is cached. ESPN's team ids do not change,
/// and a team CBBD adds mid-season is only unmapped until the cache turns
/// over, so this keeps the step at about one call a month.
const TEAMS_CACHE_TTL_SECS: i64 = 30 * 24 * 3600;

/// One entity's evidence for its ESPN id.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: Uuid,
    pub season: i32,
    /// For the report.
    pub label: String,
    /// ESPN id → how many rows named it.
    pub votes: BTreeMap<String, u32>,
    /// The id already stored on the row, if any.
    pub stored: Option<String>,
}

/// What [`resolve`] decided for one entity kind.
#[derive(Debug, Default)]
pub struct Resolution {
    /// `(entity id, espn id)` to write.
    pub writes: Vec<(Uuid, String)>,
    /// Entities whose stored id is the one found again.
    pub already: usize,
    /// Entities written or confirmed on a majority while some rows disagreed.
    pub majority: usize,
    /// One line per entity skipped, saying why.
    pub conflicts: Vec<String>,
}

/// Decide each candidate's id. `held` is every id already stored for the
/// seasons involved, `(season, espn id) → entity`.
pub fn resolve(candidates: &[Candidate], held: &HashMap<(i32, String), Uuid>) -> Resolution {
    let mut out = Resolution::default();

    // 1. Each entity's own consensus: a strict majority of its votes.
    let mut winners: Vec<(&Candidate, String, bool)> = Vec::new();
    for c in candidates {
        let total: u32 = c.votes.values().sum();
        let Some((top_id, &top)) = c.votes.iter().max_by_key(|(_, n)| **n) else {
            continue;
        };
        if top * 2 > total {
            winners.push((c, top_id.clone(), c.votes.len() > 1));
        } else {
            out.conflicts.push(format!(
                "{} ({}): rows split {}",
                c.label,
                c.season,
                fmt_votes(&c.votes)
            ));
        }
    }

    // 2. One id, one entity, per season.
    let mut claims: HashMap<(i32, &str), Vec<&str>> = HashMap::new();
    for (c, id, _) in &winners {
        claims
            .entry((c.season, id.as_str()))
            .or_default()
            .push(c.label.as_str());
    }

    // 3. Against what is stored.
    for (c, id, dissent) in &winners {
        let claimants = &claims[&(c.season, id.as_str())];
        if claimants.len() > 1 {
            out.conflicts.push(format!(
                "{} ({}): ESPN id {id} also found for {}",
                c.label,
                c.season,
                claimants
                    .iter()
                    .filter(|l| **l != c.label)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }
        match c.stored.as_deref() {
            Some(s) if s == id => {
                out.already += 1;
                out.majority += usize::from(*dissent);
                continue;
            }
            Some(s) => {
                out.conflicts.push(format!(
                    "{} ({}): stored ESPN id {s}, rows say {id}",
                    c.label, c.season
                ));
                continue;
            }
            None => {}
        }
        if let Some(holder) = held.get(&(c.season, id.clone()))
            && *holder != c.id
        {
            out.conflicts.push(format!(
                "{} ({}): ESPN id {id} is already stored on another row ({holder})",
                c.label, c.season
            ));
            continue;
        }
        out.majority += usize::from(*dissent);
        out.writes.push((c.id, id.clone()));
    }
    out.conflicts.sort();
    out
}

fn fmt_votes(votes: &BTreeMap<String, u32>) -> String {
    votes
        .iter()
        .map(|(id, n)| format!("{id}x{n}"))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Counts for one entity kind.
#[derive(Debug, Default)]
pub struct EntityTally {
    /// Entities with at least one vote in the window.
    pub candidates: usize,
    pub written: usize,
    pub already: usize,
    pub majority: usize,
    pub conflicts: Vec<String>,
}

impl EntityTally {
    fn from(candidates: usize, r: Resolution, written: usize) -> Self {
        Self {
            candidates,
            written,
            already: r.already,
            majority: r.majority,
            conflicts: r.conflicts,
        }
    }
}

#[derive(Debug, Default)]
pub struct MapReport {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub games_in_window: usize,
    /// Games `game_broadcasts` has no ESPN event for.
    pub games_no_event: usize,
    pub games: EntityTally,
    /// Games whose ESPN event is in the CBBD shadow tables.
    pub games_in_cbbd: usize,
    /// Our player rows on D1 sides of those games, and how many paired.
    pub player_rows: usize,
    pub player_rows_paired: usize,
    /// Paired CBBD lines that carry no ESPN athlete id.
    pub paired_without_espn_id: usize,
    /// Players with rows on those sides of which none paired.
    pub players_unpaired: usize,
    pub players: EntityTally,
    pub teams: EntityTally,
    /// Why teams were not mapped, when they were not.
    pub teams_skipped: Option<String>,
    /// Game sides whose CBBD team id `/teams` does not list with an ESPN id.
    pub team_sides_unknown: usize,
    pub cbbd_calls_remaining: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct GameRow {
    id: Uuid,
    season: i32,
    label: String,
    home_team_id: Option<Uuid>,
    away_team_id: Option<Uuid>,
    home_score: Option<i32>,
    away_score: Option<i32>,
    espn_id: Option<String>,
    event_id: Option<String>,
    flip: bool,
    cbbd_game_id: Option<i32>,
    cbbd_home_points: Option<i32>,
    cbbd_away_points: Option<i32>,
    cbbd_home_team_id: Option<i32>,
    cbbd_away_team_id: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct OurPlayer {
    game_id: Uuid,
    team_id: Uuid,
    player_id: Uuid,
    name: String,
    season: i32,
    espn_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CbbdPlayer {
    cbbd_game_id: i32,
    is_home: bool,
    name: String,
    espn_athlete_id: Option<String>,
}

/// Map the games played on Eastern dates `from..=to`. `cbbd` is used only for
/// the one cached `/teams` call; without it teams are skipped.
pub async fn run(
    pool: &PgPool,
    cbbd: Option<&CbbdClient>,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<MapReport> {
    let mut report = MapReport {
        from: Some(from),
        to: Some(to),
        ..Default::default()
    };

    // A game whose ESPN event matches two CBBD games is left out of the
    // player and team passes: which box is its own cannot be told.
    let rows: Vec<GameRow> = sqlx::query_as(
        "SELECT g.id, g.season, \
                g.game_date::text || ' ' || coalesce(ht.name, '(non-D1)') || ' vs ' \
                  || coalesce(at.name, '(non-D1)') AS label, \
                g.home_team_id, g.away_team_id, g.home_score, g.away_score, g.espn_id, \
                b.source_event_id AS event_id, \
                COALESCE(b.source_home_is_away, false) AS flip, \
                c.cbbd_game_id, c.home_points AS cbbd_home_points, \
                c.away_points AS cbbd_away_points, \
                c.home_team_id AS cbbd_home_team_id, c.away_team_id AS cbbd_away_team_id \
         FROM games g \
         LEFT JOIN teams ht ON ht.id = g.home_team_id \
         LEFT JOIN teams at ON at.id = g.away_team_id \
         LEFT JOIN game_broadcasts b ON b.game_id = g.id \
         LEFT JOIN cbbd_games c ON c.espn_event_id = b.source_event_id \
           AND (SELECT count(*) FROM cbbd_games c2 \
                WHERE c2.espn_event_id = b.source_event_id) = 1 \
         WHERE g.game_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("loading the window's games")?;
    report.games_in_window = rows.len();
    let seasons: Vec<i32> = rows
        .iter()
        .map(|r| r.season)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // --- games ---
    let game_candidates: Vec<Candidate> = rows
        .iter()
        .filter_map(|r| {
            let event = r.event_id.clone()?;
            Some(Candidate {
                id: r.id,
                season: r.season,
                label: r.label.clone(),
                votes: BTreeMap::from([(event, 1)]),
                stored: r.espn_id.clone(),
            })
        })
        .collect();
    report.games_no_event = rows.len() - game_candidates.len();
    let held = held_ids(pool, "games", &seasons).await?;
    let res = resolve(&game_candidates, &held);
    let written = write_ids(pool, "games", &res.writes).await?;
    report.games = EntityTally::from(game_candidates.len(), res, written);

    // --- players (and the team votes their paired sides cast) ---
    let matched: Vec<&GameRow> = rows.iter().filter(|r| r.cbbd_game_id.is_some()).collect();
    report.games_in_cbbd = matched.len();
    let game_ids: Vec<Uuid> = matched.iter().map(|r| r.id).collect();
    let cbbd_ids: Vec<i32> = matched.iter().filter_map(|r| r.cbbd_game_id).collect();

    let ours: Vec<OurPlayer> = sqlx::query_as(
        "SELECT s.game_id, s.team_id, s.player_id, p.name, p.season, p.espn_id \
         FROM player_game_stats s JOIN players p ON p.id = s.player_id \
         WHERE s.game_id = ANY($1) AND s.team_id IS NOT NULL",
    )
    .bind(&game_ids)
    .fetch_all(pool)
    .await
    .context("loading our box rows")?;
    let mut ours_by_side: HashMap<(Uuid, Uuid), Vec<&OurPlayer>> = HashMap::new();
    for p in &ours {
        ours_by_side
            .entry((p.game_id, p.team_id))
            .or_default()
            .push(p);
    }
    let theirs: Vec<CbbdPlayer> = sqlx::query_as(
        "SELECT cbbd_game_id, is_home, name, espn_athlete_id \
         FROM cbbd_player_game_stats WHERE cbbd_game_id = ANY($1)",
    )
    .bind(&cbbd_ids)
    .fetch_all(pool)
    .await
    .context("loading the CBBD shadow box rows")?;
    let mut theirs_by_side: HashMap<(i32, bool), Vec<&CbbdPlayer>> = HashMap::new();
    for p in &theirs {
        theirs_by_side
            .entry((p.cbbd_game_id, p.is_home))
            .or_default()
            .push(p);
    }

    let mut player_votes: HashMap<Uuid, Candidate> = HashMap::new();
    let mut player_seen: HashSet<Uuid> = HashSet::new();
    let mut player_paired: HashSet<Uuid> = HashSet::new();
    // Our team id → (season, label) and CBBD team id → votes.
    let mut team_votes: HashMap<Uuid, BTreeMap<i32, u32>> = HashMap::new();
    for r in &matched {
        let Some(cbbd_game) = r.cbbd_game_id else {
            continue;
        };
        for (team_id, (cbbd_is_home, cbbd_team)) in sides(r) {
            // A non-D1 side: we keep no rows for it.
            let Some(team_id) = team_id else {
                continue;
            };
            let ours = ours_by_side
                .get(&(r.id, team_id))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let theirs = theirs_by_side
                .get(&(cbbd_game, cbbd_is_home))
                .map(Vec::as_slice)
                .unwrap_or_default();
            report.player_rows += ours.len();
            player_seen.extend(ours.iter().map(|p| p.player_id));
            let names_o: Vec<&str> = ours.iter().map(|p| p.name.as_str()).collect();
            let names_t: Vec<&str> = theirs.iter().map(|p| p.name.as_str()).collect();
            let pairs = pair_box_names(&names_o, &names_t);
            report.player_rows_paired += pairs.len();
            for &(oi, ti) in &pairs {
                let (o, t) = (ours[oi], theirs[ti]);
                player_paired.insert(o.player_id);
                let Some(espn) = t.espn_athlete_id.as_deref().filter(|s| !s.is_empty()) else {
                    report.paired_without_espn_id += 1;
                    continue;
                };
                *player_votes
                    .entry(o.player_id)
                    .or_insert_with(|| Candidate {
                        id: o.player_id,
                        season: o.season,
                        label: o.name.clone(),
                        votes: BTreeMap::new(),
                        stored: o.espn_id.clone(),
                    })
                    .votes
                    .entry(espn.to_string())
                    .or_default() += 1;
            }
            // A side that paired nobody is not evidence of whose side it is.
            if let (false, Some(cbbd_team)) = (pairs.is_empty(), cbbd_team) {
                *team_votes
                    .entry(team_id)
                    .or_default()
                    .entry(cbbd_team)
                    .or_default() += 1;
            }
        }
    }
    report.players_unpaired = player_seen.difference(&player_paired).count();
    let mut player_candidates: Vec<Candidate> = player_votes.into_values().collect();
    player_candidates.sort_by_key(|c| c.id);
    let player_seasons: Vec<i32> = player_candidates
        .iter()
        .map(|c| c.season)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let held = held_ids(pool, "players", &player_seasons).await?;
    let res = resolve(&player_candidates, &held);
    let written = write_ids(pool, "players", &res.writes).await?;
    report.players = EntityTally::from(player_candidates.len(), res, written);

    // --- teams ---
    if team_votes.is_empty() {
        report.teams_skipped = Some("no paired game sides in the window".into());
        return Ok(report);
    }
    let Some(cbbd) = cbbd else {
        report.teams_skipped = Some("CBBD_API_KEY unset (teams need one /teams call)".into());
        return Ok(report);
    };
    let espn_team: HashMap<i32, String> = match cbbd
        .get::<Vec<TeamInfo>>("/teams", &[], Some(TEAMS_CACHE_TTL_SECS))
        .await
    {
        Ok(teams) => teams
            .into_iter()
            .filter_map(|t| Some((t.id, t.source_id?)))
            .collect(),
        Err(e) => {
            report.teams_skipped = Some(format!("CBBD /teams failed: {e}"));
            report.cbbd_calls_remaining = cbbd.calls_remaining();
            return Ok(report);
        }
    };
    report.cbbd_calls_remaining = cbbd.calls_remaining();

    let team_ids: Vec<Uuid> = team_votes.keys().copied().collect();
    let team_meta: Vec<(Uuid, i32, String, Option<String>)> =
        sqlx::query_as("SELECT id, season, name, espn_id FROM teams WHERE id = ANY($1)")
            .bind(&team_ids)
            .fetch_all(pool)
            .await
            .context("loading teams")?;
    let mut team_candidates = Vec::new();
    for (id, season, name, stored) in team_meta {
        let mut votes = BTreeMap::new();
        for (cbbd_team, n) in &team_votes[&id] {
            match espn_team.get(cbbd_team) {
                Some(espn) => *votes.entry(espn.clone()).or_default() += n,
                None => report.team_sides_unknown += *n as usize,
            }
        }
        if !votes.is_empty() {
            team_candidates.push(Candidate {
                id,
                season,
                label: name,
                votes,
                stored,
            });
        }
    }
    team_candidates.sort_by_key(|c| c.id);
    let team_seasons: Vec<i32> = team_candidates
        .iter()
        .map(|c| c.season)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let held = held_ids(pool, "teams", &team_seasons).await?;
    let res = resolve(&team_candidates, &held);
    let written = write_ids(pool, "teams", &res.writes).await?;
    report.teams = EntityTally::from(team_candidates.len(), res, written);
    Ok(report)
}

/// `(our team id, (CBBD is_home, CBBD team id))` for one side of a game.
type Side = (Option<Uuid>, (bool, Option<i32>));

/// Our two sides of a game, each with the CBBD side it is: `(our team id,
/// (CBBD is_home, CBBD team id))`, home first. Oriented by the score, with
/// ESPN's flip only as the fallback, because CBBD's home and away do not
/// always follow ESPN's.
fn sides(r: &GameRow) -> [Side; 2] {
    let swapped = cbbd_swapped(
        (r.home_score, r.away_score),
        (r.cbbd_home_points, r.cbbd_away_points),
        r.flip,
    );
    if swapped {
        [
            (r.home_team_id, (false, r.cbbd_away_team_id)),
            (r.away_team_id, (true, r.cbbd_home_team_id)),
        ]
    } else {
        [
            (r.home_team_id, (true, r.cbbd_home_team_id)),
            (r.away_team_id, (false, r.cbbd_away_team_id)),
        ]
    }
}

/// Every ESPN id already stored on `table` for `seasons`.
async fn held_ids(
    pool: &PgPool,
    table: &str,
    seasons: &[i32],
) -> Result<HashMap<(i32, String), Uuid>> {
    let sql = format!(
        "SELECT season, espn_id, id FROM {table} \
         WHERE espn_id IS NOT NULL AND season = ANY($1)"
    );
    let rows: Vec<(i32, String, Uuid)> =
        sqlx::query_as(&sql)
            .bind(seasons)
            .fetch_all(pool)
            .await
            .with_context(|| format!("loading stored ESPN ids on {table}"))?;
    Ok(rows.into_iter().map(|(s, e, id)| ((s, e), id)).collect())
}

/// One batched UPDATE. `espn_id IS NULL` keeps it from overwriting a value
/// written since [`resolve`] read it; returns the rows actually written.
async fn write_ids(pool: &PgPool, table: &str, writes: &[(Uuid, String)]) -> Result<usize> {
    if writes.is_empty() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = writes.iter().map(|(id, _)| *id).collect();
    let espn: Vec<&str> = writes.iter().map(|(_, e)| e.as_str()).collect();
    let sql = format!(
        "UPDATE {table} t SET espn_id = u.espn_id \
         FROM UNNEST($1::uuid[], $2::text[]) AS u(id, espn_id) \
         WHERE t.id = u.id AND t.espn_id IS NULL"
    );
    let done = sqlx::query(&sql)
        .bind(&ids)
        .bind(&espn)
        .execute(pool)
        .await
        .with_context(|| format!("writing ESPN ids to {table}"))?;
    Ok(done.rows_affected() as usize)
}

impl MapReport {
    /// One line for the nightly ledger's notes.
    pub fn notes(&self) -> String {
        let t = |e: &EntityTally| {
            format!(
                "{} new, {} already, {} conflicts",
                e.written,
                e.already,
                e.conflicts.len()
            )
        };
        let mut s = format!(
            "games: {}; players: {}; teams: {}",
            t(&self.games),
            t(&self.players),
            t(&self.teams)
        );
        if let Some(why) = &self.teams_skipped {
            let _ = write!(s, " (teams skipped: {why})");
        }
        s
    }

    /// Rows written over all three kinds.
    pub fn written(&self) -> usize {
        self.games.written + self.players.written + self.teams.written
    }

    /// The report `map-espn-ids` prints.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let window = match (self.from, self.to) {
            (Some(f), Some(t)) => format!("{f}..{t}"),
            _ => String::new(),
        };
        let _ = writeln!(s, "map espn ids {window}");
        let entity = |s: &mut String, name: &str, e: &EntityTally| {
            let _ = writeln!(
                s,
                "  {name}: {} with evidence -> {} written, {} already mapped, \
                 {} conflicts ({} decided by majority over dissenting rows)",
                e.candidates,
                e.written,
                e.already,
                e.conflicts.len(),
                e.majority
            );
        };
        entity(&mut s, "games", &self.games);
        let _ = writeln!(
            s,
            "    {} games in the window, {} with no ESPN event in game_broadcasts",
            self.games_in_window, self.games_no_event
        );
        entity(&mut s, "players", &self.players);
        let _ = writeln!(
            s,
            "    {} games in the CBBD shadow tables; {} of {} box rows paired; \
             {} players with no paired row; {} paired lines without an ESPN id",
            self.games_in_cbbd,
            self.player_rows_paired,
            self.player_rows,
            self.players_unpaired,
            self.paired_without_espn_id
        );
        match &self.teams_skipped {
            Some(why) => {
                let _ = writeln!(s, "  teams: skipped ({why})");
            }
            None => {
                entity(&mut s, "teams", &self.teams);
                let _ = writeln!(
                    s,
                    "    {} paired game sides whose CBBD team has no ESPN id",
                    self.team_sides_unknown
                );
            }
        }
        if let Some(n) = self.cbbd_calls_remaining {
            let _ = writeln!(s, "  {n} CBBD calls left this month");
        }
        for (name, e) in [
            ("games", &self.games),
            ("players", &self.players),
            ("teams", &self.teams),
        ] {
            if !e.conflicts.is_empty() {
                let _ = writeln!(s, "\n{name} conflicts (not written):");
                for c in &e.conflicts {
                    let _ = writeln!(s, "  {c}");
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(n: u128, season: i32, votes: &[(&str, u32)], stored: Option<&str>) -> Candidate {
        Candidate {
            id: Uuid::from_u128(n),
            season,
            label: format!("p{n}"),
            votes: votes.iter().map(|(e, c)| (e.to_string(), *c)).collect(),
            stored: stored.map(str::to_string),
        }
    }

    fn game(ours: (i32, i32), cbbd: (i32, i32), flip: bool) -> GameRow {
        GameRow {
            id: Uuid::from_u128(1),
            season: 2027,
            label: String::new(),
            home_team_id: Some(Uuid::from_u128(10)),
            away_team_id: Some(Uuid::from_u128(20)),
            home_score: Some(ours.0),
            away_score: Some(ours.1),
            espn_id: None,
            event_id: Some("401".into()),
            flip,
            cbbd_game_id: Some(7),
            cbbd_home_points: Some(cbbd.0),
            cbbd_away_points: Some(cbbd.1),
            cbbd_home_team_id: Some(100),
            cbbd_away_team_id: Some(200),
        }
    }

    #[test]
    fn sides_follow_the_score_over_espns_flip() {
        let home = Some(Uuid::from_u128(10));
        let away = Some(Uuid::from_u128(20));
        // Same score, ESPN flipped: CBBD's sides are ours.
        assert_eq!(
            sides(&game((69, 67), (69, 67), true)),
            [(home, (true, Some(100))), (away, (false, Some(200)))]
        );
        // Reversed score, no ESPN flip: our home is CBBD's away team.
        assert_eq!(
            sides(&game((69, 70), (70, 69), false)),
            [(home, (false, Some(200))), (away, (true, Some(100)))]
        );
    }

    #[test]
    fn unanimous_and_majority_votes_write() {
        let r = resolve(
            &[
                cand(1, 2027, &[("10", 3)], None),
                cand(2, 2027, &[("20", 2), ("21", 1)], None),
            ],
            &HashMap::new(),
        );
        assert_eq!(
            r.writes,
            vec![
                (Uuid::from_u128(1), "10".to_string()),
                (Uuid::from_u128(2), "20".to_string())
            ]
        );
        assert_eq!(r.majority, 1);
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn a_split_vote_is_a_conflict() {
        // 1-1, and a plurality short of a majority (2 of 5), both refused.
        let r = resolve(
            &[
                cand(1, 2027, &[("10", 1), ("11", 1)], None),
                cand(2, 2027, &[("20", 2), ("21", 2), ("22", 1)], None),
            ],
            &HashMap::new(),
        );
        assert!(r.writes.is_empty());
        assert_eq!(r.conflicts.len(), 2);
    }

    #[test]
    fn one_id_on_two_entities_in_a_season_skips_both() {
        let r = resolve(
            &[
                cand(1, 2027, &[("10", 2)], None),
                cand(2, 2027, &[("10", 1)], None),
                // The same id in another season is a different row's to take.
                cand(3, 2026, &[("10", 1)], None),
            ],
            &HashMap::new(),
        );
        assert_eq!(r.writes, vec![(Uuid::from_u128(3), "10".to_string())]);
        assert_eq!(r.conflicts.len(), 2);
    }

    #[test]
    fn stored_ids_are_kept_and_never_overwritten() {
        let held = HashMap::from([
            ((2027, "10".to_string()), Uuid::from_u128(1)),
            ((2027, "30".to_string()), Uuid::from_u128(9)),
        ]);
        let r = resolve(
            &[
                // Found again: already mapped, nothing written.
                cand(1, 2027, &[("10", 1)], Some("10")),
                // A different id from the stored one: reported, kept.
                cand(2, 2027, &[("20", 1)], Some("21")),
                // An id another row (not in this window) already holds.
                cand(3, 2027, &[("30", 4)], None),
            ],
            &held,
        );
        assert!(r.writes.is_empty());
        assert_eq!(r.already, 1);
        assert_eq!(r.conflicts.len(), 2);
    }
}
