"""What is the honest game margin implied by a preseason AdjEM difference?

#387 wants `/api/predict` to answer a matchup in a season that has not
started — the only regime in which the preseason projection is the *whole*
prediction rather than one leg of a blend. The quantity the code already has
is `projection::fetch_preseason_margin`:

    home_adjem - away_adjem + 3.5 (home) / 0.0 (neutral)

i.e. **the AdjEM difference served verbatim as a point margin**, with an
additive HCA. That form was never tested on its own. It only ever entered
through `apply_preseason_blend`, whose calibrator
(`cstat-ingest measure-blend-accuracy`) grid-searches `hca x w_max x
end_day` and no slope — so a mis-scaled leg is partly absorbed by the 0.70
weight and the pit leg it mixes with, and never shows up as a bias.

At weight 1.0 there is nothing to absorb it, so the scale has to be
measured. Two reasons to expect a slope below 1:

  possessions   AdjEM is points per 100 possessions; a game is ~68, so an
                efficiency edge of 10 is worth ~6.8 points on the scoreboard
  attenuation   the projection is a forecast with error, and the
                minimum-MSE linear map from a noisy predictor onto the truth
                shrinks it by var(signal) / var(predictor)

Both push the same way and neither is in the served form.

Candidates, each a map (emdiff, venue) -> margin, all fit by OLS on the
training seasons only:

    raw          emdiff + 3.5*home                  the shipped leg
    hca          emdiff + h*home                    intercept refit, slope pinned at 1
    slope        s*emdiff + 3.5*home                slope refit, HCA pinned
    slope_hca    s*emdiff + h*home                  both, one intercept
    venue_int    s*emdiff + h_home*home + c         separate home / neutral intercept
    venue_slope  per-venue slope and intercept      does HCA interact with strength

Judged walk-forward (fit on seasons < S, score S, S = 2021..2026; #361) on
every completed game whose two teams both carry a `team_preseason_projection`
row, plus the opening-14-day subset (where the blend engages today) and the
neutral-site subset (where the HCA leg is off and the labels are noisiest).
Paired z on |error| against `raw`. Win-probability sigma is fit on the same
folds by minimising Brier under the served
`projection::margin_to_win_prob` logistic, rather than assumed equal to the
residual RMSE, because that function's 1.6 gaussian-matching constant makes
the two subtly different.

Honesty caveat, stated with the numbers: `team_preseason_projection` holds
the SERVED projection, which for a historical season is in-sample at Layer 2
(#274) — the shipped calibrator trained on those seasons' actuals. An
in-sample projection has less error than a true forecast, so its attenuation
is understated and the fitted slope is biased UPWARD (too little shrinkage).
`--from-dump` re-runs the whole thing on the ex-ante backtest dump's
`roster_proj` pushed through `served_blend.served_prediction`, which is the
same quantity built from the ex-ante frame, as a sensitivity check on that.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_preseason_margin_calibration.py
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

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import served_prediction
from walk_forward import WALK_FROM, folds

# The shipped constants this is measuring against
# (`cstat-core/src/projection.rs`).
SERVED_HCA = 3.5
SERVED_SLOPE = 1.0

# The sigma the served code would use today if the preseason-only regime
# reused a model bundle's calibration, for the comparison row.
SIGMA_PROD = 10.46
SIGMA_PIT = 11.03

# `margin_to_win_prob`'s logistic-to-gaussian matching constant.
LOGISTIC_GAUSSIAN_SCALE = 1.6

# The constants this run selected, ROUNDED to what Rust actually carries
# (`PRESEASON_ONLY_*` in `cstat-core/src/projection.rs`). Scored as its own
# candidate so the summary records the served number rather than a per-fold
# refit nobody ships — rounding a fold-fitted 0.5945 to 0.59 is a change, and
# it should be measured rather than assumed free.
SHIPPED_SLOPE = 0.59
SHIPPED_HCA = 3.2
SHIPPED_SIGMA = 11.1

GAME_QUERY = """
SELECT g.season,
       g.game_date,
       g.is_neutral_site,
       (g.home_score - g.away_score)::float8                   AS actual,
       (h.projected_adj_em - a.projected_adj_em)::float8        AS emdiff,
       ht.natstat_id                                            AS home_natstat_id,
       at.natstat_id                                            AS away_natstat_id
