//! SUB-replay: reconstruct 5-man on-floor lineups and contiguous stints from
//! play-by-play `SUB` events (P2b). Source-agnostic — works off the normalized
//! `play_by_play` rows whether they came from the CSV (which has no on-floor
//! columns) or the API (which does, and serves as the validation oracle).
//!
//! Model: a live 5-man set per team, seeded from the box-score starters and
//! mutated by `SUB` rows ("X sub in" / "X sub out"). Every NON-sub play is
//! attributed to whatever the live lineups are at that moment; consecutive
//! plays sharing both lineups coalesce into one [`Stint`]. Substitutions
//! cluster at dead balls (an out paired with an in, no plays between), so a
//! team's set is back to five before the next attributable play — the transient
//! 4-man state during a cluster is never assigned to a stint.
//!
//! Score deltas chain across stint boundaries from the authoritative running
//! score (`score_home`/`score_vis`), which NatStat does not double-count (unlike
//! tag-summed points). A stint's delta is its end score minus the previous
//! stint's end score; only sub rows sit between stints, so this captures exactly
//! the scoring that happened while that lineup was on.
//!
//! The engine is deliberately pure (no DB, no I/O) so the replay logic is unit
//! tested deterministically; the DB-backed runner and the oracle-accuracy
//! measurement live alongside it.

use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

/// One play as the replay consumes it, in `seq` order within a single game.
#[derive(Debug, Clone)]
pub struct ReplayPlay {
    pub seq: i32,
    pub period: i32,
    /// Acting team for a `SUB` row (whose set the sub mutates). Ignored for
    /// non-sub plays.
    pub team_id: Option<Uuid>,
    /// The substituted player (sub rows) — resolved upstream from `player_id`
    /// or the name in the description. `None` = unresolved (counted as an
    /// anomaly; the set is left unchanged).
    pub player_id: Option<Uuid>,
    pub is_sub: bool,
    /// For a sub row: `true` = entering, `false` = leaving.
    pub sub_in: bool,
    pub score_home: Option<i32>,
    pub score_vis: Option<i32>,
}

/// A contiguous window in which both teams' on-floor fives were constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stint {
    pub period: i32,
    pub start_seq: i32,
    pub end_seq: i32,
    /// Sorted player ids on the floor for the home / visiting team. Usually 5;
    /// fewer or more signals an unresolved/missed sub (see `plays_off_five`).
    pub home_lineup: Vec<Uuid>,
    pub vis_lineup: Vec<Uuid>,
    pub home_score_delta: i32,
    pub vis_score_delta: i32,
}

/// Result of replaying one game.
#[derive(Debug, Clone, Default)]
pub struct ReplayResult {
    pub stints: Vec<Stint>,
    /// Sub rows that could not be applied (missing resolved player or team).
    pub unresolved_subs: u32,
    /// Non-sub plays attributed while a team's set was not exactly five — the
    /// honest quality signal for how clean the replay was.
    pub plays_off_five: u32,
}

/// In-progress stint accumulator.
struct Builder {
    period: i32,
    start_seq: i32,
    end_seq: i32,
    home: Vec<Uuid>,
    vis: Vec<Uuid>,
    end_home_score: i32,
    end_vis_score: i32,
}

