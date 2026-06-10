"""
Tier-1 PBP features → trajectory model: accept/reject experiment.

Adds the prior-season PBP tag rates (migration 036) to the trajectory
feature set and re-runs the leave-one-pair-out backtest against the
current 48-feature baseline on the SAME corpus. Two encodings:

  A. raw season rates from player_season_stats (paint_rate, paint_fg_pct,
     perimeter_fg_pct, transition/2nd-chance/off-TO/fouls-drawn per-40)
  B. within-season percentiles from player_percentiles (the
     density-normalized form — NatStat tag density varies by season, so
     percentile is the cross-season-comparable encoding; see migration 036)

Coverage: contextual tags exist only 2020+ (2019 corrupt-gated), so pairs
with s_n <= 2019 carry the missing sentinel. Missing → -1.0, NOT NaN —
the Rust serve path fills missing with sentinels (no NaN plumbing), and
-1 is cleanly outside every real range ([0,1] rates, >=0 per-40s).

Decision metric: pooled LOPO MAE on the full corpus, plus pooled LOPO MAE
restricted to s_n >= 2020 pairs (where the features actually vary — the
honest signal of what the features buy going forward).

Comparison script only; does not touch production models or the meta.
"""

from __future__ import annotations

import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error

import train_trajectory_model as base
from db import get_engine

RAW_PBP_COLS = [
    "prior_paint_rate",
    "prior_paint_fg_pct",
    "prior_perimeter_fg_pct",
    "prior_transition_pts_per40",
    "prior_second_chance_pts_per40",
    "prior_points_off_turnovers_per40",
    "prior_fouls_drawn_per40",
]
PCTL_PBP_COLS = [
    "prior_paint_rate_pctl",
    "prior_paint_fg_pct_pctl",
    "prior_perimeter_fg_pct_pctl",
    "prior_transition_pts_per40_pctl",
    "prior_second_chance_pts_per40_pctl",
    "prior_points_off_turnovers_per40_pctl",
    "prior_fouls_drawn_per40_pctl",
]

MISSING_SENTINEL = -1.0

# Extend the locked production query with the PBP rate columns (same pssN
# row the other rate stats come from) and the percentile row (LEFT JOIN —
# percentile gate is 10 GP / 10 MPG, stricter than the trajectory 5/5 gate,
# so some qualified rows legitimately miss).
EXT_QUERY = base.PAIRED_QUERY.replace(
    "    -- Archetype mixture (primary + secondary)",
    """    -- PBP tag rates (Tier-1; NULL pre-2020 / 2019 / uncovered players)
    pssN.paint_rate                 AS prior_paint_rate,
    pssN.paint_fg_pct               AS prior_paint_fg_pct,
    pssN.perimeter_fg_pct           AS prior_perimeter_fg_pct,
    pssN.transition_pts_per40       AS prior_transition_pts_per40,
    pssN.second_chance_pts_per40    AS prior_second_chance_pts_per40,
    pssN.points_off_turnovers_per40 AS prior_points_off_turnovers_per40,
    pssN.fouls_drawn_per40          AS prior_fouls_drawn_per40,
    ppN.paint_rate_pct                 AS prior_paint_rate_pctl,
    ppN.paint_fg_pct_pct               AS prior_paint_fg_pct_pctl,
    ppN.perimeter_fg_pct_pct           AS prior_perimeter_fg_pct_pctl,
    ppN.transition_pts_per40_pct       AS prior_transition_pts_per40_pctl,
    ppN.second_chance_pts_per40_pct    AS prior_second_chance_pts_per40_pctl,
    ppN.points_off_turnovers_per40_pct AS prior_points_off_turnovers_per40_pctl,
    ppN.fouls_drawn_per40_pct          AS prior_fouls_drawn_per40_pctl,
    -- Archetype mixture (primary + secondary)""",
).replace(
    "LEFT JOIN recruits rec",
    """LEFT JOIN player_percentiles ppN
    ON ppN.player_id = base.pid_n AND ppN.season = base.s_n
LEFT JOIN recruits rec""",
)


def build_dataset_ext() -> pd.DataFrame:
    engine = get_engine()
    df = pd.read_sql(EXT_QUERY, engine, params={"seasons": list(base.SEASONS)})
    print(f"Loaded {len(df):,} paired rows (extended query).")

    df["prior_class_year_code"] = df["prior_class_year"].map(base.encode_class_year)
    df = base.add_archetype_columns(df)
    from recruit_features import derive_recruit_features
    df = derive_recruit_features(df, prior_season_col="s_n")

    pre = len(df)
    df = df.dropna(subset=["prior_campom", "prior_ogbpm", "prior_dgbpm"])
    if len(df) < pre:
        print(f"  dropped {pre - len(df)} rows missing GBPM components")

    for col in RAW_PBP_COLS + PCTL_PBP_COLS:
        df[col] = df[col].fillna(MISSING_SENTINEL).astype(float)

    cov = (df["prior_paint_rate"] != MISSING_SENTINEL).groupby(df["s_n"]).mean()
    print("PBP feature coverage by s_n:")
    print((cov * 100).round(1).to_string())
    return df.reset_index(drop=True)


def run_variant(name: str, df: pd.DataFrame, extra_cols: list[str]) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  (+{len(extra_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = (
        base.NUMERIC_FEATURE_COLS
        + base.ARCH_FEATURE_COLS
        + list(base.RECRUIT_FEATURE_NAMES)
        + extra_cols
    )
    lopo, lopo_preds = base.leave_one_pair_out(df)

    mask = (df["s_n"] >= 2020) & (df["s_n"] != 2019) & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(
        df.loc[mask, "target_campom"], lopo_preds[mask]
    ))
    print(f"  pooled (s_n>=2020 only, n={int(mask.sum())}): MAE {sub_mae:.4f}")

    # Per-class-year breakdown on the covered subset (Fr→So expected to
    # benefit most — style signals should matter more for young players).
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
    df = build_dataset_ext()
    print(f"Rows: {len(df):,}")

    results = {
        "baseline": run_variant("baseline (48 features)", df, []),
        "raw_rates": run_variant("raw rates (+7)", df, RAW_PBP_COLS),
        "percentiles": run_variant("percentiles (+7)", df, PCTL_PBP_COLS),
    }

    print(f"\n{'=' * 64}\nSUMMARY (LOPO pooled MAE)\n{'=' * 64}")
    print(f"{'variant':<16} {'full':>9} {'s_n>=2020':>10}")
    for name, r in results.items():
        print(f"{name:<16} {r['pooled_mae']:>9.4f} {r['covered_mae']:>10.4f}")

    b = results["baseline"]
    for name in ("raw_rates", "percentiles"):
        r = results[name]
        print(f"\n{name} vs baseline: full Δ {b['pooled_mae'] - r['pooled_mae']:+.4f}  "
              f"covered Δ {b['covered_mae'] - r['covered_mae']:+.4f}  (positive = improvement)")
        wins = sum(
            1 for k in r["per_pair"] if r["per_pair"][k] < b["per_pair"].get(k, np.inf)
        )
        print(f"  per-pair wins: {wins}/{len(r['per_pair'])}")


if __name__ == "__main__":
    main()