FROM games g
JOIN teams ht ON ht.id = g.home_team_id
JOIN teams at ON at.id = g.away_team_id
JOIN team_preseason_projection h ON h.season = g.season AND h.team_id = g.home_team_id
JOIN team_preseason_projection a ON a.season = g.season AND a.team_id = g.away_team_id
WHERE g.home_score IS NOT NULL
  AND g.away_score IS NOT NULL
ORDER BY g.season, g.game_date, g.id
"""

# The same games, but WITHOUT the projection join, so `--from-dump` can supply
# the ex-ante projection itself. Keyed on natstat_id because the dump is.
GAME_QUERY_NO_PROJ = """
SELECT g.season,
       g.game_date,
       g.is_neutral_site,
       (g.home_score - g.away_score)::float8 AS actual,
       ht.natstat_id                         AS home_natstat_id,
       at.natstat_id                         AS away_natstat_id
FROM games g
JOIN teams ht ON ht.id = g.home_team_id
JOIN teams at ON at.id = g.away_team_id
WHERE g.home_score IS NOT NULL
  AND g.away_score IS NOT NULL
ORDER BY g.season, g.game_date, g.id
"""


# ----------------------------------------------------------------- candidates
def _design(df: pd.DataFrame, cols: list[str]) -> np.ndarray:
    return np.column_stack([df[c].to_numpy(dtype=float) for c in cols])


def _ols(x: np.ndarray, y: np.ndarray) -> np.ndarray:
    """Least squares with an explicit intercept column already in `x`."""
    beta, *_ = np.linalg.lstsq(x, y, rcond=None)
    return beta


def fit_raw(_train: pd.DataFrame) -> dict:
    return {"slope": SERVED_SLOPE, "h_home": SERVED_HCA, "h_neutral": 0.0}


def fit_hca(train: pd.DataFrame) -> dict:
    """Slope pinned at 1; only the additive HCA moves. Isolates how much of
    `raw`'s error is the intercept rather than the scale."""
    resid = train["actual"] - train["emdiff"]
    h = float(resid[~train["is_neutral_site"]].mean()) if (~train["is_neutral_site"]).any() else SERVED_HCA
    return {"slope": 1.0, "h_home": h, "h_neutral": 0.0}


def fit_slope(train: pd.DataFrame) -> dict:
    """HCA pinned at the served 3.5; only the scale moves."""
    hca = np.where(train["is_neutral_site"], 0.0, SERVED_HCA)
    y = train["actual"].to_numpy(dtype=float) - hca
    x = train["emdiff"].to_numpy(dtype=float)[:, None]
    s = float(_ols(x, y)[0])
    return {"slope": s, "h_home": SERVED_HCA, "h_neutral": 0.0}


def fit_slope_hca(train: pd.DataFrame) -> dict:
    """Slope plus one home intercept; neutral games get slope only."""
    x = _design(train.assign(home=(~train["is_neutral_site"]).astype(float)),
                ["emdiff", "home"])
    beta = _ols(x, train["actual"].to_numpy(dtype=float))
    return {"slope": float(beta[0]), "h_home": float(beta[1]), "h_neutral": 0.0}


def fit_venue_int(train: pd.DataFrame) -> dict:
    """Slope plus a free intercept per venue. A non-zero neutral intercept is
    a warning sign, not a feature: it means either the projection is biased or
    the `is_neutral_site` labels are wrong (early-season tournament games are
    the known offender)."""
    x = _design(train.assign(home=(~train["is_neutral_site"]).astype(float), one=1.0),
                ["emdiff", "home", "one"])
    beta = _ols(x, train["actual"].to_numpy(dtype=float))
    return {"slope": float(beta[0]),
            "h_home": float(beta[1] + beta[2]),
            "h_neutral": float(beta[2])}


