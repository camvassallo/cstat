//! Crossover calibration for the preseason × pit early-season blend
//! (ROADMAP §6 "Preseason × pit blend").
//!
//! The live predict route ([`crates/cstat-api/src/routes/predict.rs`]) blends a
//! preseason roster projection with the point-in-time (pit) game model on a
//! piecewise-linear schedule. This subcommand replays played seasons' games and
//! measures, on the **shared subset** where both legs exist:
//!
//!  - **preseason-only** MAE — `home_adjem − away_adjem (+ HCA)` from
//!    `team_preseason_projection`, the route's preseason leg.
//!  - **pit-only** MAE — `Predictor::predict_pit` on features rebuilt as of
//!    `game_date − 1`, exactly what the route serves for a completed game.
//!  - **blended** MAE under any schedule — the tool grid-searches the schedule
//!    shape (peak weight × decay-end day × HCA) pooled across all requested
//!    seasons to find the empirical optimum, and prints the exact constants to
//!    set in the route alongside the current schedule's MAE.
//!
//! Schedule form (matches the route): `w(d) = w_max·(1 − d/end_day)` clamped to
//! `[0, w_max]`, where `d` is days since Nov 1 of `season − 1`. The calibrated
//! shipped schedule is `w_max = 0.70`, `end_day = 42` (Nov 1 → ≈ Dec 13), HCA 3.5.
//!
//! Honesty caveats (printed with the report):
//!  - Pit features are rebuilt from `torvik_player_game_stats` up to
//!    `game_date − 1` — no end-of-season lookahead, matching the served pit path.
//!  - Needs `compute-projections --years …` to have run for each season under
//!    test (2024–2026 today).
//!  - Early-season "neutral" tournament labels in `games` are imperfect, so the
//!    HCA leg is directional.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

use cstat_core::features::build_all_features_pit;
use cstat_core::inference::Predictor;

/// Currently-shipped route constants, echoed for the comparison row. Keep in
/// sync with `predict.rs` (`PRESEASON_PEAK_WEIGHT` / `PRESEASON_DECAY_DAYS` /
/// `PRESEASON_HOME_COURT_ADVANTAGE`) so "current schedule" reflects production.
const CURRENT_HCA: f64 = 3.5;
const CURRENT_W_MAX: f64 = 0.70;
const CURRENT_END_DAY: i64 = 42;

/// Minimum shared-subset games in a week before its crossover verdict is
/// trusted — a single-game week's MAE is noise.
const MIN_WEEK_N: usize = 20;

#[derive(sqlx::FromRow)]
struct GameRow {
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    home_score: i32,
    away_score: i32,
    is_neutral: bool,
    is_conference: bool,
}

/// One game's actual margin plus both prediction legs (home perspective).
struct GamePred {
    season: i32,
    /// Days since Nov 1 of `season − 1`, clamped ≥ 0 — the schedule's x-axis.
    day_offset: i64,
    is_neutral: bool,
    actual: f64,
    pit_margin: f64,
    /// `home_adjem − away_adjem` (no HCA); `None` when either team lacks a
    /// preseason projection row.
    pre_diff: Option<f64>,
}

impl GamePred {
    /// Preseason margin at a given HCA (home perspective).
    fn pre_margin(&self, hca: f64) -> Option<f64> {
        self.pre_diff
            .map(|d| d + if self.is_neutral { 0.0 } else { hca })
    }
}

fn mae(errs: impl Iterator<Item = f64>) -> (f64, usize) {
    let mut sum = 0.0;
    let mut n = 0usize;
    for e in errs {
        sum += e.abs();
        n += 1;
    }
    if n == 0 {
        (0.0, 0)
    } else {
        (sum / n as f64, n)
    }
}

/// cstat-season `S` opens ~Nov 1 of `S−1`.
fn season_open(season: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(season - 1, 11, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 11, 1).expect("static Nov 1 is valid"))
}

/// Schedule weight on the preseason leg: linear from `w_max` at the season open
/// to 0 at `end_day` days later, then 0. Mirrors `predict.rs::preseason_blend_weight`.
fn schedule_weight(day_offset: i64, w_max: f64, end_day: i64) -> f64 {
    if end_day <= 0 {
        return 0.0;
    }
    let d = day_offset.max(0) as f64;
    (w_max * (1.0 - d / end_day as f64)).clamp(0.0, w_max)
}

