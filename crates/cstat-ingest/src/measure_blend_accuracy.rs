//! Calibration for the preseason × pit early-season blend (ROADMAP §6,
//! #390).
//!
//! Every served early-season prediction mixes two legs:
//!
//!  - the **preseason leg**, `slope · (home_adjem − away_adjem) + HCA` from
//!    `team_preseason_projection` ([`cstat_core::projection::preseason_leg_margin`]);
//!  - the **model leg**, the point-in-time game model on features as of
//!    `game_date − 1` — the same leg the nightly `game_projections` sweep
//!    blends, computed by the same batch code ([`crate::game_projections::model_legs`]).
//!
//! on the schedule `w(d) = w_max·(1 − d/end_day)` clamped to `[0, w_max]`,
//! where `d` is days since Nov 1 of `season − 1`.
//!
//! This subcommand replays played seasons and fits that whole family —
//! `slope × hca × w_max × end_day` — on the **shared subset** where both legs
//! exist. Until #390 the grid had no slope axis, so whether the leg's scale
//! was right in the blend was a question the tool could not answer. It
//! reports:
//!
//!  1. **The leg alone**, against actual margins — badly scaled at 1.0, best
//!     near 0.60, which is what the preseason-only regime (#387) serves.
//!  2. **The blend**, pooled and walk-forward (fit on seasons < S, score S):
//!     the shipped constants against a schedule refit with the leg held at
//!     shipped, a schedule refit around the leg-alone calibration, and a free
//!     joint fit, each with a paired z. #390's answer: the blend wants the
//!     unit-scale leg, because its over-statement cancels a model leg that is
//!     compressed in the first three weeks (see `PRESEASON_SLOPE`).
//!  3. **The per-week shape** under the shipped constants — where the model
//!     leg overtakes the preseason one.
//!
//! Only games inside the first [`HORIZON_DAYS`] days are scored. Past the
//! longest decay on the grid every candidate gives the model leg weight 1, so
//! later games add the same error to every row and move no decision.
//!
//! Honesty caveats (printed with the report):
//!  - The model leg's CamPom channel is point-in-time, but its team stats,
//!    roster aggregates and rolling form are season aggregates (#274), so for
//!    a historical November game the leg has seen the whole season. That makes
//!    it look better early than it will live, which biases the fitted weight
//!    on the preseason leg **down**.
//!  - Historical `team_preseason_projection` rows are in-sample at Layer 2, so
//!    the preseason leg looks somewhat better than it will live, biasing the
//!    weight **up**. The two caveats push in opposite directions.
//!  - Needs `compute-projections --years …` to have run for each season.
//!  - **Completed-game path only.** The model leg replayed here is the
//!    `BlendClock::AsOf` one. Unplayed games blend on `BlendClock::Live` over
//!    the prod bundle and that morning's season-aggregate tables, which this
//!    replay cannot reconstruct; the fitted constants are applied there by
//!    inference.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

use cstat_core::inference::Predictor;
use cstat_core::projection::{
    PRESEASON_DECAY_DAYS, PRESEASON_HCA, PRESEASON_PEAK_WEIGHT, PRESEASON_SLOPE,
};

/// Games scored: the first 12 weeks from the Nov 1 open. Covers the longest
/// decay on the grid, so a schedule's score is fully determined inside it.
const HORIZON_DAYS: i64 = 84;

/// Minimum shared-subset games in a week before its crossover verdict is
/// trusted — a single-game week's MAE is noise.
const MIN_WEEK_N: usize = 20;

/// Projected gap beyond which a game counts as a mismatch in the bias report.
/// The unit-scale leg's error was concentrated there (+6.6 points at 15).
const WIDE_GAP: f64 = 15.0;

/// The shipped constants, read from `projection.rs` rather than mirrored so
/// the "current" row cannot drift from production.
fn shipped() -> Schedule {
    Schedule {
        slope: PRESEASON_SLOPE as f64,
        hca: PRESEASON_HCA as f64,
        w_max: PRESEASON_PEAK_WEIGHT as f64,
        end_day: PRESEASON_DECAY_DAYS,
    }
}

