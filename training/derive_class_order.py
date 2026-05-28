"""Derive a similarity-ordered clockwise CLASS_ORDER for the 12 archetype
classes by minimizing total inter-spoke distance around the cycle.

Reads cluster centroids from `archetype_models` (standardized feature
space), builds a 12×12 cosine-distance matrix between class centroids,
and solves the symmetric cyclic TSP exactly via bitmask DP (n=12 →
~25M states, runs in seconds).

Output is the canonical clockwise spoke order used by the radial roster
plot and any future class-by-class radar viz. The result is a one-shot
ordering — paste into `web/src/components/archetypeColors.ts::CLASS_ORDER`.
No DB writes; this is a configuration derivation, not an ingest.

Usage:
    cd training && python derive_class_order.py [--season 2026]
"""

from __future__ import annotations

import argparse
import json

import numpy as np
from sqlalchemy import text

from db import get_engine


def load_centroids(season: int) -> tuple[list[str], np.ndarray]:
    """Returns (class_names_in_cluster_order, centroid_matrix [12, F])."""
    engine = get_engine()
    with engine.connect() as conn:
        row = conn.execute(
            text(
                "SELECT cluster_to_class, centroids FROM archetype_models "
                "WHERE season = :s"
            ),
            {"s": season},
        ).first()
    if row is None:
        raise SystemExit(f"No archetype_models row for season {season}")

    cluster_to_class = row[0] if isinstance(row[0], dict) else json.loads(row[0])
    centroids_payload = row[1] if isinstance(row[1], dict) else json.loads(row[1])

    # cluster_to_class is keyed by string cluster id; centroids_payload likewise.
    # Order by integer cluster id so rows align.
    cluster_ids = sorted(cluster_to_class.keys(), key=int)
    names = [cluster_to_class[k] for k in cluster_ids]
    # Each centroids[cid] is either a {"name": ..., "vector": [...]} dict or a
    # bare list, depending on which write path produced it. Handle both.
    vectors = []
    for cid in cluster_ids:
        v = centroids_payload[cid]
        if isinstance(v, dict):
            vectors.append(v["vector"])
        else:
            vectors.append(v)
    return names, np.array(vectors, dtype=np.float64)


def cosine_distance_matrix(centroids: np.ndarray) -> np.ndarray:
    norms = np.linalg.norm(centroids, axis=1, keepdims=True)
    unit = centroids / np.maximum(norms, 1e-12)
    sim = unit @ unit.T
    return 1.0 - sim


def solve_cyclic_tsp(dist: np.ndarray) -> list[int]:
    """Exact bitmask DP for symmetric cyclic TSP. Fixes node 0 as the
    starting vertex (any rotation is equivalent on a cycle) and finds the
    minimum-cost Hamiltonian cycle. n=12 → 12 * 2^12 = 49,152 states; well
    inside reach.

    Returns the node order as a 0-indexed cycle starting at 0.
    """
    n = dist.shape[0]
    INF = float("inf")
    # dp[mask][i] = minimum cost to start at 0, visit exactly the nodes in
    # `mask` (mask must include node 0 and i), and end at i.
    dp = [[INF] * n for _ in range(1 << n)]
    parent = [[-1] * n for _ in range(1 << n)]
    dp[1 << 0][0] = 0.0
    for mask in range(1 << n):
        if not (mask & 1):
            continue
        for i in range(n):
            if not (mask & (1 << i)):
                continue
            cost = dp[mask][i]
            if cost == INF:
                continue
            for j in range(n):
                if mask & (1 << j):
                    continue
                new_mask = mask | (1 << j)
                new_cost = cost + dist[i][j]
                if new_cost < dp[new_mask][j]:
                    dp[new_mask][j] = new_cost
                    parent[new_mask][j] = i

    full = (1 << n) - 1
    # Close the cycle back to node 0
    best_end = -1
    best_cost = INF
    for i in range(1, n):
        c = dp[full][i] + dist[i][0]
        if c < best_cost:
            best_cost = c
            best_end = i

    # Reconstruct the path
    path = []
    cur = best_end
    mask = full
    while cur != -1:
        path.append(cur)
        prev = parent[mask][cur]
        mask ^= 1 << cur
        cur = prev
    path.reverse()
    return path, best_cost


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--season", type=int, default=2026)
    args = parser.parse_args()

    names, centroids = load_centroids(args.season)
    print(f"Loaded {len(names)} centroids of dim {centroids.shape[1]} "
          f"for season {args.season}")

    dist = cosine_distance_matrix(centroids)
    print("\nPairwise cosine distance matrix:")
    print("      " + " ".join(f"{n[:5]:>6}" for n in names))
    for i, n in enumerate(names):
        print(f"{n[:5]:>5} " + " ".join(f"{dist[i, j]:6.3f}" for j in range(len(names))))

    order, cost = solve_cyclic_tsp(dist)
    ordered_names = [names[i] for i in order]
    print(f"\nOptimal cyclic order (total cosine cost = {cost:.4f}):")
    print(" -> ".join(ordered_names) + " -> ...")

    # Rotate so Wizard (or whichever name we prefer) sits at the top.
    # The existing CLASS_ORDER starts with Wizard at 12 o'clock; preserve
    # that landmark so the UI doesn't lose its visual reference.
    if "Wizard" in ordered_names:
        idx = ordered_names.index("Wizard")
        ordered_names = ordered_names[idx:] + ordered_names[:idx]

    print("\nRotated to start at Wizard (TypeScript array):")
    ts = ",\n  ".join(f"'{n}'" for n in ordered_names)
    print(f"export const CLASS_ORDER: readonly string[] = [\n  {ts},\n];")


if __name__ == "__main__":
    main()
