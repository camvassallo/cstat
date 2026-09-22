"""Roster-impact AdjD model — the defensive half of the NET + O + D team-rating
decomposition on the Future page. Layer 2, display-only (#378).

CURRENT BEHAVIOUR
-----------------
- Same 27-feature frame as the net calibrator (imports `build_dataset` from
  `train_roster_impact_model`); LightGBM with the net trainer's `lgb_params`.
  Mirror of `train_roster_adjo_model.py` with the target swapped.
- Target: next-season `adj_defense` RELATIVE to the base season's league mean
  (`TARGET`, wire-locked against `cstat_core::inference::ROSTER_ADJD_TARGET`).
  Lower AdjD is better; the relative form is `adj_defense − mean(adj_defense,
  base season)`, so a negative value is a better-than-average defence. The
  serve path adds the base season's mean back, ex-ante. The meta records the
  mean per season (`league_mean_adjd_by_season`).
- NET + O + D, reconciled: `routes/projections.rs` runs the net model, the
  AdjO half and this one, then nudges the two halves by half the net residual
  (`O' = O + r/2`, `D' = D − r/2`, `r = net − (O − D)`) so `O' − D'` equals the
  served net exactly. Neither half ever moves the net headline. This replaces
  deriving AdjD as `AdjO − AdjEM`, which handed the whole program premium to
  whichever half history said (Saint Mary's 2027: #76 raw roster, new head
  coach, #10 projected defence) — see HISTORY.
- Judged walk-forward on the raw relative target (#361); LOSO kept for
  continuity. The served-blend, cohort-level judgement is
  `experiments/experiment_od_anchor.py`. Stamps `input_provenance`,
  `oof_provenance` (must equal the net and AdjO models' — the API refuses to
  boot otherwise, #218), `walk_forward`, `trained_at`.
- Exports `models/roster_adjd_model.onnx` + meta only — no per-season LOSO
  set. Reaches prod by git deploy alone, like the AdjO half.
- Needs its own invocation; `retrain_downstream.sh` runs all three Layer 2
  halves in order.

HISTORY
-------
- Until #378 there was no D model: AdjD was derived as AdjO − AdjEM, and the
  27 features carry no offence/defence information, so the O-vs-D allocation
  of a projection came entirely from the two program anchors. Re-weighting the
  AdjO anchor cannot fix that — with the net fixed, `err_D = err_O − err_net`,
  so every O-half gain lands on the derived D. A direct relative-AdjD model on
  its own anchor (`baseline_d`, 3-year `level_d`), reconciled with AdjO to the
  net, measured walk-forward through the served blend on the served ex-ante
  frame: AdjD 3.666 → 3.553 (−0.113, z=−6.2, better in all six seasons), AdjO
  3.931 → 3.865 (−0.066, z=−3.6), largest on prior-season top-25 defences
  (AdjD −0.222 / AdjO −0.191), new head coaches and overhauls, net untouched.
  Direct D unreconciled was −0.076 but left a mean 1.28-point AdjO − AdjD −
  AdjEM gap on the page (`eval_history/od_anchor_20260922_summary.json`).
"""
from __future__ import annotations

import datetime as _dt
import json
from pathlib import Path

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import mean_absolute_error

from db import get_engine
from provenance import input_provenance, oof_provenance_from
from walk_forward import WALK_FROM, regression_walk_forward
from train_roster_impact_model import (
    build_dataset,
    lgb_params,
    export_to_onnx,
    SEASONS,
    OUT_DIR,
)

# The column the model is fit on. Wire-locked: `cstat_core::inference`
# refuses to boot on any other value (`ROSTER_ADJD_TARGET`), because the serve
# path adds the base season's league mean to the model's output and a model
# trained on absolute AdjD would then be served ~100 points high.
TARGET = "adj_defense_relative_to_base_league_mean"
ABSOLUTE = "adj_defense"


