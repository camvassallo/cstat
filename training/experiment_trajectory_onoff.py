"""
Tier-2 on/off features → trajectory model: accept/reject experiment.

ACCEPTED 2026-06-11 (the first positive PBP-feature verdict — see
eval_history/tier2_membership_models_20260611_summary.json) and shipped
into the production contract: `train_trajectory_model.py`'s PAIRED_QUERY /
NUMERIC_FEATURE_COLS now carry the three on/off columns natively (48→51).
This script is kept as the re-test harness: it ablates the on/off block
from the production feature set and re-runs the LOPO comparison, so the
"baseline" here is the pre-accept 48-feature model.

The three features (prior-season `player_on_off` rollup, PBP item A):

  prior_on_net_rtg    — team net rating per 100 with the player ON
  prior_net_on_off    — the on/off swing (impact evidence the box can't see)
  prior_on_poss_share — share of team possessions he was on floor for
                        (a possession-true rotation-share signal)

Coverage: lineups/PBP exist for every season EXCEPT 2019 (corrupt-gated),
and the rollup keeps only real rotation players (>=100 on-court possessions
both ends), so bench-fringe trajectory rows legitimately miss. The on/off
swing itself can be NULL for iron-men (sub-10-possession OFF sample floor).
Missing → -999.0 sentinel, NOT NaN — the Rust serve path fills missing with
sentinels (no NaN plumbing), and -999 is cleanly outside every real range
(net ratings ±~60, share in [0,1]).

Decision metric: pooled LOPO MAE on the full corpus, plus pooled LOPO MAE
restricted to rows whose on/off features are present (the honest signal of
what the features buy where they exist).

Comparison script only; does not touch production models or the meta.
"""

from __future__ import annotations

import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error

import train_trajectory_model as base

ONOFF_COLS = list(base.ONOFF_FEATURE_COLS)
MISSING_SENTINEL = base.ONOFF_MISSING_SENTINEL

# The production feature set with the on/off block ablated — the
# pre-accept 48-feature contract.
BASELINE_COLS = [c for c in base.FEATURE_COLS if c not in ONOFF_COLS]


def run_variant(name: str, df: pd.DataFrame, feature_cols: list[str]) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  ({len(feature_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = list(feature_cols)
    lopo, lopo_preds = base.leave_one_pair_out(df)

    mask = (df["prior_on_net_rtg"] != MISSING_SENTINEL) & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(
        df.loc[mask, "target_campom"], lopo_preds[mask]
    ))
    print(f"  pooled (on/off-covered rows only, n={int(mask.sum())}): MAE {sub_mae:.4f}")

    # Per-class-year breakdown on the covered subset.
    per_class = {}
    for code, sub in df[mask].groupby("prior_class_year_code"):
        per_class[int(code)] = {
            "n": int(len(sub)),
            "mae": float(mean_absolute_error(sub["target_campom"], lopo_preds[sub.index])),
        }
    print("  per prior-class (covered subset): "
          + "  ".join(f"{c}:{m['mae']:.3f}(n={m['n']})" for c, m in sorted(per_class.items())))

    return {
        "pooled_mae": lopo["pooled"]["mae"],
        "pooled_rmse": lopo["pooled"]["rmse"],
        "covered_mae": sub_mae,
        "per_pair": {k: v["mae"] for k, v in lopo["per_pair"].items()},
        "per_class": per_class,
    }


def main() -> None:
    production_cols = list(base.FEATURE_COLS)
    df = base.build_dataset().reset_index(drop=True)
    print(f"Rows: {len(df):,}")

    results = {
        "baseline": run_variant("baseline (on/off ablated)", df, BASELINE_COLS),
        "onoff": run_variant("production (with on/off)", df, production_cols),
    }

    print(f"\n{'=' * 64}\nSUMMARY (LOPO pooled MAE)\n{'=' * 64}")
    print(f"{'variant':<16} {'full':>9} {'covered':>10}")
    for name, r in results.items():
        print(f"{name:<16} {r['pooled_mae']:>9.4f} {r['covered_mae']:>10.4f}")

    b = results["baseline"]
    r = results["onoff"]
    print(f"\nonoff vs baseline: full Δ {b['pooled_mae'] - r['pooled_mae']:+.4f}  "
          f"covered Δ {b['covered_mae'] - r['covered_mae']:+.4f}  (positive = improvement)")
    wins = sum(
        1 for k in r["per_pair"] if r["per_pair"][k] < b["per_pair"].get(k, np.inf)
    )
    print(f"  per-pair wins: {wins}/{len(r['per_pair'])}")


if __name__ == "__main__":
    main()
