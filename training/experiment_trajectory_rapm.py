"""
Prior-season RAPM → trajectory model: accept/reject experiment.

The user-question this answers: the shipped trajectory on/off block
(`prior_on_net_rtg` / `prior_net_on_off` / `prior_on_poss_share`) is the raw
on/off rollup — and per the RAPM spike (docs/rapm_methodology.md section 8),
per-season RAPM is 2-5x more *stable* year-over-year than raw on/off. Does
the more stable signal make a better trajectory feature?

Note the spike verdict already killed RAPM as a standalone value metric;
this is a different, narrower question — feature value in the established
LOPO harness, where raw on/off (YoY rho ~0.05-0.11) nonetheless earned its
slot. Stability is not the criterion here; the held-out verdict is.

Three variants on the production 51-feature contract:
  production — the shipped contract (on/off block included)
  plus_rapm  — + prior_o_rapm / prior_d_rapm / prior_net_rapm (54)
  swap_rapm  — on/off block out, RAPM block in (51)

RAPM features join from `player_rapm` (training/rapm.py, zero prior,
lambda=1000) on the prior-season side, same key as the on/off join. NULLs
(2019 priors, players outside that season's paired-stint corpus) become the
same -999 sentinel. Coverage runs HIGHER than on/off (measured 91.3% of
paired rows vs ~89%): the fit covers every player in a paired stint, with no
>=100-on-possession gate (the residual gap is 2019-prior rows).

Decision metric: pooled LOPO MAE, plus the covered-subset MAE and per-pair
wins — the same bar the on/off block cleared (full -0.006, covered -0.011,
9/11 pairs).

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

RAPM_COLS = ["prior_o_rapm", "prior_d_rapm", "prior_net_rapm"]
ONOFF_COLS = list(base.ONOFF_FEATURE_COLS)
SENTINEL = base.ONOFF_MISSING_SENTINEL
EVAL_DIR = Path(__file__).parent / "eval_history"

# Extend the production query with the prior-season player_rapm join —
# anchored on stable landmarks so a contract change upstream fails loudly
# here rather than silently dropping the block.
EXT_QUERY = base.PAIRED_QUERY.replace(
    "    -- Archetype mixture (primary + secondary)",
    "    prN.o_rapm AS prior_o_rapm,\n"
    "    prN.d_rapm AS prior_d_rapm,\n"
    "    prN.net_rapm AS prior_net_rapm,\n"
    "    -- Archetype mixture (primary + secondary)",
).replace(
    "LEFT JOIN recruits rec",
    "LEFT JOIN player_rapm prN\n"
    "    ON prN.player_id = base.pid_n AND prN.season = base.s_n\n"
    "LEFT JOIN recruits rec",
)
assert "prior_net_rapm" in EXT_QUERY and "player_rapm prN" in EXT_QUERY


def run_variant(name: str, df: pd.DataFrame, feature_cols: list[str]) -> dict:
    print(f"\n{'=' * 64}\nVariant: {name}  ({len(feature_cols)} features)\n{'=' * 64}")
    base.FEATURE_COLS = list(feature_cols)
    lopo, lopo_preds = base.leave_one_pair_out(df)

    mask = (df["prior_net_rapm"] != SENTINEL) & lopo_preds.notna()
    sub_mae = float(mean_absolute_error(
        df.loc[mask, "target_campom"], lopo_preds[mask]
    ))
    print(f"  pooled (RAPM-covered rows only, n={int(mask.sum())}): MAE {sub_mae:.4f}")
    return {
        "pooled_mae": lopo["pooled"]["mae"],
        "pooled_rmse": lopo["pooled"]["rmse"],
        "covered_mae": sub_mae,
        "per_pair": {k: v["mae"] for k, v in lopo["per_pair"].items()},
    }


def main() -> None:
    production_cols = list(base.FEATURE_COLS)
    base.PAIRED_QUERY = EXT_QUERY
    df = base.build_dataset().reset_index(drop=True)
    for col in RAPM_COLS:
        df[col] = df[col].fillna(SENTINEL).astype(float)
    cov = float((df["prior_net_rapm"] != SENTINEL).mean())
    print(f"Rows: {len(df):,}; RAPM feature coverage: {cov:.1%}")

    plus_cols = production_cols + RAPM_COLS
    swap_cols = [c for c in production_cols if c not in ONOFF_COLS] + RAPM_COLS

    results = {
        "production": run_variant("production (51, on/off)", df, production_cols),
        "plus_rapm": run_variant("production + RAPM block", df, plus_cols),
        "swap_rapm": run_variant("on/off swapped for RAPM", df, swap_cols),
    }

    print(f"\n{'=' * 64}\nSUMMARY (LOPO pooled MAE)\n{'=' * 64}")
    print(f"{'variant':<14} {'full':>9} {'rapm-covered':>14}")
    for name, r in results.items():
        print(f"{name:<14} {r['pooled_mae']:>9.4f} {r['covered_mae']:>14.4f}")

    p = results["production"]
    for name in ("plus_rapm", "swap_rapm"):
        r = results[name]
        wins = sum(
            1 for k in r["per_pair"] if r["per_pair"][k] < p["per_pair"].get(k, np.inf)
        )
        print(f"\n{name} vs production: full Δ {p['pooled_mae'] - r['pooled_mae']:+.4f}  "
              f"covered Δ {p['covered_mae'] - r['covered_mae']:+.4f}  "
              f"(positive = improvement)  per-pair wins {wins}/{len(r['per_pair'])}")

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"trajectory_rapm_experiment_{stamp}_summary.json"
    out_path.write_text(json.dumps(
        {"rapm_coverage": cov, "n_rows": int(len(df)), "results": results},
        indent=2,
    ))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    main()
