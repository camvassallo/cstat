"""Should the team ratings' preseason prior taper to zero by a full season?

`experiment_rankings_preseason_prior.py` fit `w = K / (K + games)` with K = 6
and concluded the prior is "not faded", but it never compared a fade: it showed
that a blend beats no blend past twenty games. CAM tapers its prior to zero at
32 games (`experiment_cam_preseason_prior.py`), so a finished season is the
stat. This asks the same question of the team rating.

Same replay, same next-game target, same walk-forward as that experiment; the
prior goes inside the opponent adjustment, as served.

    raw        no prior
    served     w = 6 / (6 + games), the shipped blend
    taper6     that weight rescaled to reach zero at 32 games
    taper_K    the same with K refit per fold

    w(g) = max(0, (K/(K+g) - K/(K+32)) / (1 - K/(K+32)))

Each method's slope and home-court term are refit on the training seasons only,
and K is chosen by training-season MAE. Walk-forward (#361): train on seasons
< S, score S, S = 2021..2026. The in-sample-at-Layer-2 caveat on the historical
projections applies here too, and biases toward the prior, so against a taper.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_rankings_prior_taper.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_here = _os.path.dirname(_os.path.abspath(__file__))
_sys.path.insert(0, _os.path.dirname(_here))
_sys.path.insert(0, _here)

import argparse
import datetime as dt
import json
from pathlib import Path

import numpy as np
import pandas as pd

from compute_cae import EVAL_DIR
from db import get_engine
from experiment_rankings_preseason_prior import (
    BOX_QUERY,
    GAME_QUERY,
    PRIOR_QUERY,
    fit_map,
    paired_z,
    predict,
    solve,
)
from walk_forward import WALK_FROM, folds

SERVED_K = 6.0
FULL_SEASON_GAMES = 32.0
TAPER_K_GRID = [4, 6, 8, 10, 12, 16, 20, 30, 40]
BUCKETS = [(0, 1), (2, 4), (5, 9), (10, 14), (15, 19), (20, 24), (25, 31), (32, 99)]


def untapered(k: float, gp: np.ndarray) -> np.ndarray:
    return k / (k + gp)


def tapered(k: float, gp: np.ndarray) -> np.ndarray:
    end = k / (k + FULL_SEASON_GAMES)
    return np.maximum(0.0, (k / (k + gp) - end) / (1.0 - end))


def replay_season(box: pd.DataFrame, games: pd.DataFrame, prior: pd.Series) -> pd.DataFrame:
    ids = pd.Index(
        pd.unique(
            pd.concat(
                [box["team_id"], box["opponent_id"], games["home_team_id"], games["away_team_id"]]
            )
        )
    )
    n = len(ids)
    t_all, o_all = ids.get_indexer(box["team_id"]), ids.get_indexer(box["opponent_id"])
    pts_all, poss_all = box["points"].to_numpy(), box["poss"].to_numpy()
    keep = poss_all > 0
    box_dates = box["game_date"].to_numpy()

    prior_em = prior.reindex(ids).to_numpy(dtype=float)
    has_prior = ~np.isnan(prior_em)
    pe = np.nan_to_num(prior_em)

    def prior_o(league):
        return np.where(has_prior, league + pe / 2.0, 0.0)

    def prior_d(league):
        return np.where(has_prior, league - pe / 2.0, 0.0)

    g_home, g_away = ids.get_indexer(games["home_team_id"]), ids.get_indexer(games["away_team_id"])
    g_dates = games["game_date"].to_numpy()
    variants = {"served": lambda gp: untapered(SERVED_K, gp)}
    for k in TAPER_K_GRID:
        variants[f"taper_{k}"] = lambda gp, k=k: tapered(k, gp)
    out = {"g_min": np.zeros(len(games)), "em_raw": np.full(len(games), np.nan)}
    out["em_preseason"] = np.full(len(games), np.nan)
    for name in variants:
        out[f"em_{name}"] = np.full(len(games), np.nan)

    for d in np.unique(g_dates):
        m = keep & (box_dates < d)
        team, opp, pts, poss = t_all[m], o_all[m], pts_all[m], poss_all[m]
        gp = np.bincount(team, minlength=n).astype(float)
        rows = np.where(g_dates == d)[0]
        hi, ai = g_home[rows], g_away[rows]
        out["g_min"][rows] = np.minimum(gp[hi], gp[ai])
        out["em_preseason"][rows] = prior_em[hi] - prior_em[ai]
        if len(team):
            raw_o, raw_d, _ = solve(team, opp, pts, poss, n)
            raw_em = raw_o - raw_d
        else:
            raw_em = np.full(n, np.nan)
        out["em_raw"][rows] = np.nan_to_num(raw_em[hi]) - np.nan_to_num(raw_em[ai])
        for name, weight in variants.items():
            w = np.where(has_prior, weight(gp), 0.0)
            if len(team):
                io, idf, _ = solve(team, opp, pts, poss, n, prior_o, prior_d, w, inside=True)
                rating = np.where(gp > 0, w * pe + (1 - w) * np.nan_to_num(io - idf), prior_em)
            else:
                rating = prior_em.copy()
            out[f"em_{name}"][rows] = rating[hi] - rating[ai]

    scored = games.reset_index(drop=True).copy()
    for col, v in out.items():
        scored[col] = v
    scored["home"] = (~scored["is_neutral_site"]).astype(float)
    return scored[~np.isnan(scored["em_preseason"])]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--lo", type=int, default=2016)
    ap.add_argument("--hi", type=int, default=2026)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    eng = get_engine()
    params = {"lo": args.lo, "hi": args.hi}
    box = pd.read_sql(BOX_QUERY, eng, params=params)
    games = pd.read_sql(GAME_QUERY, eng, params=params)
    priors = pd.read_sql(PRIOR_QUERY, eng, params=params)
    for frame in (box, games):
        frame["game_date"] = pd.to_datetime(frame["game_date"]).to_numpy()

    parts = []
    for s in range(args.lo, args.hi + 1):
        b, g = box[box["season"] == s], games[games["season"] == s]
        p = priors[priors["season"] == s].set_index("team_id")["prior_em"]
        if b.empty or g.empty or p.empty:
            print(f"{s}: skipped")
            continue
        parts.append(replay_season(b, g, p))
        print(f"{s}: {len(parts[-1])} scored games", flush=True)
    df = pd.concat(parts, ignore_index=True)

    fixed = ["raw", "served", "taper_6"]
    held = {m: np.full(len(df), np.nan) for m in fixed + ["taper_fit"]}
    chosen = {}
    for s, tr, te in folds(df["season"], WALK_FROM):
        train, test = df[tr], df[te]
        for m in fixed:
            held[m][te.to_numpy()] = predict(test, f"em_{m}", fit_map(train, f"em_{m}"))
        best = None
        for k in TAPER_K_GRID:
            col = f"em_taper_{k}"
            beta = fit_map(train, col)
            mae = float(np.abs(train["actual"].to_numpy() - predict(train, col, beta)).mean())
            if best is None or mae < best[0]:
                best = (mae, k, beta)
        chosen[s] = best[1]
        held["taper_fit"][te.to_numpy()] = predict(test, f"em_taper_{best[1]}", best[2])

    wf = (df["season"] >= WALK_FROM).to_numpy()
    y = df["actual"].to_numpy()
    report = {"walk_from": WALK_FROM, "taper_k_chosen": chosen, "buckets": {}}

    def block(mask: np.ndarray) -> dict:
        base = y[mask] - held["served"][mask]
        res = {"n": int(mask.sum())}
        for m, p in held.items():
            e = y[mask] - p[mask]
            res[m] = {
                "mae": float(np.abs(e).mean()),
                "z_vs_served": None if m == "served" else paired_z(e, base),
            }
        return res

    def show(label: str, r: dict) -> None:
        cells = "  ".join(
            f"{m} {r[m]['mae']:.3f}"
            + ("" if r[m]["z_vs_served"] is None else f" ({r[m]['z_vs_served']:+.1f})")
            for m in held
        )
        print(f"{label:>7} n={r['n']:>6}  {cells}")

    print("taper K chosen:", chosen)
    print("MAE on the next game (paired z vs served; negative favors the method)")
    report["pooled"] = block(wf)
    show("all", report["pooled"])
    for lo, hi in BUCKETS:
        mask = wf & df["g_min"].between(lo, hi).to_numpy()
        if mask.any():
            report["buckets"][f"{lo}-{hi}"] = block(mask)
            show(f"{lo}-{hi}", report["buckets"][f"{lo}-{hi}"])

    # Fold-by-fold, late season: is any difference one season or all of them?
    report["late_by_season"] = {}
    for s in sorted(int(x) for x in df.loc[wf, "season"].unique()):
        mask = wf & (df["season"] == s).to_numpy() & (df["g_min"] >= 20).to_numpy()
        r = block(mask)
        report["late_by_season"][str(s)] = r
        show(str(s), r)

    # March: the games a late-season rating is mostly read for.
    month = pd.to_datetime(df["game_date"]).dt.month.to_numpy()
    report["march_on"] = block(wf & (month >= 3) & (month <= 4))
    show("March+", report["march_on"])

    out = Path(args.out) if args.out else (
        EVAL_DIR / f"rankings_prior_taper_{dt.date.today():%Y%m%d}_summary.json"
    )
    out.write_text(json.dumps(report, indent=1) + "\n")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
