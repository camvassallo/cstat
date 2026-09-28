use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries::{self, PlayerSortField, SortOrder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/players", get(player_list))
        .route("/api/players/compare", get(player_compare))
        .route("/api/players/{id}", get(player_detail))
        .route("/api/players/{id}/archetype", get(player_archetype))
        .route("/api/players/{id}/similar", get(player_similar))
        .route("/api/players/{id}/progression", get(player_progression))
        .route("/api/players/{id}/pbp", get(player_pbp))
        .route("/api/players/{id}/on-off", get(player_on_off))
}

/// `GET /api/players/{id}/pbp` — the player's play-by-play season profile (shot
/// location, scoring context, fouls drawn, on-floor +/-). A dedicated route the
/// player page fetches in parallel; returns `{ "pbp": null }` (200) when the
/// player has no PBP for the season rather than a 404.
async fn player_pbp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let resolved = queries::resolve_player_id_for_season(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("resolve failed: {e}") })),
            )
        })?;
    let Some(rid) = resolved else {
        return Ok(Json(json!({ "season": season, "pbp": null })));
    };

    let pbp = queries::get_player_pbp_profile(pool, rid, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({ "season": season, "pbp": pbp })))
}

/// `GET /api/players/{id}/on-off` — the player's season on/off splits (team
/// off/def rating per 100 poss with vs without him on the floor). Fetched in
/// parallel with the player page; returns `{ "on_off": null }` (200) when the
/// player has no PBP-derived on/off row rather than a 404.
async fn player_on_off(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let resolved = queries::resolve_player_id_for_season(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("resolve failed: {e}") })),
            )
        })?;
    let Some(rid) = resolved else {
        return Ok(Json(json!({ "season": season, "on_off": null })));
    };

    let on_off = queries::get_player_on_off(pool, rid, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({ "season": season, "on_off": on_off })))
}

#[derive(Deserialize)]
struct PlayerListParams {
    search: Option<String>,
    team: Option<Uuid>,
    season: Option<i32>,
    sort: Option<PlayerSortField>,
    order: Option<SortOrder>,
    /// Filter to a single archetype class (e.g. "Wizard").
    archetype: Option<String>,
    /// When true and `archetype` is set, also match players whose
    /// `secondary_class` equals the filter — used by the drill-down's
    /// "primary or secondary" toggle.
    include_secondary_archetype: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

// Page size cap. The Players tab loads the entire qualified pool in one
// request (~2-3k players for a typical season) and paginates client-side via
// AG Grid; 5000 leaves headroom for that plus scripted callers without
// letting a single request scrape an entire database scan.
const PLAYER_LIST_MAX_LIMIT: i64 = 5000;

async fn player_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlayerListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.limit.unwrap_or(50).clamp(1, PLAYER_LIST_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0).max(0);
    let sort = params.sort.unwrap_or_default();
    let include_secondary = params.include_secondary_archetype.unwrap_or(false);

