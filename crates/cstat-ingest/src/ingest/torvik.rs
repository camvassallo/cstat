//! Barttorvik data ingestion: player season stats and per-game rebound backfill.

use crate::torvik::{TorkvikClient, TorkvikGameRow};
use chrono::NaiveDate;
use sqlx::{PgPool, QueryBuilder};
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

/// Ingest Torvik player season stats, matching to existing cstat players.
pub async fn ingest_torvik_player_stats(
    client: &TorkvikClient,
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<(u64, u64)> {
    let players = client.fetch_player_stats(season).await?;
    // Build the season's cstat player index once and match in-process. Both the
    // Torvik name and the cstat name go through the same `normalize_name`, so
    // accented cstat rows (Dörries, Kostić) and NatStat's German romanizations
    // (Grünloh→Gruenloh, issue #170) meet symmetrically — a formerly SQL-side
    // match couldn't fold diacritics the same way on both sides.
    let name_index = build_player_name_index(pool, season).await?;
    let mut upserted: u64 = 0;
    let mut matched: u64 = 0;

    for p in &players {
        let pid = match p.pid {
            Some(id) => id,
            None => continue,
        };

        // Torvik team names differ from NatStat, so we fuzzy-match the team to
        // disambiguate same-name players, falling back to a name-only match.
        let player_id = match_player(&name_index, &p.player_name, &p.team);
        if player_id.is_some() {
            matched += 1;
        }

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
                    nba_pick, min_per
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
                    $64, $65
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
        .execute(pool)
        .await?;

        upserted += 1;
    }

    info!(
        season,
        upserted, matched, "Torvik player stats ingestion complete"
    );
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

/// A cstat player candidate in the season name index.
struct PlayerCandidate {
    id: Uuid,
    /// The player's cstat team name (e.g. "Virginia Cavaliers"), if teamed.
    team_name: Option<String>,
    /// The team's short name (e.g. "Virginia"), if teamed.
    short_name: Option<String>,
}

/// Build a `normalized_name -> candidates` index of every player in the season.
/// Teams are LEFT-joined so unteamed players (no `team_id`) still appear for the
/// name-only fallback, mirroring the old two-tier SQL match.
async fn build_player_name_index(
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<HashMap<String, Vec<PlayerCandidate>>> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>)>(
        r#"SELECT p.id, p.name, t.name AS team_name, t.short_name
           FROM players p
           LEFT JOIN teams t ON t.id = p.team_id AND t.season = p.season
           WHERE p.season = $1"#,
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    let mut index: HashMap<String, Vec<PlayerCandidate>> = HashMap::with_capacity(rows.len());
    for (id, name, team_name, short_name) in rows {
        index
            .entry(normalize_name(&name))
            .or_default()
            .push(PlayerCandidate {
                id,
                team_name,
                short_name,
            });
    }
    Ok(index)
}

/// Match a Torvik player to a cstat player using the pre-built season index.
/// Prefers a candidate whose cstat team fuzzy-matches the Torvik team name;
/// otherwise falls back to the first same-name candidate (the old name-only
/// tier). Both names are normalized identically, so accents and German
/// umlaut romanizations meet (issue #170).
fn match_player(
    index: &HashMap<String, Vec<PlayerCandidate>>,
    name: &str,
    torvik_team: &str,
) -> Option<Uuid> {
    let candidates = index.get(&normalize_name(name))?;
    candidates
        .iter()
        .find(|c| team_matches(c, torvik_team))
        .or_else(|| candidates.first())
        .map(|c| c.id)
}

/// Fuzzy team match mirroring the old SQL predicate: the cstat team name
/// contains the Torvik team, or the Torvik team contains the cstat short name.
/// Case-insensitive; empty short names never match (SQL `LIKE '%%'` would).
fn team_matches(candidate: &PlayerCandidate, torvik_team: &str) -> bool {
    let torvik = torvik_team.to_lowercase();
    let name_hit = candidate
        .team_name
        .as_deref()
        .is_some_and(|t| t.to_lowercase().contains(&torvik));
    let short_hit = candidate
        .short_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_some_and(|s| torvik.contains(&s.to_lowercase()));
    name_hit || short_hit
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

    // match_player tests

    fn candidate(id: Uuid, team: &str, short: &str) -> PlayerCandidate {
        PlayerCandidate {
            id,
            team_name: Some(team.to_string()),
            short_name: Some(short.to_string()),
        }
    }

    #[test]
    fn match_player_matches_across_umlaut_romanization() {
        // cstat stores the NatStat romanization; Torvik supplies the umlaut.
        let id = Uuid::from_u128(1);
        let mut index: HashMap<String, Vec<PlayerCandidate>> = HashMap::new();
        index
            .entry(normalize_name("Johann Gruenloh"))
            .or_default()
            .push(candidate(id, "Virginia Cavaliers", "Virginia"));

        assert_eq!(match_player(&index, "Johann Grünloh", "Virginia"), Some(id));
    }

    #[test]
    fn match_player_prefers_team_match_for_same_name() {
        let illinois = Uuid::from_u128(1);
        let cal_poly = Uuid::from_u128(2);
        let mut index: HashMap<String, Vec<PlayerCandidate>> = HashMap::new();
        let entry = index.entry(normalize_name("Jake Davis")).or_default();
        entry.push(candidate(illinois, "Illinois Fighting Illini", "Illinois"));
        entry.push(candidate(cal_poly, "Cal Poly Mustangs", "Cal Poly"));

        assert_eq!(
            match_player(&index, "Jake Davis", "Cal Poly"),
            Some(cal_poly)
        );
        assert_eq!(
            match_player(&index, "Jake Davis", "Illinois"),
            Some(illinois)
        );
    }

    #[test]
    fn match_player_falls_back_to_name_only() {
        // No team match → first same-name candidate, mirroring the old tier-2.
        let id = Uuid::from_u128(1);
        let mut index: HashMap<String, Vec<PlayerCandidate>> = HashMap::new();
        index
            .entry(normalize_name("Cooper Flagg"))
            .or_default()
            .push(candidate(id, "Duke Blue Devils", "Duke"));

        assert_eq!(
            match_player(&index, "Cooper Flagg", "Some Other School"),
            Some(id)
        );
    }

    #[test]
    fn match_player_returns_none_when_absent() {
        let index: HashMap<String, Vec<PlayerCandidate>> = HashMap::new();
        assert_eq!(match_player(&index, "Nobody Here", "Nowhere"), None);
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
