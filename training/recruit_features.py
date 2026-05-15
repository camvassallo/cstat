"""Shared recruit-feature extractor for trajectory + freshman-impact models.

Wire-locked feature names. The freshman-impact prior model (Phase 6) will
consume this same module so a single recruit-feature change benefits both.

Coverage caveat: only ~7% of trajectory training rows currently have recruit
data. Ingested classes: 2022, 2023, 2024, 2025, 2026 (2026 not yet played).
Class-of-2021 and earlier (most upperclassmen returners in 2024-25 and
2025-26 trajectory pairs) have no `recruits` row, so the model sees
`recruit_is_ranked=0` and sentinel values for those rows. LightGBM fits
a separate split on the unranked-majority cohort. Historical recruit
backfill (class-of-2021 and earlier) is the obvious lever to expand
coverage on a separate PR.
"""

from __future__ import annotations

from typing import Optional

import numpy as np
import pandas as pd

# Feature names in canonical order. MUST match the Rust-side
# RECRUIT_FEATURE_NAMES in cstat-core::recruit_features. The training pipeline
# and the inference path share this single source of truth via the
# trajectory_model_meta.json contract; validators hard-fail on drift.
RECRUIT_FEATURE_NAMES = [
    "recruit_is_ranked",        # 0/1 flag — explicit "we have a 247 row" signal
    "recruit_composite_rank",   # 247 overall rank (1=best). -1 if unranked.
    "recruit_composite_rating", # 247 0–1 rating score. 0.0 if missing.
    "recruit_star_rating",      # 2–5. 0 if unranked.
    "recruit_position_rank",    # Rank within position. -1 if missing.
    "recruit_rank_movement",    # previous_rank - composite_rank. 0 if either missing.
    "recruit_height_in",        # Parsed from text (e.g., "6-7" → 79). 0 if missing.
    "recruit_weight_lb",        # 0 if missing.
    "recruit_bmi_proxy",        # 703 * weight / height^2 (BMI formula). 0 if either missing.
    "recruit_position_code",    # PG=0, SG=1, SF=2, PF=3, C=4, CG=5, other=-1.
    "years_since_recruit",      # s_n - recruit.year. -1 if no recruit row.
]
N_RECRUIT_FEATURES = len(RECRUIT_FEATURE_NAMES)

# Position taxonomy from 247 (verified against ingested 2022–2026 classes).
# CG = Combo Guard (PG/SG hybrid). Anything outside this set maps to -1.
POSITION_CODES = {
    "PG": 0,
    "SG": 1,
    "SF": 2,
    "PF": 3,
    "C":  4,
    "CG": 5,
}


def parse_height_text(s: Optional[str]) -> Optional[int]:
    """247's height field is `feet-inches` text (e.g., `6-7`). Returns
    total inches or None if unparseable. Empirically all 1,147 resolved
    recruits parse cleanly (range 5-6 through 7-3)."""
    if s is None or pd.isna(s):
        return None
    try:
        feet_str, inches_str = s.split("-", 1)
        feet, inches = int(feet_str), int(inches_str)
        if feet < 4 or feet > 8 or inches < 0 or inches > 11:
            return None
        return feet * 12 + inches
    except (ValueError, AttributeError):
        return None


def encode_position(p: Optional[str]) -> int:
    """Map 247 position string to an integer code; -1 for missing/unknown.

    LightGBM treats this as a continuous feature, which is fine — the
    cardinality is small (6 values) and the model can split each off
    individually. If we ever expand to more positions or want true
    categorical handling, switch to LightGBM's `categorical_feature`.
    """
    if p is None or pd.isna(p):
        return -1
    return POSITION_CODES.get(p.strip().upper(), -1)


def derive_recruit_features(df: pd.DataFrame, prior_season_col: str = "s_n") -> pd.DataFrame:
    """Derive the locked RECRUIT_FEATURE_NAMES columns from raw recruit
    fields. Mutates the DataFrame in place and returns it for chaining.

    Input columns (callers SELECT these explicitly — the column shapes
    diverge between the trajectory query, which aliases `rec.year` as
    `recruit_year_raw`, and the freshman query, which aliases `r.year`
    three different ways — so a single SQL helper wouldn't fit both):
        recruit_composite_rank_raw, recruit_composite_rating_raw,
        recruit_star_rating_raw, recruit_position_rank_raw,
        recruit_previous_rank_raw, recruit_height_raw, recruit_weight_raw,
        recruit_position_raw, recruit_year_raw

    Plus `prior_season_col` (default `s_n`) for the `years_since_recruit`
    derivation.

    NULL handling:
      - `recruit_is_ranked = 1 if recruit_year_raw is not null else 0`
        (the year column is NOT NULL in `recruits`, so this is the cleanest
        existence check across the LEFT JOIN).
      - All rank-like fields use -1 sentinel for missing.
      - All rate/count fields use 0.0 for missing.
      - Position uses -1 for missing/unknown.
    """
    has_recruit = df["recruit_year_raw"].notna()
    df["recruit_is_ranked"] = has_recruit.astype(int)

    # Rank fields → -1 sentinel.
    df["recruit_composite_rank"] = (
        df["recruit_composite_rank_raw"].fillna(-1).astype(int)
    )
    df["recruit_position_rank"] = (
        df["recruit_position_rank_raw"].fillna(-1).astype(int)
    )

    # Rating / star → 0.0 / 0.
    df["recruit_composite_rating"] = (
        df["recruit_composite_rating_raw"].fillna(0.0).astype(float)
    )
    df["recruit_star_rating"] = (
        df["recruit_star_rating_raw"].fillna(0).astype(int)
    )

    # Rank movement: positive = climbed (previous rank was higher number),
    # negative = fell. Set to 0 when either rank is missing — neutral signal.
    prev = df["recruit_previous_rank_raw"]
    curr = df["recruit_composite_rank_raw"]
    movement = prev - curr
    df["recruit_rank_movement"] = movement.fillna(0).astype(int)

    # Height parse + weight.
    df["recruit_height_in"] = (
        df["recruit_height_raw"].apply(parse_height_text).fillna(0).astype(int)
    )
    df["recruit_weight_lb"] = (
        df["recruit_weight_raw"].fillna(0).astype(int)
    )

    # BMI proxy using the standard imperial formula. Use safe division so the
    # 0-height fallback doesn't NaN out. Vectorised because `np.where` is
    # cheap and we want to avoid surprise division-by-zero warnings.
    h = df["recruit_height_in"].astype(float)
    w = df["recruit_weight_lb"].astype(float)
    safe_h2 = np.where(h > 0, h * h, 1.0)  # placeholder denom; mask kills it
    bmi = 703.0 * w / safe_h2
    df["recruit_bmi_proxy"] = np.where((h > 0) & (w > 0), bmi, 0.0)

    # Position code.
    df["recruit_position_code"] = (
        df["recruit_position_raw"].apply(encode_position).astype(int)
    )

    # Years since recruit. Sentinel -1 when no recruit row (so the model
    # can split on "did they have a recruit row at all" via is_ranked AND
    # the years_since signal at the same time).
    years_since = df[prior_season_col] - df["recruit_year_raw"]
    df["years_since_recruit"] = years_since.fillna(-1).astype(int)

    return df
