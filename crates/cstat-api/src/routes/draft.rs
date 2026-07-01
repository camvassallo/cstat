use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::roster_projection::normalize_player_name as normalize;
use cstat_core::team_name_match::{team_match_score, team_matches};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/draft/{year}", get(draft_board))
}

/// One curated prospect from `data/draft/{year}_big_board.json`. Schema is
/// documented in ROADMAP.md (Phase 5b "NBA Draft Big Board"). Historical years
/// are actual draft results (`scripts/build_historical_draft_boards.py`); the
/// live year starts as a Tankathon prospect board. We model only the fields the
/// `/draft` page consumes — `rank` / `name` / `current_team`; every other key
/// (`position`, `class_year`, `tier`, `height`, `weight`, `age`, `stats`,
/// `source`, `as_of`) is left as an unknown key for serde to ignore.
#[derive(Debug, Deserialize)]
struct BoardEntry {
    /// Tankathon draft rank. `None` for the alphabetical "unranked" tail
    /// (players Tankathon lists without a number).
    rank: Option<i32>,
    name: String,
    /// School / team name as Tankathon writes it ("Duke", "Kansas", "BYU").
    current_team: String,
}

/// A cstat player row pulled by season for name-matching against the board.
/// We carry both the Torvik short name (for display) and the full NatStat
/// name (for alias matching against the board's school strings).
/// `minutes_per_game` is the name-collision tiebreaker, not a response field.
#[derive(sqlx::FromRow)]
struct DbCandidate {
    player_id: Uuid,
    name: String,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    team_full_name: Option<String>,
    minutes_per_game: Option<f64>,
    campom: Option<f64>,
    campom_o: Option<f64>,
    campom_d: Option<f64>,
    /// D&D-class archetype for the season (primary / secondary), from
    /// `player_archetypes`. `None` when the player didn't cluster this season.
    primary_archetype: Option<String>,
    secondary_archetype: Option<String>,
}

/// Subset of a `teams` row — enough to resolve a board school name to a
/// cstat team_id for the team link.
#[derive(sqlx::FromRow)]
struct DbTeam {
    id: Uuid,
    name: String,
    short_name: Option<String>,
}

/// One prospect returned to the frontend — the board rank/name/team plus the
/// cstat player match (if any): CamPom value + its O/D halves and the player's
/// D&D-class archetypes.
#[derive(Serialize)]
struct Prospect {
    /// Tankathon draft rank (`None` for the unranked tail).
    draft_rank: Option<i32>,
    name: String,
    /// Board school name (verbatim from Tankathon).
    current_team: String,
    /// Resolved cstat team — from the matched player's team, or a school-name
    /// lookup so the team link works even for unmatched prospects.
    team_id: Option<Uuid>,
    team_name: Option<String>,
    /// cstat player match. `None` for prospects with no college row this
    /// season — internationals, G-Leaguers, the odd name we couldn't match.
    player_id: Option<Uuid>,
    /// CamPom v3 (`cam_gbpm_v3_psos`) for the matched player. `None` when
    /// unmatched.
    campom: Option<f64>,
    /// CamPom O/D halves (`cam_{o,d}_gbpm_v3_psos`; o + d = campom, d
    /// positive-good). `None` when unmatched or where the decomposition is
    /// numerically unstable (±30 sanity envelope, gated server-side).
    campom_o: Option<f64>,
    campom_d: Option<f64>,
    /// Primary / secondary D&D-class archetype for the matched player's season
    /// (`player_archetypes`). `None` when unmatched or unclustered.
    primary_archetype: Option<String>,
    secondary_archetype: Option<String>,
}


