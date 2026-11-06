"""How much of a stored game projection's edge is hindsight, and from which features?

`game_projections` graded 2,476-1,806 against the spread on 2026 (57.8%). No
model beats a closing line by that much, and the likely reason is #274: the
point-in-time path makes only the six CamPom features point-in-time, and the
other 41 model inputs are season aggregates that include the game being
projected.

This measures it directly. A margin model with the served hyperparameters is
trained on seasons before the target from the fully pre-game training frame,
then the target season is scored two ways:

  honest   every feature as it stood before the game, the frame as built
  swapped  one or more feature groups replaced by each team's end-of-season
           value, which is what the serving path feeds the model

The model never changes between the two, so any movement comes from the inputs.
Each group is scored alone and as "everything except this group": the second is
what would remain if that group alone were rebuilt, so it orders the work.

The served model has a second, separate leak: `train.py` does its final fit on
every season, so a stored projection for a season in that list comes from a
model that has seen the result. The "seen" rows refit with the target season in
the training data to size that one.

Each row is graded on the feed's line and, with --espn-csv, on the sportsbook
line ESPN carried. The feed's line is wrong often enough to flatter any model
(#448).

"End of season" is a team's last row in the frame. Those rows are themselves
pre-game, so a team's final game stays honest, which understates the leak a
little.

Elo needs care. The frame takes NatStat's pre-game Elo where `game_forecasts` has
it and otherwise a homemade series that restarts at 1500 each season. 2026 has
none, so its frame Elo sits on a much narrower scale than every training season.
Swapping that series mixes hindsight with a scale repair, so 2026 is only clean
through the "served elo" rows, which use `team_season_stats.elo_rating`. Score
2025 for the frame's own Elo.

--walk-forward asks the other question: with nothing swapped, does the frame
itself beat the line, season after season? Everything except CamPom should land
near 50%, and the gap above it is what the CamPom channel knows.

Comparison script only; writes nothing to the database or the models.

Needs a pickled frame, about 90 minutes to build, from `build_pit_lookup.py` and
then `build_feature_matrix` under GBPM_VARIANT=pit_cam_v3 (PIT_LOOKUP_PATH points
at the lookup).

Run:  cd training && ./.venv/bin/python experiments/experiment_pit_feature_leak.py \
          --frame /path/to/frame.pkl --espn-csv lines.csv --walk-forward
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), ".."))

import argparse
import json
import math

import lightgbm as lgb
import numpy as np
import pandas as pd

from db import get_engine
from features import completeness_subset

# Served margin hyperparameters, from `train.train_margin_model`.
PARAMS = dict(
    objective="regression", metric="mae", num_leaves=24, learning_rate=0.03,
    feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
    min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0, verbose=-1,
)
MAX_TREES = 1000
EARLY_STOP = 80
SEEDS = (1, 2, 3)

# diff feature -> the home_/away_ column behind it, from `build_feature_matrix`.
TEAM = {
    "adj_offense": "adj_offense", "adj_defense": "adj_defense",
    "adj_efficiency_margin": "adj_efficiency_margin",
    "effective_fg_pct": "cum_efg_pct", "turnover_pct": "cum_tov_pct",
    "off_rebound_pct": "cum_orb_pct", "ft_rate": "cum_ft_rate",
    "opp_effective_fg_pct": "cum_opp_efg_pct", "opp_turnover_pct": "cum_opp_tov_pct",
    "def_rebound_pct": "cum_def_reb_pct", "opp_ft_rate": "cum_opp_ft_rate",
    "adj_tempo": "cum_tempo", "sos": "sos", "elo": "elo",
    "point_diff": "cum_game_point_diff", "pythag_win_pct": "cum_pythag_win_pct",
    "road_win_pct": "cum_road_win_pct", "win_pct": "cum_win_pct",
}
ROSTER = {n: n for n in (
    "roster_size", "w_ppg", "w_rpg", "w_apg", "w_spg", "w_bpg", "w_topg",
    "w_ts_pct", "w_efg_pct", "w_usage", "w_player_sos", "w_ortg", "w_ast_pct",
    "w_tov_pct", "w_stl_pct", "w_blk_pct", "star_ppg", "star_ortg",
    "minutes_stddev",
)}
FORM = {n: n for n in ("w_rolling_gs", "w_rolling_ts", "w_ppg_trend", "w_gs_trend")}
# Already point-in-time on the serving path, so never swapped.
CAMPOM = ("w_gbpm", "w_ogbpm", "w_dgbpm", "star_gbpm", "star_ogbpm", "star_dgbpm")
FLIPPED = {"adj_defense", "opp_effective_fg_pct", "opp_ft_rate"}
GROUPS = {"team": TEAM, "roster": ROSTER, "form": FORM}
# The team group by how #274 would rebuild it, cheapest first.
TEAM_PARTS = {
    "elo": ("elo",),
    "record": ("win_pct", "point_diff", "pythag_win_pct", "road_win_pct"),
    "four factors": (
        "effective_fg_pct", "turnover_pct", "off_rebound_pct", "ft_rate",
        "opp_effective_fg_pct", "opp_turnover_pct", "def_rebound_pct",
        "opp_ft_rate", "adj_tempo",
    ),
    "adjusted efficiency": ("adj_offense", "adj_defense", "adj_efficiency_margin", "sos"),
}
FIRST_LINED_SEASON = 2020


def diff_values(home: pd.Series, away: pd.Series, name: str) -> pd.Series:
    if name == "win_pct":
        home, away = home.fillna(0.5), away.fillna(0.5)
    d = home - away
    return -d if name in FLIPPED else d


def check_mapping(df: pd.DataFrame, feature_cols: list[str]) -> None:
    """Every model input is accounted for, and each mapping rebuilds its diff."""
    mapped = {f"diff_{n}" for g in GROUPS.values() for n in g}
    expected = mapped | {f"diff_{n}" for n in CAMPOM} | {"venue", "is_conference_game"}
    if set(feature_cols) != expected:
        raise SystemExit(f"feature map drifted: {sorted(set(feature_cols) ^ expected)}")
    for group in GROUPS.values():
        for name, col in group.items():
            rebuilt = diff_values(df[f"home_{col}"], df[f"away_{col}"], name)
            if not np.allclose(rebuilt, df[f"diff_{name}"], equal_nan=True):
                raise SystemExit(f"diff_{name} does not rebuild from {col}")


def end_of_season(df: pd.DataFrame, cols: list[str]) -> pd.DataFrame:
    """Each team's last value of each column. Team ids are season-scoped."""
    sides = []
    for side in ("home", "away"):
        part = df[[f"{side}_team_id", "game_date"] + [f"{side}_{c}" for c in cols]]
        sides.append(part.rename(columns=lambda c: c.removeprefix(f"{side}_")))
    both = pd.concat(sides).sort_values("game_date", kind="stable")
    return both.groupby("team_id")[cols].last()


