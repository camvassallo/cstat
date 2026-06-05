"""
Phase 0 de-risk for the minutes/role projection lever: ORACLE-MINUTES CEILING.

The roster calibrator forces every team's minute vector to be the fixed
CANONICAL_ROTATION_MPG template (train + serve). Question: if we instead knew
each team's ACTUAL minute distribution, would the projection get more accurate
-- especially on the Q1 (bust) composition error (+5.62 per ROADMAP)?

This is the STOP gate: if oracle minutes don't beat canonical, minutes
misallocation isn't the bottleneck and we should NOT build a minutes model.

Three aggregation modes (identical features/LOSO otherwise):
  canonical      -- rank by cam_v3, top 13, CANONICAL_ROTATION_MPG weights [current]
  oracle_weights -- rank by cam_v3, top 13, ACTUAL mpg weights (membership unchanged)
  oracle_full    -- rank by ACTUAL mpg, top 13, ACTUAL mpg weights (membership + weights)

Reports pooled LOSO MAE + per-ACTUAL-AdjEM-quartile MAE/bias (Q1=busts).
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import mean_absolute_error

from db import get_engine
from train_roster_impact_model import (
    TEAM_QUERY, OUTBOUND_QUERY, INBOUND_QUERY, CANONICAL_ROTATION_MPG,
    normalize_class, ARCHETYPES, lgb_params, SEASONS, _NEG,
)

# PLAYER_QUERY + actual minutes_per_game (the oracle weight).
PLAYER_QUERY_MIN = """
SELECT
    pss.team_id, pss.season, pss.player_id,
    pss.minutes_per_game AS mpg,
    COALESCE(traj.mean, fresh.mean, tps.cam_gbpm_v3_psos) AS campom,
    pa.primary_class, p.class_year
FROM player_season_stats pss
JOIN players p ON p.id = pss.player_id
LEFT JOIN torvik_player_stats tps
    ON tps.player_id = pss.player_id AND tps.season = pss.season
LEFT JOIN trajectory_oof_predictions traj
    ON traj.torvik_pid = tps.torvik_pid AND traj.target_season = pss.season
LEFT JOIN freshman_oof_predictions fresh
    ON fresh.cstat_player_id = pss.player_id AND fresh.target_season = pss.season
LEFT JOIN player_archetypes pa
    ON pa.player_id = pss.player_id AND pa.season = pss.season
WHERE pss.season = ANY(%(seasons)s)
  AND COALESCE(pss.games_played, 0) >= 5
  AND COALESCE(pss.minutes_per_game, 0) >= 5
