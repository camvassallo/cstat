"""Should game predictions weigh the preseason leg by games played?

A prediction mixes the preseason leg with the model leg. That mix used a
calendar schedule (0.70 at the Nov 1 open, zero 42 days later) while the rating
and CAM fade by games played on one tapered curve. This asks whether predictions
can use that curve too, and scores on the served leg:

  - the rating's curve at full strength;
  - a fitted K on the same curve, with no scale;
  - a scale `c` on the rating's own K;
  - both K and `c`.

Walk-forward (#361): the grid is chosen on seasons < S and scored on S, against
the calendar on the same games. A game's weight is the mean of its two teams'.

Input is the dump, which needs no model here:

    cargo run --release --bin cstat-ingest -- measure-blend-accuracy --dump legs.csv
    cd training && ./.venv/bin/python experiments/experiment_prediction_prior_curve.py \
        --legs legs.csv --out eval_history/prediction_prior_curve_YYYYMMDD_summary.json

Comparison script only; writes nothing to the database or the models.
"""

import argparse
import json

import numpy as np
import pandas as pd

RATING_K = 12.0
FULL_SEASON_GAMES = 32.0
HCA = 3.5
CALENDAR_PEAK, CALENDAR_DAYS = 0.70, 42
K_GRID = [0.5, 1, 1.5, 2, 3, 4, 5, 6, 8, 10, 12]
C_GRID = [0.4, 0.5, 0.55, 0.6, 0.65, 0.7, 0.8, 0.9, 1.0]


def taper(k, games):
    end = k / (k + FULL_SEASON_GAMES)
    return np.maximum((k / (k + games) - end) / (1.0 - end), 0.0)


def curve_weight(df, k, c):
    return c * (taper(k, df.home_games) + taper(k, df.away_games)) / 2.0


def calendar_weight(df):
    return np.clip(CALENDAR_PEAK * (1 - df.day_offset / CALENDAR_DAYS), 0, CALENDAR_PEAK)


def abs_error(df, w):
    return np.abs(w * df.leg + (1 - w) * df.pit_margin - df.actual)


def z_score(d):
    return float(d.mean() / (d.std() / np.sqrt(len(d))))


def walk_forward(df, grid, seasons):
    calendar, curve, picks = [], [], []
    for s in seasons:
        train, test = df[df.season < s], df[df.season == s]
        k, c = min(grid, key=lambda p: abs_error(train, curve_weight(train, *p)).mean())
        picks.append({"season": int(s), "k": k, "c": c})
        calendar.append(abs_error(test, calendar_weight(test)))
        curve.append(abs_error(test, curve_weight(test, k, c)))
    a, b = pd.concat(calendar), pd.concat(curve)
    return {
        "games": len(a),
        "calendar_mae": float(a.mean()),
        "curve_mae": float(b.mean()),
        "delta": float((b - a).mean()),
        "z": z_score(b - a),
        "picks": picks,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--legs", required=True, help="measure-blend-accuracy --dump CSV")
    ap.add_argument("--last-season", type=int, default=2026, help="last finished season")
    ap.add_argument("--out", help="write the summary JSON here")
    args = ap.parse_args()

    df = pd.read_csv(args.legs)
    df = df[df.season <= args.last_season].reset_index(drop=True)
    df["leg"] = df.pre_diff + np.where(df.is_neutral, 0.0, HCA)

    base = abs_error(df, calendar_weight(df))
    full = abs_error(df, curve_weight(df, RATING_K, 1.0))
    seasons = sorted(df.season.unique())[2:]
    summary = {
        "games": len(df),
        "calendar_mae": float(base.mean()),
        "full_strength": {
            "mae": float(full.mean()),
            "z_vs_calendar": z_score(full - base),
        },
        "walk_forward": {
            "k_only": walk_forward(df, [(k, 1.0) for k in K_GRID], seasons),
            "rating_k_scaled": walk_forward(df, [(RATING_K, c) for c in C_GRID], seasons),
            "k_and_scale": walk_forward(df, [(k, c) for k in K_GRID for c in C_GRID], seasons),
        },
    }
    print(json.dumps(summary, indent=2))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=2)


if __name__ == "__main__":
    main()
