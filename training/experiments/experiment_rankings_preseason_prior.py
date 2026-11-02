"""How much should the preseason projection weigh in the early-season ratings?

The rankings table serves raw AdjEM from `compute_adjusted_efficiency`, which
has no prior. The night after a season's opening slate every team has one game
and the schedule graph is ~180 disconnected pairs, so the opponent adjustment
has nothing to adjust against and the table ranks single-game margins. The
`opening` gate (docs/season_rollover_pages.md) hides it behind the projection
board until the median team has five games; this measures what should replace
it rather than the gate.

Every candidate replays the season date by date: the ratings for a game on day
D are built from games strictly before D, with the same iteration as the Rust
compute (multiplicative opponent adjustment, possession-weighted, Jacobi
updates, 50 iterations / 0.01 tolerance) ported to numpy. The preseason prior
is `team_preseason_projection.projected_adj_em`, split symmetrically around
the running league average into an O and a D half.

    raw          the served number: no prior
    preseason    the projection alone (the opening board)
    post_K       w*prior + (1-w)*raw per team, w = K / (K + games played)
    iter_K       the same weight, but applied INSIDE the iteration: each
                 opponent adjustment uses the opponent's blended rating rather
                 than its raw one, so an early schedule is judged against
                 what is known about the opponent, not against one game

Scored on the next game. For each game the rating difference is mapped to a
margin by `slope * emdiff + hca * home`, (slope, hca) refit per method on the
training seasons only. K is chosen per fold by training-season MAE over a grid.
Walk-forward (#361): train on seasons < S, score S, S = 2021..2026.

Also reported, per games-played bucket of the game's less-experienced side:
MAE and paired z against `raw`; and Spearman of each method's ratings against
the season's final raw AdjEM at fixed days into the season (what a reader of
the rankings table would see).

Honesty caveat: the historical `team_preseason_projection` rows are in-sample
at Layer 2 (#274). `experiment_preseason_margin_calibration.py` found this
immaterial for the margin slope (0.600 ex-ante vs 0.595); it would bias K
upward slightly here (too much trust in the prior), so a fitted K is if
anything a ceiling.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_rankings_preseason_prior.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import argparse
import datetime as dt
import json
import math
from pathlib import Path

import numpy as np
import pandas as pd
from scipy.stats import spearmanr

from compute_cae import EVAL_DIR
from db import get_engine
from walk_forward import WALK_FROM, folds

K_GRID = [0.5, 1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 25, 30, 40]
MAX_ITER = 50
TOL = 0.01
# Games-played buckets for the less-experienced side of each scored game.
BUCKETS = [(0, 0), (1, 1), (2, 4), (5, 9), (10, 14), (15, 19), (20, 99)]
# Days after a season's first game at which ranking quality is read.
CHECKPOINT_DAYS = [3, 7, 14, 21, 28, 42, 60]

# Box rows, one per team per game, possessions as in `compute.rs`.
BOX_QUERY = """
SELECT tgs.season, g.game_date, tgs.game_id, tgs.team_id, tgs.opponent_id,
       COALESCE(tgs.points, 0)::float8 AS points,
       (COALESCE(tgs.fga, 0) - COALESCE(tgs.off_rebounds, 0)
        + COALESCE(tgs.turnovers, 0) + 0.44 * COALESCE(tgs.fta, 0))::float8 AS poss
FROM team_game_stats tgs
JOIN games g ON g.id = tgs.game_id
WHERE tgs.points IS NOT NULL AND tgs.fga IS NOT NULL
  AND tgs.opponent_id IS NOT NULL
  AND tgs.season BETWEEN %(lo)s AND %(hi)s
ORDER BY tgs.season, g.game_date, tgs.game_id, tgs.team_id
"""

GAME_QUERY = """
SELECT g.season, g.game_date, g.id AS game_id, g.home_team_id, g.away_team_id,
       COALESCE(g.is_neutral_site, false) AS is_neutral_site,
       (g.home_score - g.away_score)::float8 AS actual
FROM games g
WHERE g.home_score IS NOT NULL AND g.away_score IS NOT NULL
  AND g.season BETWEEN %(lo)s AND %(hi)s
