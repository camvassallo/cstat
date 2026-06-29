"""
Step-0 serve-parity check for the multi-season trajectory features.

The accept/reject experiment (`experiment_trajectory_history.py`) validated
the lag-2 + slope block using NaN-native encoding — LightGBM routes NaN at a
split, but the Rust/ort serve path can't feed NaN through the ONNX input
tensor, so production fills SENTINELS instead (the on/off block already does
this: -999). Before locking the 60-feature contract we must confirm the win
SURVIVES the sentinel encoding the serve path will actually use.

Encoding under test (matches the planned production fill):
  - lag-2 LEVELS  (prior2_campom/mpg/gp/usg/ppg) -> -999 where no N-1 season
  - SLOPE deltas  (delta_campom/mpg/usg)         ->    0 where no N-1 season
  - has_prior2 indicator                          ->    0 where no N-1 season

Reports production vs plus_both under BOTH encodings on the identical row set
and split scheme, so the covered-subset delta is directly comparable to the
NaN-native experiment (covered 2.206 -> 2.141, +0.065).

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
from experiment_trajectory_history import EXT_QUERY

EVAL_DIR = Path(__file__).parent / "eval_history"

LEVEL_SENTINEL = -999.0  # lag-2 levels, mirrors ONOFF_MISSING_SENTINEL
DELTA_FILL = 0.0         # slope deltas: 0 is consistent train<->serve; has_prior2 isolates absence

LAG2_LEVELS = ["prior2_campom", "prior2_mpg", "prior2_gp", "prior2_usg", "prior2_ppg"]
DELTAS = ["delta_campom", "delta_mpg", "delta_usg"]
NEW_BLOCK = LAG2_LEVELS + ["has_prior2"] + DELTAS  # 9 features, head order


def run_variant(name: str, df: pd.DataFrame, feature_cols: list[str], covered) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  ({len(feature_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = list(feature_cols)
    lopo, lopo_preds = base.leave_one_pair_out(df)
    mask = covered & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(df.loc[mask, "target_campom"], lopo_preds[mask]))
    print(f"  pooled (prior2-covered rows only, n={int(mask.sum())}): MAE {sub_mae:.4f}")
    return {
        "pooled_mae": lopo["pooled"]["mae"],
        "covered_mae": sub_mae,
        "per_pair": {k: v["mae"] for k, v in lopo["per_pair"].items()},
    }


def main() -> None:
    production_cols = list(base.FEATURE_COLS)
    base.PAIRED_QUERY = EXT_QUERY
    df = base.build_dataset().reset_index(drop=True)

    # has_prior2 is set BEFORE sentinel fills so coverage is measured off the
    # real NULL pattern (the level columns are still NaN at this point).
    df["has_prior2"] = df["prior2_campom"].notna().astype(float)
    df["delta_campom"] = df["prior_campom"] - df["prior2_campom"]
    df["delta_mpg"] = df["prior_mpg"] - df["prior2_mpg"]
    df["delta_usg"] = df["prior_usg"] - df["prior2_usg"]
    covered = df["has_prior2"] == 1.0
    print(f"\nRows: {len(df):,}; prior-2 coverage: {covered.mean():.1%} ({int(covered.sum())} rows)")

    # --- NaN-native frame (reproduces the accept/reject experiment) ---
    df_nan = df.copy()

    # --- Sentinel frame (what the serve path will feed) ---
    df_sent = df.copy()
    for c in LAG2_LEVELS:
        df_sent[c] = df_sent[c].fillna(LEVEL_SENTINEL).astype(float)
    for c in DELTAS:
        df_sent[c] = df_sent[c].fillna(DELTA_FILL).astype(float)
    df_sent["has_prior2"] = df_sent["has_prior2"].fillna(0.0).astype(float)

    results = {
        "production": run_variant("production", df_nan, production_cols, covered),
        "plus_both_nan": run_variant("plus_both_nan", df_nan, production_cols + NEW_BLOCK, covered),
        "plus_both_sentinel": run_variant(
            "plus_both_sentinel", df_sent, production_cols + NEW_BLOCK, covered),
    }

    print(f"\n{'=' * 64}\nSUMMARY (LOPO MAE)\n{'=' * 64}")
    print(f"{'variant':<22} {'full':>9} {'covered':>9}")
    for name, r in results.items():
        print(f"{name:<22} {r['pooled_mae']:>9.4f} {r['covered_mae']:>9.4f}")

    p = results["production"]
    for name in ("plus_both_nan", "plus_both_sentinel"):
        r = results[name]
        wins = sum(1 for k in r["per_pair"] if r["per_pair"][k] < p["per_pair"].get(k, np.inf))
        print(f"\n{name} vs production: full Δ {p['pooled_mae'] - r['pooled_mae']:+.4f}  "
              f"covered Δ {p['covered_mae'] - r['covered_mae']:+.4f}  "
              f"(positive = improvement)  per-pair wins {wins}/{len(r['per_pair'])}")

    EVAL_DIR.mkdir(exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"trajectory_history_sentinel_{stamp}_summary.json"
    out_path.write_text(json.dumps({"results": results}, indent=2))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    main()
