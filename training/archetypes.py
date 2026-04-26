"""
Player archetype clustering — Phase 5a.

Pulls qualified player-seasons (>=10 GP, >=10 MPG) with Torvik shot-zone /
impact data and cstat rate stats, standardizes the feature matrix, runs
k-means with k=12, then assigns each cluster to one of 12 D&D-class archetypes
via a Hungarian-matched score against per-archetype "signature" templates.

Writes results to `player_archetypes` (one row per player-season) and stashes
centroids + scaler params in `archetype_models` so the API can answer
"most similar players" / "what would Player X be" without re-running clustering.

Usage: `python -m training.archetypes --season 2026`
"""

from __future__ import annotations

import argparse
import json
from dataclasses import dataclass

import numpy as np
import pandas as pd
from scipy.optimize import linear_sum_assignment
from scipy.special import softmax
from sklearn.cluster import KMeans
from sklearn.preprocessing import StandardScaler
from sqlalchemy import text

from db import get_engine

# Order is the canonical feature ordering — preserved in DB metadata.
FEATURE_NAMES = [
    "rim_share",       # rim_attempted / FGA
    "mid_share",       # mid_attempted / FGA
    "three_share",     # tpa / FGA
    "ast_pct",
    "tov_pct",
    "usage_rate",
    "orb_pct",
    "drb_pct",
    "stl_pct",
    "blk_pct",
    "ft_rate",
    "ogbpm",
    "dgbpm",
    "min_share",       # minutes_per_game / 40
]

# Archetype "signatures": which features should be HIGH (+1) or LOW (-1) for
# this class. Used to map cluster centroids → class names via Hungarian match.
# Anything not listed is treated as 0 (neutral). Values are rough z-score
# targets used only for relative scoring; they don't need to be calibrated.
ARCHETYPE_SIGNATURES: dict[str, dict[str, float]] = {
    "Wizard": {
        # Pure floor general — high AST, low TOV, controls tempo, plays heavy mins
        "ast_pct": 2.0, "tov_pct": -1.0, "usage_rate": 0.5, "min_share": 1.0,
        "rim_share": -0.3,
    },
    "Sorcerer": {
        # Star scorer / volume creator, efficient
        "usage_rate": 2.0, "ogbpm": 1.5, "min_share": 1.0,
        "ast_pct": 0.5,
    },
    "Warlock": {
        # High-variance gunner — heavy 3PA, high USG, boom-or-bust
        "three_share": 2.0, "usage_rate": 1.0, "tov_pct": 0.5,
        "rim_share": -1.0, "mid_share": -0.5,
    },
    "Bard": {
        # Pass-first playmaker — high AST, lower USG, elevates teammates
        "ast_pct": 1.5, "usage_rate": -0.5, "ogbpm": 0.5,
        "min_share": 0.5,
    },
    "Ranger": {
        # 3-and-D wing — 3PA + STL, low USG
        "three_share": 1.5, "stl_pct": 1.5, "usage_rate": -0.5,
        "ast_pct": -0.3, "blk_pct": -0.3,
    },
    "Barbarian": {
        # Slasher / rim attacker — high FT rate, drives, physical
        "ft_rate": 2.0, "rim_share": 1.0, "usage_rate": 0.5,
        "three_share": -1.0,
    },
    "Paladin": {
        # Two-way anchor — BLK + def impact
        "blk_pct": 1.5, "dgbpm": 1.5, "drb_pct": 1.0,
        "three_share": -1.0,
    },
    "Monk": {
        # Efficient role player — clean game, low TOV
        "tov_pct": -1.5, "usage_rate": -0.5, "ogbpm": 0.5,
        "ft_rate": 0.3,
    },
    "Cleric": {
        # Glue / connector — solid def, modest scoring volume, defensive boards
        "drb_pct": 1.0, "dgbpm": 0.5, "usage_rate": -0.5,
        "stl_pct": 0.3, "blk_pct": 0.3, "ast_pct": 0.3,
    },
    "Druid": {
        # Frontcourt anchor — high-impact interior big who owns the glass at
        # both ends, finishes through contact at the rim, and blocks shots.
        # Originally framed as a "stretch big," but the data doesn't form a
        # distinct stretch-big cluster (too rare in college); this signature
        # now matches the actual rim-running impact-big cluster instead. Note
        # the negative three_share weight: Druid in this dataset does NOT
        # shoot from outside.
        "rim_share": 1.0, "orb_pct": 1.0, "drb_pct": 1.0,
        "blk_pct": 0.5, "ogbpm": 1.5, "dgbpm": 0.5, "usage_rate": 1.0,
        "three_share": -0.3,
    },
    "Rogue": {
        # Event creator — high STL/BLK, off-ball opportunist
        "stl_pct": 2.0, "blk_pct": 1.0, "usage_rate": -0.3,
    },
    "Fighter": {
        # Balanced two-way — no specialty (kept near zero everywhere)
        "ogbpm": 0.3, "dgbpm": 0.3, "min_share": 0.3,
    },
}

