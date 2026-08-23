"""Point-in-time program-persistence calibration — does a team-keyed residual
prior improve the SERVED projection, or does the baseline blend already absorb it?

ROADMAP §6 "Program-persistence projection calibration" (the `← NEXT UP` item),
spun out of `pit_cae_backtest.py` ([[project_pit_cae_program_null]]): that backtest
found a team-keyed prior beats the raw roster projection `roster_proj` by +0.18 MAE —
*bigger* than the coach-keyed term, so the lift is program-level, not coaching.

But the SERVED forecast is NOT raw roster_proj. `/api/projections` ships
    served = the turnover-ramped, program-anchored blend (see `served_blend`)
i.e. it already leans 50% on last season's actual AdjEM — and *baseline is itself
a per-program persistence term*. The feasibility study warned this exact blend
"absorbs the persistent coach signal, driving σ²_between to 0" for the CAE null.
So before wiring a program prior into serving, the load-bearing question is:
**does the prior still add lift ON TOP OF the baseline-blended served projection,
or has baseline already eaten it?**

This script measures that. For a chosen base projector (`served` or `roster_proj`):
  1. residual_{program, season} = actual − base.
  2. Walk forward: for each target Y, EB-shrink each program's mean of its prior
     (season < Y) residuals — variance components re-estimated on the <Y cohort,
     term = (n/(n+k))·mean(prior resid), k = σ²_w/σ²_b. New/thin programs → ≈0.
  3. Score base+prior vs base alone; report MAE/RMSE lift + paired-delta z, and a
     low-end-tail check (does the prior over-correct the bottom quartile?).

The prior is keyed on `team_natstat_id` (the cross-season program key), NOT the
season-scoped `team_id` — a program appears once per target year under a
different base-season UUID but the same natstat_id.

Run
---
  python3 pit_program_calibration.py                 # base = served (the decision)
  python3 pit_program_calibration.py --base roster_proj  # reproduce the +0.18 reference
"""

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

from sqlalchemy import text

from cae_feasibility import mean, variance_components
from compute_cae import EVAL_DIR, load_backtest
from db import get_engine
from served_blend import OFFSET, W_STABLE, mirror_mismatches, served_prediction

# Kept for the summary artifact's provenance field. The blend itself comes from
# `served_blend`, which mirrors roster_projection.rs — this script hardcoded
# 0.50 through two regime changes (#322's turnover ramp, #325's program
# anchor), so its "served" column was measuring a blend nobody was served.
SHRINK_WEIGHT = W_STABLE


def attach_program_key(bt: list[dict], conn) -> list[dict]:
    """Attach `team_natstat_id` (the cross-season program key) + the served
    projection to each backtest row. The dump records the base-season team UUID;
    teams.natstat_id is stable across seasons, so it keys program persistence."""
    tid2ns = {
        r.id: r.natstat_id
        for r in conn.execute(text("SELECT id::text AS id, natstat_id FROM teams"))
    }
    stale = mirror_mismatches(bt)
    if stale:
        print(f"  ** WARNING: {stale} rows where `served_blend`'s mirrored constants "
              f"disagree with the dump's own `baseline_weight`. Either this dump "
              f"predates the current served blend or the mirror is stale against "
              f"roster_projection.rs — the `served` column below is not what anyone "
              f"is served, which is the exact defect this comparison exists to avoid.")
    rows, dropped = [], 0
    for r in bt:
        ns = tid2ns.get(r["team_id"])
        if ns is None:
            dropped += 1
            continue
        baseline = float(r["baseline"])
        roster_proj = float(r["roster_proj"])
        served = served_prediction(r)
        rows.append({
            "team_natstat_id": ns,
            "team": r["team_name"],
            "season": r["season"],
            "actual": float(r["actual"]),
            "roster_proj": roster_proj,
            "baseline": baseline,
            "served": served,
        })
    if dropped:
        print(f"  ({dropped} rows dropped: team_id not resolvable to natstat_id)")
    return rows


