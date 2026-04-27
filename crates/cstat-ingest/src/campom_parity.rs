//! Parity gate for the CamPom compute step.
//!
//! Loads the externally-computed reference dataset (`docs/campom_2026_baseline.csv`),
//! joins to `torvik_player_stats` on `(torvik_pid, season)`, and diffs every
//! intermediate and composite. Pass condition: `max abs diff < TOLERANCE` for
//! every column, every row.
//!
//! See ROADMAP §4f and `docs/campom_methodology.md`.

use anyhow::{Context, Result, anyhow};
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::Path;

/// Per-column max-allowed absolute deviation. Loose enough to swallow IEEE-754
/// rounding through several multiplications; tight enough that any real formula
/// drift will trip it.
const TOLERANCE: f64 = 0.01;

#[derive(Debug, Clone, Copy)]
struct Baseline {
    min_factor: Option<f64>,
    mp_factor: Option<f64>,
    gp_weight: Option<f64>,
    adj_gbpm: Option<f64>,
    conf_sos: Option<f64>,
    sos_adj: Option<f64>,
    adj_gbpm_sos: Option<f64>,
    cam_gbpm: Option<f64>,
    cam_gbpm_v2: Option<f64>,
    cam_gbpm_v3: Option<f64>,
    min_adj_gbpm: Option<f64>,
    min_adj_gbpm_v2: Option<f64>,
    min_adj_gbpm_v3: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Computed {
    min_factor: Option<f64>,
    mp_factor: Option<f64>,
    gp_weight: Option<f64>,
    adj_gbpm: Option<f64>,
    conf_sos: Option<f64>,
    sos_adj: Option<f64>,
    adj_gbpm_sos: Option<f64>,
    cam_gbpm: Option<f64>,
    cam_gbpm_v2: Option<f64>,
    cam_gbpm_v3: Option<f64>,
    min_adj_gbpm: Option<f64>,
    min_adj_gbpm_v2: Option<f64>,
    min_adj_gbpm_v3: Option<f64>,
}

#[derive(Debug, Default)]
pub struct ColumnStats {
    name: &'static str,
    n_compared: usize,
    n_skipped_null: usize,
    max_abs_diff: f64,
    max_diff_pid: i32,
    max_diff_baseline: f64,
    max_diff_computed: f64,
}

fn parse_opt_f64(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t == "NA" || t == "NaN" {
        None
    } else {
        t.parse::<f64>().ok()
    }
}

fn load_baseline(path: &Path) -> Result<HashMap<i32, Baseline>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .with_context(|| format!("opening baseline CSV at {}", path.display()))?;

    let headers = rdr.headers()?.clone();
    let idx = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| anyhow!("baseline CSV missing column `{name}`"))
    };

    let i_pid = idx("pid")?;
    let i_min_factor = idx("min_factor")?;
    let i_mp_factor = idx("mp_factor")?;
    let i_gp_weight = idx("gp_weight")?;
    let i_adj_gbpm = idx("adj_gbpm")?;
    let i_conf_sos = idx("conf_sos")?;
    let i_sos_adj = idx("sos_adj")?;
    let i_adj_gbpm_sos = idx("adj_gbpm_sos")?;
    let i_cam_gbpm = idx("cam_gbpm")?;
    let i_cam_gbpm_v2 = idx("cam_gbpm_v2")?;
    let i_cam_gbpm_v3 = idx("cam_gbpm_v3")?;
    let i_min_adj_gbpm = idx("min_adj_gbpm")?;
    let i_min_adj_gbpm_v2 = idx("min_adj_gbpm_v2")?;
    let i_min_adj_gbpm_v3 = idx("min_adj_gbpm_v3")?;

    let mut out = HashMap::new();
    for record in rdr.records() {
        let r = record?;
        let Some(pid) = r.get(i_pid).and_then(|s| s.trim().parse::<i32>().ok()) else {
            continue;
        };
        out.insert(
            pid,
            Baseline {
                min_factor: r.get(i_min_factor).and_then(parse_opt_f64),
                mp_factor: r.get(i_mp_factor).and_then(parse_opt_f64),
                gp_weight: r.get(i_gp_weight).and_then(parse_opt_f64),
                adj_gbpm: r.get(i_adj_gbpm).and_then(parse_opt_f64),
                conf_sos: r.get(i_conf_sos).and_then(parse_opt_f64),
                sos_adj: r.get(i_sos_adj).and_then(parse_opt_f64),
                adj_gbpm_sos: r.get(i_adj_gbpm_sos).and_then(parse_opt_f64),
                cam_gbpm: r.get(i_cam_gbpm).and_then(parse_opt_f64),
                cam_gbpm_v2: r.get(i_cam_gbpm_v2).and_then(parse_opt_f64),
                cam_gbpm_v3: r.get(i_cam_gbpm_v3).and_then(parse_opt_f64),
                min_adj_gbpm: r.get(i_min_adj_gbpm).and_then(parse_opt_f64),
                min_adj_gbpm_v2: r.get(i_min_adj_gbpm_v2).and_then(parse_opt_f64),
                min_adj_gbpm_v3: r.get(i_min_adj_gbpm_v3).and_then(parse_opt_f64),
            },
        );
    }
    Ok(out)
}

type ComputedRow = (
    i32,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);

