use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::inference::Predictor;
use cstat_core::roster_features::{
    PlayerRow, build_roster_features, fetch_roster, normalize_rotation, swap_player,
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/transfers/{year}", get(transfer_list))
}

/// One row pulled from the `transfers` table. Schema in `migrations/019_transfers.sql`.
/// Pre-PR #51 this struct came from the scraped JSON; the field-set is now
/// driven by the DB columns we want to surface, but the response shape it
/// feeds into (see `EnrichedTransfer`) is unchanged save for `rank_247` going
/// nullable (the JSON-era top-N scrape was always ranked; the full DB
/// includes the unranked tail).
#[derive(sqlx::FromRow)]
struct TransferRow {
    /// 247's within-portal rank (their `transferRank` field), not the
    /// composite cross-class rank. The pre-DB embedded JSON was a scrape of
    /// transferRank, so this column is what gives bit-for-bit parity with the
    /// old `rank_247` values. ~340 of 1497 rows carry one; the rest are the
    /// unranked tail.
    transfer_rank: Option<i32>,
    full_name: String,
    position: Option<String>,
    height: Option<String>,
    weight: Option<i32>,
    status: String,
    rating: Option<f32>,
    source_institution: Option<String>,
    destination_institution: Option<String>,
    player_profile_url: Option<String>,
}

/// Enriched row returned to the frontend — base 247 fields plus the cstat
/// player match (if any) and CamPom value.
#[derive(Serialize)]
struct EnrichedTransfer {
    rank_247: Option<i32>,
    name: String,
    player_id: Option<Uuid>,
    position: String,
    height: Option<String>,
    weight: Option<i32>,
    status: String,
    rating_247: Option<f32>,
    previous_team: Option<String>,
    previous_team_full: Option<String>,
    previous_team_id: Option<Uuid>,
    next_team: Option<String>,
    next_team_id: Option<Uuid>,
    primary_class: Option<String>,
    secondary_class: Option<String>,
    campom: Option<f64>,
    campom_pct: Option<f64>,
    minutes_per_game: Option<f64>,
    games_played: Option<i32>,
    url_247: Option<String>,
    /// Projected ΔAdjEM for adding this player to the destination's
    /// prior-season roster: `swap_pred − baseline_pred` over the
    /// rank-slot swap engine. `null` when we can't compute it — no
    /// resolved cstat player, no committed destination, no prior-season
    /// stats for the player (freshmen), no prior-season roster for the
    /// destination team, or no CamPom v3 to rank them by. See
    /// `roster_features::swap_player` for the projection methodology
    /// and absolute-AdjEM honesty caveats.
    delta_adjem: Option<f32>,
}

/// One DB candidate row pulled by name match. We may have several per name
/// (common name, transfers within season) and disambiguate by previous team.
/// We carry both the Torvik short_name (`team_name`, used for display) and
/// the full NatStat name (`team_full_name`, used for alias matching against
/// 247 prev_team strings like "NC State" → "North Carolina State Wolfpack").
#[derive(sqlx::FromRow)]
struct DbCandidate {
    player_id: Uuid,
    name: String,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    team_full_name: Option<String>,
    minutes_per_game: Option<f64>,
    games_played: Option<i32>,
    campom: Option<f64>,
    campom_pct: Option<f64>,
    primary_class: Option<String>,
    secondary_class: Option<String>,
}

/// Subset of a row from the `teams` table — just enough to map a 247 short
/// name to a cstat team_id for the previous/next team links.
#[derive(sqlx::FromRow)]
struct DbTeam {
    id: Uuid,
    name: String,
    short_name: Option<String>,
}

