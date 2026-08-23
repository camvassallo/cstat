//! End-to-end backtest for the roster-impact projection
//! pipeline (ROADMAP §5b).
//!
//! For each target season, composes the projected rosters exactly as the
//! `/api/projections` route does (returning ∪ incoming portal ∪
//! recruits), feeds every returner / arrival their *projected* cam_v3
//! (held-out trajectory OOF where available, live trajectory inference
//! otherwise; recruits carry the freshman model's prediction), scores
//! each team with `roster_impact_model.onnx`, and compares to the actual
//! `team_season_stats.adj_efficiency_margin` the season finished with.
//!
//! Two predictions per team, both measured against the same actual:
//!  - **roster-impact** — the leave-one-season-out roster-impact model scored on
//!    the projected-cam_v3 roster.
//!  - **baseline-persistence** — target AdjEM ≈ base-season AdjEM.
//!
//! (The original §5b acceptance comparison also scored the *former*
//! box-score pipeline — `project_rotation` → `build_roster_features` →
//! `predict_adj_em` — to confirm roster-impact beat the model it replaced.
//! That comparison was dropped once tiers were deprecated: the box-score
//! model reads a freshman box-score statline that no longer exists, so it
//! can't be fed projected rosters. Its acceptance verdict was long since
//! settled when roster-impact shipped.)
//!
//! The blend sweep informs the live route's recalibration — after the v2
//! OOF retrain it blends `0.50·baseline + 0.50·raw` with no offset.
//!
//! Honesty caveats (printed with the report):
//!  - The roster-impact model is the **leave-one-season-out** model for
//!    each target season (`roster_impact_loso/roster_impact_model_{year}
//!    .onnx`), trained on every season *except* the one being scored — so
//!    there is no in-sample leak from the roster model itself. This is the
//!    ROADMAP §5b v2 tightening; the live `/api/projections` route still
//!    uses the all-seasons model, correct there because the live target
//!    year is genuinely unseen.
//!  - Recruit cam_v3 comes from `compose_all_projections`, which runs
//!    live freshman inference — mildly in-sample for the freshman model
//!    on historical targets.
//!  - Uncertain (declared-draft) cohort is assumed empty: the 2024 /
//!    2025 base seasons have no `early_entrants.json`, so floor == ceiling.

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

use cstat_core::inference::{Predictor, RosterImpactModel};
use cstat_core::roster_impact::{apply_projected_cam_v3, build_roster_impact_features};
use cstat_core::roster_projection::{
    DraftScenario, compose_all_projections, fetch_draft_entrants, fetch_player_departures,
    project_returner_cam_v3, retained_talent_fraction, transition_shrink_weight,
};

/// Minimum (returning + arrivals + recruits) to score a team — mirrors
/// `MIN_QUALIFYING_FOR_PROJECTION` in `routes/projections.rs`.
const MIN_QUALIFYING: usize = 7;

/// Documented reference point (`docs/projections_methodology.md`).
const REF_BASELINE_MAE: f64 = 6.53;

struct Stats {
    n: usize,
    mae: f64,
    bias: f64,
    rmse: f64,
    r2: f64,
}

fn compute_stats(preds: &[f64], actuals: &[f64]) -> Stats {
    let n = preds.len();
    if n == 0 {
        return Stats {
            n: 0,
            mae: 0.0,
            bias: 0.0,
            rmse: 0.0,
            r2: 0.0,
        };
    }
    let nf = n as f64;
    let mae = preds
        .iter()
        .zip(actuals)
        .map(|(p, a)| (p - a).abs())
        .sum::<f64>()
        / nf;
    let bias = preds.iter().zip(actuals).map(|(p, a)| p - a).sum::<f64>() / nf;
    let ss_res: f64 = preds
        .iter()
        .zip(actuals)
        .map(|(p, a)| (p - a).powi(2))
        .sum();
    let rmse = (ss_res / nf).sqrt();
    let amean = actuals.iter().sum::<f64>() / nf;
    let ss_tot: f64 = actuals.iter().map(|a| (a - amean).powi(2)).sum();
    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };
    Stats {
        n,
        mae,
        bias,
        rmse,
        r2,
    }
}

