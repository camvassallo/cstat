//! Persist the preseason roster-impact projection per (season, team) into
//! `team_preseason_projection` (migration 023).
//!
//! `/api/projections` computes the season-wide roster-impact projection live on
//! every call (`compose_all_projections` → `score_projection_adj_em`). The
//! preseason × pit predict blend (ROADMAP §6) needs each team's projected
//! AdjEM cheaply, per predict request — so this step materializes the same
//! numbers once per season and the predict route reads them back.
//!
//! Parity: scoring goes through the shared
//! `cstat_core::roster_projection::score_projection_adj_em`, the exact
//! function the API route uses, so the persisted midpoint equals the served
//! `/api/projections` midpoint by construction.
//!
//! Keying: `compose_all_projections` returns `ProjectedRoster` keyed by the
//! **base-season** (`year − 1`) team UUID. We resolve each to its
//! **target-season** UUID via `teams.natstat_id` before writing, so the
//! predict route (which holds target-season UUIDs) reads with no
//! cross-season hop.

use anyhow::Result;
use cstat_core::inference::Predictor;
use cstat_core::roster_projection::{
    compose_all_projections, fetch_draft_entrants, project_returner_cam_v3, score_projection_adj_em,
};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

/// Midpoint return-probability for an in-season projection. By the time a
/// season is underway the draft-uncertain cohort is resolved (departed
/// players are gone, returners are on the roster), so `compose_all_projections`
/// leaves the `uncertain` bucket empty → floor == ceiling and this weight
/// cancels. 0.5 is the neutral default that matches the projections route's
/// empty-cohort behavior.
const IN_SEASON_P_RETURN: f32 = 0.5;

/// Base-season AdjEM per team (the baseline the shrink blends toward),
/// cast to f32 to match `score_projection_adj_em`'s signature.
async fn fetch_baseline_adj_em(pool: &PgPool, season: i32) -> Result<HashMap<Uuid, f32>> {
    let rows: Vec<(Uuid, Option<f64>)> = sqlx::query_as(
        "SELECT team_id, adj_efficiency_margin FROM team_season_stats WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, v)| v.map(|x| (id, x as f32)))
        .collect())
}

/// Map each base-season team_id → the same program's target-season team_id
/// via `natstat_id`. Team UUIDs are season-scoped, so a base-season
/// `ProjectedRoster.team_id` can't index a target-season table directly.
/// Teams with no target-season row (defunct program, D-I transition) are
/// absent from the map and skipped.
async fn resolve_base_to_target(
    pool: &PgPool,
    base_season: i32,
    target: i32,
) -> Result<HashMap<Uuid, Uuid>> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        r#"
        SELECT base.id, tgt.id
        FROM teams base
        JOIN teams tgt
          ON tgt.natstat_id = base.natstat_id AND tgt.season = $2
        WHERE base.season = $1
        "#,
    )
    .bind(base_season)
    .bind(target)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Compute and persist the preseason projection for each target `year`.
/// **Replaces** each season's rows in one transaction (delete-then-insert), so a
/// re-run is authoritative: rows for teams the current logic no longer produces
/// (newly too-thin / unresolvable) are pruned, not left stale.
pub async fn run(pool: &PgPool, predictor: &Predictor, years: &[i32]) -> Result<()> {
    for &year in years {
        let base_season = year - 1;

        // Firm draft departures from the `draft_entrants` table — removes
        // drafted players from the returning roster (else over-projected).
        let entrants = fetch_draft_entrants(pool, base_season).await?;

        let projections = compose_all_projections(pool, base_season, &entrants, predictor).await?;
        let baseline_map = fetch_baseline_adj_em(pool, base_season).await?;
        let target_map = resolve_base_to_target(pool, base_season, year).await?;

        // One batched trajectory cam_v3 projection for the whole slate —
        // same shape as the route. Degrades to current-season cam_v3 on error.
        let mut traj_ids: Vec<Uuid> = Vec::new();
        for p in &projections {
            traj_ids.extend(p.returning.iter().map(|r| r.player_id));
            traj_ids.extend(p.arrivals.iter().map(|a| a.player_id));
            traj_ids.extend(p.uncertain.iter().map(|(row, _)| row.player_id));
        }
        let projected_cam = project_returner_cam_v3(pool, predictor, &traj_ids, year)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "trajectory cam_v3 projection failed; falling back to current-season cam_v3"
                );
                HashMap::new()
            });

        // Authoritative per-season replace, in one transaction: clear the
        // season, then re-insert what the current logic produces. A plain
        // per-team UPSERT would leave stale stragglers for teams that became
        // too-thin / unresolvable since the last run — those rows would keep
        // serving an old preseason anchor to `/predict`. The transaction keeps a
        // mid-run failure from leaving the season half-written.
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM team_preseason_projection WHERE season = $1")
            .bind(year)
            .execute(&mut *tx)
            .await?;

        let (mut written, mut skipped_thin, mut skipped_unresolved) = (0usize, 0usize, 0usize);
        for p in &projections {
            let baseline = baseline_map.get(&p.team_id).copied();
            let Some((floor, ceiling, midpoint)) =
                score_projection_adj_em(p, predictor, baseline, IN_SEASON_P_RETURN, &projected_cam)
            else {
                skipped_thin += 1;
                continue;
            };
            let Some(&target_id) = target_map.get(&p.team_id) else {
                skipped_unresolved += 1;
                continue;
            };
            sqlx::query(
                r#"
                INSERT INTO team_preseason_projection
                    (season, team_id, projected_adj_em, floor_adj_em, ceiling_adj_em, computed_at)
                VALUES ($1, $2, $3, $4, $5, now())
                ON CONFLICT (season, team_id) DO UPDATE SET
                    projected_adj_em = EXCLUDED.projected_adj_em,
                    floor_adj_em     = EXCLUDED.floor_adj_em,
                    ceiling_adj_em   = EXCLUDED.ceiling_adj_em,
                    computed_at      = now()
                "#,
            )
            .bind(year)
            .bind(target_id)
            .bind(midpoint)
            .bind(floor)
            .bind(ceiling)
            .execute(&mut *tx)
            .await?;
            written += 1;
        }
        tx.commit().await?;
        println!(
            "compute-projections {year}: wrote {written} rows \
             (skipped {skipped_thin} too-thin, {skipped_unresolved} unresolved-target; \
             season replaced)"
        );
    }
    Ok(())
}
