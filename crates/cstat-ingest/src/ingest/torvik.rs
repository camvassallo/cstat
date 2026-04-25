//! Barttorvik data ingestion: player season stats and per-game rebound backfill.

use crate::torvik::TorkvikClient;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

/// Ingest Torvik player season stats, matching to existing cstat players.
pub async fn ingest_torvik_player_stats(
    client: &TorkvikClient,
    pool: &PgPool,
    season: i32,
) -> anyhow::Result<(u64, u64)> {
    let players = client.fetch_player_stats(season).await?;
    let mut upserted: u64 = 0;
    let mut matched: u64 = 0;

    for p in &players {
        let pid = match p.pid {
            Some(id) => id,
            None => continue,
        };

        // Try to match to an existing cstat player by name + team + season.
        // Torvik team names differ from NatStat, so we normalize and fuzzy-match.
        let player_id = match_player(pool, &p.player_name, &p.team, season).await?;
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
                    nba_pick
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
                    $64
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

/// Backfill missing rebounds in player_game_stats from Torvik game-level data.
pub async fn backfill_rebounds_from_torvik(
    client: &TorkvikClient,
    pool: &PgPool,
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

    let games = client.fetch_game_stats(season).await?;
    let mut updated: u64 = 0;

    for g in &games {
        let oreb = match g.oreb {
            Some(v) => v as i32,
            None => continue,
        };
        let dreb = match g.dreb {
            Some(v) => v as i32,
            None => continue,
        };
        let total_reb = oreb + dreb;

        let game_date = match chrono::NaiveDate::parse_from_str(&g.date_str, "%Y%m%d") {
            Ok(d) => d,
            Err(_) => continue,
        };

        let normalized = normalize_name(&g.player_name);
        let player_ids = match name_map.get(&normalized) {
            Some(ids) => ids,
            None => continue,
        };

        for pid in player_ids {
            let result = sqlx::query(
                r#"UPDATE player_game_stats
                   SET off_rebounds = $1,
                       def_rebounds = $2,
                       total_rebounds = $3
                   WHERE player_id = $4
                     AND season = $5
                     AND game_date = $6
                     AND (total_rebounds IS NULL OR total_rebounds = 0)"#,
            )
            .bind(oreb)
            .bind(dreb)
            .bind(total_reb)
            .bind(pid)
            .bind(season)
            .bind(game_date)
            .execute(pool)
            .await?;

            updated += result.rows_affected();
        }
    }

    info!(season, updated, "Torvik rebound backfill complete");
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Name normalization
// ---------------------------------------------------------------------------

/// Normalize a player name for matching across data sources.
/// Strips suffixes (Jr, Sr, II, III, IV, V), collapses whitespace,
/// removes periods/apostrophes, and lowercases.
fn normalize_name(name: &str) -> String {
    let s = name.replace(['.', '\'', '\u{2019}'], "").to_lowercase();

    // Split into tokens and strip trailing suffix tokens
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let suffixes = ["jr", "sr", "ii", "iii", "iv", "v"];

    let end = if tokens.last().is_some_and(|t| suffixes.contains(t)) {
        tokens.len() - 1
    } else {
        tokens.len()
    };

    tokens[..end].join(" ")
}

// ---------------------------------------------------------------------------
// Player matching
// ---------------------------------------------------------------------------

/// Match a Torvik player to a cstat player by name + team + season.
/// Uses normalized name matching and fuzzy team name matching.
async fn match_player(
    pool: &PgPool,
    name: &str,
    torvik_team: &str,
    season: i32,
) -> anyhow::Result<Option<Uuid>> {
    let normalized = normalize_name(name);

    // First try normalized name match + team within the season.
    // SQL-side: TRANSLATE strips . ' ' (apostrophes), then REGEXP_REPLACE strips suffixes.
    let row = sqlx::query_as::<_, (Uuid,)>(
        r#"SELECT p.id FROM players p
           JOIN teams t ON t.id = p.team_id AND t.season = p.season
           WHERE p.season = $1
             AND LOWER(TRIM(REGEXP_REPLACE(
                   TRANSLATE(p.name, E'.\x27\u2019', ''),
                   E'\\s+(Jr|Sr|II|III|IV|V)$', '', 'i'
                 ))) = $2
             AND (LOWER(t.name) LIKE '%' || LOWER($3) || '%'
                  OR LOWER($3) LIKE '%' || LOWER(t.short_name) || '%')
           LIMIT 1"#,
    )
    .bind(season)
    .bind(&normalized)
    .bind(torvik_team)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = row {
        return Ok(Some(id));
    }

    // Fallback: normalized name only within season
    let row = sqlx::query_as::<_, (Uuid,)>(
        r#"SELECT p.id FROM players p
           WHERE p.season = $1
             AND LOWER(TRIM(REGEXP_REPLACE(
                   TRANSLATE(p.name, E'.\x27\u2019', ''),
                   E'\\s+(Jr|Sr|II|III|IV|V)$', '', 'i'
                 ))) = $2
           LIMIT 1"#,
    )
    .bind(season)
    .bind(&normalized)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id,)| id))
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
