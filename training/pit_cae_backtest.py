"""Point-in-time CAE backtest — does an additive coach term improve the roster
projection, without temporal leakage?

The load-bearing gate for "Coach +/- in projections" (ROADMAP §6). The served
`coach_ratings.cae_shrunk` pools ALL seasons (2016-2026), so adding it to a
forecast for season Y leaks the future — the same outcome-conditioned trap that
killed PR B ([[project_projection_q1_bias_refuted]]). This script recomputes the
coach term *point-in-time* (from seasons strictly < Y) and measures its marginal
accuracy against the roster-only projection.

What it does
------------
For each target season Y:
  1. Take every coach's prior team-season residuals  cae_raw = actual − roster_proj
     for seasons strictly < Y.
  2. Re-estimate one-way random-effects variance components (σ²_w, σ²_b) on the
     ENTIRE <Y cohort → k = σ²_w/σ²_b season-equivalents.
  3. Per coach: pit_term = (n/(n+k)) · mean(prior cae_raw).  Heavy EB shrinkage
     means a 1-2 season coach contributes ≈0; a coach with no prior season → 0
     (unknown, correctly no bonus). If σ²_b ≤ 0 on the <Y cohort, every term is 0.
  4. Score the team-season with  pred = roster_proj + pit_term  and compare against
     pred = roster_proj alone (the served roster-only number).

Then aggregate MAE/RMSE across all target years and report the lift, plus a
paired delta with a standard error (is the improvement bigger than noise?).
Gate reference: the coaching-change indicator died at ~0.002 MAE lift; treat
anything under ~0.01 MAE as "does not clear the bar → ship display-only."

Why the comparison is fair even though roster_proj is itself a LOSO prediction:
roster_proj is IDENTICAL in both arms (with-term vs without-term), so the measured
lift is attributable purely to the coach term — and the coach term is strictly
causal (seasons < Y only). roster_proj being roster-only + coach-blind is exactly
why the residual isolates the coach; its own LOSO fit using other seasons does
not leak THIS coach's future residual into the term.

Run
---
  python3 pit_cae_backtest.py                 # default: target 2019-2026
  python3 pit_cae_backtest.py --start-year 2020
  python3 pit_cae_backtest.py --term-threshold 1.5   # subset cut for "applied" teams
"""

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

from cae_feasibility import mean, variance_components
from compute_cae import EVAL_DIR, join_coaches, load_backtest
from db import get_engine


def std_err(deltas: list[float]) -> float:
    """Standard error of the mean of paired error deltas."""
    n = len(deltas)
    if n < 2:
        return 0.0
    m = mean(deltas)
    var = sum((d - m) ** 2 for d in deltas) / (n - 1)
    return (var / n) ** 0.5


def pit_terms(rows_before: list[dict], key: str) -> tuple[dict, float, float]:
    """Entity → shrunk point-in-time term from the <Y residual cohort.

    `key` selects the grouping field on each row (`coach_id` for the coach term,
    `team_natstat_id` for the program-persistence null). Returns (terms, s2w,
    s2b); terms is keyed by that field; absent entity → 0. If the <Y cohort has
    no between-entity variance, every term is 0."""
    vc = variance_components(
        [{"coach": r[key], "resid": r["cae_raw"]} for r in rows_before]
    )
    if vc is None:
        return {}, 0.0, 0.0
    s2w, s2b, _n0, _C, _N = vc
    if s2b <= 0:
        return {}, s2w, s2b
    k = s2w / s2b
    by_ent: dict[str, list[float]] = defaultdict(list)
    for r in rows_before:
        by_ent[r[key]].append(r["cae_raw"])
    terms = {}
    for ent, resids in by_ent.items():
        n = len(resids)
        terms[ent] = (n / (n + k)) * mean(resids)
    return terms, s2w, s2b


