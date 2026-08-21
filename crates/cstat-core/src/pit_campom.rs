//! Point-in-time CamPom v3 (no-SOS) computation.
//!
//! Aggregates `torvik_player_game_stats` rows up to a cutoff date and applies
//! the CamPom v3 formula. The leak-free counterpart to the season-aggregate
//! `torvik_player_stats.cam_gbpm_v3` column. Used at inference time to rebuild
//! roster features from pre-game state — see ROADMAP §4b "point-in-time
//! historical predictions" and the predict-honesty audit
//! (`training/eval_history/honest_audit_findings_20260529.md`).
//!
//! Mirrors `training/compute_campom_at.py` so the Python pit lookup and the
//! Rust inference path agree on every intermediate. Skips the conference-SOS
//! adjustment (the Python prototype validated r=0.92 vs the season aggregate
//! without it, and a pit-SOS reconstruction is its own lift).

use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::compute::{
    CAMPOM_DEFENSE_DISCOUNT, CAMPOM_GP_K, CAMPOM_MINUTES_EXPONENT, CAMPOM_OFFENSE_EXPONENT,
    CAMPOM_USG_REF,
};

/// Minimum games-played filter for a player to be included in the pit cohort.
/// Matches the Python prototype (`compute_at(..., min_gp=5)`) and the CamPom
/// display floor used in the season-aggregate compute.
pub const PIT_MIN_GP: i64 = 5;

/// Per-player point-in-time CamPom output.
///
/// `ogbpm` / `dgbpm` are possession-weighted cumulative aggregates of
/// per-game contributions (sum of `(o|d)bpm × possessions` divided by
/// total possessions). `cam_gbpm_v3_no_sos` applies the usage / minutes /
/// GP-shrinkage chain on top, without the conference-SOS layer.
#[derive(Debug, Clone, Copy)]
pub struct PitCamPom {
    pub gp: i64,
    pub ogbpm: f64,
    pub dgbpm: f64,
    pub cam_gbpm_v3_no_sos: f64,
}

/// Raw per-player aggregate fetched from `torvik_player_game_stats`,
/// joined through `torvik_player_stats` so multiple in-season Torvik
/// stints for the same cstat player (mid-season transfers) collapse
/// into one combined row before CamPom is derived.
#[derive(sqlx::FromRow, Debug)]
struct PitAggRow {
    player_id: Uuid,
    gp: i64,
    ogbpm: Option<f64>,
    dgbpm: Option<f64>,
    usg: Option<f64>,
    min_pct: Option<f64>,
}

/// Compute point-in-time CamPom v3 (no-SOS) for every qualified player in a
/// season as of a cutoff date.
///
/// Filters to players with at least `PIT_MIN_GP` games played by the cutoff.
/// Returns a map keyed by cstat `players.id` — the join through
/// `torvik_player_stats.torvik_pid → torvik_player_stats.player_id` is done
/// inside the SQL so that transferring players (multiple Torvik pids per
/// season, all resolving to the same cstat `player_id`) aggregate into a
/// single row instead of overwriting each other in app code.
///
/// The cohort mean of `min_pct` is computed over the qualified, post-join
/// set as of the cutoff. This matches the Python prototype's intent (the
/// normalizer reflects who was playing as of the cutoff) and correctly
/// treats one human as one cohort member rather than counting each stint.
pub async fn compute_pit_campom(
    pool: &PgPool,
    season: i32,
    as_of_date: NaiveDate,
) -> Result<HashMap<Uuid, PitCamPom>, sqlx::Error> {
    // Possession-weighted ogbpm / dgbpm; minutes-pct-weighted usg; simple-mean
    // min_pct. Mirrors `training/compute_campom_at.py::compute_at`, but
    // GROUP BYs cstat `player_id` (resolved via `torvik_player_stats`) so
    // mid-season transfers collapse into one row. NULL `player_id` rows
    // (~1-2% of torvik stints don't match into cstat) are filtered out
    // here so callers don't have to handle the missing-id case downstream.
    let rows = sqlx::query_as::<_, PitAggRow>(
        r#"
        SELECT
            tps.player_id,
            COUNT(*) AS gp,
            SUM(tpgs.obpm * COALESCE(tpgs.possessions, 0))
                / NULLIF(SUM(COALESCE(tpgs.possessions, 0)), 0) AS ogbpm,
            SUM(tpgs.dbpm * COALESCE(tpgs.possessions, 0))
                / NULLIF(SUM(COALESCE(tpgs.possessions, 0)), 0) AS dgbpm,
            SUM(tpgs.usage * COALESCE(tpgs.minutes_pct, 0))
                / NULLIF(SUM(COALESCE(tpgs.minutes_pct, 0)), 0) AS usg,
            AVG(tpgs.minutes_pct) AS min_pct
        FROM torvik_player_game_stats tpgs
        JOIN torvik_player_stats tps
          ON tps.torvik_pid = tpgs.pid
         AND tps.season = tpgs.season
         AND tps.player_id IS NOT NULL
        WHERE tpgs.season = $1
          AND tpgs.game_date <= $2
        GROUP BY tps.player_id
        HAVING COUNT(*) >= $3
        -- ORDER BY is not cosmetic here. The cohort mean below folds over
        -- these rows in Rust, and floating-point addition is not associative,
        -- so an unordered GROUP BY makes `mean_min_pct` depend on whatever row
        -- order the planner happens to produce. That mean scales EVERY
        -- player's CamPom, and CamPom values sit on LightGBM split thresholds
        -- — the same last-bit-to-discrete-jump path that made the pit
        -- prediction path return 16.8 or 17.1 points for one 2026 matchup
        -- depending on the run (#266). That instance came from `HashMap`
        -- iteration order and is fixed at the call site; this one is latent
        -- (stable per plan, not across plan changes), and pinning it costs a
        -- sort of ~4,000 rows against a 76,000-row scan.
        ORDER BY tps.player_id
        "#,
    )
    .bind(season)
    .bind(as_of_date)
    .bind(PIT_MIN_GP)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(HashMap::new());
    }

    // Cohort mean of min_pct over the qualified set. NULL min_pct rows
    // contribute neither to the sum nor the count, matching pandas mean().
    let (sum_min, n_min) = rows.iter().fold((0.0, 0i64), |(s, n), r| match r.min_pct {
        Some(v) => (s + v, n + 1),
        None => (s, n),
    });
    let mean_min_pct = if n_min > 0 {
        sum_min / n_min as f64
    } else {
        0.0
    };

    let mut out = HashMap::with_capacity(rows.len());
    for r in rows {
        let cam = compute_one(
            r.ogbpm.unwrap_or(0.0),
            r.dgbpm.unwrap_or(0.0),
            r.usg.unwrap_or(0.0),
            r.min_pct.unwrap_or(0.0),
            r.gp,
            mean_min_pct,
        );
        out.insert(r.player_id, cam);
    }
    Ok(out)
}