CLASSES = list(ARCHETYPE_SIGNATURES.keys())
K = len(CLASSES)


@dataclass
class ClusterResult:
    feature_matrix: np.ndarray            # (n, F), standardized
    feature_names: list[str]
    means: np.ndarray                     # (F,) — pre-standardization mean
    stds: np.ndarray                      # (F,) — pre-standardization std
    labels: np.ndarray                    # (n,) — cluster_id per player
    centroids: np.ndarray                 # (K, F) — in standardized space
    cluster_to_class: dict[int, str]
    affinities: np.ndarray                # (n, K) — softmax over -distance


def fetch_player_features(engine, season: int) -> pd.DataFrame:
    """Pull the qualified player-season feature matrix for clustering.

    `player_season_stats` is keyed by (player_id, season, team_id) — mid-season
    transfers have two rows. We keep the dominant stint (most total minutes).
    """
    query = text("""
        WITH pss_ranked AS (
            SELECT
                pss.*,
                ROW_NUMBER() OVER (
                    PARTITION BY pss.player_id, pss.season
                    ORDER BY (pss.games_played * pss.minutes_per_game) DESC NULLS LAST
                ) AS rn
            FROM player_season_stats pss
            WHERE pss.season = :season
              AND pss.games_played >= 10
              AND pss.minutes_per_game >= 10
        ),
        torvik_ranked AS (
            SELECT
                t.*,
                ROW_NUMBER() OVER (
                    PARTITION BY t.player_id, t.season
                    ORDER BY t.total_minutes DESC NULLS LAST
                ) AS rn
            FROM torvik_player_stats t
            WHERE t.season = :season
              AND t.player_id IS NOT NULL
              AND t.ogbpm IS NOT NULL
              AND t.dgbpm IS NOT NULL
              AND t.rim_attempted IS NOT NULL
              AND t.mid_attempted IS NOT NULL
              AND t.tpa IS NOT NULL
        )
        SELECT
            t.player_id,
            t.player_id::text AS player_id_str,
            p.name AS player_name,
            p.team_id,
            tm.name AS team_name,
            t.rim_attempted,
            t.mid_attempted,
            t.tpa,
            t.two_pa,
            t.ogbpm,
            t.dgbpm,
            pss.ast_pct,
            pss.tov_pct,
            pss.usage_rate,
            pss.orb_pct,
            pss.drb_pct,
            pss.stl_pct,
            pss.blk_pct,
            pss.ft_rate,
            pss.minutes_per_game,
            pss.games_played
        FROM torvik_ranked t
        JOIN pss_ranked pss
            ON pss.player_id = t.player_id AND pss.season = t.season
            AND pss.rn = 1
        JOIN players p ON p.id = t.player_id
        LEFT JOIN teams tm ON tm.id = p.team_id
        WHERE t.rn = 1
    """)
    df = pd.read_sql(query, engine, params={"season": season})

    # Shot zone shares (fraction of FGA from each zone)
    fga = df["rim_attempted"] + df["mid_attempted"] + df["tpa"]
    fga = fga.replace(0, np.nan)
    df["rim_share"] = df["rim_attempted"] / fga
    df["mid_share"] = df["mid_attempted"] / fga
    df["three_share"] = df["tpa"] / fga

    # cstat stores rate stats on a mixed scale (some 0–1 fractions, some 0–100
    # percents); standardization makes that irrelevant per-feature, so we just
    # pass values through as-is.
    df["min_share"] = df["minutes_per_game"] / 40.0

    # Drop rows with any NaN in features (small fraction; usually shot-zone
    # players with 0 attempts).
    df = df.dropna(subset=FEATURE_NAMES).reset_index(drop=True)
    return df


