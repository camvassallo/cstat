"""The elite gap after the linear calibrator (#363): where the remaining
under-projection of the best programs lives, and what does NOT move it.

#371 replaced the calibrator's LightGBM (leaf ceiling ~32) with OLS on three
CAM aggregates and took the ex-ante top-10 2024+ bias from −5.9 to −4.1.
This script measures what is left, walk-forward through the served blend
(#361's judge) on the leak-guarded ex-ante frame (#362), in three parts:

  1. Decomposition, per player. Every rotation player on a 2021+ team-season
     with his actual CAM and whichever held-out projection covered him
     (trajectory OOF for returners/transfers, freshman OOF for ranked
     recruits, none for walk-ons / unranked / internationals). Reported two
     ways, because they disagree and the difference is the point:
       - conditioned on the team's ACTUAL AdjEM >= 25 (ex-post): every
         channel reads about −1.1 per player in 2024+ — but that cohort is
         selected on the outcome, and teams that ended great are, by
         construction, the ones whose players beat their projections;
       - conditioned on the DESTINATION's prior-season AdjEM (ex-ante, the
         cohort the projection is judged on): returners/transfers at a
         >= 20 program run −0.4 per player in 2024+ (−0.5 at >= 25), ranked
         freshmen −1.4, top-10 recruits −4.4 each (n=15).
     Over a 13-man rotation the ex-ante number is the −4 that remains.

  2. Calibrator: curvature and era terms on the linear form. Convex terms
     (hinge on cam_wmean, cam_wmean²), star-count features, a season
     intercept, a season × cam_wmean slope, and recency-weighted fits. The
     residual at the top is negative in 2021–23 and positive in 2024+, so
     every convex term fits the old shape and loses forward (top-25 z=+2).
     The season × slope term gains 0.08 at the top-25 (z=−2.7) and nothing
     pooled, on per-season slopes that bounce 4.4–7.1 with no trend — not
     worth a serving-contract change.

  3. Trajectory model: an era feature (target season, alone and interacted
     with destination strength / an elite-destination flag), walk-forward
     on the 65-feature frame. All ties (pooled z −0.9..+0.4; elite-destination
     2024+ z +0.3..+1.0). The destination block already carries what history
     can teach; the portal-era regime is three seasons old.

Verdict: the calibrator half of #363 is closed by #371; the upstream half is
a per-player −0.4 at elite destinations in the portal era plus a handful of
generational recruits, neither learnable from the seasons available. Re-run
this when 2027 lands.

Run:  cd training && ./.venv/bin/python experiment_elite_gap.py
"""

from __future__ import annotations

import datetime as dt
import json

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.linear_model import LinearRegression

from compute_cae import EVAL_DIR
from db import get_engine
from served_blend import served_prediction
from train_roster_impact_model import LINEAR_FEATURES, _rows, build_dataset
from walk_forward import WALK_FROM, folds, team_table

PLAYER_QUERY = """
WITH rot AS (
  SELECT pss.season, pss.team_id, pss.player_id, tps.torvik_pid, tps.cam_gbpm_v3_psos AS actual_cam,
         t.natstat_id AS team_ns, tss.adj_efficiency_margin AS team_em,
         (SELECT b.adj_efficiency_margin FROM team_season_stats b JOIN teams tb ON tb.id = b.team_id
            WHERE tb.natstat_id = t.natstat_id AND tb.season = pss.season - 1) AS dest_prior
  FROM player_season_stats pss
  JOIN torvik_player_stats tps ON tps.player_id = pss.player_id AND tps.season = pss.season
  JOIN teams t ON t.id = pss.team_id
  JOIN team_season_stats tss ON tss.team_id = pss.team_id AND tss.season = pss.season
  WHERE pss.season >= %(from_season)s AND pss.games_played >= 5 AND pss.minutes_per_game >= 5
    AND tps.cam_gbpm_v3_psos IS NOT NULL
)
SELECT r.*, tr.mean AS traj_oof, fr.mean AS fresh_oof, rc.composite_rank,
       (SELECT tp.natstat_id FROM player_season_stats p JOIN torvik_player_stats x ON x.player_id = p.player_id AND x.season = p.season
          JOIN teams tp ON tp.id = p.team_id
          WHERE x.torvik_pid = r.torvik_pid AND p.season = r.season - 1 ORDER BY p.games_played DESC LIMIT 1) AS prior_ns
FROM rot r
LEFT JOIN trajectory_oof_predictions tr ON tr.torvik_pid = r.torvik_pid AND tr.target_season = r.season
LEFT JOIN freshman_oof_predictions fr ON fr.cstat_player_id = r.player_id AND fr.target_season = r.season
LEFT JOIN recruits rc ON rc.cstat_player_id = r.player_id
"""


