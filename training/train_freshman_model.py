"""
Phase 6 / 5b plug-in: per-recruit freshman-impact projection.

Per-recruit LightGBM regression — the sole freshman signal in
`crates/cstat-core/src/roster_projection.rs::freshman_row`. (Historically
this replaced a 4-tier mean heuristic; those tiers were since deprecated
and deleted, see the *baseline* note below.) Same modeling shape as the
trajectory model (mean + q=0.1 + q=0.9 quantile bands), same export/meta
contract, same Rust drift validator.

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

Trained on the full **class-of-2014 through class-of-2025** paired
history (n ≈ 3253 qualified freshmen — exact figure recomputed every run
and recorded in the meta JSON). LOCO pooled MAE ≈ 2.25, beating the
rank-bucket mean baseline (≈2.42) by ~6.6%. The rank-bucket-mean baseline
is kept only as a dumb-yardstick comparison in the training output; the
4-tier scaffold it mirrored was deprecated in serving once the roster-
impact model proved it keys only on `cam_v3` / class / archetype (see
`roster_projection.rs::freshman_row`).

The mean (regression) model carries a sentinel-safe `monotone_constraints`
(non-decreasing in `recruit_composite_rating` + `recruit_star_rating`) so
that — holding the other inputs fixed — a better-rated recruit never
projects lower (a narrow guarantee; `composite_rank` stays unconstrained).
The q10/q90 band models stay unconstrained (LightGBM forbids monotone +
quantile).

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

from db import canonical_frame_order, get_engine
from provenance import input_provenance
from recruit_features import RECRUIT_FEATURE_NAMES, derive_recruit_features

OUT_DIR = Path(__file__).parent / "models"

# Rank-bucket baseline thresholds. A standalone "dumb yardstick" for the
# diagnostic MAE comparison only — the LightGBM model never sees these
# bands. (These once mirrored a 4-tier serving heuristic in
# `roster_projection.rs`; that heuristic was deprecated and deleted, so
# the buckets now live here purely as the baseline to beat.)
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

# Sentinel-safe monotonicity: force the prediction non-decreasing in
# `composite_rating` and `star_rating` so that — holding the other inputs
# fixed — a better-rated recruit never projects lower. A narrow legibility
# guarantee on the quality scores, not a global one: `composite_rank` (the
# stronger feature) stays unconstrained, so this does NOT make the
# rank-based Δ-vs-247 column monotone.
# Only features whose missing-sentinel is the FLOOR are safe:
#   - recruit_composite_rating: missing → 0.0 (= worst) ✓
#   - recruit_star_rating:      unranked → 0   (= worst) ✓
# Deliberately NOT recruit_composite_rank: its unranked sentinel is -1,
# numerically below rank 1, so a (negative) monotone there would force
# unranked recruits to project highest. Left unconstrained.
MONOTONE_INCREASING = {"recruit_composite_rating", "recruit_star_rating"}
MONOTONE_CONSTRAINTS = [
    1 if name in MONOTONE_INCREASING else 0 for name in FEATURE_COLS
]


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
-- One Torvik profile per (player, season). This join supplies the TARGET, so
-- a duplicated (player_id, season) pair produced two training rows for one
-- recruit -- double-weighting them in the fit (#311). Lowest `torvik_pid`,
-- the tiebreak used since #307.
JOIN LATERAL (
    SELECT * FROM torvik_player_stats x
    WHERE x.player_id = r.cstat_player_id
      AND x.season = r.year + 1
    ORDER BY x.torvik_pid
    LIMIT 1
) t ON TRUE
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
-- Deterministic row order (issue #222). LightGBM's `bagging_fraction`
-- subsamples by row position, so an unordered read makes the fit — and
-- therefore the OOF predictions this model persists — irreproducible.
--
-- `(cstat_player_id, year)` alone does NOT determine a row: the `pss` and
-- `t` joins both fan out for a freshman who appears on two teams in their
-- first season. (`tm_prior` cannot — `teams` is UNIQUE on
-- `(natstat_id, season)` — and `peer` is pre-grouped.)
--
-- The team id narrows it but does NOT fully close it; this frame was still
-- unstable with it in place. `db.canonical_frame_order` is what actually
-- guarantees the order. This clause is kept so the DB returns a sensible
-- order for anyone running the query by hand, not as the guarantee.
ORDER BY r.cstat_player_id, r.year, pss.team_id
"""