def cluster_and_assign(df: pd.DataFrame) -> ClusterResult:
    raw = df[FEATURE_NAMES].to_numpy(dtype=np.float64)
    scaler = StandardScaler()
    X = scaler.fit_transform(raw)

    km = KMeans(n_clusters=K, random_state=42, n_init=20)
    labels = km.fit_predict(X)
    centroids = km.cluster_centers_  # (K, F)

    cluster_to_class = match_clusters_to_classes(centroids)

    # Affinities: softmax over -distance from each player to each centroid.
    # Lower temperature sharpens; we use a moderate setting so secondary
    # classes still register meaningfully.
    dists = np.linalg.norm(X[:, None, :] - centroids[None, :, :], axis=-1)  # (n, K)
    temperature = 1.5
    aff = softmax(-dists / temperature, axis=1)  # (n, K)

    return ClusterResult(
        feature_matrix=X,
        feature_names=FEATURE_NAMES,
        means=scaler.mean_,
        stds=scaler.scale_,
        labels=labels,
        centroids=centroids,
        cluster_to_class=cluster_to_class,
        affinities=aff,
    )


def match_clusters_to_classes(centroids: np.ndarray) -> dict[int, str]:
    """Hungarian-match clusters to D&D classes by signature overlap."""
    K_, F = centroids.shape
    assert K_ == K, f"expected {K} clusters, got {K_}"

    # Build signature matrix (K, F): non-zero entries from ARCHETYPE_SIGNATURES.
    sig = np.zeros((K, F), dtype=np.float64)
    for ci, cls in enumerate(CLASSES):
        for feat, target in ARCHETYPE_SIGNATURES[cls].items():
            sig[ci, FEATURE_NAMES.index(feat)] = target

    # Score: dot(centroid, signature). High score = good match.
    # Hungarian minimizes cost, so we negate.
    score = centroids @ sig.T  # (K_clusters, K_classes) — but K==K_, so K×K
    cost = -score

    cluster_idx, class_idx = linear_sum_assignment(cost)
    return {int(c): CLASSES[k] for c, k in zip(cluster_idx, class_idx)}