/// Replay one game's plays into stints. `plays` must be in ascending `seq`
/// order. `home_team`/`vis_team` identify which set a sub row mutates; starters
/// seed the opening fives.
pub fn replay_game(
    home_team: Uuid,
    vis_team: Uuid,
    home_starters: &[Uuid],
    vis_starters: &[Uuid],
    plays: &[ReplayPlay],
) -> ReplayResult {
    let mut home: BTreeSet<Uuid> = home_starters.iter().copied().collect();
    let mut vis: BTreeSet<Uuid> = vis_starters.iter().copied().collect();

    let mut result = ReplayResult::default();
    let mut cur: Option<Builder> = None;
    // Running score, carried forward across rows that omit it.
    let (mut last_home, mut last_vis) = (0i32, 0i32);
    // Score at the end of the previously-closed stint, for delta chaining.
    let (mut prev_home, mut prev_vis) = (0i32, 0i32);

    for p in plays {
        // Carry the running score forward with `max`: it only ever increases, so
        // a spurious low/zero value (some event rows — "media timeout", "End of
        // period" — report score 0 instead of the running total) must not reset
        // it, or stint deltas blow up hugely negative/positive.
        if let Some(s) = p.score_home {
            last_home = last_home.max(s);
        }
        if let Some(s) = p.score_vis {
            last_vis = last_vis.max(s);
        }

        if p.is_sub {
            match (p.team_id, p.player_id) {
                (Some(team), Some(player)) if team == home_team => {
                    apply_sub(&mut home, player, p.sub_in);
                }
                (Some(team), Some(player)) if team == vis_team => {
                    apply_sub(&mut vis, player, p.sub_in);
                }
                _ => result.unresolved_subs += 1,
            }
            continue; // subs are boundaries, not attributable plays
        }

        let home_vec: Vec<Uuid> = home.iter().copied().collect();
        let vis_vec: Vec<Uuid> = vis.iter().copied().collect();

        match cur.as_mut() {
            Some(b) if b.home == home_vec && b.vis == vis_vec => {
                b.end_seq = p.seq;
                b.end_home_score = last_home;
                b.end_vis_score = last_vis;
            }
            _ => {
                if let Some(b) = cur.take() {
                    result.stints.push(finish(b, &mut prev_home, &mut prev_vis));
                }
                cur = Some(Builder {
                    period: p.period,
                    start_seq: p.seq,
                    end_seq: p.seq,
                    home: home_vec,
                    vis: vis_vec,
                    end_home_score: last_home,
                    end_vis_score: last_vis,
                });
            }
        }

        if home.len() != 5 || vis.len() != 5 {
            result.plays_off_five += 1;
        }
    }

    if let Some(b) = cur.take() {
        result.stints.push(finish(b, &mut prev_home, &mut prev_vis));
    }
    result
}

fn apply_sub(set: &mut BTreeSet<Uuid>, player: Uuid, sub_in: bool) {
    if sub_in {
        set.insert(player);
    } else {
        set.remove(&player);
    }
}

/// Close a builder into a stint, charging it the scoring since the previous
/// stint and advancing the running boundary.
fn finish(b: Builder, prev_home: &mut i32, prev_vis: &mut i32) -> Stint {
    let home_score_delta = b.end_home_score - *prev_home;
    let vis_score_delta = b.end_vis_score - *prev_vis;
    *prev_home = b.end_home_score;
    *prev_vis = b.end_vis_score;
    Stint {
        period: b.period,
        start_seq: b.start_seq,
        end_seq: b.end_seq,
        home_lineup: b.home,
        vis_lineup: b.vis,
        home_score_delta,
        vis_score_delta,
    }
}

// ---------------------------------------------------------------------------
// Pure per-game stint building (shared by the per-game and bulk DB paths)
// ---------------------------------------------------------------------------

/// A raw `play_by_play` row, the pure input to stint building. Carries both the
/// SUB fields (for the replay path) and the stored on-floor strings (for the
/// exact path).
#[derive(Debug, Clone)]
pub struct RawPlay {
    pub seq: i32,
    pub period: i32,
    pub team_id: Option<Uuid>,
    pub player_id: Option<Uuid>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub score_home: Option<i32>,
    pub score_vis: Option<i32>,
    pub onfloor_home: Option<String>,
    pub onfloor_vis: Option<String>,
    /// Game clock as the feed emits it, `MM:SS.ss`, counting down within a
    /// period. `None` on the ~0.4% of rows that omit it. Used for stint duration.
    pub clock: Option<String>,
}

/// Resolve a raw play into a [`ReplayPlay`], filling in the SUB direction and
/// recovering a missing sub `player_id` from the description name via
/// `name_map`. Pure.
fn to_replay_play(r: &RawPlay, name_map: &HashMap<(Uuid, String), Uuid>) -> ReplayPlay {
    let is_sub = r.tags.iter().any(|t| t == "SUB");
    let mut sub_in = false;
    let mut resolved = r.player_id;
    if is_sub && let Some((name, is_in)) = r.description.as_deref().and_then(parse_sub) {
        sub_in = is_in;
        if resolved.is_none() {
            resolved = r
                .team_id
                .and_then(|t| name_map.get(&(t, name.to_lowercase())).copied());
        }
    }
    ReplayPlay {
        seq: r.seq,
        period: r.period,
        team_id: r.team_id,
        player_id: if is_sub { resolved } else { r.player_id },
        is_sub,
        sub_in,
        score_home: r.score_home,
        score_vis: r.score_vis,
    }
}