async fn load_computed(pool: &PgPool, season: i32) -> Result<HashMap<i32, Computed>> {
    let rows: Vec<ComputedRow> = sqlx::query_as(
        "SELECT torvik_pid,
                min_factor, mp_factor, gp_weight,
                adj_gbpm, conf_sos, sos_adj, adj_gbpm_sos,
                cam_gbpm, cam_gbpm_v2, cam_gbpm_v3,
                min_adj_gbpm, min_adj_gbpm_v2, min_adj_gbpm_v3
           FROM torvik_player_stats
          WHERE season = $1",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.0,
                Computed {
                    min_factor: r.1,
                    mp_factor: r.2,
                    gp_weight: r.3,
                    adj_gbpm: r.4,
                    conf_sos: r.5,
                    sos_adj: r.6,
                    adj_gbpm_sos: r.7,
                    cam_gbpm: r.8,
                    cam_gbpm_v2: r.9,
                    cam_gbpm_v3: r.10,
                    min_adj_gbpm: r.11,
                    min_adj_gbpm_v2: r.12,
                    min_adj_gbpm_v3: r.13,
                },
            )
        })
        .collect())
}

fn compare_column(stats: &mut ColumnStats, pid: i32, baseline: Option<f64>, computed: Option<f64>) {
    match (baseline, computed) {
        (Some(b), Some(c)) => {
            let d = (b - c).abs();
            stats.n_compared += 1;
            if d > stats.max_abs_diff {
                stats.max_abs_diff = d;
                stats.max_diff_pid = pid;
                stats.max_diff_baseline = b;
                stats.max_diff_computed = c;
            }
        }
        _ => stats.n_skipped_null += 1,
    }
}

pub struct ParityReport {
    pub matched_pids: usize,
    pub baseline_only: usize,
    pub computed_only: usize,
    pub columns: Vec<ColumnStats>,
}

impl ParityReport {
    pub fn passed(&self) -> bool {
        self.columns.iter().all(|c| c.max_abs_diff < TOLERANCE)
    }

    pub fn print(&self) {
        println!(
            "Matched {} pids ({} baseline-only, {} computed-only)",
            self.matched_pids, self.baseline_only, self.computed_only
        );
        println!(
            "{:<18} {:>8} {:>8} {:>14}    worst row",
            "column", "n", "skipped", "max_abs_diff"
        );
        for c in &self.columns {
            let marker = if c.max_abs_diff < TOLERANCE {
                ""
            } else {
                "  ✗"
            };
            println!(
                "{:<18} {:>8} {:>8} {:>14.6}    pid={} baseline={:.4} computed={:.4}{}",
                c.name,
                c.n_compared,
                c.n_skipped_null,
                c.max_abs_diff,
                c.max_diff_pid,
                c.max_diff_baseline,
                c.max_diff_computed,
                marker
            );
        }
        println!(
            "\n{}",
            if self.passed() {
                "PASS — every column within tolerance"
            } else {
                "FAIL — at least one column exceeded tolerance"
            }
        );
    }
}

pub async fn run(pool: &PgPool, season: i32, baseline_path: &Path) -> Result<ParityReport> {
    let baseline = load_baseline(baseline_path)?;
    let computed = load_computed(pool, season).await?;

    println!(
        "Baseline rows: {}; computed rows: {}",
        baseline.len(),
        computed.len()
    );

    let mut columns = vec![
        ColumnStats {
            name: "min_factor",
            ..Default::default()
        },
        ColumnStats {
            name: "mp_factor",
            ..Default::default()
        },
        ColumnStats {
            name: "gp_weight",
            ..Default::default()
        },
        ColumnStats {
            name: "adj_gbpm",
            ..Default::default()
        },
        ColumnStats {
            name: "conf_sos",
            ..Default::default()
        },
        ColumnStats {
            name: "sos_adj",
            ..Default::default()
        },
        ColumnStats {
            name: "adj_gbpm_sos",
            ..Default::default()
        },
        ColumnStats {
            name: "cam_gbpm",
            ..Default::default()
        },
        ColumnStats {
            name: "cam_gbpm_v2",
            ..Default::default()
        },
        ColumnStats {
            name: "cam_gbpm_v3",
            ..Default::default()
        },
        ColumnStats {
            name: "min_adj_gbpm",
            ..Default::default()
        },
        ColumnStats {
            name: "min_adj_gbpm_v2",
            ..Default::default()
        },
        ColumnStats {
            name: "min_adj_gbpm_v3",
            ..Default::default()
        },
    ];

    let mut matched = 0usize;
    let mut baseline_only = 0usize;
    for (pid, b) in &baseline {
        let Some(c) = computed.get(pid) else {
            baseline_only += 1;
            continue;
        };
        matched += 1;
        compare_column(&mut columns[0], *pid, b.min_factor, c.min_factor);
        compare_column(&mut columns[1], *pid, b.mp_factor, c.mp_factor);
        compare_column(&mut columns[2], *pid, b.gp_weight, c.gp_weight);
        compare_column(&mut columns[3], *pid, b.adj_gbpm, c.adj_gbpm);
        compare_column(&mut columns[4], *pid, b.conf_sos, c.conf_sos);
        compare_column(&mut columns[5], *pid, b.sos_adj, c.sos_adj);
        compare_column(&mut columns[6], *pid, b.adj_gbpm_sos, c.adj_gbpm_sos);
        compare_column(&mut columns[7], *pid, b.cam_gbpm, c.cam_gbpm);
        compare_column(&mut columns[8], *pid, b.cam_gbpm_v2, c.cam_gbpm_v2);
        compare_column(&mut columns[9], *pid, b.cam_gbpm_v3, c.cam_gbpm_v3);
        compare_column(&mut columns[10], *pid, b.min_adj_gbpm, c.min_adj_gbpm);
        compare_column(&mut columns[11], *pid, b.min_adj_gbpm_v2, c.min_adj_gbpm_v2);
        compare_column(&mut columns[12], *pid, b.min_adj_gbpm_v3, c.min_adj_gbpm_v3);
    }
    let computed_only = computed.len().saturating_sub(matched);

    Ok(ParityReport {
        matched_pids: matched,
        baseline_only,
        computed_only,
        columns,
    })
}
