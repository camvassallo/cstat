"""
How many games until a player's archetype stops changing?

Answers the question that gates every in-season archetype plan (ROADMAP Phase 6,
"Archetype in-season cold start"): `training/archetypes.py` qualifies players at
>=10 GP / >=10 MPG, an inherited threshold that has never been validated. If
labels stabilise well before 10 games, that gate costs weeks of coverage at the
start of a season for nothing. If they are still churning at 15, the cold start
is real and must be designed around rather than shortened.

Method — assignment only, never a refit:
  1. Load the shipped combined-cohort model from `archetype_models` (centroids,
     feature_means, feature_stds, cluster_to_class). All 12 season rows share
     one centroid set, so the fit is frozen and identical for every season.
  2. Rebuild each player's 15-feature vector from their FIRST N GAMES ONLY:
       - cstat rate stats: the `compute_player_season_stats` SQL
         (`compute.rs:905-1030`) with a game-rank filter. Formulas mirrored, not
         re-derived.
       - Torvik shot zones: plain sums over `torvik_player_game_stats`.
       - ogbpm/dgbpm: possession-weighted mean of per-game obpm/dbpm, the method
         `compute_campom_at.py:57` already validates at ~0.99 vs the season
         aggregate. Per-game Torvik carries obpm/dbpm; it does NOT carry the
         season table's ogbpm/dgbpm, so this reconstruction is unavoidable and
         is the main source of ceiling loss below.
  3. Standardise with the frozen means/stds, take the nearest centroid, map
     cluster -> class.
  4. Score against that player's actual full-season `player_archetypes` row.

Read the numbers against the CONTROL, not against 100%. The control is this same
pipeline at N=all-games: it isolates reconstruction error (mostly the ogbpm
approximation) from genuine early-season instability. An N-game agreement of 80%
against a 93% control means 86% of the achievable signal, not 80%.

Usage (summary artifacts are tracked under eval_history/ by convention -- do not
write them to the repo root, which is not gitignored for this name):
  cd training && ./.venv/bin/python experiment_archetype_stability.py \
      --seasons 2022,2023,2024,2025,2026 \
      --out eval_history/archetype_stability_YYYYMMDD_summary.json

Re-run after any retrain: the curve is a property of the fitted model, not a
constant. ~30s for five seasons.
"""

import argparse
import json

import numpy as np
import pandas as pd
from sqlalchemy import text

from archetypes import FEATURE_NAMES  # single source of truth for order
from db import get_engine

# N=9999 is the control: every game the player played.
DEFAULT_NS = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 9999]