/// The preseason-only regime's leg (`PRESEASON_ONLY_SLOPE` / `_HCA` in
/// `projection.rs`, private there) — the leg calibrated to be right *alone*.
/// The alternative #390 proposed for the blend, kept as a reference so the
/// report always shows what dropping it in would cost.
const LEG_ALONE: (f64, f64) = (0.59, 3.2);

fn leg_alone_at_shipped() -> Schedule {
    Schedule {
        slope: LEG_ALONE.0,
        hca: LEG_ALONE.1,
        ..shipped()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Schedule {
    slope: f64,
    hca: f64,
    w_max: f64,
    end_day: i64,
}

impl Schedule {
    fn label(&self) -> String {
        format!(
            "slope {:.2} HCA {:.1} w {:.2} end {:>2}",
            self.slope, self.hca, self.w_max, self.end_day
        )
    }
}

#[derive(sqlx::FromRow)]
struct GameRow {
    id: Uuid,
    game_date: NaiveDate,
    home_team_id: Uuid,
    away_team_id: Uuid,
    home_score: i32,
    away_score: i32,
    is_neutral: bool,
}

/// One game's actual margin plus both prediction legs (home perspective).
struct GamePred {
    season: i32,
    /// Days since Nov 1 of `season − 1`, clamped ≥ 0 — the schedule's x-axis.
    day_offset: i64,
    is_neutral: bool,
    actual: f64,
    pit_margin: f64,
    /// `home_adjem − away_adjem` (no scale, no HCA).
    pre_diff: f64,
}

impl GamePred {
    /// Preseason-leg margin under a given slope and HCA (home perspective).
    fn pre_margin(&self, slope: f64, hca: f64) -> f64 {
        slope * self.pre_diff + if self.is_neutral { 0.0 } else { hca }
    }

    /// Blended margin's error under a full schedule.
    fn blend_err(&self, s: &Schedule) -> f64 {
        let w = schedule_weight(self.day_offset, s.w_max, s.end_day);
        w * self.pre_margin(s.slope, s.hca) + (1.0 - w) * self.pit_margin - self.actual
    }
}

/// cstat-season `S` opens ~Nov 1 of `S−1`.
fn season_open(season: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(season - 1, 11, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 11, 1).expect("static Nov 1 is valid"))
}

/// Schedule weight on the preseason leg. Mirrors
/// `projection::preseason_blend_weight`, with the constants as arguments.
fn schedule_weight(day_offset: i64, w_max: f64, end_day: i64) -> f64 {
    if end_day <= 0 {
        return 0.0;
    }
    let d = day_offset.max(0) as f64;
    (w_max * (1.0 - d / end_day as f64)).clamp(0.0, w_max)
}

fn mae(games: &[&GamePred], s: &Schedule) -> f64 {
    if games.is_empty() {
        return f64::NAN;
    }
    games.iter().map(|g| g.blend_err(s).abs()).sum::<f64>() / games.len() as f64
}

/// Paired comparison of two schedules on the same games: mean of
/// `|err_a| − |err_b|` and its z. Negative means `a` is better.
fn paired(games: &[&GamePred], a: &Schedule, b: &Schedule) -> (f64, f64) {
    let d: Vec<f64> = games
        .iter()
        .map(|g| g.blend_err(a).abs() - g.blend_err(b).abs())
        .collect();
    let n = d.len() as f64;
    if n < 2.0 {
        return (f64::NAN, f64::NAN);
    }
    let mean = d.iter().sum::<f64>() / n;
    let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, mean / (var / n).sqrt())
}

/// The grid. Slope runs past 1.0 on purpose: the early-season model leg is
/// compressed, so a blend that wanted to *stretch* the preseason leg is a
/// real possibility, and a grid capped at 1.0 would report 1.00 whatever the
/// optimum was — the same blind spot #390 was about, one step over.
fn slopes() -> Vec<f64> {
    (8..=26).map(|i| i as f64 * 0.05).collect() // 0.40 … 1.30
}
fn hcas() -> Vec<f64> {
    (2..=8).map(|i| i as f64 * 0.5).collect() // 1.0 … 4.0
}
fn w_maxes() -> Vec<f64> {
    (7..=20).map(|i| i as f64 * 0.05).collect() // 0.35 … 1.00
}
fn end_days() -> Vec<i64> {
    (14..=HORIZON_DAYS).step_by(7).collect() // 14 … 84
}

