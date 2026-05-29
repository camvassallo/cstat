"""
Precompute a point-in-time CamPom v3 lookup table.

For each (season, cutoff_date) in a regular grid (default: every 2 weeks
during play), computes pit cam_gbpm_v3_no_sos for every player and
persists to `pit_cam_v3_lookup.parquet`. This lets the LightGBM training
loop look up "what was player X's CamPom as of game date D?" in O(1)
without re-aggregating per query.

Grid: builds cutoff dates from min game_date to max game_date in
torvik_player_game_stats per season, spaced ~14 days apart. For any
target game on date D, callers look up the most recent cutoff <= D-1
(strictly before the game).

Output schema: (season, pid, cutoff_date, cam_gbpm_v3_no_sos, gp, ogbpm,
dgbpm, usg, min_pct). ~12 seasons × 12 cutoffs × ~5000 players ≈ 720k
rows. Parquet keeps it under 30 MB.
"""

import argparse
from datetime import timedelta
from pathlib import Path

import pandas as pd
from sqlalchemy import text

from compute_campom_at import compute_at
from db import get_engine

DEFAULT_CADENCE_DAYS = 14
OUTPUT_PATH = Path(__file__).parent / "models" / "pit_cam_v3_lookup.csv.gz"


def season_date_range(engine, season: int):
    sql = text("""
        SELECT MIN(game_date) AS lo, MAX(game_date) AS hi
        FROM torvik_player_game_stats WHERE season = :season
    """)
    with engine.connect() as conn:
        row = conn.execute(sql, {"season": season}).fetchone()
    return row[0], row[1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seasons", type=str, default="2015,2016,2017,2018,2019,2020,2021,2022,2023,2024,2025,2026")
    ap.add_argument("--cadence", type=int, default=DEFAULT_CADENCE_DAYS)
    ap.add_argument("--output", type=Path, default=OUTPUT_PATH)
    args = ap.parse_args()

    seasons = [int(s) for s in args.seasons.split(",")]
    engine = get_engine()

    frames = []
    for s in seasons:
        lo, hi = season_date_range(engine, s)
        if lo is None:
            print(f"  [{s}] no data; skipping")
            continue
        cutoffs = []
        cur = lo + timedelta(days=args.cadence)  # first cutoff is 2 weeks in
        while cur <= hi:
            cutoffs.append(cur)
            cur += timedelta(days=args.cadence)
        if not cutoffs or cutoffs[-1] != hi:
            cutoffs.append(hi)
        print(f"  [{s}] {len(cutoffs)} cutoffs from {cutoffs[0]} to {cutoffs[-1]}")
        for c in cutoffs:
            cutoff_str = c.strftime("%Y-%m-%d")
            df = compute_at(engine, s, cutoff_str)
            if len(df) == 0:
                continue
            df["cutoff_date"] = pd.to_datetime(cutoff_str)
            frames.append(
                df[["pid", "season", "cutoff_date", "gp",
                    "ogbpm", "dgbpm", "usg", "min_pct", "cam_gbpm_v3_no_sos"]]
            )

    if not frames:
        print("No frames; aborting.")
        return

    big = pd.concat(frames, ignore_index=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    big.to_csv(args.output, index=False, compression="gzip")
    print(f"\nWrote {args.output}  ({len(big)} rows, {big.memory_usage(deep=True).sum()/1e6:.1f} MB in-mem)")
    print(f"  seasons: {sorted(big['season'].unique().tolist())}")
    print(f"  cutoffs per season: {big.groupby('season')['cutoff_date'].nunique().to_dict()}")
    print(f"  players per season (avg): {big.groupby('season')['pid'].nunique().mean():.0f}")


if __name__ == "__main__":
    main()