    let (players, total) = queries::search_players(
        &state.db.pool,
        params.search.as_deref(),
        params.team,
        season,
        sort,
        params.order,
        params.archetype.as_deref(),
        include_secondary,
        limit,
        offset,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    Ok(Json(json!({
        "season": season,
        "players": players,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

#[derive(Deserialize)]
struct PlayerDetailParams {
    season: Option<i32>,
}

/// Which regime produced a player page's projected next-season CAM.
///
/// An enum rather than bare `&str` literals for the same reason
/// `predict::PredictionBasis` is one: the value is a **contract with the
/// frontend**. `PlayerTrajectory.projection_basis` in `web/src/api/client.ts` is
/// a closed union and the chip's tooltip keys off it, so a basis the server can
/// emit but the client does not list renders a caveat-free tooltip — which is
/// the exact failure #401 was filed for, one layer down.
/// `every_trajectory_basis_is_in_the_frontend_union` below walks `Self::ALL`
/// against that union.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrajectoryBasis {
    /// A persisted leave-one-pair-out prediction from
    /// `trajectory_oof_predictions`. Honest held-out output, and the only basis
    /// that is not in-sample for a historical target season. Preferred whenever
    /// it exists, which is every season the trajectory model trained on.
    HeldOut,
    /// The materialized `player_season_projection` row — the destination-aware
    /// projection. Identical to what the projected `/players` board serves (it
    /// reads the same row), and equal to the team projections page's live
    /// number to the `REAL` column's precision, which is ~1e-5 CAM against a
    /// surface that displays one decimal.
    DestinationAware,
    /// Live inference with no destination, which the feature builder fills as
    /// "returns to the same program". The honest fallback for a player no
    /// composed roster carries — a departing senior, whose number is explicitly
    /// the "had he stayed" counterfactual — and the one basis whose tooltip has
    /// to say so.
    SameProgramAssumed,
}

impl TrajectoryBasis {
    /// Every variant, for the contract test. Must stay exhaustive; the `match`
    /// in [`Self::as_str`] is what forces a new variant to be considered here.
    #[cfg(test)]
    const ALL: &'static [Self] = &[
        Self::HeldOut,
        Self::DestinationAware,
        Self::SameProgramAssumed,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::HeldOut => "held_out",
            Self::DestinationAware => "destination_aware",
            Self::SameProgramAssumed => "same_program_assumed",
        }
    }
}

/// Resolve the projected next-season CAM band for one player, and say which
/// regime produced it.
///
/// Both player-facing surfaces call this — the single-season detail page and the
/// career-progression page. They each had their own copy of this precedence,
/// which is why #401 was a bug in two places at once and got fixed in one and
/// not the other; there is now one copy.
///
/// Precedence, and why it is this order:
///
/// 1. **`trajectory_oof_predictions`** — the held-out prediction, for any
///    (torvik_pid, target_season) pair the model trained on. First because it
///    is the only basis that is not in-sample for a historical season, and
///    because keeping it first leaves those players' numbers untouched by this
///    change. That is most of them but not all: 19-27% of a historical
///    season's players have no OOF row (no `torvik_pid` mapping, or a
///    transition the model did not train on), and their answer moves from
///    destination-blind inference to step 2, which is what the team page has
///    shown them all along.
/// 2. **`player_season_projection`** — the destination-aware row the roster
///    pipeline materializes. This is the fix: for the forward season it replaces
///    a destination-blind guess with the same number the team page serves.
/// 3. **live inference with `Destination = None`** — a player no composed roster
///    carries. Retained deliberately rather than dropped: a departing senior's
///    chip is a counterfactual, and a counterfactual is worth rendering as long
///    as it is labelled as one.
///
/// `row` is the caller's already-fetched [`TrajectoryPlayerRow`] (the detail
/// route gets it inside its `try_join!`), so this adds no round trip beyond the
/// lookups it actually needs.
async fn resolve_trajectory(
    state: &AppState,
    player_id: Uuid,
    base_season: i32,
    row: Option<cstat_core::trajectory::TrajectoryPlayerRow>,
) -> Option<Value> {
    let row = row?;
    // Gate: the model's most load-bearing feature. No prior CamPom, no
    // projection — for every basis, including the table read.
    row.campom?;
    let target_season = base_season + 1;
    let pool = &state.db.pool;

    let oof = cstat_core::trajectory::fetch_trajectory_oof(pool, &[player_id], target_season)
        .await
        .ok()
        .and_then(|m| m.get(&player_id).cloned());

    let (pred, basis) = if let Some(p) = oof {
        (p, TrajectoryBasis::HeldOut)
    } else {
        let materialized = cstat_core::trajectory::fetch_player_season_projection(
            pool,
            &[player_id],
            target_season,
        )
        .await
        .map_err(|e| {
            // Warn rather than fail the page: the projection is one chip on a
            // page full of actuals, and falling through still produces a
            // number — one that is now labelled as destination-blind, so a
            // silent degradation is visible rather than merely quieter.
            tracing::warn!(
                error = ?e,
                player_id = %player_id,
                target_season,
                "player_season_projection lookup failed; falling through to destination-blind inference",
            );
        })
        .ok()
        .and_then(|m| m.get(&player_id).cloned());

        match materialized {
            Some(p) => (p, TrajectoryBasis::DestinationAware),
            None => {
                let features =
                    cstat_core::trajectory::build_trajectory_features(&row, base_season, None);
                match state.predictor.predict_trajectory(&features) {
                    Ok(p) => (p, TrajectoryBasis::SameProgramAssumed),
                    Err(e) => {
                        tracing::warn!(error = ?e, player_id = %player_id, "trajectory predict failed");
                        return None;
                    }
                }
            }
        }
    };

    Some(json!({
        "base_season": base_season,
        "target_season": target_season,
        "projected_mean": pred.mean,
        "projected_lower": pred.lower,
        "projected_upper": pred.upper,
        "prior_campom": row.campom,
        "projection_basis": basis.as_str(),
    }))
}

async fn player_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerDetailParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    // Player UUIDs are season-scoped (natstat_id is the cross-season key), so
    // a URL with last season's UUID + `?season=` switching to this season
    // initially 404s. Resolve via natstat_id and retry. Returned `player.id`
    // tells the frontend to redirect to the canonical URL.
    let resolved_id = match queries::resolve_player_id_for_season(pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })? {
        Some(rid) => rid,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "player not found for this season" })),
            ));
        }
    };

    let player = queries::get_player_by_id(pool, resolved_id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "player not found" })),
            )
        })?;

    let (
        season_stats,
        percentiles,
        game_log,
        league_averages,
        torvik_stats,
        archetype,
        available_seasons,
        trajectory_row,
    ) = tokio::try_join!(
        queries::get_player_season_stats(pool, resolved_id, season),
        queries::get_player_percentiles(pool, resolved_id, season),
        queries::get_player_game_log(pool, resolved_id, season),
        queries::get_league_averages(pool, season),
        queries::get_torvik_stats(pool, resolved_id, season),
        queries::get_player_archetype(pool, resolved_id, season),
        queries::get_player_available_seasons(pool, resolved_id),
        cstat_core::trajectory::fetch_player_trajectory_row(pool, resolved_id, season),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("query failed: {e}") })),
        )
    })?;

    // Phase 5c trajectory: project next-season CamPom from the qualified
    // prior-season row. Precedence and the `projection_basis` contract live in
    // `resolve_trajectory`, shared with the progression route below.
    let trajectory = resolve_trajectory(&state, resolved_id, season, trajectory_row).await;

    Ok(Json(json!({
        "player": player,
        "season_stats": season_stats,
        "percentiles": percentiles,
        "game_log": game_log,
        "league_averages": league_averages,
        "torvik_stats": torvik_stats,
        "archetype": archetype,
        "available_seasons": available_seasons,
        "trajectory": trajectory,
    })))
}

