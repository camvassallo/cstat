"""
Production RAPM fit — populates the `player_rapm` table (migration 038).

The display-only "Adj on/off (RAPM)" surface: a possession-weighted ridge
regression of per-stint scoring margin (per 100 possessions) on the on-floor
indicators of all ten players, fit on the opponent-paired stints in
`lineup_stints`. Methodology, the spike evidence, and the scope decisions
live in docs/rapm_methodology.md sections 4, 8, and 10.

**Default mode is the POOLED fit (shipped 2026-06-12, doc section 10.7):**
for each target season, a decayed 3-season window (the target season at
weight 1, the prior at 0.7, the one before at 0.49; 2019 is skipped — no
paired stints) with coefficients keyed by CAREER (dual-key chains:
natstat_id OR torvik_pid union-find; unresolvable players stay season-scoped
singletons). lambda=2000, the pooled CV/prequential optimum. Versus the
single-season fit this lifts split-half reliability ~30-50% (net 0.295 ->
0.382) and softens star compression; the prequentially-optimal flat pooling
(decay 1.0) was deliberately NOT shipped — recency weighting keeps the
displayed number anchored to the season the page is showing. Earliest
seasons degrade gracefully (2015's window is just 2015 — the single-season
fit at lambda 2000).

`--single` restores the original one-season fit (lambda=1000) for
comparison runs; the `prior` column records which config produced each row.

Corpus: stints where BOTH lineups are exactly 5 and possessions_for > 0,
from sources replay / onfloor / replay_shadow (the shadow label keeps
natstat-covered team-games' replay rows for exactly this fit; the per-game
natstat units are unpaired and are NOT observations).

Per-season output rows cover the players appearing in that season's stints;
the value is the career coefficient from the window fit ending at that
season. `paired_possessions` / `stint_count` are the career's totals across
the fit window (identification sample — the UI's ~250 display floor reads
this), not single-season totals.

Team attribution reuses the player's canonical players.team_id (the
box-score authority, same convention as player_on_off).

Run: cd training && python rapm.py [--seasons 2015,...] [--single]
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

POOLED_WINDOW = 3
POOLED_DECAY = 0.7
POOLED_LAMBDA = 2000.0
POOLED_PRIOR = "pooled_w3_d0.7"
SINGLE_LAMBDA = 1000.0
SINGLE_PRIOR = "zero"
CORRUPT_SEASONS = {2019}

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

KEYS_QUERY = """
SELECT p.id::text AS player_id, p.natstat_id, tps.torvik_pid
FROM players p
LEFT JOIN torvik_player_stats tps
       ON tps.player_id = p.id AND tps.season = p.season
