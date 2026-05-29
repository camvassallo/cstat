"""
Prototype: point-in-time CamPom v3 computation.

Aggregates `torvik_player_game_stats` rows up to a cutoff date, plugs the
result into the CamPom v3 formula, and emits a (pid, season, as_of_date,
cam_gbpm_v3) table. This is the leak-free version of the season-aggregate
`torvik_player_stats.cam_gbpm_v3` column.

Skips the conference-SOS adjustment for now (`sos_adj=0`). A full point-in-
time SOS would require recomputing conference quality at each cutoff date
from `team_game_stats`, which is a separate lift. The base + minutes + GP
shrinkage adjustments are the bulk of the CamPom signal and what we need
for the immediate overfitting-audit smoke test.

Validation: when called with cutoff = end of season, the result should
correlate ~0.99 with the season-aggregate `cam_gbpm_v3` already in the DB.
Magnitudes will differ slightly (per-game averaging vs season-aggregate
possession-weighting), but rank order should match closely.

Usage:
  python compute_campom_at.py --season 2026 --cutoff 2026-04-06 [--validate]
  python compute_campom_at.py --season 2026 --cutoff 2025-12-31

Constants mirror compute.rs:CAMPOM_*.
"""

import argparse
from pathlib import Path

import pandas as pd
from sqlalchemy import text

from db import get_engine

# Mirrors crates/cstat-core/src/compute.rs:1118-1129
CAMPOM_OFFENSE_EXPONENT = 0.7
CAMPOM_DEFENSE_DISCOUNT = 0.1
CAMPOM_USG_REF = 17.873_577_08
CAMPOM_MINUTES_EXPONENT = 0.5
CAMPOM_GP_K = 8.0


def compute_at(engine, season: int, cutoff_date: str, min_gp: int = 5) -> pd.DataFrame:
    """Aggregate per-game rows up to cutoff and compute CamPom v3 (no-SOS).

    Mirrors compute_campom in compute.rs (lines 1201-1235, 1287). Key
    semantic match: `mp_factor` is power(player_min_pct / cohort_mean_min_pct, 0.5),
    NOT raw min_pct^0.5. Without the cohort-mean normalization the scale is
    wrong by 3-4x (rank order stays right, but downstream consumers that
    care about magnitudes would break).
    """
    sql = text("""
        SELECT
            pid,
            COUNT(*)                                 AS gp,
            SUM(COALESCE(possessions, 0))            AS poss_total,
            SUM(obpm * COALESCE(possessions, 0)) / NULLIF(SUM(COALESCE(possessions, 0)), 0)
                                                     AS ogbpm,
            SUM(dbpm * COALESCE(possessions, 0)) / NULLIF(SUM(COALESCE(possessions, 0)), 0)
                                                     AS dgbpm,
            SUM(usage * COALESCE(minutes_pct, 0)) / NULLIF(SUM(COALESCE(minutes_pct, 0)), 0)
                                                     AS usg,
            AVG(minutes_pct)                         AS min_pct
        FROM torvik_player_game_stats
        WHERE season = :season
          AND game_date <= :cutoff
        GROUP BY pid
    """)
    with engine.connect() as conn:
        df = pd.read_sql(sql, conn, params={"season": season, "cutoff": cutoff_date})

    if len(df) == 0:
        return df

    # Filter to qualified players (matches the >= 5 GP CamPom display floor).
    df = df[df["gp"] >= min_gp].reset_index(drop=True)

    # Cohort mean for min_pct normalization (compute.rs uses the same
    # season-mean approach for mp_factor; we use the as-of cohort).
    mean_min_pct = df["min_pct"].mean()

    # adj_gbpm = OGBPM × (USG/USG_REF)^0.7  +  DGBPM × (1 - 0.1 × USG/USG_REF)
    usg_ratio = df["usg"] / CAMPOM_USG_REF
    adj_o = df["ogbpm"] * (usg_ratio.clip(lower=0) ** CAMPOM_OFFENSE_EXPONENT)
    adj_d = df["dgbpm"] * (1.0 - CAMPOM_DEFENSE_DISCOUNT * usg_ratio)
    df["adj_gbpm"] = adj_o + adj_d

    # mp_factor = (player_min_pct / cohort_mean_min_pct) ^ 0.5
    df["mp_factor"] = (df["min_pct"].clip(lower=0) / mean_min_pct) ** CAMPOM_MINUTES_EXPONENT

    df["gp_weight"] = df["gp"] / (df["gp"] + CAMPOM_GP_K)

    df["cam_gbpm_v3_no_sos"] = df["adj_gbpm"] * df["mp_factor"] * df["gp_weight"]
    df["as_of_date"] = cutoff_date
    df["season"] = season
    df["cohort_mean_min_pct"] = mean_min_pct

    return df[
        ["pid", "season", "as_of_date", "gp", "poss_total",
         "ogbpm", "dgbpm", "usg", "min_pct", "cohort_mean_min_pct",
         "adj_gbpm", "mp_factor", "gp_weight", "cam_gbpm_v3_no_sos"]
    ]