def decomposition() -> dict:
    df = pd.read_sql(PLAYER_QUERY, get_engine(), params={"from_season": WALK_FROM})

    def channel(r):
        if pd.notna(r.traj_oof) and pd.notna(r.prior_ns):
            return "returner" if r.prior_ns == r.team_ns else "transfer"
        if pd.notna(r.fresh_oof):
            return "freshman"
        return "unprojected"

    df["channel"] = df.apply(channel, axis=1)
    df["proj"] = df.traj_oof.fillna(df.fresh_oof)
    df["err"] = df.proj - df.actual_cam
    df["era"] = np.where(df.season >= 2024, "2024+", "2021-23")
    out: dict = {"n_players": int(len(df))}
    print("per-player bias (proj − actual) by channel, era and cohort:")
    for cohort, mask in (("ex-post: team actual >= 25", df.team_em >= 25),
                         ("ex-ante: destination prior >= 20", df.dest_prior >= 20),
                         ("ex-ante: destination prior >= 25", df.dest_prior >= 25),
                         ("rest", df.dest_prior < 20)):
        out[cohort] = {}
        for era, g in df[mask].groupby("era"):
            out[cohort][era] = {ch: {"bias": float(s.err.mean()), "n": int(s.err.notna().sum())}
                                for ch, s in g.groupby("channel") if s.err.notna().any()}
            out[cohort][era]["unprojected_cam_per_team"] = float(g[g.channel == "unprojected"].actual_cam.sum() / max(g.team_id.nunique(), 1))
            print(f"  {cohort:<34} {era}: " + "  ".join(f"{ch} {v['bias']:+.2f} (n={v['n']})" for ch, v in out[cohort][era].items() if isinstance(v, dict))
                  + f"  unprojected CAM/team {out[cohort][era]['unprojected_cam_per_team']:.1f}")
    f = df[(df.channel == "freshman") & (df.season >= 2024) & (df.dest_prior >= 20)].copy()
    f["rk"] = pd.cut(f.composite_rank, [0, 10, 30, 100, 9999], labels=["1-10", "11-30", "31-100", "100+"])
    out["freshman_by_rank_elite_dest_2024+"] = {str(k): {"bias": float(v.err.mean()), "n": int(len(v))} for k, v in f.groupby("rk", observed=True)}
    print("  ranked freshmen at >= 20 destinations, 2024+, by composite rank:",
          {k: f"{v['bias']:+.2f} (n={v['n']})" for k, v in out["freshman_by_rank_elite_dest_2024+"].items()})
    return out