def build_dataset() -> pd.DataFrame:
    engine = get_engine()
    df = canonical_frame_order(pd.read_sql(PAIRED_QUERY, engine))
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
    """Per-rank-bucket mean prediction — the dumb yardstick the model beats."""
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
        # Pinned for reproducibility — without this, bagging/feature subsampling
        # re-rolls every fit and model_meta.json diffs are dominated by noise
        # (~0.05 MAE drift on identical data). `seed=42` overrides all sub-seeds
        # per LightGBM docs; `deterministic=True` is needed for full multi-thread
        # reproducibility but adds noticeable training time on larger corpora.
        seed=42,
        deterministic=True,
    )
    if alpha is not None:
        # Quantile objective. LightGBM forbids monotone_constraints here
        # ("Cannot use monotone_constraints in quantile objective"), so the
        # q10/q90 band models stay unconstrained. That's fine — the bands
        # are display-only; the load-bearing mean model (below) carries the
        # monotonicity that the served `cam_v3` needs.
        p["alpha"] = alpha
    else:
        # Mean (regression) model — the one whose output becomes the served
        # per-recruit cam_v3. Force it non-decreasing in recruit quality.
        p["monotone_constraints"] = MONOTONE_CONSTRAINTS
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


def leave_one_class_out_cv(df: pd.DataFrame) -> tuple[dict, pd.Series]:
    """Leave-one-class-out CV — the rigorous out-of-sample test.

    The 5-fold random CV above lets adjacent-class signal leak between
    train and test rows; LOCO simulates the production task ("we have
    classes A/B/C; project class D") by holding out every row from one
    recruit_year, training on the rest, scoring on the held-out cohort.

    For each held-out class we also compute the tier-mean baseline using
    train-only tier means so the comparison is apples-to-apples (the
    global baseline above uses the full corpus including the held-out
    rows, which would understate baseline error here).

    Acceptance gate: pooled LOCO MAE should beat tier-mean baseline on
    the same held-out cohorts. Blowing past ~3.0 signals serious overfit;
    landing within ±0.3 of the 5-fold CV number (2.49) means the model
    generalizes across classes.
    """
    classes = sorted(df["recruit_year"].unique())
    fold_results: dict[int, dict] = {}
    pooled_pred: list[float] = []
    pooled_truth: list[float] = []
    loco_preds = pd.Series(index=df.index, dtype=float)
    for held_year in classes:
        train_df = df[df["recruit_year"] != held_year]
        test_df = df[df["recruit_year"] == held_year]
        if len(test_df) == 0:
            continue
        model = lgb.LGBMRegressor(**lgb_params("regression"))
        model.fit(train_df[FEATURE_COLS], train_df["target_campom"])
        preds = model.predict(test_df[FEATURE_COLS])
        loco_preds.loc[test_df.index] = preds
        truth = test_df["target_campom"].values
        mae = float(mean_absolute_error(truth, preds))
        rmse = float(np.sqrt(mean_squared_error(truth, preds)))
        # `None` rather than `float('nan')` for single-row folds: NaN
        # serialises as the non-standard JSON token `NaN`, which strict
        # parsers (incl. browser `JSON.parse`) reject. None → `null`,
        # universally safe. Mirrors the pooled-r2 branch below.
        r2: Optional[float] = (
            float(r2_score(truth, preds)) if len(test_df) > 1 else None
        )
        # Tier-mean baseline using TRAIN-only tier means. Keeps the
        # comparison honest — the held-out class doesn't feed its own
        # baseline.
        train_tier_means = train_df.groupby("tier")["target_campom"].mean().to_dict()
        baseline_preds = test_df["tier"].map(train_tier_means).values
        baseline_mae = float(mean_absolute_error(truth, baseline_preds))
        per_tier: dict[int, dict] = {}
        for t in (1, 2, 3, 4):
            tier_mask = (test_df["tier"] == t).values
            n_t = int(tier_mask.sum())
            if n_t == 0:
                continue
            per_tier[t] = {
                "n": n_t,
                "model_mae": float(np.mean(np.abs(preds[tier_mask] - truth[tier_mask]))),
                "baseline_mae": float(
                    np.mean(np.abs(baseline_preds[tier_mask] - truth[tier_mask]))
                ),
            }
        fold_results[int(held_year)] = {
            "n": int(len(test_df)),
            "model_mae": mae,
            "model_rmse": rmse,
            "model_r2": r2,
            "baseline_mae": baseline_mae,
            "per_tier": per_tier,
        }
        pooled_pred.extend(preds.tolist())
        pooled_truth.extend(truth.tolist())
        print(
            f"  hold-out {held_year}: n={len(test_df):3d}  "
            f"model MAE {mae:.3f}  baseline MAE {baseline_mae:.3f}  "
            f"Δ={baseline_mae - mae:+.3f}"
        )
    pooled_pred_arr = np.array(pooled_pred)
    pooled_truth_arr = np.array(pooled_truth)
    metrics = {
        "per_fold": fold_results,
        "pooled": {
            "n": int(len(pooled_truth_arr)),
            "mae": float(mean_absolute_error(pooled_truth_arr, pooled_pred_arr)),
            "rmse": float(np.sqrt(mean_squared_error(pooled_truth_arr, pooled_pred_arr))),
            "r2": float(r2_score(pooled_truth_arr, pooled_pred_arr))
            if len(pooled_truth_arr) > 1
            else None,
        },
    }
    return metrics, loco_preds


