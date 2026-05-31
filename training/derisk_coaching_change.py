"""PR E de-risk: does a coaching change carry signal in the projection residuals?

Joins the offseason coaching-change flag (derived from barttorvik coachdict.json:
coach[Y][team] != coach[Y-1][team]) to the #96 5-season roster-projection backtest
residuals, and tests whether changed-coach teams carry larger / differently-shaped
error than the rest. Gate per ROADMAP PR E: only build the full feature if the lift
is real.

Served projection = 0.5*baseline + 0.5*phase_b (matches projections_backtest.rs:239).
Residual = actual - served_projection (signed); |residual| = absolute miss.
"""

import json
import math
import urllib.request

BT_PATH = "eval_history/projections_backtest_per_team_5season_20260529.json"
COACHDICT_URL = "https://barttorvik.com/coachdict.json"

# backtest team_name -> coachdict name (only where they differ)
NAME_ALIAS = {"Texas A&M Corpus Christi": "Texas A&M Corpus Chris"}


def cd_name(team):
    return NAME_ALIAS.get(team, team)


def mean(xs):
    return sum(xs) / len(xs)


def std(xs):
    if len(xs) < 2:
        return 0.0
    m = mean(xs)
    return math.sqrt(sum((x - m) ** 2 for x in xs) / (len(xs) - 1))


def welch_t(a, b):
    """Welch's t-test (unequal variance). Returns (t, approx_two_sided_p)."""
    ma, mb = mean(a), mean(b)
    va, vb = std(a) ** 2, std(b) ** 2
    na, nb = len(a), len(b)
    se = math.sqrt(va / na + vb / nb)
    if se == 0:
        return 0.0, 1.0
    t = (ma - mb) / se
    # Welch-Satterthwaite df
    df = (va / na + vb / nb) ** 2 / (
        (va / na) ** 2 / (na - 1) + (vb / nb) ** 2 / (nb - 1)
    )
    # two-sided p via normal approx (df large enough here); good enough for a gate
    z = abs(t)
    p = 2 * (1 - 0.5 * (1 + math.erf(z / math.sqrt(2))))
    return t, p, df