/// The grid point with the lowest MAE on `games`, optionally holding the leg
/// (slope, HCA) fixed so only the schedule is fit.
///
/// The blended margin is linear in slope and HCA once the weight is known, so
/// each game is reduced, per `(w_max, end_day)`, to three numbers —
/// `w·diff`, `w·home` and `(1 − w)·pit − actual` — and every (slope, HCA) pair
/// then costs two multiply-adds and an `abs` per game. Iteration order is
/// fixed and ties keep the first point, so the fit is deterministic.
fn best(games: &[&GamePred], leg: Option<(f64, f64)>) -> Schedule {
    let (slopes, hcas) = match leg {
        Some((slope, hca)) => (vec![slope], vec![hca]),
        None => (slopes(), hcas()),
    };
    let n = games.len();
    let (mut a, mut b, mut r) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut best = (shipped(), f64::MAX);
    for w_max in w_maxes() {
        for end_day in end_days() {
            for (i, g) in games.iter().enumerate() {
                let w = schedule_weight(g.day_offset, w_max, end_day);
                a[i] = w * g.pre_diff;
                b[i] = if g.is_neutral { 0.0 } else { w };
                r[i] = (1.0 - w) * g.pit_margin - g.actual;
            }
            for &slope in &slopes {
                for &hca in &hcas {
                    let sum: f64 = (0..n)
                        .map(|i| (slope * a[i] + hca * b[i] + r[i]).abs())
                        .sum();
                    if sum < best.1 {
                        best = (
                            Schedule {
                                slope,
                                hca,
                                w_max,
                                end_day,
                            },
                            sum,
                        );
                    }
                }
            }
        }
    }
    best.0
}

/// Replay one season's early window, returning the per-game legs.
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
             `cargo run --bin cstat-ingest -- compute-projections --years {year}` first."
        );
    }

    let open = season_open(year);
    let through = open + chrono::Duration::days(HORIZON_DAYS - 1);
    let games: Vec<GameRow> = sqlx::query_as(
        "SELECT g.id, g.game_date, g.home_team_id, g.away_team_id, \
                g.home_score, g.away_score, g.is_neutral_site AS is_neutral \
         FROM games g \
         WHERE g.season = $1 AND g.game_date <= $2 \
           AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL \
           AND g.home_team_id IS NOT NULL AND g.away_team_id IS NOT NULL",
    )
    .bind(year)
    .bind(through)
    .fetch_all(pool)
    .await
    .context("fetch completed games")?;
    if games.is_empty() {
        anyhow::bail!("no completed games for season {year} — pick a played season.");
    }

    let legs = crate::game_projections::model_legs(pool, predictor, year, through).await?;
    let preds: Vec<GamePred> = games
        .iter()
        .filter_map(|g| {
            let pit = *legs.margins.get(&g.id)?;
            let pre_diff = proj.get(&g.home_team_id)? - proj.get(&g.away_team_id)?;
            Some(GamePred {
                season: year,
                day_offset: (g.game_date - open).num_days().max(0),
                is_neutral: g.is_neutral,
                actual: (g.home_score - g.away_score) as f64,
                pit_margin: pit as f64,
                pre_diff,
            })
        })
        .collect();
    println!(
        "  season {year}: {} projection rows, {} early-window games, {} with both legs \
         ({} without a model leg)",
        proj.len(),
        games.len(),
        preds.len(),
        legs.skipped,
    );
    if legs.fetch_failures > 0 {
        // A failed fetch drops every game of the team it was for, so the
        // sample is short for a reason that has nothing to do with the data.
        anyhow::bail!(
            "season {year}: {} feature fetch(es) failed — the sample would be silently \
             short; re-run once the database is healthy",
            legs.fetch_failures
        );
    }
    Ok(preds)
}

