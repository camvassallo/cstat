"""
Power / sufficiency check for the coach-vs-program refutation.

Concern: coach-travel r ~ 0 could mean (a) no coach signal, OR (b) too few
movers / too noisy per-stint to detect it (regression dilution). This script
distinguishes them:

  1. MOVEMENT CENSUS  -- how many coaches moved, crossings, seasons per stint.
  2. BOOTSTRAP CIs    -- on coach-travel r and program-across-coach r. Do their
                         CIs separate? Is coach-travel r confidently low?
  3. RELIABILITY CEILING -- within-(coach,program) split-half reliability of the
                         per-stint mean. Disattenuated coach-travel r =
                         observed / reliability. If even the disattenuated r is
                         small, a hidden coach effect is ruled out.
  4. ROBUSTNESS       -- repeat coach-travel with stricter >=3 seasons/side.
  5. CROSSED VARIANCE COMPONENTS -- tilt ~ (1|coach)+(1|program); coach variance
                         share is identifiable *because* some coaches move. If
                         it's ~0 with the data we have, we're confident.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
from derisk_cae_od_school_changers import build, coach_travel, program_across_coach


def boot_r(pairs_fn, df, col, B=1000, seed=42):
    """Bootstrap CI for the correlation produced by pairs_fn (returns r,n,rows
    where rows is list of (label,a,b))."""
    r0, n, rows = pairs_fn(df, col) if pairs_fn is program_across_coach else pairs_fn(df, col)
    # pairs_fn signatures differ; normalize to (a,b) arrays
    if pairs_fn is coach_travel:
        _, _, rws = coach_travel(df, col)
        a = np.array([x[1] for x in rws]); b = np.array([x[2] for x in rws])
    else:
        # program_across_coach returns only (r,n); rebuild pairs here
        a, b = _program_pairs(df, col)
    rng = np.random.default_rng(seed)
    rs = []
    for _ in range(B):
        idx = rng.integers(0, len(a), len(a))
        rs.append(np.corrcoef(a[idx], b[idx])[0, 1])
    return float(np.corrcoef(a, b)[0, 1]), len(a), np.percentile(rs, [2.5, 97.5])


def _program_pairs(df, col, min_per_coach=2):
    A, B = [], []
    for prog, g in df.groupby("prog"):
        cm = g.groupby("coach_id").agg(n=("season", "size"), v=(col, "mean")).reset_index()
        cm = cm[cm.n >= min_per_coach].sort_values("n", ascending=False)
        if len(cm) >= 2:
            A.append(cm.v.iloc[0]); B.append(cm.v.iloc[1])
    return np.array(A), np.array(B)


def _coach_pairs(df, col, min_per_prog=2):
    A, B = [], []
    for cid, g in df.groupby("coach_id"):
        pm = g.groupby("prog").agg(n=("season", "size"), v=(col, "mean")).reset_index()
        pm = pm[pm.n >= min_per_prog].sort_values("n", ascending=False)
        if len(pm) >= 2:
            A.append(pm.v.iloc[0]); B.append(pm.v.iloc[1])
    return np.array(A), np.array(B)


def boot_ci(a, b, B=2000, seed=42):
    rng = np.random.default_rng(seed)
    rs = [np.corrcoef(a[i], b[i])[0, 1] for i in (rng.integers(0, len(a), len(a)) for _ in range(B))]
    return float(np.corrcoef(a, b)[0, 1]), np.percentile(rs, [2.5, 97.5]), len(a)


def within_cell_reliability(df, col, min_seasons=4):
    """Split-half (odd/even seasons) within each (coach,program) cell with
    >=min_seasons; correlate half-means across cells. This is the test-retest
    reliability ceiling for a per-stint mean."""
    a, b = [], []
    for (cid, prog), g in df.groupby(["coach_id", "prog"]):
        if len(g) < min_seasons:
            continue
        g = g.sort_values("season"); rk = np.arange(len(g))
        ha = g[col].values[rk % 2 == 0]; hb = g[col].values[rk % 2 == 1]
        a.append(ha.mean()); b.append(hb.mean())
    a, b = np.array(a), np.array(b)
    r = float(np.corrcoef(a, b)[0, 1]) if len(a) > 2 else float("nan")
    return r, len(a)


def main():
    df = build()
    print("=" * 64)
    print("1. MOVEMENT CENSUS")
    nprog = df.groupby("coach_id")["prog"].nunique()
    seasons = df.groupby("coach_id").size()
    print(f"  total coached team-seasons: {len(df)}")
    print(f"  coaches: {df.coach_id.nunique()}  programs: {df.prog.nunique()}")
    print(f"  coaches at >=2 programs: {(nprog>=2).sum()}  >=3 programs: {(nprog>=3).sum()}")
    print(f"  total program-switch events: {(nprog-1).clip(lower=0).sum()}")
    # movers usable in coach-travel at various season thresholds
    for k in (2, 3, 4):
        a, b = _coach_pairs(df, "tilt", min_per_prog=k)
        print(f"  movers with >={k} seasons at each of 2 programs: {len(a)}")
    # seasons-per-stint distribution
    stint = df.groupby(["coach_id", "prog"]).size()
    print(f"  seasons per (coach,program) stint: median={stint.median():.0f} "
          f"mean={stint.mean():.2f} p25={stint.quantile(.25):.0f} p75={stint.quantile(.75):.0f}")

    print("\n" + "=" * 64)
    print("2. BOOTSTRAP CIs  (coach-travel vs program-across-coach)")
    print(f"  {'metric':<9} {'coach-travel r [95% CI]':<30} {'program r [95% CI]':<28}")
    for col in ["resid_O", "resid_D", "tilt"]:
        ca, cb = _coach_pairs(df, col); rc, cic, nc = boot_ci(ca, cb)
        pa, pb = _program_pairs(df, col); rp, cip, npp = boot_ci(pa, pb)
        print(f"  {col:<9} {rc:+.2f} [{cic[0]:+.2f},{cic[1]:+.2f}] n={nc:<8} "
              f"{rp:+.2f} [{cip[0]:+.2f},{cip[1]:+.2f}] n={npp}")

    print("\n" + "=" * 64)
    print("3. RELIABILITY CEILING + disattenuated coach-travel r")
    print(f"  {'metric':<9} {'within-stint reliability':<24} {'coach-travel r':<16} {'disattenuated':<14}")
    for col in ["resid_O", "resid_D", "tilt"]:
        rel, nrel = within_cell_reliability(df, col)
        ca, cb = _coach_pairs(df, col)
        rc = float(np.corrcoef(ca, cb)[0, 1])
        dis = rc / rel if (rel and rel > 0) else float("nan")
        print(f"  {col:<9} {rel:+.2f} (n={nrel:<3})           {rc:+.2f}            {dis:+.2f}")
    print("  (disattenuated = observed / reliability; if still small, no hidden coach effect)")

    print("\n" + "=" * 64)
    print("4. ROBUSTNESS: coach-travel r at >=3 seasons/side")
    for col in ["resid_O", "resid_D", "tilt"]:
        a, b = _coach_pairs(df, col, min_per_prog=3)
        r = float(np.corrcoef(a, b)[0, 1]) if len(a) > 2 else float("nan")
        print(f"  {col:<9} r={r:+.2f}  n={len(a)}")

    print("\n" + "=" * 64)
    print("5. CROSSED RANDOM-EFFECTS VARIANCE COMPONENTS (tilt ~ (1|coach)+(1|program))")
    try:
        import statsmodels.formula.api as smf
        d = df.copy()
        d["dummy"] = 1
        for col in ["resid_O", "resid_D", "tilt"]:
            d["_y"] = d[col]
            md = smf.mixedlm("_y ~ 1", d, groups=d["dummy"],
                             vc_formula={"coach": "0+C(coach_id)", "program": "0+C(prog)"})
            f = md.fit(reml=True, method="lbfgs")
            vc = f.vcomp  # [coach, program]
            resid = f.scale
            tot = vc[0] + vc[1] + resid
            print(f"  {col:<9} coach={vc[0]/tot:5.1%}  program={vc[1]/tot:5.1%}  "
                  f"residual={resid/tot:5.1%}")
    except Exception as e:
        print(f"  (mixed model unavailable/failed: {type(e).__name__}: {str(e)[:80]})")


if __name__ == "__main__":
    main()
