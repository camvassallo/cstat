"""PR E follow-up: is the signal coach QUALITY (not the boolean change)?

The pooled change-flag nets to zero bias because upgrades (McCollum -> Iowa) and
downgrades (Steven Pearl -> Auburn, Buzz leaving Maryland) cancel. This tests the
sharper hypothesis: does an incoming coach's PRIOR over-performance (their mean
residual at earlier team-seasons) predict the new team's residual?
"""

import json
import urllib.request

BT = "eval_history/projections_backtest_per_team_5season_20260529.json"
URL = "https://barttorvik.com/coachdict.json"
ALIAS = {"Texas A&M Corpus Christi": "Texas A&M Corpus Chris"}


def cn(t):
    return ALIAS.get(t, t)


def mean(x):
    return sum(x) / len(x) if x else 0.0


def std(x):
    if len(x) < 2:
        return 0.0
    m = mean(x)
    return (sum((v - m) ** 2 for v in x) / (len(x) - 1)) ** 0.5


def corr(xs, ys):
    mx, my = mean(xs), mean(ys)
    sx, sy = std(xs), std(ys)
    if sx == 0 or sy == 0:
        return 0.0
    return sum((a - mx) * (b - my) for a, b in zip(xs, ys)) / ((len(xs) - 1) * sx * sy)


def main():
    bt = json.load(open(BT))
    cd = json.loads(urllib.request.urlopen(URL).read())
    rows = []
    for r in bt:
        y = r["season"]
        name = cn(r["team_name"])
        cur = cd.get(str(y), {}).get(name)
        prev = cd.get(str(y - 1), {}).get(name)
        served = 0.5 * r["baseline"] + 0.5 * r["phase_b"]
        rows.append({
            "team": r["team_name"], "season": y, "served": served,
            "resid": r["actual"] - served,
            "changed": cur is not None and prev is not None and cur != prev,
            "cur": cur, "prev": prev,
        })

    aub = [x for x in rows if x["team"] == "Auburn"]
    print("Auburn in backtest:",
          [(x["season"], x.get("cur"), round(x["resid"], 1)) for x in aub])
    print()

    print("=== changed-coach signed bias BY SEASON (is 2026 special?) ===")
    for y in [2022, 2023, 2024, 2025, 2026]:
        g = [x["resid"] for x in rows if x["season"] == y and x["changed"]]
        print(f"  {y}: n={len(g):3d}  bias={mean(g):+6.2f}  "
              f"MAE={mean([abs(v) for v in g]):5.2f}")
    print()

    # coach -> list of (season, resid) across all team-seasons
    hist = {}
    for x in rows:
        if x["cur"]:
            hist.setdefault(x["cur"], []).append((x["season"], x["resid"]))

    pairs = []
    for x in rows:
        if not x["changed"]:
            continue
        prior = [r for (s, r) in hist.get(x["cur"], []) if s < x["season"]]
        if prior:
            pairs.append((mean(prior), x["resid"], x["team"], x["season"],
                          x["cur"], len(prior)))

    print(f"=== Coach-quality test: incoming coach PRIOR CAE vs new-team residual "
          f"(n={len(pairs)} of {sum(1 for x in rows if x['changed'])} changes "
          f"have prior D-I record in window) ===")
    xs = [p[0] for p in pairs]
    ys = [p[1] for p in pairs]
    r = corr(xs, ys)
    print(f"  corr(prior_coach_CAE, new_team_resid) = {r:+.3f}   r2={r*r:.3f}")
    print(f"  slope sign: {'POSITIVE (good coaches carry over-performance)' if r > 0 else 'negative'}")
    print()
    print("  incoming coaches w/ best prior record (priorCAE desc):")
    for pr, res, team, sea, coach, n in sorted(pairs, key=lambda p: -p[0])[:12]:
        print(f"    {coach:22s} -> {team:16s} {sea}  "
              f"priorCAE={pr:+5.1f}(n={n})  newResid={res:+6.1f}")
    print("  incoming coaches w/ worst prior record:")
    for pr, res, team, sea, coach, n in sorted(pairs, key=lambda p: p[0])[:8]:
        print(f"    {coach:22s} -> {team:16s} {sea}  "
              f"priorCAE={pr:+5.1f}(n={n})  newResid={res:+6.1f}")


if __name__ == "__main__":
    main()