# Rate stats over a player's first N games. Mirrors compute.rs:905-1030
# (compute_player_season_stats) with the group's game set restricted by rank.
# Grouped by (player_id, team_id) like compute.rs, so a mid-season transfer's
# stints stay separate; the dominant stint is picked later, as archetypes.py does.
RATE_SQL = text("""
WITH pg AS (
    SELECT pgs.*,
           ROW_NUMBER() OVER (PARTITION BY pgs.player_id
                              ORDER BY g.game_date, pgs.game_id) AS gn
    FROM player_game_stats pgs
    JOIN games g ON g.id = pgs.game_id
    WHERE pgs.season = :season
      AND pgs.minutes IS NOT NULL
      AND pgs.minutes > 0
)
SELECT
    pgs.player_id,
    pgs.team_id,
    COUNT(*)                                   AS games_played,
    ROUND(AVG(pgs.minutes)::numeric, 1)::float AS minutes_per_game,
    -- USG% = (Plays x Tm_MP/5) / (MP x Tm_Plays), Plays = FGA + 0.44*FTA + TOV
    CASE WHEN SUM(pgs.minutes) > 0
          AND SUM(COALESCE(tgs.fga,0) + 0.44*COALESCE(tgs.fta,0)
                  + COALESCE(tgs.turnovers,0)) > 0
        THEN (SUM(pgs.fga + 0.44*COALESCE(pgs.fta,0) + COALESCE(pgs.turnovers,0))::float
                * (SUM(COALESCE(tgs.minutes,200))::float / 5.0))
             / (SUM(pgs.minutes)::float
                * SUM(COALESCE(tgs.fga,0) + 0.44*COALESCE(tgs.fta,0)
                      + COALESCE(tgs.turnovers,0))::float)
        ELSE NULL END AS usage_rate_raw,
    -- AST% = AST / ((MP / (Tm_MP/5)) x Tm_FGM - FGM)
    CASE WHEN (5.0*SUM(pgs.minutes)::float * SUM(COALESCE(tgs.fgm,0))::float
               / NULLIF(SUM(COALESCE(tgs.minutes,200))::float,0)
               - SUM(pgs.fgm)::float) > 0
        THEN SUM(pgs.assists)::float / (
                5.0*SUM(pgs.minutes)::float * SUM(COALESCE(tgs.fgm,0))::float
                / NULLIF(SUM(COALESCE(tgs.minutes,200))::float,0)
                - SUM(pgs.fgm)::float)
        ELSE NULL END AS ast_pct_raw,
    -- TOV% = TOV / (FGA + 0.44*FTA + TOV)
    CASE WHEN (SUM(pgs.fga) + 0.44*SUM(COALESCE(pgs.fta,0))
               + SUM(COALESCE(pgs.turnovers,0))) > 0
        THEN SUM(COALESCE(pgs.turnovers,0))::float
             / (SUM(pgs.fga) + 0.44*SUM(COALESCE(pgs.fta,0))
                + SUM(COALESCE(pgs.turnovers,0)))
        ELSE NULL END AS tov_pct_raw,
    -- ORB% = 100 x (ORB x Tm_MP/5) / (MP x (Tm_ORB + Opp_DRB))
    CASE WHEN SUM(pgs.minutes) > 0
          AND SUM(COALESCE(tgs.off_rebounds,0) + COALESCE(opp.def_rebounds,0)) > 0
        THEN 100.0*SUM(COALESCE(pgs.off_rebounds,0))::float
             * (SUM(COALESCE(tgs.minutes,200))::float/5.0)
             / (SUM(pgs.minutes)::float
                * SUM(COALESCE(tgs.off_rebounds,0) + COALESCE(opp.def_rebounds,0))::float)
        ELSE NULL END AS orb_pct_raw,
    -- DRB% = 100 x (DRB x Tm_MP/5) / (MP x (Tm_DRB + Opp_ORB))
    CASE WHEN SUM(pgs.minutes) > 0
          AND SUM(COALESCE(tgs.def_rebounds,0) + COALESCE(opp.off_rebounds,0)) > 0
        THEN 100.0*SUM(COALESCE(pgs.def_rebounds,0))::float
             * (SUM(COALESCE(tgs.minutes,200))::float/5.0)
             / (SUM(pgs.minutes)::float
                * SUM(COALESCE(tgs.def_rebounds,0) + COALESCE(opp.off_rebounds,0))::float)
        ELSE NULL END AS drb_pct_raw,
    -- STL% = 100 x (STL x Tm_MP/5) / (MP x Opp_Poss)
    CASE WHEN SUM(pgs.minutes) > 0
          AND SUM(COALESCE(opp.fga,0) - COALESCE(opp.off_rebounds,0)
                  + COALESCE(opp.turnovers,0) + 0.44*COALESCE(opp.fta,0)) > 0
        THEN 100.0*SUM(COALESCE(pgs.steals,0))::float
             * (SUM(COALESCE(tgs.minutes,200))::float/5.0)
             / (SUM(pgs.minutes)::float
                * SUM(COALESCE(opp.fga,0)::float - COALESCE(opp.off_rebounds,0)::float
                      + COALESCE(opp.turnovers,0)::float + 0.44*COALESCE(opp.fta,0)::float))
        ELSE NULL END AS stl_pct_raw,
    -- BLK% = 100 x (BLK x Tm_MP/5) / (MP x (Opp_FGA - Opp_3PA))
    CASE WHEN SUM(pgs.minutes) > 0
          AND SUM(COALESCE(opp.fga,0) - COALESCE(opp.tpa,0)) > 0
        THEN 100.0*SUM(COALESCE(pgs.blocks,0))::float
             * (SUM(COALESCE(tgs.minutes,200))::float/5.0)
             / (SUM(pgs.minutes)::float
                * SUM(COALESCE(opp.fga,0) - COALESCE(opp.tpa,0))::float)
        ELSE NULL END AS blk_pct_raw,
    -- FT Rate = FTA / FGA
    CASE WHEN SUM(pgs.fga) > 0
        THEN SUM(COALESCE(pgs.fta,0))::float / SUM(pgs.fga)::float
        ELSE NULL END AS ft_rate_raw
FROM pg pgs
LEFT JOIN team_game_stats tgs
       ON tgs.game_id = pgs.game_id AND tgs.team_id = pgs.team_id
LEFT JOIN team_game_stats opp
       ON opp.game_id = pgs.game_id AND opp.team_id = pgs.opponent_id
WHERE pgs.gn <= :n
GROUP BY pgs.player_id, pgs.team_id
""")

