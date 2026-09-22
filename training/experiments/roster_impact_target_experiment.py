"""Roster-impact target experiment — does the calibrator's ceiling come from
regressing on the LEVEL, and does a residual target remove it?

The served roster-impact model's maximum output has been 32-35 AdjEM in every
season of the backtest, p99 about 30, while actual maxima went from 37-41
(2018-2024) to 46.6 and 44.7 (2025-2026). A tree ensemble cannot predict
outside the range of its training targets, and with `min_child_samples=20`
the top leaf is the mean of the ~20 strongest historical rosters — so the
top of a stretched 2025-26 distribution is unreachable by construction. On
the 2026 fold the raw model missed Duke by -12, Arizona by -15, Michigan by
-12, Illinois by -13; no blend constant recovers that (see
`program_anchor_era_diagnostic.py`).

Same frame, same features, same fixed iteration budget (no early stopping
on the held-out fold, so no target gets a leak the others do not), three
targets, leave-one-season-out:

  level      adj_em                          (the shipped target)
  d_base     adj_em - baseline               (prior season, same program)
  d_level    adj_em - program_level          (mean of the 3 prior seasons,
                                              baseline when fewer than 2)
  d_base_nf  adj_em - baseline, shipped features only (no baseline feature)
  level_lf   adj_em, with the two level features added
  lin_only   OLS on cam_sum / cam_wmean / cam_top7_mean / baseline / level
  lin_tree   that linear stage, plus a tree fit on its residual (the
             candidate that reached the end-to-end backtest — a pooled tie)

The residual variants also get `baseline` / `program_level` as features,
so the tree can still learn reversion; the point is that the LEVEL enters
the prediction linearly (pred = anchor + tree) and the ceiling goes away.
Each raw prediction is then pushed through the served blend so the number
that matters — what the board would show — is what gets compared.

This is the trainer's SAME-SEASON frame (the actual roster with projected
cam), not the end-to-end served composition, so the MAEs read lower than
`projections-backtest`. It tests the mechanism; a win here is the licence
to retrain the LOSO set and rerun the end-to-end backtest, not the result.

Run:  cd training && ./.venv/bin/python experiments/roster_impact_target_experiment.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import datetime as dt
import json

import lightgbm as lgb
import numpy as np
import pandas as pd
from sqlalchemy import text

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import W_STABLE, W_STABLE_UNANCHORED, program_anchor
from train_roster_impact_model import build_dataset, lgb_params

START_YEAR = 2018
N_ESTIMATORS = 400          # fixed; the trainer's LOSO best-iterations land 250-500
TOP_N = (10, 25)


def load_anchors(conn) -> pd.DataFrame:
    """baseline (prior season) and 3-season program level per target team-season."""
    q = text("""
        WITH em AS (
            SELECT t.natstat_id AS ns, t.season, t.id AS team_id, tss.adj_efficiency_margin AS em
            FROM teams t JOIN team_season_stats tss ON tss.team_id = t.id
            WHERE tss.adj_efficiency_margin IS NOT NULL
        )
        SELECT cur.team_id::text AS team_id, cur.season,
               prev.em AS baseline,
               (SELECT AVG(h.em) FROM em h
                 WHERE h.ns = cur.ns AND h.season BETWEEN cur.season - 4 AND cur.season - 2
                 HAVING count(*) >= 2) AS program_level
        FROM em cur
        LEFT JOIN em prev ON prev.ns = cur.ns AND prev.season = cur.season - 1
    """)
    return pd.read_sql(q, conn)


def blend_served(raw, baseline, level, w=W_STABLE):
    if pd.isna(baseline):
        return raw
    if pd.isna(level):
        return W_STABLE_UNANCHORED * baseline + (1 - W_STABLE_UNANCHORED) * raw
    a = program_anchor(float(baseline), float(level), float(raw))
    return w * a + (1 - w) * raw


def blend_old(raw, baseline):
    if pd.isna(baseline):
        return raw
    return 0.30 * baseline + 0.70 * raw


def main():
    df, feature_cols, _ = build_dataset()
    with get_engine().connect() as conn:
        anchors = load_anchors(conn)
        names = pd.read_sql(text("SELECT id::text AS team_id, name FROM teams"), conn)
    df["team_id"] = df["team_id"].astype(str)
    df = df.merge(anchors, on=["team_id", "season"], how="left").merge(names, on="team_id", how="left")
    df = df[df["baseline"].notna()].reset_index(drop=True)
    df["anchor_level"] = df["program_level"].fillna(df["baseline"])
    print(f"frame: {len(df)} team-seasons with a baseline; {df['program_level'].notna().sum()} with a program level")

    variants = {
        "level":   dict(target=lambda d: d["adj_efficiency_margin"], offset=lambda d: 0.0,
                        feats=feature_cols),
        "d_base":  dict(target=lambda d: d["adj_efficiency_margin"] - d["baseline"],
                        offset=lambda d: d["baseline"], feats=feature_cols + ["baseline", "anchor_level"]),
        "d_level": dict(target=lambda d: d["adj_efficiency_margin"] - d["anchor_level"],
                        offset=lambda d: d["anchor_level"], feats=feature_cols + ["baseline", "anchor_level"]),
        # Residual target with the SHIPPED feature vector — no new features, so
        # the Rust side would change only at inference (add the baseline back).
        "d_base_nf": dict(target=lambda d: d["adj_efficiency_margin"] - d["baseline"],
                          offset=lambda d: d["baseline"], feats=feature_cols),
        # Level target WITH the level features: the trees can split on the
        # baseline, so the top leaf averages high-baseline teams only.
        "level_lf": dict(target=lambda d: d["adj_efficiency_margin"], offset=lambda d: 0.0,
                         feats=feature_cols + ["baseline", "anchor_level"]),
    }
    params = lgb_params()
    params.pop("early_stopping_rounds", None)
    params["n_estimators"] = N_ESTIMATORS

    seasons = [s for s in sorted(df["season"].unique()) if s >= START_YEAR]
    preds = {v: pd.Series(np.nan, index=df.index) for v in variants}
    for v, spec in variants.items():
        for s in seasons:
            tr = df[df["season"] != s]
            te = df[df["season"] == s]
            m = lgb.LGBMRegressor(**params)
            m.fit(tr[spec["feats"]], spec["target"](tr))
            preds[v].loc[te.index] = m.predict(te[spec["feats"]]) + spec["offset"](te)

    # Linear stage + tree on its residual: OLS on the talent aggregates and
    # the two levels carries the Σcam ≈ AdjEM identity and extrapolates; the
    # tree only corrects what is left. The data sets the baseline weight
    # instead of the residual target pinning it at 1.0.
    from sklearn.linear_model import LinearRegression
    LIN = ["cam_sum", "cam_wmean", "cam_top7_mean", "baseline", "anchor_level"]
    lf = feature_cols + ["baseline", "anchor_level"]
    preds["lin_tree"] = pd.Series(np.nan, index=df.index)
    preds["lin_only"] = pd.Series(np.nan, index=df.index)
    lin_coefs = []
    for s in seasons:
        tr = df[df["season"] != s]
        te = df[df["season"] == s]
        lin = LinearRegression().fit(tr[LIN], tr["adj_efficiency_margin"])
        lin_coefs.append(dict(zip(LIN, lin.coef_.round(3))))
        resid = tr["adj_efficiency_margin"] - lin.predict(tr[LIN])
        m = lgb.LGBMRegressor(**params).fit(tr[lf], resid)
        preds["lin_only"].loc[te.index] = lin.predict(te[LIN])
        preds["lin_tree"].loc[te.index] = lin.predict(te[LIN]) + m.predict(te[lf])
    variants["lin_only"] = dict(feats=lf)
    variants["lin_tree"] = dict(feats=lf)
    print("linear-stage coefficients per fold:", *lin_coefs, sep="\n  ")
    df = df[df["season"] >= START_YEAR].copy()
    for v in variants:
        df[f"raw_{v}"] = preds[v].loc[df.index]
        df[f"served_{v}"] = [blend_served(r, b, l) for r, b, l in
                             zip(df[f"raw_{v}"], df["baseline"], df["program_level"])]
        df[f"old_{v}"] = [blend_old(r, b) for r, b in zip(df[f"raw_{v}"], df["baseline"])]

    def top(d, n):
        return d.groupby("season", group_keys=False).apply(lambda g: g.nlargest(n, "baseline"))

    cohorts = {"all": df, "era<=2023": df[df.season <= 2023], "era>=2024": df[df.season >= 2024]}
    for n in TOP_N:
        cohorts[f"top{n}"] = top(df, n)
        cohorts[f"top{n} era>=2024"] = top(df[df.season >= 2024], n)
    cols = [f"{k}_{v}" for v in variants for k in ("raw", "served", "old")]
    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "n": int(len(df)),
               "n_estimators": N_ESTIMATORS, "cohorts": {}, "ceiling": {}, "named_2026": {}}

    print(f"\n{'cohort':<18}{'n':>6} " + "".join(f"{c:>16}" for c in cols))
    for cname, d in cohorts.items():
        line = f"{cname:<18}{len(d):>6} "
        summary["cohorts"][cname] = {}
        for c in cols:
            e = d[c] - d["adj_efficiency_margin"]
            summary["cohorts"][cname][c] = {"mae": float(e.abs().mean()), "bias": float(e.mean())}
            line += f"{e.abs().mean():>9.3f}{e.mean():>+7.2f}"
        print(line)
    print("(each cell: MAE  bias)")

    print("\nceiling: per-season max raw prediction vs max actual")
    print(f"{'yr':>5}{'max act':>9}" + "".join(f"{'max '+v:>13}" for v in variants) +
          "".join(f"{'top10 '+v:>13}" for v in variants) + f"{'top10 act':>11}")
    for s in seasons:
        d = df[df.season == s]
        t10 = d.nlargest(10, "adj_efficiency_margin")
        summary["ceiling"][str(s)] = {"max_actual": float(d["adj_efficiency_margin"].max()),
                                      **{f"max_raw_{v}": float(d[f'raw_{v}'].max()) for v in variants}}
        print(f"{s:>5}{d['adj_efficiency_margin'].max():>9.1f}" +
              "".join(f"{d[f'raw_{v}'].max():>13.1f}" for v in variants) +
              "".join(f"{t10[f'raw_{v}'].mean():>13.1f}" for v in variants) +
              f"{t10['adj_efficiency_margin'].mean():>11.1f}")

    print("\n2026, top 12 by actual — served error (pred − actual) per target:")
    d = df[df.season == 2026].nlargest(12, "adj_efficiency_margin")
    print(f"{'team':<16}{'base':>6}{'act':>6}" + "".join(f"{'served_'+v:>15}" for v in variants))
    for _, r in d.iterrows():
        print(f"{r['name'][:15]:<16}{r['baseline']:>6.1f}{r['adj_efficiency_margin']:>6.1f}" +
              "".join(f"{r[f'served_{v}'] - r['adj_efficiency_margin']:>+15.2f}" for v in variants))
        summary["named_2026"][r["name"]] = {v: float(r[f"served_{v}"] - r["adj_efficiency_margin"]) for v in variants}

    out = EVAL_DIR / f"roster_impact_target_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