def swap(test: pd.DataFrame, end: pd.DataFrame, names: dict[str, str]) -> pd.DataFrame:
    out = test.copy()
    for name, col in names.items():
        home = test["home_team_id"].map(end[col])
        away = test["away_team_id"].map(end[col])
        out[f"diff_{name}"] = diff_values(home, away, name)
    return out


def fit(train: pd.DataFrame, cols: list[str], seed: int) -> lgb.LGBMRegressor:
    """Pick the tree count on the last training season, then refit on all of it."""
    last = train["season"].max()
    early, late = train[train["season"] < last], train[train["season"] == last]
    params = dict(PARAMS, seed=seed, bagging_seed=seed, feature_fraction_seed=seed)
    trees = 300
    if not early.empty:
        probe = lgb.LGBMRegressor(n_estimators=MAX_TREES, **params)
        probe.fit(
            early[cols], early["margin"],
            eval_set=[(late[cols], late["margin"])],
            callbacks=[lgb.early_stopping(EARLY_STOP, verbose=False)],
        )
        trees = probe.best_iteration_
    model = lgb.LGBMRegressor(n_estimators=trees, **params)
    model.fit(train[cols], train["margin"])
    return model


def grade(pred: np.ndarray, margin: np.ndarray, spread: np.ndarray) -> dict:
    """Bet every game on the side the projection favours against the line."""
    has = ~np.isnan(spread)
    pick = np.sign(pred[has] + spread[has])
    result = np.sign(margin[has] + spread[has])
    live = (pick != 0) & (result != 0)
    wins = int((pick[live] == result[live]).sum())
    n = int(live.sum())
    rate = wins / n
    return {
        "mae": float(np.abs(pred[has] - margin[has]).mean()),
        "wins": wins,
        "losses": n - wins,
        "ats": rate,
        "ats_se": math.sqrt(rate * (1 - rate) / n),
    }