def fit_venue_slope(train: pd.DataFrame) -> dict:
    """Separate slope and intercept per venue — does home court scale with the
    strength gap rather than adding to it."""
    out = {}
    for neutral, key in ((False, "home"), (True, "neutral")):
        sub = train[train["is_neutral_site"] == neutral]
        if len(sub) < 50:
            out[key] = (1.0, SERVED_HCA if not neutral else 0.0)
            continue
        x = _design(sub.assign(one=1.0), ["emdiff", "one"])
        beta = _ols(x, sub["actual"].to_numpy(dtype=float))
        out[key] = (float(beta[0]), float(beta[1]))
    return {"slope": out["home"][0], "h_home": out["home"][1],
            "slope_neutral": out["neutral"][0], "h_neutral": out["neutral"][1]}


def fit_shipped(_train: pd.DataFrame) -> dict:
    """The rounded constants, fit on nothing. Not a fold refit: this is the
    single (slope, hca) pair Rust serves for every season, so scoring it
    walk-forward is scoring the artifact rather than the method."""
    return {"slope": SHIPPED_SLOPE, "h_home": SHIPPED_HCA, "h_neutral": 0.0}


CANDIDATES = {
    "raw": fit_raw,
    "hca": fit_hca,
    "slope": fit_slope,
    "slope_hca": fit_slope_hca,
    "venue_int": fit_venue_int,
    "venue_slope": fit_venue_slope,
    "shipped": fit_shipped,
}


def apply_params(df: pd.DataFrame, p: dict) -> np.ndarray:
    neutral = df["is_neutral_site"].to_numpy(dtype=bool)
    em = df["emdiff"].to_numpy(dtype=float)
    slope = np.where(neutral, p.get("slope_neutral", p["slope"]), p["slope"])
    inter = np.where(neutral, p.get("h_neutral", 0.0), p["h_home"])
    return slope * em + inter


# ------------------------------------------------------------------- sigma
def win_prob(margin: np.ndarray, sigma: float) -> np.ndarray:
    """`cstat_core::projection::margin_to_win_prob`, vectorised."""
    z = LOGISTIC_GAUSSIAN_SCALE * margin / sigma
    return 1.0 / (1.0 + np.exp(-z))


def brier(margin: np.ndarray, actual: np.ndarray, sigma: float) -> float:
    home_won = (actual > 0).astype(float)
    return float(np.mean((win_prob(margin, sigma) - home_won) ** 2))


def logloss(margin: np.ndarray, actual: np.ndarray, sigma: float) -> float:
    """The scoring rule sigma actually has to answer to. Brier is nearly flat
    in sigma near the optimum (it is dominated by the ordering, which sigma
    cannot change), so choosing on Brier alone would make the choice look
    arbitrary when it is not — logloss punishes an over-confident tail."""
    p = np.clip(win_prob(margin, sigma), 1e-9, 1 - 1e-9)
    home_won = (actual > 0).astype(float)
    return float(-np.mean(home_won * np.log(p) + (1 - home_won) * np.log(1 - p)))


def reliability(margin: np.ndarray, actual: np.ndarray, sigma: float,
                bins: int = 10) -> tuple[float, list]:
    """(expected calibration error, per-decile table). A well-chosen sigma
    makes the observed home-win rate in each predicted-probability decile
    match the decile's mean prediction."""
    p = win_prob(margin, sigma)
    home_won = (actual > 0).astype(float)
    edges = np.quantile(p, np.linspace(0, 1, bins + 1))
    edges[0], edges[-1] = -np.inf, np.inf
    ece, table = 0.0, []
    for lo, hi in zip(edges[:-1], edges[1:]):
        m = (p > lo) & (p <= hi)
        if m.sum() == 0:
            continue
        pred, obs = float(p[m].mean()), float(home_won[m].mean())
        ece += (m.sum() / len(p)) * abs(pred - obs)
        table.append({"n": int(m.sum()), "pred": round(pred, 4), "obs": round(obs, 4)})
    return float(ece), table


def fit_sigma(margin: np.ndarray, actual: np.ndarray) -> float:
    """Sigma that minimises LOGLOSS under the served logistic. Fit rather than
    set to the residual RMSE: the 1.6 constant matches the logistic to the
    normal CDF, so the RMSE is only approximately the best-calibrating
    scale, and the preseason regime has no model meta to read one from."""
    grid = np.arange(8.0, 22.01, 0.05)
    scores = [logloss(margin, actual, s) for s in grid]
    best = int(np.argmin(scores))
    if best in (0, len(grid) - 1):
        raise SystemExit(
            f"sigma optimum landed on the grid edge ({grid[best]}) — the grid is wrong, "
            "and returning the edge would report a bound as a fit"
        )
    return float(grid[best])


