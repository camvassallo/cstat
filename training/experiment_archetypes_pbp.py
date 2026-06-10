"""
Tier-1 PBP style features → archetype clustering: accept/reject experiment.

Adds the PBP context-scoring style signals (transition / 2nd-chance /
points-off-TO / fouls-drawn per-40, paint finishing FG%) to the archetype
feature matrix and re-clusters the combined 12-season cohort. paint_rate is
deliberately excluded — rim/mid/three shot-zone shares already carry shot
location.

Coverage problem this experiment exists to measure: contextual tags exist
2020+ only, so 2015-2019 player-seasons (~40% of the cohort) get
mean-imputed (z=0) on every PBP axis. The risk is era-artifact clusters —
pre-2020 players pinned to the centroid on 5 of 19 axes.

Decision metrics (methodology doc):
  - returning-player primary stability (canonical; tripwire < 40%),
    split into pre-2020 pairs vs 2020+ pairs to expose era artifacts
  - signature-alignment violations (must stay 0)
  - primary-or-secondary match rate

Comparison script only; does not write to player_archetypes.
"""

from __future__ import annotations

import numpy as np
import pandas as pd
from scipy.special import softmax
from sklearn.cluster import KMeans
from sqlalchemy import bindparam, text

import archetypes as base
from db import get_engine

SEASONS = list(range(2015, 2027))

PBP_STYLE_FEATURES = [
    "transition_pts_per40",
    "second_chance_pts_per40",
    "points_off_turnovers_per40",
    "fouls_drawn_per40",
    "paint_fg_pct",
]


def fetch_pbp_rates(engine, seasons: list[int]) -> pd.DataFrame:
    """Season PBP rates for the dominant pss row — same ranking the archetype
    fetch uses (gp × mpg DESC), so the merge keys line up."""
    query = text("""
        SELECT DISTINCT ON (player_id, season)
            player_id, season,
            transition_pts_per40, second_chance_pts_per40,
            points_off_turnovers_per40, fouls_drawn_per40, paint_fg_pct
        FROM player_season_stats
        WHERE season IN :seasons
        ORDER BY player_id, season, (games_played * minutes_per_game) DESC NULLS LAST
    """).bindparams(bindparam("seasons", expanding=True))
    return pd.read_sql(query, engine, params={"seasons": seasons})


def cluster(df: pd.DataFrame, feature_names: list[str]):
    """NaN-tolerant analog of archetypes.cluster_and_assign: z-score with
    nan-aware moments, impute missing to 0 (= per-feature mean), k-means with
    the same K/seed/n_init, Hungarian-match via the production signature code
    (extra features simply carry zero signature weight)."""
    raw = df[feature_names].to_numpy(dtype=np.float64)
    means = np.nanmean(raw, axis=0)
    stds = np.nanstd(raw, axis=0)
    X = np.nan_to_num((raw - means) / stds, nan=0.0)

    km = KMeans(n_clusters=base.K, random_state=42, n_init=20)
    labels = km.fit_predict(X)
    centroids = km.cluster_centers_

    saved = base.FEATURE_NAMES
    base.FEATURE_NAMES = feature_names
    try:
        cluster_to_class = base.match_clusters_to_classes(centroids)
        violations = base.verify_signature_alignment(centroids, cluster_to_class)
    finally:
        base.FEATURE_NAMES = saved

    dists = np.linalg.norm(X[:, None, :] - centroids[None, :, :], axis=-1)
    aff = softmax(-dists / 1.5, axis=1)
    order = np.argsort(-aff, axis=1)
    primary = np.array([cluster_to_class[j] for j in order[:, 0]])
    secondary = np.array([cluster_to_class[j] for j in order[:, 1]])
    return primary, secondary, violations


def stability(df: pd.DataFrame, primary: np.ndarray, secondary: np.ndarray) -> dict:
    d = df[["torvik_pid", "season"]].copy()
    d["primary"] = primary
    d["secondary"] = secondary
    d = d[d["torvik_pid"].notna()]
    nxt = d.copy()
    nxt["season"] -= 1
    pairs = d.merge(nxt, on=["torvik_pid", "season"], suffixes=("_n", "_np1"))

    def rates(p: pd.DataFrame) -> tuple[float, float, int]:
        if len(p) == 0:
            return float("nan"), float("nan"), 0
        prim = (p["primary_n"] == p["primary_np1"]).mean()
        either = (
            (p["primary_n"] == p["primary_np1"])
            | (p["primary_n"] == p["secondary_np1"])
            | (p["secondary_n"] == p["primary_np1"])
        ).mean()
        return float(prim), float(either), len(p)

    out = {"pooled": rates(pairs)}
    out["pre2020"] = rates(pairs[pairs["season"] < 2019])   # s_n+1 <= 2019
    out["post2020"] = rates(pairs[pairs["season"] >= 2020])  # both seasons covered
    out["boundary"] = rates(pairs[pairs["season"].isin([2019])])
    return out


def main() -> None:
    engine = get_engine()
    print(f"Fetching qualified player-seasons for {SEASONS}…")
    df = base.fetch_player_features(engine, SEASONS)
    print(f"  {len(df):,} player-seasons")

    # torvik_pid for the stability join
    pid_map = pd.read_sql(
        text("""SELECT DISTINCT ON (player_id, season) player_id::text AS player_id_str,
                       season, torvik_pid
                FROM torvik_player_stats
                WHERE season IN :seasons AND torvik_pid IS NOT NULL AND player_id IS NOT NULL
                ORDER BY player_id, season, total_minutes DESC NULLS LAST
             """).bindparams(bindparam("seasons", expanding=True)),
        engine, params={"seasons": SEASONS},
    )
    df = df.merge(pid_map, on=["player_id_str", "season"], how="left")

    pbp = fetch_pbp_rates(engine, SEASONS)
    pbp["player_id_str"] = pbp["player_id"].astype(str)
    df = df.merge(
        pbp.drop(columns=["player_id"]), on=["player_id_str", "season"], how="left",
    )
    cov = df[PBP_STYLE_FEATURES[0]].notna().groupby(df["season"]).mean()
    print("PBP style-feature coverage by season:")
    print((cov * 100).round(1).to_string())

    results = {}
    for name, feats in (
        ("baseline (14)", list(base.FEATURE_NAMES)),
        ("+PBP style (19)", list(base.FEATURE_NAMES) + PBP_STYLE_FEATURES),
    ):
        print(f"\n{'=' * 64}\n{name}\n{'=' * 64}")
        primary, secondary, violations = cluster(df, feats)
        st = stability(df, primary, secondary)
        print(f"  signature violations: {len(violations)}")
        for v in violations:
            print(f"    {v}")
        for k, (prim, either, n) in st.items():
            print(f"  stability[{k:<9}] primary {prim:.3f}  prim-or-sec {either:.3f}  n={n}")
        results[name] = (st, len(violations))

    b, v = results["baseline (14)"], results["+PBP style (19)"]
    print(f"\n{'=' * 64}\nVERDICT INPUTS\n{'=' * 64}")
    print(f"pooled primary stability: baseline {b[0]['pooled'][0]:.3f} → +PBP {v[0]['pooled'][0]:.3f}")
    print(f"post-2020 primary:        baseline {b[0]['post2020'][0]:.3f} → +PBP {v[0]['post2020'][0]:.3f}")
    print(f"pre-2020 primary:         baseline {b[0]['pre2020'][0]:.3f} → +PBP {v[0]['pre2020'][0]:.3f}")
    print(f"violations:               baseline {b[1]} → +PBP {v[1]}")


if __name__ == "__main__":
    main()