/// Build one game's stints, hybrid: exact API on-floor lineups when present,
/// SUB-replay otherwise. Pure — all inputs are pre-loaded. This is the entry
/// point the bulk derivation calls per game.
pub fn game_stints(
    home_team: Uuid,
    vis_team: Uuid,
    home_starters: &[Uuid],
    vis_starters: &[Uuid],
    name_map: &HashMap<(Uuid, String), Uuid>,
    code_to_uuid: &HashMap<String, Uuid>,
    raw: &[RawPlay],
) -> (Vec<Stint>, StintSource) {
    if raw.iter().any(|p| p.onfloor_home.is_some()) {
        (
            stints_from_onfloor_rows(raw, code_to_uuid),
            StintSource::OnFloor,
        )
    } else {
        let plays: Vec<ReplayPlay> = raw.iter().map(|r| to_replay_play(r, name_map)).collect();
        let result = replay_game(home_team, vis_team, home_starters, vis_starters, &plays);
        (result.stints, StintSource::Replay)
    }
}

/// Coalesce stints from stored on-floor strings (pure). Resolves the
/// comma-separated NatStat codes to our UUIDs via `code_to_uuid`.
fn stints_from_onfloor_rows(raw: &[RawPlay], code_to_uuid: &HashMap<String, Uuid>) -> Vec<Stint> {
    let resolve = |s: &Option<String>| -> Vec<Uuid> {
        let mut v: Vec<Uuid> = s
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|c| !c.is_empty())
            .filter_map(|c| code_to_uuid.get(c).copied())
            .collect();
        v.sort();
        v.dedup();
        v
    };

    let mut stints = Vec::new();
    let mut cur: Option<Builder> = None;
    let (mut last_home, mut last_vis) = (0i32, 0i32);
    let (mut prev_home, mut prev_vis) = (0i32, 0i32);

    for r in raw {
        if let Some(s) = r.score_home {
            last_home = last_home.max(s);
        }
        if let Some(s) = r.score_vis {
            last_vis = last_vis.max(s);
        }
        let home = resolve(&r.onfloor_home);
        let vis = resolve(&r.onfloor_vis);
        if home.is_empty() || vis.is_empty() {
            continue;
        }
        match cur.as_mut() {
            Some(b) if b.home == home && b.vis == vis => {
                b.end_seq = r.seq;
                b.end_home_score = last_home;
                b.end_vis_score = last_vis;
            }
            _ => {
                if let Some(b) = cur.take() {
                    stints.push(finish(b, &mut prev_home, &mut prev_vis));
                }
                cur = Some(Builder {
                    period: r.period,
                    start_seq: r.seq,
                    end_seq: r.seq,
                    home,
                    vis,
                    end_home_score: last_home,
                    end_vis_score: last_vis,
                });
            }
        }
    }
    if let Some(b) = cur.take() {
        stints.push(finish(b, &mut prev_home, &mut prev_vis));
    }
    stints
}

// ---------------------------------------------------------------------------
// Possession & duration metrics (P3)
// ---------------------------------------------------------------------------

/// Per-stint possessions (per side) and on-floor duration. Aligned 1:1 with the
/// `Stint` slice [`stint_metrics`] is called with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StintMetrics {
    pub home_possessions: f64,
    pub vis_possessions: f64,
    pub seconds: i32,
}

/// Possession-component tally for one side over a stint. Possessions use
/// cstat's canonical estimate `(FGA + 3FA) - ORB + TOV + 0.44*FTA` — the exact
/// convention from `compute_adjusted_efficiency` (the 0.44 FTA coefficient, not
/// 0.475), so lineup ortg/drtg land on the same scale as team AdjO/AdjD.
///
/// Two tag-vocabulary quirks the counting must absorb (both verified against
/// box-score parity across 2015-2026):
///  - the `FGA` tag is **2-point attempts only** (mutually exclusive with
///    `3FA`), so three-point attempts must be added back in to recover total
///    FGA;
///  - the turnover tag changed vintage — `TOV` in the 2015-2018 feeds, `TO`
///    from ~2019 on — so both must count (omitting the old `TOV` undercounts
///    those seasons' possessions by a full ~13 turnovers/team-game).
#[derive(Default)]
struct PossCount {
    fga: f64,
    orb: f64,
    to: f64,
    fta: f64,
}