def evaluate(rows: list[dict], start_year: int, term_threshold: float,
             key: str = "coach_id") -> dict:
    """Walk-forward PIT evaluation. For each target year ≥ start_year, build the
    entity term (keyed by `key`) from prior seasons and score roster_proj vs
    roster_proj+term. `key='coach_id'` is the coach term; `key='team_natstat_id'`
    is the program-persistence null."""
    years = sorted({r["season"] for r in rows})
    target_years = [y for y in years if y >= start_year]

    # Per-coach team history (for the moved-team cut): the set of programs a
    # coach worked at strictly before each target year.
    prior_teams: dict[str, set] = defaultdict(set)

    scored = []  # one entry per scored target team-season
    per_year = {}

    for y in target_years:
        before = [r for r in rows if r["season"] < y]
        target = [r for r in rows if r["season"] == y]
        if not before:
            continue
        terms, s2w, s2b = pit_terms(before, key)
        k = (s2w / s2b) if s2b > 0 else float("inf")
        # Rebuild prior-team sets from scratch through season y-1.
        prior_teams.clear()
        for r in before:
            prior_teams[r["coach_id"]].add(r["team_natstat_id"])

        yr_rows = []
        for r in target:
            term = terms.get(r[key], 0.0)
            pt = prior_teams.get(r["coach_id"], set())
            moved = bool(pt) and r["team_natstat_id"] not in pt
            err_base = r["actual"] - r["roster_proj"]
            err_coach = r["actual"] - (r["roster_proj"] + term)
            rec = {
                "season": y,
                "coach_id": r["coach_id"],
                "coach": r["coach"],
                "team": r["team"],
                "roster_proj": r["roster_proj"],
                "actual": r["actual"],
                "term": term,
                "err_base": err_base,
                "err_coach": err_coach,
                "ae_base": abs(err_base),
                "ae_coach": abs(err_coach),
                "has_prior": r[key] in terms,
                "moved": moved,
            }
            yr_rows.append(rec)
            scored.append(rec)

        applied = [x for x in yr_rows if abs(x["term"]) >= term_threshold]
        per_year[y] = {
            "n": len(yr_rows),
            "n_with_prior": sum(1 for x in yr_rows if x["has_prior"]),
            "n_applied": len(applied),
            "k": k,
            "s2b": s2b,
            "mae_base": mean([x["ae_base"] for x in yr_rows]),
            "mae_coach": mean([x["ae_coach"] for x in yr_rows]),
            "mae_applied_base": mean([x["ae_base"] for x in applied]) if applied else 0.0,
            "mae_applied_coach": mean([x["ae_coach"] for x in applied]) if applied else 0.0,
        }

    return {"scored": scored, "per_year": per_year, "target_years": target_years}


def rmse(errs: list[float]) -> float:
    return (sum(e * e for e in errs) / len(errs)) ** 0.5 if errs else 0.0


