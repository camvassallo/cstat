"""
Phase 6 / 5b plug-in: per-recruit freshman-impact projection.

Upgrades the 4-tier mean heuristic in `crates/cstat-core/src/roster_projection.rs`
to a LightGBM regression. Same modeling shape as the trajectory model
(mean + q=0.1 + q=0.9 quantile bands), same export/meta contract, same
Rust drift validator.

Target: `torvik_player_stats.cam_gbpm_v3_psos` for the recruit's first
college season (`season = recruit.year + 1`).

Qualification gate: ≥5 GP / ≥5 MPG in the freshman season — matches the
trajectory model so we never serve a projection calibrated on rows the
trajectory model wouldn't have included.

Features (13 total):
  - 11 from the shared `recruit_features` block (locked names mirror the
    Rust side). `years_since_recruit` is constant 0 for freshmen and
    LightGBM ignores it; kept in the block for shape parity with the
    trajectory model.
  - 2 freshman-specific:
    * `committed_team_prior_adjem` — committed team's AdjEM the season
      BEFORE the recruit arrived (= recruit.year). Captures program
      quality at signing time. Avoids the dog-fooding trap of using
      the recruit's actual freshman-season team AdjEM, which would be
      partly determined by the very recruit we're projecting.
    * `peer_class_strength` — mean composite_rating across the committed
      team's full class for that year, INCLUDING the focal recruit.
      Captures whether they're the only signing or part of a wave.

Baseline to beat: 4-tier mean heuristic. Pooled MAE ~2.56 on qualified
freshmen across class-of-2024 + class-of-2025 (n ≈ 963 — exact figure
recomputed every run and recorded in the meta JSON). T1 (top-30 ranked,
~110 players) is the loose bucket with MAE 4.32 in the baseline — the
biggest room to improve.

Honest framing constants (mirror trajectory model):
  - Selection bias on top recruits is even sharper here: the elite
    cohort leaves for the draft, so the model is calibrated on
    returners-who-played-meaningful-minutes, not the full draft-eligible
    cohort. Future-Boozer top-30 freshmen are projected from a
    population thinner and more variable than headline MAE suggests.
  - Bands matter as much as the mean. Frame the surface as
    `mean (low–high)`, not a point estimate.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Optional

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score
from sklearn.model_selection import KFold

from db import get_engine
from recruit_features import RECRUIT_FEATURE_NAMES, derive_recruit_features

OUT_DIR = Path(__file__).parent / "models"

# Tier-mean baseline thresholds. Mirrors the 4-tier bucketing in
# `crates/cstat-core/src/roster_projection.rs`. Used only for diagnostic
# MAE comparison; the LightGBM model itself doesn't see these bands.
TIER_THRESHOLDS = [30, 100, 250]


def tier_of(rank: Optional[int]) -> int:
    if rank is None or rank > TIER_THRESHOLDS[2]:
        return 4
    if rank > TIER_THRESHOLDS[1]:
        return 3
    if rank > TIER_THRESHOLDS[0]:
        return 2
    return 1


# Freshman-specific feature names. Order is wire-locked; the Rust-side
# freshman feature builder must mirror this exactly. Note `years_since_recruit`
# stays in the recruit block (constant 0 here) so the shared extractor's
# 11-element shape is preserved.
FRESHMAN_EXTRA_FEATURES = [
    "committed_team_prior_adjem",
    "peer_class_strength",
]
FEATURE_COLS = list(RECRUIT_FEATURE_NAMES) + FRESHMAN_EXTRA_FEATURES
# 11 + 2 = 13 features.


# All recruit fields joined against the freshman cstat-season target.
# `prior_season` for `derive_recruit_features` is the recruit's signing
# season (`r.year`) so `years_since_recruit = signing_year - signing_year = 0`
# for every freshman row — a degenerate feature LightGBM will ignore, but
# preserved for shape parity with the trajectory model.
#
# Committed-team prior AdjEM uses a UUID-then-natstat_id traversal because
# UUIDs are season-scoped: `r.committed_team_id` points to that team in
# some season, and we want the team's `team_season_stats` row from the
# season BEFORE the recruit arrived (= `r.year`).
PAIRED_QUERY = """
SELECT
    r.cstat_player_id,
    r.year                AS recruit_year,
    r.year                AS s_n,
    r.year                AS recruit_year_raw,
    r.composite_rank      AS recruit_composite_rank_raw,
    r.composite_rating    AS recruit_composite_rating_raw,
    r.star_rating         AS recruit_star_rating_raw,
    r.position_rank       AS recruit_position_rank_raw,
    r.previous_rank       AS recruit_previous_rank_raw,
    r.height              AS recruit_height_raw,
    r.weight              AS recruit_weight_raw,
    r.position            AS recruit_position_raw,
    adjem.adj_efficiency_margin AS committed_team_prior_adjem_raw,
    peer.mean_rating      AS peer_class_strength_raw,
    t.cam_gbpm_v3_psos    AS target_campom