/// Cross-season "career progression" view. Returns one entry per
/// (player_id, season) the human appears in — joined across transfers
/// via `torvik_pid` the same way `get_player_available_seasons` does.
/// Each entry carries the player profile (team name changes year over
/// year for transfers), season_stats, percentiles, torvik_stats, and
/// archetype, so the frontend can render time-series + per-season
/// radars and shot-diet cards without N round-trips. The most-recent
/// season's trajectory projection is included for the header chip.
async fn player_progression(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let pool = &state.db.pool;

    let seasons = queries::get_player_available_seasons(pool, id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("seasons query failed: {e}") })),
            )
        })?;
    if seasons.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "player not found" })),
        ));
    }

    // Fetch each season's payload. Seasons go in order from
    // `get_player_available_seasons` (newest-first); we resolve the
    // season-scoped player_id for each, then fan out the per-season
    // queries via tokio::try_join. Sequential outer loop with parallel
    // inner is a good fit for ~3-6 seasons: ~50ms per season, never
    // hot-pathed enough to need futures::join_all.
    let mut entries: Vec<Value> = Vec::with_capacity(seasons.len());
    // Captured during iteration 0 so the trajectory step below doesn't
    // re-resolve the same (id, latest_season) pair.
    let mut latest_resolved_id: Option<Uuid> = None;
    for (idx, season) in seasons.iter().enumerate() {
        let resolved = queries::resolve_player_id_for_season(pool, id, *season)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("resolve failed for season {season}: {e}") })),
                )
            })?;
        let Some(rid) = resolved else {
            continue;
        };
        if idx == 0 {
            latest_resolved_id = Some(rid);
        }
        let (profile, season_stats, percentiles, torvik_stats, archetype) = tokio::try_join!(
            queries::get_player_by_id(pool, rid, *season),
            queries::get_player_season_stats(pool, rid, *season),
            queries::get_player_percentiles(pool, rid, *season),
            queries::get_torvik_stats(pool, rid, *season),
            queries::get_player_archetype(pool, rid, *season),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({ "error": format!("per-season fetch failed for season {season}: {e}") }),
                ),
            )
        })?;
        let Some(profile) = profile else {
            continue;
        };
        entries.push(json!({
            "season": season,
            "player_id": rid,
            "name": profile.name,
            "team_id": profile.team_id,
            "team_name": profile.team_name,
            "position": profile.position,
            "class_year": profile.class_year,
            "jersey_number": profile.jersey_number,
            "height_inches": profile.height_inches,
            "weight_lbs": profile.weight_lbs,
            "season_stats": season_stats,
            "percentiles": percentiles,
            "torvik_stats": torvik_stats,
            "archetype": archetype,
        }));
    }

    // Trajectory projection — same logic as `player_detail`, anchored
    // to the most-recent season. Surfaces in the page header so users
    // see the projected next-season CamPom alongside their actual
    // career arc on this page (and the link back from the chip on the
    // single-season page). Reuse the player_id resolved in iteration 0
    // of the loop above rather than re-running `resolve_player_id_for_season`.
    let latest_season = seasons[0];
    let trajectory = if let Some(rid) = latest_resolved_id {
        let row = cstat_core::trajectory::fetch_player_trajectory_row(pool, rid, latest_season)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("trajectory query failed: {e}") })),
                )
            })?;
        // Identical precedence to `player_detail` — one shared resolver, because
        // two copies of it is how this page kept projecting a transfer at the
        // program he left after the detail page stopped (#401).
        resolve_trajectory(&state, rid, latest_season, row).await
    } else {
        None
    };

    Ok(Json(json!({
        "available_seasons": seasons,
        "seasons": entries,
        "trajectory": trajectory,
    })))
}

