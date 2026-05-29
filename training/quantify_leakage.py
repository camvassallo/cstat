"""
Quantify how much the production predict model is using lookahead info.

Method:
  1. Load OOF predictions (leak-free per-game from 5-fold split).
  2. For each game, compute team CamPom diff in two ways:
       full_season_diff = home_full_cam − away_full_cam   (uses season-end data)
       pit_diff         = home_pit_cam   − away_pit_cam   (only pre-game data)
       leak_diff        = full_season_diff − pit_diff      (the lookahead signal)
  3. Regress predicted_margin on  [pit_diff, leak_diff] jointly.
       coef on pit_diff  = how much model uses legitimate pre-game info
       coef on leak_diff = how much model uses lookahead (the leakage budget)

If coef(leak_diff) ≈ 0, the model is honest.  If coef(leak_diff) > 0, it's
betting on teams that improved over the season — info it shouldn't have.

Uses the pit lookup built by build_pit_lookup.py.
"""

import argparse
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import text

from db import get_engine

PIT_LOOKUP_PATH = Path(__file__).parent / "models" / "pit_cam_v3_lookup.csv.gz"


def load_pit_lookup() -> pd.DataFrame:
    df = pd.read_csv(PIT_LOOKUP_PATH, parse_dates=["cutoff_date"])
    return df


def team_cam_at_cutoff(engine, season: int, cutoff_date: str):
    """Team CamPom (no SOS) at a specific cutoff. Minutes-weighted over rotation."""
    pit_path = PIT_LOOKUP_PATH
    pit = pd.read_csv(pit_path, parse_dates=["cutoff_date"])
    pit_at = pit[(pit["season"] == season) & (pit["cutoff_date"] == pd.Timestamp(cutoff_date))]
    # join pid → team
    map_sql = text("""
        SELECT t.torvik_pid AS pid, p.team_id AS team_id, p.id AS player_id
        FROM torvik_player_stats t
        JOIN players p ON p.id = t.player_id
        WHERE t.season = :season AND p.team_id IS NOT NULL
    """)
    with engine.connect() as conn:
        m = pd.read_sql(map_sql, conn, params={"season": season})
    j = pit_at.merge(m, on="pid")
    # Top-8 by gp per team, gp-weighted average
    j = j.sort_values(["team_id", "gp"], ascending=[True, False])
    j["rank"] = j.groupby("team_id").cumcount()
    j = j[j["rank"] < 8]
    out = (
        j.groupby("team_id").apply(
            lambda g: (g["cam_gbpm_v3_no_sos"] * g["gp"]).sum() / max(g["gp"].sum(), 1)
        )
        .rename("team_cam")
        .reset_index()
    )
    return out


def build_team_cam_panel(engine, pit_df: pd.DataFrame) -> pd.DataFrame:
    """Build a (season, team_id, cutoff_date, team_cam) table by joining pit
    lookup to per-season player→team mapping."""
    panels = []
    for season in pit_df["season"].unique():
        map_sql = text("""
            SELECT t.torvik_pid AS pid, p.team_id AS team_id
            FROM torvik_player_stats t
            JOIN players p ON p.id = t.player_id
            WHERE t.season = :season AND p.team_id IS NOT NULL
        """)
        with engine.connect() as conn:
            m = pd.read_sql(map_sql, conn, params={"season": int(season)})

        s = pit_df[pit_df["season"] == season].merge(m, on="pid")
        # Top-8 by gp per (team_id, cutoff_date)
        s = s.sort_values(["team_id", "cutoff_date", "gp"], ascending=[True, True, False])
        s["rank"] = s.groupby(["team_id", "cutoff_date"]).cumcount()
        s = s[s["rank"] < 8]
        agg = (
            s.groupby(["team_id", "cutoff_date"]).apply(
                lambda g: (g["cam_gbpm_v3_no_sos"] * g["gp"]).sum() / max(g["gp"].sum(), 1)
            )
            .rename("team_pit_cam")
            .reset_index()
        )
        agg["season"] = int(season)
        panels.append(agg)
    return pd.concat(panels, ignore_index=True)


