"""
Validate the 'balance is good, density is bad' assumption baked into
the roster-fit chip (`cstat_core::roster_fit::fit_score_against_projected`).

Two questions:

Q1. Per-archetype mean CamPom — is "Druid is good, Wizard is mid" backed
    by data, or are we anchoring on a stereotype? Minutes-weighted to
    avoid noise from low-MPG outliers.

Q2. Does archetype balance predict overperformance vs. the talent
    identity? The player-impact identity says
        AdjEM ≈ Σ(cam_v3 × minute_share)
    modulo coaching/scheme/injuries. If teams with *concentrated*
    high-CamPom-class rosters systematically beat their talent identity,
    the fit chip's "stacks Wizard rotation → bad" framing is wrong.

Runs across every qualified (≥5 GP, ≥5 MPG) player with both archetype
and CamPom coverage; no per-season slicing — pool across 2015–2026 for
maximum statistical power.

Usage:
    cd training && python validate_archetype_balance.py
"""

import numpy as np
import pandas as pd
from sqlalchemy import text

from db import get_engine

ENG = get_engine()


Q_ARCHETYPE_VALUE = """
SELECT pa.primary_class,
       SUM(tps.cam_gbpm_v3_psos * pss.minutes_per_game * pss.games_played)::float
           / SUM(pss.minutes_per_game * pss.games_played)::float AS mean_campom,
       COUNT(*) AS n_players,
       SUM(pss.minutes_per_game * pss.games_played)::float AS total_min
FROM player_archetypes pa
JOIN torvik_player_stats tps
    ON tps.player_id = pa.player_id AND tps.season = pa.season
JOIN player_season_stats pss
    ON pss.player_id = pa.player_id AND pss.season = pa.season
WHERE pss.minutes_per_game >= 5
  AND pss.games_played >= 5
  AND tps.cam_gbpm_v3_psos IS NOT NULL
GROUP BY pa.primary_class
ORDER BY mean_campom DESC
"""

Q_PLAYER_ROSTER = """
SELECT pss.team_id,
       pss.season,
       pa.primary_class,
       pa.secondary_class,
       (pss.minutes_per_game * pss.games_played)::float AS total_min,
       tps.cam_gbpm_v3_psos::float                       AS cam_v3
FROM player_season_stats pss
JOIN player_archetypes pa
    ON pa.player_id = pss.player_id AND pa.season = pss.season
LEFT JOIN torvik_player_stats tps
    ON tps.player_id = pss.player_id AND tps.season = pss.season
WHERE pss.minutes_per_game >= 5
  AND pss.games_played >= 5
"""

Q_TEAM_ADJ_EM = """
SELECT team_id, season, adj_efficiency_margin::float AS adj_em
FROM team_season_stats
WHERE adj_efficiency_margin IS NOT NULL
"""


def gini(values: list[float]) -> float:
    """Population Gini coefficient over a list of non-negative values."""
    if not values:
        return 0.0
    s = sorted(values)
    n = len(s)
    cum = np.cumsum(s)
    if cum[-1] <= 0:
        return 0.0
    return (2.0 * sum((i + 1) * x for i, x in enumerate(s)) / (n * cum[-1])) - (n + 1) / n


def effective_classes(shares: list[float]) -> float:
    """exp(Shannon entropy) — the equivalent count of equally-sized
    classes that would yield the same entropy. 12.0 = perfectly even
    across all classes; 1.0 = single class."""
    entropy = -sum(p * np.log(p) for p in shares if p > 0)
    return float(np.exp(entropy))