# ------------------------------------------------------------------- scoring
def paired_z(base_err: np.ndarray, err: np.ndarray) -> tuple[float, float]:
    """Mean |err_base| - |err| and its z, plus the share of games the
    candidate lands closer on. Negative z = the candidate is better.

    `walk_forward.paired_z` is the team-season version (it keys on dump rows
    by identity); this is the same statistic over two aligned arrays.
    """
    d = base_err - err
    if len(d) < 2:
        return float("nan"), float("nan")
    sd = float(np.std(d, ddof=1))
    m = float(np.mean(d))
    z = -m / (sd / math.sqrt(len(d))) if sd > 0 else 0.0
    return z, float(np.mean(err < base_err))


def block(actual: np.ndarray, pred: np.ndarray) -> dict:
    err = pred - actual
    return {
        "n": int(len(actual)),
        "mae": float(np.mean(np.abs(err))),
        "rmse": float(math.sqrt(np.mean(err ** 2))),
        "bias": float(np.mean(err)),
    }


def cohorts(df: pd.DataFrame) -> dict[str, pd.Series]:
    """Subsets the decision turns on. `first14d` is where the blend engages
    today; `neutral` is where the HCA leg is off; `blowout_gap` is the shape
    #387 unlocks most of (a projected schedule is mostly lopsided
    non-conference games, and an over-extreme slope costs most there)."""
    opens = pd.to_datetime(df["season"].map(lambda s: f"{int(s) - 1}-11-01"))
    day = (pd.to_datetime(df["game_date"]) - opens).dt.days
    neutral = df["is_neutral_site"].astype(bool)
    return {
        "pooled": pd.Series(True, index=df.index),
        "first14d": day < 14,
        "neutral": neutral,
        # Split because the label is not equally trustworthy across the
        # calendar — see `neutral_label_diagnostic`. The early bucket is the
        # multi-team exempt tournaments, and it is also the population a
        # PRESEASON forecast is actually asked about.
        "neutral_early": neutral & (day < 44),
        "neutral_late": neutral & (day >= 44),
        "home": ~neutral,
        "gap_ge_15": df["emdiff"].abs() >= 15.0,
        "gap_lt_15": df["emdiff"].abs() < 15.0,
    }


def neutral_label_diagnostic(df: pd.DataFrame) -> dict:
    """Is `games.is_neutral_site` telling the truth, and does it matter here?

    Shipping `h_neutral = 0` is a claim that a neutral floor is worth nothing,
    and the pooled fit disagrees with it (a stable +0.6..+0.9 residual toward
    the nominal home team). This splits that residual by date to find out
    whether it is a venue effect or a labelling artifact, because the two call
    for opposite decisions: a venue effect should be served, an artifact must
    not be.

    Regresses actual margin on the AdjEM difference within each bucket; the
    intercept is the residual home advantage the label failed to remove.
    """
    opens = pd.to_datetime(df["season"].map(lambda s: f"{int(s) - 1}-11-01"))
    day = (pd.to_datetime(df["game_date"]) - opens).dt.days
    sub = df[df["is_neutral_site"].astype(bool)]
    day = day[df["is_neutral_site"].astype(bool)]
    out = {}
    for name, mask in (("early_pre_dec_15", day < 44), ("mid_season", day >= 44)):
        rows = sub[mask.to_numpy()]
        if len(rows) < 50:
            continue
        x = np.column_stack(
            [rows["emdiff"].to_numpy(dtype=float), np.ones(len(rows))]
        )
        beta = _ols(x, rows["actual"].to_numpy(dtype=float))
        out[name] = {
            "n": int(len(rows)),
            "slope": round(float(beta[0]), 3),
            "residual_home_advantage": round(float(beta[1]), 3),
        }
    return out