def lookup_team_cam_at_date(panel: pd.DataFrame, season: int, team_id, target_date) -> float:
    """Most-recent cutoff <= target_date for (season, team_id)."""
    sub = panel[(panel["season"] == season) & (panel["team_id"] == team_id) & (panel["cutoff_date"] <= target_date)]
    if len(sub) == 0:
        return np.nan
    return sub.sort_values("cutoff_date").iloc[-1]["team_pit_cam"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--oof", type=Path, default=Path(__file__).parent / "models" / "oof_predictions.csv")
    ap.add_argument("--seasons", type=str, default="2022,2023,2024,2025,2026")
    args = ap.parse_args()

    engine = get_engine()
    print(f"Loading OOF predictions: {args.oof}")
    oof = pd.read_csv(args.oof, parse_dates=["game_date"])
    seasons = [int(s) for s in args.seasons.split(",")]
    oof = oof[oof["season"].isin(seasons)].reset_index(drop=True)
    print(f"  {len(oof)} OOF rows in seasons {seasons}")

    print("Loading pit lookup + building team-pit-cam panel...")
    pit_df = load_pit_lookup()
    panel = build_team_cam_panel(engine, pit_df)
    panel["team_id"] = panel["team_id"].astype(str)
    oof["home_team_id"] = oof["home_team_id"].astype(str)
    oof["away_team_id"] = oof["away_team_id"].astype(str)
    print(f"  panel: {len(panel)} (team, cutoff) rows")

    # Build end-of-season team CamPom (using the last cutoff per season)
    last_per_season = panel.groupby("season")["cutoff_date"].max().to_dict()
    final_panel = panel[
        panel.apply(lambda r: r["cutoff_date"] == last_per_season[r["season"]], axis=1)
    ][["season", "team_id", "team_pit_cam"]].rename(columns={"team_pit_cam": "team_final_cam"})

    print("Computing per-game team CamPom diffs...")
    # Fast bulk lookup via merge_asof per (season, team_id)
    panel_sorted = panel.sort_values(["season", "team_id", "cutoff_date"])

    def attach(side: str):
        col_team = f"{side}_team_id"
        df = oof[["game_id", "season", "game_date", col_team]].copy()
        df = df.rename(columns={col_team: "team_id"})
        # merge_asof needs the on-key globally sorted
        df = df.sort_values("game_date").reset_index(drop=True)
        right = panel_sorted.rename(columns={"cutoff_date": "game_date"}).sort_values("game_date").reset_index(drop=True)
        merged = pd.merge_asof(
            df,
            right,
            on="game_date",
            by=["season", "team_id"],
            direction="backward",
            allow_exact_matches=False,
        )
        return merged[["game_id", "team_pit_cam"]].rename(columns={"team_pit_cam": f"{side}_pit_cam"})

    home_pit = attach("home")
    away_pit = attach("away")

    out = oof.merge(home_pit, on="game_id").merge(away_pit, on="game_id")
    final_panel = final_panel.copy()
    final_panel["team_id"] = final_panel["team_id"].astype(str)
    out = out.merge(
        final_panel.rename(columns={"team_id": "home_team_id", "team_final_cam": "home_final_cam"}),
        on=["season", "home_team_id"], how="left"
    )
    out = out.merge(
        final_panel.rename(columns={"team_id": "away_team_id", "team_final_cam": "away_final_cam"}),
        on=["season", "away_team_id"], how="left"
    )

    # Drop games where we don't have all 4 pit/final values
    before = len(out)
    out = out.dropna(subset=["home_pit_cam", "away_pit_cam", "home_final_cam", "away_final_cam"])
    print(f"  {before} → {len(out)} games after dropping rows with missing CamPom")

    out["pit_diff"] = out["home_pit_cam"] - out["away_pit_cam"]
    out["final_diff"] = out["home_final_cam"] - out["away_final_cam"]
    out["leak_diff"] = out["final_diff"] - out["pit_diff"]

    # Joint regression: pred_margin ~ pit_diff + leak_diff
    from sklearn.linear_model import LinearRegression
    X = out[["pit_diff", "leak_diff"]].values
    y_pred = out["pred_margin"].values
    y_actual = out["actual_margin"].values

    reg_pred = LinearRegression().fit(X, y_pred)
    reg_actual = LinearRegression().fit(X, y_actual)

    print()
    print("=" * 78)
    print(f"  Regression of cstat predicted_margin on pit_diff + leak_diff")
    print("=" * 78)
    print(f"  intercept:                {reg_pred.intercept_:+.3f}")
    print(f"  coef on pit_diff   (good): {reg_pred.coef_[0]:+.3f}  (margin pts per CamPom of pre-game info)")
    print(f"  coef on leak_diff  (BAD):  {reg_pred.coef_[1]:+.3f}  (margin pts per CamPom of LOOKAHEAD info)")
    print(f"  R² (pred ~ X):            {reg_pred.score(X, y_pred):.4f}")

    print()
    print(f"  Same regression but on the ACTUAL margin (sanity baseline):")
    print(f"  coef on pit_diff:    {reg_actual.coef_[0]:+.3f}")
    print(f"  coef on leak_diff:   {reg_actual.coef_[1]:+.3f}")
    print(f"  R²:                  {reg_actual.score(X, y_actual):.4f}")

    print()
    print("  INTERPRETATION:")
    print("    coef(leak_diff) on PRED should be ≈ 0 if the model is honest.")
    print("    If coef(leak_diff) on PRED is similar to coef(pit_diff), the model is")
    print("    weighting future info as heavily as legitimate pre-game info.")
    print()

    # Per-season breakdown
    print("  Per-season:")
    print(f"    {'season':<8}{'n':>6}{'coef pit':>11}{'coef leak':>11}{'leak/pit':>10}")
    for season, sub in out.groupby("season"):
        Xs = sub[["pit_diff", "leak_diff"]].values
        ys = sub["pred_margin"].values
        if len(Xs) < 100:
            continue
        rs = LinearRegression().fit(Xs, ys)
        ratio = rs.coef_[1] / rs.coef_[0] if abs(rs.coef_[0]) > 1e-6 else float('nan')
        print(f"    {season:<8}{len(sub):>6}{rs.coef_[0]:>+11.3f}{rs.coef_[1]:>+11.3f}{ratio:>10.2f}")

    # Persist
    out_path = args.oof.parent / "leakage_quantified.csv"
    out[["game_id", "season", "game_date", "pred_margin", "actual_margin",
         "pit_diff", "final_diff", "leak_diff"]].to_csv(out_path, index=False)
    print(f"\n  Wrote {out_path}")


if __name__ == "__main__":
    main()