fn compute_one(
    ogbpm: f64,
    dgbpm: f64,
    usg: f64,
    min_pct: f64,
    gp: i64,
    cohort_mean_min_pct: f64,
) -> PitCamPom {
    let usg_ratio = (usg / CAMPOM_USG_REF).max(0.0);
    let adj_o = ogbpm * usg_ratio.powf(CAMPOM_OFFENSE_EXPONENT);
    let adj_d = dgbpm * (1.0 - CAMPOM_DEFENSE_DISCOUNT * (usg / CAMPOM_USG_REF));
    let adj_gbpm = adj_o + adj_d;

    let mp_factor = if cohort_mean_min_pct > 0.0 {
        (min_pct.max(0.0) / cohort_mean_min_pct).powf(CAMPOM_MINUTES_EXPONENT)
    } else {
        0.0
    };
    let gp_weight = gp as f64 / (gp as f64 + CAMPOM_GP_K);

    PitCamPom {
        gp,
        ogbpm,
        dgbpm,
        cam_gbpm_v3_no_sos: adj_gbpm * mp_factor * gp_weight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_one_matches_python_baseline() {
        // Hand-calculated baseline matching training/compute_campom_at.py for a
        // typical high-usage player. With the constants pinned:
        //   USG_REF = 17.873577.., OFFENSE_EXPONENT = 0.7, DEFENSE_DISCOUNT = 0.1,
        //   MINUTES_EXPONENT = 0.5, GP_K = 8.
        // Inputs picked to land in the middle of the elite tier so a small
        // formula slip would shift the result visibly.
        let p = compute_one(
            5.0,  // ogbpm
            2.0,  // dgbpm
            25.0, // usg
            55.0, // min_pct
            20,   // gp
            45.0, // cohort_mean_min_pct
        );

        // adj_o = 5 * (25 / 17.87357708)^0.7 = 5 * 1.26475 = 6.3238
        // adj_d = 2 * (1 - 0.1 * 25 / 17.87357708) = 2 * 0.86013 = 1.7203
        // adj_gbpm = 6.3238 + 1.7203 = 8.0441
        // mp_factor = (55/45)^0.5 = sqrt(1.2222) = 1.10554
        // gp_weight = 20 / 28 = 0.71429
        // cam = 8.0441 * 1.10554 * 0.71429 = 6.3522
        // Cross-checked against `python3` running the Python prototype's
        // formulas with the same inputs.
        let expected = 6.3522f64;
        assert!(
            (p.cam_gbpm_v3_no_sos - expected).abs() < 0.001,
            "cam_v3 {} vs expected {}",
            p.cam_gbpm_v3_no_sos,
            expected
        );
        assert_eq!(p.gp, 20);
        assert!((p.ogbpm - 5.0).abs() < 1e-9);
        assert!((p.dgbpm - 2.0).abs() < 1e-9);
    }

    #[test]
    fn negative_usage_clamps_to_zero_in_offense_term() {
        // Sanity-check the `.max(0.0)` guard in adj_o. Negative USG shouldn't
        // happen in real data — but a corrupted row shouldn't blow up the
        // pit aggregate. `(neg / USG_REF).max(0.0).powf(0.7)` = 0.
        let p = compute_one(5.0, 2.0, -5.0, 50.0, 10, 50.0);
        // adj_o = 5 * 0^0.7 = 0
        // adj_d = 2 * (1 - 0.1 * -5/17.87) = 2 * 1.02798 = 2.0560
        // mp_factor = (50/50)^0.5 = 1.0
        // gp_weight = 10/18 = 0.5556
        // cam = 2.0560 * 1.0 * 0.5556 = 1.1422
        assert!(
            (p.cam_gbpm_v3_no_sos - 1.1422).abs() < 0.001,
            "got {}",
            p.cam_gbpm_v3_no_sos
        );
    }

    #[test]
    fn zero_cohort_mean_min_pct_yields_zero_mp_factor() {
        // Degenerate case — empty cohort. mp_factor falls back to 0 rather
        // than NaN from divide-by-zero, so cam ends up 0 rather than poisoning
        // the inference path.
        let p = compute_one(5.0, 2.0, 20.0, 50.0, 10, 0.0);
        assert_eq!(p.cam_gbpm_v3_no_sos, 0.0);
    }
}
