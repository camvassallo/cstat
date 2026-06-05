"""
De-risk CAE-O / CAE-D BEFORE building any model.

Question: is offensive/defensive OVER-performance (actual minus roster-talent
expectation) a stable COACHING trait that persists across a coach's roster
turnover -- or is it just roster quality? If it doesn't persist within a coach,
CAE-O/CAE-D can't separate coaches and we should NOT build it.

Cheap proxy (no calibrator needed):
  resid_O = offensive quality beyond offensive roster talent
          = season-centered AdjO, residualized on roster sum(cam_o)
  resid_D = defensive quality beyond defensive roster talent
          = season-centered defense-good, residualized on roster sum(cam_d)
  tilt    = resid_O - resid_D   (+ = over-performs on offense vs defense)

Tests:
  (1) split-half reliability of each within-coach (odd vs even seasons) --
      high r => stable coaching trait, near-0 => roster noise.
  (2) permutation null (shuffle coach labels) -- the floor.
  (3) eyeball: do known offensive coaches (Few) rank high on resid_O and
      defensive coaches (Bennett) high on resid_D?

KenPom scale: higher AdjO = better offense; LOWER AdjD = better defense.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import numpy as np
import pandas as pd
from db import get_engine

MIN_SEASONS = 4  # for the within-coach persistence test


def residualize(y, X):
    """OLS residuals of y on [1, z(X)...]; regressors z-scored for stability."""
    Xz = (X - X.mean(0)) / (X.std(0) + 1e-9)
    A = np.column_stack([np.ones(len(y)), Xz])
    beta, *_ = np.linalg.lstsq(A, y, rcond=None)
    return y - A @ beta, beta


def split_half_r(df, val_col, min_seasons=MIN_SEASONS):
    """Per coach with >=min_seasons, split seasons odd/even by rank, take each
    half's mean; Pearson r of (half A means) vs (half B means) across coaches."""
    a, b = [], []
    for _, g in df.groupby("coach_id"):
        if len(g) < min_seasons:
            continue
        g = g.sort_values("season")
        ranks = np.arange(len(g))
        ha = g[val_col].values[ranks % 2 == 0]
        hb = g[val_col].values[ranks % 2 == 1]
        if len(ha) and len(hb):
            a.append(ha.mean()); b.append(hb.mean())
    a, b = np.array(a), np.array(b)
    return float(np.corrcoef(a, b)[0, 1]), len(a)


def icc_between_share(df, val_col, min_seasons=MIN_SEASONS):
    """Between-coach variance share = var(coach means) / total var. High =
    the trait is coach-characteristic; low = mostly within-coach noise."""
    sub = df.groupby("coach_id").filter(lambda g: len(g) >= min_seasons)
    grand = sub[val_col].mean()
    means = sub.groupby("coach_id")[val_col].mean()
    between = ((means - grand) ** 2 * sub.groupby("coach_id").size()).sum()
    total = ((sub[val_col] - grand) ** 2).sum()
    return float(between / total)