def run(df: pd.DataFrame, label: str) -> dict:
    preds: dict[str, np.ndarray] = {k: np.full(len(df), np.nan) for k in CANDIDATES}
    sigmas: dict[str, list] = {k: [] for k in CANDIDATES}
    params_by_fold: dict[int, dict] = {}

    for season, train_mask, test_mask in folds(df["season"], WALK_FROM):
        train, test = df[train_mask], df[test_mask]
        params_by_fold[season] = {}
        for name, fit in CANDIDATES.items():
            p = fit(train)
            preds[name][test_mask.to_numpy()] = apply_params(test, p)
            # Sigma is fit on the TRAINING seasons' own held-in predictions —
            # never on the fold being scored.
            s = fit_sigma(apply_params(train, p), train["actual"].to_numpy(dtype=float))
            sigmas[name].append(s)
            params_by_fold[season][name] = {**{k: round(v, 4) for k, v in p.items()},
                                            "sigma": round(s, 2)}

    scored = df[~np.isnan(preds["raw"])].copy()
    idx = ~np.isnan(preds["raw"])
    actual = scored["actual"].to_numpy(dtype=float)
    coh = cohorts(scored)

    out: dict = {"label": label, "n_scored": int(len(scored)),
                 "seasons_scored": sorted(int(s) for s in scored["season"].unique()),
                 "neutral_label_diagnostic": neutral_label_diagnostic(scored),
                 "params_by_fold": params_by_fold, "candidates": {}}

    base_err = np.abs(preds["raw"][idx] - actual)
    for name in CANDIDATES:
        p = preds[name][idx]
        sig = float(np.mean(sigmas[name]))
        ece, table = reliability(p, actual, sig)
        entry = {
            "sigma_walk_forward_mean": round(sig, 2),
            "sigma_per_fold": [round(x, 2) for x in sigmas[name]],
            "brier_at_fitted_sigma": round(brier(p, actual, sig), 5),
            "brier_at_sigma_prod": round(brier(p, actual, SIGMA_PROD), 5),
            "brier_at_sigma_pit": round(brier(p, actual, SIGMA_PIT), 5),
            "logloss_at_fitted_sigma": round(logloss(p, actual, sig), 5),
            "logloss_at_sigma_prod": round(logloss(p, actual, SIGMA_PROD), 5),
            "logloss_at_sigma_pit": round(logloss(p, actual, SIGMA_PIT), 5),
            "ece_at_fitted_sigma": round(ece, 4),
            "ece_at_sigma_prod": round(reliability(p, actual, SIGMA_PROD)[0], 4),
            "ece_at_sigma_pit": round(reliability(p, actual, SIGMA_PIT)[0], 4),
            "logloss_at_shipped_sigma": round(logloss(p, actual, SHIPPED_SIGMA), 5),
            "ece_at_shipped_sigma": round(reliability(p, actual, SHIPPED_SIGMA)[0], 4),
            "reliability_at_fitted_sigma": table,
            "cohorts": {},
            "by_season": {},
        }
        for cname, mask in coh.items():
            m = mask.to_numpy()
            if m.sum() == 0:
                continue
            b = block(actual[m], p[m])
            if name != "raw":
                z, wins = paired_z(base_err[m], np.abs(p[m] - actual[m]))
                b["paired_z_vs_raw"] = round(float(z), 2)
                b["pct_closer_than_raw"] = round(float(wins), 4)
            entry["cohorts"][cname] = {k: (round(v, 4) if isinstance(v, float) else v)
                                       for k, v in b.items()}
        for s in out["seasons_scored"]:
            m = (scored["season"] == s).to_numpy()
            entry["by_season"][str(s)] = {k: (round(v, 4) if isinstance(v, float) else v)
                                          for k, v in block(actual[m], p[m]).items()}
        out["candidates"][name] = entry
    return out


# --------------------------------------------------------------------- data
def load_served(engine) -> pd.DataFrame:
    df = pd.read_sql(GAME_QUERY, engine)
    df["is_neutral_site"] = df["is_neutral_site"].astype(bool)
    return df.reset_index(drop=True)


