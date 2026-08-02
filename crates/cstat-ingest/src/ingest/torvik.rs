//! Barttorvik data ingestion: player season stats and per-game rebound backfill.

use crate::torvik::{TorkvikClient, TorkvikGameRow};
use chrono::NaiveDate;
use cstat_core::team_name_match::team_match_score;
use sqlx::{PgPool, QueryBuilder};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};
use uuid::Uuid;

/// Ingest Torvik player season stats, matching to existing cstat players.
pub async fn ingest_torvik_player_stats(
    client: &TorkvikClient,
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<(u64, u64)> {
    let players = client.fetch_player_stats(season).await?;
    // Resolve every Torvik row to a cstat player up front (see `link_players`)
    // rather than row-by-row: the nickname-tolerant fallbacks need to know
    // which cstat players the exact-name pass already claimed before they can
    // pair off what's left.
    let roster = SeasonRoster::load(pool, season).await?;
    let links = link_players(&roster, &players, season);
    let mut upserted: u64 = 0;
    let matched = links.stats.matched();

    for (p, &player_id) in players.iter().zip(links.player_ids.iter()) {
        let pid = match p.pid {
            Some(id) => id,
            None => continue,
        };

        // Also backfill class_year and height on the player record if we matched
        // and NatStat didn't provide them.
        if let Some(pid_uuid) = player_id
            && (p.class_year.is_some() || p.height.is_some())
        {
            let height_inches = p.height.as_deref().and_then(parse_height);
            sqlx::query(
                r#"UPDATE players
                   SET class_year = COALESCE(players.class_year, $2),
                       height_inches = COALESCE(players.height_inches, $3),
                       updated_at = now()
                   WHERE id = $1 AND (players.class_year IS NULL OR players.height_inches IS NULL)"#,
            )
            .bind(pid_uuid)
            .bind(&p.class_year)
            .bind(height_inches)
            .execute(pool)
            .await?;
        }

        sqlx::query(
            r#"INSERT INTO torvik_player_stats (
                    player_id, torvik_pid, season, team_name, conf,
                    class_year, height, jersey_number, player_type, recruiting_rank,
                    games_played, minutes_per_game, total_minutes,
                    o_rtg, d_rtg, adj_oe, adj_de, usage_rate,
                    bpm, obpm, dbpm, gbpm, ogbpm, dgbpm,
                    porpag, dporpag, stops,
                    effective_fg_pct, true_shooting_pct, ft_pct, ft_rate,
                    two_p_pct, tp_pct, rim_pct, mid_pct, dunk_pct,
                    ftm, fta, two_pm, two_pa, tpm, tpa,
                    rim_made, rim_attempted, mid_made, mid_attempted,
                    dunks_made, dunks_attempted,
                    orb_pct, drb_pct, ast_pct, tov_pct, stl_pct, blk_pct,
                    personal_foul_rate, ast_to_tov,
                    ppg, oreb_pg, dreb_pg, treb_pg, ast_pg, stl_pg, blk_pg,
                    nba_pick, min_per, player_name
               ) VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8, $9, $10,
                    $11, $12, $13,
                    $14, $15, $16, $17, $18,
                    $19, $20, $21, $22, $23, $24,
                    $25, $26, $27,
                    $28, $29, $30, $31,
                    $32, $33, $34, $35, $36,
                    $37, $38, $39, $40, $41, $42,
                    $43, $44, $45, $46,
                    $47, $48,
                    $49, $50, $51, $52, $53, $54,
                    $55, $56,
                    $57, $58, $59, $60, $61, $62, $63,
                    $64, $65, $66
               ) ON CONFLICT (torvik_pid, season) DO UPDATE SET
                    player_id = COALESCE(EXCLUDED.player_id, torvik_player_stats.player_id),
                    team_name = EXCLUDED.team_name, conf = EXCLUDED.conf,
                    class_year = EXCLUDED.class_year, height = EXCLUDED.height,
                    jersey_number = EXCLUDED.jersey_number, player_type = EXCLUDED.player_type,
                    recruiting_rank = EXCLUDED.recruiting_rank,
                    games_played = EXCLUDED.games_played,
                    minutes_per_game = EXCLUDED.minutes_per_game,
                    total_minutes = EXCLUDED.total_minutes,
                    o_rtg = EXCLUDED.o_rtg, d_rtg = EXCLUDED.d_rtg,
                    adj_oe = EXCLUDED.adj_oe, adj_de = EXCLUDED.adj_de,
                    usage_rate = EXCLUDED.usage_rate,
                    bpm = EXCLUDED.bpm, obpm = EXCLUDED.obpm, dbpm = EXCLUDED.dbpm,
                    gbpm = EXCLUDED.gbpm, ogbpm = EXCLUDED.ogbpm, dgbpm = EXCLUDED.dgbpm,
                    porpag = EXCLUDED.porpag, dporpag = EXCLUDED.dporpag, stops = EXCLUDED.stops,
                    effective_fg_pct = EXCLUDED.effective_fg_pct,
                    true_shooting_pct = EXCLUDED.true_shooting_pct,
                    ft_pct = EXCLUDED.ft_pct, ft_rate = EXCLUDED.ft_rate,
                    two_p_pct = EXCLUDED.two_p_pct, tp_pct = EXCLUDED.tp_pct,
                    rim_pct = EXCLUDED.rim_pct, mid_pct = EXCLUDED.mid_pct,
                    dunk_pct = EXCLUDED.dunk_pct,
                    ftm = EXCLUDED.ftm, fta = EXCLUDED.fta,
                    two_pm = EXCLUDED.two_pm, two_pa = EXCLUDED.two_pa,
                    tpm = EXCLUDED.tpm, tpa = EXCLUDED.tpa,
                    rim_made = EXCLUDED.rim_made, rim_attempted = EXCLUDED.rim_attempted,
                    mid_made = EXCLUDED.mid_made, mid_attempted = EXCLUDED.mid_attempted,
                    dunks_made = EXCLUDED.dunks_made, dunks_attempted = EXCLUDED.dunks_attempted,
                    orb_pct = EXCLUDED.orb_pct, drb_pct = EXCLUDED.drb_pct,
                    ast_pct = EXCLUDED.ast_pct, tov_pct = EXCLUDED.tov_pct,
                    stl_pct = EXCLUDED.stl_pct, blk_pct = EXCLUDED.blk_pct,
                    personal_foul_rate = EXCLUDED.personal_foul_rate,
                    ast_to_tov = EXCLUDED.ast_to_tov,
                    ppg = EXCLUDED.ppg,
                    oreb_pg = EXCLUDED.oreb_pg, dreb_pg = EXCLUDED.dreb_pg,
                    treb_pg = EXCLUDED.treb_pg, ast_pg = EXCLUDED.ast_pg,
                    stl_pg = EXCLUDED.stl_pg, blk_pg = EXCLUDED.blk_pg,
                    nba_pick = EXCLUDED.nba_pick,
                    min_per = EXCLUDED.min_per,
                    player_name = EXCLUDED.player_name,
                    updated_at = now()
            "#,
        )
        .bind(player_id) // $1
        .bind(pid) // $2
        .bind(season) // $3
        .bind(&p.team) // $4
        .bind(&p.conf) // $5
        .bind(&p.class_year) // $6
        .bind(&p.height) // $7
        .bind(&p.jersey_number) // $8
        .bind(&p.player_type) // $9
        .bind(p.recruiting_rank) // $10
        .bind(p.gp) // $11
        .bind(p.min_per) // $12
        .bind(p.total_minutes) // $13
        .bind(p.o_rtg) // $14
        .bind(p.d_rtg) // $15
        .bind(p.adj_oe) // $16
        .bind(p.adj_de) // $17
        .bind(p.usage) // $18
        .bind(p.bpm) // $19
        .bind(p.obpm) // $20
        .bind(p.dbpm) // $21
        .bind(p.gbpm) // $22
        .bind(p.ogbpm) // $23
        .bind(p.dgbpm) // $24
        .bind(p.porpag) // $25
        .bind(p.dporpag) // $26
        .bind(p.stops) // $27
        .bind(p.effective_fg_pct) // $28
        .bind(p.true_shooting_pct) // $29
        .bind(p.ft_pct) // $30
        .bind(p.ft_rate) // $31
        .bind(p.two_p_pct) // $32
        .bind(p.tp_pct) // $33
        .bind(p.rim_pct) // $34
        .bind(p.mid_pct) // $35
        .bind(p.dunk_pct) // $36
        .bind(p.ftm) // $37
        .bind(p.fta) // $38
        .bind(p.two_pm) // $39
        .bind(p.two_pa) // $40
        .bind(p.tpm) // $41
        .bind(p.tpa) // $42
        .bind(p.rim_made) // $43
        .bind(p.rim_attempted) // $44
        .bind(p.mid_made) // $45
        .bind(p.mid_attempted) // $46
        .bind(p.dunks_made) // $47
        .bind(p.dunks_attempted) // $48
        .bind(p.orb_pct) // $49
        .bind(p.drb_pct) // $50
        .bind(p.ast_pct) // $51
        .bind(p.tov_pct) // $52
        .bind(p.stl_pct) // $53
        .bind(p.blk_pct) // $54
        .bind(p.personal_foul_rate) // $55
        .bind(p.ast_to_tov) // $56
        .bind(p.ppg) // $57
        .bind(p.oreb_pg) // $58
        .bind(p.dreb_pg) // $59
        .bind(p.treb_pg) // $60
        .bind(p.ast_pg) // $61
        .bind(p.stl_pg) // $62
        .bind(p.blk_pg) // $63
        .bind(p.nba_pick) // $64
        .bind(p.min_per) // $65 — Torvik's Min% (share of team minutes 0–100)
        .bind(&p.player_name) // $66
        .execute(pool)
        .await?;

        upserted += 1;
    }

    let s = &links.stats;
    info!(
        season,
        upserted,
        matched,
        exact = s.exact,
        name_only = s.name_only,
        family_fallback = s.family_fallback,
        given_fallback = s.given_fallback,
        "Torvik player stats ingestion complete"
    );
    // Loud on purpose (issue #243): an unlinked row keeps its Torvik stats but
    // drops out of every query that reaches them through `players`, including
    // the player-SOS step that produces the served `cam_gbpm_v3_psos` — which
    // is how 1,305 rotation players, Obi Toppin and Ja Morant among them, went
    // missing from the leaderboard without a single error.
    if s.unlinked > 0 {
        warn!(
            season,
            unlinked = s.unlinked,
            unlinked_rotation = s.unlinked_rotation,
            unresolved_teams = s.unresolved_teams.len(),
            sample = ?s.unlinked_sample,
            "Torvik rows left unlinked to a cstat player"
        );
    }
    Ok((upserted, matched))
}