async fn transfer_list(
    State(state): State<Arc<AppState>>,
    Path(year): Path<i32>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !(2000..=2100).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "year out of range" })),
        ));
    }

    // Pull every row for the requested portal class year. Ranked rows first
    // (so the response is rank-ordered like the old top-N JSON), then the
    // unranked tail by name. Frontend re-sorts by CamPom, so this ordering
    // only matters for parity with the embedded-JSON era.
    let transfers: Vec<TransferRow> = sqlx::query_as::<_, TransferRow>(
        r#"
        SELECT
            transfer_rank,
            full_name,
            position,
            height,
            weight,
            status,
            rating,
            source_institution,
            destination_institution,
            player_profile_url
        FROM transfers
        WHERE year = $1
        ORDER BY transfer_rank NULLS LAST, full_name
        "#,
    )
    .bind(year)
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("transfers query failed: {e}") })),
        )
    })?;

    if transfers.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("no transfers data for year {year}"),
            })),
        ));
    }

    // Pull every season player so we can match in Rust against our normalized
    // 247-side names. The DB stores names with mixed punctuation/suffixes
    // ("Freddie Dilione V", "A'lahn Sumler") that don't survive a strict SQL
    // `lower(name) = ANY(...)` comparison; doing the matching in Rust lets
    // both sides go through the same normalize() function. ~5K rows ≈ 1MB,
    // small enough that we don't need an index-friendly join here.
    let candidates: Vec<DbCandidate> = sqlx::query_as::<_, DbCandidate>(
        r#"
        SELECT
            p.id                     AS player_id,
            p.name                   AS name,
            t.id                     AS team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            t.name                   AS team_full_name,
            pss.minutes_per_game     AS minutes_per_game,
            pss.games_played         AS games_played,
            tps.cam_gbpm_v3_psos     AS campom,
            tps.cam_gbpm_v3_psos_pct AS campom_pct,
            pa.primary_class         AS primary_class,
            pa.secondary_class       AS secondary_class
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN teams t ON t.id = pss.team_id AND t.season = pss.season
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = p.id AND tps.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = p.id AND pa.season = pss.season
        WHERE pss.season = $1
        "#,
    )
    .bind(year)
    .fetch_all(&state.db.pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("candidates query failed: {e}") })),
        )
    })?;

    // Pull every team for the season so we can resolve 247 short names
    // (e.g. "Kansas") to a cstat team_id for the previous/next team links.
    let teams: Vec<DbTeam> =
        sqlx::query_as::<_, DbTeam>(r#"SELECT id, name, short_name FROM teams WHERE season = $1"#)
            .bind(year)
            .fetch_all(&state.db.pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("teams query failed: {e}") })),
                )
            })?;

    // Resolve a 247 short name ("Kansas", "UConn") to the team_id whose full
    // name (e.g. "Kansas Jayhawks") matches via the same prefix/alias logic
    // used to disambiguate player matches. Multiple teams can prefix-match
    // (e.g. "Miami" hits both Miami (Fla.) and Miami (Ohio)); the score
    // tiebreaker prefers an alias hit over a blind prefix hit so we land on
    // the canonical school.
    let resolve_team_id = |short: &str| -> Option<Uuid> {
        teams
            .iter()
            .filter_map(|t| {
                team_match_score(t.short_name.as_deref(), &t.name, short).map(|s| (s, t))
            })
            .min_by_key(|(s, _)| *s)
            .map(|(_, t)| t.id)
    };

    // Group candidates by normalized name for O(1) per-transfer lookup.
    let mut by_name: HashMap<String, Vec<DbCandidate>> = HashMap::new();
    for c in candidates {
        by_name.entry(normalize(&c.name)).or_default().push(c);
    }

    let mut enriched: Vec<EnrichedTransfer> = transfers
        .into_iter()
        .map(|t| {
            let key = normalize(&t.full_name);
            let pool = by_name.get(&key);
            let best: Option<&DbCandidate> = pool.and_then(|cands| {
                // Prefer the candidate whose team matches the 247 previous_team.
                t.source_institution
                    .as_deref()
                    .and_then(|prev| {
                        cands.iter().find(|c| {
                            team_matches(c.team_name.as_deref(), c.team_full_name.as_deref(), prev)
                        })
                    })
                    // Fallback: most-played candidate (handles name collisions).
                    .or_else(|| {
                        cands.iter().max_by(|a, b| {
                            a.minutes_per_game
                                .unwrap_or(0.0)
                                .partial_cmp(&b.minutes_per_game.unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                    })
            });

            // Prefer the cstat team_id we already linked the player to;
            // fall back to short-name lookup so unmatched players still get
            // a clickable previous-team link.
            let previous_team_id = best
                .and_then(|c| c.team_id)
                .or_else(|| t.source_institution.as_deref().and_then(resolve_team_id));
            let next_team_id = t
                .destination_institution
                .as_deref()
                .and_then(resolve_team_id);

            EnrichedTransfer {
                rank_247: t.transfer_rank,
                name: t.full_name,
                player_id: best.map(|c| c.player_id),
                position: t.position.unwrap_or_default(),
                height: t.height,
                weight: t.weight,
                status: t.status,
                rating_247: t.rating,
                previous_team: t.source_institution,
                previous_team_full: best.and_then(|c| c.team_name.clone()),
                previous_team_id,
                next_team: t.destination_institution,
                next_team_id,
                primary_class: best.and_then(|c| c.primary_class.clone()),
                secondary_class: best.and_then(|c| c.secondary_class.clone()),
                campom: best.and_then(|c| c.campom),
                campom_pct: best.and_then(|c| c.campom_pct),
                minutes_per_game: best.and_then(|c| c.minutes_per_game),
                games_played: best.and_then(|c| c.games_played),
                url_247: t.player_profile_url,
                delta_adjem: None,
            }
        })
        .collect();

    // Δ pipeline: project each transfer's value-add at their committed
    // destination by passing baseline + swap features through the same
    // rotation-normalization, so the Δ reflects only the incoming player
    // (not a meritocratic-rotation artifact). Failure modes set
    // `delta_adjem = None` per row rather than failing the response —
    // partial coverage is the expected state for years that lack
    // prior-season data (2024 portal has no 2023 cstat data to project
    // against, so every 2024 Δ is null by construction).
    compute_deltas(&state.db.pool, &state.predictor, year, &mut enriched).await;

    Ok(Json(json!({
        "year": year,
        "transfers": enriched,
        "total": enriched.len(),
    })))
}

/// Normalize a player name for matching: lowercase, drop accents/punctuation,
/// strip generational suffixes. We match on first+last name only — the 247
/// scrape and our DB sometimes differ on hyphenation and middle names.
fn normalize(name: &str) -> String {
    let folded: String = name
        .chars()
        .flat_map(|c| {
            // Cheap accent fold for the diacritics we actually see.
            match c {
                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => {
                    Some('a')
                }
                'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => Some('e'),
                'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => Some('i'),
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => Some('o'),
                'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => Some('u'),
                'ñ' | 'Ñ' => Some('n'),
                'ç' | 'Ç' => Some('c'),
                _ if c.is_alphabetic() || c.is_whitespace() => Some(c.to_ascii_lowercase()),
                _ => None,
            }
        })
        .collect();
    folded
        .split_whitespace()
        // "lll" appears in our DB for "Ace Glass III" (typo, three lowercase
        // L's instead of three capital I's); strip it like a generational
        // suffix so the 247 entry still matches.
        .filter(|w| !matches!(*w, "jr" | "sr" | "ii" | "iii" | "iv" | "v" | "lll"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 247 short name → cstat team-name prefix that should appear at the start of
/// `teams.name`. Listed only for cases the bare prefix branch can't catch
/// (acronyms like "UConn" don't prefix "Connecticut Huskies"), or to nudge
/// ambiguous prefix matches toward the canonical school (bare "Miami" should
/// resolve to Miami (Fla.), not Miami (Ohio)). Add entries here as we spot
/// misses.
const TEAM_ALIASES: &[(&str, &str)] = &[
    ("uconn", "connecticut"),
    ("ole miss", "mississippi"),
    ("usc", "southern california"),
    ("nc state", "north carolina state"),
    // Bare "Miami" prefix-matches both Florida and Ohio — anchor it to FL.
    ("miami", "miami (fla.)"),
    ("miami (fl)", "miami (fla.)"),
    ("miami (oh)", "miami (ohio)"),
];

/// Score how well a cstat team matches a 247 short name. Lower is better;
/// `None` means no match. Tries the Torvik-style `short_name` first (which
/// usually matches 247 directly, e.g. "Kansas" == "Kansas") and falls back
/// to the full NatStat name with alias/prefix logic for legacy edge cases.
fn team_match_score(db_short: Option<&str>, db_full: &str, short: &str) -> Option<u32> {
    let short_lc = short.to_lowercase();
    // 0 = exact short_name match. The common case now that teams.short_name is
    // populated with Torvik names — "Kansas", "UConn", "Duke" all resolve here.
    if let Some(s) = db_short
        && s.to_lowercase() == short_lc
    {
        return Some(0);
    }
    let db_lc = db_full.to_lowercase();
    if db_lc == short_lc {
        return Some(0);
    }
    // 1 = alias hit against the full name. Kept for 247-side aliases that
    // don't equal the short_name (e.g. "miami" → "Miami FL"; "ole miss" →
    // "Mississippi"; ambiguous bare names like "Miami").
    for (k, v) in TEAM_ALIASES {
        if short_lc == *k && (db_lc == *v || db_lc.starts_with(&format!("{v} "))) {
            return Some(1);
        }
    }
    // 2 = bare prefix match against the full name. Catches the case where
    // short_name is missing — falls back to old behavior.
    if db_lc.starts_with(&format!("{short_lc} ")) {
        return Some(2);
    }
    None
}

/// Boolean wrapper around `team_match_score`, kept for callers that don't
/// need the score (the player-disambiguation pass). Takes both the Torvik
/// short_name and the full NatStat name so alias entries that target the
/// full form (e.g. "nc state" → "north carolina state") still fire.
fn team_matches(db_short: Option<&str>, db_full: Option<&str>, short_name: &str) -> bool {
    db_full
        .map(|full| team_match_score(db_short, full, short_name).is_some())
        .unwrap_or(false)
}

// ─── Δ AdjEM pipeline ───────────────────────────────────────────────────────
//
// Projects each portal entry's value-add at their committed destination by
// running the destination's *prior-season* roster through baseline + swap
// predictions and reporting the difference. Prior-season is the cleanest
// framing: the destination's roster is fully known and the player wasn't on
// it yet, so the answer to "what would have happened if you'd added this
// player" doesn't suffer from forward-looking data leakage. Year-1 data is
// also a stable evaluation surface; current-season would shift under
// in-progress ingestion.
//
// Cross-season UUID resolution uses `torvik_pid` (stable across team
// changes per memory) for players and `teams.natstat_id` (unique per
// (season, natstat_id)) for teams. Players who weren't ingested at year-1
// (freshmen, non-D-I transfers) produce no resolved prior-season row and
// land with `delta_adjem = null`.

/// Helper struct for the prior-season PlayerRow batch fetch. Carries the
/// CURRENT-season player_id so the caller can rebuild a
/// (current_id → prior PlayerRow) map; everything else mirrors
/// `roster_features::PlayerRow` field-for-field.
#[derive(sqlx::FromRow)]
struct PriorPlayerRow {
    current_player_id: Uuid,
    player_id: Uuid,
    total_min: f64,
    mpg: f64,
    ppg: Option<f64>,
    rpg: Option<f64>,
    apg: Option<f64>,
    spg: Option<f64>,
    bpg: Option<f64>,
    topg: Option<f64>,
    ts: Option<f64>,
    efg: Option<f64>,
    usg: Option<f64>,
    ast_pct: Option<f64>,
    tov_pct: Option<f64>,
    orb_pct: Option<f64>,
    drb_pct: Option<f64>,
    stl_pct: Option<f64>,
    blk_pct: Option<f64>,
    ft_rate: Option<f64>,
    primary_class: Option<String>,
    cam_v3: Option<f64>,
}

impl PriorPlayerRow {
    fn into_player_row(self) -> PlayerRow {
        PlayerRow {
            player_id: self.player_id,
            total_min: self.total_min,
            mpg: self.mpg,
            ppg: self.ppg,
            rpg: self.rpg,
            apg: self.apg,
            spg: self.spg,
            bpg: self.bpg,
            topg: self.topg,
            ts: self.ts,
            efg: self.efg,
            usg: self.usg,
            ast_pct: self.ast_pct,
            tov_pct: self.tov_pct,
            orb_pct: self.orb_pct,
            drb_pct: self.drb_pct,
            stl_pct: self.stl_pct,
            blk_pct: self.blk_pct,
            ft_rate: self.ft_rate,
            primary_class: self.primary_class,
            cam_v3: self.cam_v3,
        }
    }
}

/// Resolve current-season player UUIDs → prior-season `PlayerRow`s via
/// `torvik_pid` (stable cross-season identity per memory; `natstat_id`
/// breaks on team changes). Honors the same qualification gate as
/// `roster_features::fetch_roster` so train/serve features stay aligned.
async fn fetch_prior_player_rows(
    pool: &PgPool,
    current_player_ids: &[Uuid],
    current_season: i32,
) -> Result<HashMap<Uuid, PlayerRow>, sqlx::Error> {
    if current_player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<PriorPlayerRow> = sqlx::query_as::<_, PriorPlayerRow>(
        r#"
        SELECT
            curr.player_id           AS current_player_id,
            pss.player_id,
            (COALESCE(pss.minutes_per_game, 0) * COALESCE(pss.games_played, 0))::float8 AS total_min,
            COALESCE(pss.minutes_per_game, 0)::float8 AS mpg,
            pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
            pss.true_shooting_pct AS ts,
            pss.effective_fg_pct  AS efg,
            pss.usage_rate        AS usg,
            pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
            pss.stl_pct, pss.blk_pct, pss.ft_rate,
            pa.primary_class,
            prior.cam_gbpm_v3_psos AS cam_v3
        FROM torvik_player_stats curr
        JOIN torvik_player_stats prior
            ON prior.torvik_pid = curr.torvik_pid
            AND prior.season = $2
        JOIN player_season_stats pss
            ON pss.player_id = prior.player_id
            AND pss.season = $2
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id
            AND pa.season = $2
        WHERE curr.player_id = ANY($1)
          AND curr.season = $3
          AND COALESCE(pss.games_played, 0) >= 5
          AND COALESCE(pss.minutes_per_game, 0) >= 5
        "#,
    )
    .bind(current_player_ids)
    .bind(current_season - 1)
    .bind(current_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.current_player_id, r.into_player_row()))
        .collect())
}

/// Map current-season team UUIDs → prior-season team UUIDs via the cross-
/// season-stable `teams.natstat_id` (UNIQUE on `(natstat_id, season)` per
/// migration 001). Used to look up the destination's prior-season roster
/// from the current-season `next_team_id` we already have.
async fn resolve_prior_team_ids(
    pool: &PgPool,
    current_team_ids: &[Uuid],
    current_season: i32,
) -> Result<HashMap<Uuid, Uuid>, sqlx::Error> {
    if current_team_ids.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        current_id: Uuid,
        prior_id: Uuid,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT t_current.id AS current_id, t_prior.id AS prior_id
        FROM teams t_current
        JOIN teams t_prior
            ON t_prior.natstat_id = t_current.natstat_id
            AND t_prior.season = $2
        WHERE t_current.id = ANY($1) AND t_current.season = $3
        "#,
    )
    .bind(current_team_ids)
    .bind(current_season - 1)
    .bind(current_season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.current_id, r.prior_id))
        .collect())
}

