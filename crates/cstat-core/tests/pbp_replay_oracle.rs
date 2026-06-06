//! Oracle validation for the SUB-replay engine (P2b).
//!
//! Measures how often the replayed 5-man lineup matches NatStat's per-play
//! `onfloorhome`/`onfloorvis` (the API embeds the on-floor five on every row;
//! the CSV does not, which is why we replay). The replay runs off the CSV-loaded
//! `play_by_play` rows; the oracle is read from the cached API responses in
//! `api_cache`. A play counts as a match only when BOTH teams' fives are exactly
//! right.
//!
//! Gated `#[ignore]` — needs a local DB with PBP loaded and the API responses
//! cached for the validation games. Run with:
//!   DATABASE_URL=... cargo test -p cstat-core --test pbp_replay_oracle -- --ignored --nocapture

use std::collections::{HashMap, HashSet};

use cstat_core::pbp_replay::replay_game_from_db;
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const VALIDATION_GAMES: &[&str] = &["1511104", "1480284", "1482209", "1490485"];

#[tokio::test]
#[ignore = "needs local DB + cached API oracle"]
async fn replay_matches_onfloor_oracle() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    let mut tot_cmp = 0u64;
    let mut tot_match = 0u64;

    for code in VALIDATION_GAMES {
        // Resolve the game + season.
        let Some(row) = sqlx::query("SELECT id, season FROM games WHERE natstat_id = $1")
            .bind(code)
            .fetch_optional(&pool)
            .await
            .unwrap()
        else {
            println!("game {code}: not in DB, skipping");
            continue;
        };
        let game_id: Uuid = row.get("id");
        let season: i32 = row.get("season");

        // Oracle: sequence -> (home codes, vis codes), scanned from every cached
        // playbyplay page, keeping only plays for THIS game.
        let oracle = build_oracle(&pool, code).await;
        if oracle.is_empty() {
            println!("game {code}: no cached onfloor oracle, skipping");
            continue;
        }

        // Map natstat player codes -> our player UUIDs for this season.
        let codes: HashSet<String> = oracle
            .values()
            .flat_map(|(h, v)| h.iter().chain(v.iter()).cloned())
            .collect();
        let code_to_uuid = code_uuid_map(&pool, season, &codes).await;

        // seq -> sort_order, and the set of non-sub seqs (oracle is keyed by
        // sort_order / sequence).
        let play_rows = sqlx::query(
            "SELECT seq, sort_order, NOT ('SUB' = ANY(tags)) AS non_sub
             FROM play_by_play WHERE game_id = $1 ORDER BY seq",
        )
        .bind(game_id)
        .fetch_all(&pool)
        .await
        .unwrap();

        let (_inputs, result) = replay_game_from_db(&pool, game_id).await.unwrap();
        // Stints sorted by start_seq for a covering lookup.
        let stints = &result.stints;

        let mut g_cmp = 0u64;
        let mut g_match = 0u64;
        for pr in &play_rows {
            let non_sub: bool = pr.get("non_sub");
            if !non_sub {
                continue;
            }
            let seq: i32 = pr.get("seq");
            let Some(sort_order): Option<String> = pr.get("sort_order") else {
                continue;
            };
            let Some((o_home, o_vis)) = oracle.get(&sort_order) else {
                continue;
            };
            // Covering stint for this play.
            let Some(st) = stints
                .iter()
                .find(|s| s.start_seq <= seq && seq <= s.end_seq)
            else {
                continue;
            };
            let oracle_home: HashSet<Uuid> = o_home
                .iter()
                .filter_map(|c| code_to_uuid.get(c).copied())
                .collect();
            let oracle_vis: HashSet<Uuid> = o_vis
                .iter()
                .filter_map(|c| code_to_uuid.get(c).copied())
                .collect();
            // Only score plays where the oracle fully resolved to 5+5 UUIDs, so
            // we measure replay accuracy, not code→UUID mapping gaps.
            if oracle_home.len() != 5 || oracle_vis.len() != 5 {
                continue;
            }
            let replay_home: HashSet<Uuid> = st.home_lineup.iter().copied().collect();
            let replay_vis: HashSet<Uuid> = st.vis_lineup.iter().copied().collect();
            g_cmp += 1;
            if replay_home == oracle_home && replay_vis == oracle_vis {
                g_match += 1;
            }
        }

        let pct = if g_cmp > 0 {
            100.0 * g_match as f64 / g_cmp as f64
        } else {
            0.0
        };
        println!(
            "game {code}: {g_match}/{g_cmp} plays match ({pct:.1}%) | stints={} unresolved_subs={} name_recovered={} plays_off_five={}",
            result.stints.len(),
            result.unresolved_subs,
            _inputs.subs_resolved_by_name,
            result.plays_off_five,
        );
        tot_cmp += g_cmp;
        tot_match += g_match;
    }

    let pct = if tot_cmp > 0 {
        100.0 * tot_match as f64 / tot_cmp as f64
    } else {
        0.0
    };
    println!("\nOVERALL: {tot_match}/{tot_cmp} plays match ({pct:.1}%)");
    assert!(tot_cmp > 0, "no comparable plays — oracle/DB not populated");
}

/// Build sequence -> (home codes, vis codes) for `game_code`, scanning every
/// cached `playbyplay` response and keeping only plays whose `game.code` matches
/// (the gamecode-runaway pages contain other games too).
async fn build_oracle(
    pool: &sqlx::PgPool,
    game_code: &str,
) -> HashMap<String, (Vec<String>, Vec<String>)> {
    let rows =
        sqlx::query("SELECT response_body FROM api_cache WHERE endpoint LIKE 'mbb/playbyplay%'")
            .fetch_all(pool)
            .await
            .unwrap();

    let mut map: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    for r in rows {
        let body: Value = r.get("response_body");
        let Some(plays) = body.get("playbyplay").and_then(|v| v.as_object()) else {
            continue;
        };
        for play in plays.values() {
            let g = match play.get("game") {
                Some(g) => g,
                None => continue,
            };
            if g.get("code").and_then(Value::as_str) != Some(game_code) {
                continue;
            }
            let Some(seq) = g.get("sequence").and_then(Value::as_str) else {
                continue;
            };
            let split = |k: &str| -> Vec<String> {
                g.get(k)
                    .and_then(Value::as_str)
                    .map(|s| {
                        s.split(',')
                            .filter(|x| !x.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            map.entry(seq.to_string())
                .or_insert_with(|| (split("onfloorhome"), split("onfloorvis")));
        }
    }
    map
}

/// natstat player code -> our `players.id` for the season.
async fn code_uuid_map(
    pool: &sqlx::PgPool,
    season: i32,
    codes: &HashSet<String>,
) -> HashMap<String, Uuid> {
    let codes: Vec<String> = codes.iter().cloned().collect();
    let rows = sqlx::query(
        "SELECT natstat_id, id FROM players WHERE season = $1 AND natstat_id = ANY($2)",
    )
    .bind(season)
    .bind(&codes)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|r| (r.get::<String, _>("natstat_id"), r.get::<Uuid, _>("id")))
        .collect()
}