def summarize(ev: dict, term_threshold: float) -> dict:
    scored = ev["scored"]
    n = len(scored)
    mae_base = mean([x["ae_base"] for x in scored])
    mae_coach = mean([x["ae_coach"] for x in scored])
    rmse_base = rmse([x["err_base"] for x in scored])
    rmse_coach = rmse([x["err_coach"] for x in scored])

    # Paired AE delta (base − coach): positive = coach term helped. SE → is the
    # mean improvement bigger than sampling noise?
    deltas = [x["ae_base"] - x["ae_coach"] for x in scored]
    mean_delta = mean(deltas)
    se = std_err(deltas)
    improved = sum(1 for d in deltas if d > 1e-9)
    worsened = sum(1 for d in deltas if d < -1e-9)

    # Subset where a non-trivial term was actually applied — the term is ≈0 for
    # most teams (no prior / heavy shrinkage), so the overall MAE barely moves
    # by construction. The honest question is whether it helps WHERE it fires.
    applied = [x for x in scored if abs(x["term"]) >= term_threshold]
    sub = {
        "n": len(applied),
        "mae_base": mean([x["ae_base"] for x in applied]) if applied else 0.0,
        "mae_coach": mean([x["ae_coach"] for x in applied]) if applied else 0.0,
        "mean_delta": mean([x["ae_base"] - x["ae_coach"] for x in applied]) if applied else 0.0,
        "se": std_err([x["ae_base"] - x["ae_coach"] for x in applied]) if applied else 0.0,
    }

    # Elite-roster cohort (top-quartile roster_proj): the structural worry is the
    # term re-crediting blue bloods for talent the projection already counts.
    srt = sorted(scored, key=lambda x: x["roster_proj"])
    q4 = srt[3 * len(srt) // 4:]
    elite = {
        "n": len(q4),
        "mae_base": mean([x["ae_base"] for x in q4]),
        "mae_coach": mean([x["ae_coach"] for x in q4]),
        "mean_term": mean([x["term"] for x in q4]),
    }

    return {
        "n_scored": n,
        "mae_base": mae_base,
        "mae_coach": mae_coach,
        "mae_lift": mae_base - mae_coach,
        "rmse_base": rmse_base,
        "rmse_coach": rmse_coach,
        "rmse_lift": rmse_base - rmse_coach,
        "paired_mean_delta": mean_delta,
        "paired_se": se,
        "paired_z": (mean_delta / se) if se > 0 else 0.0,
        "n_improved": improved,
        "n_worsened": worsened,
        "applied_subset": sub,
        "elite_roster_q4": elite,
        "term_threshold": term_threshold,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--start-year", type=int, default=2019,
                    help="first target season to score (needs prior seasons; default 2019)")
    ap.add_argument("--term-threshold", type=float, default=1.0,
                    help="|term| cutoff for the 'applied' subset (default 1.0 AdjEM)")
    args = ap.parse_args()

    bt = load_backtest()
    engine = get_engine()
    with engine.connect() as conn:
        rows = join_coaches(bt, conn)

    ev = evaluate(rows, args.start_year, args.term_threshold, key="coach_id")
    s = summarize(ev, args.term_threshold)

    # Program-persistence null: the SAME backtest keyed on team, not coach. If
    # this matches the coach lift, the "coach" term is mostly correcting
    # persistent program-level projection bias, not isolating coaching.
    ev_team = evaluate(rows, args.start_year, args.term_threshold, key="team_natstat_id")
    s_team = summarize(ev_team, args.term_threshold)

    print(f"\nwalk-forward PIT backtest — targets {ev['target_years'][0]}–{ev['target_years'][-1]}")
    print(f"{'year':>5} {'n':>4} {'prior':>5} {'appl':>4} {'k':>5} "
          f"{'MAE_base':>9} {'MAE_coach':>9} {'lift':>7}")
    for y, p in ev["per_year"].items():
        k = f"{p['k']:.1f}" if p["k"] != float("inf") else "inf"
        print(f"{y:>5} {p['n']:>4} {p['n_with_prior']:>5} {p['n_applied']:>4} {k:>5} "
              f"{p['mae_base']:>9.4f} {p['mae_coach']:>9.4f} "
              f"{p['mae_base'] - p['mae_coach']:>+7.4f}")

    print(f"\noverall (n={s['n_scored']}):")
    print(f"  MAE   base {s['mae_base']:.4f}  coach {s['mae_coach']:.4f}  "
          f"lift {s['mae_lift']:+.4f}")
    print(f"  RMSE  base {s['rmse_base']:.4f}  coach {s['rmse_coach']:.4f}  "
          f"lift {s['rmse_lift']:+.4f}")
    print(f"  paired ΔAE  mean {s['paired_mean_delta']:+.4f}  SE {s['paired_se']:.4f}  "
          f"z {s['paired_z']:+.2f}  (improved {s['n_improved']} / worsened {s['n_worsened']})")

    sub = s["applied_subset"]
    print(f"\napplied subset (|term| ≥ {args.term_threshold}, n={sub['n']}):")
    if sub["n"]:
        print(f"  MAE base {sub['mae_base']:.4f}  coach {sub['mae_coach']:.4f}  "
              f"ΔAE mean {sub['mean_delta']:+.4f}  SE {sub['se']:.4f}  "
              f"z {(sub['mean_delta']/sub['se']) if sub['se'] else 0:+.2f}")

    el = s["elite_roster_q4"]
    print(f"\nelite-roster cohort (top-quartile roster_proj, n={el['n']}):")
    print(f"  MAE base {el['mae_base']:.4f}  coach {el['mae_coach']:.4f}  "
          f"mean term {el['mean_term']:+.3f}")

    # Moved-team cut: the honest coaching signal (a coach carrying overperformance
    # to a NEW program can't be program persistence).
    moved = [x for x in ev["scored"] if x["moved"] and abs(x["term"]) >= args.term_threshold]
    print(f"\nmoved-team coaches w/ applied term (|term| ≥ {args.term_threshold}, n={len(moved)}):")
    if moved:
        mb = mean([x["ae_base"] for x in moved])
        mc = mean([x["ae_coach"] for x in moved])
        md = [x["ae_base"] - x["ae_coach"] for x in moved]
        print(f"  MAE base {mb:.4f}  coach {mc:.4f}  ΔAE mean {mean(md):+.4f}  "
              f"SE {std_err(md):.4f}  z {(mean(md)/std_err(md)) if std_err(md) else 0:+.2f}")

    # Program-persistence null comparison — is the coach lift > the team lift?
    print(f"\nprogram-persistence null (term keyed on TEAM, not coach):")
    print(f"  coach-keyed   MAE lift {s['mae_lift']:+.4f}  (z {s['paired_z']:+.2f})")
    print(f"  team-keyed    MAE lift {s_team['mae_lift']:+.4f}  (z {s_team['paired_z']:+.2f})")
    print(f"  coach − team  {s['mae_lift'] - s_team['mae_lift']:+.4f}  "
          f"(positive ⇒ coach identity adds beyond program persistence)")

    # Largest-|term| decomposition examples from the most recent target year —
    # the legibility check ("Duke: +28 roster, +2 Scheyer → +30").
    last = ev["target_years"][-1]
    last_rows = sorted([x for x in ev["scored"] if x["season"] == last],
                       key=lambda x: -abs(x["term"]))
    print(f"\nlargest coach terms in {last} (roster + coach → forecast vs actual):")
    for x in last_rows[:12]:
        print(f"  {x['coach']:22s} {x['team']:24s} "
              f"{x['roster_proj']:+6.1f} {x['term']:+5.1f} → "
              f"{x['roster_proj'] + x['term']:+6.1f}  (actual {x['actual']:+6.1f})")

    # Two-part gate. (1) absolute: does the persistence prior beat the noise
    # floor? (2) attribution: does the COACH key beat the TEAM (program) null?
    # A coach term may only move the *served* forecast if it clears BOTH — else
    # the lift is program-bias correction wearing a coach's name, and the honest
    # move is a program-keyed projection calibration + a display-only coach grade.
    floor = 0.01
    clears_floor = s["mae_lift"] >= floor
    beats_program = s["mae_lift"] - s_team["mae_lift"] > 0
    if clears_floor and beats_program:
        verdict = "wire COACH term into serving (clears floor AND beats program null)"
    elif clears_floor:
        verdict = ("persistence prior helps but it is PROGRAM-level, not coaching "
                   "(team null wins) → coach +/- display-only; add a program-keyed "
                   "projection calibration as a separate item")
    else:
        verdict = "does not clear the floor → ship coach +/- display-only"
    print(f"\nverdict: coach MAE lift {s['mae_lift']:+.4f} (floor {floor}); "
          f"program null {s_team['mae_lift']:+.4f}; coach−program "
          f"{s['mae_lift'] - s_team['mae_lift']:+.4f}")
    print(f"  → {verdict}")

    summary = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "start_year": args.start_year,
        "target_years": ev["target_years"],
        "overall": s,
        "per_year": ev["per_year"],
        "program_null_overall": s_team,
        "coach_minus_team_lift": s["mae_lift"] - s_team["mae_lift"],
        "bar": floor,
        "clears_floor": clears_floor,
        "beats_program_null": beats_program,
        "verdict": verdict,
    }
    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out = EVAL_DIR / f"pit_cae_backtest_{date_str}_summary.json"
    out.write_text(json.dumps(summary, indent=2, default=float))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