/// Compute `delta_adjem` for every eligible transfer and patch it into
/// `enriched` in place. Eligible = has `player_id` (resolved cstat match)
/// AND `next_team_id` (committed destination). Within the eligible set
/// each row may still produce `None` if any of: prior-season player row
/// missing (freshman, non-D-I source), prior-season destination team
/// missing, prior-season roster empty (rare; small school under the
/// qualification gate), or incoming player has no CamPom v3 to rank by.
///
/// Single round-trip cost: two batch queries (prior players + prior
/// teams), one roster fetch per unique destination, two ONNX inferences
/// per eligible row (baseline cached per destination, swap unique per
/// row). For a 1500-row portal with ~300 unique destinations, that's
/// ~300 baseline + ~1500 swap predictions, all sub-millisecond.
///
/// Errors during the pipeline (DB failure, ONNX error) log and leave the
/// affected rows at `None` rather than failing the response — partial
/// coverage is the documented contract.
async fn compute_deltas(
    pool: &PgPool,
    predictor: &Predictor,
    year: i32,
    enriched: &mut [EnrichedTransfer],
) {
    // Step 1: collect eligible rows by index.
    let eligible: Vec<(usize, Uuid, Uuid)> = enriched
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let pid = t.player_id?;
            let dest = t.next_team_id?;
            Some((i, pid, dest))
        })
        .collect();
    if eligible.is_empty() {
        return;
    }

    // Step 2: batch-resolve prior-season players + teams. Dedup before
    // hitting the DB so re-entrants (same player listed twice) and
    // popular destinations (many transfers committed to the same school)
    // don't pay the round-trip more than once.
    let mut player_ids: Vec<Uuid> = eligible.iter().map(|(_, p, _)| *p).collect();
    player_ids.sort();
    player_ids.dedup();
    let mut team_ids: Vec<Uuid> = eligible.iter().map(|(_, _, t)| *t).collect();
    team_ids.sort();
    team_ids.dedup();

    let prior_players = match fetch_prior_player_rows(pool, &player_ids, year).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "delta_adjem: prior-player fetch failed; leaving deltas null");
            return;
        }
    };
    let prior_teams = match resolve_prior_team_ids(pool, &team_ids, year).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "delta_adjem: prior-team resolution failed; leaving deltas null");
            return;
        }
    };

    // Step 3: fetch prior-season rosters per unique destination. Memoize
    // so popular destinations only hit the DB once and only get
    // normalize_rotation'd + baseline-predicted once.
    let mut roster_cache: HashMap<Uuid, Vec<PlayerRow>> = HashMap::new();
    let unique_prior_dests: Vec<Uuid> = {
        let mut v: Vec<Uuid> = prior_teams.values().copied().collect();
        v.sort();
        v.dedup();
        v
    };
    for prior_dest in unique_prior_dests {
        match fetch_roster(pool, prior_dest, year - 1).await {
            Ok(roster) => {
                roster_cache.insert(prior_dest, roster);
            }
            Err(e) => {
                tracing::warn!(team_id = %prior_dest, error = %e, "delta_adjem: roster fetch failed");
            }
        }
    }

    // Step 4: precompute baseline AdjEM per destination over the
    // *normalized* roster (the symmetric-normalization contract that
    // keeps Δ honest — see `roster_features::normalize_rotation` doc).
    let mut baseline_cache: HashMap<Uuid, f32> = HashMap::new();
    for (team_id, roster) in &roster_cache {
        if roster.is_empty() {
            continue;
        }
        let normalized = normalize_rotation(roster.clone());
        let feats = build_roster_features(&normalized);
        match predictor.predict_adj_em(&feats) {
            Ok(p) => {
                baseline_cache.insert(*team_id, p);
            }
            Err(e) => {
                tracing::warn!(team_id = %team_id, error = ?e, "delta_adjem: baseline predict failed");
            }
        }
    }

    // Step 5: for each eligible row, compose normalized baseline + swap
    // and patch in the delta.
    for (i, current_pid, current_dest_id) in eligible {
        let Some(&prior_dest_id) = prior_teams.get(&current_dest_id) else {
            continue;
        };
        let Some(roster) = roster_cache.get(&prior_dest_id) else {
            continue;
        };
        let Some(&baseline_pred) = baseline_cache.get(&prior_dest_id) else {
            continue;
        };
        let Some(incoming) = prior_players.get(&current_pid) else {
            continue;
        };
        // Without cam_v3 the rank-slot logic sinks the incoming player
        // to the bottom of the rotation → 0 mpg → near-zero Δ that's an
        // artifact of missing data, not a real prediction. Surface as
        // null to keep the column honest. See `swap_player` doc.
        if incoming.cam_v3.is_none() {
            continue;
        }
        let normalized = normalize_rotation(roster.clone());
        let swapped = swap_player(&normalized, incoming.clone());
        let feats = build_roster_features(&swapped);
        match predictor.predict_adj_em(&feats) {
            Ok(swap_pred) => {
                enriched[i].delta_adjem = Some(swap_pred - baseline_pred);
            }
            Err(e) => {
                tracing::warn!(player_id = %current_pid, error = ?e, "delta_adjem: swap predict failed");
            }
        }
    }
}
