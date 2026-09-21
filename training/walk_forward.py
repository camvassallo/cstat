"""Walk-forward evaluation — the one harness every trainer in the tree reports
through (#361).

Every model here forecasts a season from what was known before it. Leave-one-
season-out (and its pair/class variants) trains on seasons AFTER the one being
scored, which is the wrong direction for a forecast: the number it prints is
optimistic, and a variant can win LOSO and lose forward-chained (#359 and #360
both found that shape). Walk-forward — train on seasons < S, test on S — is
the honest protocol, and this module makes it the canonical one:

  - `folds(seasons, walk_from)` yields (S, train_mask, test_mask) with every
    training row strictly earlier than the test season;
  - `regression_walk_forward` runs a fit/predict callback over those folds
    and returns held-out predictions plus per-season and pooled MAE / RMSE /
    R² in the same shape the LOSO blocks already use, so a meta carries
    `walk_forward` beside `backtest_loso` / `backtest_lopo` and the two are
    read side by side;
  - the team-level metric set — cohort MAE and bias, paired z, field
    Spearman, membership@25, rho within the actual and the predicted top 25,
    pairwise concordance in the actual top 50, worst miss in the predicted
    top 10 — is what `top_band_experiment.py` and `eb_blend_experiment.py`
    converged on, kept here so no trainer or experiment grows its own.

Test seasons start at `WALK_FROM` (2021): the frames start at 2016 so the
first fold has five training seasons behind it, and 2021+ is the portal era
the served number is judged on. LOSO stays in every meta as the second
number, for continuity with every prior diagnostic.
"""

from __future__ import annotations

import math
from collections.abc import Callable, Iterator
from itertools import combinations

import numpy as np
import pandas as pd
from scipy.stats import spearmanr
from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score

WALK_FROM = 2021


# ------------------------------------------------------------------ folds
def folds(seasons: pd.Series, walk_from: int = WALK_FROM) -> Iterator[tuple[int, pd.Series, pd.Series]]:
    """(test_season, train_mask, test_mask) for every season >= walk_from that
    has at least one earlier season to train on. Masks are boolean Series
    aligned to `seasons.index`."""
    for s in sorted(int(x) for x in seasons.unique()):
        if s < walk_from:
            continue
        train = seasons < s
        test = seasons == s
        if train.any() and test.any():
            yield s, train, test


def _block(y: np.ndarray, p: np.ndarray) -> dict:
    return {
        "mae": float(mean_absolute_error(y, p)),
        "rmse": float(np.sqrt(mean_squared_error(y, p))),
        "r2": float(r2_score(y, p)) if len(y) > 1 else float("nan"),
        "bias": float(np.mean(p - y)),
        "n": int(len(y)),
    }


def regression_walk_forward(
    df: pd.DataFrame,
    seasons: pd.Series,
    target: pd.Series,
    fit_predict: Callable[[pd.DataFrame, pd.Series, pd.DataFrame], np.ndarray],
    walk_from: int = WALK_FROM,
    label: str = "walk-forward",
) -> tuple[dict, pd.Series]:
    """Run `fit_predict(train_X, train_y, test_X)` per fold.

    Returns `({"per_season": {S: block}, "pooled": block, "walk_from": ...},
    preds)` where `preds` is aligned to `df.index` and NaN before `walk_from`.
    `df` is the feature frame only — pass seasons and target as Series so the
    caller decides what the season key is (a target season, a recruit year + 1).
    """
    preds = pd.Series(np.nan, index=df.index, dtype=float)
    per_season: dict[str, dict] = {}
    for s, tr, te in folds(seasons, walk_from):
        p = np.asarray(fit_predict(df[tr], target[tr], df[te]), dtype=float)
        preds.loc[te] = p
        per_season[str(s)] = _block(target[te].to_numpy(), p)
        b = per_season[str(s)]
        print(f"  {label} {s}: MAE {b['mae']:.3f}  RMSE {b['rmse']:.3f}  R² {b['r2']:.3f}  n={b['n']}  (train n={int(tr.sum())})")
    scored = preds.notna()
    pooled = _block(target[scored].to_numpy(), preds[scored].to_numpy()) if scored.any() else {}
    if pooled:
        print(f"  {label} pooled ({walk_from}+): MAE {pooled['mae']:.3f}  RMSE {pooled['rmse']:.3f}  R² {pooled['r2']:.3f}  n={pooled['n']}")
    return {"walk_from": walk_from, "per_season": per_season, "pooled": pooled}, preds