#[derive(Deserialize)]
struct PlayerArchetypeParams {
    season: Option<i32>,
}

async fn player_archetype(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerArchetypeParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let archetype = queries::get_player_archetype(&state.db.pool, id, season)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    match archetype {
        Some(a) => Ok(Json(json!({
            "season": season,
            "archetype": a,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no archetype assigned for this player/season" })),
        )),
    }
}

#[derive(Deserialize)]
struct PlayerSimilarParams {
    season: Option<i32>,
    k: Option<i64>,
    /// Opt in to searching every ingested season instead of just `season`.
    /// Defaults to false, so the pre-existing single-season behaviour — and its
    /// ~22 ms response, against the cross-year path's ~280 ms — is what an
    /// unchanged caller keeps getting.
    cross_year: Option<bool>,
}

/// `GET /api/players/{id}/similar` — nearest neighbours in the archetype
/// feature space. `?cross_year=true` widens the candidate pool to every season;
/// the target vector still comes from the requested `(id, season)`, each
/// neighbour carries its own `season`, each human appears at most once (their
/// nearest season), and the target's own other seasons are returned flagged
/// `is_self` rather than dropped or silently mixed in. See
/// `queries::get_similar_players` for why cross-era distances are commensurable
/// and which era skew is disclosed rather than corrected.
async fn player_similar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<PlayerSimilarParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let limit = params.k.unwrap_or(10).clamp(1, 50);
    let cross_year = params.cross_year.unwrap_or(false);
    let players = queries::get_similar_players(&state.db.pool, id, season, limit, cross_year)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;

    Ok(Json(json!({
        "season": season,
        "cross_year": cross_year,
        "players": players,
    })))
}

#[derive(Deserialize)]
struct PlayerCompareParams {
    ids: String,
    season: Option<i32>,
}

const MAX_COMPARE_PLAYERS: usize = 4;

/// One requested comparison slot: a player UUID plus the season to render it
/// in. `ids` accepts `<uuid>@<season>` per slot; a bare `<uuid>` inherits the
/// request-level `season`, so every pre-existing caller keeps its behaviour.
#[derive(Debug)]
struct CompareSlot {
    /// The UUID exactly as the caller wrote it. Echoed back as
    /// `requested_id`, because cross-season resolution can hand back a
    /// *different* UUID for the same human and `player.id` alone no longer
    /// tells a caller which slot it answered. Note it is not the join key
    /// for a raw `ids` token either — `<uuid>@<year>` equals no UUID — so
    /// entries stay positionally aligned with `ids` and callers join by
    /// index. See `PlayerCompare`'s fetch handler.
    requested_id: Uuid,
    season: i32,
}

/// Split `ids` into slots. `<uuid>` and `<uuid>@<season>` are both accepted;
/// UUIDs contain no `@`, so a single split on it is unambiguous.
fn parse_compare_slots(
    ids: &str,
    default_season: i32,
) -> Result<Vec<CompareSlot>, (StatusCode, Json<Value>)> {
    ids.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|slot| {
            let (id_str, season) = match slot.split_once('@') {
                Some((id_str, season_str)) => {
                    let season = season_str.trim().parse::<i32>().map_err(|e| {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "error": format!("invalid season in ids slot {slot:?}: {e}"),
                            })),
                        )
                    })?;
                    (id_str.trim(), season)
                }
                None => (slot, default_season),
            };
            let requested_id = Uuid::parse_str(id_str).map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("invalid uuid in ids: {e}") })),
                )
            })?;
            Ok(CompareSlot {
                requested_id,
                season,
            })
        })
        .collect()
}

