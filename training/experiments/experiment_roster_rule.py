"""Which roster should a game's training row describe (#467)?

The game models' roster aggregates (`diff_w_*`, `diff_star_*`, the CamPom
channel) used to be built from the box score of the game being trained on:
the players who actually played. Serving cannot know that. `features.rs` builds
a game's roster from everyone on the team so far with at least 5 games and 10
minutes a game, with no idea who is available tonight. So the model learned
from one roster and was asked about another, and every walk-forward number on
the box frame graded it on a roster it would never see.

`training/features.py` now takes the rule from ROSTER_RULE:

  box      who played in this game (the old frame, kept for comparison)
  season   everyone seen on the team before this game (the served rule)
  recentN  who appeared in the team's previous N games

This trains on one rule and grades on another, over identical games. The arm
that matters is "box>season": the model as it was trained, scored on the
roster it is actually given. "season>season" is the fix. "box>box" is the old
walk-forward number, optimistic because its test rows know who played.

Each season 2020 through the last is scored by a model trained only on earlier
seasons (two seeds averaged), and graded on MAE over every common game and ATS
against the feed's line (2020-2025) or ESPN's (2026, --espn-csv).

Comparison script only; writes nothing to the database or the models.

Needs one pickled frame per rule, each built with `build_feature_matrix` under
GBPM_VARIANT=pit_cam_v3 and ROSTER_RULE set (about 90 minutes each, or minutes
when the per-date efficiency snapshots are replayed from a cache: they depend
only on game dates).

Run:  cd training && ./.venv/bin/python experiments/experiment_roster_rule.py \\
          --frame box=/path/box.pkl --frame season=/path/season.pkl \\
          --frame recent3=/path/recent3.pkl --espn-csv lines.csv --out summary.json
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), ".."))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import json

import numpy as np
import pandas as pd

from experiment_pit_feature_leak import FIRST_LINED_SEASON, fit, load_forecast_lines
from db import get_engine
from features import completeness_subset

# (train rule, test rule). Arms naming a missing frame are skipped.
ARMS = (
    ("box", "box"),
    ("box", "season"),
    ("season", "season"),
    ("box", "recent3"),
    ("recent3", "recent3"),
)
# (arm, baseline): paired per-game MAE differences.
CONTRASTS = (
    ("season>season", "box>season"),
    ("recent3>recent3", "season>season"),
    ("recent3>recent3", "box>season"),
)


def load_frames(specs: list[str]) -> dict[str, pd.DataFrame]:
    """Read every frame and keep only the games all of them can score."""
    frames = {}
    for spec in specs:
        rule, path = spec.split("=", 1)
        frames[rule] = pd.read_pickle(path)
    first = next(iter(frames.values()))
    cols = feature_columns(first)
    sub = completeness_subset(cols)
    frames = {k: v.dropna(subset=sub) for k, v in frames.items()}
    common = set.intersection(*(set(v["game_id"].astype(str)) for v in frames.values()))
    print({k: len(v) for k, v in frames.items()}, "common", len(common), flush=True)
    frames = {
        k: v[v["game_id"].astype(str).isin(common)].sort_values("game_id").reset_index(drop=True)
        for k, v in frames.items()
    }
    margins = [v["margin"].to_numpy() for v in frames.values()]
    assert all((m == margins[0]).all() for m in margins), "frames disagree on results"
    return frames


def feature_columns(df: pd.DataFrame) -> list[str]:
    """The served margin model's inputs, in `train.py` order."""
    return ["venue", "is_conference_game", "diff_win_pct"] + [
        c for c in df.columns if c.startswith("diff_") and c != "diff_win_pct"
    ]


def run(frames: dict[str, pd.DataFrame], espn: pd.Series | None, last: int) -> dict:
    cols = feature_columns(next(iter(frames.values())))
    lines = load_forecast_lines(get_engine())
    arms = [(tr, te) for tr, te in ARMS if tr in frames and te in frames]
    names = [f"{tr}>{te}" for tr, te in arms]
    errors = {n: [] for n in names}
    record = {n: [0, 0] for n in names}
    seasons = []
    for season in range(FIRST_LINED_SEASON, last + 1):
        fits = {
            rule: [fit(df[df["season"] < season], cols, s) for s in (1, 2)]
            for rule, df in frames.items()
            if any(tr == rule for tr, _ in arms)
        }
        row = {"season": season}
        for name, (tr, te) in zip(names, arms):
            test = frames[te][frames[te]["season"] == season]
            pred = np.mean([m.predict(test[cols]) for m in fits[tr]], axis=0)
            margin = test["margin"].to_numpy(float)
            err = np.abs(pred - margin)
            errors[name].append(err)
            source = espn if season > 2025 and espn is not None else lines
            spread = test["game_id"].astype(str).map(source).to_numpy(float)
            has = ~np.isnan(spread)
            result, pick = np.sign(margin[has] + spread[has]), np.sign(pred[has] + spread[has])
            live = (result != 0) & (pick != 0)
            wins = int((pick[live] == result[live]).sum())
            record[name][0] += wins
            record[name][1] += int(live.sum())
            row[name] = {"mae": float(err.mean()), "ats": wins / max(int(live.sum()), 1), "n_ats": int(live.sum())}
        print(season, {n: round(row[n]["mae"], 3) for n in names}, flush=True)
        seasons.append(row)

    pooled = {}
    for name in names:
        err = np.concatenate(errors[name])
        wins, n = record[name]
        pooled[name] = {"mae": float(err.mean()), "ats": wins / n, "wins": wins, "n_ats": n}
        print(f"{name:18s} MAE {err.mean():.3f}  ATS {wins / n * 100:.2f}% n={n}")
    contrasts = []
    for arm, base in CONTRASTS:
        if arm not in errors or base not in errors:
            continue
        diff = np.concatenate(errors[arm]) - np.concatenate(errors[base])
        z = diff.mean() / (diff.std() / len(diff) ** 0.5)
        folds = sum(np.mean(a) < np.mean(b) for a, b in zip(errors[arm], errors[base]))
        contrasts.append({
            "arm": arm, "baseline": base, "mae_diff": float(diff.mean()), "z": float(z),
            "folds_better": int(folds), "folds": len(errors[arm]),
        })
        print(f"{arm} minus {base}: {diff.mean():+.4f} z={z:+.2f}; folds better {folds}/{len(errors[arm])}")
    return {
        "n_games": int(sum(len(e) for e in next(iter(errors.values())))),
        "pooled": pooled,
        "contrasts": contrasts,
        "seasons": seasons,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--frame", action="append", required=True, help="RULE=path, once per rule")
    ap.add_argument("--espn-csv", help="game_id,espn_spread in the `games` row's frame")
    ap.add_argument("--last", type=int, default=2026)
    ap.add_argument("--out")
    args = ap.parse_args()
    espn = None
    if args.espn_csv:
        lines = pd.read_csv(args.espn_csv)
        espn = lines.set_index(lines["game_id"].astype(str))["espn_spread"]
    summary = run(load_frames(args.frame), espn, args.last)
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=1)


if __name__ == "__main__":
    main()
