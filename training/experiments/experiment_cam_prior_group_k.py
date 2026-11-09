"""Should CAM's preseason prior be worth a different number of games by player type?

`experiment_cam_preseason_prior.py` fits one K for everyone (`CAM_PRIOR_GAMES`
= 100). Split by type, that single K helps returners and narrow-band players
and is slightly worse for freshmen and the widest-band players, who would like
the projection to let go sooner. This asks whether a K per group beats one K
held out, for several ways of grouping players, all knowable before the season:

    cohort      freshman / transfer / returner on the same team
    elite       the projection's rank in its season: top 5%, 5-25%, the rest
    class       class year on the current roster (Fr / So / Jr / Sr)
    stars       freshmen by recruit stars (5 / 4 / 3-or-fewer / unrated), every
                returner in one group
    role        last season's minutes, terciles, freshmen their own group
    archetype   last season's primary archetype, freshmen their own group
    band        the projection's q10-q90 width, quartiles
    cohort_elite  cohort x elite
    random      four groups by a hash of the player id: the null. Whatever this
                "gains" is what fitting more Ks buys by chance.

Same replay, target, taper and walk-forward as the parent experiment: CAM is
rebuilt as of team game T from the per-game Torvik rows and scored on the
player's remaining games; train on seasons < S, score S, S = 2021..2026. Per
fold, a flat K and a K per group are both chosen by training-season MAE; a group
with fewer than MIN_GROUP training rows falls back to the flat K. The flat K is
the baseline, so the comparison is one K against several, fitted the same way.

One change from the parent, and it matters: the observed side and the target
carry the player's SOS term, `psos_adj * mp_factor * 0.8`, as the served
`cam_rating` and the projection's own target (`cam_gbpm_v3_psos`) do. The
parent scores a no-SOS composite against an SOS-inclusive prior, so a
high-major player's projection looks about 2.4 too high and a low-major one's
0.5 too low. Under that mismatch top-5% players fit K = 10 and a per-tier
correction to the prior "wins" by 0.066 MAE; with the scales matched both
vanish (`--no-sos` reproduces them). The term uses the season-end SOS, so it is
known on both sides; the served number reads its opponents' blended ratings,
which is close to the same thing by the first weeks.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_cam_prior_group_k.py
"""

from __future__ import annotations

import os as _os
import sys as _sys

_here = _os.path.dirname(_os.path.abspath(__file__))
_sys.path.insert(0, _os.path.dirname(_here))
_sys.path.insert(0, _here)

import argparse
import datetime as dt
import json
import zlib
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import text

import experiment_cam_preseason_prior as parent
from compute_cae import EVAL_DIR
from db import get_engine
from walk_forward import WALK_FROM, folds

K_GRID = [10, 20, 30, 40, 50, 60, 80, 100, 120, 160, 200, 300, 500]
MIN_GROUP = 2000
GROUPINGS = [
    "cohort", "elite", "class", "stars", "role", "archetype", "band", "cohort_elite", "random",
]

PRIOR_QUERY = text(
    """
    SELECT o.torvik_pid AS pid, o.mean AS prior, o.upper - o.lower AS band,
           'returner' AS kind,
           (b.team_name IS DISTINCT FROM c.team_name) AS moved,
           b.total_minutes AS last_min, p.class_year, NULL::int AS stars,
           (SELECT pa.primary_class FROM player_archetypes pa
             WHERE pa.player_id = b.player_id AND pa.season = o.target_season - 1
             ORDER BY pa.provisional NULLS FIRST LIMIT 1) AS archetype
      FROM trajectory_oof_predictions o
      LEFT JOIN torvik_player_stats c
        ON c.torvik_pid = o.torvik_pid AND c.season = o.target_season
      LEFT JOIN torvik_player_stats b
        ON b.torvik_pid = o.torvik_pid AND b.season = o.target_season - 1
      LEFT JOIN players p ON p.id = c.player_id
     WHERE o.target_season = :season
    UNION ALL
    SELECT t.torvik_pid, f.mean, f.upper - f.lower, 'freshman', false, NULL,
           p.class_year,
           (SELECT max(r.star_rating) FROM recruits r
             WHERE r.cstat_player_id = f.cstat_player_id),
           NULL
      FROM freshman_oof_predictions f
      JOIN torvik_player_stats t
        ON t.player_id = f.cstat_player_id AND t.season = f.target_season
      LEFT JOIN players p ON p.id = f.cstat_player_id
     WHERE f.target_season = :season AND t.torvik_pid IS NOT NULL
    """
)


SOS_QUERY = text(
    """
    SELECT DISTINCT ON (season, torvik_pid)
           season, torvik_pid AS pid, psos_adj::float8 AS psos, mp_factor::float8 AS mpf
      FROM torvik_player_stats
     WHERE season BETWEEN :lo AND :hi AND torvik_pid IS NOT NULL
     ORDER BY season, torvik_pid, total_minutes DESC NULLS LAST
    """
)


