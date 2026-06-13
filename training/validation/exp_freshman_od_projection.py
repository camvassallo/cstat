"""
Are freshman CPO / CPD projectable from recruit features?

The served freshman-impact model predicts a recruit's first-season net
CamPom (cam_gbpm_v3_psos). This asks whether the SAME recruit features can
project the O/D halves (cam_o/cam_d, envelope-gated), and how the split
compares to the net in skill — recruit→production is already noisy, so the
halves may carry even less signal.

Reuses the served freshman feature frame (`build_dataset` + `FEATURE_COLS`
from train_freshman_model) and its leave-one-class-out harness. Adds the
gated O/D targets by joining torvik_player_stats for the freshman season.

For net / cam_o / cam_d, reports LOCO MAE vs the tier-mean naive baseline
(skill %), so "worth it" = the split beats its own naive baseline by a
margin comparable to what net does.
"""
from __future__ import annotations

import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import mean_absolute_error

from db import get_engine
from train_freshman_model import build_dataset, FEATURE_COLS, lgb_params, tier_of


def mae(a, b):
    a = np.asarray(a, float); b = np.asarray(b, float)
    m = np.isfinite(a) & np.isfinite(b)
    return float(np.mean(np.abs(a[m] - b[m])))


def loco_oof(df, target):
    """Leave-one-recruit-class-out OOF predictions for `target`."""
    oof = pd.Series(index=df.index, dtype=float)
    for held in sorted(df["recruit_year"].unique()):
        tr = df[df["recruit_year"] != held]
        te = df[df["recruit_year"] == held]
        if len(te) == 0:
            continue
        m = lgb.LGBMRegressor(**lgb_params("regression"))
        m.fit(tr[FEATURE_COLS], tr[target])
        oof.loc[te.index] = m.predict(te[FEATURE_COLS])
    return oof.values


def tier_naive(df, target):
    """Per-tier mean, computed leave-one-class-out (train-only means)."""
    pred = pd.Series(index=df.index, dtype=float)
    for held in sorted(df["recruit_year"].unique()):
        tr = df[df["recruit_year"] != held]
        te = df[df["recruit_year"] == held]
        means = tr.groupby("tier")[target].mean().to_dict()
        gmean = tr[target].mean()
        pred.loc[te.index] = te["tier"].map(means).fillna(gmean).values
    return pred.values


def main():
    df = build_dataset()
    eng = get_engine()
    # Envelope-gated O/D halves for the freshman (target) season = recruit_year + 1.
    od = pd.read_sql(
        "SELECT player_id, season, cam_o_gbpm_v3_psos AS cam_o, "
        "cam_d_gbpm_v3_psos AS cam_d FROM torvik_player_stats "
        "WHERE cam_o_gbpm_v3_psos IS NOT NULL AND cam_d_gbpm_v3_psos IS NOT NULL "
        "AND abs(cam_o_gbpm_v3_psos) <= 30 AND abs(cam_d_gbpm_v3_psos) <= 30",
        eng,
    )
    od["player_id"] = od["player_id"].astype(str)
    df["cstat_player_id"] = df["cstat_player_id"].astype(str)
    df["target_season"] = df["recruit_year"] + 1
    df = df.merge(
        od.rename(columns={"player_id": "cstat_player_id", "season": "target_season"}),
        on=["cstat_player_id", "target_season"], how="inner",
    ).reset_index(drop=True)
    print(f"Frame: {len(df):,} freshmen with net + gated O/D targets "
          f"(of the qualified cohort; gating drops a few).")
    for nm in ("target_campom", "cam_o", "cam_d"):
        print(f"  SD[{nm}] = {df[nm].std():.2f}")
    r_od = np.corrcoef(df["cam_o"], df["cam_d"])[0, 1]
    print(f"  corr(cam_o, cam_d) = {r_od:+.3f}")

    print("\n=== freshman LOCO MAE (lower=better) ===")
    print(f"  {'target':<14}{'naive':>9}{'model':>9}{'skill%':>9}")
    rows = {}
    for col, label in (("target_campom", "net CamPom"), ("cam_o", "CPO"), ("cam_d", "CPD")):
        nv = mae(df[col].values, tier_naive(df, col))
        md = mae(df[col].values, loco_oof(df, col))
        rows[col] = (nv, md)
        print(f"  {label:<14}{nv:>9.3f}{md:>9.3f}{100 * (1 - md / nv):>8.1f}%")

    print("\n=== read ===")
    net_skill = 100 * (1 - rows["target_campom"][1] / rows["target_campom"][0])
    o_skill = 100 * (1 - rows["cam_o"][1] / rows["cam_o"][0])
    d_skill = 100 * (1 - rows["cam_d"][1] / rows["cam_d"][0])
    print(f"  net skill {net_skill:.1f}% | CPO {o_skill:.1f}% | CPD {d_skill:.1f}%")
    print("  CPD skill near/below 0 = defense unpredictable from recruit rank "
          "(rim protection/effort doesn't travel from a 247 number).")


if __name__ == "__main__":
    main()
