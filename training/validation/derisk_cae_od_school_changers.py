"""
Decisive coach-vs-program adjudicator for CAE-O/CAE-D.

The persistence found in derisk_cae_od_signal.py could be PROGRAM identity
(coach ~= one program in 2015-2026), not coaching skill. This separates them:

  (A) COACH-TRAVEL test: for coaches who changed schools, does their O/D tilt
      at program B match their tilt at program A? If tilt travels -> coaching.
  (B) PROGRAM-across-coach test: at a fixed program, do DIFFERENT coaches show
      the same tilt? If yes -> it's the program, not the coach.

If (A) r >> (B) r, the O/D signature is the coach. If (B) >= (A), it's the program.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
from db import get_engine


def residualize(y, X):
    Xz = (X - X.mean(0)) / (X.std(0) + 1e-9)
    A = np.column_stack([np.ones(len(y)), Xz])
    beta, *_ = np.linalg.lstsq(A, y, rcond=None)
    return y - A @ beta


def build():
    eng = get_engine()
    ts = pd.read_sql("""
        SELECT cs.coach_id, co.canonical_name AS coach, cs.season,
               t.id AS team_id, cs.team_natstat_id AS prog,
               ts.adj_offense, ts.adj_defense
        FROM coach_seasons cs
        JOIN coaches co ON co.id = cs.coach_id
        JOIN teams t ON t.natstat_id = cs.team_natstat_id AND t.season = cs.season
        JOIN team_season_stats ts ON ts.team_id = t.id AND ts.season = cs.season
        WHERE ts.adj_offense IS NOT NULL AND ts.adj_defense IS NOT NULL
    """, eng)
    roster = pd.read_sql("""
        SELECT p.team_id, tps.season,
               SUM(tps.cam_o_gbpm_v2) AS rcam_o, SUM(tps.cam_d_gbpm_v2) AS rcam_d
        FROM torvik_player_stats tps JOIN players p ON p.id = tps.player_id
        WHERE tps.cam_o_gbpm_v2 IS NOT NULL AND tps.cam_d_gbpm_v2 IS NOT NULL
          AND tps.games_played >= 5
        GROUP BY p.team_id, tps.season
    """, eng)
    df = ts.merge(roster, on=["team_id", "season"], how="inner").dropna(
        subset=["rcam_o", "rcam_d"]).reset_index(drop=True)
    df = df[np.isfinite(df[["rcam_o", "rcam_d"]].to_numpy(float)).all(1)].reset_index(drop=True)
    df["o_good"] = df["adj_offense"] - df.groupby("season")["adj_offense"].transform("mean")
    df["d_good"] = df.groupby("season")["adj_defense"].transform("mean") - df["adj_defense"]
    df["resid_O"] = residualize(df["o_good"].values, df[["rcam_o"]].values)
    df["resid_D"] = residualize(df["d_good"].values, df[["rcam_d"]].values)
    df["tilt"] = df["resid_O"] - df["resid_D"]
    return df


def coach_travel(df, col, min_per_prog=2):
    """For coaches at >=2 programs with >=min_per_prog seasons each, correlate
    their mean(col) at their two most-tenured programs."""
    A, B, rows = [], [], []
    for cid, g in df.groupby("coach_id"):
        prog_means = g.groupby("prog").agg(n=("season", "size"),
                                           v=(col, "mean")).reset_index()
        prog_means = prog_means[prog_means.n >= min_per_prog].sort_values("n", ascending=False)
        if len(prog_means) >= 2:
            A.append(prog_means.v.iloc[0]); B.append(prog_means.v.iloc[1])
            rows.append((g.coach.iloc[0], prog_means.v.iloc[0], prog_means.v.iloc[1]))
    A, B = np.array(A), np.array(B)
    r = float(np.corrcoef(A, B)[0, 1]) if len(A) > 2 else float("nan")
    return r, len(A), rows


def program_across_coach(df, col, min_per_coach=2):
    """At programs coached by >=2 different coaches (>=min_per_coach seasons
    each), correlate the two coaches' mean(col) -- the program-effect floor."""
    A, B = [], []
    for prog, g in df.groupby("prog"):
        cm = g.groupby("coach_id").agg(n=("season", "size"),
                                       v=(col, "mean")).reset_index()
        cm = cm[cm.n >= min_per_coach].sort_values("n", ascending=False)
        if len(cm) >= 2:
            A.append(cm.v.iloc[0]); B.append(cm.v.iloc[1])
    A, B = np.array(A), np.array(B)
    r = float(np.corrcoef(A, B)[0, 1]) if len(A) > 2 else float("nan")
    return r, len(A)


def main():
    df = build()
    print(f"rows {len(df)}  coaches {df.coach_id.nunique()}  programs {df.prog.nunique()}")
    changers = df.groupby("coach_id")["prog"].nunique()
    print(f"coaches at >=2 programs: {(changers >= 2).sum()}")

    print("\n=== (A) COACH-TRAVEL vs (B) PROGRAM-ACROSS-COACH (Pearson r) ===")
    print(f"  {'metric':<10}{'coach-travel r':>16}{'(n)':>6}{'program r':>12}{'(n)':>6}")
    for col in ["resid_O", "resid_D", "tilt"]:
        ra, na, _ = coach_travel(df, col)
        rb, nb = program_across_coach(df, col)
        print(f"  {col:<10}{ra:>16.3f}{na:>6}{rb:>12.3f}{nb:>6}")

    print("\n=== coaches who changed schools: tilt at prog A vs prog B ===")
    _, _, rows = coach_travel(df, "tilt")
    for coach, a, b in sorted(rows, key=lambda x: -abs(x[1])):
        print(f"  {coach:<24} progA_tilt={a:+5.2f}  progB_tilt={b:+5.2f}  "
              f"{'TRAVELS' if a*b > 0 else 'flips'}")


if __name__ == "__main__":
    main()
