"""
Sensitivity sweep of CamPom constants.

Perturbs each CAMPOM_* knob by ±20% (one at a time), recomputes
end-of-season CamPom v3 using the per-game data in
`torvik_player_game_stats`, and reports rank correlation + top-50
intersection vs. baseline. A robust composite shows near-flat sensitivity
(Pearson r > 0.99 everywhere); load-bearing knobs show r dropping
materially.

This is the cheap-tier overfitting check from ROADMAP §"CamPom
overfitting audit". Doesn't need a Rust rebuild — we already have a
Python implementation in `compute_campom_at.py`.
"""

import argparse
from itertools import product

import pandas as pd
from scipy.stats import spearmanr

from compute_campom_at import (
    CAMPOM_DEFENSE_DISCOUNT,
    CAMPOM_GP_K,
    CAMPOM_MINUTES_EXPONENT,
    CAMPOM_OFFENSE_EXPONENT,
    CAMPOM_USG_REF,
    compute_at,
)
from db import get_engine

BASELINE = {
    "OFFENSE_EXPONENT": CAMPOM_OFFENSE_EXPONENT,
    "DEFENSE_DISCOUNT": CAMPOM_DEFENSE_DISCOUNT,
    "USG_REF": CAMPOM_USG_REF,
    "MINUTES_EXPONENT": CAMPOM_MINUTES_EXPONENT,
    "GP_K": CAMPOM_GP_K,
}


def compute_with_overrides(engine, season, cutoff, overrides) -> pd.DataFrame:
    """Same as compute_campom_at.compute_at but with knob overrides."""
    import compute_campom_at as cca
    saved = {k: getattr(cca, f"CAMPOM_{k}") for k in overrides.keys()}
    try:
        for k, v in overrides.items():
            setattr(cca, f"CAMPOM_{k}", v)
        df = compute_at(engine, season, cutoff)
    finally:
        for k, v in saved.items():
            setattr(cca, f"CAMPOM_{k}", v)
    return df


def compare(baseline: pd.DataFrame, perturbed: pd.DataFrame, label: str):
    merged = baseline.merge(perturbed, on="pid", suffixes=("_base", "_pert"))
    pearson = merged["cam_gbpm_v3_no_sos_base"].corr(merged["cam_gbpm_v3_no_sos_pert"])
    spear, _ = spearmanr(
        merged["cam_gbpm_v3_no_sos_base"], merged["cam_gbpm_v3_no_sos_pert"]
    )
    # Top-50 set intersection
    top50_base = set(merged.nlargest(50, "cam_gbpm_v3_no_sos_base")["pid"])
    top50_pert = set(merged.nlargest(50, "cam_gbpm_v3_no_sos_pert")["pid"])
    top50_overlap = len(top50_base & top50_pert)
    # Largest individual cam_gbpm shift (absolute)
    delta = (merged["cam_gbpm_v3_no_sos_pert"] - merged["cam_gbpm_v3_no_sos_base"]).abs()
    p95_delta = delta.quantile(0.95)
    return {
        "knob": label,
        "n": len(merged),
        "pearson": pearson,
        "spearman": spear,
        "top50_overlap": top50_overlap,
        "p95_abs_delta": p95_delta,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--season", type=int, default=2026)
    ap.add_argument("--cutoff", type=str, default="2026-04-06")
    ap.add_argument("--delta", type=float, default=0.20, help="±delta fraction (default 0.20 = ±20%)")
    args = ap.parse_args()

    engine = get_engine()
    print(f"Baseline CamPom @ {args.season} cutoff {args.cutoff} ...")
    baseline = compute_at(engine, args.season, args.cutoff)
    print(f"  {len(baseline)} players (GP>=5)")
    print(f"  baseline knobs: {BASELINE}")
    print()

    results = []
    for knob, baseval in BASELINE.items():
        for sign, label in [(+1, "+"), (-1, "-")]:
            new_val = baseval * (1.0 + sign * args.delta)
            print(f"  perturbing {knob} {label}{int(args.delta*100)}% = {baseval:.4f} → {new_val:.4f}")
            perturbed = compute_with_overrides(engine, args.season, args.cutoff, {knob: new_val})
            res = compare(baseline, perturbed, f"{knob}{label}{int(args.delta*100)}%")
            results.append(res)

    df = pd.DataFrame(results)
    print()
    print("=" * 84)
    print(f"  Sensitivity to ±{int(args.delta*100)}% perturbation of each CamPom constant")
    print("=" * 84)
    print(f"  {'knob':<25}{'n':>6}{'pearson':>10}{'spearman':>10}{'top50/50':>10}{'p95|Δ|':>10}")
    for _, r in df.iterrows():
        print(f"  {r['knob']:<25}{r['n']:>6}{r['pearson']:>10.4f}{r['spearman']:>10.4f}"
              f"{r['top50_overlap']:>9}{r['p95_abs_delta']:>10.3f}")

    # Verdict
    print()
    weak = df[df["pearson"] < 0.99]
    if len(weak) == 0:
        print("  VERDICT: all knobs robust (Pearson > 0.99 under ±20% perturbation).")
        print("           CamPom rank-order signal is not sensitive to constant choice.")
    else:
        print("  VERDICT: load-bearing knobs detected:")
        for _, r in weak.iterrows():
            print(f"    - {r['knob']}: Pearson {r['pearson']:.4f}, top50 overlap {r['top50_overlap']}/50")
        print("           These knobs deserve scrutiny in the methodology doc — small ")
        print("           changes meaningfully reorder the CamPom leaderboard.")

    # Persist
    df.to_csv(f"campom_sensitivity_{args.season}.csv", index=False)
    print(f"\n  Wrote campom_sensitivity_{args.season}.csv")


if __name__ == "__main__":
    main()
