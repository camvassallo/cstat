"""
Is the SERVED coach ranking measuring coaching, or program/projection residual?

Runs the coach-vs-program travel test on the ACTUAL coach_season_cae values
(cae_raw + cae_debiased, the headline), not the o/d proxy. Also checks whether
CAE just tracks team quality (correlation with actual AdjEM) -- the "excuse for
projections" failure mode.

If cae coach-travel r ~ 0 while program-across-coach r is large, the ranking is
program/projection identity, not individual coaching skill.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
from db import get_engine


def _coach_pairs(df, col, k=2):
    A, B = [], []
    for _, g in df.groupby("coach_id"):
        pm = g.groupby("prog").agg(n=("season", "size"), v=(col, "mean")).reset_index()
        pm = pm[pm.n >= k].sort_values("n", ascending=False)
        if len(pm) >= 2:
            A.append(pm.v.iloc[0]); B.append(pm.v.iloc[1])
    return np.array(A), np.array(B)


def _prog_pairs(df, col, k=2):
    A, B = [], []
    for _, g in df.groupby("prog"):
        cm = g.groupby("coach_id").agg(n=("season", "size"), v=(col, "mean")).reset_index()
        cm = cm[cm.n >= k].sort_values("n", ascending=False)
        if len(cm) >= 2:
            A.append(cm.v.iloc[0]); B.append(cm.v.iloc[1])
    return np.array(A), np.array(B)


def boot_ci(a, b, B=2000, seed=42):
    if len(a) < 3:
        return float("nan"), (float("nan"), float("nan"))
    rng = np.random.default_rng(seed)
    rs = [np.corrcoef(a[i], b[i])[0, 1] for i in (rng.integers(0, len(a), len(a)) for _ in range(B))]
    return float(np.corrcoef(a, b)[0, 1]), tuple(np.percentile(rs, [2.5, 97.5]))


def within_reliability(df, col, k=4):
    a, b = [], []
    for _, g in df.groupby(["coach_id", "prog"]):
        if len(g) < k:
            continue
        g = g.sort_values("season"); r = np.arange(len(g))
        a.append(g[col].values[r % 2 == 0].mean()); b.append(g[col].values[r % 2 == 1].mean())
    a, b = np.array(a), np.array(b)
    return (float(np.corrcoef(a, b)[0, 1]) if len(a) > 2 else float("nan")), len(a)


def main():
    eng = get_engine()
    df = pd.read_sql("""
        SELECT csc.coach_id, co.canonical_name AS coach, csc.season,
               csc.team_natstat_id AS prog,
               csc.actual_adjem, csc.projection, csc.cae_raw, csc.cae_debiased
        FROM coach_season_cae csc
        JOIN coaches co ON co.id = csc.coach_id
        WHERE csc.cae_raw IS NOT NULL
    """, eng)
    print(f"scored coach-seasons: {len(df)}  coaches: {df.coach_id.nunique()}  "
          f"programs: {df.prog.nunique()}")
    nprog = df.groupby("coach_id")["prog"].nunique()
    print(f"coaches at >=2 programs (scored): {(nprog>=2).sum()}")

    print("\n=== 'excuse for projections' check: what does CAE correlate with? ===")
    for col in ["cae_raw", "cae_debiased"]:
        rq = np.corrcoef(df[col], df["actual_adjem"])[0, 1]
        rp = np.corrcoef(df[col], df["projection"])[0, 1]
        print(f"  {col:<13} corr(CAE, actual_AdjEM)={rq:+.2f}   corr(CAE, projection)={rp:+.2f}")
    print("  (high corr with actual_AdjEM => CAE largely re-encodes team quality)")

    print("\n=== coach-travel vs program-across-coach (Pearson r [95% CI]) ===")
    print(f"  {'metric':<13}{'coach-travel r [CI]':<28}{'program r [CI]':<26}{'within-rel':>11}")
    for col in ["cae_raw", "cae_debiased"]:
        ca, cb = _coach_pairs(df, col); rc, cic = boot_ci(ca, cb)
        pa, pb = _prog_pairs(df, col); rp, cip = boot_ci(pa, pb)
        rel, nrel = within_reliability(df, col)
        dis = rc / rel if rel and rel > 0 else float("nan")
        print(f"  {col:<13}{rc:+.2f} [{cic[0]:+.2f},{cic[1]:+.2f}] n={len(ca):<6}"
              f"{rp:+.2f} [{cip[0]:+.2f},{cip[1]:+.2f}] n={len(pa):<5}{rel:+.2f}(d{dis:+.2f})")

    print("\n=== robustness: coach-travel at >=3 scored seasons/side ===")
    for col in ["cae_raw", "cae_debiased"]:
        a, b = _coach_pairs(df, col, k=3)
        r = float(np.corrcoef(a, b)[0, 1]) if len(a) > 2 else float("nan")
        print(f"  {col:<13} r={r:+.2f}  n={len(a)}")


if __name__ == "__main__":
    main()
