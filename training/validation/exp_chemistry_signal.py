"""
ROADMAP §6 new-signal (3c): does returning-continuity / chemistry carry
projection signal the cam distribution does NOT already capture?

Test: regress the roster calibrator's HONEST LOSO residual (actual AdjEM −
projected) on a battery of continuity proxies. If a proxy has a meaningful,
significant slope on the residual, it's buildable signal. If flat, (3c) is
also dead and the projection is at its floor.

Continuity is keyed cross-season by torvik_pid (stable) per program (natstat).
A returner = a torvik_pid playing for the same program in S and S−1.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
from db import get_engine
from train_roster_impact_model import (
    TEAM_QUERY, OUTBOUND_QUERY, INBOUND_QUERY, SEASONS,
)
from exp_oracle_minutes_ceiling import PLAYER_QUERY_MIN, build, loso_oof

# LEAKAGE-FREE: projected (OOF) cam — what serving sees — NOT actual season-S
# cam (Σ actual cam ≈ AdjEM leaks the target). Also carries each player's
# PRIOR-season cam for an unambiguously-preseason talent measure.
CONT_QUERY = """
SELECT te.natstat_id AS prog, te.id AS team_id, pss.season,
       tps.torvik_pid AS pid, pss.minutes_per_game AS mpg,
       COALESCE(traj.mean, fresh.mean, tps.cam_gbpm_v3_psos) AS proj_cam
FROM player_season_stats pss
JOIN teams te ON te.id = pss.team_id
JOIN torvik_player_stats tps
    ON tps.player_id = pss.player_id AND tps.season = pss.season
LEFT JOIN trajectory_oof_predictions traj
    ON traj.torvik_pid = tps.torvik_pid AND traj.target_season = pss.season
LEFT JOIN freshman_oof_predictions fresh
    ON fresh.cstat_player_id = pss.player_id AND fresh.target_season = pss.season
WHERE pss.season = ANY(%(seasons)s)
  AND COALESCE(pss.games_played,0) >= 5 AND COALESCE(pss.minutes_per_game,0) >= 5
  AND tps.torvik_pid IS NOT NULL
