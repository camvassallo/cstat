"""Quantify the marginal value of cstat's derived features in the freshman
model by retraining with progressively smaller feature subsets.

Three variants, all run through the same LOCO-by-class CV harness as the
production model so the MAEs are apples-to-apples with the shipped
`freshman_model_meta.json` numbers:

  1. `247_strict`  — only the 10 per-recruit 247 fields (recruit_*, drop
                     `years_since_recruit` which is degenerate for freshmen).
  2. `247_plus_peer` — variant 1 + `peer_class_strength` (a 247-aggregate
                       feature we derive from other recruits' ratings;
                       still 247-data-only at the source).
  3. `full` — production set (variant 2 + `committed_team_prior_adjem`,
              the only non-247-sourced feature).

Reuses `build_dataset()` and `lgb_params()` from `train_freshman_model.py`
so any future change to data path / hyperparameters carries through.
"""

import sys
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error

sys.path.insert(0, str(Path(__file__).parent))
from train_freshman_model import build_dataset, lgb_params, tier_mean_baseline

FEATURE_VARIANTS = {
    "247_strict": [
        "recruit_is_ranked",
        "recruit_composite_rank",
        "recruit_composite_rating",
        "recruit_star_rating",
        "recruit_position_rank",
        "recruit_rank_movement",
        "recruit_height_in",
        "recruit_weight_lb",
        "recruit_bmi_proxy",
        "recruit_position_code",
    ],
}
FEATURE_VARIANTS["247_plus_peer"] = FEATURE_VARIANTS["247_strict"] + ["peer_class_strength"]
FEATURE_VARIANTS["full"] = FEATURE_VARIANTS["247_plus_peer"] + ["committed_team_prior_adjem"]


def loco_mae(df: pd.DataFrame, feature_cols: list[str]) -> tuple[float, dict[int, float]]:
    """LOCO CV pooled MAE + per-class MAE, matching the production harness."""
    classes = sorted(df["recruit_year"].unique())
    preds = pd.Series(np.nan, index=df.index, dtype=float)
    for held in classes:
        train_mask = df["recruit_year"] != held
        test_mask = df["recruit_year"] == held
        model = lgb.LGBMRegressor(**lgb_params("regression"))
        model.fit(df.loc[train_mask, feature_cols].values,
                  df.loc[train_mask, "target_campom"].values)
        preds.loc[test_mask] = model.predict(df.loc[test_mask, feature_cols].values)

    pooled_mae = float(mean_absolute_error(df["target_campom"], preds))
    per_class: dict[int, float] = {}
    for held in classes:
        m = df["recruit_year"] == held
        per_class[int(held)] = float(mean_absolute_error(df.loc[m, "target_campom"], preds[m]))
    return pooled_mae, per_class


def main() -> None:
    df = build_dataset()
    print(f"Loaded {len(df):,} freshman rows across {df['recruit_year'].nunique()} classes")

    baseline = tier_mean_baseline(df)
    print(f"\nTier-mean baseline (in-sample, 4 buckets by composite_rank): MAE {baseline['mae']:.4f}")

    print("\n=== LOCO CV by class ===\n")
    print(f"{'variant':<18} {'features':>10} {'pooled_MAE':>12} {'vs_247_strict':>15}")
    print("-" * 60)

    results = {}
    for name, cols in FEATURE_VARIANTS.items():
        pooled, per_class = loco_mae(df, cols)
        results[name] = (pooled, per_class)
        delta = f"{results['247_strict'][0] - pooled:+.4f}" if "247_strict" in results else "—"
        print(f"{name:<18} {len(cols):>10d} {pooled:>12.4f} {delta:>15}")

    print("\n=== Per-class breakdown ===\n")
    classes = sorted(df["recruit_year"].unique())
    header = f"{'class':>6}  " + "  ".join(f"{v:>12}" for v in FEATURE_VARIANTS)
    print(header)
    print("-" * len(header))
    for c in classes:
        row = f"{int(c):>6d}  " + "  ".join(
            f"{results[v][1][int(c)]:>12.4f}" for v in FEATURE_VARIANTS
        )
        print(row)

    strict, plus_peer, full = (results[k][0] for k in ["247_strict", "247_plus_peer", "full"])
    print("\n=== Summary ===")
    print(f"  247 raw features only:               {strict:.4f}")
    print(f"  + peer_class_strength:               {plus_peer:.4f}  (lift {strict - plus_peer:+.4f})")
    print(f"  + committed_team_prior_adjem (full): {full:.4f}  (lift {plus_peer - full:+.4f})")
    print(f"\n  Full model total lift vs 247-strict: {strict - full:.4f}  "
          f"({(strict - full) / strict * 100:.1f}% relative)")


if __name__ == "__main__":
    main()
