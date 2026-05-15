"""
Player archetype clustering — Phase 5a.

See `docs/archetypes_methodology.md` for the full retraining playbook,
health-metric tripwires, and decision tree for "this drifted, now what."
This module docstring is a summary; the doc is the source of truth.

Pulls qualified player-seasons (>=10 GP, >=10 MPG) with Torvik shot-zone /
impact data and cstat rate stats, standardizes the feature matrix, runs
k-means with k=12 on a **combined multi-season cohort**, then assigns each
cluster to one of 12 D&D-class archetypes via a Hungarian-matched score
against per-archetype "signature" templates.

Combined-cohort training is the load-bearing fix for cross-season class
stability. When clustering ran per-season, returning players (matched by
`torvik_pid`) only kept their primary class ~28% of the time, because
k-means redrew cluster boundaries each season independently. Training on
the union of seasons gives one set of centroids that every season's
players are classified against — the same skill profile gets the same
class assignment regardless of which season we're looking at.

Trade-off: doesn't capture genuine year-to-year shifts (rising 3PT volume,
small-ball trends, etc). At a 2-3 season horizon that effect is tiny
compared to the stability gain. When the historical archive grows past
~5 seasons (Phase 6), revisit — at that scale a sliding 3-season window
or era-aware clustering starts making sense.

Writes results to `player_archetypes` (one row per player-season) and stashes
centroids + scaler params in `archetype_models` keyed by season. Both seasons
share the same centroids on disk; the per-season key just lets the API look
up "the model for season N" without knowing about training-cohort details.

The model table is currently unread by the API — `get_similar_players` works
directly off the per-row `feature_vector` column. The model table is kept so
a future endpoint can classify an arbitrary feature vector (hypothetical
roster, "what would Player X be?") against existing centroids without
re-running clustering.

Usage:
    python -m training.archetypes                    # default: 2025,2026
    python -m training.archetypes --seasons 2026     # single-season fit
    python -m training.archetypes --seasons 2024,2025,2026 --diagnostics
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass

import numpy as np
import pandas as pd
from scipy.optimize import linear_sum_assignment
from scipy.special import softmax
from sklearn.cluster import KMeans
from sklearn.preprocessing import StandardScaler
from sqlalchemy import bindparam, text

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
        # Elite lead-guard creator — highest AST% in the dataset, heavy
        # minutes, POSITIVE two-way impact. The POY-shortlist floor general
        # (Braden Smith, Walter Clayton, Kam Jones tier). OGBPM/DGBPM
        # weights are load-bearing: without them Hungarian gave this
        # cluster to Bard because Bard's signature was the only one that
        # rewarded impact on top of AST. No tov_pct weight: elite ball-
        # handlers turn it over slightly above average because they
        # touch every possession.
        "ast_pct": 2.5, "usage_rate": 0.5, "min_share": 1.0,
        "ogbpm": 1.5, "dgbpm": 0.5, "rim_share": -0.3,
    },
    "Sorcerer": {
        # High-volume star scorer — strong offensive impact at heavy
        # minutes. The defining axes are OGBPM and minutes, not USG:
        # elite guards (Wizard) and mid-major lead-creators (Bard) both
        # outrank Sorcerer on raw usage in this cohort. Negative AST
        # weight separates Sorcerer from Wizard — Wizards create for
        # others, Sorcerers hunt their own shots.
        "ogbpm": 1.5, "min_share": 1.0, "usage_rate": 0.5,
        "ast_pct": -0.3,
    },
    "Warlock": {
        # Three-point specialist — heaviest 3PA share, LOW USG, clean
        # game from mostly spotting up. Catch-and-shoot role player,
        # not a primary creator.
        "three_share": 2.0, "rim_share": -1.0, "mid_share": -0.5,
        "usage_rate": -0.5, "tov_pct": -0.5,
    },
    "Bard": {
        # High-USG primary scorer on a non-elite roster — the team's only
        # real offensive option, plays heavy minutes, hunts shots. Eric
        # Dixon / Antonio Reeves / Dimingus Stevens tier. Cluster identity
        # shifted twice: from "pass-first distributor" (2-season) to
        # "mid-major primary creator" (4-season) to "primary scorer with
        # modest assist rate" (5-season w/ 2022). The high-AST cluster
        # that the old prose described now lands on Fighter. ast_pct
        # dropped from the signature because this cluster's ast_pct is
        # only marginally above mean; usage_rate added to anchor the
        # "high-USG scoring lead" identity.
        "usage_rate": 0.5, "min_share": 0.5, "ogbpm": 0.3,
    },
    "Ranger": {
        # Perimeter spacer — high 3PA share at low USG. Role-player
        # shooter. (Removed the +stl_pct weight: the cluster does NOT
        # generate elite steals — the D&D "bow" analogy is load-bearing
        # via three_share, not stl.)
        "three_share": 1.5, "usage_rate": -0.5,
        "ast_pct": -0.3, "blk_pct": -0.3,
    },
    "Barbarian": {
        # Interior finisher — highest rim share, lowest 3PA share. A
        # low-USG physical big who gets fed at the rim. Drop the
        # usage_rate weight: the cluster is decidedly NOT high-usage.
        "ft_rate": 2.0, "rim_share": 1.5, "three_share": -1.0,
        "blk_pct": 0.3,
    },
    "Paladin": {
        # Defensive anchor — elite BLK%, highest DGBPM in the dataset.
        "blk_pct": 1.5, "dgbpm": 1.5, "drb_pct": 1.0,
        "three_share": -1.0,
    },
    "Monk": {
        # Versatile mid-rotation forward — balanced inside/outside, modest
        # minutes, not a star. Cluster identity drifted to "stretch four"
        # as we added 2023/2024 to the cohort; the Monk label fits "agile,
        # adaptable" but stretches the D&D metaphor. Prose should match
        # the new cluster identity (stretch-4 forwards 79–82" who mix
        # rim and three).
        "three_share": 0.5, "rim_share": 0.3, "min_share": -0.3,
        "ogbpm": -0.3, "usage_rate": -0.3,
    },
    "Cleric": {
        # Low-volume backup big — rebounds, low USG, no column dominance.
        # Removed dgbpm/stl_pct/ast_pct weights: cluster has below-average
        # values on all three (guardrail flagged these as SIGN violations).
        "drb_pct": 1.0, "orb_pct": 0.5, "usage_rate": -0.5,
        "three_share": -0.5,
    },
    "Druid": {
        # Elite two-way big — highest combined OGBPM + DGBPM in the dataset.
        # Owns the glass at both ends, finishes through contact, blocks
        # shots. POY-shortlist / lottery-pick frontcourt.
        "rim_share": 1.0, "orb_pct": 1.0, "drb_pct": 1.0,
        "blk_pct": 0.5, "ogbpm": 1.5, "dgbpm": 0.5, "usage_rate": 1.0,
        "three_share": -0.3,
    },
    "Rogue": {
        # Disruptive two-way wing — high STL, strong DGBPM. Softened
        # blk_pct from +1.0 to +0.3: the cluster doesn't have elite
        # blocks (that's Paladin's lane).
        "stl_pct": 2.0, "dgbpm": 1.0, "usage_rate": -0.3,
        "blk_pct": 0.3,
    },
    "Fighter": {
        # Low-USG pass-first guard / backup point — modest minutes, high
        # AST% relative to shot volume, weak two-way impact. Christian
        # Ings / Jaden Ray / Jayden Pierre tier. Cluster identity absorbed
        # the "pass-first distributor" prose that Bard historically held —
        # cluster z on ast_pct is high (~+1.2) but the ORDER constraint
        # with Bard was dropped because Bard's cluster doesn't compete on
        # AST anymore. Negative anchors (min_share, usage_rate, ogbpm)
        # hold the rotation-depth identity. stl_pct kept for the steal
        # tendency of low-usage guards.
        "stl_pct": 0.3,
        "min_share": -0.3, "usage_rate": -0.3,
        "ogbpm": -0.3,
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


def fetch_player_features(engine, seasons: list[int]) -> pd.DataFrame:
    """Pull the qualified player-season feature matrix for clustering.

    Returns one row per (player_id, season) — the dominant stint by minutes
    when a player has multiple `player_season_stats` rows in a season (e.g.,
    mid-season transfers).

    Multi-season fetches stack rows for combined-cohort clustering. The
    returned `season` column is what `write_results` partitions on when it
    inserts into `player_archetypes`.
    """
    # SQLAlchemy `expanding=True` rewrites `:seasons` into a parameterized
    # IN-list at execute time, so this works with any season count without
    # hand-rolled string interpolation.
    query = text("""
        WITH pss_ranked AS (
            SELECT
                pss.*,
                ROW_NUMBER() OVER (
                    PARTITION BY pss.player_id, pss.season
                    ORDER BY (pss.games_played * pss.minutes_per_game) DESC NULLS LAST
                ) AS rn
            FROM player_season_stats pss
            WHERE pss.season IN :seasons
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
            WHERE t.season IN :seasons
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
            t.season AS season,
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
    """).bindparams(bindparam("seasons", expanding=True))
    df = pd.read_sql(query, engine, params={"seasons": seasons})

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


def verify_signature_alignment(
    centroids: np.ndarray,
    cluster_to_class: dict[int, str],
    sign_tol: float = 0.2,
    order_tol: float = 0.3,
) -> list[str]:
    """Sanity-check Hungarian's cluster→class assignment against the signatures.

    Returns a list of human-readable violation strings; empty list = clean.

    Two checks per non-zero signature weight:

    1. SIGN — the assigned cluster's centroid z-score must agree in sign with
       the signature weight (within ±sign_tol). Catches "this cluster doesn't
       fit this description at all."
    2. ORDERING — when two classes both put weight on the same feature, the
       class with the larger weight must have the larger cluster z (within
       ±order_tol). Catches Hungarian putting similar clusters in swapped
       slots, e.g. the elite-guard cluster labeled as the low-impact
       distributor class because both signatures want high AST%.

    Tolerances are loose by design — signatures are rough relative targets,
    not specifications, and we don't want false positives on borderline calls.
    """
    class_to_cluster = {v: k for k, v in cluster_to_class.items()}
    violations: list[str] = []

    for cls, sig in ARCHETYPE_SIGNATURES.items():
        cid = class_to_cluster[cls]
        for feat, weight in sig.items():
            if weight == 0:
                continue
            z = centroids[cid, FEATURE_NAMES.index(feat)]
            if weight > 0 and z < -sign_tol:
                violations.append(
                    f"SIGN: {cls} wants HIGH {feat} (w={weight:+.1f}), "
                    f"cluster z={z:+.2f}"
                )
            elif weight < 0 and z > sign_tol:
                violations.append(
                    f"SIGN: {cls} wants LOW {feat} (w={weight:+.1f}), "
                    f"cluster z={z:+.2f}"
                )

    # Per-feature: (class, weight, cluster_z) for every class that weights it.
    by_feature: dict[str, list[tuple[str, float, float]]] = {}
    for cls, sig in ARCHETYPE_SIGNATURES.items():
        cid = class_to_cluster[cls]
        for feat, weight in sig.items():
            if weight == 0:
                continue
            z = float(centroids[cid, FEATURE_NAMES.index(feat)])
            by_feature.setdefault(feat, []).append((cls, weight, z))

    for feat, entries in by_feature.items():
        for i in range(len(entries)):
            for j in range(i + 1, len(entries)):
                cls_i, w_i, z_i = entries[i]
                cls_j, w_j, z_j = entries[j]
                if w_i - w_j > order_tol and z_i < z_j - order_tol:
                    violations.append(
                        f"ORDER: {cls_i} wants {feat} > {cls_j} "
                        f"(w {w_i:+.1f} vs {w_j:+.1f}), but cluster z "
                        f"{cls_i}={z_i:+.2f} < {cls_j}={z_j:+.2f}"
                    )
                elif w_j - w_i > order_tol and z_j < z_i - order_tol:
                    violations.append(
                        f"ORDER: {cls_j} wants {feat} > {cls_i} "
                        f"(w {w_j:+.1f} vs {w_i:+.1f}), but cluster z "
                        f"{cls_j}={z_j:+.2f} < {cls_i}={z_i:+.2f}"
                    )

    return violations


def write_results(engine, seasons: list[int], df: pd.DataFrame, result: ClusterResult):
    """Persist per-row class assignments and shared model metadata.

    Every season passed in gets its rows in `player_archetypes` replaced, and
    a row in `archetype_models` keyed by that season pointing at the shared
    centroids. Combined-cohort training means all seasons in this run share
    identical centroids on disk; the per-season key is just a lookup
    convenience for the API.
    """
    cluster_to_class = result.cluster_to_class

    rows = []
    for i, df_row in df.iterrows():
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
            "player_id": str(df_row["player_id"]),
            "season": int(df_row["season"]),
            "cluster_id": cid,
            "primary_class": primary_class,
            "secondary_class": secondary_class,
            "primary_score": primary_score,
            "secondary_score": secondary_score,
            "affinity_scores": json.dumps(aff_by_class),
            "feature_vector": fv,
        })

    per_season_counts = {s: sum(1 for r in rows if r["season"] == s) for s in seasons}
    print(f"Writing {len(rows)} archetype rows across {len(seasons)} season(s): "
          + ", ".join(f"{s}={per_season_counts[s]}" for s in seasons))

    with engine.begin() as conn:
        # Replace all in-scope seasons' rows wholesale — clustering is not
        # incremental and stale rows from a prior run would otherwise linger.
        conn.execute(
            text("DELETE FROM player_archetypes WHERE season IN :seasons")
                .bindparams(bindparam("seasons", expanding=True)),
            {"seasons": seasons},
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
        # re-running clustering. One row per season — all sharing the same
        # centroids/scaler from this combined-cohort fit.
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
        for season in seasons:
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
                    # n_qualified = the size of this season's slice of the
                    # combined cohort, not the total fit size — that's what
                    # consumers of the model row would expect.
                    "n_qualified": per_season_counts[season],
                },
            )


def print_diagnostics(df: pd.DataFrame, result: ClusterResult):
    """Show per-cluster size + mean of each feature in original units."""
    df_out = df.copy()
    df_out["cluster_id"] = result.labels
    df_out["class"] = df_out["cluster_id"].map(result.cluster_to_class)

    print("\n=== Cluster sizes (combined cohort) ===")
    print(df_out["class"].value_counts().sort_index())

    if df_out["season"].nunique() > 1:
        print("\n=== Cluster sizes by season ===")
        print(
            df_out.groupby(["class", "season"]).size().unstack(fill_value=0).sort_index()
        )

    print("\n=== Mean features per class (original units) ===")
    cols = ["class"] + FEATURE_NAMES
    summary = df_out[cols].groupby("class").mean(numeric_only=True).round(3)
    print(summary)

    print("\n=== Sample players per class ===")
    for cls in CLASSES:
        members = df_out[df_out["class"] == cls].sort_values(
            "ogbpm", ascending=False
        )
        sample = members.head(4)[["player_name", "team_name", "season",
                                  "ogbpm", "dgbpm", "usage_rate", "ast_pct",
                                  "blk_pct"]]
        print(f"\n--- {cls} (n={len(members)}) ---")
        print(sample.to_string(index=False))


def parse_seasons(arg: str) -> list[int]:
    """Parse `--seasons 2025,2026` into [2025, 2026]. Trailing commas and
    surrounding whitespace are tolerated; empty input is rejected."""
    parts = [p.strip() for p in arg.split(",") if p.strip()]
    if not parts:
        raise argparse.ArgumentTypeError("must specify at least one season")
    try:
        return sorted({int(p) for p in parts})
    except ValueError as e:
        raise argparse.ArgumentTypeError(f"invalid season list: {arg}") from e


def main():
    parser = argparse.ArgumentParser(
        description="Cluster D-I players into archetype classes via k-means "
                    "on a combined multi-season cohort.",
    )
    parser.add_argument(
        "--seasons",
        type=parse_seasons,
        default=parse_seasons("2025,2026"),
        help="Comma-separated season list (default: 2025,2026). "
             "All seasons are clustered together; rows are written per-season.",
    )
    parser.add_argument("--diagnostics", action="store_true",
                        help="Print per-cluster summaries before writing")
    parser.add_argument(
        "--no-verify", action="store_true",
        help="Skip the signature-alignment guardrail. Use only when "
             "intentionally rebalancing signatures and you've reviewed "
             "diagnostics manually.",
    )
    args = parser.parse_args()

    engine = get_engine()
    print(f"Fetching qualified player-seasons for {args.seasons}…")
    df = fetch_player_features(engine, args.seasons)
    print(f"  {len(df)} player-seasons passed the qualification filter "
          f"({df['season'].value_counts().sort_index().to_dict()})")

    print(f"Clustering with k={K} on combined cohort…")
    result = cluster_and_assign(df)

    if args.diagnostics:
        print_diagnostics(df, result)

    if not args.no_verify:
        violations = verify_signature_alignment(result.centroids, result.cluster_to_class)
        if violations:
            print("\n=== Signature alignment violations ===")
            for v in violations:
                print(f"  {v}")
            print(
                f"\n{len(violations)} violation(s) — Hungarian likely put labels on "
                f"the wrong clusters, or signatures need tuning. See the "
                f"decision tree in docs/archetypes_methodology.md. "
                f"Rerun with --no-verify to write anyway."
            )
            sys.exit(1)
        print("Signature alignment check passed.")

    write_results(engine, args.seasons, df, result)
    print("Done.")


if __name__ == "__main__":
    main()