/// One team's two predictions plus its actual AdjEM, and the turnover key the
/// serving blend would have used on it.
struct TeamResult {
    team_id: Uuid,
    team_name: String,
    season: i32,
    roster_proj: f64,
    baseline: f64,
    actual: f64,
    /// [`retained_talent_fraction`] for this composed roster — the EX-ANTE
    /// turnover signal, known in the offseason from the four departure
    /// channels. `None` for a roster with no cam coverage at all.
    ///
    /// Dumped because `transition_blend_diagnostic.py` used to reconstruct a
    /// *hindsight* proxy for it (which of last season's players actually
    /// appear in the target season's box scores), and then tuned the served
    /// weights against cohorts the serving code could not reproduce — it
    /// cannot see next season's box scores in August, and injuries and
    /// midseason attrition move the hindsight number for reasons that have
    /// nothing to do with roster construction. Carrying the real key here
    /// closes that gap.
    retained: Option<f64>,
    /// The blend weight [`transition_shrink_weight`] hands the serving path
    /// for this roster. Redundant with `retained` (it is a pure function of
    /// it plus two constants), dumped anyway so a residual analysis can score
    /// what was actually served without re-implementing the ramp in Python.
    baseline_weight: f64,
}

/// Fetch `team_season_stats.adj_efficiency_margin` for a season, keyed by
/// team_id (rows with NULL AdjEM dropped). Used for the base-season
/// baseline — `ProjectedRoster.team_id` is a base-season UUID.
async fn fetch_adj_em(pool: &PgPool, season: i32) -> Result<HashMap<Uuid, f64>> {
    let rows: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT team_id, adj_efficiency_margin
         FROM team_season_stats
         WHERE season = $1 AND adj_efficiency_margin IS NOT NULL",
    )
    .bind(season)
    .fetch_all(pool)
    .await
    .with_context(|| format!("fetch adj_efficiency_margin for season {season}"))?;
    Ok(rows.into_iter().collect())
}

/// Fetch the *target* season's actual AdjEM, keyed by the **base-season**
/// team_id. UUIDs are season-scoped, so `ProjectedRoster.team_id` (a
/// base-season UUID) can't index a target-season-keyed map directly —
/// the join bridges the two via the cross-season `natstat_id`.
async fn fetch_actual_adj_em(
    pool: &PgPool,
    base_season: i32,
    target_season: i32,
) -> Result<HashMap<Uuid, f64>> {
    let rows: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT t_base.id, tss.adj_efficiency_margin
         FROM teams t_base
         JOIN teams t_tgt
           ON t_tgt.natstat_id = t_base.natstat_id AND t_tgt.season = $2
         JOIN team_season_stats tss
           ON tss.team_id = t_tgt.id AND tss.season = $2
         WHERE t_base.season = $1 AND tss.adj_efficiency_margin IS NOT NULL",
    )
    .bind(base_season)
    .bind(target_season)
    .fetch_all(pool)
    .await
    .with_context(|| format!("fetch actual adj_efficiency_margin {base_season}→{target_season}"))?;
    Ok(rows.into_iter().collect())
}