async fn draft_board(
    State(state): State<Arc<AppState>>,
    Path(year): Path<i32>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !(2000..=2100).contains(&year) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "year out of range" })),
        ));
    }

    // The curated big board. A missing file just means we have no board for
    // this draft cycle yet — a clean 404, not a server error.
    let board_path = PathBuf::from("data/draft").join(format!("{year}_big_board.json"));
    let board_raw = std::fs::read_to_string(&board_path).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no draft board for year {year}") })),
        )
    })?;
    let board: Vec<BoardEntry> = serde_json::from_str(&board_raw).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("draft board parse failed: {e}") })),
        )
    })?;

    // Every season player, so we can name-match the board against cstat in
    // Rust through the same normalize() both sides go through. ~5K rows is
    // small enough not to need an index-friendly join. Mirrors transfers.rs.
    let candidates: Vec<DbCandidate> = sqlx::query_as::<_, DbCandidate>(
        r#"
        SELECT
            p.id                     AS player_id,
            p.name                   AS name,
            t.id                     AS team_id,
            COALESCE(t.short_name, t.name) AS team_name,
            t.name                   AS team_full_name,
            pss.minutes_per_game     AS minutes_per_game,
            tps.cam_gbpm_v3_psos     AS campom,
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_o_gbpm_v3_psos END AS campom_o,
            CASE WHEN abs(tps.cam_o_gbpm_v3_psos) <= 30 AND abs(tps.cam_d_gbpm_v3_psos) <= 30
                 THEN tps.cam_d_gbpm_v3_psos END AS campom_d,
            pa.primary_class         AS primary_archetype,
            pa.secondary_class       AS secondary_archetype
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

    // Every team for the season, to resolve a board school name to a cstat
    // team_id for the team link.
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

    // Resolve a board school name ("Duke") to the team_id whose full name
    // ("Duke Blue Devils") matches via the shared prefix/alias logic. The
    // score tiebreaker prefers an alias hit over a blind prefix hit.
    let resolve_team_id = |short: &str| -> Option<Uuid> {
        teams
            .iter()
            .filter_map(|t| {
                team_match_score(t.short_name.as_deref(), &t.name, short).map(|s| (s, t))
            })
            .min_by_key(|(s, _)| *s)
            .map(|(_, t)| t.id)
    };

    // Group candidates by normalized name for O(1) per-prospect lookup.
    let mut by_name: HashMap<String, Vec<DbCandidate>> = HashMap::new();
    for c in candidates {
        by_name.entry(normalize(&c.name)).or_default().push(c);
    }

    let prospects: Vec<Prospect> = board
        .into_iter()
        .map(|b| {
            let key = normalize(&b.name);
            let best: Option<&DbCandidate> = by_name.get(&key).and_then(|cands| {
                // Prefer the candidate whose team matches the board school.
                cands
                    .iter()
                    .find(|c| {
                        team_matches(
                            c.team_name.as_deref(),
                            c.team_full_name.as_deref(),
                            &b.current_team,
                        )
                    })
                    // Fallback: most-played candidate (handles name collisions).
                    .or_else(|| {
                        cands.iter().max_by(|a, c| {
                            a.minutes_per_game
                                .unwrap_or(0.0)
                                .partial_cmp(&c.minutes_per_game.unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                    })
            });

            // Prefer the cstat team_id we matched the player to; fall back to
            // a school-name lookup so unmatched prospects still get a link.
            let team_id = best
                .and_then(|c| c.team_id)
                .or_else(|| resolve_team_id(&b.current_team));

            Prospect {
                draft_rank: b.rank,
                name: b.name,
                team_id,
                team_name: best.and_then(|c| c.team_name.clone()),
                current_team: b.current_team,
                player_id: best.map(|c| c.player_id),
                campom: best.and_then(|c| c.campom),
                campom_o: best.and_then(|c| c.campom_o),
                campom_d: best.and_then(|c| c.campom_d),
                primary_archetype: best.and_then(|c| c.primary_archetype.clone()),
                secondary_archetype: best.and_then(|c| c.secondary_archetype.clone()),
            }
        })
        .collect();

    Ok(Json(json!({
        "year": year,
        "prospects": prospects,
        "total": prospects.len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_entry_deserializes_real_schema() {
        // Mirrors `data/draft/{year}_big_board.json`, including the keys we
        // deliberately don't model (`position` / `class_year` / `tier` /
        // `height` / `weight` / `age` / `stats` / `source` / `as_of`) — serde
        // must ignore them — and both a ranked and an unranked entry.
        let raw = r#"[
            { "rank": 1, "name": "Cameron Boozer", "current_team": "Duke",
              "position": "PF", "height": "6-9", "weight": 250,
              "class_year": "Freshman", "age": 18.9, "tier": "lottery",
              "stats": { "pts": 24.2, "reb": 11.0, "ast": 4.4, "blk": 0.7, "stl": 1.5 },
              "source": "tankathon", "as_of": "2026-05-10" },
            { "rank": null, "name": "Some Walkon", "current_team": "Whoever" }
        ]"#;
        let board: Vec<BoardEntry> = serde_json::from_str(raw).expect("board parses");
        assert_eq!(board.len(), 2);
        assert_eq!(board[0].rank, Some(1));
        assert_eq!(board[0].name, "Cameron Boozer");
        assert_eq!(board[0].current_team, "Duke");
        // The unranked tail: rank absent → None; unmodeled keys ignored.
        assert_eq!(board[1].rank, None);
        assert_eq!(board[1].name, "Some Walkon");
    }
}