# compute.rs stores each rate stat ROUNDed; matching that precision keeps the
# control measuring ogbpm reconstruction error rather than rounding drift.
_ROUNDING = {"usage_rate": 3, "ast_pct": 3, "tov_pct": 3, "ft_rate": 3,
             "orb_pct": 1, "drb_pct": 1, "stl_pct": 1, "blk_pct": 1}

# Torvik half over the same first-N window. ogbpm/dgbpm are possession-weighted
# means of per-game obpm/dbpm -- see compute_campom_at.py:57.
TORVIK_SQL = text("""
WITH tg AS (
    SELECT t.*,
           ROW_NUMBER() OVER (PARTITION BY t.pid ORDER BY t.game_date, t.game_uid) AS gn
    FROM torvik_player_game_stats t
    WHERE t.season = :season
)
SELECT
    pid,
    SUM(COALESCE(rim_attempted,0))             AS rim_attempted,
    SUM(COALESCE(mid_attempted,0))             AS mid_attempted,
    SUM(COALESCE(tpa,0))                       AS tpa,
    SUM(obpm * COALESCE(possessions,0)) / NULLIF(SUM(COALESCE(possessions,0)),0) AS ogbpm,
    SUM(dbpm * COALESCE(possessions,0)) / NULLIF(SUM(COALESCE(possessions,0)),0) AS dgbpm
FROM tg
WHERE gn <= :n
GROUP BY pid
""")

# Truth: the shipped full-season archetype, plus the torvik_pid bridge from the
# cstat player uuid to Torvik's per-game rows.
TRUTH_SQL = text("""
SELECT DISTINCT ON (pa.player_id)
       pa.player_id, pa.primary_class, pa.secondary_class, tps.torvik_pid AS pid
FROM player_archetypes pa
JOIN torvik_player_stats tps
  ON tps.player_id = pa.player_id AND tps.season = pa.season
WHERE pa.season = :season AND tps.torvik_pid IS NOT NULL
ORDER BY pa.player_id, tps.total_minutes DESC NULLS LAST
""")


def load_model(engine, season):
    row = pd.read_sql(
        text("""SELECT centroids, feature_means, feature_stds, cluster_to_class,
                       feature_names
                FROM archetype_models WHERE season = :season"""),
        engine, params={"season": season},
    )
    if row.empty:
        raise SystemExit(f"no archetype_models row for season {season}")
    r = row.iloc[0]
    j = lambda v: v if isinstance(v, (list, dict)) else json.loads(v)

    names = j(r["feature_names"])
    if list(names) != list(FEATURE_NAMES):
        raise SystemExit(
            f"model feature order != archetypes.FEATURE_NAMES:\n{names}\n{FEATURE_NAMES}"
        )

    # Stored shapes (see archetypes.py::write_results):
    #   centroids       {cluster_id: {"class": name, "vector": [...]}}
    #   feature_means   {feature_name: value}   <- keyed by NAME, not position
    #   cluster_to_class{cluster_id: class_name}
    cents = j(r["centroids"])
    k = max(int(i) for i in cents) + 1
    centroids = np.asarray([cents[str(i)]["vector"] for i in range(k)], dtype=float)

    means_d, stds_d = j(r["feature_means"]), j(r["feature_stds"])
    means = np.asarray([means_d[f] for f in FEATURE_NAMES], dtype=float)
    stds = np.asarray([stds_d[f] for f in FEATURE_NAMES], dtype=float)

    c2c = {int(i): v for i, v in j(r["cluster_to_class"]).items()}
    # The centroid payload carries its own class label; if the two disagree the
    # model is internally inconsistent and every assignment below is suspect.
    for i in range(k):
        if cents[str(i)]["class"] != c2c[i]:
            raise SystemExit(f"cluster {i}: centroid class != cluster_to_class")
    return centroids, means, stds, c2c


