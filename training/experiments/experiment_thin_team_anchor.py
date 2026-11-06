"""Should a team with no preseason projection get a fallback anchor in the game blend?

`apply_preseason_blend` needs a preseason number for both teams, so a game with
a thin team (about 70 a season, no projection) skips the blend and runs on the
model alone, on one or two games of data. In opening week of 2027 that put
Butler +2.1 over Fairleigh Dickinson against a line of -27.5.

The rankings already answer this team: `FALLBACK_PRIOR_SHRINK` x last season's
AdjEM (#446). This scores the same number as the blend's preseason leg, on every
completed game inside the blend window with exactly the thin team(s) missing:

    blended = w * (diff + home court) + (1 - w) * stored margin

The stored `game_projections` margin of such a game is the unblended model, so
it is the served answer. It is also generous to the served side: only the CamPom
channel is point-in-time and the rest of its features hold the game itself
(#274), which the comparison leaves in.

No constant is fitted here: 0.9 and the blend schedule are the served values, so
each season is an out-of-sample fold. The shrink grid is a sensitivity check.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_thin_team_anchor.py \
          --out eval_history/thin_team_anchor_YYYYMMDD_summary.json
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import datetime as dt
import json

import numpy as np
import pandas as pd

from db import get_engine

SHRINK = 0.9
GRID = [0.5, 0.7, 0.8, 0.9, 1.0]
PEAK, DECAY_DAYS, HCA = 0.70, 42, 3.5

QUERY = """
WITH anchors AS (
    SELECT t.id AS team_id, t.season, p.projected_adj_em::float8 AS projected,
           prev.adj_efficiency_margin::float8 AS last_em
    FROM teams t
    LEFT JOIN team_preseason_projection p ON p.team_id = t.id AND p.season = t.season
    LEFT JOIN teams tp ON tp.natstat_id = t.natstat_id AND tp.season = t.season - 1
    LEFT JOIN team_season_stats prev ON prev.team_id = tp.id AND prev.season = t.season - 1
)
SELECT g.season, g.game_date, g.is_neutral_site AS neutral,
       (g.home_score - g.away_score)::float8 AS actual,
       gp.projected_margin::float8 AS stored,
       h.projected AS h_proj, h.last_em AS h_last,
       a.projected AS a_proj, a.last_em AS a_last
FROM game_projections gp
JOIN games g ON g.id = gp.game_id
JOIN anchors h ON h.team_id = g.home_team_id AND h.season = g.season
JOIN anchors a ON a.team_id = g.away_team_id AND a.season = g.season
WHERE gp.home_team_id = g.home_team_id
  AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
  AND g.season BETWEEN 2016 AND 2026
"""


def weight(row_date: dt.date, season: int) -> float:
    days = ((row_date - dt.timedelta(days=1)) - dt.date(season - 1, 11, 1)).days
    if days <= 0:
        return PEAK
    return max(0.0, PEAK * (1 - days / DECAY_DAYS))


def blended(df: pd.DataFrame, shrink: float) -> pd.Series:
    h = df["h_proj"].fillna(shrink * df["h_last"])
    a = df["a_proj"].fillna(shrink * df["a_last"])
    hca = np.where(df["neutral"], 0.0, HCA)
    return df["w"] * (h - a + hca) + (1 - df["w"]) * df["stored"]


def paired(old: pd.Series, new: pd.Series) -> dict:
    d = new - old
    return {
        "n": int(len(d)),
        "served_mae": float(old.mean()),
        "fallback_mae": float(new.mean()),
        "delta": float(d.mean()),
        "z": float(d.mean() / (d.std(ddof=1) / np.sqrt(len(d)))) if len(d) > 1 else None,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out")
    args = parser.parse_args()

    df = pd.read_sql(QUERY, get_engine())
    df["w"] = [weight(d, s) for d, s in zip(df["game_date"], df["season"])]
    window = df[df["w"] > 0]
    thin = window[
        (window["h_proj"].isna() | window["a_proj"].isna())
        & (window["h_proj"].notna() | window["h_last"].notna())
        & (window["a_proj"].notna() | window["a_last"].notna())
    ].copy()
    anchored = window[window["h_proj"].notna() & window["a_proj"].notna()]

    served = (thin["stored"] - thin["actual"]).abs()
    fallback = (blended(thin, SHRINK) - thin["actual"]).abs()
    pooled = paired(served, fallback)
    folds = {
        int(s): paired(served[g.index], fallback[g.index]) for s, g in thin.groupby("season")
    }
    wins = sum(f["delta"] < 0 for f in folds.values())
    grid = {
        str(k): float((blended(thin, k) - thin["actual"]).abs().mean()) for k in GRID
    }
    gap = thin["h_proj"].isna() & thin["a_proj"].isna()
    summary = {
        "pooled": pooled,
        "folds_improved": f"{wins}/{len(folds)}",
        "folds": folds,
        "shrink_grid_mae": grid,
        "both_thin_games": int(gap.sum()),
        "anchored_games_mae": float((anchored["stored"] - anchored["actual"]).abs().mean()),
        "anchored_games": int(len(anchored)),
    }
    print(json.dumps(summary, indent=2))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=2)


if __name__ == "__main__":
    main()
