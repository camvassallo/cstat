"""Should the served net AdjEM be O_hat − D_hat instead of a direct net model?
And should the AdjO split be trained relative to the league's scoring
environment? Judged walk-forward (#361), through the served blend for BOTH
halves, so the answer is about what ships.

The served design when this ran (2026-09-21) was NET + SPLIT: one calibrator
predicts next-season AdjEM directly (load-bearing), a second predicts AdjO on
the same 27 features (display), and AdjD is derived as AdjO − AdjEM. #378
later replaced the derived half with its own model reconciled to the net;
the net verdict below is unaffected and still the reason the net stays direct. Each half is program-anchored
and blended at the same turnover-aware weight toward its own base-season
value (`projections.rs`: `anchor` / `anchor_o`). The alternative — two
independent O and D models summed — was measured LOSO in
`validation/exp_team_adjod_projection.py` and lost by a hair (+0.008 net),
because team-level O and D errors are positively correlated and cancel in
O − D. That verdict was LOSO; #361's scorecard then found the AdjO half has
a real forward gap (walk-forward 4.48 vs LOSO 4.10) that is a season-level
bias: league AdjO rose ~7 points 2021→2026 and the model, trained on absolute
AdjO from CAM features that carry no scoring-environment signal, is centred
on the 11-season mean. LOSO interpolates that drift; walk-forward has to
extrapolate it. So the question is re-asked forward-chained, with the
obvious fix in the mix.

Variants, all walk-forward (train < S, test S, S = 2021..2026), all the
trainer's params with the export protocol (fixed iterations), identical
features, frame = `frames/roster_impact_ex_ante.json` (the served
composition):

  net: direct           the served design — one net model
  net: O−D              independent absolute-O and absolute-D models, summed
  net: O−D relative     the same with each target taken relative to its
                        base-season league mean (mean added back)
  adjo: direct abs      the served AdjO model
  adjo: relative        target = AdjO − league mean AdjO(base season); the
                        base-season mean is added back at serve. Ex-ante:
                        the base season is complete in August.
  adjd: derived         served AdjO − served net (the shipped path), per
                        AdjO variant
  adjd: direct abs/rel  a D model of its own, blended like the others

Every raw prediction goes through the served blend for its half (net:
`served_prediction`; O and D: the same weight and program anchor against the
half's own baseline and 3-season level), so "served" means served. Reported:
the net cohort table + rank metrics with paired z against the direct net;
per-season MAE and bias for AdjO and AdjD.

Run:  cd training && ./.venv/bin/python experiments/experiment_od_decomposition.py
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

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import program_anchor, served_prediction, served_weight
from train_roster_impact_model import build_dataset, lgb_params
from walk_forward import WALK_FROM, folds, mae_bias, team_table

N_ESTIMATORS = 157   # the shipped calibrator's final-fit budget (meta.final_n_estimators)
LEVEL_SEASONS = 3    # program level = mean over the 3 seasons before the base season, >= 2 of them

OD_QUERY = """
WITH tss AS (
    SELECT t.natstat_id, s.season, s.adj_offense, s.adj_defense
    FROM team_season_stats s JOIN teams t ON t.id = s.team_id
    WHERE s.adj_offense IS NOT NULL
), league AS (
    SELECT season, avg(adj_offense) AS mean_o, avg(adj_defense) AS mean_d FROM tss GROUP BY season
)
SELECT tgt.id AS team_id, cur.season,
       cur.adj_offense AS actual_o, cur.adj_defense AS actual_d,
       base.adj_offense AS baseline_o, base.adj_defense AS baseline_d,
       lb.mean_o AS league_o_base, lb.mean_d AS league_d_base,
       (SELECT avg(p.adj_offense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_o,
       (SELECT count(*) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_o_n,
       (SELECT avg(p.adj_defense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_d
FROM tss cur
JOIN teams tgt ON tgt.natstat_id = cur.natstat_id AND tgt.season = cur.season
JOIN tss base ON base.natstat_id = cur.natstat_id AND base.season = cur.season - 1
JOIN league lb ON lb.season = cur.season - 1
"""


def load_od(df: pd.DataFrame) -> pd.DataFrame:
    od = pd.read_sql(OD_QUERY, get_engine(), params={"lvl": LEVEL_SEASONS})
    od["team_id"] = od["team_id"].astype(str)
    od.loc[od["level_o_n"] < 2, ["level_o", "level_d"]] = np.nan
    out = df.merge(od, on=["team_id", "season"], how="inner")
    print(f"frame {len(df)} rows; with O/D targets and baselines: {len(out)}")
    return out.reset_index(drop=True)


def wf_predict(df: pd.DataFrame, cols: list[str], target: pd.Series) -> pd.Series:
    p = lgb_params()
    p.pop("early_stopping_rounds", None)
    p["n_estimators"] = N_ESTIMATORS
    out = pd.Series(np.nan, index=df.index, dtype=float)
    for s, tr, te in folds(df["season"], WALK_FROM):
        out.loc[te] = lgb.LGBMRegressor(**p).fit(df.loc[tr, cols], target[tr]).predict(df.loc[te, cols])
    return out


def blend_half(raw: float, baseline: float, level: float | None, retained: float | None) -> float:
    """The served path for one half: program anchor on that half's own
    baseline/level, blended at the turnover-aware weight (`projections.rs`
    `anchor_o`). `retained`/anchored semantics identical to the net."""
    anchored = level is not None and not (isinstance(level, float) and np.isnan(level))
    lvl = float(level) if anchored else None
    w = served_weight(retained, anchored)
    return w * program_anchor(baseline, lvl, raw) + (1.0 - w) * raw


def main() -> None:
    df, cols, _ = build_dataset()
    df = load_od(df)
    y_net, y_o, y_d = df["adj_efficiency_margin"], df["actual_o"], df["actual_d"]
    rel_o = y_o - df["league_o_base"]
    rel_d = y_d - df["league_d_base"]

    print("\nwalk-forward fits (6 folds each): net, O abs, D abs, O rel, D rel …")
    raw = {
        "net": wf_predict(df, cols, y_net),
        "o_abs": wf_predict(df, cols, y_o),
        "d_abs": wf_predict(df, cols, y_d),
        "o_rel": wf_predict(df, cols, rel_o) + df["league_o_base"],
        "d_rel": wf_predict(df, cols, rel_d) + df["league_d_base"],
    }
    test = df["season"] >= WALK_FROM
    rows = []
    for i in df.index[test]:
        r = df.loc[i]
        rows.append({
            "i": int(i), "season": int(r.season), "team": r.team_name, "actual": float(r.adj_efficiency_margin),
            "baseline": float(r.baseline), "retained": None if pd.isna(r.retained) else float(r.retained),
            "program_level": None if pd.isna(r.program_level) else float(r.program_level),
        })

    # --- net variants, through the served net blend
    def served_net(raw_net):
        return {id(r): served_prediction({**r, "roster_proj": float(raw_net[r["i"]])}) for r in rows}

    net_preds = {
        "net: direct": served_net(raw["net"]),
        "net: O−D": served_net(raw["o_abs"] - raw["d_abs"]),
        "net: O−D relative": served_net(raw["o_rel"] - raw["d_rel"]),
    }
    print("\n=== NET, served, walk-forward ===")
    net_table = team_table(rows, net_preds, reference="net: direct")

    # --- AdjO / AdjD variants, through the served half-blend
    def served_half(raw_half, base_col, level_col):
        return {id(r): blend_half(float(raw_half[r["i"]]), float(df.loc[r["i"], base_col]),
                                  df.loc[r["i"], level_col], r["retained"]) for r in rows}

    o_preds = {"adjo: direct abs": served_half(raw["o_abs"], "baseline_o", "level_o"),
               "adjo: relative": served_half(raw["o_rel"], "baseline_o", "level_o")}
    d_preds = {"adjd: derived (abs O − net)": {id(r): o_preds["adjo: direct abs"][id(r)] - net_preds["net: direct"][id(r)] for r in rows},
               "adjd: derived (rel O − net)": {id(r): o_preds["adjo: relative"][id(r)] - net_preds["net: direct"][id(r)] for r in rows},
               "adjd: direct abs": served_half(raw["d_abs"], "baseline_d", "level_d"),
               "adjd: direct relative": served_half(raw["d_rel"], "baseline_d", "level_d")}

    def half_report(label, preds, actual_col):
        print(f"\n=== {label}, served, walk-forward (MAE  bias) ===")
        names = list(preds)
        print(f"{'season':<10}" + "".join(f"{v:>30}" for v in names))
        out = {}
        for s in sorted({r["season"] for r in rows}) + ["pooled"]:
            rs = rows if s == "pooled" else [r for r in rows if r["season"] == s]
            line = f"{str(s):<10}"
            out[str(s)] = {}
            for v in names:
                e = [preds[v][id(r)] - float(df.loc[r["i"], actual_col]) for r in rs]
                m, b = float(np.mean(np.abs(e))), float(np.mean(e))
                out[str(s)][v] = {"mae": m, "bias": b}
                line += f"{m:>22.3f}{b:>+8.2f}"
            print(line)
        return out

    o_table = half_report("AdjO", o_preds, "actual_o")
    d_table = half_report("AdjD", d_preds, "actual_d")

    # Raw (unblended) halves too, so the era bias is visible before the blend hides 70% of it.
    print("\nraw (unblended) per-season bias, AdjO abs vs relative:")
    for s in sorted({r["season"] for r in rows}):
        m = (df["season"] == s)
        print(f"  {s}: abs {float((raw['o_abs'][m] - y_o[m]).mean()):+.2f}   rel {float((raw['o_rel'][m] - y_o[m]).mean()):+.2f}"
              f"   | league mean O base→target {float(df.loc[m, 'league_o_base'].iloc[0]):.2f}→{float(y_o[m].mean()):.2f}")

    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "n": len(rows), "walk_from": WALK_FROM,
               "n_estimators": N_ESTIMATORS, "net": net_table, "adjo": o_table, "adjd": d_table,
               "raw_o_bias_by_season": {str(s): {"abs": float((raw["o_abs"][df["season"] == s] - y_o[df["season"] == s]).mean()),
                                                 "rel": float((raw["o_rel"][df["season"] == s] - y_o[df["season"] == s]).mean())}
                                        for s in sorted({r["season"] for r in rows})}}
    out = EVAL_DIR / f"od_decomposition_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
