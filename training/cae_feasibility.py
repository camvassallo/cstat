"""Coach-Above-Expectation (CAE) feasibility — is there persistent between-coach signal?

Before scoping the full metric, answer the load-bearing question: does a coach's
over-performance vs the roster projection PERSIST (real skill) or is it season noise?
If between-coach variance ~0 / autocorrelation ~0, every shrunk rating collapses to 0
and the metric is not worth building.

CAE expectation denominator choices (both reported):
  - roster_proj : roster-talent-only projection (cleanest coach attribution, noisier)
  - served  : 0.5*baseline + 0.5*roster_proj (better-calibrated, but baseline leaks prior coaching)
Residual = actual - expectation. Positive = team beat its talent projection.
"""

import json
import urllib.request
from collections import defaultdict

BT = "eval_history/projections_backtest_per_team_5season_20260529.json"
URL = "https://barttorvik.com/coachdict.json"
ALIAS = {"Texas A&M Corpus Christi": "Texas A&M Corpus Chris"}


def cn(t):
    return ALIAS.get(t, t)


def mean(x):
    return sum(x) / len(x) if x else 0.0


def var(x):
    if len(x) < 2:
        return 0.0
    m = mean(x)
    return sum((v - m) ** 2 for v in x) / (len(x) - 1)


def std(x):
    return var(x) ** 0.5


def corr(xs, ys):
    if len(xs) < 2:
        return 0.0
    mx, my, sx, sy = mean(xs), mean(ys), std(xs), std(ys)
    if sx == 0 or sy == 0:
        return 0.0
    return sum((a - mx) * (b - my) for a, b in zip(xs, ys)) / ((len(xs) - 1) * sx * sy)


def load(denom):
    bt = json.load(open(BT))
    cd = json.loads(urllib.request.urlopen(URL).read())
    rows = []
    for r in bt:
        y = r["season"]
        name = cn(r["team_name"])
        coach = cd.get(str(y), {}).get(name)
        if coach is None:
            continue
        # Back-compat: old dumps key this `phase_b`, new dumps `roster_proj`.
        rp = r.get("roster_proj", r.get("phase_b"))
        exp = rp if denom == "roster_proj" else 0.5 * r["baseline"] + 0.5 * rp
        rows.append({"team": r["team_name"], "season": y, "coach": coach,
                     "resid": r["actual"] - exp})
    return rows


def variance_components(rows):
    """One-way unbalanced random effects: resid_{c,i} = a_c + eps. Returns
    (sigma2_within, sigma2_between, n0, reliability@n)."""
    by_coach = defaultdict(list)
    for x in rows:
        by_coach[x["coach"]].append(x["resid"])
    # only coaches with >=2 seasons contribute to the within estimate
    multi = {c: v for c, v in by_coach.items() if len(v) >= 2}
    N = sum(len(v) for v in multi.values())
    C = len(multi)
    if C < 2:
        return None
    grand = mean([r for v in multi.values() for r in v])
    ss_within = sum(sum((r - mean(v)) ** 2 for r in v) for v in multi.values())
    s2_within = ss_within / (N - C)
    ss_between = sum(len(v) * (mean(v) - grand) ** 2 for v in multi.values())
    ms_between = ss_between / (C - 1)
    n_sq = sum(len(v) ** 2 for v in multi.values())
    n0 = (N - n_sq / N) / (C - 1)
    s2_between = max(0.0, (ms_between - s2_within) / n0)
    return s2_within, s2_between, n0, C, N


def main():
    for denom in ["roster_proj", "served"]:
        rows = load(denom)
        print(f"\n{'='*68}\n  DENOMINATOR = {denom}   (residual = actual - {denom})\n{'='*68}")
        print(f"  team-seasons: {len(rows)}   total resid σ: {std([x['resid'] for x in rows]):.2f}")

        vc = variance_components(rows)
        if vc:
            s2w, s2b, n0, C, N = vc
            print(f"\n  Variance components (coaches w/ ≥2 seasons: C={C}, N={N}):")
            print(f"    σ²_within  (season noise)        = {s2w:6.2f}  (σ={s2w**0.5:.2f})")
            print(f"    σ²_between (true coach skill)    = {s2b:6.2f}  (σ={s2b**0.5:.2f})")
            icc = s2b / (s2b + s2w) if (s2b + s2w) else 0
            print(f"    ICC (1-season reliability)       = {icc:.3f}")
            for n in [2, 3, 4, 5]:
                rel = n * s2b / (n * s2b + s2w) if s2b > 0 else 0
                print(f"    reliability @ {n} seasons         = {rel:.3f}  "
                      f"(shrink toward 0 by {1-rel:.0%})")
            if s2b > 0:
                print(f"    shrinkage constant k = σ²_w/σ²_b = {s2w/s2b:.1f} seasons-equivalent")

        # Persistence: same coach, consecutive seasons (mostly same team)
        by_coach = defaultdict(dict)
        for x in rows:
            by_coach[x["coach"]][x["season"]] = (x["resid"], x["team"])
        same_team_pairs, diff_team_pairs = [], []
        for coach, seas in by_coach.items():
            for s in seas:
                if s + 1 in seas:
                    (r0, t0), (r1, t1) = seas[s], seas[s + 1]
                    (same_team_pairs if t0 == t1 else diff_team_pairs).append((r0, r1))
        print(f"\n  Year-over-year persistence (resid_s vs resid_s+1, same coach):")
        if same_team_pairs:
            xs, ys = zip(*same_team_pairs)
            print(f"    same team   n={len(same_team_pairs):3d}  "
                  f"corr={corr(list(xs), list(ys)):+.3f}  "
                  f"(coach skill + program effect + projection bias, confounded)")
        if diff_team_pairs:
            xs, ys = zip(*diff_team_pairs)
            print(f"    moved teams n={len(diff_team_pairs):3d}  "
                  f"corr={corr(list(xs), list(ys)):+.3f}  "
                  f"(pure transferable coach skill)")

        # Split-half reliability of coach mean (odd vs even seasons)
        odd, even = defaultdict(list), defaultdict(list)
        for x in rows:
            (odd if x["season"] % 2 else even)[x["coach"]].append(x["resid"])
        both = [c for c in odd if c in even]
        if len(both) >= 5:
            xs = [mean(odd[c]) for c in both]
            ys = [mean(even[c]) for c in both]
            print(f"    split-half (odd vs even yrs) n={len(both):3d}  "
                  f"corr={corr(xs, ys):+.3f}")

        # Program confound: do the top-CAE coaches cluster at blue-bloods the
        # projection is known to under-rate (the Auburn/Florida 2025 cluster)?
        cmean = {c: mean([r for s, (r, t) in by_coach[c].items()])
                 for c in by_coach if len(by_coach[c]) >= 3}
        top = sorted(cmean.items(), key=lambda kv: -kv[1])[:10]
        print(f"\n  Top raw CAE (coaches w/ ≥3 seasons, denom={denom}):")
        for c, m in top:
            teams = sorted({t for s, (r, t) in by_coach[c].items()})
            print(f"    {c:22s} CAE={m:+5.1f}  n={len(by_coach[c])}  {','.join(teams)}")


if __name__ == "__main__":
    main()
