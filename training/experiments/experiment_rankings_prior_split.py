"""Should the rating prior be split by the team's own offense/defense profile?

The blended ratings (#436, #446) put a prior margin into the opponent
adjustment: the preseason projection for a projected team, 0.9 x last
season's AdjEM for one the projection skips. `team_preseason_projection`
stored only the margin, so `compute_adjusted_efficiency` split every prior
evenly around the league average, `lg + em/2` on offense and `lg - em/2` on
defense, and the morning after opening night the best projected teams held
the best defenses and ranked 25th to 40th on offense (#451).

Migration 065 stores the projection's reconciled AdjO / AdjD halves (the ones
the Future page shows). This replays seasons with the served blend and tilts
each prior by

    tilt = s x ((O - mean_O) + (D - mean_D)) / 2

on both halves, which moves offense and defense together and leaves the prior
margin exactly where it was. A projected team's O / D are its projected
halves; a fallback team's are last season's raw ratings, shrunk by the same
0.9 as its margin. Means are the previous season's (the scale both are on).
`s = 0` is the even split served before this change.

Scored on what a split can change: each side's points per 100 possessions in
its next game, predicted as `O_team + D_opp - lg` from the ratings before the
game and mapped per fold (intercept, slope, home), walk-forward (#361): `s` is
chosen on seasons < S and scored on S. Division I sides only. The margin is
reported too, to confirm the split leaves it alone.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_rankings_prior_split.py
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
from walk_forward import WALK_FROM, folds

SERVED_K = 6.0
FALLBACK_K = 2.0
FALLBACK_SHRINK = 0.9
S_GRID = [0.0, 0.5, 1.0, 1.5]
BUCKETS = [(0, 0), (1, 1), (2, 4), (5, 9), (10, 99)]

PRIOR_QUERY = """
SELECT season, team_id, projected_adj_em::float8 AS em,
       projected_adj_o::float8 AS o, projected_adj_d::float8 AS d
FROM team_preseason_projection
WHERE season BETWEEN %(lo)s AND %(hi)s
"""

# Final raw ratings per (natstat_id, season), for the fallback and the means.
HISTORY_QUERY = """
SELECT t.natstat_id, s.season, s.adj_efficiency_margin::float8 AS em,
       s.adj_offense::float8 AS o, s.adj_defense::float8 AS d