def load_from_dump(engine, dump: Path) -> pd.DataFrame:
    """Same games, but the projection is the EX-ANTE backtest dump's served
    prediction rather than the served table's (in-sample at Layer 2) row.

    Joined on `(season, team_name)`, not on the dump's `team_id`: team uuids
    are season-scoped, and a dump row's uuid is its BASE-season team row while
    its `season` is the season being projected, so keying on the pair matches
    nothing. The name join resolves all 3,625 rows; asserted below, because a
    silent miss here would leave the sensitivity check scoring an empty frame
    and reporting it as agreement.
    """
    rows = json.loads(dump.read_text())["teams"]
    names = pd.read_sql(
        "SELECT season, natstat_id, COALESCE(short_name, name) AS nm FROM teams", engine)
    by_name = {(int(t.season), t.nm): t.natstat_id for t in names.itertuples()}

    proj: dict[tuple[int, str], float] = {}
    unresolved = 0
    for r in rows:
        key = by_name.get((int(r["season"]), r["team_name"]))
        if key is None:
            unresolved += 1
            continue
        proj[(int(r["season"]), key)] = served_prediction(r)
    if unresolved > len(rows) // 20:
        raise SystemExit(
            f"{unresolved} of {len(rows)} dump rows did not resolve to a team-season; "
            "the name join has drifted and the sensitivity run would be vacuous")

    df = pd.read_sql(GAME_QUERY_NO_PROJ, engine)
    h = [proj.get((int(s), n)) for s, n in zip(df["season"], df["home_natstat_id"])]
    a = [proj.get((int(s), n)) for s, n in zip(df["season"], df["away_natstat_id"])]
    df["emdiff"] = [None if (x is None or y is None) else x - y for x, y in zip(h, a)]
    df = df[df["emdiff"].notna()].copy()
    if df.empty:
        raise SystemExit("no game had both sides projected in the dump")
    df["emdiff"] = df["emdiff"].astype(float)
    df["is_neutral_site"] = df["is_neutral_site"].astype(bool)
    return df.reset_index(drop=True)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dump", type=Path,
                    help="backtest per-team dump for the --from-dump sensitivity run")
    ap.add_argument("--from-dump", action="store_true",
                    help="also run on the ex-ante dump projection (needs --dump)")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    engine = get_engine()
    result = {"generated_at": dt.datetime.now(dt.UTC).isoformat(),
              "walk_from": WALK_FROM,
              "served_form": {"slope": SERVED_SLOPE, "hca": SERVED_HCA},
              "runs": []}

    served = load_served(engine)
    print(f"served table: {len(served)} games, seasons "
          f"{served['season'].min()}..{served['season'].max()}")
    result["runs"].append(run(served, "served_table"))

    if args.from_dump:
        if not args.dump:
            raise SystemExit("--from-dump needs --dump <backtest per-team json>")
        ex_ante = load_from_dump(engine, args.dump)
        print(f"ex-ante dump: {len(ex_ante)} games")
        result["runs"].append(run(ex_ante, f"ex_ante_dump:{args.dump.name}"))

    for r in result["runs"]:
        print(f"\n=== {r['label']} (n={r['n_scored']}, seasons {r['seasons_scored']})")
        for bucket, d in r["neutral_label_diagnostic"].items():
            print(f"    neutral label, {bucket:16s} n={d['n']:5d} slope {d['slope']:.3f} "
                  f"residual home advantage {d['residual_home_advantage']:+.2f}")
        print(f"{'candidate':12s} {'slope':>7s} {'hca':>6s} {'sigma':>6s} "
              f"{'rmse':>7s} {'mae':>7s} {'bias':>7s} {'z':>7s} {'logloss':>8s} {'ece':>6s}")
        for name, e in r["candidates"].items():
            last = r["params_by_fold"][max(r["params_by_fold"])][name]
            p = e["cohorts"]["pooled"]
            print(f"{name:12s} {last['slope']:7.3f} {last['h_home']:6.2f} "
                  f"{e['sigma_walk_forward_mean']:6.2f} {p['rmse']:7.3f} {p['mae']:7.3f} "
                  f"{p['bias']:7.3f} {p.get('paired_z_vs_raw', 0.0):7.2f} "
                  f"{e['logloss_at_fitted_sigma']:8.5f} {e['ece_at_fitted_sigma']:6.4f}")

    out = args.out or (EVAL_DIR /
                       f"preseason_margin_calibration_{dt.date.today():%Y%m%d}_summary.json")
    out.write_text(json.dumps(result, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