/// The preseason leg on its own (weight 1, no model leg), against actual
/// margins: MAE, bias, and bias on wide projected gaps.
fn leg_report(games: &[&GamePred], slope: f64, hca: f64) -> (f64, f64, f64, usize) {
    let errs: Vec<(f64, f64)> = games
        .iter()
        .map(|g| (g.pre_diff, g.pre_margin(slope, hca) - g.actual))
        .collect();
    let n = errs.len() as f64;
    let mae = errs.iter().map(|(_, e)| e.abs()).sum::<f64>() / n;
    let bias = errs.iter().map(|(_, e)| e).sum::<f64>() / n;
    // Signed so a positive number always means "over-predicted the favourite".
    let wide: Vec<f64> = errs
        .iter()
        .filter(|(d, _)| d.abs() >= WIDE_GAP)
        .map(|(d, e)| e * d.signum())
        .collect();
    let wide_bias = wide.iter().sum::<f64>() / wide.len().max(1) as f64;
    (mae, bias, wide_bias, wide.len())
}

/// Per-week table under one schedule, pooled across seasons: where the model
/// leg overtakes the preseason one, and what each week's own best weight is.
fn print_week_table(games: &[&GamePred], s: &Schedule) {
    let mut weeks: BTreeMap<i64, Vec<&GamePred>> = BTreeMap::new();
    for g in games {
        weeks.entry(g.day_offset / 7).or_default().push(g);
    }
    println!(
        "\n  per-week, pooled (leg slope {:.2} HCA {:.1}):\n  {:<6} {:>5}  {:>7} {:>7} {:>8} {:>7} {:>6}",
        s.slope, s.hca, "week", "n", "pre-MAE", "pit-MAE", "schedule", "best", "best-w",
    );
    println!("  {}", "-".repeat(56));
    let mut crossover: Option<i64> = None;
    for (week, gs) in &weeks {
        let n = gs.len() as f64;
        let pre = gs
            .iter()
            .map(|g| (g.pre_margin(s.slope, s.hca) - g.actual).abs())
            .sum::<f64>()
            / n;
        let pit = gs
            .iter()
            .map(|g| (g.pit_margin - g.actual).abs())
            .sum::<f64>()
            / n;
        let sched = mae(gs, s);
        let (bw, bm) = (0..=20)
            .map(|i| {
                let w = i as f64 * 0.05;
                let m = gs
                    .iter()
                    .map(|g| {
                        (w * g.pre_margin(s.slope, s.hca) + (1.0 - w) * g.pit_margin - g.actual)
                            .abs()
                    })
                    .sum::<f64>()
                    / n;
                (w, m)
            })
            .fold((0.0, f64::MAX), |a, b| if b.1 < a.1 { b } else { a });
        if crossover.is_none() && gs.len() >= MIN_WEEK_N && pit <= pre {
            crossover = Some(*week);
        }
        println!(
            "  {:<6} {:>5}  {pre:>7.2} {pit:>7.2} {sched:>8.2} {bm:>7.2} {bw:>6.2}",
            week + 1,
            gs.len(),
        );
    }
    if let Some(w) = crossover {
        println!("    crossover (model leg ≤ preseason leg): week {}", w + 1);
    }
}

