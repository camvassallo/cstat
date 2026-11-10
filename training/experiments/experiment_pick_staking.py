"""Should the pick strategy's bets be sized by Kelly instead of flat (#447)?

`experiment_pick_strategy.py` chooses which bets to make, walk-forward, at a
flat one unit each. This keeps that selection exactly and asks only how much
to stake on each, by simulating a bankroll over the same walk-forward seasons.

**Win probability, walk-forward.** Kelly needs a probability per bet, and the
served cover curve is far too confident (spread sigma 10.46 where the fitted
one is about 27). Every probability here is fitted on the seasons before the
one it sizes:

- **spread / total**: a no-intercept logistic on the side the edge favours,
  `P(cover) = sigmoid(b * |edge| + c * early * |edge|)`, edge clipped at
  `EDGE_CLIP` (a 13+ point edge is more often a bad line than a beaten one).
  `1.6 / b` is the equivalent sigma on the served curve, so it can be read
  against 10.46 / 17.2; `early` is November-December. Fitted on every game
  with a line in the training seasons, not only the selected bets, so the
  calibration is not conditioned on the threshold the selection chose. Totals
  use the strategy's own edge (bias-corrected when the season's parameters
  correct it).
- **moneyline**: `logit P(home) = a + w_m * logit(model) + w_k * logit(no-vig
  market)`, so the fit decides how far to shrink the model toward the market.

Calibration is reported on the test seasons' selected bets: predicted against
realised by bucket, and the expected ROI the probabilities imply against the
ROI realised.

**Staking.** Bets on one date are simultaneous: every stake is a fraction of
the bankroll at the start of that day. Schemes: flat one unit of a 100-unit
bankroll (fixed, not compounding, the strategy file's number), flat 1% of the
current bankroll, Kelly at 1, 1/2, 1/4 and 1/8, and full Kelly capped per bet.
A bet whose calibrated Kelly fraction is not positive gets no stake under any
Kelly scheme (flat schemes still take it). A spread and a moneyline on the
same side of the same game are close to the same bet, so their combined
stake is cut to the larger of the two. A day whose stakes sum past
`DAILY_CAP` of the bankroll is scaled down to it.

**Sensitivity.** On the real outcomes, overstating every edge by a factor k
is the same as multiplying the Kelly fraction by k (the fraction is linear in
`p - breakeven`), so the multiplier ladder is the sensitivity to overstated
edges. A Monte Carlo then replays the bet sequence with outcomes drawn from
(a) the calibrated probabilities, (b) edges half the calibrated size, and (c)
no edge at all (the breakeven of the price), which is the case the leak
caveat (#274, #466) makes plausible.

Run (CSV from `cstat-ingest backtest-picks --out`):
    cd training && ./.venv/bin/python experiments/experiment_pick_staking.py \\
        --csv /tmp/picks.csv --out eval_history/pick_staking_YYYYMMDD_summary.json

Reads the file only; writes nothing to the database or the models.
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import json
import math

import numpy as np
import pandas as pd
from scipy.optimize import minimize

from experiment_pick_selection import STANDARD_ODDS, implied, load, payout
from experiment_pick_strategy import (
    ML_GRID,
    SPREAD_GRID,
    TOTAL_GRID,
    ml_units,
    running_bias,
    spread_served,
    spread_units,
    total_served,
    total_units,
    walk_forward,
)

EDGE_CLIP = 12.0
DAILY_CAP = 0.30
MC_SIMS = 400

# Recorded with the summary; re-read it against the printed tables after a rerun.
VERDICT = (
    "Size by Kelly weight, not flat, but at 1/8 Kelly with a 1% per-bet cap and a 10% daily "
    "exposure cap. Summed full-Kelly stakes are about 3x the bankroll on a median day, so the "
    "daily cap, not the Kelly multiplier, sets the risk; at equal exposure Kelly weighting beats "
    "flat (stake-weighted ROI +7.5% vs +4.2%). The calibrated edges are overstated (totals about "
    "2x, spread about 1.35x), same-day outcomes are more correlated than independent bets "
    "(variance ratio 1.15-1.4), and with no true edge every scheme loses, so full and half Kelly "
    "are rejected."
)
SEED = 447

# name -> (kind, Kelly multiplier or flat fraction, per-bet cap, daily cap)
SCHEMES = {
    "flat_1u": ("flat_fixed", 0.01, None, None),
    "flat_1pct": ("flat_pct", 0.01, None, DAILY_CAP),
    "kelly_1": ("kelly", 1.0, None, DAILY_CAP),
    "kelly_1/2": ("kelly", 0.5, None, DAILY_CAP),
    "kelly_1/4": ("kelly", 0.25, None, DAILY_CAP),
    "kelly_1/8": ("kelly", 0.125, None, DAILY_CAP),
    "kelly_1_cap3": ("kelly", 1.0, 0.03, DAILY_CAP),
    "kelly_1_cap2": ("kelly", 1.0, 0.02, DAILY_CAP),
    "kelly_1/4_cap1": ("kelly", 0.25, 0.01, DAILY_CAP),
    "kelly_1/8_cap1": ("kelly", 0.125, 0.01, DAILY_CAP),
    "kelly_1/8_cap1_day10": ("kelly", 0.125, 0.01, 0.10),
}


# ------------------------------------------------------------------ fits
def _logit(p):
    p = np.clip(np.asarray(p, dtype=float), 1e-4, 1 - 1e-4)
    return np.log(p / (1 - p))


def fit_logistic(X: np.ndarray, y: np.ndarray, intercept: bool) -> np.ndarray:
    if intercept:
        X = np.column_stack([np.ones(len(X)), X])

    def nll(w):
        z = X @ w
        return float(np.sum(np.logaddexp(0, z) - y * z)), X.T @ (1 / (1 + np.exp(-z)) - y)

    w = minimize(nll, np.zeros(X.shape[1]), jac=True, method="L-BFGS-B").x
    return w


def edge_features(aedge: np.ndarray, early: np.ndarray) -> np.ndarray:
    a = np.minimum(aedge, EDGE_CLIP)
    return np.column_stack([a, a * early])


def fit_cover(edge: pd.Series, result: pd.Series, early: pd.Series) -> np.ndarray:
    keep = (edge != 0) & (result != 0) & edge.notna() & result.notna()
    y = (np.sign(result[keep]) == np.sign(edge[keep])).astype(float).to_numpy()
    return fit_logistic(edge_features(edge[keep].abs().to_numpy(), early[keep].to_numpy(float)), y, False)


def cover_p(w: np.ndarray, aedge, early) -> np.ndarray:
    z = edge_features(np.asarray(aedge, float), np.asarray(early, float)) @ w
    return 1 / (1 + np.exp(-z))


def novig_home(d: pd.DataFrame) -> np.ndarray:
    ih, ia = implied(d["home_moneyline"]), implied(d["away_moneyline"])
    return ih / (ih + ia)


def ml_X(d: pd.DataFrame) -> np.ndarray:
    return np.column_stack([_logit(d["home_win_prob"]), _logit(novig_home(d))])


def fit_ml(d: pd.DataFrame) -> np.ndarray:
    keep = d["actual_margin"] != 0
    return fit_logistic(ml_X(d[keep]), (d.loc[keep, "actual_margin"] > 0).astype(float).to_numpy(), True)


def ml_p(w: np.ndarray, d: pd.DataFrame) -> np.ndarray:
    z = w[0] + ml_X(d) @ w[1:]
    return 1 / (1 + np.exp(-z))


# ------------------------------------------------------------------ bets
def build_bets(df: pd.DataFrame, tests: list[int]):
    """Every bet the strategy makes on a test season, with its calibrated
    probability, side, price and settled units per unit staked."""
    sp = df[df["home_spread"].notna()]
    to = df[df["total_line"].notna() & df["projected_total"].notna() & df["bias"].notna()]
    ml = df[df["home_moneyline"].notna() & df["away_moneyline"].notna()]
    wf = {
        "spread": walk_forward(sp, SPREAD_GRID, spread_units, spread_served, tests),
        "total": walk_forward(to, TOTAL_GRID, total_units, total_served, tests),
        "moneyline": walk_forward(ml, ML_GRID, ml_units, lambda d: ml_units(d, 0.0, 999), tests),
    }
    fits, out = {m: {} for m in wf}, []
    for m, rows in wf.items():
        for r in rows:
            s, u = r["season"], r["_units"].dropna()
            if u.empty:
                continue
            b = df.loc[u.index]
            if m == "spread":
                train = sp[sp["season"] < s]
                tedge = train["projected_margin"] + train["home_spread"]
                w = fit_cover(tedge, train["actual_margin"] + train["home_spread"], train["early"])
                edge = b["projected_margin"] + b["home_spread"]
                side = np.sign(edge)
                p = cover_p(w, edge.abs(), b["early"])
                price = np.where(side > 0, b["home_spread_odds"].fillna(STANDARD_ODDS),
                                 b["away_spread_odds"].fillna(STANDARD_ODDS))
                team = side
                fits[m][s] = {"sigma_late": 1.6 / w[0], "sigma_early": 1.6 / (w[0] + w[1])}
            elif m == "total":
                debias = bool(r["params"][0])
                train = to[to["season"] < s]
                tedge = train["projected_total"] - (train["bias"] if debias else 0.0) - train["total_line"]
                w = fit_cover(tedge, train["actual_total"] - train["total_line"], train["early"])
                edge = b["projected_total"] - (b["bias"] if debias else 0.0) - b["total_line"]
                side = np.sign(edge)
                p = cover_p(w, edge.abs(), b["early"])
                price = np.where(side > 0, b["over_odds"].fillna(STANDARD_ODDS),
                                 b["under_odds"].fillna(STANDARD_ODDS))
                team = np.zeros(len(b))
                fits[m][s] = {"sigma_late": 1.6 / w[0], "sigma_early": 1.6 / (w[0] + w[1]),
                              "debias": debias}
            else:
                w = fit_ml(ml[ml["season"] < s])
                min_edge, cap = r["params"]
                hp, ap = b["home_moneyline"], b["away_moneyline"]
                eh = np.where(hp > cap, -np.inf, b["home_win_prob"] - implied(hp))
                ea = np.where(ap > cap, -np.inf, (1 - b["home_win_prob"]) - implied(ap))
                side = np.where((eh > min_edge) & (eh >= ea), 1, np.where(ea > min_edge, -1, 0))
                ph = ml_p(w, b)
                p = np.where(side > 0, ph, 1 - ph)
                price = np.where(side > 0, hp, ap)
                team = side
                fits[m][s] = {"intercept": w[0], "w_model": w[1], "w_market": w[2]}
            bb = payout(pd.Series(price, index=b.index))
            out.append(pd.DataFrame({
                "idx": b.index, "season": s, "game_id": b["game_id"].to_numpy(),
                "game_date": b["game_date"].to_numpy(), "market": m, "team": team,
                "p": p, "b": bb, "units": u.to_numpy(), "early": b["early"].to_numpy(),
            }))
    bets = pd.concat(out, ignore_index=True)
    bets["breakeven"] = 1 / (1 + bets["b"])
    bets["kelly"] = bets["p"] - (1 - bets["p"]) / bets["b"]
    return bets.sort_values(["game_date", "game_id", "market"]).reset_index(drop=True), fits


# ------------------------------------------------------------------ calibration
def calibration(bets: pd.DataFrame) -> dict:
    out = {}
    print("\n== calibration on test seasons (selected bets, pushes excluded)")
    for m, g in bets.groupby("market"):
        g = g[g["units"] != 0]
        won = (g["units"] > 0).astype(float)
        edges = [-1, 0.0, 0.01, 0.02, 0.04, 0.08, 1]
        # Bucket on the estimated edge over breakeven, where the stake comes from.
        e = g["p"] - g["breakeven"]
        rows = []
        for lo, hi in zip(edges[:-1], edges[1:]):
            k = (e > lo) & (e <= hi)
            if k.sum() < 30:
                continue
            rows.append({"edge_lo": lo, "edge_hi": hi, "n": int(k.sum()),
                         "pred": float(g.loc[k, "p"].mean()), "real": float(won[k].mean()),
                         "breakeven": float(g.loc[k, "breakeven"].mean())})
        exp_roi = float((g["p"] * g["b"] - (1 - g["p"])).mean())
        real_roi = float(g["units"].mean())
        brier = float(((g["p"] - won) ** 2).mean())
        out[m] = {"buckets": rows, "pred_win": float(g["p"].mean()), "real_win": float(won.mean()),
                  "expected_roi": exp_roi, "realised_roi": real_roi, "brier": brier,
                  "kelly_nonpositive_share": float((g["kelly"] <= 0).mean())}
        print(f"  {m}: n={len(g)} predicted win {g['p'].mean():.4f} realised {won.mean():.4f}  "
              f"expected ROI {100 * exp_roi:+.2f}% realised {100 * real_roi:+.2f}%  "
              f"Kelly<=0 on {100 * out[m]['kelly_nonpositive_share']:.0f}%")
        for r in rows:
            print(f"     est. edge ({r['edge_lo']:+.2f},{r['edge_hi']:+.2f}]  n={r['n']:>6}  "
                  f"pred {r['pred']:.3f}  real {r['real']:.3f}  breakeven {r['breakeven']:.3f}")
    return out


# ------------------------------------------------------------------ staking
def stake_fractions(bets: pd.DataFrame, kind: str, mult: float, cap: float | None,
                    daily_cap: float | None = DAILY_CAP) -> np.ndarray:
    """Fraction of the start-of-day bankroll staked on each bet."""
    if kind == "flat_fixed":
        return np.full(len(bets), mult)
    if kind == "flat_pct":
        f = np.full(len(bets), mult)
    else:
        f = np.clip(bets["kelly"].to_numpy() * mult, 0, None)
        if cap is not None:
            f = np.minimum(f, cap)
        # Spread + moneyline on the same team in the same game: one bet's worth.
        same = bets[bets["market"].isin(["spread", "moneyline"]) & (bets["team"] != 0)]
        for _, g in same.groupby(["game_id", "team"]):
            if len(g) > 1:
                ix = g.index.to_numpy()
                tot, top = f[ix].sum(), f[ix].max()
                if tot > 0:
                    f[ix] *= top / tot
    if daily_cap is None:
        return f
    day = bets["game_date"].to_numpy()
    tot = pd.Series(f).groupby(day).transform("sum").to_numpy()
    scale = np.where(tot > daily_cap, daily_cap / np.maximum(tot, 1e-12), 1.0)
    return f * scale


def day_starts(bets: pd.DataFrame) -> np.ndarray:
    d = bets["game_date"].to_numpy()
    return np.flatnonzero(np.r_[True, d[1:] != d[:-1]])


def path_stats(daily_ret: np.ndarray, fixed: bool) -> dict:
    """Bankroll path from per-day returns (fixed: additive on the starting
    bankroll; otherwise compounding)."""
    path = 1 + np.cumsum(daily_ret) if fixed else np.cumprod(1 + daily_ret)
    path = np.r_[1.0, path]
    peak = np.maximum.accumulate(path)
    final = float(path[-1])
    return {"final": final, "log_growth": math.log(final) if final > 0 else -math.inf,
            "max_drawdown": float(np.max(1 - path / peak)), "min_bankroll": float(path.min())}


def simulate(bets: pd.DataFrame, label: str) -> dict:
    starts = day_starts(bets)
    seasons = bets["season"].to_numpy()
    units = bets["units"].to_numpy()
    # Variance of one unit staked, for the correlation check below.
    var_u = bets["p"].to_numpy() * (1 - bets["p"].to_numpy()) * (1 + bets["b"].to_numpy()) ** 2
    mean_u = bets["p"].to_numpy() * (1 + bets["b"].to_numpy()) - 1
    out = {}
    print(f"\n== bankroll, {label} (start 1.0; flat_1u = 1 unit of a 100-unit bank, not compounding)")
    print("   ROI = sum(fraction x units) / sum(fraction): stake-weighted, path-free.")
    print("   season log = mean per-season log multiple (bankroll reset each season).")
    print(f"{'scheme':<21} {'season log':>10} {'worst season':>13} {'ROI':>6} {'exposure/day':>12} "
          f"{'maxDD':>6} {'min':>5} {'var ratio':>9}")
    for name, (kind, mult, cap, daily) in SCHEMES.items():
        f = stake_fractions(bets, kind, mult, cap, daily)
        ret = np.add.reduceat(f * units, starts)
        fixed = kind == "flat_fixed"
        st = path_stats(ret, fixed)
        dseason = seasons[starts]
        per = {}
        for s in np.unique(dseason):
            r = ret[dseason == s]
            per[int(s)] = float(1 + r.sum()) if fixed else float(np.prod(1 + r))
        logs = [math.log(v) if v > 0 else -math.inf for v in per.values()]
        # Realised day-to-day variance over what independent bets at the
        # calibrated probabilities would give: above 1 means same-day outcomes
        # move together (or the probabilities are off), which Kelly ignores.
        resid = ret - np.add.reduceat(f * mean_u, starts)
        vr = float((resid ** 2).sum() / np.add.reduceat(f ** 2 * var_u, starts).sum())
        st.update({"roi": float((f * units).sum() / f.sum()), "bets_staked": int((f > 0).sum()),
                   "mean_daily_exposure": float(np.add.reduceat(f, starts).mean()),
                   "seasons": per, "mean_season_log": float(np.mean(logs)),
                   "worst_season": min(per.values()), "variance_ratio": vr})
        out[name] = st
        ws = min(per, key=per.get)
        print(f"{name:<21} {st['mean_season_log']:>+10.2f} {f'{ws}:{per[ws]:.2f}':>13} "
              f"{100 * st['roi']:>+5.1f}% {st['mean_daily_exposure']:>12.3f} "
              f"{100 * st['max_drawdown']:>5.1f}% {st['min_bankroll']:>5.2f} {vr:>9.2f}")
    print("  per-season multiples:")
    for name in out:
        print(f"   {name:<21} " + " ".join(f"{s}:{v:.2f}" for s, v in out[name]["seasons"].items()))
    return out


def season_paths(r: np.ndarray, dseason: np.ndarray, fixed: bool):
    """Per-season bankroll paths from day returns (sims x days), each season
    starting from 1.0. Yields (final, min, max drawdown) arrays per season."""
    for s in np.unique(dseason):
        x = r[:, dseason == s]
        path = 1 + np.cumsum(x, axis=1) if fixed else np.cumprod(1 + x, axis=1)
        path = np.hstack([np.ones((len(path), 1)), path])
        dd = np.max(1 - path / np.maximum.accumulate(path, axis=1), axis=1)
        yield path[:, -1], path.min(axis=1), dd


def monte_carlo(bets: pd.DataFrame, rng: np.random.Generator) -> dict:
    """Replay the bet sequence with drawn outcomes (pushes dropped)."""
    starts = day_starts(bets)
    dseason = bets["season"].to_numpy()[starts]
    be, p, b = bets["breakeven"].to_numpy(), bets["p"].to_numpy(), bets["b"].to_numpy()
    worlds = {"calibrated": p, "half_edge": be + 0.5 * (p - be), "no_edge": be}
    fr = {n: stake_fractions(bets, *SCHEMES[n]) for n in SCHEMES}
    out = {w: {} for w in worlds}
    print(f"\n== Monte Carlo over the 2019-2026 bet sequence ({MC_SIMS} draws per world, outcomes independent)")
    print("   per season, bankroll reset to 1.0; quantiles over draws x seasons")
    print(f"{'world':<11} {'scheme':<21} {'median':>7} {'p05':>6} {'p95':>7} {'P(<1)':>6} "
          f"{'P(min<.5)':>9} {'medDD':>6}")
    for wname, pt in worlds.items():
        rets = {n: [] for n in SCHEMES}
        for _ in range(MC_SIMS // 50):
            win = rng.random((50, len(p))) < pt
            u = np.where(win, b, -1.0)
            for n, f in fr.items():
                rets[n].append(np.add.reduceat(u * f, starts, axis=1))
        for n in SCHEMES:
            r = np.vstack(rets[n])
            parts = list(season_paths(r, dseason, SCHEMES[n][0] == "flat_fixed"))
            final = np.concatenate([x[0] for x in parts])
            low = np.concatenate([x[1] for x in parts])
            dd = np.concatenate([x[2] for x in parts])
            st = {"median": float(np.median(final)), "p05": float(np.quantile(final, 0.05)),
                  "p95": float(np.quantile(final, 0.95)), "p_loss": float((final < 1).mean()),
                  "p_min_below_half": float((low < 0.5).mean()),
                  "median_max_drawdown": float(np.median(dd))}
            out[wname][n] = st
            print(f"{wname:<11} {n:<21} {st['median']:>7.2f} {st['p05']:>6.2f} {st['p95']:>7.2f} "
                  f"{st['p_loss']:>6.2f} {st['p_min_below_half']:>9.2f} {100 * st['median_max_drawdown']:>5.1f}%")
    return out


def daily_cap_sensitivity(bets: pd.DataFrame) -> dict:
    starts = day_starts(bets)
    dseason = bets["season"].to_numpy()[starts]
    out = {}
    print("\n== daily exposure cap, real outcomes (mean season log multiple / worst in-season drawdown)")
    for n in ["kelly_1/8_cap1", "kelly_1/8", "kelly_1/4", "kelly_1"]:
        row = {}
        for c in [0.05, 0.10, 0.20, 0.30, 0.50, 1.0]:
            f = stake_fractions(bets, *SCHEMES[n][:3], daily_cap=c)
            r = np.add.reduceat(f * bets["units"].to_numpy(), starts)[None, :]
            parts = list(season_paths(r, dseason, False))
            row[str(c)] = {"mean_season_log": float(np.mean([math.log(x[0][0]) for x in parts])),
                           "worst_drawdown": float(max(x[2][0] for x in parts))}
        out[n] = row
        print(f"   {n:<15} " + "  ".join(f"{c}: {v['mean_season_log']:+.2f}/{100 * v['worst_drawdown']:.0f}%"
                                         for c, v in row.items()))
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--csv", required=True, help="file from `cstat-ingest backtest-picks --out`")
    ap.add_argument("--first-test", type=int, default=2019)
    ap.add_argument("--live-season", type=int, default=2027,
                    help="the in-progress season, reported on its own")
    ap.add_argument("--out", help="summary JSON")
    args = ap.parse_args()

    df = load(args.csv)
    df["bias"] = running_bias(df)
    tests = sorted(int(s) for s in df["season"].unique() if s >= args.first_test)
    bets, fits = build_bets(df, tests)

    print("== walk-forward calibration fits (spread/total: equivalent sigma on the served curve)")
    for m, by in fits.items():
        print(f"  {m}: " + "  ".join(
            f"{s}:" + "/".join(f"{v:.2f}" if isinstance(v, float) else str(v) for v in d.values())
            for s, d in by.items()))

    hist = bets[bets["season"] < args.live_season].reset_index(drop=True)
    live = bets[bets["season"] == args.live_season].reset_index(drop=True)
    same = hist[hist["market"].isin(["spread", "moneyline"]) & (hist["team"] != 0)]
    n_same = int((same.groupby(["game_id", "team"]).size() > 1).sum())
    per_day = hist.groupby("game_date").size()
    print(f"\n{len(hist)} bets on {per_day.size} days (median {per_day.median():.0f}/day, max {per_day.max()}); "
          f"{n_same} games with a spread and moneyline bet on the same team")
    k = hist["kelly"].clip(lower=0)
    print(f"full-Kelly fraction per staked bet: median {k[k > 0].median():.4f}, p90 {k[k > 0].quantile(.9):.4f}; "
          f"summed per day: median {k.groupby(hist['game_date']).sum().median():.3f}, "
          f"max {k.groupby(hist['game_date']).sum().max():.3f}")

    out = {"csv": args.csv, "edge_clip": EDGE_CLIP, "daily_cap": DAILY_CAP, "mc_sims": MC_SIMS,
           "fits": {m: {int(s): v for s, v in by.items()} for m, by in fits.items()},
           "bets": len(hist), "days": int(per_day.size), "same_game_spread_ml": n_same}
    out["calibration"] = calibration(hist)
    out["bankroll"] = simulate(hist, f"{args.first_test}-{args.live_season - 1}")
    if not live.empty:
        out["bankroll_live"] = simulate(live, f"{args.live_season} so far")
        out["calibration_live"] = calibration(live)
    out["daily_cap_sensitivity"] = daily_cap_sensitivity(hist)
    out["monte_carlo"] = monte_carlo(hist, np.random.default_rng(SEED))

    out["verdict"] = VERDICT
    if args.out:
        with open(args.out, "w") as f:
            json.dump(out, f, indent=1, default=float)
        print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
