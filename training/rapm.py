"""
Production RAPM fit — populates the `player_rapm` table (migration 038).

The display-only "Adj on/off (RAPM)" surface: a possession-weighted ridge
regression of per-stint scoring margin (per 100 possessions) on the on-floor
indicators of all ten players, fit per season on the opponent-paired stints
in `lineup_stints`. Methodology, spike evidence, and the narrowed scope
decision (companion line to raw on/off in the UI — NOT a CamPom rival, NOT
an ML feature) live in docs/rapm_methodology.md sections 4 and 8.

Configuration is the spike-validated zero-prior fit at lambda=1000 (the
game-blocked CV optimum on 2026; the curve is flat-topped, and per-season
re-tuning moved nothing in the spike). The CamPom-prior variant exists in
experiment_rapm_spike.py only — it inherits CamPom's stability, which would
defeat the point of an independent stint-evidence line next to CamPom.

Corpus: stints where BOTH lineups are exactly 5 and possessions_for > 0,
from sources replay / onfloor / replay_shadow. The shadow label is the
replay reconstruction of natstat-covered team-games (compute_pbp_lineups
emits it precisely so this corpus survives the Tier-2 source swap); the
per-game natstat units themselves are unpaired and are NOT observations.
2019 has no paired source (corrupt replay) and legitimately has no rows.

Each stint contributes one offense observation per team perspective (the
table already stores both): y = 100 * points_for / possessions_for, weight
= possessions_for, +1 on the five offensive players' O columns and the five
defenders' D columns, plus unpenalized intercept and a home-offense (HCA)
indicator. o_rapm higher-better; d_rapm = points allowed, lower-better;
net = o - d.

Team attribution for the output row reuses the player's canonical
players.team_id (the box-score authority, same convention as player_on_off).

Run: cd training && python rapm.py [--seasons 2015,2016,...]
Writes per-season atomically (delete + insert in one transaction).
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone

import numpy as np
import pandas as pd
from scipy import sparse
from sklearn.linear_model import Ridge
from sqlalchemy import text

from db import get_engine

LAMBDA = 1000.0
PRIOR = "zero"

SEASONS_QUERY = """
SELECT DISTINCT season FROM lineup_stints
WHERE source IN ('replay', 'onfloor', 'replay_shadow')
ORDER BY season
"""

STINT_QUERY = """
SELECT ls.game_id::text      AS game_id,
       ls.team_id::text      AS team_id,
       ls.lineup::text[]     AS lineup,
       ls.opp_lineup::text[] AS opp_lineup,
       ls.points_for         AS points_for,
       ls.possessions_for    AS possessions_for,
       (ls.team_id = g.home_team_id AND NOT g.is_neutral_site)::int AS home_offense
FROM lineup_stints ls
JOIN games g ON g.id = ls.game_id
WHERE ls.season = %(season)s
  AND ls.source IN ('replay', 'onfloor', 'replay_shadow')
  AND array_length(ls.lineup, 1) = 5
  AND array_length(ls.opp_lineup, 1) = 5
  AND ls.possessions_for > 0