def main() -> None:
    print("=" * 72)
    print("Q1 — Per-archetype mean CamPom (minutes-weighted)")
    print("=" * 72)
    df_a = pd.read_sql(text(Q_ARCHETYPE_VALUE), ENG)
    print(df_a.to_string(index=False))
    arch_value: dict[str, float] = dict(zip(df_a["primary_class"], df_a["mean_campom"]))
    print()

    print("=" * 72)
    print("Q2 — Does balance predict residual vs talent identity?")
    print("=" * 72)
    df_p = pd.read_sql(text(Q_PLAYER_ROSTER), ENG)
    df_t = pd.read_sql(text(Q_TEAM_ADJ_EM), ENG)

    rows = []
    for (team_id, season), grp in df_p.groupby(["team_id", "season"]):
        total_min = grp["total_min"].sum()
        if total_min <= 0:
            continue

        # Talent identity uses only rows with both archetype + cam_v3
        # (drives the residual calculation). We require ≥80% of the
        # team's minutes to have cam_v3 so the per-team mean isn't
        # dominated by a thin Torvik-coverage tail.
        cam_data = grp[grp["cam_v3"].notna()]
        if cam_data.empty:
            continue
        cam_min = cam_data["total_min"].sum()
        cam_coverage = cam_min / total_min
        if cam_coverage < 0.8:
            continue
        talent_identity = (cam_data["cam_v3"] * cam_data["total_min"]).sum() / cam_min

        # Archetype shares: primary 1.0× + secondary 0.5×, mirroring
        # `roster_fit::build_projected_class_minutes`.
        class_min: dict[str, float] = {}
        for _, p in grp.iterrows():
            m = p["total_min"]
            if p["primary_class"]:
                class_min[p["primary_class"]] = class_min.get(p["primary_class"], 0.0) + m
            sec = p["secondary_class"]
            if sec and sec != p["primary_class"]:
                class_min[sec] = class_min.get(sec, 0.0) + 0.5 * m
        total_weighted = sum(class_min.values())
        if total_weighted <= 0:
            continue
        shares = {k: v / total_weighted for k, v in class_min.items()}

        max_share = max(shares.values())
        dominant_class = max(shares, key=shares.get)
        share_values = list(shares.values())

        rows.append(
            {
                "team_id": team_id,
                "season": season,
                "talent_identity": talent_identity,
                "cam_coverage": cam_coverage,
                "max_share": max_share,
                "dominant_class": dominant_class,
                "gini": gini(share_values),
                "eff_classes": effective_classes(share_values),
                "n_archetypes": len(shares),
            }
        )

    df = pd.DataFrame(rows).merge(df_t, on=["team_id", "season"], how="inner")
    df["residual"] = df["adj_em"] - df["talent_identity"]
    df["dominant_class_value"] = df["dominant_class"].map(arch_value)

    print(f"n = {len(df)} team-seasons (qualified + CamPom coverage ≥ 80%)")
    print()

    print("Talent identity vs actual AdjEM:")
    print(
        f"  Σ(cam_v3 × share)  mean = {df['talent_identity'].mean():+6.2f}  "
        f"std = {df['talent_identity'].std():5.2f}"
    )
    print(
        f"  Actual AdjEM       mean = {df['adj_em'].mean():+6.2f}  "
        f"std = {df['adj_em'].std():5.2f}"
    )
    print(
        f"  Residual           mean = {df['residual'].mean():+6.2f}  "
        f"std = {df['residual'].std():5.2f}"
    )
    print(f"  Pearson r (talent_identity, AdjEM) = {df['talent_identity'].corr(df['adj_em']):.3f}")
    print()

    print("Residual vs balance metrics (Pearson r — sign tells the story):")
    print(
        f"  max_share       r = {df['residual'].corr(df['max_share']):+.3f}  "
        f"(positive → concentration helps)"
    )
    print(
        f"  gini            r = {df['residual'].corr(df['gini']):+.3f}  "
        f"(positive → unequal-shares help)"
    )
    print(
        f"  eff_classes     r = {df['residual'].corr(df['eff_classes']):+.3f}  "
        f"(negative → fewer effective classes help)"
    )
    print()

    print("Residual by dominant-class CamPom-value quartile:")
    df["dom_value_q"] = pd.qcut(
        df["dominant_class_value"], 4, labels=["Q1 low", "Q2", "Q3", "Q4 high"]
    )
    by_q = df.groupby("dom_value_q", observed=True).agg(
        n=("residual", "count"),
        mean_residual=("residual", "mean"),
        mean_max_share=("max_share", "mean"),
        mean_adj_em=("adj_em", "mean"),
    )
    print(by_q.to_string())
    print()

    print("Residual ~ max_share, within each dominant-class value quartile:")
    for q, sub in df.groupby("dom_value_q", observed=True):
        r = sub["residual"].corr(sub["max_share"])
        print(f"  {q:8s}  n={len(sub):4d}  r = {r:+.3f}")
    print()

    print("Quadrant analysis (dominant-class value × concentration):")
    df["hi_share"] = df["max_share"] > df["max_share"].median()
    df["hi_value"] = df["dominant_class_value"] > df["dominant_class_value"].median()
    quad = df.groupby(["hi_value", "hi_share"], observed=True).agg(
        n=("residual", "count"),
        mean_residual=("residual", "mean"),
        mean_adj_em=("adj_em", "mean"),
    )
    print(quad.to_string())
    print()
    print(
        "If 'saturate the high-value archetype' works, the (hi_value=True, "
        "hi_share=True) cell should have the largest positive residual."
    )
    print()

    print("Mean residual by dominant class (sorted by class CamPom-value):")
    by_class = df.groupby("dominant_class").agg(
        n=("residual", "count"),
        mean_residual=("residual", "mean"),
        mean_max_share=("max_share", "mean"),
        mean_adj_em=("adj_em", "mean"),
    )
    by_class["class_value"] = by_class.index.map(arch_value)
    by_class = by_class.sort_values("class_value", ascending=False)
    print(by_class.to_string())
    print()


if __name__ == "__main__":
    main()
