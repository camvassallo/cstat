use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/transfers/{year}", get(transfer_list))
}

/// Compile-time-embedded transfer rosters keyed by portal year. We ship
/// these inside the binary so the deployed container doesn't need a `data/`
/// dir at runtime; local dev can still override by writing to
/// `TRANSFERS_DIR` (defaults to `data/transfers`) for fast iteration.
const EMBEDDED_TRANSFERS: &[(i32, &str)] =
    &[(2026, include_str!("../../../../data/transfers/2026.json"))];

/// Raw row from the scraped 247Sports JSON.
#[derive(Deserialize)]
struct Transfer247 {
    rank: i32,
    name: String,
    #[serde(default)]
    position: String,
    #[serde(default)]
    height: Option<String>,
    #[serde(default)]
    weight: Option<i32>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    rating_247: Option<f64>,
    #[serde(default)]
    previous_team: Option<String>,
    #[serde(default)]
    next_team: Option<String>,
    #[serde(default)]
    url_247: Option<String>,
}

/// Enriched row returned to the frontend — base 247 fields plus the cstat
/// player match (if any) and CamPom value.
#[derive(Serialize)]
struct EnrichedTransfer {
    rank_247: i32,
    name: String,
    player_id: Option<Uuid>,
    position: String,
    height: Option<String>,
    weight: Option<i32>,
    status: String,
    rating_247: Option<f64>,
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
}

/// One DB candidate row pulled by name match. We may have several per name
/// (common name, transfers within season) and disambiguate by previous team.
#[derive(sqlx::FromRow)]
struct DbCandidate {
    player_id: Uuid,
    name: String,
    team_id: Option<Uuid>,
    team_name: Option<String>,
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

    // Prefer an on-disk override (set TRANSFERS_DIR for local dev so changes
    // to the JSON take effect without a rebuild). Fall back to the binary's
    // embedded copy so prod containers don't need a `data/` dir.
    let dir = std::env::var("TRANSFERS_DIR").unwrap_or_else(|_| "data/transfers".into());
    let path = format!("{dir}/{year}.json");
    let raw: std::borrow::Cow<'_, [u8]> = match tokio::fs::read(&path).await {
        Ok(b) => std::borrow::Cow::Owned(b),
        Err(_) => match EMBEDDED_TRANSFERS.iter().find(|(y, _)| *y == year) {
            Some((_, s)) => std::borrow::Cow::Borrowed(s.as_bytes()),
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "error": format!("no transfers data for year {year}"),
                    })),
                ));
            }
        },
    };
    let transfers: Vec<Transfer247> = serde_json::from_slice(&raw).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("transfers file invalid: {e}") })),
        )
    })?;

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
            t.name                   AS team_name,
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
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    // Pull every team for the season so we can resolve 247 short names
    // (e.g. "Kansas") to a cstat team_id for the previous/next team links.
    let teams: Vec<DbTeam> =
        sqlx::query_as::<_, DbTeam>(r#"SELECT id, name FROM teams WHERE season = $1"#)
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
            .filter_map(|t| team_match_score(&t.name, short).map(|s| (s, t)))
            .min_by_key(|(s, _)| *s)
            .map(|(_, t)| t.id)
    };

    // Group candidates by normalized name for O(1) per-transfer lookup.
    let mut by_name: HashMap<String, Vec<DbCandidate>> = HashMap::new();
    for c in candidates {
        by_name.entry(normalize(&c.name)).or_default().push(c);
    }

    let enriched: Vec<EnrichedTransfer> = transfers
        .into_iter()
        .map(|t| {
            let key = normalize(&t.name);
            let pool = by_name.get(&key);
            let best: Option<&DbCandidate> = pool.and_then(|cands| {
                // Prefer the candidate whose team matches the 247 previous_team.
                t.previous_team
                    .as_deref()
                    .and_then(|prev| {
                        cands
                            .iter()
                            .find(|c| team_matches(c.team_name.as_deref(), prev))
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
                .or_else(|| t.previous_team.as_deref().and_then(resolve_team_id));
            let next_team_id = t.next_team.as_deref().and_then(resolve_team_id);

            EnrichedTransfer {
                rank_247: t.rank,
                name: t.name,
                player_id: best.map(|c| c.player_id),
                position: t.position,
                height: t.height,
                weight: t.weight,
                status: t.status,
                rating_247: t.rating_247,
                previous_team: t.previous_team,
                previous_team_full: best.and_then(|c| c.team_name.clone()),
                previous_team_id,
                next_team: t.next_team,
                next_team_id,
                primary_class: best.and_then(|c| c.primary_class.clone()),
                secondary_class: best.and_then(|c| c.secondary_class.clone()),
                campom: best.and_then(|c| c.campom),
                campom_pct: best.and_then(|c| c.campom_pct),
                minutes_per_game: best.and_then(|c| c.minutes_per_game),
                games_played: best.and_then(|c| c.games_played),
                url_247: t.url_247,
            }
        })
        .collect();

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

/// Score how well a cstat full team name matches a 247 short name. Lower is
/// better; `None` means no match. Used both as a boolean test (via
/// `team_matches`) and as a tiebreaker when several teams prefix-match the
/// same short name (e.g. "Miami").
fn team_match_score(db: &str, short: &str) -> Option<u32> {
    let db_lc = db.to_lowercase();
    let short_lc = short.to_lowercase();
    // 0 = exact match (rare, but the most specific signal).
    if db_lc == short_lc {
        return Some(0);
    }
    // 1 = alias hit. Beats a blind prefix match so ambiguous bare names
    // ("Miami") resolve to the school the alias points at.
    for (k, v) in TEAM_ALIASES {
        if short_lc == *k && (db_lc == *v || db_lc.starts_with(&format!("{v} "))) {
            return Some(1);
        }
    }
    // 2 = bare prefix match. Catches the common case ("Kansas" → "Kansas
    // Jayhawks") while still rejecting "Arkansas" → "Arkansas State".
    if db_lc.starts_with(&format!("{short_lc} ")) {
        return Some(2);
    }
    None
}

/// Boolean wrapper around `team_match_score`, kept for callers that don't
/// need the score (the player-disambiguation pass).
fn team_matches(db_name: Option<&str>, short_name: &str) -> bool {
    db_name
        .map(|db| team_match_score(db, short_name).is_some())
        .unwrap_or(false)
}