FROM recruits r
JOIN torvik_player_stats t
    ON t.player_id = r.cstat_player_id
    AND t.season = r.year + 1
JOIN player_season_stats pss
    ON pss.player_id = r.cstat_player_id AND pss.season = r.year + 1
LEFT JOIN teams tm_signing
    ON tm_signing.id = r.committed_team_id
LEFT JOIN teams tm_prior
    ON tm_prior.natstat_id = tm_signing.natstat_id
    AND tm_prior.season = r.year
LEFT JOIN team_season_stats adjem
    ON adjem.team_id = tm_prior.id AND adjem.season = r.year
LEFT JOIN (
    SELECT year, committed_team_id, AVG(composite_rating) AS mean_rating
    FROM recruits
    WHERE composite_rating IS NOT NULL AND committed_team_id IS NOT NULL
    GROUP BY year, committed_team_id
) peer
    ON peer.year = r.year AND peer.committed_team_id = r.committed_team_id
WHERE r.cstat_player_id IS NOT NULL
  AND t.cam_gbpm_v3_psos IS NOT NULL
  AND pss.games_played >= 5
  AND pss.minutes_per_game >= 5
"""


def build_dataset() -> pd.DataFrame:
    engine = get_engine()
    df = pd.read_sql(PAIRED_QUERY, engine)
    print(f"Loaded {len(df):,} qualified freshman rows.")

    df = derive_recruit_features(df, prior_season_col="s_n")

    # School-context features: NULL → 0.0 sentinel. The `committed_team`
    # join misses for teams without a `team_season_stats` row in
    # `recruit.year - 1` (defunct programs, conference-realignment edge
    # cases). `peer_class_strength` is NULL when the committed team has
    # no other recruits in the same class with a rating, which happens
    # for solo signings.
    df["committed_team_prior_adjem"] = df["committed_team_prior_adjem_raw"].fillna(0.0).astype(float)
    df["peer_class_strength"] = df["peer_class_strength_raw"].fillna(0.0).astype(float)

    df["tier"] = df["recruit_composite_rank_raw"].apply(tier_of)
    print(f"  by tier: {df['tier'].value_counts().sort_index().to_dict()}")
    print(f"  has prior_adjem: {df['committed_team_prior_adjem_raw'].notna().sum()}")
    print(f"  has peer_strength: {df['peer_class_strength_raw'].notna().sum()}")
    return df


def tier_mean_baseline(df: pd.DataFrame) -> dict:
    """Per-tier mean prediction. Same bucketing as roster_projection.rs."""
    means = df.groupby("tier")["target_campom"].mean().to_dict()
    pred = df["tier"].map(means)
    return {
        "tier_means": {int(k): float(v) for k, v in means.items()},
        "mae": float(mean_absolute_error(df["target_campom"], pred)),
        "rmse": float(np.sqrt(mean_squared_error(df["target_campom"], pred))),
        "r2": float(r2_score(df["target_campom"], pred)),
        "per_tier_mae": {
            int(tier): float(np.mean(np.abs(grp["target_campom"] - means[tier])))
            for tier, grp in df.groupby("tier")
        },
        "per_tier_n": df["tier"].value_counts().sort_index().to_dict(),
    }


def lgb_params(objective: str = "regression", alpha: Optional[float] = None) -> dict:
    # Conservative settings deliberately. n=963 with 13 features is small;
    # the v1 with aggressive params (num_leaves=24, n_estimators=600) lifted
    # T1 by 0.37 MAE but regressed T2/T3 because the model found spurious
    # splits in the lower-variance buckets. Tightening regularization and
    # shrinking tree complexity keeps the T1 gain while letting T2/T3 fall
    # back toward something close to the tier-mean baseline.
    p = dict(
        objective=objective,
        learning_rate=0.03,
        num_leaves=12,
        max_depth=4,
        min_data_in_leaf=30,
        feature_fraction=0.85,
        bagging_fraction=0.8,
        bagging_freq=4,
        lambda_l2=1.5,
        verbose=-1,
        n_estimators=400,
    )
    if alpha is not None:
        p["alpha"] = alpha
    return p


def kfold_cv(df: pd.DataFrame, n_splits: int = 5) -> dict:
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)
    X = df[FEATURE_COLS].values
    y = df["target_campom"].values
    fold_mae = []
    fold_rmse = []
    fold_r2 = []
    tier_arr = df["tier"].values
    per_tier_predictions: dict[int, list] = {1: [], 2: [], 3: [], 4: []}
    per_tier_truth: dict[int, list] = {1: [], 2: [], 3: [], 4: []}
    for fold_i, (tr, te) in enumerate(kf.split(X), 1):
        model = lgb.LGBMRegressor(**lgb_params("regression"))
        model.fit(X[tr], y[tr])
        preds = model.predict(X[te])
        mae = mean_absolute_error(y[te], preds)
        rmse = float(np.sqrt(mean_squared_error(y[te], preds)))
        r2 = r2_score(y[te], preds)
        fold_mae.append(float(mae))
        fold_rmse.append(rmse)
        fold_r2.append(float(r2))
        for i, t in enumerate(tier_arr[te]):
            per_tier_predictions[int(t)].append(float(preds[i]))
            per_tier_truth[int(t)].append(float(y[te][i]))
        print(f"  fold {fold_i}: MAE {mae:.3f}  RMSE {rmse:.3f}  R² {r2:.3f}")
    return {
        "mae": float(np.mean(fold_mae)),
        "rmse": float(np.mean(fold_rmse)),
        "r2": float(np.mean(fold_r2)),
        "per_fold_mae": fold_mae,
        "per_tier_mae": {
            tier: float(
                np.mean(np.abs(np.array(per_tier_predictions[tier]) - np.array(per_tier_truth[tier])))
            )
            for tier in (1, 2, 3, 4)
            if per_tier_predictions[tier]
        },
        "per_tier_n": {tier: len(per_tier_truth[tier]) for tier in (1, 2, 3, 4)},
    }


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    from onnxmltools.convert import convert_lightgbm
    from onnxconverter_common.data_types import FloatTensorType
    initial_type = [("input", FloatTensorType([None, n_features]))]
    onnx_model = convert_lightgbm(model, initial_types=initial_type, target_opset=15)
    onnx_path.write_bytes(onnx_model.SerializeToString())


def fit_final(df: pd.DataFrame, objective: str = "regression", alpha: Optional[float] = None) -> lgb.LGBMRegressor:
    model = lgb.LGBMRegressor(**lgb_params(objective, alpha))
    model.fit(df[FEATURE_COLS], df["target_campom"])
    return model


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print("=" * 60)
    print("Building dataset…")
    print("=" * 60)
    df = build_dataset()
    print(f"Features: {len(FEATURE_COLS)}  | rows: {len(df)}")

    print()
    print("=" * 60)
    print("Tier-mean baseline (current production heuristic)")
    print("=" * 60)
    baseline = tier_mean_baseline(df)
    print(f"  pooled: MAE {baseline['mae']:.3f}  RMSE {baseline['rmse']:.3f}  R² {baseline['r2']:.3f}")
    for tier in sorted(baseline["per_tier_mae"].keys()):
        n = baseline["per_tier_n"][tier]
        mae = baseline["per_tier_mae"][tier]
        mean = baseline["tier_means"][tier]
        print(f"  T{tier} (n={n:3d}, mean={mean:+.2f}): MAE {mae:.3f}")

    print()
    print("=" * 60)
    print("5-fold random CV (LightGBM mean model)")
    print("=" * 60)
    cv = kfold_cv(df)
    print(f"  pooled: MAE {cv['mae']:.3f}  RMSE {cv['rmse']:.3f}  R² {cv['r2']:.3f}")
    for tier in sorted(cv["per_tier_mae"].keys()):
        n = cv["per_tier_n"][tier]
        mae = cv["per_tier_mae"][tier]
        delta = baseline["per_tier_mae"][tier] - mae
        print(f"  T{tier} (n={n:3d}): MAE {mae:.3f}  vs baseline {baseline['per_tier_mae'][tier]:.3f}  Δ={delta:+.3f}")

    delta = baseline["mae"] - cv["mae"]
    if delta > 0:
        print(f"\n  Model beats baseline by {delta:.3f} ({100*delta/baseline['mae']:.1f}%) pooled.")
    else:
        print(f"\n  Model REGRESSED by {abs(delta):.3f}. Investigate before shipping.")

    print()
    print("=" * 60)
    print("Final fit on all data — mean + quantile (q=0.1, q=0.9)")
    print("=" * 60)
    mean_model = fit_final(df, "regression")
    lo_model = fit_final(df, "quantile", alpha=0.1)
    hi_model = fit_final(df, "quantile", alpha=0.9)

    # Top features
    print("\nTop features (mean model):")
    importance = sorted(zip(FEATURE_COLS, mean_model.feature_importances_), key=lambda x: -x[1])
    for name, imp in importance:
        print(f"  {name:35s} {imp}")

    for name, model in (("freshman_mean", mean_model), ("freshman_q10", lo_model), ("freshman_q90", hi_model)):
        path = OUT_DIR / f"{name}_model.onnx"
        export_to_onnx(model, len(FEATURE_COLS), path)
        print(f"Exported → {path}")

    meta = {
        "model": "freshman_model",
        "target": "cam_gbpm_v3_psos (freshman season = recruit.year + 1)",
        "join_key": "recruits.cstat_player_id → torvik_player_stats.player_id",
        "training_classes": sorted(df["recruit_year"].unique().tolist()),
        "n_rows": int(len(df)),
        "n_features": len(FEATURE_COLS),
        "features": FEATURE_COLS,
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        "quantile_alphas": {"q10": 0.1, "q90": 0.9},
        "tier_thresholds": TIER_THRESHOLDS,
        "tier_mean_baseline": baseline,
        "cv_5fold": cv,
        "top_features": [{"name": n, "importance": int(i)} for n, i in importance],
        "known_limitations": [
            "Selection bias on top-30 recruits: elite freshmen leave for the draft, so the calibrated cohort skews toward returners.",
            "School-context features (committed_team_prior_adjem, peer_class_strength) skip the dog-fooding trap by using the season BEFORE the recruit arrived.",
            "Sample size below ~30th ranked drops fast; bands widen accordingly. Surface the projection with the q10–q90 band, not just the mean.",
        ],
    }
    meta_path = OUT_DIR / "freshman_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