def features_at(engine, season, n):
    """Build the 15-feature frame from each player's first N games."""
    rate = pd.read_sql(RATE_SQL, engine, params={"season": season, "n": n})
    if rate.empty:
        return rate
    # compute.rs ROUNDs each rate stat before storing it, so player_season_stats
    # holds the rounded value and that is what the shipped model was fit on.
    # Round identically here or the control conflates rounding drift with the
    # ogbpm reconstruction error it is meant to isolate.
    for col, prec in _ROUNDING.items():
        rate[col] = rate[f"{col}_raw"].astype(float).round(prec)
    rate = rate.drop(columns=[f"{c}_raw" for c in _ROUNDING])
    # Dominant stint per player, mirroring archetypes.py's rn=1 ordering.
    rate["_w"] = rate["games_played"] * rate["minutes_per_game"]
    rate = (rate.sort_values("_w", ascending=False)
                .drop_duplicates("player_id").drop(columns="_w"))
    tor = pd.read_sql(TORVIK_SQL, engine, params={"season": season, "n": n})
    truth = pd.read_sql(TRUTH_SQL, engine, params={"season": season})

    df = truth.merge(rate, on="player_id", how="inner").merge(tor, on="pid", how="inner")
    fga = (df["rim_attempted"] + df["mid_attempted"] + df["tpa"]).replace(0, np.nan)
    df["rim_share"] = df["rim_attempted"] / fga
    df["mid_share"] = df["mid_attempted"] / fga
    df["three_share"] = df["tpa"] / fga
    df["min_share"] = df["minutes_per_game"] / 40.0
    return df.dropna(subset=FEATURE_NAMES).reset_index(drop=True)


def assign(df, centroids, means, stds, cluster_to_class):
    """Standardise with the frozen scaler, take the nearest centroid."""
    X = (df[FEATURE_NAMES].to_numpy(dtype=float) - means) / stds
    d = np.linalg.norm(X[:, None, :] - centroids[None, :, :], axis=-1)
    return np.array([cluster_to_class[int(c)] for c in d.argmin(axis=1)])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seasons", default="2023,2024,2025,2026")
    ap.add_argument("--ns", default=",".join(str(n) for n in DEFAULT_NS))
    ap.add_argument("--out")
    a = ap.parse_args()
    seasons = [int(s) for s in a.seasons.split(",") if s.strip()]
    ns = [int(n) for n in a.ns.split(",") if n.strip()]
    engine = get_engine()

    rows = []
    for season in seasons:
        centroids, means, stds, c2c = load_model(engine, season)
        for n in ns:
            df = features_at(engine, season, n)
            if df.empty:
                continue
            pred = assign(df, centroids, means, stds, c2c)
            primary_hit = (pred == df["primary_class"].to_numpy())
            top2_hit = primary_hit | (pred == df["secondary_class"].to_numpy())
            rows.append({
                "season": season, "n": n, "players": int(len(df)),
                "pct_primary": round(100.0 * primary_hit.mean(), 1),
                "pct_top2": round(100.0 * top2_hit.mean(), 1),
            })
            label = "ALL (control)" if n == 9999 else f"N={n}"
            print(f"  {season}  {label:>14}  n={len(df):5d}  "
                  f"primary={rows[-1]['pct_primary']:5.1f}%  top2={rows[-1]['pct_top2']:5.1f}%")

    res = pd.DataFrame(rows)
    print("\n=== pooled across seasons (agreement with the full-season label) ===")
    pooled = (res.groupby("n")
                 .apply(lambda g: pd.Series({
                     "players": int(g["players"].mean()),
                     "pct_primary": round(float(np.average(g["pct_primary"], weights=g["players"])), 1),
                     "pct_top2": round(float(np.average(g["pct_top2"], weights=g["players"])), 1),
                 }), include_groups=False)
                 .reset_index())
    ctrl = pooled.loc[pooled["n"] == 9999, "pct_primary"]
    ceiling = float(ctrl.iloc[0]) if len(ctrl) else float("nan")
    for _, r in pooled.iterrows():
        label = "ALL (control)" if r["n"] == 9999 else f"N={int(r['n'])}"
        frac = "" if not ceiling == ceiling else f"  ({100.0*r['pct_primary']/ceiling:5.1f}% of ceiling)"
        print(f"  {label:>14}  primary={r['pct_primary']:5.1f}%  top2={r['pct_top2']:5.1f}%{frac}")
    print(f"\n  CONTROL (reconstruction ceiling) = {ceiling}% — read every row against this, not 100%.")

    if a.out:
        json.dump({"per_season": rows, "pooled": pooled.to_dict("records"),
                   "ceiling_pct_primary": ceiling},
                  open(a.out, "w"), indent=2)
        print(f"  wrote {a.out}")


if __name__ == "__main__":
    main()