ORDER BY g.season, g.game_date, g.id
"""

PRIOR_QUERY = """
SELECT season, team_id, projected_adj_em::float8 AS prior_em
FROM team_preseason_projection
WHERE season BETWEEN %(lo)s AND %(hi)s
"""


# ------------------------------------------------------------- the iteration
def solve(team, opp, pts, poss, n, prior_o=None, prior_d=None, w=None, inside=False):
    """Port of `compute_adjusted_efficiency` over index arrays.

    `team`/`opp`/`pts`/`poss` are one row per team-game (offense = `team`).
    With `inside`, opponent adjustments read the blended rating
    `w*prior + (1-w)*adj`; the returned ratings are always the post-iteration
    adjusted ones, unblended, so the caller blends once at the end. Teams with
    no rows come back NaN.
    """
    off_pts = np.bincount(team, weights=pts, minlength=n)
    off_pos = np.bincount(team, weights=poss, minlength=n)
    def_pts = np.bincount(opp, weights=pts, minlength=n)
    def_pos = np.bincount(opp, weights=poss, minlength=n)
    tot = off_pos.sum()
    league = off_pts.sum() / tot * 100.0 if tot > 0 else 100.0
    with np.errstate(invalid="ignore", divide="ignore"):
        raw_off = np.where(off_pos > 0, off_pts / off_pos * 100.0, league)
        raw_def = np.where(def_pos > 0, def_pts / def_pos * 100.0, league)
    adj_off, adj_def = raw_off.copy(), raw_def.copy()
    blend = inside and w is not None
    for _ in range(MAX_ITER):
        if blend:
            use_off = w * prior_o(league) + (1 - w) * adj_off
            use_def = w * prior_d(league) + (1 - w) * adj_def
        else:
            use_off, use_def = adj_off, adj_def
        s_def = np.bincount(team, weights=use_def[opp] * poss, minlength=n)
        s_off = np.bincount(opp, weights=use_off[team] * poss, minlength=n)
        with np.errstate(invalid="ignore", divide="ignore"):
            opp_def = np.where(off_pos > 0, s_def / off_pos, league)
            opp_off = np.where(def_pos > 0, s_off / def_pos, league)
            new_off = np.where(off_pos > 0, raw_off * (league / opp_def), adj_off)
            new_def = np.where(def_pos > 0, raw_def * (league / opp_off), adj_def)
        change = max(np.nanmax(np.abs(new_off - adj_off)), np.nanmax(np.abs(new_def - adj_def)))
        adj_off, adj_def = new_off, new_def
        if change < TOL:
            break
    played = off_pos > 0
    adj_off[~played] = np.nan
    adj_def[~played] = np.nan
    return adj_off, adj_def, league


# ------------------------------------------------------------- replay
def replay_season(box: pd.DataFrame, games: pd.DataFrame, prior: pd.Series):
    """Rating differences for every scored game in one season, per method.

    Returns a frame of scored games with columns `g_min` (games played by the
    less-experienced side before tip), `home`, `actual`, and one `em_<method>`
    column per method (home minus away), plus a per-checkpoint ratings dict.
    """
    ids = pd.Index(pd.unique(pd.concat([box["team_id"], box["opponent_id"],
                                         games["home_team_id"], games["away_team_id"]])))
    n = len(ids)
    t_all = ids.get_indexer(box["team_id"])
    o_all = ids.get_indexer(box["opponent_id"])
    pts_all = box["points"].to_numpy()
    poss_all = box["poss"].to_numpy()
    keep = poss_all > 0
    box_dates = box["game_date"].to_numpy()

    prior_em = prior.reindex(ids).to_numpy(dtype=float)
    has_prior = ~np.isnan(prior_em)
    pe = np.nan_to_num(prior_em)

    def prior_o(league):
        return np.where(has_prior, league + pe / 2.0, 0.0)

    def prior_d(league):
        return np.where(has_prior, league - pe / 2.0, 0.0)

    g_home = ids.get_indexer(games["home_team_id"])
    g_away = ids.get_indexer(games["away_team_id"])
    g_dates = games["game_date"].to_numpy()
    out = {"g_min": np.zeros(len(games)), "em_raw": np.full(len(games), np.nan),
           "em_preseason": np.full(len(games), np.nan)}
    for k in K_GRID:
        out[f"em_post_{k}"] = np.full(len(games), np.nan)
        out[f"em_iter_{k}"] = np.full(len(games), np.nan)

    first_day = g_dates.min()
    checkpoints = {}
    final = None

    dates = np.unique(g_dates)
    # The first game date on or after each checkpoint day (not every day has
    # games, so an exact-day match would silently drop checkpoints).
    cp_at = {}
    for c in CHECKPOINT_DAYS:
        later = dates[(dates - first_day) >= np.timedelta64(c, "D")]
        if len(later):
            cp_at.setdefault(later[0], c)
    for d in list(dates) + [None]:
        if d is None:
            m = keep
        else:
            m = keep & (box_dates < d)
        team, opp, pts, poss = t_all[m], o_all[m], pts_all[m], poss_all[m]
        gp = np.bincount(team, minlength=n).astype(float)
        if len(team):
            raw_o, raw_d, league = solve(team, opp, pts, poss, n)
        else:
            raw_o = np.full(n, np.nan)
            raw_d = np.full(n, np.nan)
            league = 100.0
        raw_em = raw_o - raw_d
        if d is None:
            final = raw_em
            break
        rows = np.where(g_dates == d)[0]
        hi, ai = g_home[rows], g_away[rows]
        out["g_min"][rows] = np.minimum(gp[hi], gp[ai])
        out["em_raw"][rows] = np.nan_to_num(raw_em[hi]) - np.nan_to_num(raw_em[ai])
        out["em_preseason"][rows] = prior_em[hi] - prior_em[ai]

        cp = cp_at.get(d)
        cp_store = {"raw": raw_em.copy(), "preseason": prior_em.copy()} if cp else None

        for k in K_GRID:
            w = np.where(has_prior, k / (k + gp), 0.0)
            # A team with no games has nothing but its prior.
            post = np.where(gp > 0, w * pe + (1 - w) * np.nan_to_num(raw_em), prior_em)
            out[f"em_post_{k}"][rows] = post[hi] - post[ai]
            if len(team):
                io, idf, lg = solve(team, opp, pts, poss, n, prior_o, prior_d, w, inside=True)
                iem = io - idf
                it = np.where(gp > 0, w * pe + (1 - w) * np.nan_to_num(iem), prior_em)
            else:
                it = prior_em.copy()
            out[f"em_iter_{k}"][rows] = it[hi] - it[ai]
            if cp:
                cp_store[f"post_{k}"] = post.copy()
                cp_store[f"iter_{k}"] = it.copy()
        if cp:
            checkpoints[cp] = cp_store

    scored = games.reset_index(drop=True).copy()
    for col, v in out.items():
        scored[col] = v
    scored["home"] = (~scored["is_neutral_site"]).astype(float)
    # A game needs both sides' prior to be comparable across methods.
    scored = scored[~np.isnan(scored["em_preseason"])]
    ranking = {}
    d1 = has_prior & ~np.isnan(final)
    for cp, store in checkpoints.items():
        ranking[cp] = {m: float(spearmanr(v[d1], final[d1], nan_policy="omit").statistic)
                       for m, v in store.items()}
    return scored, ranking


# ------------------------------------------------------------- scoring
def fit_map(train: pd.DataFrame, col: str):
    x = np.column_stack([train[col].to_numpy(), train["home"].to_numpy()])
    beta, *_ = np.linalg.lstsq(x, train["actual"].to_numpy(), rcond=None)
    return beta


def predict(df: pd.DataFrame, col: str, beta) -> np.ndarray:
    return beta[0] * df[col].to_numpy() + beta[1] * df["home"].to_numpy()


def paired_z(err_a: np.ndarray, err_b: np.ndarray) -> float:
    d = np.abs(err_a) - np.abs(err_b)
    if len(d) < 2 or d.std(ddof=1) == 0:
        return float("nan")
    return float(d.mean() / (d.std(ddof=1) / math.sqrt(len(d))))


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
    for df in (box, games):
        df["game_date"] = pd.to_datetime(df["game_date"]).to_numpy()

    frames, rankings = [], {}
    for s in range(args.lo, args.hi + 1):
        b, g = box[box["season"] == s], games[games["season"] == s]
        p = priors[priors["season"] == s].set_index("team_id")["prior_em"]
        if b.empty or g.empty or p.empty:
            print(f"{s}: skipped (box={len(b)} games={len(g)} priors={len(p)})")
            continue
        sc, rk = replay_season(b, g, p)
        frames.append(sc)
        rankings[s] = rk
        print(f"{s}: {len(sc)} scored games")
    df = pd.concat(frames, ignore_index=True)

    methods = ["raw", "preseason"] + [f"post_{k}" for k in K_GRID] + [f"iter_{k}" for k in K_GRID]
    held = {m: np.full(len(df), np.nan) for m in ["raw", "preseason", "post", "iter"]}
    chosen = {"post": {}, "iter": {}}
    for s, tr, te in folds(df["season"], WALK_FROM):
        train, test = df[tr], df[te]
        for m in ["raw", "preseason"]:
            beta = fit_map(train, f"em_{m}")
            held[m][te.to_numpy()] = predict(test, f"em_{m}", beta)
        for fam in ["post", "iter"]:
            best = None
            for k in K_GRID:
                col = f"em_{fam}_{k}"
                beta = fit_map(train, col)
                mae = float(np.mean(np.abs(train["actual"].to_numpy() - predict(train, col, beta))))
                if best is None or mae < best[0]:
                    best = (mae, k, beta)
            _, k, beta = best
            chosen[fam][s] = k
            held[fam][te.to_numpy()] = predict(test, f"em_{fam}_{k}", beta)

    wf = df["season"] >= WALK_FROM
    y = df["actual"].to_numpy()
    report = {"seasons": [int(s) for s in sorted(df["season"].unique())],
              "walk_from": WALK_FROM, "k_chosen": chosen, "buckets": {}}

    def block(mask):
        res = {"n": int(mask.sum())}
        err_raw = y[mask] - held["raw"][mask]
        for m, p in held.items():
            e = y[mask] - p[mask]
            res[m] = {"mae": float(np.mean(np.abs(e))), "bias": float(np.mean(e)),
                      "z_vs_raw": paired_z(e, err_raw) if m != "raw" else None}
        return res

    report["pooled"] = block(wf.to_numpy())
    for lo, hi in BUCKETS:
        m = (wf & df["g_min"].between(lo, hi)).to_numpy()
        report["buckets"][f"{lo}-{hi}"] = block(m)
    # Per-K pooled walk-forward MAE with a per-fold refit map, to show the
    # shape of the curve rather than only the argmin.
    curve = {}
    for m in methods:
        errs = []
        for s, tr, te in folds(df["season"], WALK_FROM):
            beta = fit_map(df[tr], f"em_{m}")
            errs.append(np.abs(df[te]["actual"].to_numpy() - predict(df[te], f"em_{m}", beta)))
        curve[m] = float(np.mean(np.concatenate(errs)))
    report["curve_mae"] = curve
    # Early-season curve: the games where the choice actually matters.
    early = {}
    for m in methods:
        errs = []
        for s, tr, te in folds(df["season"], WALK_FROM):
            beta = fit_map(df[tr], f"em_{m}")
            t = df[te & (df["g_min"] <= 9)]
            errs.append(np.abs(t["actual"].to_numpy() - predict(t, f"em_{m}", beta)))
        early[m] = float(np.mean(np.concatenate(errs)))
    report["curve_mae_g_le_9"] = early
    report["ranking_spearman_vs_final"] = {
        str(cp): {m: float(np.nanmean([rankings[s][cp][m] for s in rankings
                                        if s >= WALK_FROM and cp in rankings[s]]))
                  for m in rankings[max(rankings)][cp]}
        for cp in CHECKPOINT_DAYS if cp in rankings[max(rankings)]
    }

    print(json.dumps({k: report[k] for k in ("k_chosen", "pooled")}, indent=1))
    for b, r in report["buckets"].items():
        print(f"g {b:>6} n={r['n']:>6}  " + "  ".join(
            f"{m} {r[m]['mae']:.2f}" + (f" (z {r[m]['z_vs_raw']:+.1f})" if r[m]['z_vs_raw'] is not None else "")
            for m in held))
    out = Path(args.out) if args.out else (
        EVAL_DIR / f"rankings_preseason_prior_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(report, indent=1))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