def load_target(seasons) -> tuple[pd.DataFrame, dict[int, float]]:
    """Per (target-season team, season): absolute AdjD, the base season's
    league mean, and the relative target. Also returns the league means by
    season, stamped into the meta so the training-time definition is on
    record next to the one the serve path computes."""
    eng = get_engine()
    od = pd.read_sql(
        "SELECT team_id, season, adj_defense FROM team_season_stats "
        "WHERE adj_defense IS NOT NULL AND season = ANY(%(seasons)s)",
        eng, params={"seasons": list(seasons)},
    )
    od["team_id"] = od["team_id"].astype(str)
    # The D-I mean over every team with an AdjD that season — the same
    # population `fetch_league_mean_adj_d` averages on the Rust side.
    league = pd.read_sql(
        "SELECT season, avg(adj_defense) AS league_mean_adjd FROM team_season_stats "
        "WHERE adj_defense IS NOT NULL GROUP BY season", eng,
    )
    means = {int(r.season): float(r.league_mean_adjd) for r in league.itertuples()}
    od["league_mean_adjd_base"] = od["season"].map(lambda s: means.get(int(s) - 1))
    od[TARGET] = od[ABSOLUTE] - od["league_mean_adjd_base"]
    return od, means


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print("=" * 60)
    print("Building dataset (reusing the served roster-impact frame)...")
    df, feature_cols, coverage = build_dataset()
    # Adjacent to the read it describes, not at meta-write time — see the note
    # in train_trajectory_model.main().
    stamp = input_provenance("roster_adjd")
    # feature_cols is fixed BEFORE this merge, so adj_defense can never leak
    # in as a feature (same discipline as the validation experiment).
    df["team_id"] = df["team_id"].astype(str)
    targets, league_means = load_target(SEASONS)
    df = df.merge(targets, on=["team_id", "season"], how="inner").reset_index(drop=True)
    df = df.dropna(subset=[TARGET]).reset_index(drop=True)
    assert TARGET not in feature_cols and ABSOLUTE not in feature_cols, "target leaked into features"
    print(f"Features: {len(feature_cols)} | rows with {TARGET}: {len(df)}")
    print("  league mean AdjD by season: " + ", ".join(f"{s}: {m:.2f}" for s, m in sorted(league_means.items())))

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

    # The canonical judge (#361): train strictly earlier, fixed iterations.
    print("\n" + "=" * 60)
    print(f"Walk-forward (test {WALK_FROM}+, target={TARGET})")
    print("=" * 60)
    wf_params = dict(final_params)

    def fit_predict(x_tr, y_tr, x_te):
        return lgb.LGBMRegressor(**wf_params).fit(x_tr, y_tr).predict(x_te)

    walk, _ = regression_walk_forward(df[feature_cols], df["season"], df[TARGET], fit_predict, WALK_FROM, "adjd")

    onnx_path = OUT_DIR / "roster_adjd_model.onnx"
    export_to_onnx(final, len(feature_cols), onnx_path)
    print(f"Exported ONNX → {onnx_path}")

    meta = {
        "model": "roster_adjd_model",
        # Date of this fit (UTC). `docs/MODELS.md` reads it as the "last
        # retrain" column (#364); date-level so a same-day rerun of a
        # reproducible trainer (#222) still writes an identical meta.
        "trained_at": _dt.datetime.now(_dt.timezone.utc).date().isoformat(),
        # Wire-locked against `cstat_core::inference::ROSTER_ADJD_TARGET`.
        "target": TARGET,
        "serve_add_back": "league mean adj_defense of the base season (team_season_stats, every team with an AdjD)",
        "league_mean_adjd_by_season": {str(k): v for k, v in sorted(league_means.items())},
        "decomposition": "NET+O+D reconciled: O' = O + r/2, D' = D - r/2, r = net - (O - D) at serve time (#378)",
        "seasons": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(feature_cols),
        "features": feature_cols,
        # Must equal Rust QUAL_FILTER_STRING — validated at boot (same frame
        # as roster_impact_model, so the same gate).
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        "cam_v3_source": "oof_only",
        "frame_source": "ex_ante_backtest_composition",
        "cam_v3_coverage": coverage,
        # Full input fingerprint, the superset `check_provenance.py` reads
        # (issue #223). Declared identical to roster_impact's — the two share
        # one frame via `build_dataset`, so they cannot honestly differ.
        "input_provenance": stamp,
        # Must equal roster_impact_model_meta.json's and the AdjO meta's stamp
        # — the boot validator compares all three and refuses to serve a
        # mismatched set (issue #218). Retrain ALL whenever the OOF is regenerated;
        # `training/retrain_downstream.sh` does this in the right order.
        # Projected out of `stamp` so one run can never write two disagreeing
        # views of the same snapshot.
        "oof_provenance": oof_provenance_from(stamp),
        "final_n_estimators": final_n,
        "walk_forward": walk,
        "backtest_loso": {"pooled_mae": loso_mae, "naive_mae": naive,
                          "per_season": per_season},
    }
    meta_path = OUT_DIR / "roster_adjd_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