def seen_fit(train: pd.DataFrame, test: pd.DataFrame, cols: list[str], seed: int, trees: int):
    """The served shape: the final fit includes the season being projected."""
    params = dict(PARAMS, seed=seed, bagging_seed=seed, feature_fraction_seed=seed)
    model = lgb.LGBMRegressor(n_estimators=trees, **params)
    model.fit(pd.concat([train[cols], test[cols]]), pd.concat([train["margin"], test["margin"]]))
    return model


def load_lines(engine, season: int) -> pd.DataFrame:
    """Spread from the `games` row's home side, plus the stored projection."""
    return pd.read_sql(
        """
        SELECT g.id AS game_id,
               COALESCE(
                   CASE WHEN l.home_team_id = g.home_team_id THEN l.home_spread
                        ELSE -l.home_spread END,
                   CASE WHEN g.season BETWEEN 2020 AND 2025 THEN f.spread END
               ) AS spread,
               p.projected_margin AS stored_margin
        FROM games g
        LEFT JOIN game_lines l ON l.game_id = g.id
        LEFT JOIN game_forecasts f ON f.game_id = g.id AND abs(f.spread) < 60
        LEFT JOIN game_projections p ON p.game_id = g.id
        WHERE g.season = %(season)s
        """,
        engine, params={"season": season},
    )


def load_forecast_lines(engine) -> pd.Series:
    """NatStat's historical spreads, already the home line for 2020-2025.

    Before 2020 the column means something else: its sign is reversed, and 2018
    tracks the final score far too closely to be a pre-game line (4.85 MAE). The
    moneyline columns barely correlate with results in any season.
    """
    rows = pd.read_sql(
        """
        SELECT g.id::text AS game_id, f.spread
        FROM games g JOIN game_forecasts f ON f.game_id = g.id
        WHERE f.spread IS NOT NULL AND abs(f.spread) < 60
          AND g.season BETWEEN %(first)s AND 2025
        """,
        engine, params={"first": FIRST_LINED_SEASON},
    )
    return rows.set_index("game_id")["spread"]


