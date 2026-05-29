"""
Quick leakage-budget measurement.

Computes team-level point-in-time CamPom at four mid-season cutoffs and
end-of-season for one season. Measures how much team rankings shift over
time. If teams are stable (small Δ between mid- and end-season), the
lookahead leakage in the full-season-CamPom features can't meaningfully
bias the predict model. If teams are unstable, the leakage signal could
be material.

This is a cheap upper-bound for how much honest LOSO retraining will
hurt AUC, without doing the retrain itself.

Method:
  1. For each cutoff date D, compute pit cam_v3 (no SOS) per player
  2. Roll up to team-level (minutes-weighted avg over top 8 by GP)
  3. Compare team CamPom at D vs end-of-season: corr + delta distribution
  4. The per-team Δ between mid- and end-season is the maximum leakage
     advantage the model could exploit
"""

import argparse

import pandas as pd
from sqlalchemy import text

from compute_campom_at import compute_at
from db import get_engine


def team_aggregates(engine, season: int, df_pit: pd.DataFrame, top_n: int = 8) -> pd.DataFrame:
    """Roll player-level pit CamPom to team-level (weighted avg over top N by GP)."""
    sql = text("""
        SELECT t.torvik_pid AS pid, p.team_id AS team_id
        FROM torvik_player_stats t
        JOIN players p ON p.id = t.player_id
        WHERE t.season = :season AND p.team_id IS NOT NULL
    """)
    with engine.connect() as conn:
        map_df = pd.read_sql(sql, conn, params={"season": season})

    j = df_pit.merge(map_df, on="pid", how="inner")
    # Top N by GP per team
    j = j.sort_values(["team_id", "gp"], ascending=[True, False])
    j["rank"] = j.groupby("team_id").cumcount()
    rotation = j[j["rank"] < top_n].copy()

    out = (
        rotation.groupby("team_id")
        .apply(
            lambda g: pd.Series({
                "team_cam_no_sos": (g["cam_gbpm_v3_no_sos"] * g["gp"]).sum() / max(g["gp"].sum(), 1),
                "rotation_n": len(g),
            })
        )
        .reset_index()
    )
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--season", type=int, default=2026)
    ap.add_argument("--cutoffs", type=str, default="2025-12-01,2026-01-15,2026-02-15,2026-03-15,2026-04-06")
    args = ap.parse_args()

    cutoffs = [c.strip() for c in args.cutoffs.split(",")]
    last = cutoffs[-1]
    engine = get_engine()

    print(f"Computing team CamPom at {len(cutoffs)} cutoffs for season {args.season}")
    per_cutoff = {}
    for c in cutoffs:
        pit = compute_at(engine, args.season, c)
        team = team_aggregates(engine, args.season, pit)
        per_cutoff[c] = team
        print(f"  {c}: {len(team)} teams, mean={team['team_cam_no_sos'].mean():.2f}, std={team['team_cam_no_sos'].std():.2f}")

    end = per_cutoff[last].rename(columns={"team_cam_no_sos": "end_cam"})

    print()
    print("=" * 80)
    print(f"  Team CamPom drift: mid-season cutoff vs end-of-season ({last})")
    print("=" * 80)
    print(f"  {'cutoff':<14}{'n':>5}{'pearson':>10}{'spearman':>10}{'mae':>8}{'p95Δ':>8}{'maxΔ':>8}")
    rows = []
    for c in cutoffs[:-1]:
        m = per_cutoff[c].rename(columns={"team_cam_no_sos": "mid_cam"})
        j = m.merge(end[["team_id", "end_cam"]], on="team_id")
        pearson = j["mid_cam"].corr(j["end_cam"])
        from scipy.stats import spearmanr
        spear, _ = spearmanr(j["mid_cam"], j["end_cam"])
        delta = (j["end_cam"] - j["mid_cam"]).abs()
        rows.append({
            "cutoff": c,
            "n": len(j),
            "pearson": pearson,
            "spearman": spear,
            "mae": delta.mean(),
            "p95": delta.quantile(0.95),
            "max": delta.max(),
        })
        print(f"  {c:<14}{len(j):>5}{pearson:>10.4f}{spear:>10.4f}{delta.mean():>8.3f}{delta.quantile(0.95):>8.3f}{delta.max():>8.3f}")

    # Identify the most volatile teams (largest end-vs-Dec drift) — these are
    # exactly the teams whose game predictions benefit most from lookahead.
    j_dec = (
        per_cutoff[cutoffs[0]]
        .rename(columns={"team_cam_no_sos": "early_cam"})
        .merge(end[["team_id", "end_cam"]], on="team_id")
    )
    j_dec["growth"] = j_dec["end_cam"] - j_dec["early_cam"]
    print()
    print(f"  Top 5 'late risers' (early-season cutoff to end-of-season)")
    print(f"    {'team_id':<40}{'early':>8}{'end':>8}{'growth':>9}")
    for _, r in j_dec.nlargest(5, "growth").iterrows():
        print(f"    {str(r['team_id']):<40}{r['early_cam']:>+8.2f}{r['end_cam']:>+8.2f}{r['growth']:>+9.2f}")
    print()
    print(f"  Top 5 'late fallers'")
    for _, r in j_dec.nsmallest(5, "growth").iterrows():
        print(f"    {str(r['team_id']):<40}{r['early_cam']:>+8.2f}{r['end_cam']:>+8.2f}{r['growth']:>+9.2f}")

    print()
    print("  INTERPRETATION:")
    print("    - Pearson > 0.95 + small p95Δ → team CamPom is stable; lookahead has little to exploit.")
    print("    - p95Δ ≈ 1 CamPom point typically translates to ~1 point of predicted margin.")
    print("    - 'Late risers' are the teams whose predictions inflate most from full-season features.")


if __name__ == "__main__":
    main()