def write_results(engine, season: int, df: pd.DataFrame, result: ClusterResult):
    cluster_to_class = result.cluster_to_class
    class_to_idx = {cls: i for i, cls in enumerate(CLASSES)}

    rows = []
    for i, row in df.iterrows():
        cid = int(result.labels[i])
        affs = result.affinities[i]  # (K,) over CLUSTERS — index j is cluster j
        # Re-key affinity by class name (cluster j → cluster_to_class[j])
        aff_by_class = {cluster_to_class[j]: float(affs[j]) for j in range(K)}
        # Sort classes by affinity descending
        ranked = sorted(aff_by_class.items(), key=lambda kv: kv[1], reverse=True)
        primary_class, primary_score = ranked[0]
        secondary_class, secondary_score = ranked[1]

        # Re-order feature_vector storage to match FEATURE_NAMES (it already is)
        fv = result.feature_matrix[i].astype(np.float32).tolist()

        rows.append({
            "player_id": str(row["player_id"]),
            "season": season,
            "cluster_id": cid,
            "primary_class": primary_class,
            "secondary_class": secondary_class,
            "primary_score": primary_score,
            "secondary_score": secondary_score,
            "affinity_scores": json.dumps(aff_by_class),
            "feature_vector": fv,
        })

    print(f"Writing {len(rows)} archetype rows for season {season}…")
    with engine.begin() as conn:
        # Replace this season's rows wholesale (clustering is not incremental).
        conn.execute(
            text("DELETE FROM player_archetypes WHERE season = :season"),
            {"season": season},
        )
        conn.execute(
            text(
                """
                INSERT INTO player_archetypes
                    (player_id, season, cluster_id, primary_class, secondary_class,
                     primary_score, secondary_score, affinity_scores, feature_vector)
                VALUES
                    (:player_id, :season, :cluster_id, :primary_class, :secondary_class,
                     :primary_score, :secondary_score, CAST(:affinity_scores AS JSONB),
                     :feature_vector)
                """
            ),
            rows,
        )

        # Stash model metadata so the API can do similarity queries without
        # re-running clustering.
        centroid_payload = {
            str(j): {
                "class": cluster_to_class[j],
                "vector": result.centroids[j].astype(float).tolist(),
            }
            for j in range(K)
        }
        cluster_to_class_str = {str(k): v for k, v in cluster_to_class.items()}
        feature_means = {
            FEATURE_NAMES[i]: float(result.means[i]) for i in range(len(FEATURE_NAMES))
        }
        feature_stds = {
            FEATURE_NAMES[i]: float(result.stds[i]) for i in range(len(FEATURE_NAMES))
        }
        conn.execute(
            text(
                """
                INSERT INTO archetype_models
                    (season, feature_names, cluster_to_class, centroids,
                     feature_means, feature_stds, n_qualified)
                VALUES
                    (:season, CAST(:feature_names AS JSONB),
                     CAST(:cluster_to_class AS JSONB),
                     CAST(:centroids AS JSONB),
                     CAST(:feature_means AS JSONB),
                     CAST(:feature_stds AS JSONB),
                     :n_qualified)
                ON CONFLICT (season) DO UPDATE SET
                    feature_names = EXCLUDED.feature_names,
                    cluster_to_class = EXCLUDED.cluster_to_class,
                    centroids = EXCLUDED.centroids,
                    feature_means = EXCLUDED.feature_means,
                    feature_stds = EXCLUDED.feature_stds,
                    n_qualified = EXCLUDED.n_qualified,
                    created_at = now()
                """
            ),
            {
                "season": season,
                "feature_names": json.dumps(FEATURE_NAMES),
                "cluster_to_class": json.dumps(cluster_to_class_str),
                "centroids": json.dumps(centroid_payload),
                "feature_means": json.dumps(feature_means),
                "feature_stds": json.dumps(feature_stds),
                "n_qualified": len(rows),
            },
        )
    class_to_idx  # silence linter (kept for potential downstream use)


def print_diagnostics(df: pd.DataFrame, result: ClusterResult):
    """Show per-cluster size + mean of each feature in original units."""
    df_out = df.copy()
    df_out["cluster_id"] = result.labels
    df_out["class"] = df_out["cluster_id"].map(result.cluster_to_class)

    print("\n=== Cluster sizes ===")
    print(df_out["class"].value_counts().sort_index())

    print("\n=== Mean features per class (original units) ===")
    cols = ["class"] + FEATURE_NAMES
    summary = df_out[cols].groupby("class").mean(numeric_only=True).round(3)
    print(summary)

    print("\n=== Sample players per class ===")
    for cls in CLASSES:
        members = df_out[df_out["class"] == cls].sort_values(
            "ogbpm", ascending=False
        )
        sample = members.head(4)[["player_name", "team_name", "ogbpm", "dgbpm",
                                  "usage_rate", "ast_pct", "blk_pct"]]
        print(f"\n--- {cls} (n={len(members)}) ---")
        print(sample.to_string(index=False))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--season", type=int, default=2026)
    parser.add_argument("--diagnostics", action="store_true",
                        help="Print per-cluster summaries before writing")
    args = parser.parse_args()

    engine = get_engine()
    print(f"Fetching qualified player-seasons for {args.season}…")
    df = fetch_player_features(engine, args.season)
    print(f"  {len(df)} players passed the qualification filter.")

    print(f"Clustering with k={K}…")
    result = cluster_and_assign(df)

    if args.diagnostics:
        print_diagnostics(df, result)

    write_results(engine, args.season, df, result)
    print("Done.")


if __name__ == "__main__":
    main()