def loco_quantile_predictions(df: pd.DataFrame, alpha: float) -> pd.Series:
    """Held-out quantile predictions across the same class-year splits as
    `leave_one_class_out_cv`. Used to assemble lower/upper bands for the
    OOF table — without this, persisted historical projections would have
    an honest mean but in-sample bands."""
    preds = pd.Series(index=df.index, dtype=float)
    for held_year in sorted(df["recruit_year"].unique()):
        train_df = df[df["recruit_year"] != held_year]
        test_df = df[df["recruit_year"] == held_year]
        if len(test_df) == 0:
            continue
        model = lgb.LGBMRegressor(**lgb_params("quantile", alpha=alpha))
        model.fit(train_df[FEATURE_COLS], train_df["target_campom"])
        preds.loc[test_df.index] = model.predict(test_df[FEATURE_COLS])
    return preds


def persist_freshman_oof(
    df: pd.DataFrame,
    mean_preds: pd.Series,
    q10_preds: pd.Series,
    q90_preds: pd.Series,
) -> None:
    """Persist LOCO held-out predictions to `freshman_oof_predictions`,
    keyed by (cstat_player_id, target_season = recruit_year + 1). Atomic
    swap inside a transaction so concurrent readers never see an empty
    window."""
    from sqlalchemy import text
    oof = pd.DataFrame({
        "cstat_player_id": df["cstat_player_id"].values,
        "target_season": (df["recruit_year"].astype(int) + 1).values,
        "mean": mean_preds.astype(float).values,
        "lower": q10_preds.astype(float).values,
        "upper": q90_preds.astype(float).values,
    })
    pre = len(oof)
    oof = oof.dropna(subset=["mean", "lower", "upper"])
    if len(oof) < pre:
        print(f"  dropped {pre - len(oof)} rows with missing predictions")
    oof = oof.drop_duplicates(subset=["cstat_player_id", "target_season"], keep="last")
    engine = get_engine()
    with engine.begin() as conn:
        conn.execute(text("TRUNCATE TABLE freshman_oof_predictions"))
        oof.to_sql(
            "freshman_oof_predictions",
            conn,
            if_exists="append",
            index=False,
        )
    print(f"  persisted {len(oof):,} OOF predictions → freshman_oof_predictions")


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    from onnxmltools.convert import convert_lightgbm
    from onnxconverter_common.data_types import FloatTensorType
    initial_type = [("input", FloatTensorType([None, n_features]))]
    onnx_model = convert_lightgbm(model, initial_types=initial_type, target_opset=15)
    # Deterministic graph name (issue #222) — onnxmltools otherwise stamps a
    # random UUID, so two exports of an identical model differ in bytes while
    # predicting identically. See train_roster_impact_model.export_to_onnx.
    onnx_model.graph.name = onnx_path.stem
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
    # Adjacent to the read it describes, not at meta-write time — see the note
    # in train_trajectory_model.main(). A post-fit stamp can claim a snapshot
    # the model never trained on, and it errs toward reporting stale as current.
    stamp = input_provenance("freshman")

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
    print("Leave-one-class-out CV (rigorous out-of-sample test)")
    print("=" * 60)
    loco, loco_mean = leave_one_class_out_cv(df)
    pooled = loco["pooled"]
    print(
        f"  pooled (n={pooled['n']}): MAE {pooled['mae']:.3f}  RMSE {pooled['rmse']:.3f}  "
        f"R² {pooled['r2']:.3f}"
    )
    # Honesty gate: pooled LOCO MAE should beat tier-mean baseline on the
    # same held-out cohorts AND stay within ~±0.3 of the 5-fold CV number.
    # Print the comparison so the takeaway is visible at the bottom of every
    # training run.
    cv_gap = pooled["mae"] - cv["mae"]
    baseline_gap = baseline["mae"] - pooled["mae"]
    if baseline_gap > 0:
        print(
            f"  Beats tier-mean baseline ({baseline['mae']:.3f}) by {baseline_gap:.3f} "
            f"({100 * baseline_gap / baseline['mae']:.1f}%) on held-out cohorts."
        )
    else:
        print(
            f"  LOSES to tier-mean baseline ({baseline['mae']:.3f}) by {-baseline_gap:.3f} "
            f"on held-out cohorts — investigate before shipping."
        )
    print(f"  Gap vs 5-fold random CV ({cv['mae']:.3f}): {cv_gap:+.3f}")

    print()
    print("=" * 60)
    print("LOCO quantile predictions (q=0.1, q=0.9) for OOF persistence")
    print("=" * 60)
    loco_q10 = loco_quantile_predictions(df, alpha=0.1)
    loco_q90 = loco_quantile_predictions(df, alpha=0.9)
    persist_freshman_oof(df, loco_mean, loco_q10, loco_q90)

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
        # Set true once the LOCO held-out predictions land in
        # `freshman_oof_predictions`. The Rust boot validator gates on
        # this so a stale meta + empty table can't silently regress the
        # Recruits page to in-sample serving.
        "oof_persisted": True,
        # Fingerprint of the Layer 0 snapshot this frame was built from
        # (issue #223). Same reasoning as the trajectory model: this writes
        # `freshman_oof_predictions`, so Layer 0 drift that stops here still
        # reaches Layer 2 under a valid `oof_provenance` stamp. Compared
        # against the live database by `check_provenance.py`.
        # Captured next to the frame read, not here — see main().
        "input_provenance": stamp,
        "tier_thresholds": TIER_THRESHOLDS,
        "tier_mean_baseline": baseline,
        "cv_5fold": cv,
        "loco_cv": loco,
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