/// Write the per-game legs as CSV, so a question the report does not answer
/// can be asked offline without re-running the model over every season.
fn write_dump(path: &std::path::Path, games: &[GamePred]) -> Result<()> {
    use std::fmt::Write as _;
    let mut out = String::from("season,day_offset,is_neutral,actual,pit_margin,pre_diff\n");
    for g in games {
        let _ = writeln!(
            out,
            "{},{},{},{},{},{}",
            g.season, g.day_offset, g.is_neutral, g.actual, g.pit_margin, g.pre_diff
        );
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

/// Every season that has both a preseason projection and a completed game —
/// the default, because the walk-forward needs history: with three seasons it
/// scores one fold.
async fn calibratable_seasons(pool: &PgPool) -> Result<Vec<i32>> {
    sqlx::query_scalar(
        "SELECT DISTINCT p.season FROM team_preseason_projection p \
         WHERE EXISTS (SELECT 1 FROM games g WHERE g.season = p.season \
                       AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL) \
         ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .context("listing seasons with projections and results")
}

pub async fn run(
    pool: &PgPool,
    predictor: &Predictor,
    years: &[i32],
    dump: Option<&std::path::Path>,
) -> Result<()> {
    // The walk-forward folds read `years` in order and train on everything
    // earlier, so the list must be ascending and unique whatever was typed.
    let mut years = years.to_vec();
    if years.is_empty() {
        years = calibratable_seasons(pool).await?;
    }
    years.sort_unstable();
    years.dedup();
    let years = years.as_slice();
    if years.len() < 3 {
        println!(
            "  note: {} season(s) leaves {} walk-forward fold(s); pass more seasons for a usable z",
            years.len(),
            years.len().saturating_sub(2)
        );
    }
    println!("{}", "=".repeat(78));
    println!("preseason × pit blend calibration — seasons {years:?}");
    println!("{}", "=".repeat(78));

    let mut pooled: Vec<GamePred> = Vec::new();
    for &y in years {
        pooled.extend(analyze_season(pool, predictor, y).await?);
    }
    if let Some(path) = dump {
        write_dump(path, &pooled)?;
        println!("  wrote {} games to {}", pooled.len(), path.display());
    }
    let all: Vec<&GamePred> = pooled.iter().collect();
    if all.is_empty() {
        anyhow::bail!("no games had both legs — nothing to calibrate");
    }
    let cur = shipped();

    // ---- 1. The preseason leg on its own ----
    println!(
        "\n  1. preseason leg alone (weight 1), {} games:\n  {:<26} {:>7} {:>7} {:>13}",
        all.len(),
        "leg",
        "MAE",
        "bias",
        format!("bias |gap|≥{WIDE_GAP:.0}"),
    );
    let mut leg_best = (cur.slope, cur.hca, f64::MAX);
    for slope in slopes() {
        for hi in 0..=10 {
            let hca = hi as f64 * 0.5;
            let (m, ..) = leg_report(&all, slope, hca);
            if m < leg_best.2 {
                leg_best = (slope, hca, m);
            }
        }
    }
    for (name, slope, hca) in [
        ("shipped", cur.slope, cur.hca),
        ("leg-alone calibration", LEG_ALONE.0, LEG_ALONE.1),
        ("best leg", leg_best.0, leg_best.1),
    ] {
        let (m, b, wb, wn) = leg_report(&all, slope, hca);
        println!(
            "  {:<26} {m:>7.3} {b:>+7.3} {wb:>+8.3} (n={wn})",
            format!("{name} {slope:.2}/{hca:.1}"),
        );
    }

    // ---- 2. Joint fit, pooled ----
    let joint = best(&all, None);
    let fixed_leg = best(&all, Some((cur.slope, cur.hca)));
    let alone_leg = best(&all, Some(LEG_ALONE));
    let pit_only = all
        .iter()
        .map(|g| (g.pit_margin - g.actual).abs())
        .sum::<f64>()
        / all.len() as f64;
    println!(
        "\n  2. blended MAE, pooled ({} games, first {HORIZON_DAYS} days):",
        all.len()
    );
    println!("  {:<44} {:>7}", "model leg only", format!("{pit_only:.3}"));
    println!(
        "  {:<44} {:>7.3}   (reference)                [{}]",
        "shipped",
        mae(&all, &cur),
        cur.label()
    );
    for (name, s) in [
        (
            "leg-alone calibration, shipped schedule",
            leg_alone_at_shipped(),
        ),
        ("best schedule, leg held at shipped", fixed_leg),
        ("best schedule, leg-alone calibration", alone_leg),
        ("best joint", joint),
    ] {
        let (d, z) = paired(&all, &s, &cur);
        println!(
            "  {:<44} {:>7.3}   vs shipped {d:>+7.4} (z {z:>+6.1})   [{}]",
            name,
            mae(&all, &s),
            s.label()
        );
    }

    // ---- 3. Walk-forward: fit on seasons < S, score S ----
    println!("\n  3. walk-forward (fit on earlier seasons, score the next):");
    println!(
        "  {:<6} {:>6}  {:>8} {:>8} {:>8} {:>8}   fitted (leg held / leg alone / joint)",
        "season", "n", "shipped", "held", "alone", "joint",
    );
    // Per scored game: the three schedules fitted on the seasons before it.
    let mut wf: Vec<(&GamePred, [Schedule; 3])> = Vec::new();
    for &s in years.iter().skip(2) {
        let train: Vec<&GamePred> = all.iter().copied().filter(|g| g.season < s).collect();
        let test: Vec<&GamePred> = all.iter().copied().filter(|g| g.season == s).collect();
        if train.is_empty() || test.is_empty() {
            continue;
        }
        let fits = [
            best(&train, Some((cur.slope, cur.hca))),
            best(&train, Some(LEG_ALONE)),
            best(&train, None),
        ];
        println!(
            "  {s:<6} {:>6}  {:>8.3} {:>8.3} {:>8.3} {:>8.3}   [{}] / [{}] / [{}]",
            test.len(),
            mae(&test, &cur),
            mae(&test, &fits[0]),
            mae(&test, &fits[1]),
            mae(&test, &fits[2]),
            fits[0].label(),
            fits[1].label(),
            fits[2].label(),
        );
        wf.extend(test.into_iter().map(|g| (g, fits)));
    }
    if wf.len() > 1 {
        let n = wf.len() as f64;
        let shp = wf.iter().map(|(g, _)| g.blend_err(&cur).abs()).sum::<f64>() / n;
        let refit = |i: usize| {
            wf.iter()
                .map(|(g, f)| g.blend_err(&f[i]).abs())
                .sum::<f64>()
                / n
        };
        println!(
            "  {:<6} {:>6}  {shp:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            "pooled",
            wf.len(),
            refit(0),
            refit(1),
            refit(2),
        );
        for (i, name) in ["leg held", "leg alone", "joint"].iter().enumerate() {
            let d: Vec<f64> = wf
                .iter()
                .map(|(g, f)| g.blend_err(&f[i]).abs() - g.blend_err(&cur).abs())
                .collect();
            let mean = d.iter().sum::<f64>() / n;
            let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
            println!(
                "    {name:<9} refit vs shipped: {mean:+.4} (z {:+.1})",
                mean / (var / n).sqrt()
            );
        }
    }

    // ---- 4. Per-season stability and the weekly shape ----
    println!("\n  4. per season, blended MAE:");
    for &y in years {
        let s: Vec<&GamePred> = all.iter().copied().filter(|g| g.season == y).collect();
        if s.is_empty() {
            continue;
        }
        println!(
            "    {y}: shipped {:.3}   leg-alone calibration {:.3}   model leg only {:.3}",
            mae(&s, &cur),
            mae(&s, &leg_alone_at_shipped()),
            s.iter()
                .map(|g| (g.pit_margin - g.actual).abs())
                .sum::<f64>()
                / s.len() as f64,
        );
    }
    print_week_table(&all, &cur);

    println!("\n  {}", "-".repeat(74));
    println!(
        "  Shipped: PRESEASON_SLOPE {:.2}, PRESEASON_HCA {:.1}, PRESEASON_PEAK_WEIGHT {:.2}, \
         PRESEASON_DECAY_DAYS {} (projection.rs).",
        cur.slope, cur.hca, cur.w_max, cur.end_day
    );
    println!(
        "  Scope: completed-game (as-of) path only; unplayed games blend over the live model \
         leg, which this replay cannot reconstruct."
    );
    println!(
        "  Caveats: the model leg's non-CamPom features are season aggregates (#274), which \
         flatters it early and biases the fitted preseason weight down; historical preseason \
         projections are in-sample at Layer 2, which biases it up."
    );
    Ok(())
}