impl PossCount {
    fn add(&mut self, tags: &[String]) {
        let has = |t: &str| tags.iter().any(|x| x == t);
        if has("FGA") || has("3FA") {
            self.fga += 1.0;
        }
        if has("ORB") {
            self.orb += 1.0;
        }
        if has("TO") || has("TOV") {
            self.to += 1.0;
        }
        if has("FTA") {
            self.fta += 1.0;
        }
    }
    fn possessions(&self) -> f64 {
        self.fga - self.orb + self.to + 0.44 * self.fta
    }
}

/// Parse a `MM:SS.ss` (or `MM:SS`) game clock into whole seconds remaining,
/// dropping the hundredths. `None` for malformed/absent clocks.
pub fn parse_clock(clock: &str) -> Option<i32> {
    let (m, rest) = clock.split_once(':')?;
    let s = rest.split('.').next()?; // drop the ".ss" hundredths
    Some(m.trim().parse::<i32>().ok()? * 60 + s.trim().parse::<i32>().ok()?)
}

/// Per-stint possessions (each side) and on-floor seconds, in one merge-walk
/// over the game's seq-ordered plays against the contiguous, non-overlapping
/// stints. A play is attributed to the stint whose `[start_seq, end_seq]`
/// contains its `seq`; plays that fall in the gaps between stints (the SUB
/// boundaries) belong to neither. Duration is summed from positive clock
/// decrements within a stint, so a clock reset across a period boundary inside
/// one stint just drops that single break interval rather than going negative.
pub fn stint_metrics(
    raw: &[RawPlay],
    stints: &[Stint],
    home_team: Uuid,
    vis_team: Uuid,
) -> Vec<StintMetrics> {
    let mut out = vec![StintMetrics::default(); stints.len()];
    if stints.is_empty() {
        return out;
    }
    let mut si = 0usize;
    let mut home = PossCount::default();
    let mut vis = PossCount::default();
    let mut seconds = 0i32;
    let mut last_clock: Option<i32> = None;

    for r in raw {
        // Leave every stint this play has passed, flushing its tally.
        while si < stints.len() && r.seq > stints[si].end_seq {
            out[si] = StintMetrics {
                home_possessions: home.possessions(),
                vis_possessions: vis.possessions(),
                seconds,
            };
            home = PossCount::default();
            vis = PossCount::default();
            seconds = 0;
            last_clock = None;
            si += 1;
        }
        if si >= stints.len() {
            break;
        }
        if r.seq < stints[si].start_seq {
            continue; // a SUB boundary between stints — attributed to neither
        }
        if r.team_id == Some(home_team) {
            home.add(&r.tags);
        } else if r.team_id == Some(vis_team) {
            vis.add(&r.tags);
        }
        if let Some(cur) = r.clock.as_deref().and_then(parse_clock) {
            if let Some(prev) = last_clock {
                let d = prev - cur;
                if d > 0 {
                    seconds += d;
                }
            }
            last_clock = Some(cur);
        }
    }
    // Flush the final still-open stint.
    if si < stints.len() {
        out[si] = StintMetrics {
            home_possessions: home.possessions(),
            vis_possessions: vis.possessions(),
            seconds,
        };
    }
    out
}

// ---------------------------------------------------------------------------
// DB-backed runner
// ---------------------------------------------------------------------------

/// One `play_by_play` row as loaded for replay:
/// (seq, period, team_id, player_id, description, tags, score_home, score_vis).
type PbpRowTuple = (
    i32,
    i32,
    Option<Uuid>,
    Option<Uuid>,
    Option<String>,
    Vec<String>,
    Option<i32>,
    Option<i32>,
);