/// Run the backtest for one target season. Returns the per-team results
/// (teams with no actual AdjEM, or below the qualifying gate, are omitted).
async fn backtest_year(
    pool: &PgPool,
    predictor: &Predictor,
    loso_model: &RosterImpactModel,
    year: i32,
) -> Result<Vec<TeamResult>> {
    let base_season = year - 1;

    // Firm draft departures from the `draft_entrants` table (historical lists
    // built from Tankathon past-drafts). Without these, drafted players are
    // miscounted as returning and draft-factory teams are over-projected.
    let entrants = fetch_draft_entrants(pool, base_season).await?;
    if !entrants.is_empty() {
        println!(
            "  note: {} draft entrants loaded for base {base_season} — \
             backtest uses the Ceiling scenario",
            entrants.len(),
        );
    }

    // Curated non-portal, non-draft exits. Historically sparse (the capture
    // starts at base 2026), so this is a no-op for most backtest folds.
    let departures = fetch_player_departures(pool, base_season).await?;

    let projections = compose_all_projections(
        pool,
        base_season,
        &entrants,
        &departures,
        predictor,
        crate::target_season_retro_complete(year),
    )
    .await
    .with_context(|| format!("compose_all_projections base {base_season}"))?;
    let baseline_map = fetch_adj_em(pool, base_season).await?;
    let actual_map = fetch_actual_adj_em(pool, base_season, year).await?;

    let mut results = Vec::new();
    let mut skipped_thin = 0_usize;
    let mut skipped_no_actual = 0_usize;
    let mut skipped_no_baseline = 0_usize;

    for p in &projections {
        let qualifying = p.returning.len() + p.arrivals.len() + p.recruits.len();
        if qualifying < MIN_QUALIFYING {
            skipped_thin += 1;
            continue;
        }
        let Some(&actual) = actual_map.get(&p.team_id) else {
            skipped_no_actual += 1;
            continue;
        };
        // Require a base-season AdjEM: baseline-persistence and the box-score model's
        // blend are both undefined without it. Skipping keeps all three
        // predictors measured on an identical team set (the no-baseline
        // cohort is a handful of D-I-transition teams).
        let Some(&baseline) = baseline_map.get(&p.team_id) else {
            skipped_no_baseline += 1;
            continue;
        };

        // --- roster-impact: projected cam_v3 → LOSO impact-aggregation model. -
        let mut traj_ids: Vec<Uuid> = Vec::new();
        traj_ids.extend(p.returning.iter().map(|r| r.player_id));
        traj_ids.extend(p.arrivals.iter().map(|a| a.player_id));
        traj_ids.extend(p.uncertain.iter().map(|(row, _)| row.player_id));
        let projected_cam = project_returner_cam_v3(pool, predictor, &traj_ids, year).await?;

        let mut roster_b = p.for_scenario(DraftScenario::Ceiling);
        apply_projected_cam_v3(&mut roster_b, &projected_cam);
        let roster_proj = loso_model
            .predict(&build_roster_impact_features(
                &roster_b,
                p.outbound_cam_v3_sum,
                p.inbound_cam_v3_sum,
            ))
            .map_err(|e| anyhow::anyhow!("LOSO roster-impact predict ({}): {e}", p.team_name))?;

        results.push(TeamResult {
            team_id: p.team_id,
            team_name: p.team_name.clone(),
            season: year,
            roster_proj: roster_proj as f64,
            baseline,
            actual,
            retained: retained_talent_fraction(p).map(f64::from),
            baseline_weight: f64::from(transition_shrink_weight(p)),
        });
    }

    println!(
        "  {year}: scored {} teams  (skipped {skipped_thin} too-thin, \
         {skipped_no_actual} no actual AdjEM, {skipped_no_baseline} no base-season AdjEM)",
        results.len(),
    );
    Ok(results)
}

fn print_block(label: &str, s: &Stats) {
    println!(
        "    {label:<22} MAE {:>6.2}  bias {:>+6.2}  RMSE {:>6.2}  R² {:>6.3}  n={}",
        s.mae, s.bias, s.rmse, s.r2, s.n,
    );
}

fn report(label: &str, results: &[TeamResult]) {
    let actuals: Vec<f64> = results.iter().map(|r| r.actual).collect();
    let roster_proj = compute_stats(
        &results.iter().map(|r| r.roster_proj).collect::<Vec<_>>(),
        &actuals,
    );
    let baseline = compute_stats(
        &results.iter().map(|r| r.baseline).collect::<Vec<_>>(),
        &actuals,
    );
    println!("\n  {label}");
    print_block("roster_proj (impact, was roster-impact)", &roster_proj);
    print_block("baseline-persistence", &baseline);
}

/// Sweep the baseline blend weight on the roster-impact model's raw output and report the
/// MAE curve. `pred = w·baseline + (1−w)·phaseB_raw`. This is the PR 2
/// recalibration in miniature — roster-impact raw is a strong raw signal,
/// so the optimum sits at a low `w`. Returns `(best_w, best_mae)`.
fn blend_sweep(results: &[TeamResult]) -> (f64, f64) {
    let actuals: Vec<f64> = results.iter().map(|r| r.actual).collect();
    println!("\n  roster-impact blended  (pred = w·baseline + (1−w)·phaseB_raw):");
    let mut best = (0.0_f64, f64::MAX);
    for i in 0..=20 {
        let w = i as f64 * 0.05;
        let preds: Vec<f64> = results
            .iter()
            .map(|r| w * r.baseline + (1.0 - w) * r.roster_proj)
            .collect();
        let s = compute_stats(&preds, &actuals);
        if s.mae < best.1 {
            best = (w, s.mae);
        }
        if i % 2 == 0 {
            println!(
                "    w={w:.2}   MAE {:>6.2}   bias {:>+6.2}   R² {:>6.3}",
                s.mae, s.bias, s.r2
            );
        }
    }
    println!("    → best: w={:.2}  MAE {:.2}", best.0, best.1);
    (best.0, best.1)
}