async fn player_compare(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlayerCompareParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let pool = &state.db.pool;

    let slots = parse_compare_slots(&params.ids, season)?;

    if slots.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ids query param is required" })),
        ));
    }
    if slots.len() > MAX_COMPARE_PLAYERS {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("max {MAX_COMPARE_PLAYERS} players per compare request"),
            })),
        ));
    }

    // League averages are what the "vs league average" shading is measured
    // against, so a slot rendered in 2015 has to be shaded against 2015 — one
    // object for the request season would be wrong for every off-season slot.
    // Fetch one per distinct season present (at most MAX_COMPARE_PLAYERS + 1);
    // `league_averages` stays as the request-season object so single-season
    // callers see an unchanged payload.
    let mut league_seasons: Vec<i32> = slots.iter().map(|s| s.season).collect();
    league_seasons.push(season);
    league_seasons.sort_unstable();
    league_seasons.dedup();

    // Concurrently, not in an await loop: this is one wave of round trips
    // instead of up to five. On a same-host DB that is a wash (measured: 5
    // seasons, 0.31s concurrent vs 0.35s sequential warm, parity cold), but
    // the API talks to a remote Postgres where every serialized round trip
    // costs a full RTT — the same shape that made the nightly's per-row loops
    // stall. Folding the seasons into one `unnest`-driven query is the other
    // obvious fix and measures ~2x SLOWER (0.62s vs 0.34s): the per-season
    // form gets a much better plan, so parallelise it rather than replace it.
    // Peak connections per request are unchanged — the per-slot `try_join!`
    // below already holds six at once.
    let mut fetches = tokio::task::JoinSet::new();
    for s in league_seasons.iter().copied() {
        let pool = pool.clone();
        fetches.spawn(async move { (s, queries::get_league_averages(&pool, s).await) });
    }
    let mut averages = std::collections::HashMap::with_capacity(league_seasons.len());
    while let Some(joined) = fetches.join_next().await {
        let (s, result) = joined.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("league averages task failed: {e}") })),
            )
        })?;
        let avg = result.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;
        averages.insert(s, avg);
    }

    // Sorted `league_seasons` drives the insertion order, so the payload's
    // season keys come out ascending regardless of how the map iterates.
    let mut league_averages_by_season = serde_json::Map::new();
    for s in &league_seasons {
        league_averages_by_season.insert(s.to_string(), json!(averages.get(s)));
    }
    let league_averages = json!(averages.get(&season));

    let mut players_data = Vec::with_capacity(slots.len());
    for slot in &slots {
        // Player UUIDs are season-scoped, so a slot pointing a 2026 UUID at
        // 2015 only resolves through natstat_id / torvik_pid — the same path
        // the detail routes take. Cross-year makes this the common case.
        //
        // Identity and season options are asked of the REQUESTED id, not the
        // resolved one: they are the two things an unavailable slot still owes
        // its column ("whose year is empty" and "which years would work"), so
        // they have to come from an id that exists whether or not the slot
        // resolved. Both are cross-season joins over the same human, so the
        // answer is identical either way for a slot that did resolve. Joined
        // WITH the resolve rather than awaited after it — all three need only
        // `slot.requested_id`, so one wave keeps the extra fields off the
        // request's latency path.
        let (resolved, requested_name, available_seasons) = tokio::try_join!(
            queries::resolve_player_id_for_season(pool, slot.requested_id, slot.season),
            queries::get_player_name(pool, slot.requested_id),
            queries::get_player_available_seasons(pool, slot.requested_id),
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("query failed: {e}") })),
            )
        })?;
        let identity = (requested_name, available_seasons);

        // A slot that does not resolve gets an explicit unavailable entry
        // rather than vanishing from the array: "not in Division I that year"
        // is a legitimate, frequent cross-year answer, and a dropped element
        // leaves the UI rendering three columns for four picks with no
        // explanation. Positional alignment with `ids` is preserved.
        let Some(resolved_id) = resolved else {
            players_data.push(unavailable_compare_slot(slot, &identity));
            continue;
        };

        let (player, season_stats, percentiles, game_log, torvik_stats, archetype) =
            tokio::try_join!(
                queries::get_player_by_id(pool, resolved_id, slot.season),
                queries::get_player_season_stats(pool, resolved_id, slot.season),
                queries::get_player_percentiles(pool, resolved_id, slot.season),
                queries::get_player_game_log(pool, resolved_id, slot.season),
                queries::get_torvik_stats(pool, resolved_id, slot.season),
                queries::get_player_archetype(pool, resolved_id, slot.season),
            )
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("query failed: {e}") })),
                )
            })?;

        // The resolver found a (player, season) row but the profile query did
        // not — same user-visible state as an unresolvable slot.
        let Some(player) = player else {
            players_data.push(unavailable_compare_slot(slot, &identity));
            continue;
        };

        let (requested_name, available_seasons) = identity;
        players_data.push(json!({
            "requested_id": slot.requested_id,
            "season": slot.season,
            "available": true,
            "requested_name": requested_name,
            "available_seasons": available_seasons,
            "player": player,
            "season_stats": season_stats,
            "percentiles": percentiles,
            "game_log": game_log,
            "torvik_stats": torvik_stats,
            "archetype": archetype,
        }));
    }

    Ok(Json(json!({
        "season": season,
        "league_averages": league_averages,
        "league_averages_by_season": league_averages_by_season,
        "players": players_data,
    })))
}