/// Blended-margin MAE over a slice of games under a full schedule (HCA, peak
/// weight, decay-end day). Only games with both legs contribute.
fn schedule_mae(games: &[&GamePred], hca: f64, w_max: f64, end_day: i64) -> (f64, usize) {
    mae(games.iter().filter_map(|g| {
        let pre = g.pre_margin(hca)?;
        let w = schedule_weight(g.day_offset, w_max, end_day);
        Some((w * pre + (1.0 - w) * g.pit_margin) - g.actual)
    }))
}

/// Best *constant* blend weight + its MAE (no schedule), at a fixed HCA.
fn best_constant_w(games: &[&GamePred], hca: f64) -> (f64, f64) {
    let mut best = (0.0_f64, f64::MAX);
    for i in 0..=20 {
        let w = i as f64 * 0.05;
        let (m, n) = mae(games.iter().filter_map(|g| {
            let pre = g.pre_margin(hca)?;
            Some((w * pre + (1.0 - w) * g.pit_margin) - g.actual)
        }));
        if n > 0 && m < best.1 {
            best = (w, m);
        }
    }
    best
}

/// Per-week oracle MAE: the unattainable ceiling where each (season, week)
/// bucket uses its own best constant weight. Bounds what any date schedule can do.
fn per_week_oracle_mae(games: &[&GamePred], hca: f64) -> f64 {
    let mut buckets: BTreeMap<(i32, i64), Vec<&GamePred>> = BTreeMap::new();
    for g in games {
        buckets
            .entry((g.season, g.day_offset / 7))
            .or_default()
            .push(g);
    }
    let mut sum_abs = 0.0;
    let mut n = 0usize;
    for gs in buckets.values() {
        let (w, _) = best_constant_w(gs, hca);
        for g in gs {
            if let Some(pre) = g.pre_margin(hca) {
                sum_abs += ((w * pre + (1.0 - w) * g.pit_margin) - g.actual).abs();
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { sum_abs / n as f64 }
}

/// Run point-in-time inference over one season's completed games, returning the
/// per-game predictions (with day offsets). Prints a per-season summary line.
async fn analyze_season(pool: &PgPool, predictor: &Predictor, year: i32) -> Result<Vec<GamePred>> {
    let proj_rows: Vec<(Uuid, f32)> = sqlx::query_as(
        "SELECT team_id, projected_adj_em FROM team_preseason_projection WHERE season = $1",
    )
    .bind(year)
    .fetch_all(pool)
    .await
    .context("fetch team_preseason_projection")?;
    let proj: HashMap<Uuid, f64> = proj_rows.into_iter().map(|(t, v)| (t, v as f64)).collect();
    if proj.is_empty() {
        anyhow::bail!(
            "no team_preseason_projection rows for season {year}. Run \
             `cargo run --bin cstat-ingest -- compute-projections --years {year}` first \
             (played seasons with portal+recruit data; 2024–2026 today)."
        );
    }

    let games: Vec<GameRow> = sqlx::query_as(
        "SELECT g.game_date, g.home_team_id, g.away_team_id, \
                g.home_score, g.away_score, \
                g.is_neutral_site AS is_neutral, \
                COALESCE(g.is_conference, false) AS is_conference \
         FROM games g \
         WHERE g.season = $1 AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL \
           AND g.home_team_id IS NOT NULL AND g.away_team_id IS NOT NULL \
         ORDER BY g.game_date",
    )
    .bind(year)
    .fetch_all(pool)
    .await
    .context("fetch completed games")?;
    if games.is_empty() {
        anyhow::bail!("no completed games for season {year} — pick a played season (2024–2026).");
    }
    let total = games.len();
    let open = season_open(year);
    println!(
        "  season {year}: {} projection rows, {total} games — running pit inference…",
        proj.len(),
    );

    let mut preds: Vec<GamePred> = Vec::with_capacity(total);
    let mut skipped = 0usize;
    for (i, g) in games.iter().enumerate() {
        if i > 0 && i % 1000 == 0 {
            println!("    … {i}/{total}");
        }
        let as_of = g.game_date.pred_opt().unwrap_or(g.game_date);
        let feats = match build_all_features_pit(
            pool,
            g.home_team_id,
            g.away_team_id,
            year,
            g.is_neutral,
            g.is_conference,
            as_of,
        )
        .await
        {
            Ok(f) => f,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let pit = match predictor.predict_pit(&feats.diff, &feats.diff_and_sum) {
            Ok(p) => p.predicted_margin as f64,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let pre_diff = match (proj.get(&g.home_team_id), proj.get(&g.away_team_id)) {
            (Some(h), Some(a)) => Some(h - a),
            _ => None,
        };
        preds.push(GamePred {
            season: year,
            day_offset: (g.game_date - open).num_days().max(0),
            is_neutral: g.is_neutral,
            actual: (g.home_score - g.away_score) as f64,
            pit_margin: pit,
            pre_diff,
        });
    }

    let shared: Vec<&GamePred> = preds.iter().filter(|g| g.pre_diff.is_some()).collect();
    let (pre, _) = mae(shared
        .iter()
        .filter_map(|g| g.pre_margin(CURRENT_HCA).map(|p| p - g.actual)));
    let (pit, _) = mae(shared.iter().map(|g| g.pit_margin - g.actual));
    let (cw, cw_mae) = best_constant_w(&shared, CURRENT_HCA);
    println!(
        "    {} scored, {skipped} skipped, {} shared | pre-only {pre:.2}  pit-only {pit:.2}  \
         best-const w={cw:.2} → {cw_mae:.2}",
        preds.len(),
        shared.len(),
    );
    Ok(preds)
}

/// Print the per-week table for a single season (validation of the decay shape).
fn print_week_table(preds: &[GamePred], year: i32) {
    let mut weeks: BTreeMap<i64, (NaiveDate, Vec<&GamePred>)> = BTreeMap::new();
    let open = season_open(year);
    for g in preds {
        let idx = g.day_offset / 7;
        weeks
            .entry(idx)
            .or_insert_with(|| (open + chrono::Duration::days(idx * 7), Vec::new()))
            .1
            .push(g);
    }
    println!(
        "\n  per-week (season {year}, shared subset, HCA {CURRENT_HCA:.1}):\n  {:<12} {:>4} {:>4}  {:>7} {:>7}  {:>7} {:>6}",
        "week-of", "all", "both", "pre-MAE", "pit-MAE", "best", "best-w",
    );
    println!("  {}", "-".repeat(58));
    let mut crossover: Option<NaiveDate> = None;
    for (start, gs) in weeks.values() {
        let both: Vec<&GamePred> = gs
            .iter()
            .filter(|g| g.pre_diff.is_some())
            .copied()
            .collect();
        let (pre, n) = mae(both
            .iter()
            .filter_map(|g| g.pre_margin(CURRENT_HCA).map(|p| p - g.actual)));
        let (pit, _) = mae(both.iter().map(|g| g.pit_margin - g.actual));
        let (bw, bm) = best_constant_w(&both, CURRENT_HCA);
        if crossover.is_none() && n >= MIN_WEEK_N && pit <= pre {
            crossover = Some(*start);
        }
        if n > 0 {
            println!(
                "  {:<12} {:>4} {:>4}  {pre:>7.2} {pit:>7.2}  {bm:>7.2} {bw:>6.2}",
                start.to_string(),
                gs.len(),
                n,
            );
        }
    }
    if let Some(d) = crossover {
        println!("    crossover (pit ≤ preseason): week of {d}");
    }
}

pub async fn run(pool: &PgPool, predictor: &Predictor, years: &[i32]) -> Result<()> {
    println!("{}", "=".repeat(78));
    println!("preseason × pit blend calibration — seasons {years:?}");
    println!("{}", "=".repeat(78));

    let mut pooled: Vec<GamePred> = Vec::new();
    for &y in years {
        let p = analyze_season(pool, predictor, y).await?;
        if years.len() == 1 {
            print_week_table(&p, y);
        }
        pooled.extend(p);
    }

    let shared: Vec<&GamePred> = pooled.iter().filter(|g| g.pre_diff.is_some()).collect();
    if shared.is_empty() {
        anyhow::bail!("no games had both legs — nothing to calibrate");
    }

    // ---- HCA sweep (preseason-only, non-neutral, default schedule-free) ----
    println!("\n  HCA sweep (preseason-only MAE, non-neutral games):");
    let home_games: Vec<&GamePred> = shared.iter().copied().filter(|g| !g.is_neutral).collect();
    let mut best_hca_solo = (CURRENT_HCA, f64::MAX);
    for i in 0..=12 {
        let h = i as f64 * 0.5;
        let (m, n) = mae(home_games
            .iter()
            .filter_map(|g| g.pre_margin(h).map(|p| p - g.actual)));
        if n > 0 && m < best_hca_solo.1 {
            best_hca_solo = (h, m);
        }
        if i % 2 == 0 {
            println!("    HCA {h:>4.1}   MAE {m:>6.2}");
        }
    }
    println!("    → preseason-leg HCA optimum {:.1}", best_hca_solo.0);

    // ---- Schedule grid search (the calibration) ----
    println!("\n  schedule grid search (pooled, blended MAE on shared subset):");
    let hcas = [1.0, 1.5, 2.0, 2.5, 3.0, 3.5];
    let end_days = [14_i64, 21, 28, 35, 42, 49, 56, 63, 75];
    let mut best = (CURRENT_HCA, CURRENT_W_MAX, CURRENT_END_DAY, f64::MAX);
    for &hca in &hcas {
        for &end_day in &end_days {
            for i in 7..=17 {
                let w_max = i as f64 * 0.05; // 0.35 … 0.85
                let (m, n) = schedule_mae(&shared, hca, w_max, end_day);
                if n > 0 && m < best.3 {
                    best = (hca, w_max, end_day, m);
                }
            }
        }
    }
    let (b_hca, b_wmax, b_end, b_mae) = best;
    let b_end_date = season_open(years[years.len() - 1]) + chrono::Duration::days(b_end);

    // ---- Reference points for context ----
    let (pit_only, _) = mae(shared.iter().map(|g| g.pit_margin - g.actual));
    let (cur_mae, _) = schedule_mae(&shared, CURRENT_HCA, CURRENT_W_MAX, CURRENT_END_DAY);
    let (old_mae, _) = schedule_mae(&shared, 3.5, 1.0, 75); // pre-calibration default
    let (const_w, const_mae) = best_constant_w(&shared, b_hca);
    let oracle = per_week_oracle_mae(&shared, b_hca);

    println!("    pit-only                          MAE {pit_only:>6.2}",);
    println!("    pre-calibration (w=1.0 end=75 HCA=3.5) MAE {old_mae:>6.2}",);
    println!(
        "    current route  (w={CURRENT_W_MAX:.2} end={CURRENT_END_DAY} HCA={CURRENT_HCA:.1}) MAE {cur_mae:>6.2}",
    );
    println!("    best constant w={const_w:.2}             MAE {const_mae:>6.2}",);
    println!("    ★ best schedule w_max={b_wmax:.2} end={b_end} HCA={b_hca:.1}  MAE {b_mae:>6.2}",);
    println!("    per-week oracle (ceiling)         MAE {oracle:>6.2}",);

    // ---- Per-season stability of the chosen optimum ----
    println!("\n  per-season MAE under the best schedule (stability check):");
    for &y in years {
        let s: Vec<&GamePred> = shared.iter().copied().filter(|g| g.season == y).collect();
        let (opt, _) = schedule_mae(&s, b_hca, b_wmax, b_end);
        let (pit, _) = mae(s.iter().map(|g| g.pit_margin - g.actual));
        let (cur, _) = schedule_mae(&s, CURRENT_HCA, CURRENT_W_MAX, CURRENT_END_DAY);
        println!("    {y}: best {opt:>6.2}   (pit-only {pit:.2}, current {cur:.2})");
    }

    println!("\n  {}", "-".repeat(74));
    println!(
        "  ★ Optimum: PRESEASON_HOME_COURT_ADVANTAGE = {b_hca:.1}, peak weight {b_wmax:.2}, \
         decay to 0 over {b_end} days (≈ {b_end_date}, season-relative)."
    );
    println!(
        "    Set in `predict.rs::preseason_blend_weight` (w_max + end day) and \
         `PRESEASON_HOME_COURT_ADVANTAGE`, mirror in this module's CURRENT_* consts."
    );
    println!(
        "    Caveats: pit features are honest point-in-time; early-season 'neutral' \
         labels are imperfect (HCA directional); schedule is linear-from-open, the \
         oracle row bounds the residual headroom from a fancier shape."
    );
    Ok(())
}