def pit_priors(rows_before: list[dict], base_kind: str) -> tuple[dict, float, float]:
    """program_natstat_id → EB-shrunk prior from the <Y residual cohort.
    residual = actual − base. Empty/zero when no between-program variance."""
    resids = [{"coach": r["team_natstat_id"], "resid": r["actual"] - r[base_kind]}
              for r in rows_before]
    vc = variance_components(resids)
    if vc is None:
        return {}, 0.0, 0.0
    s2w, s2b, _n0, _C, _N = vc
    if s2b <= 0:
        return {}, s2w, s2b
    k = s2w / s2b
    by_prog: dict[str, list[float]] = defaultdict(list)
    for r in rows_before:
        by_prog[r["team_natstat_id"]].append(r["actual"] - r[base_kind])
    priors = {p: (len(v) / (len(v) + k)) * mean(v) for p, v in by_prog.items()}
    return priors, s2w, s2b


def rmse(errs: list[float]) -> float:
    return (sum(e * e for e in errs) / len(errs)) ** 0.5 if errs else 0.0


def std_err(xs: list[float]) -> float:
    n = len(xs)
    if n < 2:
        return 0.0
    m = mean(xs)
    return (sum((x - m) ** 2 for x in xs) / (n - 1) / n) ** 0.5


def evaluate(rows: list[dict], base_kind: str, start_year: int) -> dict:
    years = sorted({r["season"] for r in rows})
    targets = [y for y in years if y >= start_year]
    scored, per_year = [], {}
    for y in targets:
        before = [r for r in rows if r["season"] < y]
        target = [r for r in rows if r["season"] == y]
        if not before:
            continue
        priors, s2w, s2b = pit_priors(before, base_kind)
        k = (s2w / s2b) if s2b > 0 else float("inf")
        yr = []
        for r in target:
            prior = priors.get(r["team_natstat_id"], 0.0)
            base = r[base_kind]
            rec = {
                "season": y, "team": r["team"], "team_natstat_id": r["team_natstat_id"],
                "base_pred": base, "prior": prior, "cal_pred": base + prior,
                "actual": r["actual"],
                "ae_base": abs(r["actual"] - base),
                "ae_cal": abs(r["actual"] - (base + prior)),
                "has_prior": r["team_natstat_id"] in priors,
            }
            yr.append(rec)
            scored.append(rec)
        per_year[y] = {
            "n": len(yr), "k": k,
            "mae_base": mean([x["ae_base"] for x in yr]),
            "mae_cal": mean([x["ae_cal"] for x in yr]),
        }
    return {"scored": scored, "per_year": per_year, "targets": targets}


