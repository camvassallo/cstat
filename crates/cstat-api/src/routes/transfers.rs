use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use cstat_core::queries::get_archetype_distributions_for_teams;
use cstat_core::roster_fit::{FitTier, compute_fit_score};
use cstat_core::roster_projection::normalize_player_name as normalize;
use cstat_core::team_name_match::{team_match_score, team_matches};
use cstat_core::trajectory::{
    TRAJECTORY_NUM_FEATURES, build_trajectory_features, fetch_player_trajectory_rows,
    fetch_trajectory_oof,
};
use serde::Serialize;
use serde_json::{Value, json};
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
    /// Phase 5c trajectory projection — predicted CamPom for the
    /// transfer's first season at the destination (= year+1). Computed
    /// from the player's source-season stats; the trajectory model is
    /// destination-agnostic (no team feature), so the projection
    /// assumes a role similar to what they played at their source.
    /// NULL when the player didn't match a cstat row, or didn't pass
    /// the trajectory qualification gate (≥5 GP, ≥5 MPG), or batch
    /// inference failed. The Phase 6 honesty note applies here too:
    /// elite transfers (current CamPom ≥+15) get systematically
    /// under-projected by ≈−3 due to regression-to-the-mean — see
    /// `trajectory_model_meta.json::mae_by_current_campom`.
    projected_campom_mean: Option<f32>,
    projected_campom_lower: Option<f32>,
    projected_campom_upper: Option<f32>,
    /// Archetype-based roster fit score in `[-1.0, +1.0]`. Positive
    /// means the candidate's primary (+ secondary at half weight) class
    /// fills a gap in the destination's current archetype distribution;
    /// negative means it piles onto an already-overweighted class.
    /// NULL when we don't have both a destination team_id and a
    /// primary_class for the candidate. Baseline is the destination's
    /// **source-season** archetype distribution — the same numbers
    /// surfaced as Identity/Gaps on TeamDetail. Doesn't account for
    /// next-season turnover (graduating seniors, outbound portal); v2
    /// can hook into projected-roster distributions once recruit
    /// archetypes are addressable.
    fit_score: Option<f64>,
    fit_tier: Option<FitTier>,
    /// One-line rationale, e.g. "Fills Cleric gap", "Stacks Wizard
    /// rotation", "Roster-neutral". Mirrors the `fit_score` null gate.
    fit_label: Option<String>,
    /// Destination's index ratio for the candidate's primary class
    /// (team_share ÷ d1_share). Exposed so the tooltip can show raw
    /// context — "Duke is 2.3× over-indexed in Wizard" — without
    /// recomputing client-side.
    fit_primary_index: Option<f64>,
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
                projected_campom_mean: None,
                projected_campom_lower: None,
                projected_campom_upper: None,
                fit_score: None,
                fit_tier: None,
                fit_label: None,
                fit_primary_index: None,
            }
        })
        .collect();

    // Phase 5c trajectory projections — predicted CamPom for the
    // transfer's first destination season (= source-season `year` + 1).
    //
    // Precedence: OOF (LOPO held-out) predictions first for any player
    // whose torvik_pid has a row in `trajectory_oof_predictions` at
    // target_season = year + 1; live inference only for the remainder.
    // For any historical year the model trained on, this serves the
    // honest held-out prediction instead of in-sample inference (which
    // inflates elites like Saunders/Jefferson/Cluff by ~3-5 CamPom).
    // For the forward year (next class the model hasn't seen yet) the
    // OOF table is empty and everything falls through to live.
    //
    // On any error (fetch failure or inference failure), log once and
    // serve NULL projections route-wide — the frontend renders an
    // em-dash.
    let matched_ids: Vec<Uuid> = enriched.iter().filter_map(|e| e.player_id).collect();
    let target_season = year + 1;
    if !matched_ids.is_empty() {
        let oof_map = fetch_trajectory_oof(&state.db.pool, &matched_ids, target_season)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = ?e,
                    target_season,
                    n = matched_ids.len(),
                    "trajectory OOF lookup failed for transfers; falling through to live inference",
                );
                HashMap::new()
            });
        // Splice OOF hits straight in. Track which player_ids still
        // need live inference (forward-year cohort + the small
        // ~4% missing torvik_pid mapping).
        let mut need_live: Vec<Uuid> = Vec::new();
        for e in enriched.iter_mut() {
            if let Some(pid) = e.player_id {
                if let Some(pred) = oof_map.get(&pid) {
                    e.projected_campom_mean = Some(pred.mean);
                    e.projected_campom_lower = Some(pred.lower);
                    e.projected_campom_upper = Some(pred.upper);
                } else {
                    need_live.push(pid);
                }
            }
        }
        if !need_live.is_empty() {
            match fetch_player_trajectory_rows(&state.db.pool, &need_live, year).await {
                Ok(row_map) => {
                    let mut indices: Vec<usize> = Vec::new();
                    let mut feature_vectors: Vec<[f32; TRAJECTORY_NUM_FEATURES]> = Vec::new();
                    for (i, e) in enriched.iter().enumerate() {
                        if let Some(pid) = e.player_id
                            && !oof_map.contains_key(&pid)
                            && let Some(row) = row_map.get(&pid)
                        {
                            indices.push(i);
                            feature_vectors.push(build_trajectory_features(row, year));
                        }
                    }
                    match state.predictor.predict_trajectory_batch(&feature_vectors) {
                        Ok(preds) => {
                            for (idx, pred) in indices.iter().zip(preds.iter()) {
                                enriched[*idx].projected_campom_mean = Some(pred.mean);
                                enriched[*idx].projected_campom_lower = Some(pred.lower);
                                enriched[*idx].projected_campom_upper = Some(pred.upper);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = ?e,
                                year,
                                n = feature_vectors.len(),
                                "trajectory batch predict failed for transfers; serving NULL projections for live-inference cohort",
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        year,
                        n = need_live.len(),
                        "trajectory features fetch failed for transfers; serving NULL projections for live-inference cohort",
                    );
                }
            }
        }
    }

    // Archetype roster-fit scoring (v1). For each transfer with both a
    // resolved destination team and a known primary archetype class,
    // score how well the candidate fits the destination's current
    // archetype distribution. Bulk-fetch all needed distributions in
    // one round-trip; baseline season is `year` (the source season),
    // since the destination's `year+1` archetype distribution doesn't
    // exist yet for the in-flight portal cycle. Honest caveat in the
    // EnrichedTransfer doc comment.
    let fit_team_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = enriched
            .iter()
            .filter_map(|e| {
                if e.primary_class.is_some() {
                    e.next_team_id
                } else {
                    None
                }
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    };
    if !fit_team_ids.is_empty() {
        match get_archetype_distributions_for_teams(&state.db.pool, &fit_team_ids, year).await {
            Ok(dist_map) => {
                // Teams absent from the map have no archetype coverage
                // for the season; compute_fit_score treats every class
                // as a max-strength gap given an empty slice, which is
                // the right behavior (transition-period D-I teams
                // genuinely have unknown rosters). Hoist the empty
                // sentinel out of the row loop so we don't allocate
                // per row.
                let empty: Vec<cstat_core::queries::ArchetypeShare> = Vec::new();
                for e in enriched.iter_mut() {
                    let (Some(primary), Some(team_id)) =
                        (e.primary_class.as_deref(), e.next_team_id)
                    else {
                        continue;
                    };
                    let dist = dist_map.get(&team_id).unwrap_or(&empty);
                    let fit = compute_fit_score(primary, e.secondary_class.as_deref(), dist);
                    e.fit_score = Some(fit.raw);
                    e.fit_tier = Some(fit.tier);
                    e.fit_label = Some(fit.label);
                    e.fit_primary_index = Some(fit.primary_index);
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = ?err,
                    year,
                    n = fit_team_ids.len(),
                    "archetype distribution fetch failed for transfers; serving NULL fit scores",
                );
            }
        }
    }

    Ok(Json(json!({
        "year": year,
        "transfers": enriched,
        "total": enriched.len(),
    })))
}