def validate_against_season(engine, season: int, df_pit: pd.DataFrame):
    """End-of-season point-in-time should correlate ~0.99 with persisted aggregate."""
    sql = text("""
        SELECT torvik_pid AS pid, cam_gbpm_v3, ogbpm, dgbpm
        FROM torvik_player_stats
        WHERE season = :season AND cam_gbpm_v3 IS NOT NULL
    """)
    with engine.connect() as conn:
        season_df = pd.read_sql(sql, conn, params={"season": season})

    merged = df_pit.merge(season_df, on="pid", how="inner", suffixes=("_pit", "_season"))
    print()
    print(f"=== validation: point-in-time (end of season) vs persisted aggregate ===")
    print(f"  matched players: {len(merged)}")
    if len(merged) == 0:
        return

    # Drop NaNs for correlation
    sub = merged.dropna(subset=["cam_gbpm_v3_no_sos", "cam_gbpm_v3"])
    sub = sub[sub["gp"] >= 5]  # min sample; aggregate-table players typically meet this
    print(f"  with GP>=5:      {len(sub)}")
    corr_overall = sub["cam_gbpm_v3_no_sos"].corr(sub["cam_gbpm_v3"])
    print(f"  Pearson r:       {corr_overall:.4f}")

    sub_top = sub.nlargest(50, "cam_gbpm_v3")
    corr_top = sub_top["cam_gbpm_v3_no_sos"].corr(sub_top["cam_gbpm_v3"])
    print(f"  Pearson r (top 50 by season aggregate): {corr_top:.4f}")

    # Show top 10 head-to-head to eyeball
    print()
    print(f"  top 10 by season aggregate (head-to-head):")
    print(f"    {'pid':<8}{'season_cam':>12}{'pit_cam_no_sos':>18}{'gp':>5}")
    for _, r in sub_top.head(10).iterrows():
        print(f"    {int(r['pid']):<8}{r['cam_gbpm_v3']:>12.2f}{r['cam_gbpm_v3_no_sos']:>18.2f}{int(r['gp']):>5}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--season", type=int, required=True)
    ap.add_argument("--cutoff", type=str, required=True,
                    help="end-inclusive date string, e.g. 2026-01-15")
    ap.add_argument("--validate", action="store_true",
                    help="compare against persisted season aggregate (only meaningful at end-of-season cutoff)")
    ap.add_argument("--output", type=Path, default=None)
    args = ap.parse_args()

    engine = get_engine()
    df = compute_at(engine, args.season, args.cutoff)
    print(f"Computed cam_gbpm_v3_no_sos for {len(df)} players (season {args.season}, as of {args.cutoff})")

    if len(df) > 0:
        print(f"  top 5 by cam_gbpm_v3_no_sos:")
        for _, r in df.nlargest(5, "cam_gbpm_v3_no_sos").iterrows():
            print(f"    pid={int(r['pid']):<8} gp={int(r['gp']):>2}  cam_no_sos={r['cam_gbpm_v3_no_sos']:>+7.2f}  ogbpm={r['ogbpm']:>+5.2f} dgbpm={r['dgbpm']:>+5.2f} usg={r['usg']:>4.1f}")

    if args.validate:
        validate_against_season(engine, args.season, df)

    if args.output:
        df.to_csv(args.output, index=False)
        print(f"\nWrote {args.output}")


if __name__ == "__main__":
    main()