WHERE p.season = ANY(%(seasons)s)
"""

TEAM_QUERY = """
SELECT id::text AS player_id, team_id::text AS team_id
FROM players WHERE season = %(season)s AND team_id IS NOT NULL
"""


def load_stints(engine, season: int) -> pd.DataFrame | None:
    df = pd.read_sql(STINT_QUERY, engine, params={"season": season})
    if df.empty:
        return None
    df["lineup_key"] = df["lineup"].map(lambda a: "|".join(sorted(a)))
    df["opp_key"] = df["opp_lineup"].map(lambda a: "|".join(sorted(a)))
    df = (
        df.groupby(["game_id", "team_id", "lineup_key", "opp_key",
                    "home_offense"], as_index=False)
        .agg(points_for=("points_for", "sum"),
             possessions_for=("possessions_for", "sum"),
             stints=("points_for", "size"))
    )
    df["y"] = 100.0 * df["points_for"] / df["possessions_for"]
    return df


def career_map(engine, seasons: list[int]) -> dict:
    """Union-find over season-scoped player ids (natstat_id OR torvik_pid)."""
    df = pd.read_sql(KEYS_QUERY, engine, params={"seasons": list(seasons)})
    parent: dict = {}

    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    for key in ("natstat_id", "torvik_pid"):
        sub = df.dropna(subset=[key])
        for _, grp in sub.groupby(key):
            ids = grp["player_id"].tolist()
            for other in ids[1:]:
                union(ids[0], other)
    return {pid: find(pid) for pid in df["player_id"]}


def window_seasons(target: int, available: set[int]) -> list[int]:
    """The pooled fit window ending at `target`: up to POOLED_WINDOW seasons,
    skipping corrupt/absent ones."""
    out = []
    s = target
    while len(out) < POOLED_WINDOW and s >= 2015:
        if s not in CORRUPT_SEASONS and s in available:
            out.append(s)
        s -= 1
    return sorted(out)


def fit_window(frames: dict, seasons: list[int], decay: float, lam: float,
               pid2career: dict):
    """Decayed-window ridge over career columns (O block | D block | HCA).
    Returns (o_eff, d_eff, poss, stint_n) dicts keyed by career."""
    parts = []
    newest = max(seasons)
    for s in seasons:
        f = frames[s].copy()
        f["wmult"] = decay ** (newest - s)
        parts.append(f)
    df = pd.concat(parts, ignore_index=True)

    careers = sorted(
        {pid2career.get(p, p) for k in df["lineup_key"] for p in k.split("|")}
        | {pid2career.get(p, p) for k in df["opp_key"] for p in k.split("|")}
    )
    cidx = {c: i for i, c in enumerate(careers)}
    n = len(careers)
    rows, cols = [], []
    for i, (lk, ok, home) in enumerate(
        zip(df["lineup_key"], df["opp_key"], df["home_offense"])
    ):
        for p in lk.split("|"):
            rows.append(i)
            cols.append(cidx[pid2career.get(p, p)])
        for p in ok.split("|"):
            rows.append(i)
            cols.append(n + cidx[pid2career.get(p, p)])
        if home:
            rows.append(i)
            cols.append(2 * n)
    x = sparse.csr_matrix(
        (np.ones(len(rows)), (rows, cols)), shape=(len(df), 2 * n + 1)
    )
    model = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
    model.fit(x, df["y"].to_numpy(),
              sample_weight=(df["possessions_for"] * df["wmult"]).to_numpy())

    # Identification sample per career: PLAIN (undecayed) window totals —
    # the display floor reads this as "how much evidence backs the number".
    poss: dict = {}
    stint_n: dict = {}
    for lk, ok, pf, sn in zip(df["lineup_key"], df["opp_key"],
                              df["possessions_for"], df["stints"]):
        for p in lk.split("|"):
            c = pid2career.get(p, p)
            poss[c] = poss.get(c, 0.0) + pf
            stint_n[c] = stint_n.get(c, 0) + int(sn)
        for p in ok.split("|"):
            c = pid2career.get(p, p)
            poss[c] = poss.get(c, 0.0) + pf
            stint_n[c] = stint_n.get(c, 0) + int(sn)

    o_eff = dict(zip(careers, model.coef_[:n]))
    d_eff = dict(zip(careers, model.coef_[n:2 * n]))
    print(f"    window {seasons} decay {decay} lambda {int(lam)}: "
          f"{len(df):,} rows, {n:,} careers, intercept "
          f"{model.intercept_:.2f}, HCA {model.coef_[2 * n]:+.2f}")
    return o_eff, d_eff, poss, stint_n


def season_players(frame: pd.DataFrame) -> set:
    """Season-scoped player ids appearing in the season's own stints."""
    out: set = set()
    for lk, ok in zip(frame["lineup_key"], frame["opp_key"]):
        out.update(lk.split("|"))
        out.update(ok.split("|"))
    return out


def write_season(engine, season: int, out: pd.DataFrame, lam: float,
                 prior: str) -> int:
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
                    "lam": lam,
                    "prior": prior,
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
    ap.add_argument("--single", action="store_true",
                    help="original single-season fit (lambda=1000) instead "
                    "of the shipped pooled config")
    args = ap.parse_args()

    engine = get_engine()
    available = set(
        pd.read_sql(SEASONS_QUERY, engine)["season"].tolist())
    targets = ([int(s) for s in args.seasons.split(",")] if args.seasons
               else sorted(available))

    # Career chains span every season any window touches.
    needed = set()
    for t in targets:
        needed.update(window_seasons(t, available) if not args.single else [t])
    pid2career = (career_map(engine, sorted(needed)) if not args.single
                  else {})
    if not args.single:
        print(f"Career chains over {sorted(needed)}: "
              f"{len(pid2career):,} player-seasons -> "
              f"{len(set(pid2career.values())):,} careers")

    frames: dict = {}

    def frame(s):
        if s not in frames:
            frames[s] = load_stints(engine, s)
        return frames[s]

    total = 0
    for season in targets:
        if frame(season) is None:
            print(f"season {season}: no paired stints — skipped")
            continue
        if args.single:
            window, decay, lam, prior = [season], 1.0, SINGLE_LAMBDA, SINGLE_PRIOR
        else:
            window = window_seasons(season, available)
            decay, lam, prior = POOLED_DECAY, POOLED_LAMBDA, POOLED_PRIOR
        print(f"season {season}:")
        sub = {s: frame(s) for s in window}
        o_eff, d_eff, poss, stint_n = fit_window(
            sub, window, decay, lam, pid2career)

        pids = sorted(season_players(frame(season)))
        rows = []
        for pid in pids:
            c = pid2career.get(pid, pid)
            if c not in o_eff:
                continue
            rows.append({
                "player_id": pid,
                "o_rapm": o_eff[c],
                "d_rapm": d_eff[c],
                "net_rapm": o_eff[c] - d_eff[c],
                "paired_possessions": poss.get(c, 0.0),
                "stint_count": stint_n.get(c, 0),
            })
        n = write_season(engine, season, pd.DataFrame(rows), lam, prior)
        total += n
        print(f"  -> wrote {n:,} player_rapm rows ({prior})")
    print(f"\nDone: {total:,} rows across {len(targets)} seasons")


if __name__ == "__main__":
    main()