def summarize(scored: list[dict]) -> dict:
    deltas = [x["ae_base"] - x["ae_cal"] for x in scored]
    mae_base = mean([x["ae_base"] for x in scored])
    mae_cal = mean([x["ae_cal"] for x in scored])
    se = std_err(deltas)
    # Low-end tail: does the prior over-correct the weakest projected quartile
    # (the calibration worry — lowballing portal-built mid-majors back upward)?
    srt = sorted(scored, key=lambda x: x["base_pred"])
    q1 = srt[: len(srt) // 4]
    return {
        "n": len(scored),
        "mae_base": mae_base, "mae_cal": mae_cal, "mae_lift": mae_base - mae_cal,
        "rmse_base": rmse([x["actual"] - x["base_pred"] for x in scored]),
        "rmse_cal": rmse([x["actual"] - x["cal_pred"] for x in scored]),
        "paired_mean_delta": mean(deltas), "paired_se": se,
        "paired_z": (mean(deltas) / se) if se > 0 else 0.0,
        "n_improved": sum(1 for d in deltas if d > 1e-9),
        "n_worsened": sum(1 for d in deltas if d < -1e-9),
        "low_q1": {
            "n": len(q1),
            "mae_base": mean([x["ae_base"] for x in q1]),
            "mae_cal": mean([x["ae_cal"] for x in q1]),
            "mean_prior": mean([x["prior"] for x in q1]),
        },
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", choices=["served", "roster_proj"], default="served",
                    help="projector to calibrate (default: served — the production number)")
    ap.add_argument("--start-year", type=int, default=2019)
    # load_backtest()'s fallback picks by filename, not recency; pass the dump
    # explicitly when analysing a specific projection generation (see #218).
    ap.add_argument("--dump", type=Path, default=None,
                    help="per-team backtest dump to read (default: newest by "
                         "filename, which is not always newest on disk)")
    args = ap.parse_args()

    bt = load_backtest(args.dump)
    engine = get_engine()
    with engine.connect() as conn:
        rows = attach_program_key(bt, conn)

    ev = evaluate(rows, args.base, args.start_year)
    s = summarize(ev["scored"])
    # Reference: always also measure against raw roster_proj so the served result is
    # read next to the +0.18 the original backtest reported.
    ref = summarize(evaluate(rows, "roster_proj", args.start_year)["scored"])

    print(f"\nprogram-persistence calibration — base = {args.base.upper()}  "
          f"(targets {ev['targets'][0]}–{ev['targets'][-1]})")
    print(f"{'year':>5} {'n':>4} {'k':>6} {'MAE_base':>9} {'MAE_cal':>9} {'lift':>8}")
    for y, p in ev["per_year"].items():
        kk = f"{p['k']:.1f}" if p["k"] != float("inf") else "inf"
        print(f"{y:>5} {p['n']:>4} {kk:>6} {p['mae_base']:>9.4f} {p['mae_cal']:>9.4f} "
              f"{p['mae_base'] - p['mae_cal']:>+8.4f}")

    print(f"\noverall (n={s['n']}):")
    print(f"  base={args.base}  MAE {s['mae_base']:.4f} → {s['mae_cal']:.4f}  "
          f"lift {s['mae_lift']:+.4f}  (RMSE {s['rmse_base']:.4f} → {s['rmse_cal']:.4f})")
    print(f"  paired ΔAE mean {s['paired_mean_delta']:+.4f}  SE {s['paired_se']:.4f}  "
          f"z {s['paired_z']:+.2f}  (improved {s['n_improved']} / worsened {s['n_worsened']})")
    lq = s["low_q1"]
    print(f"  low-end Q1 (n={lq['n']}): MAE {lq['mae_base']:.4f} → {lq['mae_cal']:.4f}  "
          f"mean prior {lq['mean_prior']:+.3f}")
    print(f"\n  reference — base=roster_proj lift {ref['mae_lift']:+.4f} (z {ref['paired_z']:+.2f})")

    floor = 0.01
    helps = s["mae_lift"] >= floor and s["paired_z"] >= 2.0
    if args.base == "served":
        verdict = ("WIRE program prior into served forecast" if helps else
                   "baseline blend already absorbs program persistence → do NOT change "
                   "the served forecast (ship a display-only decomposition at most)")
    else:
        verdict = f"reference only (raw roster_proj lift {s['mae_lift']:+.4f})"
    print(f"\nverdict ({args.base}): lift {s['mae_lift']:+.4f}, z {s['paired_z']:+.2f} → {verdict}")

    summary = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "base": args.base, "shrink_weight": SHRINK_WEIGHT, "offset": OFFSET,
        "start_year": args.start_year, "targets": ev["targets"],
        "overall_served_or_chosen": s, "reference_roster_proj": ref,
        "per_year": ev["per_year"], "floor": floor, "helps": helps, "verdict": verdict,
    }
    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out = EVAL_DIR / f"pit_program_calibration_{args.base}_{date_str}_summary.json"
    out.write_text(json.dumps(summary, indent=2, default=float))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
