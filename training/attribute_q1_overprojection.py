"""PR B follow-up: attribute the Q1 team over-projection into three
disjoint components, after the attrition mechanism check refuted the
"trajectory over-projects thin-roster returners" hypothesis
(`pr_b_attrition_mechanism_20260529.md`).

The existing `decompose_projection_error.py` splits the pipeline error into
`upstream` (phase_b − oracle_roster) and `calibrator` (oracle_roster −
actual). But its "oracle_roster" feeds the calibrator *projected* cam_v3
(the `COALESCE(traj.mean, fresh.mean, …)` in `fetch_team_roster`) — the
same cam source as phase_b — so its `upstream` is really pure COMPOSITION
(only the player SET differs) and the cam-projection error is hidden inside
its `calibrator` bucket. This script makes the cut clean by adding a TRUE
oracle (actual roster + ACTUAL cam_v3):

  A  phase_b           = projected roster + projected cam   (pipeline)
  B  oracle_proj_cam   = actual roster   + projected cam    (= existing oracle)
  C  oracle_actual_cam = actual roster   + actual cam       (true oracle)

Three disjoint components (signs: + = over-projected):

  composition     = A − B   (which players / how many — wrong roster SET)
  cam_value       = B − C   (trajectory + freshman cam projection error on
                             the correctly-identified roster)
  calibrator_floor= C − actual  (calibrator mean-reversion given PERFECT
                                 roster AND perfect cam)

Reported per actual quartile, with Q1 (bust teams) called out — that's the
+5.62 we're chasing. Whichever component carries Q1's bias is the lever;
the attrition check already told us it isn't trajectory per-player value
accuracy, so we expect composition (hard) and/or calibrator_floor (PR C's
monotone/tail recalibration) to dominate, with cam_value small.

NOTE the calibrator was trained on PROJECTED (OOF) cam_v3
(`train_roster_impact_model.py` v2), so B is its in-distribution input and
C (actual cam) is mildly out-of-distribution. `cam_value` therefore reads
as "how much do cam projection errors move the team estimate", not a
calibrator-quality statement.
"""

from __future__ import annotations

import datetime as dt
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
import pandas as pd
from sqlalchemy import text

from db import get_engine
from decompose_projection_error import (
    EVAL_DIR,
    build_feature_vector,
    fetch_portal_sums,
    load_feature_contract,
    load_loso_models,
    load_per_team,
    resolve_target_team_id,
)

# Two roster fetches differing ONLY in the cam_v3 source. `proj` replicates
# the existing oracle (projected/OOF cam); `actual` is the true oracle.
ROSTER_SQL = """
SELECT
    pss.team_id, pss.season, pss.player_id,
    {campom_expr} AS campom,
    pa.primary_class,
    p.class_year
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
WHERE pss.season = :season
  AND pss.team_id = CAST(:team_id AS uuid)
  AND COALESCE(pss.games_played, 0) >= 5
  AND COALESCE(pss.minutes_per_game, 0) >= 5
"""
PROJ_CAM = "COALESCE(traj.mean, fresh.mean, tps.cam_gbpm_v3_psos)"
ACTUAL_CAM = "tps.cam_gbpm_v3_psos"


def fetch_roster(conn, team_id: str, season: int, actual: bool) -> pd.DataFrame:
    expr = ACTUAL_CAM if actual else PROJ_CAM
    sql = text(ROSTER_SQL.format(campom_expr=expr))
    return pd.read_sql(sql, conn, params={"season": season, "team_id": team_id})


def score(session, roster, out_sum, in_sum, feature_names) -> float:
    vec = build_feature_vector(roster, out_sum, in_sum, feature_names)
    raw = session.run(None, {session.get_inputs()[0].name: vec})[0]
    return float(np.asarray(raw).flatten()[0])


def per_actual_quartile(df: pd.DataFrame, cols: list[str]) -> list[dict]:
    q = df["actual"].quantile([0.25, 0.5, 0.75]).values
    bounds = [-np.inf, q[0], q[1], q[2], np.inf]
    labels = ["Q1 bottom", "Q2 below-median", "Q3 above-median", "Q4 top"]
    out = []
    for i in range(4):
        m = (df["actual"] > bounds[i]) & (df["actual"] <= bounds[i + 1])
        sub = df[m]
        if len(sub) == 0:
            continue
        rec = {"bucket": labels[i], "n": int(len(sub))}
        for c in cols:
            rec[f"{c}_mae"] = float(sub[c].abs().mean())
            rec[f"{c}_bias"] = float(sub[c].mean())
        out.append(rec)
    return out


def main() -> None:
    df = load_per_team()
    sessions = load_loso_models()
    feature_names = load_feature_contract()
    print(f"per-team dump: {len(df)} rows; feature contract {len(feature_names)}")

    engine = get_engine()
    B, C = [], []
    with engine.connect() as conn:
        for i, row in df.iterrows():
            season = int(row["season"])
            if season not in sessions:
                B.append(np.nan)
                C.append(np.nan)
                continue
            tgt = resolve_target_team_id(conn, row["team_id"], season)
            if tgt is None:
                B.append(np.nan)
                C.append(np.nan)
                continue
            out_sum, in_sum = fetch_portal_sums(conn, tgt, season)
            r_proj = fetch_roster(conn, tgt, season, actual=False)
            r_act = fetch_roster(conn, tgt, season, actual=True)
            B.append(score(sessions[season], r_proj, out_sum, in_sum, feature_names))
            C.append(score(sessions[season], r_act, out_sum, in_sum, feature_names))
            if (i + 1) % 50 == 0:
                print(f"  scored {i + 1}/{len(df)}")

    df["oracle_proj_cam"] = B
    df["oracle_actual_cam"] = C
    df = df.dropna(subset=["oracle_proj_cam", "oracle_actual_cam"]).reset_index(drop=True)

    df["composition"] = df["phase_b"] - df["oracle_proj_cam"]
    df["cam_value"] = df["oracle_proj_cam"] - df["oracle_actual_cam"]
    df["calibrator_floor"] = df["oracle_actual_cam"] - df["actual"]
    df["total"] = df["phase_b"] - df["actual"]  # = sum of the three
    comps = ["total", "composition", "cam_value", "calibrator_floor"]

    findings = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "n": int(len(df)),
        "headline": {
            c: {"mae": float(df[c].abs().mean()), "bias": float(df[c].mean())}
            for c in comps
        },
        "per_actual_quartile": per_actual_quartile(df, comps),
    }

    print(f"\nscored {len(df)} teams against both oracles")
    print("\n=== HEADLINE (signs: + = over-projected) ===")
    for c in comps:
        s = findings["headline"][c]
        print(f"  {c:<18} MAE {s['mae']:5.2f}  bias {s['bias']:+5.2f}")

    print("\n=== ATTRIBUTION BY ACTUAL QUARTILE (bias) ===")
    print(f"  {'bucket':<18}{'n':>4}  {'total':>8}{'composit':>9}{'cam_val':>9}{'calib':>8}")
    for b in findings["per_actual_quartile"]:
        print(f"  {b['bucket']:<18}{b['n']:>4}  "
              f"{b['total_bias']:>+8.2f}{b['composition_bias']:>+9.2f}"
              f"{b['cam_value_bias']:>+9.2f}{b['calibrator_floor_bias']:>+8.2f}")

    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out_json = EVAL_DIR / f"q1_attribution_{date_str}_summary.json"
    out_json.write_text(json.dumps(findings, indent=2, default=float))
    print(f"\nwrote {out_json}")


if __name__ == "__main__":
    main()
