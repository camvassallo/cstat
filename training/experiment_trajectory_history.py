"""
Multi-season history → trajectory model: accept/reject experiment.

User question: the shipped trajectory model is single-prior-season — it maps
season N's features → season N+1 CamPom and never sees season N-1. So it's
blind to a player's *progression slope*: Caden Pierce went 10.34 (2024) → 0.34
(2025), and the model projecting off 2025 has no idea 2024 was elite. Does
adding the prior-prior season (lag-2 levels) and/or the year-over-year change
(progression slope) improve the held-out projection?

Two feature families, layered on the production 51-feature contract:
  lag2  — prior2_campom / prior2_mpg / prior2_gp / prior2_usg / prior2_ppg
          (+ has_prior2 indicator)  → the level two years back
  slope — delta_campom / delta_mpg / delta_usg (prior_N − prior_{N-1})
          (+ has_prior2)            → the recent trajectory

Cross-season linkage uses `torvik_pid` (same stable key the N→N+1 pairing
uses), so a transfer's prior-prior season at a different school still links.
The N-1 join is a LEFT JOIN — it adds columns, drops zero rows — so every
variant runs on the identical row set as production (apples-to-apples). Rows
with no N-1 season (freshman-as-N, or a career starting before the 2015 data
floor) get NaN lag features; LightGBM routes NaN natively, and `has_prior2`
lets the tree split "no history" explicitly.

Decision metric mirrors the on/off + RAPM experiments: pooled LOPO MAE (full),
**plus the covered-subset MAE** (rows where `has_prior2`, the only rows a lag
feature can move — the full number dilutes the effect across the freshman
majority), plus per-pair wins. A per-class-year covered breakdown shows
*where* any lift lands.

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

# --- Extend the production query with the prior-prior (N-1) season. ---------
# Anchored on stable landmarks so an upstream contract change fails loudly here
# rather than silently dropping the block.
EXT_QUERY = (
    base.PAIRED_QUERY
    # 1. base CTE: pull the N-1 torvik row (level + the cross-season player id)
    .replace(
        "        b.cam_gbpm_v3_psos AS target_campom\n    FROM torvik_player_stats a",
        "        b.cam_gbpm_v3_psos AS target_campom,\n"
        "        c.player_id AS pid_nm1,\n"
        "        c.cam_gbpm_v3_psos AS prior2_campom\n"
        "    FROM torvik_player_stats a",
    )
    .replace(
        "        AND b.season = a.season + 1\n    WHERE a.torvik_pid IS NOT NULL",
        "        AND b.season = a.season + 1\n"
        "    LEFT JOIN torvik_player_stats c\n"
        "        ON c.torvik_pid = a.torvik_pid AND c.season = a.season - 1\n"
        "    WHERE a.torvik_pid IS NOT NULL",
    )
    # 2. outer SELECT: expose N-1 box/role from player_season_stats
    .replace(
        "    rec.year             AS recruit_year_raw\nFROM base",
        "    rec.year             AS recruit_year_raw,\n"
        "    base.prior2_campom AS prior2_campom,\n"
        "    pssNM1.minutes_per_game AS prior2_mpg,\n"
        "    pssNM1.games_played AS prior2_gp,\n"
        "    pssNM1.usage_rate AS prior2_usg,\n"
        "    pssNM1.ppg AS prior2_ppg\n"
        "FROM base",
    )
    # 3. outer FROM: LEFT JOIN the N-1 season stats on the cross-season id
    .replace(
        "LEFT JOIN recruits rec\n    ON rec.cstat_player_id = base.pid_n",
        "LEFT JOIN player_season_stats pssNM1\n"
        "    ON pssNM1.player_id = base.pid_nm1 AND pssNM1.season = base.s_n - 1\n"
        "LEFT JOIN recruits rec\n    ON rec.cstat_player_id = base.pid_n",
    )
)
assert "prior2_campom" in EXT_QUERY and "pssNM1" in EXT_QUERY, "query injection failed"

LAG2_COLS = ["prior2_campom", "prior2_mpg", "prior2_gp", "prior2_usg", "prior2_ppg", "has_prior2"]
SLOPE_COLS = ["delta_campom", "delta_mpg", "delta_usg", "has_prior2"]


def run_variant(name: str, df: pd.DataFrame, feature_cols: list[str], covered) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  ({len(feature_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = list(feature_cols)
    lopo, lopo_preds = base.leave_one_pair_out(df)
    mask = covered & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(df.loc[mask, "target_campom"], lopo_preds[mask]))
    print(f"  pooled (prior2-covered rows only, n={int(mask.sum())}): MAE {sub_mae:.4f}")
    return {
        "pooled_mae": lopo["pooled"]["mae"],
        "pooled_rmse": lopo["pooled"]["rmse"],
        "covered_mae": sub_mae,
        "per_pair": {k: v["mae"] for k, v in lopo["per_pair"].items()},
        "_preds": lopo_preds,
    }


def main() -> None:
    production_cols = list(base.FEATURE_COLS)
    base.PAIRED_QUERY = EXT_QUERY
    df = base.build_dataset().reset_index(drop=True)

    # Derived history features. Levels stay NaN where no N-1 exists (LightGBM
    # routes NaN); has_prior2 makes the absence explicit; deltas are the slope.
    df["has_prior2"] = df["prior2_campom"].notna().astype(float)
    df["delta_campom"] = df["prior_campom"] - df["prior2_campom"]
    df["delta_mpg"] = df["prior_mpg"] - df["prior2_mpg"]
    df["delta_usg"] = df["prior_usg"] - df["prior2_usg"]
    covered = df["has_prior2"] == 1.0
    cov = float(covered.mean())
    print(f"\nRows: {len(df):,}; prior-2 season coverage: {cov:.1%} ({int(covered.sum())} rows)")

    # Covered-subset naive baseline (does ANY model beat 'N+1 ≈ N' there?).
    nb = float(mean_absolute_error(
        df.loc[covered, "target_campom"], df.loc[covered, "prior_campom"]))
    print(f"Covered-subset naive baseline (N+1≈N) MAE: {nb:.4f}")

    # Dedup the combined extra block (has_prior2 lives in both families).
    both_extra: list[str] = []
    for c in LAG2_COLS + SLOPE_COLS:
        if c not in production_cols and c not in both_extra:
            both_extra.append(c)
    variants = {
        "production": production_cols,
        "plus_lag2": production_cols + LAG2_COLS,
        "plus_slope": production_cols + SLOPE_COLS,
        "plus_both": production_cols + both_extra,
    }
    results = {name: run_variant(name, df, cols, covered) for name, cols in variants.items()}

    print(f"\n{'=' * 64}\nSUMMARY (LOPO MAE)\n{'=' * 64}")
    print(f"{'variant':<14} {'full':>9} {'prior2-covered':>16}")
    for name, r in results.items():
        print(f"{name:<14} {r['pooled_mae']:>9.4f} {r['covered_mae']:>16.4f}")

    p = results["production"]
    for name in ("plus_lag2", "plus_slope", "plus_both"):
        r = results[name]
        wins = sum(1 for k in r["per_pair"]
                   if r["per_pair"][k] < p["per_pair"].get(k, np.inf))
        print(f"\n{name} vs production: full Δ {p['pooled_mae'] - r['pooled_mae']:+.4f}  "
              f"covered Δ {p['covered_mae'] - r['covered_mae']:+.4f}  "
              f"(positive = improvement)  per-pair wins {wins}/{len(r['per_pair'])}")

    # Per-class-year covered breakdown: production vs plus_both — shows whether
    # any lift concentrates on upperclassmen (more career history to lean on).
    print(f"\n{'=' * 64}\nCovered-subset MAE by prior class year (production -> plus_both)\n{'=' * 64}")
    code_name = {-1: "?", 0: "Fr", 1: "So", 2: "Jr", 3: "Sr", 4: "Gr"}
    pb = results["plus_both"]
    for code in sorted(df.loc[covered, "prior_class_year_code"].unique()):
        m = covered & (df["prior_class_year_code"] == code) & p["_preds"].notna()
        if m.sum() < 20:
            continue
        mae_p = mean_absolute_error(df.loc[m, "target_campom"], p["_preds"][m])
        mae_b = mean_absolute_error(df.loc[m, "target_campom"], pb["_preds"][m])
        print(f"  {code_name.get(int(code), code):<3} n={int(m.sum()):<5} "
              f"{mae_p:.4f} -> {mae_b:.4f}  ({mae_p - mae_b:+.4f})")

    for r in results.values():
        r.pop("_preds", None)
    EVAL_DIR.mkdir(exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"trajectory_history_experiment_{stamp}_summary.json"
    out_path.write_text(json.dumps(
        {"prior2_coverage": cov, "n_rows": int(len(df)),
         "covered_naive_mae": nb, "results": results}, indent=2))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    main()