def main():
    eng = get_engine()

    # coached team-seasons + actual AdjO/AdjD + coach name
    ts = pd.read_sql("""
        SELECT cs.coach_id, co.canonical_name AS coach, cs.season,
               t.id AS team_id, cs.team_natstat_id,
               ts.adj_offense, ts.adj_defense
        FROM coach_seasons cs
        JOIN coaches co ON co.id = cs.coach_id
        JOIN teams t ON t.natstat_id = cs.team_natstat_id AND t.season = cs.season
        JOIN team_season_stats ts ON ts.team_id = t.id AND ts.season = cs.season
        WHERE ts.adj_offense IS NOT NULL AND ts.adj_defense IS NOT NULL
    """, eng)

    # roster offensive/defensive talent: sum of per-player cam_o / cam_d.
    # Use the v2 composites (GP-shrunk, NO SOS-division degeneracy) so the
    # roster-talent regressor isn't corrupted by near-zero-adj blow-ups.
    roster = pd.read_sql("""
        SELECT p.team_id, tps.season,
               SUM(tps.cam_o_gbpm_v2) AS rcam_o,
               SUM(tps.cam_d_gbpm_v2) AS rcam_d,
               COUNT(*) AS n_players
        FROM torvik_player_stats tps
        JOIN players p ON p.id = tps.player_id
        WHERE tps.cam_o_gbpm_v2 IS NOT NULL
          AND tps.cam_d_gbpm_v2 IS NOT NULL
          AND tps.games_played >= 5
        GROUP BY p.team_id, tps.season
    """, eng)

    df = ts.merge(roster, on=["team_id", "season"], how="inner")
    df = df.dropna(subset=["rcam_o", "rcam_d"]).reset_index(drop=True)
    print(f"Coached team-seasons with roster cam: {len(df)}  "
          f"coaches: {df.coach_id.nunique()}  "
          f">= {MIN_SEASONS} seasons: {(df.groupby('coach_id').size() >= MIN_SEASONS).sum()}")

    # season-center: offense-good = AdjO - seasonMean; defense-good = seasonMean - AdjD
    df["o_good"] = df["adj_offense"] - df.groupby("season")["adj_offense"].transform("mean")
    df["d_good"] = df.groupby("season")["adj_defense"].transform("mean") - df["adj_defense"]

    # residualize on roster talent -> over-performance beyond roster
    df["resid_O"], bO = residualize(df["o_good"].values, df[["rcam_o"]].values)
    df["resid_D"], bD = residualize(df["d_good"].values, df[["rcam_d"]].values)
    df["tilt"] = df["resid_O"] - df["resid_D"]
    print(f"roster->offense slope: {bO[1]:+.3f}   roster->defense slope: {bD[1]:+.3f}")
    print(f"corr(resid_O, resid_D) = {np.corrcoef(df.resid_O, df.resid_D)[0,1]:+.3f}  "
          f"(near 0 => O and D over-perf are independent axes)")

    print("\n=== within-coach split-half reliability (>= %d seasons) ===" % MIN_SEASONS)
    print(f"  {'metric':<12}{'split-half r':>13}{'between-var share':>19}{'null r (shuffled)':>19}")
    rng = np.random.default_rng(42)
    for col in ["resid_O", "resid_D", "tilt"]:
        r, ncoach = split_half_r(df, col)
        icc = icc_between_share(df, col)
        # permutation null: shuffle coach_id, redo split-half
        nulls = []
        for _ in range(200):
            d2 = df.copy()
            d2["coach_id"] = rng.permutation(d2["coach_id"].values)
            nr, _ = split_half_r(d2, col)
            if np.isfinite(nr):
                nulls.append(nr)
        print(f"  {col:<12}{r:>13.3f}{icc:>19.3f}{np.mean(nulls):>13.3f}+-{np.std(nulls):.2f}")
    print(f"  (n_coaches in test ~ {ncoach})")

    # eyeball known coaches
    cm = df.groupby(["coach_id", "coach"]).agg(
        n=("season", "size"), resid_O=("resid_O", "mean"),
        resid_D=("resid_D", "mean"), tilt=("tilt", "mean")).reset_index()
    cm = cm[cm.n >= MIN_SEASONS]

    print("\n=== top 12 OFFENSIVE over-performers (mean resid_O) ===")
    for _, r in cm.nlargest(12, "resid_O").iterrows():
        print(f"  {r.coach:<26} n={int(r.n):<3} resid_O={r.resid_O:+5.2f} resid_D={r.resid_D:+5.2f}")
    print("\n=== top 12 DEFENSIVE over-performers (mean resid_D) ===")
    for _, r in cm.nlargest(12, "resid_D").iterrows():
        print(f"  {r.coach:<26} n={int(r.n):<3} resid_O={r.resid_O:+5.2f} resid_D={r.resid_D:+5.2f}")

    print("\n=== sanity spot-check: known coaches ===")
    for name in ["Few", "Bennett", "Izzo", "Painter", "Drew", "Pearl", "Beard",
                 "Hurley", "Self", "Pitino", "Boeheim", "Calipari"]:
        hit = cm[cm.coach.str.contains(name, case=False, na=False)]
        for _, r in hit.iterrows():
            print(f"  {r.coach:<26} n={int(r.n):<3} resid_O={r.resid_O:+5.2f} "
                  f"resid_D={r.resid_D:+5.2f} tilt={r.tilt:+5.2f}")


if __name__ == "__main__":
    main()