def load(engine, sos: bool = True) -> pd.DataFrame:
    parent.PRIOR_QUERY = PRIOR_QUERY
    parts = [parent.season_frame(engine, s)[0] for s in range(parent.FIRST_SEASON, parent.LAST_SEASON + 1)]
    df = pd.concat(parts, ignore_index=True)
    for col in ("band", "last_min", "stars"):
        df[col] = pd.to_numeric(df[col], errors="coerce")
    term = pd.read_sql(
        SOS_QUERY, engine, params={"lo": parent.FIRST_SEASON, "hi": parent.LAST_SEASON}
    )
    df = df.merge(term, on=["season", "pid"], how="left")
    add = (df.psos.fillna(0.0) * df.mpf.fillna(1.0) * parent.FULL_WEIGHT) if sos else 0.0
    df["y"] = df.rate_rest * parent.FULL_WEIGHT + add
    df["obs"] = df.rate * parent.FULL_WEIGHT + add
    fresh = df.kind == "freshman"

    df["cohort"] = np.where(fresh, "freshman", np.where(df.moved, "transfer", "returner"))

    # Rank among every player with a projection that season, once per player.
    ranks = df.drop_duplicates(["season", "pid"])[["season", "pid", "prior"]].copy()
    ranks["pct"] = ranks.groupby("season").prior.rank(pct=True, ascending=False)
    df = df.merge(ranks[["season", "pid", "pct"]], on=["season", "pid"])
    df["elite"] = np.select([df.pct <= 0.05, df.pct <= 0.25], ["top5", "top25"], "rest")
    df["cohort_elite"] = df.cohort + "_" + df.elite

    df["class"] = df.class_year.fillna("unknown")
    df["stars"] = np.where(
        fresh,
        np.select([df.stars >= 5, df.stars == 4, df.stars.notna()], ["5star", "4star", "3star-"], "unrated"),
        "returner",
    )
    terc = df[~fresh].groupby("season").last_min.transform(
        lambda s: pd.qcut(s.rank(method="first"), 3, labels=["low", "mid", "high"])
    )
    terc = terc.reindex(df.index).astype(str)
    df["role"] = np.where(fresh, "freshman", "min_" + terc)
    df["archetype"] = np.where(fresh, "freshman", df.archetype.fillna("none"))
    df["band"] = pd.qcut(df.band, 4, labels=["narrow", "mid-", "mid+", "wide"]).astype(str)
    df["random"] = [f"r{zlib.crc32(str(p).encode()) % 4}" for p in df.pid]
    return df


def predict(d: pd.DataFrame, k) -> np.ndarray:
    w = parent.taper_weight(k, d.gp.values)
    return w * d.prior.values + (1 - w) * d.obs.values


def fit_k(d: pd.DataFrame) -> float:
    y = d.y.values
    return float(min(K_GRID, key=lambda k: np.abs(predict(d, k) - y).mean()))


def paired_z(a: np.ndarray, b: np.ndarray) -> float:
    d = a - b
    return float(d.mean() / (d.std(ddof=1) / np.sqrt(len(d))))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--no-sos", action="store_true", help="the parent's no-SOS target")
    args = ap.parse_args()

    df = load(get_engine(), sos=not args.no_sos)
    print(f"{len(df):,} player-checkpoints")

    flat = np.full(len(df), np.nan)
    grouped = {g: np.full(len(df), np.nan) for g in GROUPINGS}
    chosen = {g: {} for g in GROUPINGS}
    flat_k = {}
    for season, train, test in folds(df.season, WALK_FROM):
        tr, te = df[train], df[test]
        k0 = fit_k(tr)
        flat_k[season] = k0
        flat[test.to_numpy()] = predict(te, k0)
        for g in GROUPINGS:
            ks = {}
            for level, sub in tr.groupby(g):
                ks[level] = fit_k(sub) if len(sub) >= MIN_GROUP else k0
            chosen[g][season] = ks
            k_row = te[g].map(ks).fillna(k0).to_numpy(dtype=float)
            grouped[g][test.to_numpy()] = predict(te, k_row)

    wf = (df.season >= WALK_FROM).to_numpy()
    res = df[wf].copy()
    y = res.y.to_numpy()
    e_flat = np.abs(flat[wf] - y)
    print(f"flat K per fold: {flat_k}")
    print(f"flat pooled MAE {e_flat.mean():.4f}\n")
    summary = {"sos_target": not args.no_sos, "rows": int(wf.sum()), "flat_k": flat_k, "flat_mae": float(e_flat.mean()), "groupings": {}}

    print(f"{'grouping':>13} {'MAE':>7} {'delta':>8} {'z':>6} {'seasons':>8}")
    for g in GROUPINGS:
        e = np.abs(grouped[g][wf] - y)
        by_season = {
            int(s): float((e - e_flat)[(res.season == s).to_numpy()].mean())
            for s in sorted(res.season.unique())
        }
        wins = sum(v < 0 for v in by_season.values())
        z = paired_z(e, e_flat)
        levels = {}
        for level, idx in res.groupby(g).indices.items():
            ks = [chosen[g][s].get(level) for s in sorted(chosen[g])]
            levels[str(level)] = {
                "n": int(len(idx)),
                "k_by_fold": ks,
                "flat_mae": float(e_flat[idx].mean()),
                "group_mae": float(e[idx].mean()),
            }
        summary["groupings"][g] = {
            "mae": float(e.mean()),
            "delta": float(e.mean() - e_flat.mean()),
            "z": z,
            "seasons_better": wins,
            "by_season": by_season,
            "levels": levels,
        }
        print(f"{g:>13} {e.mean():7.4f} {e.mean() - e_flat.mean():+8.4f} {z:6.1f} {wins:>5}/6")

    for g in GROUPINGS:
        print(f"\n{g}:")
        for level, v in sorted(summary["groupings"][g]["levels"].items(), key=lambda kv: -kv[1]["n"]):
            print(
                f"  {level:>16} n={v['n']:>6}  K {v['k_by_fold']}  "
                f"MAE {v['flat_mae']:.3f} -> {v['group_mae']:.3f}"
            )

    out = args.out or (EVAL_DIR / f"cam_prior_group_k_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(summary, indent=2, default=str) + "\n")
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