"""

TEAM_QUERY = """
SELECT id::text AS player_id, team_id::text AS team_id
FROM players WHERE season = %(season)s AND team_id IS NOT NULL
"""


def fit_season(engine, season: int) -> pd.DataFrame | None:
    df = pd.read_sql(STINT_QUERY, engine, params={"season": season})
    if df.empty:
        print(f"season {season}: no paired stints — skipped")
        return None

    # Collapse duplicate (game, lineups, side) rows — identical weighted
    # solution, smaller solve.
    df["lineup_key"] = df["lineup"].map(lambda a: "|".join(sorted(a)))
    df["opp_key"] = df["opp_lineup"].map(lambda a: "|".join(sorted(a)))
    df = (
        df.groupby(["game_id", "team_id", "lineup_key", "opp_key",
                    "home_offense"], as_index=False)
        .agg(points_for=("points_for", "sum"),
             possessions_for=("possessions_for", "sum"),
             stints=("points_for", "size"))
    )
    y = (100.0 * df["points_for"] / df["possessions_for"]).to_numpy()
    w = df["possessions_for"].to_numpy()

    players = sorted(
        {p for key in df["lineup_key"] for p in key.split("|")}
        | {p for key in df["opp_key"] for p in key.split("|")}
    )
    pidx = {p: i for i, p in enumerate(players)}
    n = len(players)

    rows, cols = [], []
    for i, (lk, ok, home) in enumerate(
        zip(df["lineup_key"], df["opp_key"], df["home_offense"])
    ):
        for p in lk.split("|"):
            rows.append(i)
            cols.append(pidx[p])
        for p in ok.split("|"):
            rows.append(i)
            cols.append(n + pidx[p])
        if home:
            rows.append(i)
            cols.append(2 * n)
    x = sparse.csr_matrix(
        (np.ones(len(rows)), (rows, cols)), shape=(len(df), 2 * n + 1)
    )

    model = Ridge(alpha=LAMBDA, fit_intercept=True, solver="sparse_cg")
    model.fit(x, y, sample_weight=w)
    coef = model.coef_

    poss = {p: 0.0 for p in players}
    stint_n = {p: 0 for p in players}
    for lk, ok, pf, sn in zip(df["lineup_key"], df["opp_key"],
                              df["possessions_for"], df["stints"]):
        for p in lk.split("|"):
            poss[p] += pf
            stint_n[p] += int(sn)
        for p in ok.split("|"):
            poss[p] += pf
            stint_n[p] += int(sn)

    out = pd.DataFrame({
        "player_id": players,
        "o_rapm": coef[:n],
        "d_rapm": coef[n:2 * n],
        "net_rapm": coef[:n] - coef[n:2 * n],
        "paired_possessions": [poss[p] for p in players],
        "stint_count": [stint_n[p] for p in players],
    })
    print(f"season {season}: {len(df):,} collapsed rows, {n:,} players, "
          f"intercept {model.intercept_:.2f}, HCA {coef[2 * n]:+.2f}")
    return out


def write_season(engine, season: int, out: pd.DataFrame) -> int:
    teams = pd.read_sql(TEAM_QUERY, engine, params={"season": season})
    out = out.merge(teams, on="player_id", how="left")
    fitted_at = datetime.now(timezone.utc).replace(tzinfo=None)
    with engine.begin() as conn:
        conn.execute(text("DELETE FROM player_rapm WHERE season = :s"),
                     {"s": season})
        conn.execute(
            text(
                "INSERT INTO player_rapm (season, player_id, team_id, o_rapm,"
                " d_rapm, net_rapm, paired_possessions, stint_count, lambda,"
                " prior, fitted_at)"
                " VALUES (:season, CAST(:player_id AS uuid),"
                " CAST(:team_id AS uuid), :o, :d, :net, :poss, :stints,"
                " :lam, :prior, :fitted_at)"
            ),
            [
                {
                    "season": season,
                    "player_id": r.player_id,
                    "team_id": r.team_id if pd.notna(r.team_id) else None,
                    "o": float(r.o_rapm),
                    "d": float(r.d_rapm),
                    "net": float(r.net_rapm),
                    "poss": float(r.paired_possessions),
                    "stints": int(r.stint_count),
                    "lam": LAMBDA,
                    "prior": PRIOR,
                    "fitted_at": fitted_at,
                }
                for r in out.itertuples()
            ],
        )
    return len(out)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seasons", help="comma-separated; default: all with "
                    "paired stints")
    args = ap.parse_args()

    engine = get_engine()
    if args.seasons:
        seasons = [int(s) for s in args.seasons.split(",")]
    else:
        seasons = pd.read_sql(SEASONS_QUERY, engine)["season"].tolist()

    total = 0
    for season in seasons:
        out = fit_season(engine, season)
        if out is None:
            continue
        n = write_season(engine, season, out)
        total += n
        print(f"  -> wrote {n:,} player_rapm rows")
    print(f"\nDone: {total:,} rows across {len(seasons)} seasons")


if __name__ == "__main__":
    main()
