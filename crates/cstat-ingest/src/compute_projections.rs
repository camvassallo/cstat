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
    compose_all_projections, fetch_draft_entrants, fetch_player_departures,
    project_returner_cam_v3, project_returner_cam_v3_banded, score_projection_adj_em,
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

/// Display identity (`name`, `natstat_id`) for every real player projected onto
/// a roster, keyed by their season-scoped `players.id`. Freshmen aren't here —
/// their name comes from `RecruitMeta`, and they have no `players` row. Used to
/// denormalize `player_season_projection` so the `/players` projected page reads
/// one table with no joins.
async fn fetch_player_identity(
    pool: &PgPool,
    player_ids: &[Uuid],
) -> Result<HashMap<Uuid, (String, Option<String>)>> {
    if player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, natstat_id FROM players WHERE id = ANY($1)")
            .bind(player_ids)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, natstat_id)| (id, (name, natstat_id)))
        .collect())
}

/// One `player_season_projection` row to insert. Grouped into a struct so the
/// insert helper doesn't trip `clippy::too_many_arguments` (14 columns).
struct PlayerProjectionRow<'a> {
    target_season: i32,
    player_id: Uuid,
    source: &'a str,
    name: &'a str,
    team_id: Uuid,
    team_name: &'a str,
    natstat_id: Option<&'a str>,
    /// Mean projected cam_v3 (stored as REAL; bound as f32).
    mean: f64,
    lower: Option<f32>,
    upper: Option<f32>,
    class_year: Option<&'a str>,
    primary_archetype: Option<&'a str>,
    composite_rank: Option<i32>,
    star_rating: Option<i16>,
}

/// Insert one per-player projection row. `ON CONFLICT DO UPDATE` (last write
/// wins) guards against the rare case of a player appearing on two composed
/// rosters; the PK is `(target_season, player_id)`.
async fn insert_player_projection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    r: PlayerProjectionRow<'_>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO player_season_projection
            (target_season, player_id, source, name, team_id, team_name, natstat_id,
             projected_cam_mean, projected_cam_lower, projected_cam_upper,
             class_year, primary_archetype, composite_rank, star_rating, computed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, now())
        ON CONFLICT (target_season, player_id) DO UPDATE SET
            source              = EXCLUDED.source,
            name                = EXCLUDED.name,
            team_id             = EXCLUDED.team_id,
            team_name           = EXCLUDED.team_name,
            natstat_id          = EXCLUDED.natstat_id,
            projected_cam_mean  = EXCLUDED.projected_cam_mean,
            projected_cam_lower = EXCLUDED.projected_cam_lower,
            projected_cam_upper = EXCLUDED.projected_cam_upper,
            class_year          = EXCLUDED.class_year,
            primary_archetype   = EXCLUDED.primary_archetype,
            composite_rank      = EXCLUDED.composite_rank,
            star_rating         = EXCLUDED.star_rating,
            computed_at         = now()
        "#,
    )
    .bind(r.target_season)
    .bind(r.player_id)
    .bind(r.source)
    .bind(r.name)
    .bind(r.team_id)
    .bind(r.team_name)
    .bind(r.natstat_id)
    .bind(r.mean as f32)
    .bind(r.lower)
    .bind(r.upper)
    .bind(r.class_year)
    .bind(r.primary_archetype)
    .bind(r.composite_rank)
    .bind(r.star_rating)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Compute and persist the preseason projection for each target `year`.