def main():
    bt = json.load(open(BT_PATH))
    cd = json.loads(urllib.request.urlopen(COACHDICT_URL).read())

    rows = []
    no_flag = 0
    for r in bt:
        y = r["season"]
        name = cd_name(r["team_name"])
        cur = cd.get(str(y), {}).get(name)
        prev = cd.get(str(y - 1), {}).get(name)
        if cur is None or prev is None:
            no_flag += 1
            continue
        served = 0.5 * r["baseline"] + 0.5 * r["phase_b"]
        resid = r["actual"] - served  # signed: + => team beat projection
        rows.append(
            {
                "team": r["team_name"],
                "season": y,
                "actual": r["actual"],
                "served": served,
                "resid": resid,
                "abs": abs(resid),
                "changed": cur != prev,
                "prev_coach": prev,
                "cur_coach": cur,
            }
        )

    chg = [x for x in rows if x["changed"]]
    same = [x for x in rows if not x["changed"]]

    print("=== PR E de-risk: coaching-change signal in projection residuals ===")
    print(f"joined team-seasons: {len(rows)}  (dropped {no_flag} w/o coach in Y or Y-1)")
    print(f"  changed-coach:   {len(chg)} ({100*len(chg)/len(rows):.1f}%)")
    print(f"  unchanged-coach: {len(same)}")
    print()

    def block(label, g):
        a = [x["abs"] for x in g]
        s = [x["resid"] for x in g]
        print(
            f"  {label:18s} n={len(g):4d}  "
            f"MAE={mean(a):5.2f}  |resid|σ={std(a):5.2f}  "
            f"bias={mean(s):+5.2f}  resid σ={std(s):5.2f}"
        )

    print("Error by group:")
    block("changed coach", chg)
    block("unchanged coach", same)
    print()

    # Test 1: is |residual| larger for changed-coach teams? (the PR E hypothesis)
    t, p, df = welch_t([x["abs"] for x in chg], [x["abs"] for x in same])
    dmae = mean([x["abs"] for x in chg]) - mean([x["abs"] for x in same])
    print(f"|residual| difference (changed - unchanged): {dmae:+.2f} MAE")
    print(f"  Welch t={t:+.2f}  p~{p:.4f}  df~{df:.0f}  "
          f"{'<-- SIGNIFICANT' if p < 0.05 else '(n.s.)'}")
    print()

    # Test 2: is the *variance* of the (signed) residual larger? coaching changes
    # add surprise in both directions -> variance, not necessarily bias.
    sc, ss = std([x["resid"] for x in chg]), std([x["resid"] for x in same])
    print(f"signed-residual σ:  changed={sc:.2f}  unchanged={ss:.2f}  "
          f"ratio={sc/ss:.2f}x")
    print()

    # Direction check: are changed-coach teams systematically over- or under-projected?
    bc, bs = mean([x["resid"] for x in chg]), mean([x["resid"] for x in same])
    print(f"signed bias:  changed={bc:+.2f}  unchanged={bs:+.2f}  "
          f"(+ = team beat projection)")
    print()

    # Worst changed-coach misses (the cases the audit flagged)
    print("Largest changed-coach misses (|resid| desc):")
    for x in sorted(chg, key=lambda z: -z["abs"])[:15]:
        print(
            f"  {x['team']:22s} {x['season']}  "
            f"served={x['served']:+6.1f} actual={x['actual']:+6.1f} "
            f"resid={x['resid']:+6.1f}  "
            f"{x['prev_coach']} -> {x['cur_coach']}"
        )
    print()

    # Roadmap-named anchor cases
    anchors = [("Auburn", 2025), ("Missouri", 2025), ("Maryland", 2025),
               ("Maryland", 2026), ("Florida", 2025)]
    print("Roadmap-named anchor cases:")
    by = {(x["team"], x["season"]): x for x in rows}
    for key in anchors:
        x = by.get(key)
        if x:
            tag = "CHANGED" if x["changed"] else "same"
            print(
                f"  {x['team']:10s} {x['season']}  [{tag:7s}] "
                f"served={x['served']:+6.1f} actual={x['actual']:+6.1f} "
                f"resid={x['resid']:+6.1f}  {x['prev_coach']} -> {x['cur_coach']}"
            )
        else:
            print(f"  {key[0]} {key[1]}: not in joined set")