def loso_on_same_rows(loso_preds: pd.Series, target: pd.Series, seasons: pd.Series, walk_from: int = WALK_FROM) -> dict:
    """The LOSO/LOPO/LOCO number restricted to the walk-forward test rows, so
    the optimism of training on later seasons is read directly: this minus
    the walk-forward pooled MAE, on identical rows."""
    m = (seasons >= walk_from) & loso_preds.notna()
    if not m.any():
        return {}
    return _block(target[m].to_numpy(), loso_preds[m].to_numpy())


# ------------------------------------------------- team-level metric set
# Rows are dicts with at least `season`, `actual`, `baseline`; predictions are
# {id(row): value}. Everything below is per season then averaged, because the
# top-25 questions only make sense within a season.
def mae_bias(rows: list[dict], pred: dict) -> tuple[float, float]:
    e = [pred[id(r)] - r["actual"] for r in rows if id(r) in pred]
    if not e:
        return float("nan"), float("nan")
    return sum(abs(x) for x in e) / len(e), sum(e) / len(e)


def paired_z(rows: list[dict], a: dict, b: dict) -> tuple[float, float]:
    """Mean |err_a| − |err_b| and its z; negative = a is better."""
    d = [abs(a[id(r)] - r["actual"]) - abs(b[id(r)] - r["actual"]) for r in rows if id(r) in a and id(r) in b]
    if len(d) < 2:
        return float("nan"), float("nan")
    m = sum(d) / len(d)
    sd = math.sqrt(sum((x - m) ** 2 for x in d) / (len(d) - 1))
    return m, (m / (sd / math.sqrt(len(d))) if sd > 0 else 0.0)


def top_n_ex_ante(rows: list[dict], n: int) -> list[dict]:
    """Top-n by PRIOR-season AdjEM (`baseline`), per season — the ex-ante
    elite cohort, chosen without the outcome."""
    out: list[dict] = []
    for s in sorted({r["season"] for r in rows}):
        out += sorted((r for r in rows if r["season"] == s), key=lambda r: -r["baseline"])[:n]
    return out


def per_season(rows: list[dict], pred: dict, fn: Callable, min_rows: int = 50) -> float:
    vals = []
    for s in sorted({r["season"] for r in rows}):
        rs = [r for r in rows if r["season"] == s and id(r) in pred]
        if len(rs) >= min_rows:
            vals.append(fn(rs, pred))
    return float(np.mean(vals)) if vals else float("nan")


def membership25(rs: list[dict], pred: dict) -> float:
    """|predicted top-25 ∩ actual top-25| / 25 — precision == recall at equal k."""
    p = {id(r) for r in sorted(rs, key=lambda r: -pred[id(r)])[:25]}
    a = {id(r) for r in sorted(rs, key=lambda r: -r["actual"])[:25]}
    return len(p & a) / 25


def rho_actual_top25(rs: list[dict], pred: dict) -> float:
    """Spearman inside the ACTUAL end-of-year top 25 — did we order the teams
    that ended up good; rank-based, so a common shift cannot game it."""
    top = sorted(rs, key=lambda r: -r["actual"])[:25]
    return float(spearmanr([pred[id(r)] for r in top], [r["actual"] for r in top]).correlation)


def rho_pred_top25(rs: list[dict], pred: dict) -> float:
    """Spearman inside OUR predicted top 25 — the list the site publishes."""
    top = sorted(rs, key=lambda r: -pred[id(r)])[:25]
    return float(spearmanr([pred[id(r)] for r in top], [r["actual"] for r in top]).correlation)


def rho_field(rs: list[dict], pred: dict) -> float:
    return float(spearmanr([pred[id(r)] for r in rs], [r["actual"] for r in rs]).correlation)