/// The placeholder entry for a slot with no row in its season. Carries the same
/// key set as a resolved entry, with every STAT field empty, so anything that
/// only reads stats needs no narrowing — `available` and the null `player` are
/// what distinguish it. `requested_name` / `available_seasons` are deliberately
/// populated: they are what lets the UI name the empty column and offer the
/// years that would fill it, instead of a dead end. `ComparePlayerUnavailable`
/// in `web/src/api/client.ts` mirrors this key set; keep the two in step.
fn unavailable_compare_slot(
    slot: &CompareSlot,
    (requested_name, available_seasons): &(Option<String>, Vec<i32>),
) -> Value {
    json!({
        "requested_id": slot.requested_id,
        "season": slot.season,
        "available": false,
        "requested_name": requested_name,
        "available_seasons": available_seasons,
        "player": null,
        "season_stats": null,
        "percentiles": null,
        "game_log": [],
        "torvik_stats": null,
        "archetype": null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(ids: &str, default_season: i32) -> Vec<(Uuid, i32)> {
        parse_compare_slots(ids, default_season)
            .expect("slots should parse")
            .into_iter()
            .map(|s| (s.requested_id, s.season))
            .collect()
    }

    const A: &str = "11111111-1111-1111-1111-111111111111";
    const B: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn bare_uuids_inherit_the_request_season() {
        let parsed = slots(&format!("{A},{B}"), 2026);
        assert_eq!(
            parsed,
            vec![
                (Uuid::parse_str(A).unwrap(), 2026),
                (Uuid::parse_str(B).unwrap(), 2026),
            ]
        );
    }

    #[test]
    fn per_slot_season_overrides_the_request_season() {
        let parsed = slots(&format!("{A}@2015,{B}"), 2026);
        assert_eq!(
            parsed,
            vec![
                (Uuid::parse_str(A).unwrap(), 2015),
                (Uuid::parse_str(B).unwrap(), 2026),
            ]
        );
    }

    #[test]
    fn the_same_uuid_can_appear_twice_in_different_seasons() {
        // Comparing a player against his own earlier self is a first-class
        // cross-year case, so duplicate UUIDs must survive as distinct slots.
        let parsed = slots(&format!("{A}@2024,{A}@2026"), 2026);
        assert_eq!(
            parsed,
            vec![
                (Uuid::parse_str(A).unwrap(), 2024),
                (Uuid::parse_str(A).unwrap(), 2026),
            ]
        );
    }

    #[test]
    fn whitespace_and_empty_slots_are_tolerated() {
        let parsed = slots(&format!(" {A} @ 2015 , ,{B}"), 2026);
        assert_eq!(
            parsed,
            vec![
                (Uuid::parse_str(A).unwrap(), 2015),
                (Uuid::parse_str(B).unwrap(), 2026),
            ]
        );
    }

    #[test]
    fn a_bad_uuid_or_season_is_a_400() {
        for ids in [format!("{A},not-a-uuid"), format!("{A}@nineteen")] {
            let (status, _) = parse_compare_slots(&ids, 2026).expect_err("should reject");
            assert_eq!(status, StatusCode::BAD_REQUEST, "for ids {ids:?}");
        }
    }

    #[test]
    fn every_trajectory_basis_is_in_the_frontend_union() {
        // `projection_basis` is a closed union on the TypeScript side and the
        // key for the projection chip's caveat. A basis the server emits but the
        // client does not list type-checks fine (the response is `any` at the
        // wire) and renders the unqualified tooltip — which is precisely the
        // #401 failure: a destination-blind number presented as "projected
        // next-season CAM" with nothing saying what it assumed. Sibling of
        // `predict::every_basis_the_server_can_emit_is_in_the_frontend_union`.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/cstat-api has a grandparent");
        let client = std::fs::read_to_string(root.join("web/src/api/client.ts"))
            .expect("read web/src/api/client.ts");

        // Narrow to the union itself. Matching the whole file would pass on a
        // basis that merely appears in a comment somewhere — a vacuous pass is
        // the failure mode this kind of grep-with-an-opinion has to rule out.
        let start = client
            .find("projection_basis:")
            .expect("client.ts declares projection_basis");
        let union = &client[start..start + client[start..].find(';').expect("union terminates")];
        // Sanity-check the slice, so a moved field or a stray earlier mention of
        // the name cannot make the loop below pass on nothing. Keyed on a member
        // rather than on a count: a count equal to `ALL.len()` would fire here
        // instead of at the actionable assertion, hiding the message that says
        // what to do.
        assert!(
            union.contains("'held_out'") && union.contains('|'),
            "did not parse the projection_basis union out of client.ts — the field moved, \
             so this check would be vacuous: {union}"
        );

        for basis in TrajectoryBasis::ALL {
            assert!(
                union.contains(&format!("'{}'", basis.as_str())),
                "the server can emit projection_basis {:?} but web/src/api/client.ts does not \
                 list it. Add it to the union, and decide what the chip tooltip in \
                 web/src/pages/PlayerDetail.tsx and web/src/pages/PlayerProgression.tsx should \
                 say about it — a basis that assumes something the user cannot see needs to \
                 say so.\n\nunion: {union}",
                basis.as_str()
            );
        }
    }
}