/// Everything the replay needs for one game, loaded from the DB.
#[derive(Debug, Clone)]
pub struct GameReplayInputs {
    pub home_team: Uuid,
    pub vis_team: Uuid,
    pub home_starters: Vec<Uuid>,
    pub vis_starters: Vec<Uuid>,
    pub plays: Vec<ReplayPlay>,
    /// Sub rows that arrived with a NULL `player_id` and were recovered by
    /// matching the description name against the game roster — a quality signal.
    pub subs_resolved_by_name: u32,
    /// Sub rows still unresolved after the name fallback.
    pub subs_unresolved: u32,
}

/// Strip the trailing " sub in" / " sub out" from a SUB description to get the
/// player name, and report whether it was a sub-in.
fn parse_sub(description: &str) -> Option<(&str, bool)> {
    if let Some(name) = description.strip_suffix(" sub in") {
        Some((name.trim(), true))
    } else {
        description
            .strip_suffix(" sub out")
            .map(|name| (name.trim(), false))
    }
}

/// Load a game's replay inputs from the DB: home/vis teams, box-score starters,
/// and the ordered plays with subs resolved. SUB rows that lack a `player_id`
/// (≈4% on tracked teams) are recovered by matching the description name to a
/// rostered player on the acting team.
pub async fn load_game_inputs(
    pool: &sqlx::PgPool,
    game_id: Uuid,
) -> Result<GameReplayInputs, sqlx::Error> {
    // A game can have a NULL home/away team (an unresolved non-D1 opponent).
    // Coalesce to nil so that side simply never matches a sub's team_id (its
    // subs fall through to `unresolved_subs`); the tracked team still replays,
    // and lineup aggregates only ever use tracked teams anyway.
    let (home_team, vis_team): (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT home_team_id, away_team_id FROM games WHERE id = $1")
            .bind(game_id)
            .fetch_one(pool)
            .await?;
    let home_team = home_team.unwrap_or_else(Uuid::nil);
    let vis_team = vis_team.unwrap_or_else(Uuid::nil);

    // Box-score starters per team.
    let starter_rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT team_id, player_id FROM player_game_stats WHERE game_id = $1 AND starter IS TRUE",
    )
    .bind(game_id)
    .fetch_all(pool)
    .await?;
    let home_starters: Vec<Uuid> = starter_rows
        .iter()
        .filter(|(t, _)| *t == home_team)
        .map(|(_, p)| *p)
        .collect();
    let vis_starters: Vec<Uuid> = starter_rows
        .iter()
        .filter(|(t, _)| *t == vis_team)
        .map(|(_, p)| *p)
        .collect();

    // (team_id, lowercased name) -> player_id, for the null-player sub fallback.
    let roster: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT pgs.team_id, lower(pl.name), pgs.player_id
         FROM player_game_stats pgs JOIN players pl ON pl.id = pgs.player_id
         WHERE pgs.game_id = $1",
    )
    .bind(game_id)
    .fetch_all(pool)
    .await?;
    let mut name_map: HashMap<(Uuid, String), Uuid> = HashMap::new();
    for (team, name, pid) in roster {
        name_map.entry((team, name)).or_insert(pid);
    }

    let rows: Vec<PbpRowTuple> = sqlx::query_as(
        "SELECT seq, period, team_id, player_id, description, tags, score_home, score_vis
             FROM play_by_play WHERE game_id = $1 ORDER BY seq",
    )
    .bind(game_id)
    .fetch_all(pool)
    .await?;

    let mut plays = Vec::with_capacity(rows.len());
    let mut subs_resolved_by_name = 0u32;
    let mut subs_unresolved = 0u32;
    for (seq, period, team_id, player_id, description, tags, score_home, score_vis) in rows {
        let is_sub = tags.iter().any(|t| t == "SUB");
        let mut sub_in = false;
        let mut resolved_player = player_id;
        if is_sub && let Some((name, is_in)) = description.as_deref().and_then(parse_sub) {
            sub_in = is_in;
            if resolved_player.is_none() {
                // Name fallback against the acting team's roster.
                resolved_player =
                    team_id.and_then(|team| name_map.get(&(team, name.to_lowercase())).copied());
                match resolved_player {
                    Some(_) => subs_resolved_by_name += 1,
                    None => subs_unresolved += 1,
                }
            }
        }
        plays.push(ReplayPlay {
            seq,
            period,
            team_id,
            player_id: if is_sub { resolved_player } else { player_id },
            is_sub,
            sub_in,
            score_home,
            score_vis,
        });
    }

    Ok(GameReplayInputs {
        home_team,
        vis_team,
        home_starters,
        vis_starters,
        plays,
        subs_resolved_by_name,
        subs_unresolved,
    })
}