"""


def continuity_features(cont: pd.DataFrame) -> pd.DataFrame:
    # prior-season roster: pid set AND each pid's prior (S-1) projected cam.
    prior_set, prior_cam = {}, {}
    for (prog, s), g in cont.groupby(["prog", "season"]):
        prior_set[(prog, s)] = set(g["pid"])
        prior_cam[(prog, s)] = dict(zip(g["pid"], g["proj_cam"]))
    rows = []
    for (prog, s), g in cont.groupby(["prog", "season"]):
        prev = prior_set.get((prog, s - 1))
        if prev is None:
            continue
        pcam = prior_cam.get((prog, s - 1), {})
        g = g.copy()
        g["ret"] = g["pid"].isin(prev)
        mtot = g["mpg"].sum()
        # cam-weighted continuity uses PROJECTED cam (serve-visible, leak-free)
        campos = g["proj_cam"].clip(lower=0)
        camtot = campos.sum()
        top3 = g.sort_values("proj_cam", ascending=False).head(3)
        # returners' PRIOR-season cam (unambiguously preseason)
        ret_prior = sum(max(0.0, pcam.get(pid, 0.0)) for pid in g.loc[g.ret, "pid"])
        rows.append({
            "team_id": g["team_id"].iloc[0], "season": s,
            "ret_min_share": g.loc[g.ret, "mpg"].sum() / mtot if mtot else 0.0,
            "ret_projcam_share": campos[g.ret.values].sum() / camtot if camtot else 0.0,
            "ret_count": int(g.ret.sum()),
            "ret_frac": g.ret.mean(),
            "ret_projcam_sum": g.loc[g.ret, "proj_cam"].sum(),
            "ret_priorcam_sum": ret_prior,
            "top3_continuity": int(top3["pid"].isin(prev).sum()),
            "roster_n": len(g),
        })
    return pd.DataFrame(rows)


def uni(resid, x):
    """Univariate: correlation, slope, t-stat of resid ~ x."""
    m = np.isfinite(resid) & np.isfinite(x)
    r, x_ = resid[m], x[m]
    n = len(r)
    xs = (x_ - x_.mean()) / (x_.std() + 1e-12)
    b = np.polyfit(xs, r, 1)[0]                  # slope per 1 SD of x
    corr = np.corrcoef(xs, r)[0, 1]
    t = corr * np.sqrt((n - 2) / max(1e-9, 1 - corr**2))
    return corr, b, t, n


def main():
    eng = get_engine()
    players = pd.read_sql(PLAYER_QUERY_MIN, eng, params={"seasons": list(SEASONS)})
    teams = pd.read_sql(TEAM_QUERY, eng, params={"seasons": list(SEASONS)})
    py = [s - 1 for s in SEASONS]
    outbound = pd.read_sql(OUTBOUND_QUERY, eng, params={"portal_years": py})
    inbound = pd.read_sql(INBOUND_QUERY, eng, params={"portal_years": py})

    # honest residual from the canonical calibrator
    df, fcols = build(players, teams, outbound, inbound, "canonical")
    oof = loso_oof(df, fcols)
    df = df.copy()
    df["resid"] = df["adj_efficiency_margin"].values - oof   # + = team over-performed projection

    cont = pd.read_sql(CONT_QUERY, eng, params={"seasons": list(SEASONS)})
    cf = continuity_features(cont)
    M = df.merge(cf, on=["team_id", "season"], how="inner")
    print(f"team-seasons with residual + continuity: {len(M)}")
    print(f"residual: mean {M.resid.mean():+.2f}  sd {M.resid.std():.2f}  "
          f"(|resid| mae {M.resid.abs().mean():.2f})")

    feats = ["ret_min_share", "ret_projcam_share", "ret_frac", "ret_count",
             "ret_projcam_sum", "ret_priorcam_sum", "top3_continuity", "roster_n"]
    print("\n=== does continuity explain the projection residual? (univariate) ===")
    print(f"  {'proxy':<16}{'corr':>8}{'slope/SD':>10}{'t':>8}  signal?")
    for f in feats:
        corr, b, t, n = uni(M["resid"].values, M[f].values.astype(float))
        sig = "<-- SIGNAL" if abs(t) > 2.5 and abs(b) > 0.5 else ("weak" if abs(t) > 2 else "")
        print(f"  {f:<16}{corr:>+8.3f}{b:>+10.3f}{t:>+8.2f}  {sig}")

    print("\n=== residual by returning-minutes-share quartile (does chemistry tilt it?) ===")
    M["q"] = pd.qcut(M["ret_min_share"], 4, labels=["Q1 overhaul", "Q2", "Q3", "Q4 continuous"])
    for ql in ["Q1 overhaul", "Q2", "Q3", "Q4 continuous"]:
        s = M[M.q == ql]
        print(f"  {ql:<15} n={len(s):<5} mean_resid={s.resid.mean():+.2f}  "
              f"mean_ret_share={s.ret_min_share.mean():.2f}")

    # multivariate: do ALL continuity proxies together explain residual variance?
    X = M[feats].apply(lambda c: (c - c.mean()) / (c.std() + 1e-12)).values
    A = np.column_stack([np.ones(len(X)), X])
    beta, *_ = np.linalg.lstsq(A, M["resid"].values, rcond=None)
    pred = A @ beta
    ss_res = ((M["resid"].values - pred) ** 2).sum()
    ss_tot = ((M["resid"].values - M["resid"].mean()) ** 2).sum()
    print(f"\n=== multivariate R² of ALL continuity proxies on residual: "
          f"{1 - ss_res/ss_tot:.4f} ===")
    print("  (near 0 => chemistry carries ~no signal the cam distribution misses => (3c) dead)")

    # DECISIVE: does adding continuity features actually cut LOSO MAE out-of-sample?
    print("\n" + "=" * 56)
    print("DECISIVE: retrain calibrator WITH continuity features (same rows)")
    add = ["ret_projcam_sum", "ret_priorcam_sum", "top3_continuity", "ret_projcam_share"]
    base_oof = loso_oof(M, fcols)
    cont_oof = loso_oof(M, fcols + add)
    y = M["adj_efficiency_margin"].values
    from sklearn.metrics import mean_absolute_error as MAE
    q = pd.qcut(y, 4, labels=["Q1", "Q2", "Q3", "Q4"])
    q1 = np.asarray(q == "Q1")
    print(f"  {'features':<22}{'pooled MAE':>11}{'Q1 bias':>10}")
    print(f"  {'base (27)':<22}{MAE(y, base_oof):>11.3f}{float((base_oof[q1]-y[q1]).mean()):>+10.2f}")
    print(f"  {'base + continuity':<22}{MAE(y, cont_oof):>11.3f}"
          f"{float((cont_oof[q1]-y[q1]).mean()):>+10.2f}")
    print(f"  Δ MAE = {MAE(y, cont_oof) - MAE(y, base_oof):+.3f}")


if __name__ == "__main__":
    main()
