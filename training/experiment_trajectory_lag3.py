"""
Does a THIRD prior season (N-2) help beyond the shipped lag-2 + slope block?

The productionized trajectory model looks back one extra season: it uses N and
N-1 (lag-2 levels + the most recent 1-year slope). This asks whether adding the
prior-PRIOR-prior season (N-2) — a lag-3 level block and/or an *acceleration*
term (how the year-over-year slope itself is changing) — moves the held-out
projection further.

Baseline here is the SHIPPED 60-feature contract (train_trajectory_model already
bakes in the lag-2 block), so this is a clean marginal test on top of it.

Variants:
  production   — shipped 60-feature model (incl. lag-2 + slope)
  plus_lag3    — + prior3_campom/mpg/gp/usg/ppg levels + has_prior3
  plus_accel   — + has_prior3 + accel_campom = (prior−prior2) − (prior2−prior3)
  plus_both3   — union of the two

Decision metric mirrors the lag-2 experiment: pooled LOPO MAE (full) + the
lag-3-COVERED-subset MAE (the only rows a lag-3 feature can move — players with
three consecutive qualifying seasons) + per-pair wins.

Comparison script only; does not touch production models or the meta.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error

import train_trajectory_model as base

EVAL_DIR = Path(__file__).parent / "eval_history"

LEVEL_SENTINEL = -999.0
DELTA_FILL = 0.0

# Extend the SHIPPED query with the N-2 season (same torvik_pid self-join, one
# more season back). Anchored on the lag-2 landmarks the production query now
# contains, so an upstream change fails loudly here.
EXT_QUERY = (
    base.PAIRED_QUERY
    .replace(
        "        c.player_id AS pid_nm1,\n"
        "        c.cam_gbpm_v3_psos AS prior2_campom\n"
        "    FROM torvik_player_stats a",
        "        c.player_id AS pid_nm1,\n"
        "        c.cam_gbpm_v3_psos AS prior2_campom,\n"
        "        d.player_id AS pid_nm2,\n"
        "        d.cam_gbpm_v3_psos AS prior3_campom\n"
        "    FROM torvik_player_stats a",
    )
    .replace(
        "    LEFT JOIN torvik_player_stats c\n"
        "        ON c.torvik_pid = a.torvik_pid AND c.season = a.season - 1\n"
        "    WHERE a.torvik_pid IS NOT NULL",
        "    LEFT JOIN torvik_player_stats c\n"
        "        ON c.torvik_pid = a.torvik_pid AND c.season = a.season - 1\n"
        "    LEFT JOIN torvik_player_stats d\n"
        "        ON d.torvik_pid = a.torvik_pid AND d.season = a.season - 2\n"
        "    WHERE a.torvik_pid IS NOT NULL",
    )
    .replace(
        "    pssNM1.ppg AS prior2_ppg\n"
        "FROM base",
        "    pssNM1.ppg AS prior2_ppg,\n"
        "    base.prior3_campom AS prior3_campom,\n"
        "    pssNM2.minutes_per_game AS prior3_mpg,\n"
        "    pssNM2.games_played AS prior3_gp,\n"
        "    pssNM2.usage_rate AS prior3_usg,\n"
        "    pssNM2.ppg AS prior3_ppg\n"
        "FROM base",
    )
    .replace(
        "LEFT JOIN player_season_stats pssNM1\n"
        "    ON pssNM1.player_id = base.pid_nm1 AND pssNM1.season = base.s_n - 1",
        "LEFT JOIN player_season_stats pssNM1\n"
        "    ON pssNM1.player_id = base.pid_nm1 AND pssNM1.season = base.s_n - 1\n"
        "LEFT JOIN player_season_stats pssNM2\n"
        "    ON pssNM2.player_id = base.pid_nm2 AND pssNM2.season = base.s_n - 2",
    )
)
assert "prior3_campom" in EXT_QUERY and "pssNM2" in EXT_QUERY, "lag-3 query injection failed"

LAG3_LEVELS = ["prior3_campom", "prior3_mpg", "prior3_gp", "prior3_usg", "prior3_ppg"]


def run_variant(name: str, df: pd.DataFrame, feature_cols: list[str], covered) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  ({len(feature_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = list(feature_cols)
    lopo, lopo_preds = base.leave_one_pair_out(df)
    mask = covered & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(df.loc[mask, "target_campom"], lopo_preds[mask]))
    print(f"  pooled (lag3-covered rows only, n={int(mask.sum())}): MAE {sub_mae:.4f}")
    return {
        "pooled_mae": lopo["pooled"]["mae"],
        "covered_mae": sub_mae,
        "per_pair": {k: v["mae"] for k, v in lopo["per_pair"].items()},
    }


def main() -> None:
    base.PAIRED_QUERY = EXT_QUERY
    # build_dataset() derives + sentinel-fills the lag-2 block already; the
    # returned FEATURE_COLS is the shipped 60-feature contract.
    df = base.build_dataset().reset_index(drop=True)
    production_cols = list(base.FEATURE_COLS)

    # Lag-3 derivations. has_prior3 off the raw (still-NaN) level; accel needs
    # all three CamPom levels present.
    df["has_prior3"] = df["prior3_campom"].notna().astype(float)
    # accel = recent slope − prior slope = (prior−prior2) − (prior2−prior3).
    # prior2_campom is ALREADY sentinel-filled (-999) by build_dataset, so
    # recompute the slopes from raw where possible: only defined when all three
    # exist (has_prior3==1 implies has_prior2==1 by the consecutive-season join).
    df["accel_campom"] = (
        (df["prior_campom"] - df["prior2_campom"])
        - (df["prior2_campom"] - df["prior3_campom"])
    )
    for c in LAG3_LEVELS:
        df[c] = df[c].fillna(LEVEL_SENTINEL).astype(float)
    df["accel_campom"] = df["accel_campom"].fillna(DELTA_FILL).astype(float)
    df["has_prior3"] = df["has_prior3"].fillna(0.0).astype(float)

    covered = df["has_prior3"] == 1.0
    cov = float(covered.mean())
    print(f"\nRows: {len(df):,}; lag-3 (3 consecutive seasons) coverage: {cov:.1%} "
          f"({int(covered.sum())} rows)")
    nb = float(mean_absolute_error(
        df.loc[covered, "target_campom"], df.loc[covered, "prior_campom"]))
    print(f"Covered-subset naive baseline (N+1≈N) MAE: {nb:.4f}")

    variants = {
        "production": production_cols,
        "plus_lag3": production_cols + LAG3_LEVELS + ["has_prior3"],
        "plus_accel": production_cols + ["has_prior3", "accel_campom"],
        "plus_both3": production_cols + LAG3_LEVELS + ["has_prior3", "accel_campom"],
    }
    results = {name: run_variant(name, df, cols, covered) for name, cols in variants.items()}

    print(f"\n{'=' * 64}\nSUMMARY (LOPO MAE)\n{'=' * 64}")
    print(f"{'variant':<14} {'full':>9} {'lag3-covered':>14}")
    for name, r in results.items():
        print(f"{name:<14} {r['pooled_mae']:>9.4f} {r['covered_mae']:>14.4f}")

    p = results["production"]
    for name in ("plus_lag3", "plus_accel", "plus_both3"):
        r = results[name]
        wins = sum(1 for k in r["per_pair"] if r["per_pair"][k] < p["per_pair"].get(k, np.inf))
        print(f"\n{name} vs production: full Δ {p['pooled_mae'] - r['pooled_mae']:+.4f}  "
              f"covered Δ {p['covered_mae'] - r['covered_mae']:+.4f}  "
              f"(positive = improvement)  per-pair wins {wins}/{len(r['per_pair'])}")

    EVAL_DIR.mkdir(exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"trajectory_lag3_experiment_{stamp}_summary.json"
    out_path.write_text(json.dumps(
        {"lag3_coverage": cov, "n_rows": int(len(df)),
         "covered_naive_mae": nb, "results": results}, indent=2))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    main()
