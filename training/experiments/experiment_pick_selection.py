"""Which pick-selection rule would have done best against the closing line (#447)?

Reads the per-game file `cstat-ingest backtest-picks --out` writes: every
played game's projection, its lines and prices, the served pick on each market
and how it came out. Re-runs the markets under alternative selection rules and
grades them the way `cstat_core::record` does (units at the listed price, or
-110 without one; a push returns the stake).

**Choose early, judge late.** Every rule here is a choice made after looking at
history, so a grid searched over every season only finds the rule that fit
those seasons best. Rules are chosen on seasons up to `--choose-through` and
judged on the seasons after it, which the choice never saw; the judge window
is the only number to quote. The full grid is printed for both windows so a
rule can be inspected, not so the best judge-window row can be picked from it.

**The file is only as honest as its projections.** A season graded on stored
`game_projections` rows written by a bundle that trained on that season shows
the leak (#468): about 58% against the spread where an honest frame is about
54%. Re-run `game-projections` with the held-out bundles before trusting a
past season. The per-season line check flags seasons whose lines look wrong
(a line that tracks results worse than the model is more likely bad data than
a beaten market).

Rules, per market:
- **spread**: the served rule (chance of covering beats the price, sigma
  10.46), a minimum edge in points, an edge band, each also restricted to the
  favourite or the underdog, the home or the away side, and November-December
  or January on.
- **total**: the served rule (sigma 17.2), a minimum edge, over or under only.
- **moneyline**: the served rule (any edge over the vigged price, nothing
  past +999), a minimum probability edge, a price cap, favourites or
  underdogs only.

Calibration: the sigma that best explains cover outcomes on the choose window
(maximum likelihood on the served cover curve), against the served one,
and the realised cover rate by edge bucket.

Run:
    cargo run --release --bin cstat-ingest -- backtest-picks \\
        --years 2016,2017,2018,2019,2020,2021,2022,2023,2024,2025,2026 --out /tmp/picks.csv
    cd training && ./.venv/bin/python experiments/experiment_pick_selection.py \\
        --csv /tmp/picks.csv --out eval_history/pick_selection_YYYYMMDD_summary.json

Reads the file only; writes nothing to the database or the models.
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import argparse
import json
import math
from collections.abc import Callable
from dataclasses import dataclass

import numpy as np
import pandas as pd
from scipy.optimize import minimize_scalar

# The served constants (`cstat_core::projection::PREDICT_SIGMA_PROD`,
# `cstat_core::record::{TOTAL_SIGMA, LONGEST_MONEYLINE, STANDARD_ODDS}`).
SPREAD_SIGMA = 10.46
TOTAL_SIGMA = 17.2
LONGEST_MONEYLINE = 999
STANDARD_ODDS = -110

# A rule needs this many bets on the choose window to be chosen: below it the
# grid's best row is mostly the luckiest small sample.
MIN_BETS = 200


# ------------------------------------------------------------------ odds
def cover_prob(edge, sigma: float):
    """`cstat_core::projection::win_prob_at_sigma`: the logistic approximation
    of the normal (scale 1.6), on a margin the Rust side holds as f32. Exact,
    so the served rule lands on the same side of every price the grader did."""
    a = np.abs(np.asarray(edge, dtype=float)).astype(np.float32).astype(float)
    return 1.0 / (1.0 + np.exp(-1.6 * a / sigma))


def implied(odds: pd.Series) -> pd.Series:
    o = odds.astype(float)
    return np.where(o > 0, 100.0 / (o + 100.0), -o / (-o + 100.0))


def payout(odds: pd.Series) -> pd.Series:
    o = odds.astype(float)
    return np.where(o > 0, o / 100.0, 100.0 / -o)


def settle(side: pd.Series, result: pd.Series, price: pd.Series) -> pd.Series:
    """Units per game: `side` +1 takes the positive (home / over) side, -1 the
    other, 0 no bet. `result` is the actual value minus the line from the
    positive side; zero is a push."""
    won = np.sign(result) == side
    units = np.where(won, payout(price), -1.0)
    units = np.where(result == 0, 0.0, units)
    return pd.Series(np.where(side == 0, np.nan, units), index=side.index)


# ------------------------------------------------------------------ data
def load(path: str) -> pd.DataFrame:
    df = pd.read_csv(path, parse_dates=["game_date"])
    df["month"] = df["game_date"].dt.month
    df["early"] = df["month"].isin([11, 12])
    df["projected_total"] = pd.to_numeric(df["projected_total"], errors="coerce")
    return df


@dataclass
class Market:
    name: str
    frame: pd.DataFrame  # rows with a line
    edge: pd.Series  # positive favours home / over
    result: pd.Series  # actual minus line, from home / over
    prices: tuple[pd.Series, pd.Series]  # positive side, negative side


def spread_market(df: pd.DataFrame) -> Market:
    d = df[df["home_spread"].notna()].copy()
    return Market(
        "spread",
        d,
        d["projected_margin"] + d["home_spread"],
        d["actual_margin"] + d["home_spread"],
        (d["home_spread_odds"].fillna(STANDARD_ODDS), d["away_spread_odds"].fillna(STANDARD_ODDS)),
    )


def total_market(df: pd.DataFrame) -> Market:
    d = df[df["total_line"].notna() & df["projected_total"].notna()].copy()
    return Market(
        "total",
        d,
        d["projected_total"] - d["total_line"],
        d["actual_total"] - d["total_line"],
        (d["over_odds"].fillna(STANDARD_ODDS), d["under_odds"].fillna(STANDARD_ODDS)),
    )


# ------------------------------------------------------------------ rules
Rule = Callable[[Market], tuple[pd.Series, pd.Series]]


def line_rule(min_edge: float | None, max_edge: float = math.inf, sigma: float | None = None,
              mask: Callable[[Market, pd.Series], pd.Series] | None = None) -> Rule:
    """Take the side the edge favours when |edge| is in [min_edge, max_edge),
    or (with `sigma`) when the chance of covering beats the price."""

    def rule(m: Market):
        side = np.sign(m.edge)
        price = pd.Series(np.where(side > 0, m.prices[0], m.prices[1]), index=m.edge.index)
        a = m.edge.abs()
        if sigma is not None:
            take = (a > 0) & (cover_prob(a, sigma) > implied(price))
        else:
            take = (a >= min_edge) & (a < max_edge)
        if mask is not None:
            take &= mask(m, side)
        return pd.Series(np.where(take, side, 0), index=m.edge.index), price

    return rule


def fav_mask(want_fav: bool):
    # The side taken is the favourite when it is the side the line favours:
    # home favourite has home_spread < 0.
    def f(m: Market, side):
        home_fav = m.frame["home_spread"] < 0
        took_fav = np.where(side > 0, home_fav, ~home_fav)
        return pd.Series(took_fav == want_fav, index=side.index)

    return f


def side_mask(want: int):
    return lambda m, side: pd.Series(side == want, index=side.index)


def when_mask(early: bool):
    return lambda m, side: m.frame["early"] == early


def spread_rules() -> dict[str, Rule]:
    rules: dict[str, Rule] = {"served": line_rule(None, sigma=SPREAD_SIGMA)}
    for t in [0.5, 1, 1.5, 2, 3, 4, 5, 6, 8, 10]:
        rules[f"edge>={t}"] = line_rule(t)
    for lo, hi in [(0.5, 2), (2, 4), (4, 6), (6, 9), (9, 13), (13, math.inf)]:
        rules[f"edge[{lo},{hi})"] = line_rule(lo, hi)
    for t in [0.5, 2, 4]:
        rules[f"edge>={t} fav"] = line_rule(t, mask=fav_mask(True))
        rules[f"edge>={t} dog"] = line_rule(t, mask=fav_mask(False))
        rules[f"edge>={t} home"] = line_rule(t, mask=side_mask(1))
        rules[f"edge>={t} away"] = line_rule(t, mask=side_mask(-1))
        rules[f"edge>={t} nov-dec"] = line_rule(t, mask=when_mask(True))
        rules[f"edge>={t} jan+"] = line_rule(t, mask=when_mask(False))
    return rules


def total_rules() -> dict[str, Rule]:
    rules: dict[str, Rule] = {"served": line_rule(None, sigma=TOTAL_SIGMA)}
    for t in [1, 2, 3, 4, 6, 8, 10, 12]:
        rules[f"edge>={t}"] = line_rule(t)
        if t in (1, 4, 8):
            rules[f"edge>={t} over"] = line_rule(t, mask=side_mask(1))
            rules[f"edge>={t} under"] = line_rule(t, mask=side_mask(-1))
    for lo, hi in [(1, 4), (4, 8), (8, 12), (12, math.inf)]:
        rules[f"edge[{lo},{hi})"] = line_rule(lo, hi)
    return rules


def moneyline_bets(df: pd.DataFrame, min_edge: float, cap: int, only: str | None):
    """Units per game for the moneyline rule: the side whose win probability
    most exceeds its vigged price, by at least `min_edge`, priced no longer
    than `cap`; `only` restricts to favourites or underdogs."""
    d = df[df["home_moneyline"].notna() & df["away_moneyline"].notna()]
    hp, ap = d["home_moneyline"], d["away_moneyline"]
    eh = np.where(hp > cap, -np.inf, d["home_win_prob"] - implied(hp))
    ea = np.where(ap > cap, -np.inf, (1 - d["home_win_prob"]) - implied(ap))
    side = np.where((eh > min_edge) & (eh >= ea), 1, np.where(ea > min_edge, -1, 0))
    # The served rule takes any positive edge; a zero edge is no bet.
    if min_edge == 0:
        side = np.where((eh > 0) & (eh >= ea), 1, np.where(ea > 0, -1, 0))
    price = np.where(side > 0, hp, ap)
    if only is not None:
        fav = price < 0
        side = np.where(fav == (only == "fav"), side, 0)
    units = settle(pd.Series(side, index=d.index), d["actual_margin"], pd.Series(price, index=d.index))
    return d, units


def moneyline_rules():
    rules = {"served": (0.0, LONGEST_MONEYLINE, None)}
    for e in [0.02, 0.04, 0.06, 0.08, 0.10, 0.15]:
        rules[f"edge>={e}"] = (e, LONGEST_MONEYLINE, None)
    for cap in [500, 300, 200, 150, 100]:
        rules[f"cap+{cap}"] = (0.0, cap, None)
        rules[f"edge>=0.04 cap+{cap}"] = (0.04, cap, None)
    for only in ["fav", "dog"]:
        rules[f"{only} only"] = (0.0, LONGEST_MONEYLINE, only)
        rules[f"edge>=0.05 {only}"] = (0.05, LONGEST_MONEYLINE, only)
    return rules


def rust_units(frame: pd.DataFrame, market: str) -> pd.Series:
    """The Rust grader's units per game, NaN where it placed no bet (a pass
    writes 0 units there, as a push does)."""
    bet = frame[f"{market}_pick"].isin(["home", "away", "over", "under"])
    units = pd.to_numeric(frame[f"{market}_units"], errors="coerce")
    return units.where(bet)


def parity(served: pd.Series, rust: pd.Series) -> int:
    both = served.notna() & rust.notna()
    return int(((served[both] - rust[both]).abs() > 1e-3).sum()
               + (served.notna() != rust.notna()).sum())


# ------------------------------------------------------------------ grading
def summarize(units: pd.Series, result: pd.Series | None = None) -> dict:
    u = units.dropna()
    n = len(u)
    if n == 0:
        return {"bets": 0}
    wins = int((u > 0).sum())
    losses = int((u < 0).sum())
    pushes = n - wins - losses
    mean = float(u.mean())
    se = float(u.std(ddof=1) / math.sqrt(n)) if n > 1 else float("nan")
    return {
        "bets": n,
        "wins": wins,
        "losses": losses,
        "pushes": pushes,
        "win_pct": wins / (wins + losses) if wins + losses else None,
        "units": float(u.sum()),
        "roi": mean,
        "z": mean / se if se and se > 0 else None,
    }


def grid(rule_units: dict[str, pd.Series], seasons: pd.Series, choose: set[int], judge: set[int]):
    rows = []
    for name, units in rule_units.items():
        rows.append({
            "rule": name,
            "choose": summarize(units[seasons.isin(choose)]),
            "judge": summarize(units[seasons.isin(judge)]),
            "by_season": {int(s): summarize(units[seasons == s]) for s in sorted(seasons.unique())},
        })
    return rows


def chosen(rows: list[dict]) -> dict:
    eligible = [r for r in rows if r["choose"].get("bets", 0) >= MIN_BETS]
    best = max(eligible, key=lambda r: r["choose"]["units"]) if eligible else None
    served = next(r for r in rows if r["rule"] == "served")
    return {"best_on_choose": best, "served": served}


def fit_sigma(m: Market, mask: pd.Series) -> float | None:
    """The sigma that maximises the likelihood of who covered, from |edge|."""
    e = m.edge[mask]
    r = m.result[mask]
    keep = (e != 0) & (r != 0)
    e, r = e[keep], r[keep]
    if len(e) < 100:
        return None
    covered = (np.sign(r) == np.sign(e)).astype(float).to_numpy()
    a = e.abs().to_numpy()

    def nll(s):
        p = np.clip(cover_prob(a, s), 1e-6, 1 - 1e-6)
        return -np.sum(covered * np.log(p) + (1 - covered) * np.log(1 - p))

    return float(minimize_scalar(nll, bounds=(2, 200), method="bounded").x)


def cover_by_bucket(m: Market, mask: pd.Series, sigma: float, edges: list[float]) -> list[dict]:
    e, r = m.edge[mask], m.result[mask]
    keep = (e != 0) & (r != 0)
    e, r = e[keep], r[keep]
    out = []
    for lo, hi in zip(edges, edges[1:] + [math.inf]):
        b = (e.abs() >= lo) & (e.abs() < hi)
        if b.sum() == 0:
            continue
        out.append({
            "edge": f"[{lo},{hi})",
            "games": int(b.sum()),
            "covered": float((np.sign(r[b]) == np.sign(e[b])).mean()),
            "model": float(cover_prob(e[b], sigma).mean()),
        })
    return out


def line_check(df: pd.DataFrame) -> list[dict]:
    """Per season: how well the spread tracked results against the model, on
    the same games. A line worse than the model is a data problem until shown
    otherwise."""
    d = df[df["home_spread"].notna()]
    out = []
    for s, g in d.groupby("season"):
        out.append({
            "season": int(s),
            "lined": int(len(g)),
            "games": int((df["season"] == s).sum()),
            "line_mae": float((-g["home_spread"] - g["actual_margin"]).abs().mean()),
            "model_mae": float((g["projected_margin"] - g["actual_margin"]).abs().mean()),
            "espn_share": float((g["spread_source"] == "espn").mean()),
            "stored_share": float((g["source"] == "stored").mean()),
        })
    return out


# ------------------------------------------------------------------ report
def fmt(s: dict) -> str:
    if not s.get("bets"):
        return f"{'-':>32}"
    wp = f"{100 * s['win_pct']:.1f}%" if s["win_pct"] is not None else "-"
    z = f"{s['z']:+.1f}" if s["z"] is not None else "-"
    return f"{s['bets']:>6} {wp:>6} {s['units']:>+8.1f}u {100 * s['roi']:>+6.1f}% z{z:>5}"


def print_grid(title: str, rows: list[dict]) -> None:
    print(f"\n== {title}")
    print(f"{'rule':<20} {'choose: bets win% units roi z':>36}   {'judge: bets win% units roi z':>36}")
    for r in rows:
        print(f"{r['rule']:<20} {fmt(r['choose'])}   {fmt(r['judge'])}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--csv", required=True, help="file from `cstat-ingest backtest-picks --out`")
    ap.add_argument("--choose-through", type=int, default=2022,
                    help="last season the rules are chosen on (default 2022)")
    ap.add_argument("--exclude", type=lambda s: {int(x) for x in s.split(",")}, default=set(),
                    help="seasons to leave out, e.g. one whose lines fail the check")
    ap.add_argument("--out", help="summary JSON")
    args = ap.parse_args()

    df = load(args.csv)
    df = df[~df["season"].isin(args.exclude)]
    seasons_all = sorted(int(s) for s in df["season"].unique())
    choose = {s for s in seasons_all if s <= args.choose_through}
    judge = {s for s in seasons_all if s > args.choose_through}
    print(f"seasons: choose {sorted(choose)}, judge {sorted(judge)}")

    checks = line_check(df)
    print("\n== line check (spread vs model MAE on lined games)")
    print(f"{'season':>6} {'lined':>6} {'games':>6} {'line':>6} {'model':>6} {'espn':>5} {'stored':>6}")
    for c in checks:
        flag = "  <- line worse than model" if c["line_mae"] > c["model_mae"] else ""
        print(f"{c['season']:>6} {c['lined']:>6} {c['games']:>6} {c['line_mae']:>6.2f} "
              f"{c['model_mae']:>6.2f} {c['espn_share']:>5.0%} {c['stored_share']:>6.0%}{flag}")

    summary: dict = {"csv": args.csv, "choose": sorted(choose), "judge": sorted(judge),
                     "min_bets": MIN_BETS, "line_check": checks, "markets": {}}

    for build, rules, sigma, buckets in [
        (spread_market, spread_rules(), SPREAD_SIGMA, [0, 1, 2, 3, 4, 6, 8, 10, 13]),
        (total_market, total_rules(), TOTAL_SIGMA, [0, 2, 4, 6, 8, 10, 13, 16]),
    ]:
        m = build(df)
        if m.frame.empty:
            continue
        units = {}
        for name, rule in rules.items():
            side, price = rule(m)
            units[name] = settle(side, m.result, price)
        # Parity: the served rule must reproduce the Rust grader's units.
        mismatch = parity(units["served"], rust_units(m.frame, m.name))
        rows = grid(units, m.frame["season"], choose, judge)
        in_choose = m.frame["season"].isin(choose)
        fitted = fit_sigma(m, in_choose)
        cal = cover_by_bucket(m, m.frame["season"].isin(judge), sigma, buckets)
        summary["markets"][m.name] = {
            "parity_mismatches": mismatch,
            "fitted_sigma_on_choose": fitted,
            "served_sigma": sigma,
            "judge_cover_by_edge": cal,
            **chosen(rows),
            "grid": rows,
        }
        print_grid(f"{m.name} ({len(m.frame)} lined games; served-rule parity mismatches: {mismatch})", rows)
        print(f"   sigma fitted on choose: {fitted if fitted is None else round(fitted, 2)} "
              f"(served {sigma})")
        print("   judge window cover rate by |edge|:")
        for b in cal:
            print(f"     {b['edge']:<10} {b['games']:>6} covered {b['covered']:.3f}  model {b['model']:.3f}")

    ml_units = {}
    ml_frame = None
    for name, (e, cap, only) in moneyline_rules().items():
        ml_frame, ml_units[name] = moneyline_bets(df, e, cap, only)
    if ml_frame is not None and not ml_frame.empty:
        mismatch = parity(ml_units["served"], rust_units(ml_frame, "moneyline"))
        rows = grid(ml_units, ml_frame["season"], choose, judge)
        summary["markets"]["moneyline"] = {"parity_mismatches": mismatch, **chosen(rows), "grid": rows}
        print_grid(f"moneyline ({len(ml_frame)} priced games; served-rule parity mismatches: {mismatch})", rows)

    print("\n== chosen on the choose window, judged on the judge window")
    for name, mk in summary["markets"].items():
        best, served = mk["best_on_choose"], mk["served"]
        if best is None:
            print(f"{name:<10} no rule reached {MIN_BETS} bets on the choose window")
            continue
        print(f"{name:<10} best '{best['rule']}': judge {fmt(best['judge'])}")
        print(f"{'':<10} served rule:      judge {fmt(served['judge'])}")

    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=1, default=float)
        print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
