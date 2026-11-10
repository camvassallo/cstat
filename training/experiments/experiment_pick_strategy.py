"""A betting strategy fitted walk-forward against historical lines (#447).

`experiment_pick_selection.py` asks which single rule would have done best,
choosing once on early seasons. This asks the question a bettor has: a
strategy across all three markets, re-fitted before every season on the
seasons already played, then graded on a season it never saw. Every number
quoted is that out-of-sample record, season by season; the parameters chosen
each year are printed so their stability can be judged too.

Per market, the parameters searched (flat one-unit stakes throughout, graded
at the listed price, or -110 without one, as `cstat_core::record` does):

- **spread**: the smallest edge in points worth a bet; the largest edge
  trusted (a line far from the model is as often a bad line as a beaten
  one); and whether November-December needs a bigger edge or is skipped.
- **total**: the projected total corrected by its own running bias first.
  The stored totals run 4-8 points low from 2019 on while the line is close,
  so an uncorrected edge is mostly that bias. The correction is the mean of
  `projected - actual` over the games already played this season (blended
  with last season's while few have been), so it uses nothing a bettor would
  not have had. Then a minimum edge and over/under/both.
- **moneyline**: the smallest edge of win probability over the vigged price,
  and the longest price taken (the rule served before this study took
  anything to +999).

**"served" in the output means the rules the site used before this study**
(spread and total when the cover chance beat the price, moneyline at any
edge up to +999), which is the baseline the strategy was judged against.
The site now serves the spread and moneyline rules this fit settled on.

Each is chosen to maximise units on the training seasons with at least
`MIN_BETS` bets, and judged on the next season. Training starts in 2016 and
the first judged season is `--first-test` (default 2019, so every choice has
three seasons under it).

**Honest only as far as the projections are.** Stored past-season
projections come from walk-forward bundles (#468), but the game model's
features other than CamPom and team strength are season aggregates (#274),
and the CamPom edge is unexplained (#466). Treat the absolute return as a
ceiling and the comparison with the served rules as the finding.

Run (CSV from `cstat-ingest backtest-picks --out`):
    cd training && ./.venv/bin/python experiments/experiment_pick_strategy.py \\
        --csv /tmp/picks.csv --out eval_history/pick_strategy_YYYYMMDD_summary.json

Reads the file only; writes nothing to the database or the models.
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import itertools
import json
import math

import numpy as np
import pandas as pd

from experiment_pick_selection import (
    PREVIOUS_LONGEST_MONEYLINE,
    SPREAD_SIGMA,
    STANDARD_ODDS,
    TOTAL_SIGMA,
    cover_prob,
    implied,
    load,
    settle,
    summarize,
)

MIN_BETS = 300
# Games of this season's own bias that count as much as last season's mean.
BIAS_PRIOR_GAMES = 300


# ------------------------------------------------------------------ spread
def spread_units(d: pd.DataFrame, min_edge, max_edge, early) -> pd.Series:
    """`early`: 'same' threshold, '+1' / '+2' points more in Nov-Dec, or 'skip'."""
    edge = d["projected_margin"] + d["home_spread"]
    a = edge.abs()
    need = pd.Series(float(min_edge), index=d.index)
    if early == "skip":
        need[d["early"]] = math.inf
    elif early != "same":
        need[d["early"]] += float(early)
    side = np.sign(edge)
    take = (a >= need) & (a < max_edge) & (a > 0)
    price = pd.Series(np.where(side > 0, d["home_spread_odds"].fillna(STANDARD_ODDS),
                               d["away_spread_odds"].fillna(STANDARD_ODDS)), index=d.index)
    return settle(pd.Series(np.where(take, side, 0), index=d.index),
                  d["actual_margin"] + d["home_spread"], price)


def spread_served(d: pd.DataFrame) -> pd.Series:
    edge = d["projected_margin"] + d["home_spread"]
    side = np.sign(edge)
    price = pd.Series(np.where(side > 0, d["home_spread_odds"].fillna(STANDARD_ODDS),
                               d["away_spread_odds"].fillna(STANDARD_ODDS)), index=d.index)
    take = (edge.abs() > 0) & (cover_prob(edge, SPREAD_SIGMA) > implied(price))
    return settle(pd.Series(np.where(take, side, 0), index=d.index),
                  d["actual_margin"] + d["home_spread"], price)


SPREAD_GRID = list(itertools.product(
    [0.5, 1, 1.5, 2, 2.5, 3, 4, 5],          # min edge
    [8, 10, 13, math.inf],                    # max edge
    ["same", 1, 2, "skip"],                   # Nov-Dec
))


# ------------------------------------------------------------------ total
def running_bias(df: pd.DataFrame) -> pd.Series:
    """Per game: the mean of projected - actual total over this season's
    games on earlier dates, shrunk toward last season's full mean by
    `BIAS_PRIOR_GAMES`. The first season has no prior and shrinks to zero."""
    d = df[df["projected_total"].notna()]
    err = d["projected_total"] - d["actual_total"]
    season_mean = err.groupby(d["season"]).mean()
    out = pd.Series(np.nan, index=df.index)
    for s, g in d.groupby("season"):
        prior = float(season_mean.get(s - 1, 0.0))
        e = err[g.index]
        daily = e.groupby(g["game_date"]).agg(["sum", "count"]).sort_index()
        # Strictly earlier dates: a game's own night is not known before tip.
        cum = daily.cumsum().shift(1).fillna(0.0)
        est = (cum["sum"] + prior * BIAS_PRIOR_GAMES) / (cum["count"] + BIAS_PRIOR_GAMES)
        out[g.index] = g["game_date"].map(est).to_numpy()
    return out


def total_units(d: pd.DataFrame, debias: bool, min_edge, sides) -> pd.Series:
    proj = d["projected_total"] - (d["bias"] if debias else 0.0)
    edge = proj - d["total_line"]
    side = np.sign(edge)
    take = (edge.abs() >= min_edge) & (edge.abs() > 0)
    if sides == "over":
        take &= side > 0
    elif sides == "under":
        take &= side < 0
    price = pd.Series(np.where(side > 0, d["over_odds"].fillna(STANDARD_ODDS),
                               d["under_odds"].fillna(STANDARD_ODDS)), index=d.index)
    return settle(pd.Series(np.where(take, side, 0), index=d.index),
                  d["actual_total"] - d["total_line"], price)


def total_served(d: pd.DataFrame) -> pd.Series:
    edge = d["projected_total"] - d["total_line"]
    side = np.sign(edge)
    price = pd.Series(np.where(side > 0, d["over_odds"].fillna(STANDARD_ODDS),
                               d["under_odds"].fillna(STANDARD_ODDS)), index=d.index)
    take = (edge.abs() > 0) & (cover_prob(edge, TOTAL_SIGMA) > implied(price))
    return settle(pd.Series(np.where(take, side, 0), index=d.index),
                  d["actual_total"] - d["total_line"], price)


TOTAL_GRID = list(itertools.product(
    [False, True],                            # correct the running bias
    [0.5, 1, 2, 3, 4, 6, 8],                  # min edge
    ["both", "over", "under"],
))


# ------------------------------------------------------------------ moneyline
def ml_units(d: pd.DataFrame, min_edge, cap) -> pd.Series:
    hp, ap = d["home_moneyline"], d["away_moneyline"]
    eh = np.where(hp > cap, -np.inf, d["home_win_prob"] - implied(hp))
    ea = np.where(ap > cap, -np.inf, (1 - d["home_win_prob"]) - implied(ap))
    side = np.where((eh > min_edge) & (eh >= ea), 1, np.where(ea > min_edge, -1, 0))
    price = np.where(side > 0, hp, ap)
    return settle(pd.Series(side, index=d.index), d["actual_margin"], pd.Series(price, index=d.index))


ML_GRID = list(itertools.product(
    [0.0, 0.02, 0.04, 0.06, 0.08, 0.10],      # min probability edge
    [100, 150, 200, 300, 500, PREVIOUS_LONGEST_MONEYLINE],  # longest price
))


# ------------------------------------------------------------------ walk-forward
def walk_forward(d: pd.DataFrame, grid, units_fn, served_fn, tests: list[int]):
    """Fit on every season before each test season, grade on it."""
    cache = {p: units_fn(d, *p) for p in grid}
    served = served_fn(d)
    seasons = d["season"]
    rows = []
    for s in tests:
        train = seasons < s
        best, best_units = None, -math.inf
        for p, u in cache.items():
            t = u[train].dropna()
            if len(t) >= MIN_BETS and t.sum() > best_units:
                best, best_units = p, float(t.sum())
        test = seasons == s
        rows.append({
            "season": s,
            "params": best,
            "strategy": summarize(cache[best][test]) if best else {"bets": 0},
            "served": summarize(served[test]),
            "_units": cache[best][test] if best else pd.Series(dtype=float),
            "_served": served[test],
        })
    return rows


def fmt(s: dict) -> str:
    if not s.get("bets"):
        return f"{'-':>30}"
    wp = f"{100 * s['win_pct']:.1f}%" if s["win_pct"] is not None else "-"
    return f"{s['bets']:>5} {wp:>6} {s['units']:>+7.1f}u {100 * s['roi']:>+6.1f}%"


def report(name: str, rows: list[dict]) -> dict:
    print(f"\n== {name} (walk-forward: fitted on earlier seasons, graded on this one)")
    print(f"{'season':>6}  {'params':<28} {'strategy: bets win% units roi':>30}   {'served':>30}")
    for r in rows:
        print(f"{r['season']:>6}  {str(r['params']):<28} {fmt(r['strategy'])}   {fmt(r['served'])}")
    strat = pd.concat([r["_units"] for r in rows])
    served = pd.concat([r["_served"] for r in rows])
    total = {"strategy": summarize(strat), "served": summarize(served)}
    late = [r for r in rows if r["season"] >= 2024]
    if late:
        total["strategy_2024_on"] = summarize(pd.concat([r["_units"] for r in late]))
        total["served_2024_on"] = summarize(pd.concat([r["_served"] for r in late]))
    print(f"{'all':>6}  {'':<28} {fmt(total['strategy'])}   {fmt(total['served'])}")
    if late:
        print(f"{'2024+':>6}  {'(sportsbook lines)':<28} {fmt(total['strategy_2024_on'])}   "
              f"{fmt(total['served_2024_on'])}")
    z = total["strategy"].get("z")
    if z is not None:
        print(f"        strategy z {z:+.1f}")
    return {
        "seasons": [{k: (list(v) if k == "params" and v else v) for k, v in r.items()
                     if not k.startswith("_")} for r in rows],
        **total,
        "_strat": strat,
        "_served": served,
    }


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--csv", required=True, help="file from `cstat-ingest backtest-picks --out`")
    ap.add_argument("--first-test", type=int, default=2019)
    ap.add_argument("--out", help="summary JSON")
    args = ap.parse_args()

    df = load(args.csv)
    df["bias"] = running_bias(df)
    tests = sorted(int(s) for s in df["season"].unique() if s >= args.first_test)

    sp = df[df["home_spread"].notna()]
    to = df[df["total_line"].notna() & df["projected_total"].notna() & df["bias"].notna()]
    ml = df[df["home_moneyline"].notna() & df["away_moneyline"].notna()]

    out = {"csv": args.csv, "min_bets": MIN_BETS, "bias_prior_games": BIAS_PRIOR_GAMES, "markets": {}}
    out["markets"]["spread"] = report("spread", walk_forward(sp, SPREAD_GRID, spread_units, spread_served, tests))
    out["markets"]["total"] = report("total", walk_forward(to, TOTAL_GRID, total_units, total_served, tests))
    out["markets"]["moneyline"] = report(
        "moneyline", walk_forward(ml, ML_GRID, ml_units, lambda d: ml_units(d, 0.0, PREVIOUS_LONGEST_MONEYLINE), tests))

    # Bias the correction leaves, per season: what the totals edge is made of.
    d = df[df["projected_total"].notna()]
    raw = (d["projected_total"] - d["actual_total"]).groupby(d["season"]).mean()
    left = (d["projected_total"] - d["bias"] - d["actual_total"]).groupby(d["season"]).mean()
    print("\n== total bias by season: raw, and after the running correction")
    print("  ".join(f"{s}: {raw[s]:+.1f} -> {left[s]:+.1f}" for s in raw.index))
    out["total_bias"] = {int(s): {"raw": float(raw[s]), "corrected": float(left[s])} for s in raw.index}

    strat = pd.concat([m.pop("_strat") for m in out["markets"].values()])
    served = pd.concat([m.pop("_served") for m in out["markets"].values()])
    out["portfolio"] = {"strategy": summarize(strat), "served": summarize(served)}
    print("\n== all three markets, walk-forward seasons")
    print(f"   strategy {fmt(out['portfolio']['strategy'])}")
    print(f"   served   {fmt(out['portfolio']['served'])}")

    if args.out:
        with open(args.out, "w") as f:
            json.dump(out, f, indent=1, default=float)
        print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