def conditional_analysis():
    """Is the changed-coach residual DIRECTION predictable from roster strength?
    Hypothesis: new coach at a weak team = rebuild hire (trends up); new coach at
    a strong team = replacing success, talent often leaves (trends down)."""
    import json, urllib.request
    bt = json.load(open(BT_PATH))
    cd = json.loads(urllib.request.urlopen(COACHDICT_URL).read())
    rows = []
    for r in bt:
        y = r["season"]; name = cd_name(r["team_name"])
        cur = cd.get(str(y), {}).get(name); prev = cd.get(str(y - 1), {}).get(name)
        if cur is None or prev is None: continue
        served = 0.5 * r["baseline"] + 0.5 * r["phase_b"]
        rows.append({"served": served, "resid": r["actual"] - served,
                     "abs": abs(r["actual"] - served), "changed": cur != prev,
                     "team": r["team_name"], "season": y})
    chg = [x for x in rows if x["changed"]]

    # split changed-coach by served-projection sign (proxy for team strength)
    print("\n=== Conditional: changed-coach residual by projected strength ===")
    med = sorted(x["served"] for x in chg)[len(chg)//2]
    print(f"(median served projection among changed-coach teams = {med:+.1f})")
    for label, pred in [("weak (served < median)", lambda x: x["served"] < med),
                        ("strong (served >= median)", lambda x: x["served"] >= med)]:
        g = [x for x in chg if pred(x)]
        s = [x["resid"] for x in g]; a = [x["abs"] for x in g]
        print(f"  {label:26s} n={len(g):3d}  bias={mean(s):+6.2f}  "
              f"MAE={mean(a):5.2f}  σ={std(s):5.2f}")

    # quartile cut for finer resolution
    print("\n  by served quartile (changed-coach only):")
    srt = sorted(chg, key=lambda x: x["served"])
    q = len(srt) // 4
    for i, lab in enumerate(["Q1 weakest", "Q2", "Q3", "Q4 strongest"]):
        g = srt[i*q:(i+1)*q] if i < 3 else srt[i*q:]
        s = [x["resid"] for x in g]
        print(f"    {lab:14s} n={len(g):3d}  "
              f"served∈[{g[0]['served']:+.0f},{g[-1]['served']:+.0f}]  "
              f"bias={mean(s):+6.2f}")

    # correlation between served and residual among changed-coach teams
    xs = [x["served"] for x in chg]; ys = [x["resid"] for x in chg]
    mx, my = mean(xs), mean(ys)
    cov = sum((x-mx)*(y-my) for x, y in zip(xs, ys)) / (len(xs)-1)
    r = cov / (std(xs)*std(ys))
    print(f"\n  corr(served_projection, residual | changed-coach) = {r:+.3f}")
    print(f"  => slope sign {'NEGATIVE (strong teams underperform, weak overperform)' if r < 0 else 'positive'}")
    # same correlation for unchanged, as baseline (mean reversion exists for everyone)
    un = [x for x in rows if not x["changed"]]
    xs2 = [x["served"] for x in un]; ys2 = [x["resid"] for x in un]
    mx2, my2 = mean(xs2), mean(ys2)
    cov2 = sum((x-mx2)*(y-my2) for x, y in zip(xs2, ys2)) / (len(xs2)-1)
    r2 = cov2 / (std(xs2)*std(ys2))
    print(f"  corr(served, residual | UNCHANGED) = {r2:+.3f}  (baseline mean-reversion)")
    print(f"  excess reversion from coaching change: {r - r2:+.3f}")


def mae_ceiling():
    """Optimistic ceiling: apply the BEST in-sample linear correction to changed-coach
    teams (resid ~ a + b*served), recompute global MAE. In-sample => upper bound on
    what a real feature could deliver out-of-sample."""
    import json, urllib.request
    bt = json.load(open(BT_PATH))
    cd = json.loads(urllib.request.urlopen(COACHDICT_URL).read())
    rows = []
    for r in bt:
        y = r["season"]; name = cd_name(r["team_name"])
        cur = cd.get(str(y), {}).get(name); prev = cd.get(str(y - 1), {}).get(name)
        if cur is None or prev is None: continue
        served = 0.5 * r["baseline"] + 0.5 * r["phase_b"]
        rows.append({"served": served, "resid": r["actual"] - served,
                     "changed": cur != prev})
    chg = [x for x in rows if x["changed"]]
    xs = [x["served"] for x in chg]; ys = [x["resid"] for x in chg]
    mx, my = mean(xs), mean(ys)
    b = sum((x-mx)*(y-my) for x, y in zip(xs, ys)) / sum((x-mx)**2 for x in xs)
    a = my - b*mx
    print("\n=== Optimistic MAE ceiling (in-sample best correction) ===")
    base = mean([abs(x["resid"]) for x in rows])
    # apply correction only to changed teams; subtract predicted resid
    corr = []
    for x in rows:
        if x["changed"]:
            corr.append(abs(x["resid"] - (a + b*x["served"])))
        else:
            corr.append(abs(x["resid"]))
    print(f"  global MAE before:           {base:.4f}")
    print(f"  global MAE after correction: {mean(corr):.4f}")
    print(f"  in-sample best-case lift:    {base - mean(corr):+.4f} MAE")
    # also: just removing the changed-coach mean bias (boolean-only feature)
    bias = my
    corr2 = [abs(x["resid"] - (bias if x["changed"] else 0)) for x in rows]
    print(f"  boolean-only (mean-shift) lift: {base - mean(corr2):+.4f} MAE")


if __name__ == "__main__":
    main()
    conditional_analysis()
    mae_ceiling()