def calibrator_terms() -> dict:
    df, cols, _ = build_dataset()
    y = df["adj_efficiency_margin"]
    L = list(LINEAR_FEATURES)

    def wf(feats_fn, train_window=None):
        out = pd.Series(np.nan, index=df.index)
        for s, tr, te in folds(df["season"], WALK_FROM):
            if train_window:
                tr = tr & (df.season >= s - train_window)
            out.loc[te] = LinearRegression().fit(feats_fn(df[tr]), y[tr]).predict(feats_fn(df[te]))
        return out

    variants = {
        "lin3 (served)": wf(lambda d: d[L]),
        "+cam_count_gt15": wf(lambda d: d[L + ["cam_count_gt15"]]),
        "+hinge(wmean-4)": wf(lambda d: d[L].assign(h=np.maximum(d.cam_wmean - 4, 0))),
        "+wmean^2": wf(lambda d: d[L].assign(sq=d.cam_wmean ** 2)),
        "+season": wf(lambda d: d[L].assign(t=d.season - 2016)),
        "+season*wmean": wf(lambda d: d[L].assign(tw=(d.season - 2016) * d.cam_wmean)),
        "last 3 seasons": wf(lambda d: d[L], train_window=3),
    }
    rows = [r for r in _rows(df) if r["season"] >= WALK_FROM]
    idx = [i for i, r in zip(df.index, _rows(df)) if r["season"] >= WALK_FROM]
    preds = {v: {id(r): served_prediction({**r, "roster_proj": float(variants[v][i])}) for r, i in zip(rows, idx)} for v in variants}
    print("\ncalibrator terms, served, walk-forward:")
    table = team_table(rows, preds, reference="lin3 (served)")
    slopes = {}
    for s, g in df.groupby("season"):
        m = LinearRegression().fit(g[L], g.adj_efficiency_margin)
        slopes[str(s)] = float(m.coef_[0])
    print("  per-season in-sample slope on cam_wmean:", {k: round(v, 2) for k, v in slopes.items()})
    return {"table": table, "per_season_wmean_slope": slopes}


def trajectory_era() -> dict:
    from train_trajectory_model import FEATURE_COLS, build_dataset as traj_dataset, lgb_params

    df = traj_dataset().reset_index(drop=True)
    y = df["target_campom"]
    df["season_t"] = df["s_np1"] - 2016
    df["dest_x_t"] = df["dest_prior_adj_em"].clip(lower=-30) * df["season_t"]
    df["elite_x_t"] = (df["dest_prior_adj_em"] >= 20).astype(float) * df["season_t"]
    sets = {"base65": list(FEATURE_COLS), "+season": list(FEATURE_COLS) + ["season_t"],
            "+season+dest*t": list(FEATURE_COLS) + ["season_t", "dest_x_t"], "+elite*t": list(FEATURE_COLS) + ["elite_x_t"]}
    preds = {}
    for name, feats in sets.items():
        out = pd.Series(np.nan, index=df.index)
        for s, tr, te in folds(df["s_np1"], WALK_FROM):
            out.loc[te] = lgb.LGBMRegressor(**lgb_params()).fit(df.loc[tr, feats], y[tr]).predict(df.loc[te, feats])
        preds[name] = out
    t = df[df.s_np1 >= WALK_FROM]
    cohorts = {"all 2021+": t.index, "elite dest (>=20) 2024+": t.index[(t.dest_prior_adj_em >= 20) & (t.s_np1 >= 2024)],
               "elite dest (>=25) 2024+": t.index[(t.dest_prior_adj_em >= 25) & (t.s_np1 >= 2024)]}
    out: dict = {}
    print("\ntrajectory era features, walk-forward (MAE, bias; paired z vs base65):")
    for cname, ix in cohorts.items():
        out[cname] = {}
        for n in sets:
            e = preds[n][ix] - y[ix]
            d = (preds[n][ix] - y[ix]).abs() - (preds["base65"][ix] - y[ix]).abs()
            z = float(d.mean() / (d.std(ddof=1) / np.sqrt(len(d)))) if n != "base65" and len(d) > 1 else 0.0
            out[cname][n] = {"mae": float(e.abs().mean()), "bias": float(e.mean()), "z_vs_base": z, "n": int(len(ix))}
        print(f"  {cname:<26}" + "".join(f"  {n}: {out[cname][n]['mae']:.3f} {out[cname][n]['bias']:+.2f} z={out[cname][n]['z_vs_base']:+.2f}" for n in sets))
    return out


def main() -> None:
    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "walk_from": WALK_FROM,
               "decomposition": decomposition(), "calibrator_terms": calibrator_terms(), "trajectory_era": trajectory_era()}
    out = EVAL_DIR / f"elite_gap_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
