"""Should the blended ratings adjust for home court?

`compute_adjusted_efficiency` treats a team's points per 100 the same home or
away, so a home-heavy schedule flatters a team and an away-heavy one hurts it,
most of all in November, when good teams host cupcakes. The raw solve feeds the
models and is not up for changing, so this tests the blended solve only.

Replays seasons with the served prior (K = 6, the #446 fallback, the #451
tilt), removing `h` points per 100 possessions of home edge from every
non-neutral game before the solve: `h / 2` off the home side's offense, `h / 2`
onto the away side's. `h = 0` is the served behaviour.

Scored on next-game margin in Division I games, walk-forward (#361): the margin
map (intercept, slope, home) is refit per fold and per `h`, so the comparison
is the rating difference with the game's own venue handled identically on both
sides, and `h` is chosen on seasons < S and scored on S.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_rankings_home_court.py
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import datetime as dt
import json
from pathlib import Path

import numpy as np
import pandas as pd

from compute_cae import EVAL_DIR
from db import get_engine
from experiment_rankings_preseason_prior import BOX_QUERY, GAME_QUERY, paired_z, solve
from experiment_rankings_prior_split import (
    FALLBACK_K,
    FALLBACK_SHRINK,
    HISTORY_QUERY,
    PRIOR_QUERY,
    SERVED_K,
    TEAM_QUERY,
)
from walk_forward import WALK_FROM, folds

H_GRID = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]
BUCKETS = [(0, 1), (2, 4), (5, 9), (10, 99)]


def replay(box, games, prior_em, prior_tilt, is_fallback):
    """Home-minus-away pre-game blended margin for every D1 game, per `h`."""
    ids = pd.Index(pd.unique(pd.concat([box["team_id"], box["opponent_id"]])))
    n = len(ids)
    t_all = ids.get_indexer(box["team_id"])
    o_all = ids.get_indexer(box["opponent_id"])
    pts_all = box["points"].to_numpy()
    poss_all = box["poss"].to_numpy()
    keep = poss_all > 0
    dates = box["game_date"].to_numpy()
    d1 = np.bincount(t_all[keep], minlength=n) > 0

    em = prior_em.reindex(ids).to_numpy(dtype=float)
    tilt = np.nan_to_num(prior_tilt.reindex(ids).to_numpy(dtype=float))
    fb = is_fallback.reindex(ids).fillna(False).to_numpy(dtype=bool)
    has = ~np.isnan(em)
    e0 = np.nan_to_num(em)
    k = np.where(fb, FALLBACK_K, SERVED_K)

    # +1 for the home side's offense row, -1 for the away side's, 0 neutral.
    g = games.set_index("game_id")
    joined = box[["game_id", "team_id"]].join(
        g[["home_team_id", "is_neutral_site"]], on="game_id")
    side = np.where(joined["team_id"] == joined["home_team_id"], 1.0, -1.0)
    side = np.where(joined["is_neutral_site"].fillna(True).astype(bool), 0.0, side)

    sc = games[games["home_team_id"].isin(ids) & games["away_team_id"].isin(ids)].copy()
    hi = ids.get_indexer(sc["home_team_id"])
    ai = ids.get_indexer(sc["away_team_id"])
    both = d1[hi] & d1[ai]
    sc, hi, ai = sc[both].reset_index(drop=True), hi[both], ai[both]
    g_dates = sc["game_date"].to_numpy()
    out = {f"em_{h:g}": np.full(len(sc), np.nan) for h in H_GRID}
    out["g_min"] = np.zeros(len(sc))

    for d in np.unique(g_dates):
        m = keep & (dates < d)
        team, opp, poss = t_all[m], o_all[m], poss_all[m]
        gp = np.bincount(team, minlength=n).astype(float)
        w = np.where(has, k / (k + gp), 0.0)
        rows = np.where(g_dates == d)[0]
        out["g_min"][rows] = np.minimum(gp[hi[rows]], gp[ai[rows]])
        for h in H_GRID:
            pts = pts_all[m] - side[m] * (h / 2.0) * poss / 100.0
            po = lambda lg: lg + e0 / 2.0 + tilt
            pd_ = lambda lg: lg - e0 / 2.0 + tilt
            if len(team):
                io, idf, lg = solve(team, opp, pts, poss, n, po, pd_, w, inside=True)
                iem = np.nan_to_num(io - idf)
            else:
                iem, lg = np.zeros(n), 100.0
            rating = np.where(gp > 0, w * e0 + (1 - w) * iem, np.where(has, e0, 0.0))
            out[f"em_{h:g}"][rows] = rating[hi[rows]] - rating[ai[rows]]

    for col, v in out.items():
        sc[col] = v
    sc["home"] = (~sc["is_neutral_site"]).astype(float)
    return sc


def fit(train, col):
    x = np.column_stack([train[col].to_numpy(), train["home"].to_numpy()])
    beta, *_ = np.linalg.lstsq(x, train["actual"].to_numpy(), rcond=None)
    return beta


def pred(df, col, beta):
    return beta[0] * df[col].to_numpy() + beta[1] * df["home"].to_numpy()


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
    hist = pd.read_sql(HISTORY_QUERY, eng)
    teams = pd.read_sql(TEAM_QUERY, eng, params=params)
    for df in (box, games):
        df["game_date"] = pd.to_datetime(df["game_date"]).to_numpy()
    means = hist.groupby("season")[["o", "d"]].mean()
    hist_by = hist.set_index(["natstat_id", "season"])

    frames = []
    for s in range(args.lo, args.hi + 1):
        b, g = box[box["season"] == s], games[games["season"] == s]
        p = priors[priors["season"] == s].set_index("team_id")
        if b.empty or p.empty or (s - 1) not in means.index or p["o"].isna().all():
            print(f"{s}: skipped")
            continue
        mo, md = means.loc[s - 1, "o"], means.loc[s - 1, "d"]
        em = p["em"].copy()
        tilt = ((p["o"] - mo) + (p["d"] - md)) / 2.0
        fallback = pd.Series(False, index=em.index)
        played = set(b["team_id"])
        tm = teams[teams["season"] == s]
        for tid, code in zip(tm["team_id"], tm["natstat_id"]):
            if tid in em.index or tid not in played:
                continue
            if (code, s - 1) in hist_by.index:
                h = hist_by.loc[(code, s - 1)]
                h = h.iloc[0] if isinstance(h, pd.DataFrame) else h
                em[tid] = FALLBACK_SHRINK * h["em"]
                tilt[tid] = FALLBACK_SHRINK * ((h["o"] - mo) + (h["d"] - md)) / 2.0
            else:
                em[tid], tilt[tid] = 0.0, 0.0
            fallback[tid] = True
        sc = replay(b, g, em, tilt, fallback)
        frames.append(sc)
        print(f"{s}: {len(sc)} D1 games")
    df = pd.concat(frames, ignore_index=True)

    y = df["actual"].to_numpy()
    cols = [f"em_{h:g}" for h in H_GRID]
    held = {c: np.full(len(df), np.nan) for c in cols + ["chosen"]}
    picks = {}
    for s, tr, te in folds(df["season"], WALK_FROM):
        train, test = df[tr], df[te]
        best = None
        for c in cols:
            b = fit(train, c)
            held[c][te.to_numpy()] = pred(test, c, b)
            mae = float(np.mean(np.abs(train["actual"].to_numpy() - pred(train, c, b))))
            if best is None or mae < best[0]:
                best = (mae, c)
        picks[int(s)] = best[1]
        held["chosen"][te.to_numpy()] = held[best[1]][te.to_numpy()]

    wf = (df["season"] >= WALK_FROM).to_numpy()

    def block(mask):
        e0 = y[mask] - held["em_0"][mask]
        r = {"n": int(mask.sum()), "served_mae": float(np.mean(np.abs(e0)))}
        for c in cols[1:] + ["chosen"]:
            e = y[mask] - held[c][mask]
            r[f"{c}_mae"] = float(np.mean(np.abs(e)))
            r[f"{c}_z"] = paired_z(e, e0)
        return r

    report = {"picks": picks, "all": block(wf), "buckets": {}, "per_season": {}}
    for lo, hi in BUCKETS:
        report["buckets"][f"{lo}-{hi}"] = block(wf & df["g_min"].between(lo, hi).to_numpy())
    for s in sorted(df["season"].unique()):
        m = wf & (df["season"] == s).to_numpy()
        if m.any():
            report["per_season"][int(s)] = block(m)

    print(json.dumps({k: report[k] for k in ("picks", "all", "buckets")}, indent=1))
    out = Path(args.out) if args.out else (
        EVAL_DIR / f"rankings_home_court_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(report, indent=1))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