"""

N_ROT = len(CANONICAL_ROTATION_MPG)


def aggregate(group: pd.DataFrame, mode: str) -> pd.Series:
    g = group.copy()
    if mode == "oracle_full":
        g["_rank_key"] = g["mpg"].fillna(0.0)           # rank by actual minutes
    else:
        g["_rank_key"] = g["campom"].fillna(_NEG)       # rank by cam_v3
    g = g.sort_values("_rank_key", ascending=False).reset_index(drop=True)
    g = g.head(N_ROT)

    if mode == "canonical":
        g["w"] = [CANONICAL_ROTATION_MPG[i] for i in range(len(g))]
    else:  # oracle_weights / oracle_full -> actual mpg as the weight
        g["w"] = g["mpg"].astype(float).clip(lower=0.0).values
    total_w = float(g["w"].sum())

    row: dict[str, float] = {"roster_size": int(len(g))}
    cam = g.dropna(subset=["campom"])
    if len(cam) == 0:
        for k in ("cam_wmean", "cam_sum", "cam_top1", "cam_top3_mean",
                  "cam_top7_mean", "cam_count_gt5", "cam_count_gt10", "cam_count_gt15"):
            row[k] = np.nan
    else:
        vals = cam["campom"].astype(float)
        w = cam["w"].astype(float); ws = float(w.sum())
        row["cam_wmean"] = float((vals * w).sum() / ws) if ws > 0 else np.nan
        row["cam_sum"] = float(vals.sum())
        sc = vals.sort_values(ascending=False).reset_index(drop=True)
        row["cam_top1"] = float(sc.iloc[0])
        row["cam_top3_mean"] = float(sc.head(3).mean())
        row["cam_top7_mean"] = float(sc.head(7).mean())
        row["cam_count_gt5"] = float((vals > 5.0).sum())
        row["cam_count_gt10"] = float((vals > 10.0).sum())
        row["cam_count_gt15"] = float((vals > 15.0).sum())

    cls = g["class_year"].map(normalize_class)
    for code, key in (("Fr", "exp_fr_share"), ("So", "exp_so_share"),
                      ("Jr", "exp_jr_share"), ("Sr", "exp_sr_share")):
        row[key] = float(g.loc[cls == code, "w"].sum()) / total_w if total_w > 0 else 0.0
    for arch in ARCHETYPES:
        row[f"arch_{arch.lower()}"] = (
            float(g.loc[g["primary_class"] == arch, "w"].sum()) / total_w if total_w > 0 else 0.0)
    return pd.Series(row)


def build(players, teams, outbound, inbound, mode):
    agg = (players.groupby(["team_id", "season"], as_index=False)
           .apply(lambda x: aggregate(x, mode), include_groups=False).reset_index(drop=True))
    df = agg.merge(teams, on=["team_id", "season"], how="inner")
    df = df.merge(outbound, on=["team_id", "season"], how="left")
    df = df.merge(inbound, on=["team_id", "season"], how="left")
    df["outbound_cam_v3_sum"] = df["outbound_cam_v3_sum"].fillna(0.0)
    df["inbound_cam_v3_sum"] = df["inbound_cam_v3_sum"].fillna(0.0)
    df = df.dropna(subset=["cam_wmean", "adj_efficiency_margin"]).reset_index(drop=True)
    fcols = [c for c in df.columns if c not in ("team_id", "season", "adj_efficiency_margin")]
    return df, fcols


def loso_oof(df, fcols):
    oof = np.full(len(df), np.nan)
    for s in SEASONS:
        tr = df["season"] != s; te = df["season"] == s
        if te.sum() == 0:
            continue
        m = lgb.LGBMRegressor(**lgb_params())
        m.fit(df.loc[tr, fcols], df.loc[tr, "adj_efficiency_margin"],
              eval_set=[(df.loc[te, fcols], df.loc[te, "adj_efficiency_margin"])],
              eval_metric="mae")
        oof[te.values] = m.predict(df.loc[te, fcols])
    return oof


def main():
    eng = get_engine()
    players = pd.read_sql(PLAYER_QUERY_MIN, eng, params={"seasons": list(SEASONS)})
    teams = pd.read_sql(TEAM_QUERY, eng, params={"seasons": list(SEASONS)})
    py = [s - 1 for s in SEASONS]
    outbound = pd.read_sql(OUTBOUND_QUERY, eng, params={"portal_years": py})
    inbound = pd.read_sql(INBOUND_QUERY, eng, params={"portal_years": py})
    print(f"players {len(players):,}  teams {len(teams):,}")

    results = {}
    for mode in ("canonical", "oracle_weights", "oracle_full"):
        df, fcols = build(players, teams, outbound, inbound, mode)
        oof = loso_oof(df, fcols)
        y = df["adj_efficiency_margin"].values
        # quartile by ACTUAL AdjEM
        q = pd.qcut(y, 4, labels=["Q1 bust", "Q2", "Q3", "Q4 elite"])
        results[mode] = (df, oof, y, q)
        print(f"\n=== {mode}  (pooled LOSO MAE = {mean_absolute_error(y, oof):.3f}) ===")
        for ql in ["Q1 bust", "Q2", "Q3", "Q4 elite"]:
            msk = np.asarray(q == ql)
            mae = mean_absolute_error(y[msk], oof[msk])
            bias = float((oof[msk] - y[msk]).mean())  # + = over-projected
            print(f"  {ql:<9} n={msk.sum():<5} MAE={mae:5.2f}  bias={bias:+5.2f}")

    print("\n" + "=" * 56)
    print("CEILING SUMMARY (vs canonical baseline)")
    base = mean_absolute_error(results["canonical"][2], results["canonical"][1])
    print(f"  {'mode':<16}{'pooled MAE':>11}{'Δ vs canon':>12}{'Q1 bias':>10}")
    for mode in ("canonical", "oracle_weights", "oracle_full"):
        df, oof, y, q = results[mode]
        mae = mean_absolute_error(y, oof)
        q1 = np.asarray(q == "Q1 bust")
        q1bias = float((oof[q1] - y[q1]).mean())
        print(f"  {mode:<16}{mae:>11.3f}{mae-base:>+12.3f}{q1bias:>+10.2f}")
    print("\n  STOP-GATE: if oracle Δ ~ 0 and Q1 bias unchanged -> minutes is NOT the lever.")


if __name__ == "__main__":
    main()
