"""
Re-derive the 4-tier freshman profile constants (`{T1..T4}_PROFILE` in
`crates/cstat-core/src/roster_projection.rs`) from the full qualified-freshman
cohort.

These per-tier statline means are the fallback / profile scaffold for the
per-recruit freshman model: they (a) supply the non-CamPom statline of the
synthesized PlayerRow (closest-centroid to the predicted CamPom) and (b) act
as the tier-mean fallback when per-recruit inference fails. The per-recruit
LightGBM model (train_freshman_model.py) predicts `cam_v3` directly and that
drives the projection, so these profiles are near-cosmetic — but they were
last hand-derived in PR #59 from the original ~558-player sample, and the
model has since been retrained on the full class-of-2014→2025 history
(n≈3252, PR #77). This re-derives the profiles from that same larger cohort.

Cohort + qualification gate mirror train_freshman_model.py exactly:
  recruit → freshman season (recruit.year + 1), gp >= 5, mpg >= 5, non-null
  cam_gbpm_v3_psos. Tiering by composite_rank uses the same [30, 100, 250]
  thresholds as roster_projection.rs::FreshmanTier::from_rank.

Column→field mapping is taken verbatim from the real-player projection query
(roster_projection.rs:813-823) so the synthesized statline is on the same
scale as a real PlayerRow.

Run: cd training && python derive_freshman_profiles.py
Then paste the printed Rust blocks over the consts in roster_projection.rs.
"""

from __future__ import annotations

import pandas as pd

from db import get_engine

# Mirrors FreshmanTier::from_rank / TIER_THRESHOLDS in train_freshman_model.py.
TIER_THRESHOLDS = [30, 100, 250]

# (Rust field, SQL column, decimal places) in FreshmanProfile declaration order.
# Decimals match the existing const formatting so diffs stay legible.
PROFILE_FIELDS = [
    ("mpg", "mpg", 1),
    ("gp", "gp", 1),
    ("ppg", "ppg", 2),
    ("rpg", "rpg", 2),
    ("apg", "apg", 2),
    ("spg", "spg", 2),
    ("bpg", "bpg", 2),
    ("topg", "topg", 2),
    ("ts", "ts", 3),
    ("efg", "efg", 3),
    ("usg", "usg", 3),
    ("ast_pct", "ast_pct", 3),
    ("tov_pct", "tov_pct", 3),
    ("orb_pct", "orb_pct", 3),
    ("drb_pct", "drb_pct", 3),
    ("stl_pct", "stl_pct", 3),
    ("blk_pct", "blk_pct", 3),
    ("ft_rate", "ft_rate", 3),
    ("cam_v3", "cam_v3", 2),
]

# Column sources verbatim from roster_projection.rs:813-823.
COHORT_QUERY = """
SELECT
    r.composite_rank             AS composite_rank,
    pss.minutes_per_game         AS mpg,
    pss.games_played             AS gp,
    pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
    pss.true_shooting_pct        AS ts,
    pss.effective_fg_pct         AS efg,
    pss.usage_rate               AS usg,
    pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
    pss.stl_pct, pss.blk_pct, pss.ft_rate,
    t.cam_gbpm_v3_psos           AS cam_v3
FROM recruits r
JOIN torvik_player_stats t
    ON t.player_id = r.cstat_player_id AND t.season = r.year + 1
JOIN player_season_stats pss
    ON pss.player_id = r.cstat_player_id AND pss.season = r.year + 1
WHERE r.cstat_player_id IS NOT NULL
  AND t.cam_gbpm_v3_psos IS NOT NULL
  AND pss.games_played >= 5
  AND pss.minutes_per_game >= 5
"""


def tier_of(rank) -> int:
    if rank is None or pd.isna(rank) or rank > TIER_THRESHOLDS[2]:
        return 4
    if rank > TIER_THRESHOLDS[1]:
        return 3
    if rank > TIER_THRESHOLDS[0]:
        return 2
    return 1


def main() -> None:
    df = pd.read_sql(COHORT_QUERY, get_engine())
    df["tier"] = df["composite_rank"].apply(tier_of)
    n_by_tier = df["tier"].value_counts().sort_index().to_dict()
    print(f"Cohort: {len(df):,} qualified freshmen")
    print(f"  by tier: {n_by_tier}\n")

    for tier, t_const in ((1, "T1"), (2, "T2"), (3, "T3"), (4, "T4")):
        grp = df[df["tier"] == tier]
        print(f"const {t_const}_PROFILE: FreshmanProfile = FreshmanProfile {{")
        for field, col, dp in PROFILE_FIELDS:
            val = grp[col].astype(float).mean()
            print(f"    {field}: {val:.{dp}f},")
        print(f"}};  // n={len(grp)}\n")


if __name__ == "__main__":
    main()
