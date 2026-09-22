"""Do roster CAM-O / CAM-D aggregates help the team O/D projection?

The served split is NET + SPLIT: one calibrator predicts next-season AdjEM,
a second predicts AdjO on the SAME 27 net-CAM features, and AdjD is derived
as AdjO − AdjEM. None of the 27 features carries any offence/defence
information — `cam_o` / `cam_d` are not inputs — so the O-vs-D allocation of
a projection comes entirely from the two program anchors. Saint Mary's 2027
is the shape that exposes it: a #76 raw roster and a new head coach project
to the #10 defence, because the net is anchored to a defence-first program
and the offence half is not told anything different.

Every prior O/D experiment kept the 27 features fixed and varied the
architecture (`validation/exp_team_adjod_projection.py`, LOSO;
`experiment_od_decomposition.py`, walk-forward, #369). This is the other
axis: same architecture, more information.

Two ex-ante O/D signals per rotation player, both known in August:

  prior  the player's own prior-season `cam_o_gbpm_v3_psos` /
         `cam_d_gbpm_v3_psos` (returners and arrivals; recruits have none)
  split  his PROJECTED net (the OOF the frame already carries) allocated
         by his prior-season magnitude share  o / (|o| + |d|), so the
         projection and the split agree on the level; a player with no
         prior (recruit) is split at the rotation's own weighted share,
         which adds no O/D information for him by construction

aggregated the way the calibrator aggregates net CAM (canonical-MPG
weighted mean, sum, top-3 mean), plus the rotation's offence share and its
O/D coverage. Variants on the AdjO half (relative target, the served
design), a DIRECT AdjD half (relative), and the net:

  base       the 27 served features
  +prior     + camo_prior_wmean / camd_prior_wmean / camo_prior_top3 /
               camd_prior_top3 / od_prior_share / od_coverage
  +split     + projo_wmean / projd_wmean / projo_sum / projd_sum
  +both      union

Frame: the trainer's SQL frame (`build_sql_dataset`, ex-post composition)
for the 27 features and the new aggregates on the SAME rotation, with the
blend inputs (`baseline`, `program_level`, `retained`) merged from the
served ex-ante frame. Ex-post presence makes the in-frame numbers
optimistic (`top_band_experiment.py` measured that skew at 0.30); this is a
paired A/B on identical rows, so the DELTA is what is read, and a win here
licenses adding the aggregates to the Rust frame writer and re-judging on
the served composition — not a ship.

Judged walk-forward (train < S, test S, S = 2021..2026; #361) through the
served blend for each half (`blend_half`, as in #369), paired |error| z
against `base`, pooled and on the cohorts the question is about: overhaul
rosters (retained < 0.40), teams whose prior-season defence ranked top-25,
and teams whose prior-season offence ranked top-25.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_od_inputs.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import datetime as dt
import json
import math

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.linear_model import LinearRegression

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import program_anchor, served_prediction, served_weight
from train_roster_impact_model import (
    CANONICAL_ROTATION_MPG,
    LINEAR_FEATURES,
    PLAYER_QUERY,
    SEASONS,
    build_dataset,
    build_sql_dataset,
    lgb_params,
)
from walk_forward import WALK_FROM, folds, paired_z

N_ESTIMATORS = 157   # the AdjO trainer's export budget (meta.final_n_estimators)
LEVEL_SEASONS = 3
_NEG = -1e9

# The trainer's player query with the prior-season O/D columns added. The
# query already joins `tps_prev` (the prior-season Torvik row through the
# cross-season torvik_pid) for the archetype; the same row carries the split.
OD_PLAYER_QUERY = PLAYER_QUERY.replace(
    "    pa.primary_class,\n",
    "    pa.primary_class,\n"
    "    tps_prev.cam_o_gbpm_v3_psos AS prior_cam_o,\n"
    "    tps_prev.cam_d_gbpm_v3_psos AS prior_cam_d,\n",
)
assert "prior_cam_o" in OD_PLAYER_QUERY, "PLAYER_QUERY moved its landmark; re-anchor"

OD_TEAM_QUERY = """
WITH tss AS (
    SELECT t.natstat_id, s.season, s.adj_offense, s.adj_defense,
           rank() OVER (PARTITION BY s.season ORDER BY s.adj_defense ASC)  AS d_rank,
           rank() OVER (PARTITION BY s.season ORDER BY s.adj_offense DESC) AS o_rank
    FROM team_season_stats s JOIN teams t ON t.id = s.team_id
    WHERE s.adj_offense IS NOT NULL
), league AS (
    SELECT season, avg(adj_offense) AS mean_o, avg(adj_defense) AS mean_d FROM tss GROUP BY season
)
SELECT tgt.id AS team_id, cur.season,
       cur.adj_offense AS actual_o, cur.adj_defense AS actual_d,
       base.adj_offense AS baseline_o, base.adj_defense AS baseline_d,
       base.d_rank AS baseline_d_rank, base.o_rank AS baseline_o_rank,
       lb.mean_o AS league_o_base, lb.mean_d AS league_d_base,
       (SELECT avg(p.adj_offense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_o,
       (SELECT count(*) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_n,
       (SELECT avg(p.adj_defense) FROM tss p WHERE p.natstat_id = cur.natstat_id
          AND p.season BETWEEN cur.season - 1 - %(lvl)s AND cur.season - 2) AS level_d
FROM tss cur
JOIN teams tgt ON tgt.natstat_id = cur.natstat_id AND tgt.season = cur.season
JOIN tss base ON base.natstat_id = cur.natstat_id AND base.season = cur.season - 1
JOIN league lb ON lb.season = cur.season - 1
"""

PRIOR_COLS = ["camo_prior_wmean", "camd_prior_wmean", "camo_prior_top3", "camd_prior_top3",
              "od_prior_share", "od_coverage"]
SPLIT_COLS = ["projo_wmean", "projd_wmean", "projo_sum", "projd_sum"]


def od_aggregates(group: pd.DataFrame) -> pd.Series:
    """The new aggregates over the SAME rotation `aggregate_team_season`
    keeps: rank by projected net, top 13, canonical MPG by rank."""
    g = group.copy()
    g["_rank_key"] = g["campom"].fillna(_NEG)
    g["_tiebreak"] = g["player_id"].astype(str)
    g = g.sort_values(["_rank_key", "_tiebreak"], ascending=[False, True], kind="stable").reset_index(drop=True)
    g = g.head(len(CANONICAL_ROTATION_MPG))
    g["w"] = [CANONICAL_ROTATION_MPG[i] for i in range(len(g))]
    row: dict[str, float] = {}

    has = g.dropna(subset=["prior_cam_o", "prior_cam_d"])
    ws = float(has["w"].sum())
    tot = float(g["w"].sum())
    row["od_coverage"] = ws / tot if tot > 0 else 0.0
    if len(has) == 0:
        for k in PRIOR_COLS[:5]:
            row[k] = np.nan
        share = np.nan
    else:
        o, d, w = has["prior_cam_o"].astype(float), has["prior_cam_d"].astype(float), has["w"].astype(float)
        row["camo_prior_wmean"] = float((o * w).sum() / ws)
        row["camd_prior_wmean"] = float((d * w).sum() / ws)
        row["camo_prior_top3"] = float(o.sort_values(ascending=False).head(3).mean())
        row["camd_prior_top3"] = float(d.sort_values(ascending=False).head(3).mean())
        mag = float((o.abs() * w).sum() + (d.abs() * w).sum())
        share = float((o.abs() * w).sum() / mag) if mag > 0 else 0.5
        row["od_prior_share"] = share

    # Projected split: each player's projected net at his own prior magnitude
    # share; a player with no prior takes the rotation's share, which gives
    # him the rotation's O/D mix and no information of his own.
    proj = g.dropna(subset=["campom"])
    if len(proj) == 0 or (isinstance(share, float) and math.isnan(share)):
        for k in SPLIT_COLS:
            row[k] = np.nan
        return pd.Series(row)
    po = proj["prior_cam_o"].astype(float)
    pdd = proj["prior_cam_d"].astype(float)
    denom = po.abs() + pdd.abs()
    own = (po.abs() / denom).where(denom > 0, share).fillna(share)
    net = proj["campom"].astype(float)
    # Keep the sign of each half: a player whose net is +6 with prior shares
    # (o=+4, d=+2) projects (+4, +2); one with (o=+8, d=-2) projects the
    # same magnitudes signed as his prior halves.
    sign_o = np.sign(po).replace(0, 1).fillna(1.0)
    sign_d = np.sign(pdd).replace(0, 1).fillna(1.0)
    proj_o = net.abs() * own * sign_o
    proj_d = net.abs() * (1.0 - own) * sign_d
    # For a player whose net and halves disagree in sign the magnitudes above
    # still sum to |net|; recentre so proj_o + proj_d == net exactly.
    adj = (net - (proj_o + proj_d)) / 2.0
    proj_o, proj_d = proj_o + adj, proj_d + adj
    w = proj["w"].astype(float)
    row["projo_wmean"] = float((proj_o * w).sum() / float(w.sum()))
    row["projd_wmean"] = float((proj_d * w).sum() / float(w.sum()))
    row["projo_sum"] = float(proj_o.sum())
    row["projd_sum"] = float(proj_d.sum())
    return pd.Series(row)


def build_frame() -> tuple[pd.DataFrame, list[str]]:
    engine = get_engine()
    df, cols, _ = build_sql_dataset()
    players = pd.read_sql(OD_PLAYER_QUERY, engine, params={"seasons": list(SEASONS)})
    players = players[players["campom_source"] != "actual_fallback"].reset_index(drop=True)
    od = (players.groupby(["team_id", "season"], as_index=False)
                 .apply(od_aggregates, include_groups=False).reset_index(drop=True))
    df["team_id"] = df["team_id"].astype(str)
    od["team_id"] = od["team_id"].astype(str)
    df = df.merge(od, on=["team_id", "season"], how="left")

    # Blend inputs from the served ex-ante frame; targets/baselines for the halves.
    ex_ante, _, _ = build_dataset()
    ex_ante["team_id"] = ex_ante["team_id"].astype(str)
    df = df.merge(ex_ante[["team_id", "season", "team_name", "baseline", "program_level", "retained"]],
                  on=["team_id", "season"], how="inner")
    t = pd.read_sql(OD_TEAM_QUERY, engine, params={"lvl": LEVEL_SEASONS})
    t["team_id"] = t["team_id"].astype(str)
    t.loc[t["level_n"] < 2, ["level_o", "level_d"]] = np.nan
    df = df.merge(t, on=["team_id", "season"], how="inner").reset_index(drop=True)
    cov = df["od_coverage"].describe()
    print(f"frame {len(df)} team-seasons; O/D coverage of the rotation: mean {cov['mean']:.2f}, "
          f"min {cov['min']:.2f}; rows with any prior O/D: {int(df['camo_prior_wmean'].notna().sum())}")
    return df, cols


def wf_lgb(df: pd.DataFrame, cols: list[str], target: pd.Series) -> pd.Series:
    p = lgb_params()
    p.pop("early_stopping_rounds", None)
    p["n_estimators"] = N_ESTIMATORS
    out = pd.Series(np.nan, index=df.index, dtype=float)
    for _, tr, te in folds(df["season"], WALK_FROM):
        out.loc[te] = lgb.LGBMRegressor(**p).fit(df.loc[tr, cols], target[tr]).predict(df.loc[te, cols])
    return out


def wf_ols(df: pd.DataFrame, cols: list[str], target: pd.Series) -> pd.Series:
    x = df[cols].fillna(0.0)
    out = pd.Series(np.nan, index=df.index, dtype=float)
    for _, tr, te in folds(df["season"], WALK_FROM):
        out.loc[te] = LinearRegression().fit(x.loc[tr], target[tr]).predict(x.loc[te])
    return out


def blend_half(raw: float, baseline: float, level, retained) -> float:
    anchored = level is not None and not (isinstance(level, float) and np.isnan(level))
    lvl = float(level) if anchored else None
    ret = None if retained is None or (isinstance(retained, float) and np.isnan(retained)) else float(retained)
    w = served_weight(ret, anchored)
    return w * program_anchor(baseline, lvl, raw) + (1.0 - w) * raw


def report(label: str, df: pd.DataFrame, preds: dict[str, pd.Series], actual: pd.Series,
           cohorts: dict[str, pd.Series], reference: str) -> dict:
    test = df["season"] >= WALK_FROM
    names = list(preds)
    print(f"\n=== {label}, served, walk-forward — MAE (bias) | paired vs {reference}: Δ|err| z ===")
    out: dict = {}
    for cname, mask in [("pooled", test)] + [(k, test & v) for k, v in cohorts.items()]:
        rows = [{"actual": float(actual[i]), "i": int(i)} for i in df.index[mask]]
        out[cname] = {"n": len(rows)}
        line = f"{cname:<26}{len(rows):>5}"
        ref = {id(r): float(preds[reference][r["i"]]) for r in rows}
        for v in names:
            p = {id(r): float(preds[v][r["i"]]) for r in rows}
            e = np.array([p[id(r)] - r["actual"] for r in rows])
            m, b = float(np.abs(e).mean()), float(e.mean())
            d, z = paired_z(rows, p, ref) if v != reference else (0.0, 0.0)
            out[cname][v] = {"mae": m, "bias": b, "delta": d, "z": z}
            line += f"  {v:>7}: {m:6.3f} ({b:+5.2f}) {d:+.3f} z{z:+5.1f}"
        print(line)
    return out


def main() -> None:
    df, base_cols = build_frame()
    variants = {
        "base": base_cols,
        "+prior": base_cols + PRIOR_COLS,
        "+split": base_cols + SPLIT_COLS,
        "+both": base_cols + PRIOR_COLS + SPLIT_COLS,
    }
    y_net = df["adj_efficiency_margin"]
    rel_o = df["actual_o"] - df["league_o_base"]
    rel_d = df["actual_d"] - df["league_d_base"]
    test = df["season"] >= WALK_FROM

    cohorts = {
        "overhaul (<0.4 retained)": df["retained"].fillna(1.0) < 0.40,
        "prior top-25 defence": df["baseline_d_rank"] <= 25,
        "prior top-25 offence": df["baseline_o_rank"] <= 25,
        "era>=2024": df["season"] >= 2024,
    }
    for season in sorted(df.loc[test, "season"].unique()):
        cohorts[f"season {int(season)}"] = df["season"] == season

    # --- the served net, fixed across variants (OLS on the 3 CAM aggregates)
    raw_net_base = wf_ols(df, list(LINEAR_FEATURES), y_net)
    served_net = pd.Series({i: served_prediction({"actual": float(y_net[i]), "baseline": float(df.baseline[i]),
                                                  "program_level": None if pd.isna(df.program_level[i]) else float(df.program_level[i]),
                                                  "retained": None if pd.isna(df.retained[i]) else float(df.retained[i]),
                                                  "roster_proj": float(raw_net_base[i]), "season": int(df.season[i])})
                            for i in df.index[test]})

    # --- net with O/D added to the linear calibrator (does it help the headline?)
    net_preds = {"base": served_net}
    for name, extra in (("+prior", PRIOR_COLS), ("+split", SPLIT_COLS)):
        raw = wf_ols(df, list(LINEAR_FEATURES) + extra, y_net)
        net_preds[name] = pd.Series({i: served_prediction({"actual": float(y_net[i]), "baseline": float(df.baseline[i]),
                                                            "program_level": None if pd.isna(df.program_level[i]) else float(df.program_level[i]),
                                                            "retained": None if pd.isna(df.retained[i]) else float(df.retained[i]),
                                                            "roster_proj": float(raw[i]), "season": int(df.season[i])})
                                     for i in df.index[test]})
    net_table = report("NET (OLS, served blend)", df, net_preds, y_net, cohorts, "base")

    # --- AdjO half (relative target, served design), AdjD derived and direct
    o_preds, d_derived, d_direct = {}, {}, {}
    for name, cols in variants.items():
        raw_o = wf_lgb(df, cols, rel_o) + df["league_o_base"]
        raw_d = wf_lgb(df, cols, rel_d) + df["league_d_base"]
        o_preds[name] = pd.Series({i: blend_half(float(raw_o[i]), float(df.baseline_o[i]), df.level_o[i], df.retained[i])
                                   for i in df.index[test]})
        d_derived[name] = o_preds[name] - served_net
        d_direct[name] = pd.Series({i: blend_half(float(raw_d[i]), float(df.baseline_d[i]), df.level_d[i], df.retained[i])
                                    for i in df.index[test]})
    o_table = report("AdjO (relative, LightGBM)", df, o_preds, df["actual_o"], cohorts, "base")
    dd_table = report("AdjD derived = AdjO − served net", df, d_derived, df["actual_d"], cohorts, "base")
    dx_table = report("AdjD direct (relative, LightGBM)", df, d_direct, df["actual_d"], cohorts, "base")

    # Gain share of the new features in the AdjO fit, on the full frame.
    p = lgb_params(); p.pop("early_stopping_rounds", None); p["n_estimators"] = N_ESTIMATORS
    m = lgb.LGBMRegressor(**p).fit(df[variants["+both"]], rel_o)
    gain = pd.Series(m.booster_.feature_importance("gain"), index=variants["+both"])
    gain = gain / gain.sum()
    print("\nAdjO gain share of the new features (full-frame fit):")
    for k in PRIOR_COLS + SPLIT_COLS:
        print(f"  {k:<18} {gain[k]:.3f}")

    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "n": int(test.sum()),
               "walk_from": WALK_FROM, "variants": {k: v for k, v in variants.items()},
               "net": net_table, "adjo": o_table, "adjd_derived": dd_table, "adjd_direct": dx_table,
               "adjo_gain_share": {k: float(gain[k]) for k in PRIOR_COLS + SPLIT_COLS}}
    out = EVAL_DIR / f"od_inputs_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
