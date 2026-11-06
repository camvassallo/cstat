"""What prior should a team WITHOUT a preseason projection get in the blend?

The blended ratings (`compute::PRESEASON_PRIOR_GAMES`, #436) put the
preseason projection in as a prior worth K = 6 games. A team the projection
skips (`compute-projections` drops a roster with fewer than seven projectable
players as too thin: 14-69 teams a season since 2016, 71 in 2027) gets no
prior at all, so its blended rating is its raw one, and on the morning after
opening night that is one game at full weight. New Mexico topped the 2027
board on one result this way (#446).

`experiment_rankings_preseason_prior.py`, which fitted K, scored only games
where both sides had a projection, so this case was never measured. This
replays the same seasons with the served blend for projected teams (K = 6,
inside the opponent adjustment) and, for unprojected ones, each of:

    none         the served behaviour: no prior
    zero_k       league average (0 AdjEM), weight k / (k + games)
    prev_r_k     r x last season's final raw AdjEM (cross-season natstat_id)
    lvl_r_k      r x the mean of the three seasons before (>= 2 present)

A team with no history under prev / lvl falls back to zero_k's prior.
Scored on the next game, walk-forward (#361): the margin map (slope, hca) and
the fallback (family, r, k) are both chosen on seasons < S and scored on S.
Reported on games between two Division I teams where at least one side is
unprojected, overall and by the games played of the less-experienced side,
with paired z against `none`; and on the D1 games where both sides are
projected, which a fallback moves only through the opponent adjustment.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_rankings_fallback_prior.py
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
# `--taper`: the projection's K under the taper, and where the taper ends.
TAPERED_K = 12.0
FULL_SEASON_GAMES = 32.0
R_GRID = [0.3, 0.5, 0.7, 0.9, 1.0]
K_GRID = [1.0, 2.0, 4.0, 6.0, 8.0, 12.0, 16.0]
BUCKETS = [(0, 0), (1, 1), (2, 4), (5, 9), (10, 99)]

# Final raw AdjEM per (natstat_id, season), for the history-based fallbacks.
HISTORY_QUERY = """
SELECT t.natstat_id, s.season, s.adj_efficiency_margin::float8 AS em
FROM team_season_stats s
JOIN teams t ON t.id = s.team_id
WHERE s.adj_efficiency_margin IS NOT NULL AND t.natstat_id IS NOT NULL
"""

TEAM_QUERY = """
SELECT id AS team_id, natstat_id, season FROM teams
WHERE season BETWEEN %(lo)s AND %(hi)s
"""


def configs():
    yield ("none", None, None)
    for k in K_GRID:
        yield ("zero", 0.0, k)
        for r in R_GRID:
            yield ("prev", r, k)
            yield ("lvl", r, k)


def name(c):
    fam, r, k = c
    if fam == "none":
        return "none"
    if fam == "zero":
        return f"zero_{k:g}"
    return f"{fam}_{r:g}_{k:g}"


def prior_weight(k, gp, taper):
    """`k / (k + games)`, or with `taper` that weight rescaled to reach zero at
    a full season (`compute::tapered_prior_weight`)."""
    w = k / (k + gp + 1e-12)
    if not taper:
        return w
    end = k / (k + FULL_SEASON_GAMES)
    return np.maximum(0.0, (w - end) / (1.0 - end))


def replay(box, games, proj, prev_em, lvl_em, d1_only=False, taper=False):
    """Per-config home-minus-away blended rating for every game of a season."""
    ids = pd.Index(pd.unique(pd.concat([box["team_id"], box["opponent_id"],
                                         games["home_team_id"], games["away_team_id"]])))
    n = len(ids)
    t_all = ids.get_indexer(box["team_id"])
    o_all = ids.get_indexer(box["opponent_id"])
    pts_all = box["points"].to_numpy()
    poss_all = box["poss"].to_numpy()
    keep = poss_all > 0
    box_dates = box["game_date"].to_numpy()

    pe = proj.reindex(ids).to_numpy(dtype=float)
    projected = ~np.isnan(pe)
    d1 = np.bincount(t_all[keep], minlength=n) > 0
    # With `d1_only`, a team with no box rows of its own (non-D1) gets no
    # fallback prior and keeps the served behaviour.
    eligible = d1 if d1_only else np.ones(n, dtype=bool)
    prev = prev_em.reindex(ids).to_numpy(dtype=float)
    lvl = lvl_em.reindex(ids).to_numpy(dtype=float)

    # Each config's prior EM and fallback k, per team.
    cfgs = list(configs())
    prior_em = {}
    fb_k = {}
    for c in cfgs:
        fam, r, k = c
        em = np.where(projected, pe, np.nan)
        if fam == "zero":
            em = np.where(projected, pe, np.where(eligible, 0.0, np.nan))
        elif fam == "prev":
            em = np.where(projected, pe, np.where(
                eligible, np.where(np.isnan(prev), 0.0, r * prev), np.nan))
        elif fam == "lvl":
            em = np.where(projected, pe, np.where(
                eligible, np.where(np.isnan(lvl), 0.0, r * lvl), np.nan))
        prior_em[c] = em
        fb_k[c] = k

    g_home = ids.get_indexer(games["home_team_id"])
    g_away = ids.get_indexer(games["away_team_id"])
    g_dates = games["game_date"].to_numpy()
    out = {"g_min": np.zeros(len(games))}
    for c in cfgs:
        out[name(c)] = np.full(len(games), np.nan)

    for d in np.unique(g_dates):
        m = keep & (box_dates < d)
        team, opp, pts, poss = t_all[m], o_all[m], pts_all[m], poss_all[m]
        gp = np.bincount(team, minlength=n).astype(float)
        rows = np.where(g_dates == d)[0]
        hi, ai = g_home[rows], g_away[rows]
        out["g_min"][rows] = np.minimum(gp[hi], gp[ai])
        for c in cfgs:
            em = prior_em[c]
            has = ~np.isnan(em)
            k_fb = fb_k[c]
            k_proj = TAPERED_K if taper else SERVED_K
            w = np.where(projected, prior_weight(k_proj, gp, taper),
                         np.where(has, prior_weight(k_fb or 0.0, gp, taper), 0.0))
            w = np.where(has, w, 0.0)
            e0 = np.nan_to_num(em)
            if len(team):
                io, idf, _ = solve(team, opp, pts, poss, n,
                                   lambda lg: lg + e0 / 2.0, lambda lg: lg - e0 / 2.0, w, inside=True)
                iem = io - idf
                it = np.where(gp > 0, w * e0 + (1 - w) * np.nan_to_num(iem), np.where(has, e0, 0.0))
            else:
                it = np.where(has, e0, 0.0)
            out[name(c)][rows] = it[hi] - it[ai]

    scored = games.reset_index(drop=True).copy()
    for col, v in out.items():
        scored[col] = v
    scored["home"] = (~scored["is_neutral_site"]).astype(float)
    # Division I = a team with box rows of its own this season. A non-D1
    # opponent has none, sits at zero games all season, and has no projection
    # or history, so it would flood the comparison with blowouts no prior can
    # inform. Both sides D1, at least one unprojected.
    both_d1 = d1[g_home] & d1[g_away]
    scored["d1"] = both_d1
    scored["affected"] = both_d1 & ~(projected[g_home] & projected[g_away])
    return scored, [name(c) for c in cfgs]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--lo", type=int, default=2016)
    ap.add_argument("--hi", type=int, default=2026)
    ap.add_argument("--out", default=None)
    ap.add_argument("--taper", action="store_true",
                    help="taper both priors to zero at a full season, projection at K = 12")
    ap.add_argument("--d1-only", action="store_true",
                    help="give the fallback prior only to teams with box rows of their own")
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
    hist_by = hist.set_index(["natstat_id", "season"])["em"]

    frames, cols = [], None
    for s in range(args.lo, args.hi + 1):
        b, g = box[box["season"] == s], games[games["season"] == s]
        p = priors[priors["season"] == s].set_index("team_id")["prior_em"]
        if b.empty or g.empty or p.empty:
            print(f"{s}: skipped")
            continue
        tm = teams[teams["season"] == s]
        prev = {}
        lvl = {}
        for tid, code in zip(tm["team_id"], tm["natstat_id"]):
            v = hist_by.get((code, s - 1))
            if v is not None:
                prev[tid] = float(v)
            past = [hist_by.get((code, s - j)) for j in (1, 2, 3)]
            past = [float(x) for x in past if x is not None]
            if len(past) >= 2:
                lvl[tid] = float(np.mean(past))
        sc, cols = replay(b, g, p, pd.Series(prev, dtype=float), pd.Series(lvl, dtype=float),
                          d1_only=args.d1_only, taper=args.taper)
        frames.append(sc)
        print(f"{s}: {len(sc)} games, {int(sc['affected'].sum())} with an unprojected side")
    df = pd.concat(frames, ignore_index=True)

    y = df["actual"].to_numpy()
    held = {"none": np.full(len(df), np.nan), "chosen": np.full(len(df), np.nan)}
    picks = {}
    for s, tr, te in folds(df["season"], WALK_FROM):
        train, test = df[tr & df["affected"]], df[te]
        beta = fit_map(train, "none")
        held["none"][te.to_numpy()] = predict(test, "none", beta)
        best = None
        for c in cols:
            b = fit_map(train, c)
            mae = float(np.mean(np.abs(train["actual"].to_numpy() - predict(train, c, b))))
            if best is None or mae < best[0]:
                best = (mae, c, b)
        _, c, b = best
        picks[int(s)] = c
        held["chosen"][te.to_numpy()] = predict(test, c, b)

    # Per-config pooled walk-forward MAE on affected games, map refit per fold.
    curve = {}
    for c in cols:
        errs = []
        for s, tr, te in folds(df["season"], WALK_FROM):
            b = fit_map(df[tr & df["affected"]], c)
            t = df[te & df["affected"]]
            errs.append(np.abs(t["actual"].to_numpy() - predict(t, c, b)))
        curve[c] = float(np.mean(np.concatenate(errs)))

    wf = (df["season"] >= WALK_FROM).to_numpy()
    aff = df["affected"].to_numpy()

    def block(mask):
        e_none = y[mask] - held["none"][mask]
        e_ch = y[mask] - held["chosen"][mask]
        return {"n": int(mask.sum()), "none_mae": float(np.mean(np.abs(e_none))),
                "chosen_mae": float(np.mean(np.abs(e_ch))), "z": paired_z(e_ch, e_none)}

    report = {"picks": picks, "affected": block(wf & aff), "unaffected": block(wf & ~aff & df["d1"].to_numpy()),
              "buckets": {}, "curve_affected": dict(sorted(curve.items(), key=lambda kv: kv[1])[:15])}
    for lo, hi in BUCKETS:
        m = wf & aff & df["g_min"].between(lo, hi).to_numpy()
        report["buckets"][f"{lo}-{hi}"] = block(m)
    per_season = {}
    for s in sorted(df["season"].unique()):
        m = wf & aff & (df["season"] == s).to_numpy()
        if m.any():
            per_season[int(s)] = block(m)
    report["per_season_affected"] = per_season

    print(json.dumps({k: report[k] for k in ("picks", "affected", "unaffected", "buckets")}, indent=1))
    print("best configs (affected, pooled WF MAE):")
    for c, v in list(report["curve_affected"].items())[:10]:
        print(f"  {c:<14} {v:.3f}   (none {curve['none']:.3f})")
    out = Path(args.out) if args.out else (
        EVAL_DIR / f"rankings_fallback_prior{'_taper' if args.taper else ''}"
        f"{'_d1only' if args.d1_only else ''}"
        f"_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(report, indent=1))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