/// Persist all per-game Torvik rows into `torvik_player_game_stats`.
///
/// Uses the same gzip JSON as `backfill_rebounds_from_torvik` (one fetch
/// per season). Rows with missing `pid` or unparseable `date_str` are
/// skipped — both fields are NOT NULL in the schema.
pub async fn persist_torvik_game_stats(
    client: &TorkvikClient,
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<u64> {
    let games = client.fetch_game_stats(season).await?;
    apply_persist_torvik_game_stats(pool, &games, season).await
}

/// Same as `persist_torvik_game_stats`, but skips the network fetch and
/// operates on a pre-fetched `games` slice. Lets callers that need *both*
/// the rebound backfill and the per-game persistence share one fetch.
pub async fn apply_persist_torvik_game_stats(
    pool: &PgPool,
    games: &[TorkvikGameRow],
    season: i32,
) -> anyhow::Result<u64> {
    // Stage valid rows, dedup by the (pid, game_uid) conflict key — a batched
    // INSERT ... ON CONFLICT can't touch the same key twice, and we keep the
    // last occurrence to match the prior row-by-row upsert. This collapses a
    // ~113k-query N+1 (seconds on localhost, but ~10 min over the prod DB's
    // round-trip latency — see docs/in_season_ingest_plan.md) into ~115 batched
    // statements.
    let mut skipped: u64 = 0;
    let mut by_key: HashMap<(i32, &str), (i32, NaiveDate, &TorkvikGameRow)> =
        HashMap::with_capacity(games.len());
    for g in games {
        let Some(pid) = g.pid else {
            skipped += 1;
            continue;
        };
        let Ok(game_date) = NaiveDate::parse_from_str(&g.date_str, "%Y%m%d") else {
            skipped += 1;
            continue;
        };
        by_key.insert((pid, g.game_uid.as_str()), (pid, game_date, g));
    }
    let staged: Vec<(i32, NaiveDate, &TorkvikGameRow)> = by_key.into_values().collect();

    let mut inserted: u64 = 0;
    // 36 columns/row; chunk so each statement stays under Postgres' 65535-bind
    // cap (1000 * 36 = 36000).
    for chunk in staged.chunks(1000) {
        let mut qb = QueryBuilder::new(
            "INSERT INTO torvik_player_game_stats (\
             pid, game_uid, season, game_date, team, opponent, location, class_year, height_inches, \
             minutes_pct, o_rtg, usage, pts, oreb, dreb, ast, tov, stl, blk, pf, \
             two_pm, two_pa, tpm, tpa, ftm, fta, \
             rim_made, rim_attempted, mid_made, mid_attempted, dunks_made, dunks_attempted, \
             bpm, obpm, dbpm, possessions) ",
        );
        qb.push_values(chunk, |mut b, row| {
            let (pid, game_date, g) = (row.0, row.1, row.2);
            b.push_bind(pid)
                .push_bind(&g.game_uid)
                .push_bind(season)
                .push_bind(game_date)
                .push_bind(&g.team)
                .push_bind(&g.opponent)
                .push_bind(&g.location)
                .push_bind(&g.class_year)
                .push_bind(g.height_inches)
                .push_bind(g.minutes_pct)
                .push_bind(g.o_rtg)
                .push_bind(g.usage)
                .push_bind(g.pts)
                .push_bind(g.oreb)
                .push_bind(g.dreb)
                .push_bind(g.ast)
                .push_bind(g.tov)
                .push_bind(g.stl)
                .push_bind(g.blk)
                .push_bind(g.pf)
                .push_bind(g.two_pm)
                .push_bind(g.two_pa)
                .push_bind(g.tpm)
                .push_bind(g.tpa)
                .push_bind(g.ftm)
                .push_bind(g.fta)
                .push_bind(g.rim_made)
                .push_bind(g.rim_attempted)
                .push_bind(g.mid_made)
                .push_bind(g.mid_attempted)
                .push_bind(g.dunks_made)
                .push_bind(g.dunks_attempted)
                .push_bind(g.bpm)
                .push_bind(g.obpm)
                .push_bind(g.dbpm)
                .push_bind(g.possessions);
        });
        qb.push(
            " ON CONFLICT (pid, game_uid) DO UPDATE SET \
             season = EXCLUDED.season, game_date = EXCLUDED.game_date, team = EXCLUDED.team, \
             opponent = EXCLUDED.opponent, location = EXCLUDED.location, class_year = EXCLUDED.class_year, \
             height_inches = EXCLUDED.height_inches, minutes_pct = EXCLUDED.minutes_pct, \
             o_rtg = EXCLUDED.o_rtg, usage = EXCLUDED.usage, pts = EXCLUDED.pts, \
             oreb = EXCLUDED.oreb, dreb = EXCLUDED.dreb, ast = EXCLUDED.ast, tov = EXCLUDED.tov, \
             stl = EXCLUDED.stl, blk = EXCLUDED.blk, pf = EXCLUDED.pf, \
             two_pm = EXCLUDED.two_pm, two_pa = EXCLUDED.two_pa, tpm = EXCLUDED.tpm, tpa = EXCLUDED.tpa, \
             ftm = EXCLUDED.ftm, fta = EXCLUDED.fta, \
             rim_made = EXCLUDED.rim_made, rim_attempted = EXCLUDED.rim_attempted, \
             mid_made = EXCLUDED.mid_made, mid_attempted = EXCLUDED.mid_attempted, \
             dunks_made = EXCLUDED.dunks_made, dunks_attempted = EXCLUDED.dunks_attempted, \
             bpm = EXCLUDED.bpm, obpm = EXCLUDED.obpm, dbpm = EXCLUDED.dbpm, \
             possessions = EXCLUDED.possessions",
        );
        inserted += qb.build().execute(pool).await?.rows_affected();
    }

    info!(
        season,
        inserted, skipped, "Torvik per-game persistence complete"
    );
    Ok(inserted)
}

/// Backfill missing rebounds in player_game_stats from Torvik game-level data.
pub async fn backfill_rebounds_from_torvik(
    client: &TorkvikClient,
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<u64> {
    let games = client.fetch_game_stats(season).await?;
    apply_rebound_backfill(pool, &games, season).await
}

/// Same as `backfill_rebounds_from_torvik`, but skips the network fetch and
/// operates on a pre-fetched `games` slice. Lets callers that need *both*
/// the rebound backfill and the per-game persistence share one fetch.
pub async fn apply_rebound_backfill(
    pool: &PgPool,
    games: &[TorkvikGameRow],
    season: i32,
) -> anyhow::Result<u64> {
    // Pre-build a lookup: normalized_name → Vec<player_id> for this season.
    // This avoids running REGEXP_REPLACE in SQL for every one of 113k rows.
    let players =
        sqlx::query_as::<_, (Uuid, String)>("SELECT id, name FROM players WHERE season = $1")
            .bind(season)
            .fetch_all(pool)
            .await?;

    let mut name_map: std::collections::HashMap<String, Vec<Uuid>> =
        std::collections::HashMap::new();
    for (id, name) in &players {
        name_map.entry(normalize_name(name)).or_default().push(*id);
    }

    // Stage the (player_id, game_date) -> (oreb, dreb, total) updates, deduped
    // by the join key (keep last), then apply them in batched
    // `UPDATE ... FROM (VALUES ...)` statements. The old code issued one UPDATE
    // per torvik row (~113k) — fine on localhost but ~5 min over the prod DB's
    // round-trip latency (the run's long pole once the persist was batched).
    let mut by_key: HashMap<(Uuid, NaiveDate), (i32, i32, i32)> = HashMap::new();
    for g in games {
        let (Some(oreb), Some(dreb)) = (g.oreb, g.dreb) else {
            continue;
        };
        let (oreb, dreb) = (oreb as i32, dreb as i32);
        let total_reb = oreb + dreb;
        let Ok(game_date) = NaiveDate::parse_from_str(&g.date_str, "%Y%m%d") else {
            continue;
        };
        let Some(player_ids) = name_map.get(&normalize_name(&g.player_name)) else {
            continue;
        };
        for pid in player_ids {
            by_key.insert((*pid, game_date), (oreb, dreb, total_reb));
        }
    }
    let staged: Vec<(Uuid, NaiveDate, i32, i32, i32)> = by_key
        .into_iter()
        .map(|((pid, date), (oreb, dreb, total_reb))| (pid, date, oreb, dreb, total_reb))
        .collect();

    let mut updated: u64 = 0;
    // 5 binds/row; chunk well under Postgres' 65535-param cap.
    for chunk in staged.chunks(2000) {
        let mut qb = QueryBuilder::new(
            "UPDATE player_game_stats AS p \
             SET off_rebounds = v.oreb, def_rebounds = v.dreb, total_rebounds = v.total_reb \
             FROM ( ",
        );
        qb.push_values(chunk, |mut b, (pid, date, oreb, dreb, total_reb)| {
            b.push_bind(*pid)
                .push_bind(*date)
                .push_bind(*oreb)
                .push_bind(*dreb)
                .push_bind(*total_reb);
        });
        qb.push(" ) AS v(player_id, game_date, oreb, dreb, total_reb) WHERE p.player_id = v.player_id AND p.season = ");
        qb.push_bind(season);
        qb.push(
            " AND p.game_date = v.game_date AND (p.total_rebounds IS NULL OR p.total_rebounds = 0)",
        );
        updated += qb.build().execute(pool).await?.rows_affected();
    }

    info!(season, updated, "Torvik rebound backfill complete");
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Name normalization
// ---------------------------------------------------------------------------

/// Normalize a player name for matching across data sources.
/// Folds diacritics, strips suffixes (Jr, Sr, II, III, IV, V), drops
/// punctuation/apostrophes, collapses whitespace, and lowercases.
///
/// German umlauts expand to their digraph romanization (ä→ae, ö→oe, ü→ue,
/// ß→ss) rather than folding to a bare vowel, because NatStat romanizes some
/// German names that way — e.g. Torvik's "Johann Grünloh" is stored by NatStat
/// as "Johann Gruenloh" (issue #170). Torvik keeps the native umlaut, so both
/// sides must expand to the same digraph to meet. Every other diacritic folds
/// to its base letter. Both sides of a Torvik↔cstat match are run through this
/// function, so accented cstat names (Dörries, Kostić) normalize identically.
fn normalize_name(name: &str) -> String {
    let folded = fold_diacritics(name);

    // Split into tokens and strip trailing suffix tokens
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    let suffixes = ["jr", "sr", "ii", "iii", "iv", "v"];

    let end = if tokens.last().is_some_and(|t| suffixes.contains(t)) {
        tokens.len() - 1
    } else {
        tokens.len()
    };

    tokens[..end].join(" ")
}

/// Last token of a normalized name — the family name the surname fallback
/// keys on. Empty for an empty/whitespace name.
fn family_name(name: &str) -> String {
    normalize_name(name)
        .rsplit(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

/// First token of a normalized name — the given name the second fallback
/// keys on. Empty for an empty/whitespace name.
fn given_name(name: &str) -> String {
    normalize_name(name)
        .split(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Everything after the given name, spaces removed — the comparison key for
/// the surname guard. Joining the tokens is what lets "Van Soelen" meet
/// "VanSoelen"; a single-token name is its own surname key.
fn surname_key(name: &str) -> String {
    let norm = normalize_name(name);
    match norm.split_once(' ') {
        Some((_, rest)) => rest.replace(' ', ""),
        None => norm,
    }
}

/// Are these two surnames plausibly the same person's, given that the given
/// name and team already matched exactly?
///
/// Two accepted shapes, both drawn from the real mismatches in the data:
/// - **Containment** — one surname is a compound of the other, which is how
///   Torvik's "Tory Miller-Stewart" meets cstat's "Tory Miller" and how
///   "Jacob Enevold" meets "Jacob Enevold Jensen". Both sides must be at
///   least [`SURNAME_CONTAINMENT_MIN`] characters so short fragments can't
///   swallow unrelated names.
/// - **Near-spelling** — edit distance ≤ [`SURNAME_MAX_EDITS`], covering the
///   transposition/typo class ("Chirvous"/"Chievous", "Ostekowski"/
///   "Osetkowski", "Müller"/"Muller").
///
/// This guard is what makes the given-name fallback safe: a shared first name
/// carries far less identifying signal than a shared surname, so the pass is
/// only allowed to fire when the surname corroborates it. It rejects the
/// genuinely-different people the pass would otherwise pair — "Max Montana"
/// vs "Max Hoetzel", "Jaren Holmes" vs "Jaren English".
fn surnames_compatible(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.len() >= SURNAME_CONTAINMENT_MIN
        && b.len() >= SURNAME_CONTAINMENT_MIN
        && (a.contains(b) || b.contains(a))
    {
        return true;
    }
    levenshtein(a, b) <= SURNAME_MAX_EDITS
}

/// Minimum length for either side of a surname containment match.
const SURNAME_CONTAINMENT_MIN: usize = 4;
/// Maximum edit distance for a surname near-spelling match.
const SURNAME_MAX_EDITS: usize = 2;

/// Plain Levenshtein distance over `char`s, with an early bail once the
/// length gap alone exceeds what [`SURNAME_MAX_EDITS`] could bridge.
fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > SURNAME_MAX_EDITS {
        return usize::MAX;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(ca != cb));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Lowercase a name and fold its diacritics to ASCII, dropping punctuation
/// (periods, apostrophes — including the curly `’` U+2019 and the Windows-1252
/// mojibake control char U+0092 we see in a few DB rows). German umlauts and
/// ß expand to digraphs; all other Latin diacritics fold to their base letter.
/// Alphabetic characters with no mapping pass through lowercased; everything
/// else (punctuation, control chars) is dropped.
fn fold_diacritics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            // German umlauts / eszett → digraph romanization
            'ä' | 'Ä' | 'æ' | 'Æ' => out.push_str("ae"),
            'ö' | 'Ö' | 'œ' | 'Œ' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'þ' | 'Þ' => out.push_str("th"),
            // Latin diacritics → base letter
            'á' | 'à' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' | 'Á' | 'À' | 'Â' | 'Ã' | 'Å' | 'Ā'
            | 'Ă' | 'Ą' => out.push('a'),
            'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' | 'Ç' | 'Ć' | 'Č' | 'Ĉ' | 'Ċ' => out.push('c'),
            'đ' | 'ď' | 'ð' | 'Đ' | 'Ď' | 'Ð' => out.push('d'),
            'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'É' | 'È' | 'Ê' | 'Ë' | 'Ē'
            | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => out.push('e'),
            'ğ' | 'ĝ' | 'ġ' | 'ģ' | 'Ğ' | 'Ĝ' | 'Ġ' | 'Ģ' => out.push('g'),
            'í' | 'ì' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' | 'Í' | 'Ì' | 'Î' | 'Ï' | 'Ī' | 'Ĭ'
            | 'Į' | 'İ' => out.push('i'),
            'ł' | 'ĺ' | 'ļ' | 'ľ' | 'Ł' | 'Ĺ' | 'Ļ' | 'Ľ' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' | 'Ñ' | 'Ń' | 'Ņ' | 'Ň' => out.push('n'),
            'ó' | 'ò' | 'ô' | 'õ' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' | 'Ō'
            | 'Ŏ' | 'Ő' => out.push('o'),
            'ŕ' | 'ř' | 'Ŕ' | 'Ř' => out.push('r'),
            'ś' | 'š' | 'ş' | 'ŝ' | 'ș' | 'Ś' | 'Š' | 'Ş' | 'Ŝ' | 'Ș' => out.push('s'),
            'ţ' | 'ť' | 'ț' | 'Ţ' | 'Ť' | 'Ț' => out.push('t'),
            'ú' | 'ù' | 'û' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'Ú' | 'Ù' | 'Û' | 'Ū' | 'Ŭ' | 'Ů'
            | 'Ű' | 'Ų' => out.push('u'),
            'ý' | 'ÿ' | 'Ý' | 'Ÿ' => out.push('y'),
            'ź' | 'ž' | 'ż' | 'Ź' | 'Ž' | 'Ż' => out.push('z'),
            _ if c.is_alphabetic() || c.is_whitespace() => {
                out.extend(c.to_lowercase());
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Player matching
// ---------------------------------------------------------------------------

/// A cstat player row in the season roster.
struct PlayerRow {
    id: Uuid,
    /// Cstat name, as stored. Fallback keys derive from this.
    name: String,
    /// The player's cstat team for the season, if teamed.
    team_id: Option<Uuid>,
}

/// One season's cstat players and teams, indexed for in-process matching.
///
/// Loaded once per ingest so the fallback passes can iterate the roster
/// without going back to the DB. Both the Torvik name and the cstat name go
/// through the same `normalize_name`, so accented cstat rows (Dörries,
/// Kostić) and NatStat's German romanizations (Grünloh→Gruenloh, issue #170)
/// meet symmetrically — a SQL-side match couldn't fold diacritics the same
/// way on both sides.
struct SeasonRoster {
    players: Vec<PlayerRow>,
    /// normalized cstat name -> indices into `players`
    by_name: HashMap<String, Vec<usize>>,
    teams: Vec<TeamRow>,
}

/// A cstat team in the season, as the shared scorer wants it.
struct TeamRow {
    id: Uuid,
    full_name: String,
    short_name: Option<String>,
}

impl SeasonRoster {
    async fn load(pool: &PgPool, season: i32) -> anyhow::Result<Self> {
        let rows = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
            r#"SELECT p.id, p.name, p.team_id
               FROM players p
               WHERE p.season = $1"#,
        )
        .bind(season)
        .fetch_all(pool)
        .await?;

        let players: Vec<PlayerRow> = rows
            .into_iter()
            .map(|(id, name, team_id)| PlayerRow { id, name, team_id })
            .collect();

        let mut by_name: HashMap<String, Vec<usize>> = HashMap::with_capacity(players.len());
        for (i, p) in players.iter().enumerate() {
            by_name.entry(normalize_name(&p.name)).or_default().push(i);
        }

        let teams = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
            r#"SELECT id, name, short_name FROM teams WHERE season = $1"#,
        )
        .bind(season)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id, full_name, short_name)| TeamRow {
            id,
            full_name,
            short_name,
        })
        .collect();

        Ok(Self {
            players,
            by_name,
            teams,
        })
    }

    /// Resolve a Torvik team name to a cstat team id via the shared
    /// cross-source scorer — the same one the transfers / recruits /
    /// coachdict paths use, so a Torvik-vocabulary alias fixed for one is
    /// fixed for all. `teams.short_name` is already maintained in Torvik's
    /// vocabulary (`data/team_short_names.json`), so all but a handful of
    /// names resolve on the scorer's exact tier.
    ///
    /// Only the *best-scoring* team wins, and a tie at the best score
    /// resolves to `None`. That matters because the scorer's weakest tier is
    /// a bare prefix match on the full NatStat name: Torvik's "Houston"
    /// prefix-matches "Houston Baptist Huskies" (score 2) as well as
    /// "Houston Cougars" (score 0 on short_name), and only taking the minimum
    /// keeps the roster off the wrong school.
    fn resolve_team(&self, torvik_team: &str) -> Option<Uuid> {
        let mut best: Option<(u32, Uuid)> = None;
        let mut tied = false;
        for t in &self.teams {
            let Some(score) = team_match_score(t.short_name.as_deref(), &t.full_name, torvik_team)
            else {
                continue;
            };
            match best {
                Some((b, _)) if score > b => {}
                Some((b, _)) if score == b => tied = true,
                _ => {
                    best = Some((score, t.id));
                    tied = false;
                }
            }
        }
        best.filter(|_| !tied).map(|(_, id)| id)
    }
}

/// How each Torvik row resolved, for the run log and the CLI summary.
#[derive(Debug, Default)]
struct LinkStats {
    /// Normalized name matched a cstat player on the same team.
    exact: u64,
    /// Name matched exactly one cstat player, but not on the Torvik team —
    /// kept because a unique name is strong evidence on its own.
    name_only: u64,
    /// Recovered by the family-name fallback (nicknames).
    family_fallback: u64,
    /// Recovered by the given-name fallback (surname misspellings).
    given_fallback: u64,
    unlinked: u64,
    /// Unlinked rows at rotation minutes — the ones a user would notice.
    unlinked_rotation: u64,
    /// Torvik team names with no cstat counterpart, e.g. a program cstat
    /// hasn't ingested yet (Le Moyne, 2026).
    unresolved_teams: Vec<String>,
    /// A few `Name (Team)` strings to make the warning actionable.
    unlinked_sample: Vec<String>,
}

impl LinkStats {
    fn matched(&self) -> u64 {
        self.exact + self.name_only + self.family_fallback + self.given_fallback
    }
}

/// Minutes per game at or above which an unlinked row counts as a *rotation*
/// player for the log. Torvik's true MPG lives in `total_minutes`; the
/// `minutes_per_game` column actually holds Min% (see the column-naming
/// gotcha in `compute::compute_campom`).
const ROTATION_MPG: f64 = 10.0;

/// How many unlinked rows to name in the warning.
const UNLINKED_SAMPLE: usize = 8;

struct Links {
    /// One entry per input row, positionally aligned.
    player_ids: Vec<Option<Uuid>>,
    stats: LinkStats,
}

/// Resolve every Torvik row for a season to a cstat player.
///
/// Three passes, each strictly more permissive than the last and each barred
/// from taking a player an earlier pass already claimed (issue #243):
///
/// 1. **Exact name.** Normalized name plus the resolved team. If no candidate
///    is on that team but the name matches exactly one cstat player in the
///    whole season, take it — that covers a team cstat hasn't resolved and
///    genuine cstat team-label errors. Two-or-more candidates with no team
///    agreement are left unlinked rather than coin-flipped onto the first one,
///    which is how Torvik's Xavier "Anthony Robinson" ended up attached to
///    Missouri's.
/// 2. **Family name + team.** Torvik uses the common name where NatStat keeps
///    the legal one — Obi/Obadiah Toppin, Ja/Temetrius Morant, Johnny/Jonathan
///    Davis. Nicknames bear no systematic relation to the legal given name, so
///    this pass deliberately does not constrain it; safety comes from
///    requiring exactly one unclaimed cstat player *and* exactly one unmatched
///    Torvik row for the (team, family name) pair.
/// 3. **Given name + team, guarded by surname similarity.** The mirror case,
///    where the surname is misspelled or hyphenated differently on one side.
///    A shared given name is weak evidence by itself, so
///    [`surnames_compatible`] must also hold.
///
/// Passes 2 and 3 both require a resolved team; the residual after all three
/// is dominated by players NatStat never ingested at all (Anthony Barber's
/// 2015 N.C. State season, Fred VanVleet's 2015 Wichita State season), which
/// no matching rule can recover.
fn link_players(
    roster: &SeasonRoster,
    rows: &[crate::torvik::TorkvikPlayerSeason],
    season: i32,
) -> Links {
    let mut player_ids: Vec<Option<Uuid>> = vec![None; rows.len()];
    let mut stats = LinkStats::default();
    let mut claimed: HashSet<Uuid> = HashSet::with_capacity(rows.len());

    // Resolve each distinct Torvik team once.
    let mut team_ids: HashMap<&str, Option<Uuid>> = HashMap::new();
    for r in rows {
        team_ids
            .entry(r.team.as_str())
            .or_insert_with(|| roster.resolve_team(&r.team));
    }
    stats.unresolved_teams = {
        let mut v: Vec<String> = team_ids
            .iter()
            .filter(|(_, id)| id.is_none())
            .map(|(name, _)| (*name).to_string())
            .collect();
        v.sort();
        v
    };

    // Pass 1 — exact name.
    let mut unmatched: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.pid.is_none() {
            continue; // not persisted at all; must not claim a player
        }
        let team = team_ids[r.team.as_str()];
        let candidates = roster
            .by_name
            .get(&normalize_name(&r.player_name))
            .map(Vec::as_slice)
            .unwrap_or_default();

        let on_team = team.and_then(|t| {
            candidates
                .iter()
                .copied()
                .find(|&c| roster.players[c].team_id == Some(t))
        });
        match on_team {
            Some(c) => {
                stats.exact += 1;
                claimed.insert(roster.players[c].id);
                player_ids[i] = Some(roster.players[c].id);
            }
            None if candidates.len() == 1 => {
                stats.name_only += 1;
                claimed.insert(roster.players[candidates[0]].id);
                player_ids[i] = Some(roster.players[candidates[0]].id);
            }
            None => unmatched.push(i),
        }
    }

    // Passes 2 and 3 — same shape, different key and guard.
    unmatched = fallback_pass(
        roster,
        rows,
        &team_ids,
        &mut claimed,
        &mut player_ids,
        unmatched,
        family_name,
        |_, _| true,
        &mut stats.family_fallback,
    );
    unmatched = fallback_pass(
        roster,
        rows,
        &team_ids,
        &mut claimed,
        &mut player_ids,
        unmatched,
        given_name,
        |torvik, cstat| surnames_compatible(&surname_key(torvik), &surname_key(cstat)),
        &mut stats.given_fallback,
    );

    stats.unlinked = unmatched.len() as u64;
    let mut rotation: Vec<usize> = unmatched
        .iter()
        .copied()
        .filter(|&i| rows[i].total_minutes.is_some_and(|m| m >= ROTATION_MPG))
        .collect();
    stats.unlinked_rotation = rotation.len() as u64;
    // Best players first: the warning is only actionable if it names the ones
    // whose absence from the leaderboard someone would notice.
    rotation.sort_by(|&a, &b| {
        rows[b]
            .gbpm
            .partial_cmp(&rows[a].gbpm)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    stats.unlinked_sample = rotation
        .iter()
        .take(UNLINKED_SAMPLE)
        .map(|&i| format!("{} ({})", rows[i].player_name, rows[i].team))
        .collect();
    debug_assert_eq!(
        stats.matched() + stats.unlinked,
        rows.iter().filter(|r| r.pid.is_some()).count() as u64,
        "season {season}: every pid-carrying row must be counted exactly once"
    );

    Links { player_ids, stats }
}

/// One fallback pass: pair off `unmatched` Torvik rows against unclaimed cstat
/// players by `key`, within the same team, accepting only when the pairing is
/// unambiguous in *both* directions and `guard` agrees. Returns the rows still
/// unmatched.
///
/// Requiring uniqueness on the Torvik side as well as the cstat side is what
/// makes the pass order-independent: with at most one candidate and at most
/// one claimant per (team, key), no two groups can ever contend for the same
/// player, so the result doesn't depend on which row is visited first.
#[allow(clippy::too_many_arguments)]
fn fallback_pass(
    roster: &SeasonRoster,
    rows: &[crate::torvik::TorkvikPlayerSeason],
    team_ids: &HashMap<&str, Option<Uuid>>,
    claimed: &mut HashSet<Uuid>,
    player_ids: &mut [Option<Uuid>],
    unmatched: Vec<usize>,
    key: fn(&str) -> String,
    guard: fn(&str, &str) -> bool,
    counter: &mut u64,
) -> Vec<usize> {
    // Group the unmatched Torvik rows by (team, key).
    let mut groups: HashMap<(Uuid, String), Vec<usize>> = HashMap::new();
    let mut skipped: Vec<usize> = Vec::new();
    for i in unmatched {
        match team_ids[rows[i].team.as_str()] {
            Some(team) => groups
                .entry((team, key(&rows[i].player_name)))
                .or_default()
                .push(i),
            None => skipped.push(i),
        }
    }

    // Index the still-unclaimed cstat players the same way.
    let mut pool: HashMap<(Uuid, String), Vec<usize>> = HashMap::new();
    for (i, p) in roster.players.iter().enumerate() {
        if let Some(team) = p.team_id
            && !claimed.contains(&p.id)
        {
            pool.entry((team, key(&p.name))).or_default().push(i);
        }
    }

    let mut still = skipped;
    for (group_key, group) in groups {
        let candidates = pool.get(&group_key).map(Vec::as_slice).unwrap_or_default();
        match (group.as_slice(), candidates) {
            ([row], [candidate])
                if guard(&rows[*row].player_name, &roster.players[*candidate].name) =>
            {
                claimed.insert(roster.players[*candidate].id);
                player_ids[*row] = Some(roster.players[*candidate].id);
                *counter += 1;
            }
            _ => still.extend(group),
        }
    }
    still.sort_unstable();
    still
}

/// Parse height string like "6-5" to inches.
fn parse_height(s: &str) -> Option<i32> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 2 {
        let feet: i32 = parts[0].trim().parse().ok()?;
        let inches: i32 = parts[1].trim().parse().ok()?;
        Some(feet * 12 + inches)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // normalize_name tests

    #[test]
    fn normalize_strips_jr_suffix() {
        assert_eq!(normalize_name("Roddy Gayle Jr"), "roddy gayle");
    }

    #[test]
    fn normalize_strips_sr_suffix() {
        assert_eq!(normalize_name("John Smith Sr"), "john smith");
    }

    #[test]
    fn normalize_strips_roman_numeral_suffixes() {
        assert_eq!(normalize_name("Robert Davis II"), "robert davis");
        assert_eq!(normalize_name("Robert Davis III"), "robert davis");
        assert_eq!(normalize_name("Robert Davis IV"), "robert davis");
    }

    #[test]
    fn normalize_strips_periods() {
        assert_eq!(normalize_name("D.J. Wagner"), "dj wagner");
    }

    #[test]
    fn normalize_strips_apostrophes() {
        assert_eq!(normalize_name("D'Angelo Russell"), "dangelo russell");
    }

    #[test]
    fn normalize_strips_unicode_apostrophe() {
        assert_eq!(normalize_name("D\u{2019}Angelo Russell"), "dangelo russell");
    }

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize_name("COOPER FLAGG"), "cooper flagg");
    }

    #[test]
    fn normalize_no_suffix_unchanged() {
        assert_eq!(normalize_name("Cooper Flagg"), "cooper flagg");
    }

    #[test]
    fn normalize_v_suffix_stripped() {
        // "V" is treated as a suffix (Roman numeral 5)
        assert_eq!(normalize_name("Someone V"), "someone");
    }

    // Diacritic folding (issue #170)

    #[test]
    fn normalize_expands_german_umlaut_to_digraph() {
        // Torvik keeps the umlaut, NatStat romanizes it as "ue" — both must
        // land on the same "gruenloh" for the match to fire.
        assert_eq!(normalize_name("Johann Grünloh"), "johann gruenloh");
        assert_eq!(normalize_name("Johann Gruenloh"), "johann gruenloh");
    }

    #[test]
    fn normalize_expands_all_german_umlauts_and_eszett() {
        assert_eq!(normalize_name("Amon Dörries"), "amon doerries");
        assert_eq!(normalize_name("Carlos Jürgens"), "carlos juergens");
        assert_eq!(normalize_name("Jäger"), "jaeger");
        assert_eq!(normalize_name("Weiß"), "weiss");
        // æ ligature folds like ä.
        assert_eq!(normalize_name("Sivert Wærstad"), "sivert waerstad");
    }

    #[test]
    fn normalize_folds_latin_diacritics_to_base_letter() {
        assert_eq!(normalize_name("Aleksej Kostić"), "aleksej kostic");
        assert_eq!(normalize_name("Fedor Žugić"), "fedor zugic");
        assert_eq!(normalize_name("Francis Lācis"), "francis lacis");
        assert_eq!(normalize_name("Javonté Johnson"), "javonte johnson");
        assert_eq!(normalize_name("Josué Grullon"), "josue grullon");
    }

    #[test]
    fn normalize_drops_windows1252_apostrophe_mojibake() {
        // "D\u{0092}Angelo Allen" — a Windows-1252 curly apostrophe stored raw.
        assert_eq!(normalize_name("D\u{0092}Angelo Allen"), "dangelo allen");
    }

    // Name-part helpers (issue #243)

    #[test]
    fn name_parts_split_on_normalized_tokens() {
        assert_eq!(family_name("Obi Toppin"), "toppin");
        assert_eq!(given_name("Obi Toppin"), "obi");
        // The suffix is stripped before the family name is taken.
        assert_eq!(family_name("Ace Baldwin Jr."), "baldwin");
        // Compound surnames join into one key so spacing can't split them.
        assert_eq!(surname_key("Keaton Van Soelen"), "vansoelen");
        assert_eq!(surname_key("Keaton VanSoelen"), "vansoelen");
        // A mononym is its own surname key.
        assert_eq!(surname_key("Pele"), "pele");
    }

    #[test]
    fn surnames_compatible_accepts_compound_and_near_spellings() {
        // Compound vs bare — the hyphenation split between the two sources.
        assert!(surnames_compatible("millerstewart", "miller"));
        assert!(surnames_compatible("enevold", "enevoldjensen"));
        // Typos and transpositions.
        assert!(surnames_compatible("chirvous", "chievous"));
        assert!(surnames_compatible("ostekowski", "osetkowski"));
        assert!(surnames_compatible("mueller", "muller"));
    }

    #[test]
    fn surnames_compatible_rejects_different_people() {
        // Same given name, unrelated surname — the case the guard exists for.
        assert!(!surnames_compatible("montana", "hoetzel"));
        assert!(!surnames_compatible("holmes", "english"));
        assert!(!surnames_compatible("navarro", "colon"));
        // A short fragment must not swallow a longer surname.
        assert!(!surnames_compatible("lee", "leemanwilliams"));
        assert!(!surnames_compatible("", "toppin"));
    }

    // Team resolution (issue #243)

    /// Build a roster from `(id, name, team_id)` players and
    /// `(id, short_name, full_name)` teams, mirroring what `load` reads.
    fn roster(
        players: &[(Uuid, &str, Option<Uuid>)],
        teams: &[(Uuid, &str, &str)],
    ) -> SeasonRoster {
        let players: Vec<PlayerRow> = players
            .iter()
            .map(|(id, name, team_id)| PlayerRow {
                id: *id,
                name: (*name).to_string(),
                team_id: *team_id,
            })
            .collect();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in players.iter().enumerate() {
            by_name.entry(normalize_name(&p.name)).or_default().push(i);
        }
        let teams = teams
            .iter()
            .map(|(id, short, full)| TeamRow {
                id: *id,
                full_name: (*full).to_string(),
                short_name: Some((*short).to_string()),
            })
            .collect();
        SeasonRoster {
            players,
            by_name,
            teams,
        }
    }

    fn torvik_row(
        name: &str,
        team: &str,
        pid: i32,
        mpg: f64,
    ) -> crate::torvik::TorkvikPlayerSeason {
        crate::torvik::TorkvikPlayerSeason {
            player_name: name.to_string(),
            team: team.to_string(),
            pid: Some(pid),
            total_minutes: Some(mpg),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_team_matches_on_short_name() {
        let duke = Uuid::from_u128(10);
        let r = roster(&[], &[(duke, "Duke", "Duke Blue Devils")]);
        assert_eq!(r.resolve_team("Duke"), Some(duke));
    }

    #[test]
    fn resolve_team_handles_torvik_22_char_truncation() {
        // Torvik cuts its team-name column at 22 chars, which is why every
        // Texas A&M-Corpus Christi roster used to miss. The alias table
        // already carried both cstat spellings for the coachdict ingest.
        let tamc = Uuid::from_u128(12);
        let truncated = "Texas A&M Corpus Chris";
        assert_eq!(truncated.len(), 22);
        for full in [
            "Texas A&M-Corpus Christi Islanders",
            "Texas A&M Corpus Christi",
        ] {
            let r = roster(&[], &[(tamc, "Texas A&M Corpus Christi", full)]);
            assert_eq!(r.resolve_team(truncated), Some(tamc), "full name {full}");
        }
    }

    #[test]
    fn resolve_team_prefers_the_best_score_over_a_bare_prefix() {
        // Torvik's "Houston" prefix-matches "Houston Baptist Huskies" on the
        // scorer's weakest tier; only taking the minimum keeps that roster
        // off the wrong school.
        let (houston, hbu) = (Uuid::from_u128(13), Uuid::from_u128(14));
        let r = roster(
            &[],
            &[
                (hbu, "Houston Baptist", "Houston Baptist Huskies"),
                (houston, "Houston", "Houston Cougars"),
            ],
        );
        assert_eq!(r.resolve_team("Houston"), Some(houston));
    }

    #[test]
    fn resolve_team_follows_school_renames() {
        // Torvik retro-names Houston Baptist's pre-2022 seasons.
        let hbu = Uuid::from_u128(14);
        let r = roster(&[], &[(hbu, "Houston Baptist", "Houston Baptist Huskies")]);
        assert_eq!(r.resolve_team("Houston Christian"), Some(hbu));
    }

    #[test]
    fn resolve_team_returns_none_for_an_unknown_program() {
        let r = roster(&[], &[(Uuid::from_u128(15), "Duke", "Duke Blue Devils")]);
        assert_eq!(r.resolve_team("Le Moyne"), None);
    }

    #[test]
    fn resolve_team_returns_none_when_two_teams_tie() {
        // A tie at the best score is ambiguous, not a coin flip.
        let (a, b) = (Uuid::from_u128(16), Uuid::from_u128(17));
        let r = roster(
            &[],
            &[
                (a, "Miami", "Miami (Fla.) Hurricanes"),
                (b, "Miami", "Miami (Ohio) RedHawks"),
            ],
        );
        assert_eq!(r.resolve_team("Miami"), None);
    }

    // link_players (issue #243)

    #[test]
    fn link_exact_name_prefers_the_matching_team() {
        let (illinois, cal_poly) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let (t_ill, t_cp) = (Uuid::from_u128(90), Uuid::from_u128(91));
        let r = roster(
            &[
                (illinois, "Jake Davis", Some(t_ill)),
                (cal_poly, "Jake Davis", Some(t_cp)),
            ],
            &[
                (t_ill, "Illinois", "Illinois Fighting Illini"),
                (t_cp, "Cal Poly", "Cal Poly Mustangs"),
            ],
        );
        let rows = [
            torvik_row("Jake Davis", "Cal Poly", 1, 20.0),
            torvik_row("Jake Davis", "Illinois", 2, 20.0),
        ];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.player_ids, vec![Some(cal_poly), Some(illinois)]);
        assert_eq!(links.stats.exact, 2);
    }

    #[test]
    fn link_exact_name_matches_across_umlaut_romanization() {
        // cstat stores the NatStat romanization; Torvik supplies the umlaut.
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Johann Gruenloh", Some(team))],
            &[(team, "Virginia", "Virginia Cavaliers")],
        );
        let rows = [torvik_row("Johann Grünloh", "Virginia", 1, 20.0)];
        assert_eq!(link_players(&r, &rows, 2026).player_ids, vec![Some(id)]);
    }

    #[test]
    fn link_keeps_the_name_only_tier_for_a_unique_name() {
        // Team didn't resolve, but exactly one player in the season carries
        // the name — strong enough on its own.
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Cooper Flagg", Some(team))],
            &[(team, "Duke", "Duke Blue Devils")],
        );
        let rows = [torvik_row("Cooper Flagg", "Some Other School", 1, 20.0)];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.player_ids, vec![Some(id)]);
        assert_eq!(links.stats.name_only, 1);
        assert_eq!(links.stats.unresolved_teams, vec!["Some Other School"]);
    }

    #[test]
    fn link_leaves_an_ambiguous_off_team_name_unlinked() {
        // Two same-name players, neither on the Torvik team: the old code
        // coin-flipped onto the first, attaching Torvik's Xavier "Anthony
        // Robinson" row to Missouri's Anthony Robinson.
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let (t_a, t_b) = (Uuid::from_u128(90), Uuid::from_u128(91));
        let r = roster(
            &[
                (a, "Anthony Robinson", Some(t_a)),
                (b, "Anthony Robinson", Some(t_b)),
            ],
            &[
                (t_a, "Missouri", "Missouri Tigers"),
                (t_b, "South Carolina", "South Carolina Gamecocks"),
            ],
        );
        let rows = [torvik_row("Anthony Robinson", "Xavier", 1, 20.0)];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.player_ids, vec![None]);
        assert_eq!(links.stats.unlinked, 1);
        assert_eq!(links.stats.unlinked_rotation, 1);
    }

    #[test]
    fn link_recovers_a_nickname_by_family_name_and_team() {
        // Torvik "Obi Toppin" vs NatStat "Obadiah Toppin" (2020 AP POY).
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Obadiah Toppin", Some(team))],
            &[(team, "Dayton", "Dayton Flyers")],
        );
        let rows = [torvik_row("Obi Toppin", "Dayton", 1, 31.6)];
        let links = link_players(&r, &rows, 2020);
        assert_eq!(links.player_ids, vec![Some(id)]);
        assert_eq!(links.stats.family_fallback, 1);
    }

    #[test]
    fn link_family_fallback_needs_a_unique_pair_on_both_sides() {
        // Two unmatched Barneses on one team: no way to tell which is which,
        // so neither links.
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let team = Uuid::from_u128(90);
        let r = roster(
            &[
                (a, "Corey Barnes", Some(team)),
                (b, "Devon Barnes", Some(team)),
            ],
            &[(team, "Alcorn St.", "Alcorn State Braves")],
        );
        let rows = [
            torvik_row("CJ Barnes", "Alcorn St.", 1, 20.0),
            torvik_row("Dee Barnes", "Alcorn St.", 2, 20.0),
        ];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.player_ids, vec![None, None]);
        assert_eq!(links.stats.family_fallback, 0);
    }

    #[test]
    fn link_family_fallback_will_not_steal_an_exact_match() {
        // The exact-name pass claims Corey Barnes first, so the fallback has
        // no unclaimed candidate left and must not re-take him.
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let team = Uuid::from_u128(90);
        let r = roster(
            &[
                (a, "Corey Barnes", Some(team)),
                (b, "Someone Else", Some(team)),
            ],
            &[(team, "Alcorn St.", "Alcorn State Braves")],
        );
        let rows = [
            torvik_row("CJ Barnes", "Alcorn St.", 1, 20.0),
            torvik_row("Corey Barnes", "Alcorn St.", 2, 20.0),
        ];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.player_ids, vec![None, Some(a)]);
        assert_eq!(links.stats.exact, 1);
        assert_eq!(links.stats.family_fallback, 0);
    }

    #[test]
    fn link_recovers_a_misspelled_surname_by_given_name_and_team() {
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Quinton Chievous", Some(team))],
            &[(team, "Hampton", "Hampton Pirates")],
        );
        let rows = [torvik_row("Quinton Chirvous", "Hampton", 1, 25.0)];
        let links = link_players(&r, &rows, 2015);
        assert_eq!(links.player_ids, vec![Some(id)]);
        assert_eq!(links.stats.given_fallback, 1);
    }

    #[test]
    fn link_given_fallback_stops_at_an_unrelated_surname() {
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Max Hoetzel", Some(team))],
            &[(team, "San Diego St.", "San Diego State Aztecs")],
        );
        let rows = [torvik_row("Max Montana", "San Diego St.", 1, 20.0)];
        let links = link_players(&r, &rows, 2018);
        assert_eq!(links.player_ids, vec![None]);
        assert_eq!(links.stats.given_fallback, 0);
    }

    #[test]
    fn link_fallbacks_require_a_resolved_team() {
        // Without a team anchor a surname alone is far too weak to pair on.
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Obadiah Toppin", Some(team))],
            &[(team, "Dayton", "Dayton Flyers")],
        );
        let rows = [torvik_row("Obi Toppin", "Le Moyne", 1, 31.6)];
        let links = link_players(&r, &rows, 2020);
        assert_eq!(links.player_ids, vec![None]);
    }

    #[test]
    fn link_ignores_rows_without_a_torvik_pid() {
        // Those rows are never persisted, so they must not claim a player.
        let id = Uuid::from_u128(1);
        let team = Uuid::from_u128(90);
        let r = roster(
            &[(id, "Cooper Flagg", Some(team))],
            &[(team, "Duke", "Duke Blue Devils")],
        );
        let rows = [crate::torvik::TorkvikPlayerSeason {
            player_name: "Cooper Flagg".to_string(),
            team: "Duke".to_string(),
            pid: None,
            ..Default::default()
        }];
        let links = link_players(&r, &rows, 2025);
        assert_eq!(links.player_ids, vec![None]);
        assert_eq!(links.stats.matched(), 0);
        assert_eq!(links.stats.unlinked, 0);
    }

    #[test]
    fn link_counts_only_rotation_minutes_as_rotation_unlinked() {
        let r = roster(&[], &[(Uuid::from_u128(90), "Duke", "Duke Blue Devils")]);
        let rows = [
            torvik_row("Nobody Here", "Duke", 1, ROTATION_MPG - 0.1),
            torvik_row("Also Missing", "Duke", 2, ROTATION_MPG),
        ];
        let links = link_players(&r, &rows, 2026);
        assert_eq!(links.stats.unlinked, 2);
        assert_eq!(links.stats.unlinked_rotation, 1);
        assert_eq!(links.stats.unlinked_sample, vec!["Also Missing (Duke)"]);
    }

    // parse_height tests

    #[test]
    fn parse_height_standard() {
        assert_eq!(parse_height("6-5"), Some(77));
    }

    #[test]
    fn parse_height_with_spaces() {
        assert_eq!(parse_height("6 - 5"), Some(77));
    }

    #[test]
    fn parse_height_short() {
        assert_eq!(parse_height("5-10"), Some(70));
    }

    #[test]
    fn parse_height_tall() {
        assert_eq!(parse_height("7-1"), Some(85));
    }

    #[test]
    fn parse_height_invalid() {
        assert_eq!(parse_height("six-five"), None);
    }

    #[test]
    fn parse_height_single_part() {
        assert_eq!(parse_height("75"), None);
    }
}
