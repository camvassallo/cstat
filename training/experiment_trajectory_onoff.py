"""
Tier-2 on/off features → trajectory model: accept/reject experiment.

Adds prior-season player on/off splits (the `player_on_off` rollup, PBP
item A) to the trajectory feature set and re-runs the leave-one-pair-out
backtest against the current 48-feature baseline on the SAME corpus:

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
from db import get_engine

ONOFF_COLS = [
    "prior_on_net_rtg",
    "prior_net_on_off",
    "prior_on_poss_share",
]

MISSING_SENTINEL = -999.0

# Extend the locked production query with the on/off columns. LEFT JOIN —
# the rollup's >=100-on-possession gate is stricter than the trajectory
# 5 MPG / 5 GP gate, so some qualified rows legitimately miss; 2019 misses
# entirely (no PBP/lineups).
EXT_QUERY = base.PAIRED_QUERY.replace(
    "    -- Archetype mixture (primary + secondary)",
    """    -- On/off splits (Tier-2; NULL for 2019 / sub-rotation players)
    ooN.on_net_rtg AS prior_on_net_rtg,
    ooN.net_on_off AS prior_net_on_off,
    CASE WHEN ooN.on_possessions_for + ooN.off_possessions_for > 0
         THEN ooN.on_possessions_for
              / (ooN.on_possessions_for + ooN.off_possessions_for)
    END AS prior_on_poss_share,
    -- Archetype mixture (primary + secondary)""",
).replace(
    "LEFT JOIN recruits rec",
    """LEFT JOIN player_on_off ooN
    ON ooN.player_id = base.pid_n AND ooN.season = base.s_n
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

    for col in ONOFF_COLS:
        df[col] = df[col].fillna(MISSING_SENTINEL).astype(float)

    cov = (df["prior_on_net_rtg"] != MISSING_SENTINEL).groupby(df["s_n"]).mean()
    print("On/off feature coverage by s_n:")
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
    df = build_dataset_ext()
    print(f"Rows: {len(df):,}")

    results = {
        "baseline": run_variant("baseline (48 features)", df, []),
        "onoff": run_variant("on/off (+3)", df, ONOFF_COLS),
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