/// **Replaces** each season's rows in one transaction (delete-then-insert), so a
/// re-run is authoritative: rows for teams the current logic no longer produces
/// (newly too-thin / unresolvable) are pruned, not left stale.
pub async fn run(
    pool: &PgPool,
    predictor: &Predictor,
    model_dir: &std::path::Path,
    years: &[i32],
) -> Result<()> {
    for &year in years {
        let base_season = year - 1;

        // Firm draft departures from the `draft_entrants` table — removes
        // drafted players from the returning roster (else over-projected).
        let entrants = fetch_draft_entrants(pool, base_season).await?;

        // Curated exits no feed reports — pro signings abroad, retirements,
        // dismissals. Same over-projection failure mode as the draft list.
        let departures = fetch_player_departures(pool, base_season).await?;

        let projections = compose_all_projections(
            pool,
            base_season,
            &entrants,
            &departures,
            predictor,
            crate::target_season_retro_complete(year),
        )
        .await?;
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

        // Banded (mean + q10/q90) twin of the above, for the per-player
        // `player_season_projection` materialization below. The team-AdjEM
        // scoring only needs the mean; the projected-players page wants the band.
        let projected_cam_banded = project_returner_cam_v3_banded(pool, predictor, &traj_ids, year)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "banded trajectory projection failed; per-player bands omitted"
                );
                HashMap::new()
            });

        // Name / natstat_id for every real (non-recruit) player on any roster.
        let mut real_ids: Vec<Uuid> = Vec::new();
        for p in &projections {
            real_ids.extend(p.returning.iter().map(|r| r.player_id));
            real_ids.extend(p.arrivals.iter().map(|a| a.player_id));
        }
        let player_identity = fetch_player_identity(pool, &real_ids).await?;

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
        // Per-player projected CamPom (migration 045). Independent of the
        // team-AdjEM qualification gate above — a player on a too-thin team
        // still has a valid projection — so iterate every roster's members.
        // Same authoritative replace: clear the season, re-insert the current
        // composition. team_id is the base-season roster team (destination for
        // an incoming transfer); the frontend links at `?season=base_season`.
        sqlx::query("DELETE FROM player_season_projection WHERE target_season = $1")
            .bind(year)
            .execute(&mut *tx)
            .await?;
        let mut player_rows = 0usize;
        for p in &projections {
            for (row, source) in p
                .returning
                .iter()
                .map(|r| (r, "returning"))
                .chain(p.arrivals.iter().map(|a| (a, "transfer")))
            {
                let Some((name, natstat_id)) = player_identity.get(&row.player_id) else {
                    // No players row (shouldn't happen for a composed roster
                    // member); skip rather than write an unnamed row.
                    continue;
                };
                let band = projected_cam_banded.get(&row.player_id);
                let mean = band.map(|b| b.mean as f64).or(row.cam_v3).unwrap_or(0.0);
                insert_player_projection(
                    &mut tx,
                    PlayerProjectionRow {
                        target_season: year,
                        player_id: row.player_id,
                        source,
                        name,
                        team_id: p.team_id,
                        team_name: &p.team_name,
                        natstat_id: natstat_id.as_deref(),
                        mean,
                        lower: band.map(|b| b.lower),
                        upper: band.map(|b| b.upper),
                        class_year: row.class_year.as_deref(),
                        primary_archetype: row.primary_class.as_deref(),
                        composite_rank: None,
                        star_rating: None,
                    },
                )
                .await?;
                player_rows += 1;
            }
            for (row, meta) in &p.recruits {
                insert_player_projection(
                    &mut tx,
                    PlayerProjectionRow {
                        target_season: year,
                        player_id: meta.recruit_id,
                        source: "freshman",
                        name: &meta.name,
                        team_id: p.team_id,
                        team_name: &p.team_name,
                        natstat_id: None,
                        mean: row.cam_v3.unwrap_or(0.0),
                        lower: meta.projected_campom_lower,
                        upper: meta.projected_campom_upper,
                        class_year: row.class_year.as_deref(),
                        primary_archetype: None,
                        composite_rank: meta.composite_rank,
                        star_rating: meta.star_rating,
                    },
                )
                .await?;
                player_rows += 1;
            }
        }

        // Record WHICH model generation produced this season's rows (#238),
        // inside the same transaction as the rows themselves. Doing it outside
        // would allow a crash between the two to leave projections whose
        // recorded origin is the previous run's — a provenance record that
        // lies is worse than none, because the staleness report would then
        // vouch for rows it should be flagging.
        //
        // `roster_adjo` is included even though this command never runs it:
        // `/api/projections` derives the served AdjO/AdjD split live from that
        // model against these very rows, so the pair is what a reader needs to
        // reproduce what the site showed.
        record_provenance(&mut tx, year, model_dir).await?;

        tx.commit().await?;
        println!(
            "compute-projections {year}: wrote {written} rows \
             (skipped {skipped_thin} too-thin, {skipped_unresolved} unresolved-target; \
             season replaced); {player_rows} player projections"
        );
    }
    Ok(())
}

/// Stamp `artifact_provenance` for the season just written (migration 047).
///
/// Keyed per season rather than once per run because `--years` is routinely a
/// subset: a run that refreshes only 2026 must not overwrite the record for
/// seasons it did not touch, or the report would claim a stale season was
/// regenerated by a model it never saw.
async fn record_provenance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    year: i32,
    model_dir: &std::path::Path,
) -> Result<()> {
    let provenance = cstat_core::provenance::layer3_provenance(
        model_dir,
        "cstat-ingest compute-projections",
        &[
            cstat_core::provenance::ROSTER_IMPACT,
            cstat_core::provenance::ROSTER_ADJO,
        ],
        // The LOSO set is a backtest input; it has no bearing on the served
        // projection, which scores with the all-seasons model.
        false,
    )?;
    sqlx::query(
        r#"
        INSERT INTO artifact_provenance (artifact, artifact_key, provenance, computed_at)
        VALUES ('team_preseason_projection', $1, $2, now())
        ON CONFLICT (artifact, artifact_key) DO UPDATE SET
            provenance  = EXCLUDED.provenance,
            computed_at = now()
        "#,
    )
    .bind(year.to_string())
    .bind(provenance)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