FROM team_season_stats s
JOIN teams t ON t.id = s.team_id
WHERE s.adj_offense IS NOT NULL AND s.adj_defense IS NOT NULL
"""

TEAM_QUERY = """
SELECT id AS team_id, natstat_id, season FROM teams
WHERE season BETWEEN %(lo)s AND %(hi)s
"""


def replay(box, games, prior_em, prior_tilt, is_fallback):
    """Pre-game ratings for every D1 side of every game, per tilt scale."""
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

    home = games.set_index("game_id")
    side = []
    for gid, tid in zip(box["game_id"], box["team_id"]):
        r = home.loc[gid] if gid in home.index else None
        if r is None or bool(r["is_neutral_site"]):
            side.append(0.0)
        else:
            side.append(1.0 if tid == r["home_team_id"] else -1.0)
    side = np.asarray(side)

    # The side's margin in the game: its points minus the opponent's.
    opp_pts = dict(zip(zip(box["game_id"], box["opponent_id"]), pts_all))
    margin_all = np.array([p - opp_pts.get((g, t), np.nan)
                           for g, t, p in zip(box["game_id"], box["team_id"], pts_all)])
    rows = np.where(keep & d1[t_all] & d1[o_all])[0]
    out = {"row": rows, "g_min": np.zeros(len(rows)), "fallback_side": np.zeros(len(rows), bool),
           "home": side[rows], "actual_off": pts_all[rows] / poss_all[rows] * 100.0,
           "actual_margin": margin_all[rows]}
    for s in S_GRID:
        out[f"off_{s:g}"] = np.full(len(rows), np.nan)
        out[f"em_{s:g}"] = np.full(len(rows), np.nan)
    pos = {r: i for i, r in enumerate(rows)}

    for d in np.unique(dates[rows]):
        m = keep & (dates < d)
        team, opp, pts, poss = t_all[m], o_all[m], pts_all[m], poss_all[m]
        gp = np.bincount(team, minlength=n).astype(float)
        w = np.where(has, k / (k + gp), 0.0)
        today = rows[dates[rows] == d]
        ti, oi = t_all[today], o_all[today]
        idx = np.array([pos[r] for r in today])
        out["g_min"][idx] = np.minimum(gp[ti], gp[oi])
        out["fallback_side"][idx] = fb[ti] | fb[oi]
        for s in S_GRID:
            po = lambda lg, s=s: lg + e0 / 2.0 + s * tilt
            pd_ = lambda lg, s=s: lg - e0 / 2.0 + s * tilt
            if len(team):
                io, idf, lg = solve(team, opp, pts, poss, n, po, pd_, w, inside=True)
            else:
                io = idf = np.full(n, np.nan)
                lg = 100.0
            bo = np.where(gp > 0, w * po(lg) + (1 - w) * np.nan_to_num(io),
                          np.where(has, po(lg), lg))
            bd = np.where(gp > 0, w * pd_(lg) + (1 - w) * np.nan_to_num(idf),
                          np.where(has, pd_(lg), lg))
            out[f"off_{s:g}"][idx] = bo[ti] + bd[oi] - lg
            out[f"em_{s:g}"][idx] = (bo[ti] - bd[ti]) - (bo[oi] - bd[oi])
    return pd.DataFrame(out)


def fit(train, col, target="actual_off"):
    x = np.column_stack([np.ones(len(train)), train[col].to_numpy(), train["home"].to_numpy()])
    beta, *_ = np.linalg.lstsq(x, train[target].to_numpy(), rcond=None)
    return beta


def pred(df, col, beta):
    return beta[0] + beta[1] * df[col].to_numpy() + beta[2] * df["home"].to_numpy()


def margin_mae(df, s):
    """Walk-forward next-game margin MAE at tilt scale `s`, map refit per fold."""
    col = f"em_{s:g}"
    errs = []
    for _, tr, te in folds(df["season"], WALK_FROM):
        train = df[tr & df["actual_margin"].notna()]
        test = df[te & df["actual_margin"].notna()]
        b = fit(train, col, "actual_margin")
        errs.append(np.abs(test["actual_margin"].to_numpy() - pred(test, col, b)))
    return float(np.mean(np.concatenate(errs)))


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
        # The served fallback for a team with box rows and no projection.
        played = set(b["team_id"])
        for tid, code in zip(teams.loc[teams["season"] == s, "team_id"],
                             teams.loc[teams["season"] == s, "natstat_id"]):
            if tid in em.index or tid not in played:
                continue
            key = (code, s - 1)
            if key in hist_by.index:
                h = hist_by.loc[key]
                h = h.iloc[0] if isinstance(h, pd.DataFrame) else h
                em[tid] = FALLBACK_SHRINK * h["em"]
                tilt[tid] = FALLBACK_SHRINK * ((h["o"] - mo) + (h["d"] - md)) / 2.0
            else:
                em[tid] = 0.0
                tilt[tid] = 0.0
            fallback[tid] = True
        sc = replay(b, g, em, tilt, fallback)
        sc["season"] = s
        frames.append(sc)
        print(f"{s}: {len(sc)} D1 sides, {int(sc['fallback_side'].sum())} involving a fallback team")
    df = pd.concat(frames, ignore_index=True)

    y = df["actual_off"].to_numpy()
    cols = [f"off_{s:g}" for s in S_GRID]
    held = {c: np.full(len(df), np.nan) for c in cols + ["chosen"]}
    picks = {}
    for s, tr, te in folds(df["season"], WALK_FROM):
        train, test = df[tr], df[te]
        best = None
        for c in cols:
            b = fit(train, c)
            held[c][te.to_numpy()] = pred(test, c, b)
            mae = float(np.mean(np.abs(train["actual_off"].to_numpy() - pred(train, c, b))))
            if best is None or mae < best[0]:
                best = (mae, c)
        picks[int(s)] = best[1]
        held["chosen"][te.to_numpy()] = held[best[1]][te.to_numpy()]

    wf = (df["season"] >= WALK_FROM).to_numpy()
    base = "off_0"

    def block(mask):
        e0 = y[mask] - held[base][mask]
        r = {"n": int(mask.sum()), "even_mae": float(np.mean(np.abs(e0)))}
        for c in cols[1:] + ["chosen"]:
            e = y[mask] - held[c][mask]
            r[f"{c}_mae"] = float(np.mean(np.abs(e)))
            r[f"{c}_z"] = paired_z(e, e0)
        return r

    report = {"picks": picks, "all": block(wf),
              "fallback_sides": block(wf & df["fallback_side"].to_numpy()),
              "buckets": {}, "per_season": {}}
    for lo, hi in BUCKETS:
        report["buckets"][f"{lo}-{hi}"] = block(wf & df["g_min"].between(lo, hi).to_numpy())
    for s in sorted(df["season"].unique()):
        m = wf & (df["season"] == s).to_numpy()
        if m.any():
            report["per_season"][int(s)] = block(m)
    # The tilt reaches the margin only through the opponent adjustment.
    report["margin_mae"] = {f"em_{s:g}": margin_mae(df, s) for s in S_GRID}
    # Largest per-game change in the rating margin against the even split.
    report["max_margin_shift"] = {
        f"em_{s:g}": float(np.nanmax(np.abs(df[f"em_{s:g}"] - df["em_0"]))) for s in S_GRID[1:]
    }

    print(json.dumps({k: report[k] for k in ("picks", "all", "fallback_sides", "buckets",
                                              "margin_mae", "max_margin_shift")}, indent=1))
    out = Path(args.out) if args.out else (
        EVAL_DIR / f"rankings_prior_split_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(report, indent=1))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