def walk_forward(df: pd.DataFrame, feature_cols: list[str], espn: pd.Series | None, last: int) -> list[dict]:
    """Train on earlier seasons, bet every game of the next, for each feature set."""
    lines = load_forecast_lines(get_engine())
    campom = [f"diff_{c}" for c in CAMPOM]
    sets = {
        "all": feature_cols,
        "no campom": [c for c in feature_cols if c not in campom],
        "campom only": campom + ["venue", "is_conference_game"],
    }
    rows = []
    for season in range(FIRST_LINED_SEASON, last + 1):
        train = df[df["season"] < season]
        test = df[df["season"] == season]
        source = espn if season > 2025 and espn is not None else lines
        spread = test["game_id"].astype(str).map(source).to_numpy(float)
        margin = test["margin"].to_numpy(float)
        if np.isnan(spread).all():
            continue
        row = {"season": season, "line_mae": float(np.nanmean(np.abs(spread + margin)))}
        for label, cols in sets.items():
            pred = np.mean([fit(train, cols, s).predict(test[cols]) for s in SEEDS[:2]], axis=0)
            row[label] = grade(pred, margin, spread)
        rows.append(row)
        print(season, {k: round(row[k]["ats"] * 100, 1) for k in sets}, flush=True)
    for label in sets:
        wins = sum(r[label]["wins"] for r in rows)
        losses = sum(r[label]["losses"] for r in rows)
        print(f"{label:12s} pooled {wins}-{losses} {wins / (wins + losses) * 100:.1f}%")
    return rows


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--frame", required=True, help="pickled build_feature_matrix frame")
    ap.add_argument("--season", type=int, default=2026)
    ap.add_argument("--espn-csv", help="game_id,espn_spread in the `games` row's frame")
    ap.add_argument("--walk-forward", action="store_true")
    ap.add_argument("--out")
    args = ap.parse_args()

    df = pd.read_pickle(args.frame)
    feature_cols = ["venue", "is_conference_game", "diff_win_pct"] + [
        c for c in df.columns if c.startswith("diff_") and c != "diff_win_pct"
    ]
    check_mapping(df, feature_cols)

    # End-of-season values come from every game, before the completeness filter
    # drops a team's early rows.
    target = df[df["season"] == args.season]
    all_cols = [c for g in GROUPS.values() for c in g.values()]
    end = end_of_season(target, all_cols)
    served = pd.read_sql(
        "SELECT team_id, elo_rating FROM team_season_stats WHERE season = %(season)s",
        get_engine(), params={"season": args.season},
    )
    end["served_elo"] = served.set_index("team_id")["elo_rating"]

    df = df.dropna(subset=completeness_subset(feature_cols)).reset_index(drop=True)
    train = df[df["season"] < args.season]
    test = df[df["season"] == args.season].reset_index(drop=True)
    lines = load_lines(get_engine(), args.season)
    test = test.merge(lines, on="game_id", how="left")
    print(f"train {len(train)} rows, test {len(test)}, lined {test['spread'].notna().sum()}")

    margin = test["margin"].to_numpy(float)
    line_sets = {"feed": test["spread"].to_numpy(float)}
    by_game = None
    if args.espn_csv:
        espn = pd.read_csv(args.espn_csv)[["game_id", "espn_spread"]]
        by_game = espn.set_index(espn["game_id"].astype(str))["espn_spread"]
        line_sets["espn"] = test["game_id"].astype(str).map(by_game).to_numpy(float)

    variants: dict[str, dict[str, str]] = {"honest": {}}
    for label, group in GROUPS.items():
        variants[f"{label} leaked"] = group
    for label in GROUPS:
        variants[f"all but {label} leaked"] = {
            n: c for other, g in GROUPS.items() if other != label for n, c in g.items()
        }
    variants["all leaked"] = {n: c for g in GROUPS.values() for n, c in g.items()}
    for label, names in TEAM_PARTS.items():
        variants[f"{label} leaked"] = {n: TEAM[n] for n in names}
    variants["served elo leaked"] = {"elo": "served_elo"}
    variants["all leaked, served elo"] = {**variants["all leaked"], "elo": "served_elo"}
    variants["all but elo leaked"] = {n: c for n, c in variants["all leaked"].items() if n != "elo"}

    frames = {label: swap(test, end, names) for label, names in variants.items()}
    scores: dict[tuple[str, str], list[dict]] = {}
    for seed in SEEDS:
        model = fit(train, feature_cols, seed)
        seen = seen_fit(train, test, feature_cols, seed, model.n_estimators)
        for label, frame in frames.items():
            for book, spread in line_sets.items():
                scores.setdefault((label, book), []).append(
                    grade(model.predict(frame[feature_cols]), margin, spread)
                )
                if label in ("honest", "all leaked"):
                    scores.setdefault((f"seen, {label}", book), []).append(
                        grade(seen.predict(frame[feature_cols]), margin, spread)
                    )

    report: dict[str, dict] = {}
    for (label, book), runs in scores.items():
        report.setdefault(label, {})[book] = {
            "mae": float(np.mean([r["mae"] for r in runs])),
            "ats": float(np.mean([r["ats"] for r in runs])),
            "ats_low": min(r["ats"] for r in runs),
            "ats_high": max(r["ats"] for r in runs),
            "ats_se": runs[0]["ats_se"],
            "bets": runs[0]["wins"] + runs[0]["losses"],
        }
    stored = test["stored_margin"].to_numpy(float)
    report["stored game_projections"] = {}
    report["line itself"] = {}
    for book, spread in line_sets.items():
        usable = np.where(np.isnan(stored), np.nan, spread)
        report["stored game_projections"][book] = grade(np.nan_to_num(stored), margin, usable)
        has = ~np.isnan(spread)
        report["line itself"][book] = {"mae": float(np.abs(spread[has] + margin[has]).mean())}

    for label, books in report.items():
        cells = []
        for book, r in books.items():
            ats = f"{r['ats'] * 100:5.1f}%" if "ats" in r else "   -  "
            cells.append(f"{book} MAE {r['mae']:6.3f} ATS {ats}")
        print(f"{label:28s} " + "  |  ".join(cells))
    seasons = walk_forward(df, feature_cols, by_game, args.season) if args.walk_forward else []
    if args.out:
        with open(args.out, "w") as f:
            json.dump(
                {"season": args.season, "seeds": list(SEEDS), "report": report, "walk_forward": seasons},
                f, indent=2,
            )


if __name__ == "__main__":
    main()
