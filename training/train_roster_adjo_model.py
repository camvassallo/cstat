"""
Roster-impact AdjO model — the offensive half of a NET+SPLIT team-rating
decomposition for the Future page.

The served `roster_impact_model.onnx` maps projected-roster aggregates ->
next-season team AdjEM (net). This trains an identical-shape model on the
SAME feature frame whose target is next-season `adj_offense` (absolute,
~105 scale). At serve time the Rust route runs both, keeps the net headline
untouched, and derives AdjD = AdjO - AdjEM (exact reconciliation, since
AdjEM = AdjO - AdjD holds to ~0.025 in the data).

Why NET+SPLIT and not two independent models: validated in
`validation/exp_team_adjod_projection.py` (LOSO, 4,255 team-seasons) —
decomposing barely touches the net (DIRECT-both net only +0.008 MAE worse
than served), because team-level O/D errors are positively correlated and
cancel in EM = O - D. AdjO is projectable at ~51% skill (MAE 3.42).

Reuses `build_dataset` / `lgb_params` / `export_to_onnx` from the net
trainer so the feature contract is byte-identical (the Rust boot validator
reuses ROSTER_IMPACT_FEATURE_NAMES for this model). Display-only — NOT a
coach grade, and it never moves the served net forecast.
"""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import mean_absolute_error

from db import get_engine
from provenance import input_provenance, oof_provenance_from
from train_roster_impact_model import (
    build_dataset,
    lgb_params,
    export_to_onnx,
    SEASONS,
    OUT_DIR,
)

TARGET = "adj_offense"


def load_target(seasons) -> pd.DataFrame:
    eng = get_engine()
    od = pd.read_sql(
        "SELECT team_id, season, adj_offense FROM team_season_stats "
        "WHERE adj_offense IS NOT NULL AND season = ANY(%(seasons)s)",
        eng, params={"seasons": list(seasons)},
    )
    od["team_id"] = od["team_id"].astype(str)
    return od


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print("=" * 60)
    print("Building dataset (reusing the served roster-impact frame)...")
    df, feature_cols, coverage = build_dataset()
    # Adjacent to the read it describes, not at meta-write time — see the note
    # in train_trajectory_model.main().
    stamp = input_provenance("roster_adjo")
    # feature_cols is fixed BEFORE this merge, so adj_offense can never leak
    # in as a feature (same discipline as the validation experiment).
    df["team_id"] = df["team_id"].astype(str)
    df = df.merge(load_target(SEASONS), on=["team_id", "season"], how="inner").reset_index(drop=True)
    df = df.dropna(subset=[TARGET]).reset_index(drop=True)
    assert TARGET not in feature_cols, "target leaked into features"
    print(f"Features: {len(feature_cols)} | rows with {TARGET}: {len(df)}")

    # LOSO: honest per-season MAE + the early-stopping iteration budget for
    # the final fit (mirrors train_roster_impact_model.leave_one_season_out).
    print("\n" + "=" * 60)
    print(f"Leave-one-season-out backtest (target={TARGET})")
    print("=" * 60)
    best_iters: list[int] = []
    oy, op = [], []
    per_season = {}
    for season in SEASONS:
        tr = df[df["season"] != season]
        te = df[df["season"] == season]
        if len(te) == 0:
            continue
        m = lgb.LGBMRegressor(**lgb_params())
        m.fit(
            tr[feature_cols], tr[TARGET],
            eval_set=[(te[feature_cols], te[TARGET])], eval_metric="mae",
        )
        bi = m.best_iteration_
        best_iters.append(bi if bi and bi > 0 else lgb_params()["n_estimators"])
        preds = m.predict(te[feature_cols])
        mae = mean_absolute_error(te[TARGET], preds)
        per_season[season] = {"mae": float(mae), "n": int(len(te))}
        oy.extend(te[TARGET].tolist()); op.extend(preds.tolist())
        print(f"  season {season}: MAE {mae:.2f}  n={len(te)}")
    loso_mae = float(mean_absolute_error(oy, op))
    naive = float(mean_absolute_error(oy, np.full(len(oy), np.mean(oy))))
    print(f"  pooled LOSO MAE {loso_mae:.3f}  (naive mean {naive:.3f}; "
          f"skill {100 * (1 - loso_mae / naive):.1f}%)")

    print("\n" + "=" * 60)
    print("Final fit on all data")
    print("=" * 60)
    final_params = lgb_params()
    final_params.pop("early_stopping_rounds", None)
    final_n = max(50, round(sum(best_iters) / len(best_iters)))
    final_params["n_estimators"] = final_n
    print(f"Final-fit n_estimators = {final_n}  (LOSO best-iters: {best_iters})")
    final = lgb.LGBMRegressor(**final_params)
    final.fit(df[feature_cols], df[TARGET])

    onnx_path = OUT_DIR / "roster_adjo_model.onnx"
    export_to_onnx(final, len(feature_cols), onnx_path)
    print(f"Exported ONNX → {onnx_path}")

    meta = {
        "model": "roster_adjo_model",
        "target": TARGET,
        "decomposition": "NET+SPLIT: AdjD derived as AdjO - AdjEM at serve time",
        "seasons": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(feature_cols),
        "features": feature_cols,
        # Must equal Rust QUAL_FILTER_STRING — validated at boot (same frame
        # as roster_impact_model, so the same gate).
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        "cam_v3_source": "oof",
        "cam_v3_coverage": coverage,
        # Full input fingerprint, the superset `check_provenance.py` reads
        # (issue #223). Declared identical to roster_impact's — the two share
        # one frame via `build_dataset`, so they cannot honestly differ.
        "input_provenance": stamp,
        # Must equal roster_impact_model_meta.json's stamp — the boot
        # validator compares them and refuses to serve a mismatched pair
        # (issue #218). Retrain BOTH whenever the OOF is regenerated;
        # `training/retrain_downstream.sh` does this in the right order.
        # Projected out of `stamp` so one run can never write two disagreeing
        # views of the same snapshot.
        "oof_provenance": oof_provenance_from(stamp),
        "final_n_estimators": final_n,
        "backtest_loso": {"pooled_mae": loso_mae, "naive_mae": naive,
                          "per_season": per_season},
    }
    meta_path = OUT_DIR / "roster_adjo_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