def concordance_top50(rs: list[dict], pred: dict) -> float:
    """Among the actual top 50, the fraction of pairs whose predicted order
    matches their actual order — "team X is better than team Y", directly."""
    top = sorted(rs, key=lambda r: -r["actual"])[:50]
    ok = tot = 0
    for a, b in combinations(top, 2):
        d = (pred[id(a)] - pred[id(b)]) * (a["actual"] - b["actual"])
        if d != 0:
            tot += 1
            ok += d > 0
    return ok / tot if tot else float("nan")


def worst_miss_top10(rs: list[dict], pred: dict) -> float:
    top = sorted(rs, key=lambda r: -pred[id(r)])[:10]
    return max(abs(pred[id(r)] - r["actual"]) for r in top)


RANK_METRICS: tuple[tuple[str, Callable], ...] = (
    ("membership@25", membership25),
    ("rho(actual T25)", rho_actual_top25),
    ("rho(pred T25)", rho_pred_top25),
    ("concordance T50", concordance_top50),
    ("worst miss T10", worst_miss_top10),
    ("rho(field)", rho_field),
)


def standard_cohorts(rows: list[dict]) -> dict[str, list[dict]]:
    """The cohort table every projection diagnostic since #326 has printed."""
    c: dict[str, list[dict]] = {
        "all": rows,
        "era>=2024": [r for r in rows if r["season"] >= 2024],
        "top50 (ex-ante)": top_n_ex_ante(rows, 50),
        "top25 (ex-ante)": top_n_ex_ante(rows, 25),
        "top10 (ex-ante)": top_n_ex_ante(rows, 10),
        "top10 era>=2024": [r for r in top_n_ex_ante(rows, 10) if r["season"] >= 2024],
        "overhaul (<0.2)": [r for r in rows if r.get("retained") is not None and r["retained"] < 0.2],
        "level-changers |dev|>=15": [
            r for r in rows if r.get("program_level") is not None and abs(r["baseline"] - r["program_level"]) >= 15
        ],
    }
    return c


def team_table(rows: list[dict], preds: dict[str, dict], reference: str | None = None, print_it: bool = True) -> dict:
    """Cohort MAE/bias per variant, the rank metric set, and paired z against
    `reference` (a key of `preds`). Returns a JSON-ready dict; prints the
    table when asked."""
    names = list(preds)
    common = [r for r in rows if all(id(r) in p for p in preds.values())]
    cohorts = standard_cohorts(common)
    out: dict = {"n": len(common), "variants": names, "cohorts": {}, "rank_metrics": {}, "paired": {}}
    for cname, crows in cohorts.items():
        out["cohorts"][cname] = {"n": len(crows)}
        for v in names:
            m, b = mae_bias(crows, preds[v])
            out["cohorts"][cname][v] = {"mae": m, "bias": b}
    for label, fn in RANK_METRICS:
        out["rank_metrics"][label] = {v: per_season(common, preds[v], fn) for v in names}
    if reference is not None:
        for v in names:
            if v == reference:
                continue
            out["paired"][v] = {}
            for lab, rs in (("pooled", common), ("top25", cohorts["top25 (ex-ante)"]), ("top10", cohorts["top10 (ex-ante)"]),
                            ("level-changers", cohorts["level-changers |dev|>=15"])):
                d, z = paired_z(rs, preds[v], preds[reference])
                out["paired"][v][lab] = {"delta": d, "z": z}
    if print_it:
        print(f"\n{'cohort':<26}{'n':>5}" + "".join(f"{v:>18}" for v in names))
        for cname, block in out["cohorts"].items():
            print(f"{cname:<26}{block['n']:>5}" + "".join(f"{block[v]['mae']:>10.3f}{block[v]['bias']:>+8.2f}" for v in names))
        print("(cell: MAE  bias)")
        print(f"\n{'metric (per-season mean)':<31}" + "".join(f"{v:>18}" for v in names))
        for label, _ in RANK_METRICS:
            print(f"{label:<31}" + "".join(f"{out['rank_metrics'][label][v]:>18.3f}" for v in names))
        if reference is not None:
            print(f"\npaired |err| vs {reference} (delta, z; negative = better): pooled | top-25 | top-10 | level-changers")
            for v, blocks in out["paired"].items():
                print(f"  {v:<16}" + " | ".join(f"{b['delta']:+.3f} z={b['z']:+.2f}" for b in blocks.values()))
    return out