/// Convenience: load a game and replay it.
pub async fn replay_game_from_db(
    pool: &sqlx::PgPool,
    game_id: Uuid,
) -> Result<(GameReplayInputs, ReplayResult), sqlx::Error> {
    let inputs = load_game_inputs(pool, game_id).await?;
    let result = replay_game(
        inputs.home_team,
        inputs.vis_team,
        &inputs.home_starters,
        &inputs.vis_starters,
        &inputs.plays,
    );
    Ok((inputs, result))
}

/// How a game's stints were sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StintSource {
    /// Exact, from the API's per-play `onfloorhome`/`onfloorvis`.
    OnFloor,
    /// Approximate (~86%), from SUB-replay off the CSV.
    Replay,
}

impl StintSource {
    pub fn as_str(self) -> &'static str {
        match self {
            StintSource::OnFloor => "onfloor",
            StintSource::Replay => "replay",
        }
    }
}

// The hybrid per-game entry point is the pure `game_stints` above; the bulk
// derivation (`compute::compute_pbp_lineups`) feeds it pre-loaded maps and a
// single streamed plays query rather than querying per game.

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn parse_sub_extracts_name_and_direction() {
        assert_eq!(parse_sub("Aday Mara sub in"), Some(("Aday Mara", true)));
        assert_eq!(
            parse_sub("Kareem Rozier sub out"),
            Some(("Kareem Rozier", false))
        );
        assert_eq!(parse_sub("Made layup"), None);
        // Trailing "in"/"out" that isn't a sub must not match.
        assert_eq!(parse_sub("Tip in"), None);
    }

    const HOME: Uuid = Uuid::from_u128(1000);
    const VIS: Uuid = Uuid::from_u128(2000);

    // Home starters 1..5, vis starters 11..15.
    fn starters() -> (Vec<Uuid>, Vec<Uuid>) {
        ((1..=5).map(id).collect(), (11..=15).map(id).collect())
    }

    fn play(seq: i32, score_home: i32, score_vis: i32) -> ReplayPlay {
        ReplayPlay {
            seq,
            period: 1,
            team_id: None,
            player_id: None,
            is_sub: false,
            sub_in: false,
            score_home: Some(score_home),
            score_vis: Some(score_vis),
        }
    }

    fn sub(seq: i32, team: Uuid, player: u128, sub_in: bool) -> ReplayPlay {
        ReplayPlay {
            seq,
            period: 1,
            team_id: Some(team),
            player_id: Some(id(player)),
            is_sub: true,
            sub_in,
            score_home: None,
            score_vis: None,
        }
    }

    #[test]
    fn parse_clock_drops_hundredths() {
        assert_eq!(parse_clock("15:54.54"), Some(15 * 60 + 54));
        assert_eq!(parse_clock("0:03.03"), Some(3));
        assert_eq!(parse_clock("11:40"), Some(11 * 60 + 40));
        assert_eq!(parse_clock(""), None);
        assert_eq!(parse_clock("garbage"), None);
    }

    // Minimal RawPlay for the metrics walk: only seq, team, tags, clock matter.
    fn rp(seq: i32, team: Option<Uuid>, tags: &[&str], clock: &str) -> RawPlay {
        RawPlay {
            seq,
            period: 1,
            team_id: team,
            player_id: None,
            description: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            score_home: None,
            score_vis: None,
            onfloor_home: None,
            onfloor_vis: None,
            clock: Some(clock.to_string()),
        }
    }

    #[test]
    fn stint_metrics_counts_possessions_and_duration_per_side() {
        // One stint spanning seq 1..=6. Home (1000) takes a 2pt FGA, a 3pt
        // attempt (3FA, no FGA tag), and a turnover; grabs one ORB. Vis (2000)
        // takes two FTA. A NULL-team marker row contributes to neither.
        let stints = vec![Stint {
            period: 1,
            start_seq: 1,
            end_seq: 6,
            home_lineup: (1..=5).map(id).collect(),
            vis_lineup: (11..=15).map(id).collect(),
            home_score_delta: 0,
            vis_score_delta: 0,
        }];
        let raw = vec![
            rp(1, Some(HOME), &["FGA", "paint"], "10:00"),
            rp(2, Some(HOME), &["3FA"], "9:40"),
            rp(3, Some(HOME), &["ORB"], "9:30"),
            rp(4, Some(HOME), &["TO"], "9:00"),
            rp(5, None, &["TIMEOUT"], "9:00"),
            rp(6, Some(VIS), &["FTA", "FTA"], "8:30"),
        ];
        let m = stint_metrics(&raw, &stints, HOME, VIS);
        assert_eq!(m.len(), 1);
        // Home: fga(2pt)=1, 3fa=1 => fga total 2; orb=1; to=1; fta=0
        //   => 2 - 1 + 1 + 0.44*0 = 2.0
        assert!((m[0].home_possessions - 2.0).abs() < 1e-9);
        // Vis: fga=0, orb=0, to=0, fta=2 => 0.44*2 = 0.88 (one FTA tag per row,
        // and only one vis row, so fta=1) -> 0.44
        assert!((m[0].vis_possessions - 0.44).abs() < 1e-9);
        // Duration: 10:00 -> 8:30 = 90s of positive decrements.
        assert_eq!(m[0].seconds, 90);
    }

    #[test]
    fn stint_metrics_counts_legacy_tov_turnover_tag() {
        // The turnover tag is `TOV` in the 2015-2018 feeds and `TO` from ~2019
        // on; both must count as a possession-ending turnover, or the older
        // seasons undercount possessions by a full ~13 turnovers/team-game.
        let stints = vec![Stint {
            period: 1,
            start_seq: 1,
            end_seq: 2,
            home_lineup: (1..=5).map(id).collect(),
            vis_lineup: (11..=15).map(id).collect(),
            home_score_delta: 0,
            vis_score_delta: 0,
        }];
        let raw = vec![
            rp(1, Some(HOME), &["TOV"], "10:00"), // legacy tag
            rp(2, Some(HOME), &["TO"], "9:40"),   // modern tag
        ];
        let m = stint_metrics(&raw, &stints, HOME, VIS);
        // Two turnovers, no FGA/ORB/FTA => possessions = 0 - 0 + 2 + 0 = 2.0.
        assert!((m[0].home_possessions - 2.0).abs() < 1e-9);
    }

    #[test]
    fn stint_metrics_attributes_plays_to_the_containing_stint() {
        // Two stints; a SUB-gap play at seq 3 (between them) is attributed to
        // neither. Stint A = seq 1..=2, stint B = seq 4..=5.
        let mk = |start, end| Stint {
            period: 1,
            start_seq: start,
            end_seq: end,
            home_lineup: (1..=5).map(id).collect(),
            vis_lineup: (11..=15).map(id).collect(),
            home_score_delta: 0,
            vis_score_delta: 0,
        };
        let stints = vec![mk(1, 2), mk(4, 5)];
        let raw = vec![
            rp(1, Some(HOME), &["FGA"], "10:00"),
            rp(2, Some(HOME), &["FGA"], "9:50"),
            rp(3, Some(HOME), &["SUB"], "9:50"), // gap — belongs to neither stint
            rp(4, Some(HOME), &["FGA"], "9:40"),
            rp(5, Some(HOME), &["TO"], "9:20"),
        ];
        let m = stint_metrics(&raw, &stints, HOME, VIS);
        assert!((m[0].home_possessions - 2.0).abs() < 1e-9); // two FGA
        assert!((m[1].home_possessions - 2.0).abs() < 1e-9); // FGA + TO
    }

    #[test]
    fn no_subs_is_one_stint_with_starters() {
        let (h, v) = starters();
        let plays = vec![play(1, 2, 0), play(2, 2, 3), play(3, 5, 3)];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert_eq!(r.stints.len(), 1);
        let s = &r.stints[0];
        assert_eq!(s.home_lineup, h);
        assert_eq!(s.vis_lineup, v);
        assert_eq!(s.home_score_delta, 5);
        assert_eq!(s.vis_score_delta, 3);
        assert_eq!(r.plays_off_five, 0);
        assert_eq!(r.unresolved_subs, 0);
    }

    #[test]
    fn substitution_opens_new_stint_with_correct_deltas() {
        let (h, v) = starters();
        // Player 5 out, player 6 in (a clean dead-ball cluster) after seq 2.
        let plays = vec![
            play(1, 2, 0),
            play(2, 4, 0),
            sub(3, HOME, 5, false),
            sub(4, HOME, 6, true),
            play(5, 4, 2),
            play(6, 7, 2),
        ];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert_eq!(r.stints.len(), 2);
        // Stint 1: starters, scored 4-0.
        assert_eq!(r.stints[0].home_lineup, h);
        assert_eq!(r.stints[0].home_score_delta, 4);
        assert_eq!(r.stints[0].vis_score_delta, 0);
        // Stint 2: 6 replaces 5; scored 3-2 (7-2 minus 4-0).
        let mut want: Vec<Uuid> = vec![id(1), id(2), id(3), id(4), id(6)];
        want.sort();
        assert_eq!(r.stints[1].home_lineup, want);
        assert_eq!(r.stints[1].home_score_delta, 3);
        assert_eq!(r.stints[1].vis_score_delta, 2);
        assert_eq!(r.plays_off_five, 0);
    }

    #[test]
    fn spurious_score_zero_does_not_reset_running_score() {
        let (h, v) = starters();
        // A "media timeout"-style row reports 0/0 mid-game between two real
        // scores; the running score must not dip, so deltas stay non-negative.
        let plays = vec![
            play(1, 10, 8),
            play(2, 0, 0), // spurious reset row
            play(3, 12, 8),
        ];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert_eq!(r.stints.len(), 1);
        assert_eq!(r.stints[0].home_score_delta, 12); // not 10-0+12
        assert_eq!(r.stints[0].vis_score_delta, 8);
    }

    #[test]
    fn period_is_carried_onto_stints() {
        let (h, v) = starters();
        let mut p1 = play(1, 1, 1);
        p1.period = 2; // OT
        let r = replay_game(HOME, VIS, &h, &v, &[p1]);
        assert_eq!(r.stints.len(), 1);
        assert_eq!(r.stints[0].period, 2);
    }

    #[test]
    fn unresolved_sub_is_counted_and_skipped() {
        let (h, v) = starters();
        let mut bad = sub(2, HOME, 0, false);
        bad.player_id = None; // unresolved
        let plays = vec![play(1, 0, 0), bad, play(3, 2, 0)];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert_eq!(r.unresolved_subs, 1);
        // Set left intact (still 5), so the lineup is unchanged across the gap.
        assert_eq!(r.stints.len(), 1);
        assert_eq!(r.stints[0].home_lineup, h);
        assert_eq!(r.plays_off_five, 0);
    }

    #[test]
    fn unbalanced_sub_flags_plays_off_five() {
        let (h, v) = starters();
        // A sub-out with no matching sub-in leaves home at four.
        let plays = vec![play(1, 0, 0), sub(2, HOME, 5, false), play(3, 2, 0)];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert!(!r.stints.is_empty());
        assert_eq!(r.plays_off_five, 1); // the seq-3 play ran 4-on-5
        assert_eq!(r.stints.last().unwrap().home_lineup.len(), 4);
    }

    #[test]
    fn subs_on_both_teams_track_independently() {
        let (h, v) = starters();
        let plays = vec![
            play(1, 0, 0),
            sub(2, VIS, 11, false),
            sub(3, VIS, 16, true),
            play(4, 0, 3),
        ];
        let r = replay_game(HOME, VIS, &h, &v, &plays);
        assert_eq!(r.stints.len(), 2);
        // Home unchanged; vis swapped 11 -> 16.
        assert_eq!(r.stints[1].home_lineup, h);
        let mut want_vis: Vec<Uuid> = vec![id(12), id(13), id(14), id(15), id(16)];
        want_vis.sort();
        assert_eq!(r.stints[1].vis_lineup, want_vis);
        assert_eq!(r.plays_off_five, 0);
    }
}
