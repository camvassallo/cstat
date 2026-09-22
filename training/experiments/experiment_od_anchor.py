"""Layer 4 on the O/D split: should the AdjO half be anchored as hard as the
net, and should AdjD be derived or modelled?

The served split is NET + SPLIT: AdjEM from the net calibrator, AdjO from a
second model on the same 27 features, AdjD = AdjO − AdjEM. Each half is
program-anchored and blended at the SAME turnover-aware weight
(`projections.rs`: `anchor` / `anchor_o`, `served_weight`). Nothing in the
27 features carries offence/defence information (`experiment_od_inputs.py`
measured that adding it buys under 1%), so the O-vs-D allocation of a
projection is the two anchors'. On an overhaul or new-coach roster at a
program with a strong identity, that hands the whole program premium to
whichever half history says — Saint Mary's 2027: #76 raw roster, new head
coach, #10 projected defence, because the net is anchored to a defence-first
program and the offence half is anchored to a modest one.

This asks whether the O half's anchor weight should differ from the net's,
judged walk-forward through the served blend, and re-asks direct-vs-derived
AdjD with the net headline held fixed.

AdjO variants (all on the same raw relative-target model, #368):

  served        w = served_weight(retained), the net's ramp (0.70 / 0.55)
  refit         (w_stable, w_overhaul) for the O half re-searched inside each
                fold on earlier walk-forward rows — the `constants_refit`
                protocol the net trainer runs (#361), applied to the O half
  scale 0.75    w_o = 0.75 · w          (both cohorts)
  scale 0.50    w_o = 0.50 · w
  overhaul 0.2  w_stable unchanged, w_overhaul_o = 0.20 (the unanchored pair
                on overhauls only)
  new-hc unanch a new head coach's roster takes the UNANCHORED pair
                (0.30 / 0.20) on the O half — "the program is not the coach"
  raw           w = 0

AdjD variants:

  derived       served AdjO variant − served net (the shipped path), one per
                AdjO variant above
  direct        a relative AdjD model of its own, blended on its own anchor at
                the served weights (`experiment_od_decomposition.py` found
                this 0.08 better than derived, LOSO and WF, but it does not
                reconcile with the net)
  direct refit  the same with the D half's weights refit per fold
  reconciled    both direct halves, then the net residual r = net − (O − D)
                split evenly: O' = O + r/2, D' = D − r/2, so O' − D' == the
                served net exactly. Keeps the headline, uses both models.

Reported: MAE / bias and paired |error| z against `served` (O) and against
`derived served` (D), pooled, per season, and on the cohorts the question is
about — overhaul rosters (retained < 0.40), new head coach, prior-season
top-25 defence, prior-season top-25 offence, 2024+. Plus the net identity
gap |O − D − net| for the direct variants, which is the cost NET + SPLIT
avoids.

Frame: the served ex-ante composition (`build_dataset`,
`frames/roster_impact_ex_ante.json`), so these are the honest numbers, not a
screen. Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_od_anchor.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import datetime as dt
import itertools
import json
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.linear_model import LinearRegression

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import (
    W_OVERHAUL,
    W_OVERHAUL_UNANCHORED,
    W_STABLE,
    W_STABLE_UNANCHORED,
    program_anchor,
    served_prediction,
    served_weight,
)
from train_roster_impact_model import LINEAR_FEATURES, build_dataset, lgb_params
from walk_forward import WALK_FROM, folds, paired_z

RAW_WALK_FROM = 2019   # raw halves start here so each fold has >= 2 earlier walk-forward seasons to refit on
LEVEL_SEASONS = 3
GRID_STABLE = [round(x, 2) for x in np.arange(0.20, 0.95, 0.05)]
GRID_OVERHAUL = [round(x, 2) for x in np.arange(0.10, 0.85, 0.05)]

OD_QUERY = """
WITH tss AS (
    SELECT t.natstat_id, s.season, s.adj_offense, s.adj_defense,
           rank() OVER (PARTITION BY s.season ORDER BY s.adj_defense ASC)  AS d_rank,
           rank() OVER (PARTITION BY s.season ORDER BY s.adj_offense DESC) AS o_rank
    FROM team_season_stats s JOIN teams t ON t.id = s.team_id
    WHERE s.adj_offense IS NOT NULL
), league AS (
    SELECT season, avg(adj_offense) AS mean_o, avg(adj_defense) AS mean_d FROM tss GROUP BY season
)
SELECT tgt.id AS team_id, cur.season, cur.natstat_id,
       cur.adj_offense AS actual_o, cur.adj_defense AS actual_d,
       base.adj_offense AS baseline_o, base.adj_defense AS baseline_d,
       base.d_rank AS baseline_d_rank, base.o_rank AS baseline_o_rank,
       lb.mean_o AS league_o_base, lb.mean_d AS league_d_base,
       (SELECT avg(p.adj_offense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_o,
       (SELECT count(*) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_n,
       (SELECT avg(p.adj_defense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_d,
       cs.is_new_hc
FROM tss cur
JOIN teams tgt ON tgt.natstat_id = cur.natstat_id AND tgt.season = cur.season
JOIN tss base ON base.natstat_id = cur.natstat_id AND base.season = cur.season - 1
JOIN league lb ON lb.season = cur.season - 1
LEFT JOIN coach_seasons cs ON cs.team_natstat_id = cur.natstat_id AND cs.season = cur.season
"""


def n_estimators_from_meta() -> int:
    meta = Path(__file__).resolve().parent.parent / "models" / "roster_adjo_model_meta.json"
    return int(json.loads(meta.read_text()).get("final_n_estimators") or 157)


def load_frame() -> tuple[pd.DataFrame, list[str]]:
    df, cols, _ = build_dataset()
    od = pd.read_sql(OD_QUERY, get_engine(), params={"lvl": LEVEL_SEASONS})
    od["team_id"] = od["team_id"].astype(str)
    od.loc[od["level_n"] < 2, ["level_o", "level_d"]] = np.nan
    df["team_id"] = df["team_id"].astype(str)
    out = df.merge(od, on=["team_id", "season"], how="inner").reset_index(drop=True)
    print(f"frame {len(df)} rows; with O/D targets, anchors and coach flag: {len(out)}; "
          f"new-HC rows {int((out['is_new_hc'] == True).sum())}")
    return out, cols


def wf_lgb(df: pd.DataFrame, cols: list[str], target: pd.Series, n_est: int, walk_from: int) -> pd.Series:
    p = lgb_params()
    p.pop("early_stopping_rounds", None)
    p["n_estimators"] = n_est
    out = pd.Series(np.nan, index=df.index, dtype=float)
    for _, tr, te in folds(df["season"], walk_from):
        out.loc[te] = lgb.LGBMRegressor(**p).fit(df.loc[tr, cols], target[tr]).predict(df.loc[te, cols])
    return out


def wf_ols(df: pd.DataFrame, cols: list[str], target: pd.Series, walk_from: int) -> pd.Series:
    x = df[cols].fillna(0.0)
    out = pd.Series(np.nan, index=df.index, dtype=float)
    for _, tr, te in folds(df["season"], walk_from):
        out.loc[te] = LinearRegression().fit(x.loc[tr], target[tr]).predict(x.loc[te])
    return out


def _clean(v):
    return None if v is None or (isinstance(v, float) and np.isnan(v)) else float(v)


def blend_half(raw: float, baseline: float, level, retained, w_stable: float, w_overhaul: float,
               force_unanchored_pair: bool = False, scale: float = 1.0) -> float:
    """The served half-blend with the weights as parameters. `program_anchor`
    keeps its corroboration gate; only the weight moves."""
    anchored = _clean(level) is not None
    ret = _clean(retained)
    if force_unanchored_pair:
        w = served_weight(ret, anchored=False)
    else:
        w = served_weight(ret, anchored, w_stable=w_stable, w_overhaul=w_overhaul)
    w *= scale
    return w * program_anchor(baseline, _clean(level), raw) + (1.0 - w) * raw


def half_series(df: pd.DataFrame, raw: pd.Series, base_col: str, level_col: str, idx,
                w_stable: float = W_STABLE, w_overhaul: float = W_OVERHAUL, **kw) -> pd.Series:
    return pd.Series({i: blend_half(float(raw[i]), float(df.loc[i, base_col]), df.loc[i, level_col],
                                    df.loc[i, "retained"], w_stable, w_overhaul, **kw) for i in idx})


def refit_half(df: pd.DataFrame, raw: pd.Series, actual: pd.Series, base_col: str, level_col: str) -> tuple[pd.Series, dict]:
    """Per fold S: pick (w_stable, w_overhaul) minimising the half's MAE on
    walk-forward rows with RAW_WALK_FROM <= season < S, apply to S."""
    out = pd.Series(np.nan, index=df.index, dtype=float)
    picks: dict = {}
    for s in sorted(df.loc[df["season"] >= WALK_FROM, "season"].unique()):
        pool = df.index[(df["season"] >= RAW_WALK_FROM) & (df["season"] < s)]
        best = None
        for ws, wo in itertools.product(GRID_STABLE, GRID_OVERHAUL):
            if wo > ws:
                continue
            pred = half_series(df, raw, base_col, level_col, pool, ws, wo)
            m = float((pred - actual[pool]).abs().mean())
            if best is None or m < best[0]:
                best = (m, ws, wo)
        _, ws, wo = best
        te = df.index[df["season"] == s]
        out.loc[te] = half_series(df, raw, base_col, level_col, te, ws, wo)
        picks[str(int(s))] = {"w_stable": ws, "w_overhaul": wo, "pool_mae": best[0]}
    return out, picks


def report(label: str, df: pd.DataFrame, preds: dict[str, pd.Series], actual: pd.Series,
           cohorts: dict[str, pd.Series], reference: str) -> dict:
    test = df["season"] >= WALK_FROM
    names = list(preds)
    print(f"\n=== {label}, walk-forward — MAE (bias) | paired Δ|err| z vs `{reference}` ===")
    out: dict = {}
    for cname, mask in [("pooled", test)] + [(k, test & v) for k, v in cohorts.items()]:
        rows = [{"actual": float(actual[i]), "i": int(i)} for i in df.index[mask]]
        if not rows:
            continue
        out[cname] = {"n": len(rows)}
        ref = {id(r): float(preds[reference][r["i"]]) for r in rows}
        line = f"{cname:<24}{len(rows):>5}"
        for v in names:
            p = {id(r): float(preds[v][r["i"]]) for r in rows}
            e = np.array([p[id(r)] - r["actual"] for r in rows])
            m, b = float(np.abs(e).mean()), float(e.mean())
            d, z = paired_z(rows, p, ref) if v != reference else (0.0, 0.0)
            out[cname][v] = {"mae": m, "bias": b, "delta": d, "z": z}
            line += f" | {v}: {m:.3f} ({b:+.2f}) {d:+.3f} z{z:+.1f}"
        print(line)
    return out


def main() -> None:
    df, cols = load_frame()
    n_est = n_estimators_from_meta()
    y_net, y_o, y_d = df["adj_efficiency_margin"], df["actual_o"], df["actual_d"]
    test = df["season"] >= WALK_FROM
    idx = df.index[test]

    print(f"walk-forward raw fits from {RAW_WALK_FROM} (n_estimators={n_est}) …")
    raw_net = wf_ols(df, list(LINEAR_FEATURES), y_net, RAW_WALK_FROM)
    raw_o = wf_lgb(df, cols, y_o - df["league_o_base"], n_est, RAW_WALK_FROM) + df["league_o_base"]
    raw_d = wf_lgb(df, cols, y_d - df["league_d_base"], n_est, RAW_WALK_FROM) + df["league_d_base"]

    served_net = pd.Series({i: served_prediction({"actual": float(y_net[i]), "baseline": float(df.baseline[i]),
                                                  "program_level": _clean(df.program_level[i]),
                                                  "retained": _clean(df.retained[i]),
                                                  "roster_proj": float(raw_net[i]), "season": int(df.season[i])})
                            for i in idx})

    new_hc = df["is_new_hc"] == True  # noqa: E712 - nullable boolean column
    cohorts = {
        "overhaul (<0.4)": df["retained"].fillna(1.0) < 0.40,
        "new HC": new_hc,
        "prior top-25 D": df["baseline_d_rank"] <= 25,
        "prior top-25 O": df["baseline_o_rank"] <= 25,
        "era>=2024": df["season"] >= 2024,
    }
    for season in sorted(df.loc[test, "season"].unique()):
        cohorts[f"season {int(season)}"] = df["season"] == season

    # --- AdjO variants
    o_refit, o_picks = refit_half(df, raw_o, y_o, "baseline_o", "level_o")
    o_preds = {
        "served": half_series(df, raw_o, "baseline_o", "level_o", idx),
        "refit": o_refit.loc[idx],
        "scale 0.75": half_series(df, raw_o, "baseline_o", "level_o", idx, scale=0.75),
        "scale 0.50": half_series(df, raw_o, "baseline_o", "level_o", idx, scale=0.50),
        "overhaul 0.2": half_series(df, raw_o, "baseline_o", "level_o", idx, W_STABLE, 0.20),
        "raw": raw_o.loc[idx],
    }
    # new-HC rows on the unanchored pair; everyone else served.
    nh = half_series(df, raw_o, "baseline_o", "level_o", idx, force_unanchored_pair=True)
    o_preds["new-hc unanch"] = pd.Series({i: (nh[i] if bool(new_hc[i]) else o_preds["served"][i]) for i in idx})
    print("\nO-half refit picks per fold (served 0.70 / 0.55):")
    for s, p in o_picks.items():
        print(f"  {s}: w_stable {p['w_stable']:.2f}  w_overhaul {p['w_overhaul']:.2f}  (pool MAE {p['pool_mae']:.3f})")
    o_table = report("AdjO", df, o_preds, y_o, cohorts, "served")

    # --- AdjD: derived from each O variant, direct, direct refit, reconciled
    d_refit, d_picks = refit_half(df, raw_d, y_d, "baseline_d", "level_d")
    d_direct = half_series(df, raw_d, "baseline_d", "level_d", idx)
    d_preds = {f"derived {k}": v - served_net for k, v in o_preds.items()}
    d_preds["direct"] = d_direct
    d_preds["direct refit"] = d_refit.loc[idx]
    resid = served_net - (o_preds["served"] - d_direct)
    o_recon = o_preds["served"] + resid / 2.0
    d_recon = d_direct - resid / 2.0
    d_preds["reconciled"] = d_recon
    o_preds_recon = {"served": o_preds["served"], "reconciled": o_recon}
    print("\nD-half refit picks per fold (served 0.70 / 0.55):")
    for s, p in d_picks.items():
        print(f"  {s}: w_stable {p['w_stable']:.2f}  w_overhaul {p['w_overhaul']:.2f}  (pool MAE {p['pool_mae']:.3f})")
    d_table = report("AdjD", df, d_preds, y_d, cohorts, "derived served")
    o_recon_table = report("AdjO, reconciled form", df, o_preds_recon, y_o, cohorts, "served")
    gap = float((o_preds["served"] - d_direct - served_net).abs().mean())
    print(f"\nnet identity gap |O − D_direct − net|, mean over test rows: {gap:.3f} "
          f"(derived and reconciled forms: 0 by construction)")

    summary = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "n": int(test.sum()), "walk_from": WALK_FROM, "raw_walk_from": RAW_WALK_FROM, "n_estimators": n_est,
        "served_weights": {"w_stable": W_STABLE, "w_overhaul": W_OVERHAUL,
                           "unanchored": [W_STABLE_UNANCHORED, W_OVERHAUL_UNANCHORED]},
        "adjo": o_table, "adjo_refit_picks": o_picks,
        "adjd": d_table, "adjd_refit_picks": d_picks, "adjo_reconciled": o_recon_table,
        "net_identity_gap_direct": gap,
    }
    out = EVAL_DIR / f"od_anchor_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
