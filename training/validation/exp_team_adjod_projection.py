"""
Is projecting team AdjO / AdjD worth it (for a Future-page decomposition)?

The served roster-impact model maps roster aggregates -> next-season team
AdjEM (net). This asks whether the same features can project the O/D halves
(adj_offense / adj_defense) accurately, and which architecture to use if so.

Reuses the EXACT served feature frame (`build_dataset` from
train_roster_impact_model) and the same LOSO harness, just swapping/adding
targets. Identity in the data: AdjEM = AdjO - AdjD (mean |resid| 0.025), so
a split that predicts net + one half reconciles the other exactly.

Three architectures, matched LOSO folds:
  (DIRECT-net)  predict AdjEM directly         -- the SERVED model. Net ref.
  (NET+SPLIT)   predict AdjEM + AdjO; derive AdjD = AdjO - AdjEM.
                Served net UNTOUCHED; one extra model for the split.
  (DIRECT-both) predict AdjO and AdjD independently; net = AdjO - AdjD.
                Tests whether decomposing HURTS the net (var compounding).

Reports per-target LOSO MAE vs a season-mean naive baseline, the net
reconstruction under DIRECT-both, and the O/D error correlation (the
team-level halves are negatively correlated, unlike player cam, so
differencing may cancel rather than compound error).
"""
from __future__ import annotations

import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import mean_absolute_error

from db import get_engine
from train_roster_impact_model import build_dataset, lgb_params, SEASONS


def mae(a, b):
    a = np.asarray(a, float); b = np.asarray(b, float)
    m = np.isfinite(a) & np.isfinite(b)
    return float(np.mean(np.abs(a[m] - b[m])))


def fit_predict(Xtr, ytr, Xte):
    # The served params carry early_stopping_rounds (needs an eval set);
    # for this no-eval CV, drop it and use a fixed iteration budget near
    # where the production LOSO folds settle (~400).
    params = {k: v for k, v in lgb_params().items() if k != "early_stopping_rounds"}
    params["n_estimators"] = 400
    m = lgb.LGBMRegressor(**params)
    m.fit(Xtr, ytr)
    return m.predict(Xte)


def main():
    df, feature_cols, _cov = build_dataset()
    eng = get_engine()
    od = pd.read_sql(
        "SELECT team_id, season, adj_offense, adj_defense "
        "FROM team_season_stats "
        "WHERE adj_offense IS NOT NULL AND adj_defense IS NOT NULL "
        "AND season = ANY(%(seasons)s)",
        eng, params={"seasons": list(SEASONS)},
    )
    od["team_id"] = od["team_id"].astype(str)
    df["team_id"] = df["team_id"].astype(str)
    df = df.merge(od, on=["team_id", "season"], how="inner").reset_index(drop=True)
    print(f"Frame: {len(df):,} team-seasons with AdjEM + AdjO/AdjD; "
          f"{len(feature_cols)} features.")
    # Confirm the identity in this frame.
    resid = (df["adj_efficiency_margin"] - (df["adj_offense"] - df["adj_defense"])).abs().mean()
    print(f"identity check: mean |AdjEM - (AdjO - AdjD)| = {resid:.4f} (≈0 → split reconciles)")
    for nm in ("adj_offense", "adj_defense", "adj_efficiency_margin"):
        print(f"  SD[{nm}] = {df[nm].std():.2f}")
    r_od = np.corrcoef(df["adj_offense"], df["adj_defense"])[0, 1]
    print(f"  corr(AdjO, AdjD) = {r_od:+.3f} "
          f"(negative → good teams are high-O AND low-D; differencing amplifies EM spread)")

    X = df[feature_cols].values
    oof = {k: np.full(len(df), np.nan) for k in ("em", "o", "d")}
    naive = {k: np.full(len(df), np.nan) for k in ("em", "o", "d")}
    tgt = {"em": "adj_efficiency_margin", "o": "adj_offense", "d": "adj_defense"}

    for season in SEASONS:
        te = (df["season"] == season).values
        tr = ~te
        if te.sum() == 0:
            continue
        for k, col in tgt.items():
            oof[k][te] = fit_predict(X[tr], df[col].values[tr], X[te])
            naive[k][te] = df[col].values[tr].mean()  # season-blind train mean

    yem, yo, yd = (df[tgt[k]].values for k in ("em", "o", "d"))

    print("\n=== LOSO MAE (lower=better) ===")
    print(f"  {'target':<10}{'naive':>9}{'model':>9}{'skill%':>9}")
    for k, y in (("em", yem), ("o", yo), ("d", yd)):
        nv, md = mae(y, naive[k]), mae(y, oof[k])
        print(f"  {tgt[k]:<10}{nv:>9.3f}{md:>9.3f}{100 * (1 - md / nv):>8.1f}%")

    print("\n=== net reconstruction: does decomposing HURT the served net? ===")
    em_direct = mae(yem, oof["em"])
    net_recon = oof["o"] - oof["d"]                       # DIRECT-both net
    em_recon = mae(yem, net_recon)
    recon_resid = float(np.mean(np.abs(net_recon - oof["em"])))
    print(f"  DIRECT-net   (served)         AdjEM MAE = {em_direct:.3f}")
    print(f"  DIRECT-both  net=O_pred-D_pred AdjEM MAE = {em_recon:.3f}  "
          f"(Δ {em_recon - em_direct:+.3f} vs served)")
    print(f"  NET+SPLIT    net=AdjEM model            = {em_direct:.3f}  "
          f"(served net untouched by construction)")
    print(f"  |O_pred - D_pred - EM_pred| mean residual = {recon_resid:.3f} "
          f"(DIRECT-both fails to reconcile by this much; NET+SPLIT = 0)")

    print("\n=== O/D error structure ===")
    eo = oof["o"] - yo
    ed = oof["d"] - yd
    print(f"  corr(err_O, err_D) = {np.corrcoef(eo, ed)[0, 1]:+.3f} "
          f"(positive → errors cancel in EM=O-D; negative → compound)")

    print("\n=== NET+SPLIT split quality (served net + AdjO model -> AdjD) ===")
    d_split = oof["o"] - oof["em"]                        # AdjD = AdjO - AdjEM
    print(f"  AdjO  MAE = {mae(yo, oof['o']):.3f}  (SD {df['adj_offense'].std():.2f})")
    print(f"  AdjD  MAE (derived) = {mae(yd, d_split):.3f}  "
          f"(vs direct AdjD {mae(yd, oof['d']):.3f}; SD {df['adj_defense'].std():.2f})")


if __name__ == "__main__":
    main()