/// Run the backtest across `years` and print the report. Returns Ok even
/// when roster-impact underperforms — this is a diagnostic, not a CI gate; the
/// printed verdict carries the conclusion.
///
/// When `output_path` is `Some`, also dumps per-team predictions to a
/// JSON file (one record per scored team) so downstream audit scripts can
/// join projection error against per-team explanatory variables. The
/// dump is the full pre-pooling cohort — same row set the report blocks
/// summarize.
pub async fn run(
    pool: &PgPool,
    predictor: &Predictor,
    model_dir: &Path,
    years: &[i32],
    output_path: Option<&Path>,
) -> Result<()> {
    println!("{}", "=".repeat(72));
    println!("roster-impact projection backtest — target seasons: {years:?}");
    println!("{}", "=".repeat(72));

    // Per-target-season leave-one-season-out roster-impact models — each
    // trained on every season except the one it scores, so the backtest
    // carries no in-sample leak from the roster model (ROADMAP §5b v2).
    let loso_dir = model_dir.join("roster_impact_loso");
    let mut loso: HashMap<i32, RosterImpactModel> = HashMap::new();
    for &year in years {
        let path = loso_dir.join(format!("roster_impact_model_{year}.onnx"));
        if !path.exists() {
            anyhow::bail!(
                "LOSO backtest model {} not found. Run \
                 `cd training && python3 train_roster_impact_model.py` to \
                 generate the per-season models — only backtestable seasons \
                 (those with portal data + a finished actual AdjEM; 2025 and \
                 2026 today) are exported.",
                path.display(),
            );
        }
        let model = RosterImpactModel::load_loso(model_dir, year)
            .map_err(|e| anyhow::anyhow!("load LOSO roster-impact model for {year}: {e}"))?;
        loso.insert(year, model);
    }

    let mut pooled: Vec<TeamResult> = Vec::new();
    for &year in years {
        let yr = backtest_year(pool, predictor, &loso[&year], year).await?;
        report(&format!("season {year}"), &yr);
        pooled.extend(yr);
    }

    if years.len() > 1 {
        report("POOLED", &pooled);
    }

    // Verdict against the pooled numbers: roster-impact raw, the best
    // blended roster-impact (the blend sweep is the PR 2 recalibration in
    // miniature), and baseline-persistence.
    let actuals: Vec<f64> = pooled.iter().map(|r| r.actual).collect();
    let b = compute_stats(
        &pooled.iter().map(|r| r.roster_proj).collect::<Vec<_>>(),
        &actuals,
    );
    let baseline = compute_stats(
        &pooled.iter().map(|r| r.baseline).collect::<Vec<_>>(),
        &actuals,
    );
    let (best_w, best_mae) = blend_sweep(&pooled);

    println!("\n{}", "-".repeat(72));
    println!(
        "  roster-impact raw pooled MAE {:.2} (bias {:+.2}); baseline-persistence \
         {:.2} (live) / {REF_BASELINE_MAE:.2} (documented).",
        b.mae, b.bias, baseline.mae,
    );
    println!("  roster-impact *blended* best MAE {best_mae:.2} at w={best_w:.2}.");
    if best_mae <= baseline.mae {
        println!(
            "  ✓ Best-blended roster-impact ({best_mae:.2}) beats baseline-persistence ({:.2}).",
            baseline.mae,
        );
    } else {
        println!(
            "  ✗ Best-blended roster-impact ({best_mae:.2}) is {:.2} MAE short of baseline-persistence ({:.2}).",
            best_mae - baseline.mae,
            baseline.mae,
        );
    }
    println!(
        "  roster-impact raw bias {:+.2} — near-zero, so PR 2's blend needs little/no offset.",
        b.bias,
    );
    println!("{}", "-".repeat(72));
    println!(
        "  Caveats: roster-impact model is now leave-one-season-out (no \
         in-sample leak from the roster model); recruit cam_v3 still uses \
         live freshman inference (mildly in-sample). See module docs.",
    );

    // Which models scored this backtest (#238). The LOSO set is the whole
    // point here: it is gitignored, so it never appears in `git status`, and a
    // set drawn from a different frame than the committed serving model
    // produces a dump that looks fine and grades coaches against a projection
    // generation that no longer ships.
    let provenance = cstat_core::provenance::layer3_provenance(
        model_dir,
        "cstat-ingest projections-backtest",
        &[cstat_core::provenance::ROSTER_IMPACT],
        true,
    )?;

    if let Some(path) = output_path {
        dump_per_team_json(path, &pooled, &provenance)?;
        println!("  wrote per-team dump → {}", path.display());

        // Also recorded in the database, keyed by dump filename, so the record
        // survives the dump being deleted — these files are gitignored and one
        // once accounted for 89% of a PR's diff, so they do get cleaned up.
        let key = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        sqlx::query(
            r#"
            INSERT INTO artifact_provenance (artifact, artifact_key, provenance, computed_at)
            VALUES ('projections_backtest_dump', $1, $2, now())
            ON CONFLICT (artifact, artifact_key) DO UPDATE SET
                provenance  = EXCLUDED.provenance,
                computed_at = now()
            "#,
        )
        .bind(key)
        .bind(&provenance)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Dump per-team predictions to JSON for downstream residual analysis.
///
/// Schema (since #238): `{"provenance": {...}, "teams": [...]}`, where `teams`
/// is the flat array of `{team_id, team_name, season, roster_proj, baseline,
/// actual}` records this file has always carried — one per scored team, plus
/// (since #322) the ex-ante `retained` turnover fraction and the
/// `baseline_weight` the serving ramp derives from it. Floats
/// kept at their native f64 precision; downstream audit code handles rounding.
///
/// The envelope is what lets a consumer say *which projection generation these
/// residuals came from*. `compute_cae.py`, `transition_blend_diagnostic.py`,
/// `pit_cae_backtest.py` and `pit_program_calibration.py` all read these dumps
/// through `load_backtest()`, which accepts both this shape and the historical
/// bare array — the same back-compat approach as the `phase_b` -> `roster_proj`
/// rename shim, so existing dumps need no regeneration.
fn dump_per_team_json(path: &Path, results: &[TeamResult], provenance: &Value) -> Result<()> {
    use serde_json::json;
    let arr: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "team_id": r.team_id,
                "team_name": r.team_name,
                "season": r.season,
                "roster_proj": r.roster_proj,
                "baseline": r.baseline,
                "actual": r.actual,
                "retained": r.retained,
                "baseline_weight": r.baseline_weight,
            })
        })
        .collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let f = std::fs::File::create(path)
        .with_context(|| format!("create dump file {}", path.display()))?;
    serde_json::to_writer_pretty(f, &json!({ "provenance": provenance, "teams": arr }))
        .with_context(|| format!("write JSON dump {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod dump_format_tests {
    use super::*;
    use serde_json::json;

    fn one_result() -> TeamResult {
        TeamResult {
            team_id: Uuid::nil(),
            team_name: "Duke Blue Devils".to_string(),
            season: 2026,
            roster_proj: 20.0,
            baseline: 18.0,
            actual: 22.0,
            retained: Some(0.55),
            baseline_weight: 0.45,
        }
    }

    /// The dump envelope is a cross-language format contract: Rust writes it,
    /// four Python consumers read it through `compute_cae.read_dump_records` /
    /// `load_backtest`. Nothing else pins the key names, so a rename here would
    /// surface as a `KeyError` in a diagnostic script months later.
    #[test]
    fn dump_has_a_provenance_envelope_around_the_team_array() {
        let dir = std::env::temp_dir().join(format!("cstat_dump_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dump.json");

        dump_per_team_json(&path, &[one_result()], &json!({"produced_by": "test"})).unwrap();

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["provenance"]["produced_by"], "test");
        let teams = v["teams"].as_array().expect("`teams` must be an array");
        assert_eq!(teams.len(), 1);
        // The per-team keys downstream reads by name.
        for key in [
            "team_id",
            "team_name",
            "season",
            "roster_proj",
            "baseline",
            "actual",
            "retained",
            "baseline_weight",
        ] {
            assert!(teams[0].get(key).is_some(), "dump row lost the `{key}` key");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
