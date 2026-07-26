"""
Phase 5c growth model: project a returning player's next-season CamPom v3.

One row per (torvik_pid, season_N, season_N+1) pair. Trained on every
consecutive-season player in DB (currently the 11 pairs 2015→2016 ..
2025→2026, ~24,600 rows after the qualification gate).

Target: next-season `torvik_player_stats.cam_gbpm_v3_psos`.

Features are anchored on the prior season — rate stats + impact metrics
(CamPom + GBPM components) + a multi-season history block (prior-PRIOR-season
CamPom/mpg/gp/usg/ppg levels + year-over-year slope deltas + a has_prior2
indicator, so the model sees a player's progression trajectory rather than a
single snapshot; validated 2026-06-18, ~53% coverage, biggest lift on
upperclassmen) + prior-season on/off splits (on-court net
rating, on/off swing, possession share; `player_on_off` rollup, -999
sentinel where the rollup has no row — accepted by the Tier-2 membership
backtest 2026-06-11, see eval_history) + archetype mixture (primary 1.0× /
secondary 0.5×) + volume + class_year + height + recruit-rank block
(composite rank, rating, star, position rank, rank movement, height/weight,
BMI proxy, position code, years_since_recruit). Recruit features come via LEFT JOIN
on `recruits.cstat_player_id`; only class-of-2024/2025 are ingested, so
~7% of training rows have a recruit row and the rest fall into the
`recruit_is_ranked=0` bucket. LightGBM fits a separate split on the
majority-unranked cohort. Shared feature derivation lives in
`training/recruit_features.py` so the freshman-impact prior model can
reuse it without divergence.

Cross-season pairing is via `torvik_pid` (per memory: stable cross-season
key; `natstat_id` breaks on transfers — different code per team).
Transfers ARE included; the model is destination-agnostic in v1
(documented limitation).

Three LightGBMs trained per run: mean + q=0.1 + q=0.9. Three ONNX files
shipped so the Rust inference path can return (predicted, lower, upper)
as a single floor/ceiling band on PlayerDetail.

Honest framing: the corpus is deep (11 pairs) but per-player projections
remain directional — pooled LOPO MAE ~2.1 CamPom points vs a ~2.3 naive
baseline. Document MAE per bucket in the meta JSON; surface the headline
MAE in the UI so users understand the projection is directional, not a
point estimate.
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
SEASONS = (2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024, 2025, 2026)
ARCHETYPES = (
    "Wizard", "Sorcerer", "Warlock", "Bard", "Ranger", "Barbarian",
    "Paladin", "Monk", "Cleric", "Druid", "Rogue", "Fighter",
)

# Pull the prior-season feature row joined to the next-season target.
# Cross-season join via torvik_pid (stable across team changes).
# Qualification gate: ≥5 GP / ≥5 MPG in BOTH seasons — matches the
# roster_model gate so the Rust inference path can share the QUAL_FILTER
# string and we don't need a second gate for trajectory inputs.
#
# Recruit features: LEFT JOIN on `recruits.cstat_player_id`. ~7% of rows
# have a recruit row (class-of-2024/2025 only); the rest fall into the
# `recruit_is_ranked=0` bucket via `derive_recruit_features()`. See
# `training/recruit_features.py` for the column → feature derivation.
PAIRED_QUERY = """
WITH base AS (
    SELECT
        a.torvik_pid,
        a.season AS s_n,
        a.player_id AS pid_n,
        b.season AS s_np1,
        b.player_id AS pid_np1,
        b.cam_gbpm_v3_psos AS target_campom,
        -- Prior-PRIOR (N-1) season for the multi-season history block. LEFT
        -- JOIN on the same stable torvik_pid: adds columns, drops zero rows,
        -- so the row set is identical to the single-season contract. NULL for
        -- freshmen-as-N and careers starting before the 2015 data floor.
        c.player_id AS pid_nm1,
        c.cam_gbpm_v3_psos AS prior2_campom
    FROM torvik_player_stats a
    JOIN torvik_player_stats b
        ON a.torvik_pid = b.torvik_pid
        AND b.season = a.season + 1
    LEFT JOIN torvik_player_stats c
        ON c.torvik_pid = a.torvik_pid AND c.season = a.season - 1
    WHERE a.torvik_pid IS NOT NULL
      AND a.cam_gbpm_v3_psos IS NOT NULL
      AND b.cam_gbpm_v3_psos IS NOT NULL
      AND a.season = ANY(%(seasons)s)
)
SELECT
    base.torvik_pid,
    base.s_n,
    base.s_np1,
    base.target_campom,
    -- Volume / context
    pssN.minutes_per_game AS prior_mpg,
    pssN.games_played AS prior_gp,
    COALESCE(pssN.minutes_per_game, 0) * COALESCE(pssN.games_played, 0) AS prior_total_min,
    plyN.height_inches AS prior_height_in,
    plyN.class_year AS prior_class_year,
    -- Box score per-game
    pssN.ppg AS prior_ppg,
    pssN.rpg AS prior_rpg,
    pssN.apg AS prior_apg,
    pssN.spg AS prior_spg,
    pssN.bpg AS prior_bpg,
    pssN.topg AS prior_topg,
    -- Rate stats
    pssN.true_shooting_pct AS prior_ts,
    pssN.effective_fg_pct AS prior_efg,
    pssN.usage_rate AS prior_usg,
    pssN.ast_pct AS prior_ast_pct,
    pssN.tov_pct AS prior_tov_pct,
    pssN.orb_pct AS prior_orb_pct,
    pssN.drb_pct AS prior_drb_pct,
    pssN.stl_pct AS prior_stl_pct,
    pssN.blk_pct AS prior_blk_pct,
    pssN.ft_rate AS prior_ft_rate,
    -- Impact metrics (the load-bearing features)
    tpsN.ogbpm AS prior_ogbpm,
    tpsN.dgbpm AS prior_dgbpm,
    tpsN.gbpm AS prior_gbpm,
    tpsN.cam_gbpm_v3_psos AS prior_campom,
    -- On/off splits (Tier-2 membership features; backtested 2026-06-11).
    -- LEFT JOIN: the rollup's >=100-on-possession gate is stricter than the
    -- 5 GP / 5 MPG gate here, so bench-fringe rows legitimately miss; 2019
    -- misses entirely (no PBP/lineups). NULL -> -999 sentinel downstream.
    ooN.on_net_rtg AS prior_on_net_rtg,
    ooN.net_on_off AS prior_net_on_off,
    CASE WHEN ooN.on_possessions_for + ooN.off_possessions_for > 0
         THEN ooN.on_possessions_for
              / (ooN.on_possessions_for + ooN.off_possessions_for)
    END AS prior_on_poss_share,
    -- Archetype mixture (primary + secondary)
    paN.primary_class AS prior_primary_class,
    paN.secondary_class AS prior_secondary_class,
    -- Recruit fields (LEFT JOIN). Derived by recruit_features.py.
    rec.composite_rank   AS recruit_composite_rank_raw,
    rec.composite_rating AS recruit_composite_rating_raw,
    rec.star_rating      AS recruit_star_rating_raw,
    rec.position_rank    AS recruit_position_rank_raw,
    rec.previous_rank    AS recruit_previous_rank_raw,
    rec.height           AS recruit_height_raw,
    rec.weight           AS recruit_weight_raw,
    rec.position         AS recruit_position_raw,
    rec.year             AS recruit_year_raw,
    -- Multi-season history (N-1): lag-2 CamPom level from the base CTE, plus
    -- N-1 box/role from player_season_stats. Slope (delta_*) is derived in
    -- build_dataset() as prior_N − prior_{N-1}. NULL where no N-1 season.
    base.prior2_campom AS prior2_campom,
    pssNM1.minutes_per_game AS prior2_mpg,
    pssNM1.games_played AS prior2_gp,
    pssNM1.usage_rate AS prior2_usg,
    pssNM1.ppg AS prior2_ppg
FROM base
JOIN player_season_stats pssN
    ON pssN.player_id = base.pid_n AND pssN.season = base.s_n
JOIN player_season_stats pssNP1
    ON pssNP1.player_id = base.pid_np1 AND pssNP1.season = base.s_np1
JOIN players plyN
    ON plyN.id = base.pid_n
JOIN torvik_player_stats tpsN
    ON tpsN.player_id = base.pid_n AND tpsN.season = base.s_n
LEFT JOIN player_archetypes paN
    ON paN.player_id = base.pid_n AND paN.season = base.s_n
LEFT JOIN player_on_off ooN
    ON ooN.player_id = base.pid_n AND ooN.season = base.s_n
LEFT JOIN player_season_stats pssNM1
    ON pssNM1.player_id = base.pid_nm1 AND pssNM1.season = base.s_n - 1
LEFT JOIN recruits rec
    ON rec.cstat_player_id = base.pid_n
WHERE pssN.minutes_per_game >= 5
  AND pssN.games_played >= 5
  AND pssNP1.minutes_per_game >= 5
  AND pssNP1.games_played >= 5
-- Deterministic row order (issue #222). LightGBM's `bagging_fraction`
-- subsamples by row position, so an unordered read makes the fit — and
-- therefore the OOF predictions this model persists — irreproducible.
--
-- `(torvik_pid, s_n)` alone does NOT determine a row: `player_season_stats`
-- is unique on `(player_id, team_id, season)`, so a player who appears on
-- two teams in one season fans the pssN / pssNP1 / pssNM1 joins out.
--
-- The team ids narrow it but do NOT fully close it; this frame was still
-- unstable with them in place. `db.canonical_frame_order` is what actually
-- guarantees the order. This clause is kept so the DB returns a sensible
-- order for anyone running the query by hand, not as the guarantee.
--
-- (Those fan-out rows are a pre-existing property of this query — 274 of
-- them are exact duplicates, which double-weights multi-team player-seasons
-- in training. Out of scope here; noted in #222.)
ORDER BY base.torvik_pid, base.s_n,
         pssN.team_id, pssNP1.team_id, pssNM1.team_id
"""

# Class year encoding. NULL maps to -1 (separate bucket — LightGBM splits
# can isolate it). Keeps the spelling permissive (NatStat uses "Freshman"
# / "Senior"; Torvik backfill uses "Fr"/"Sr"). If a value is unknown, it
# falls into the -1 bucket too.
CLASS_YEAR_CODES = {
    "Fr": 0, "Freshman": 0,
    "So": 1, "Sophomore": 1,
    "Jr": 2, "Junior": 2,
    "Sr": 3, "Senior": 3,
    "Gr": 4, "Graduate": 4, "Grad": 4,
}

# Locked feature order — must match the Rust-side TRAJECTORY_FEATURE_NAMES
# in cstat-core. Boot-time validator hard-fails if these drift.
NUMERIC_FEATURE_COLS = [
    "prior_mpg", "prior_gp", "prior_total_min", "prior_height_in",
    "prior_class_year_code",
    "prior_ppg", "prior_rpg", "prior_apg", "prior_spg", "prior_bpg", "prior_topg",
    "prior_ts", "prior_efg", "prior_usg",
    "prior_ast_pct", "prior_tov_pct",
    "prior_orb_pct", "prior_drb_pct", "prior_stl_pct", "prior_blk_pct", "prior_ft_rate",
    "prior_ogbpm", "prior_dgbpm", "prior_gbpm", "prior_campom",
    "prior_on_net_rtg", "prior_net_on_off", "prior_on_poss_share",
    # Multi-season history block (9) — lag-2 levels + has_prior2 indicator +
    # slope deltas. Validated 2026-06-18 (covered LOPO MAE 2.206→2.141, 10/11
    # folds), serve-parity confirmed under sentinel encoding 2026-06-27
    # (covered +0.0625). Levels fill -999 / deltas fill 0 where no N-1 season
    # exists; has_prior2 lets the tree isolate the no-history cohort. See
    # docs/trajectory_methodology.md and eval_history/trajectory_history_*.
    "prior2_campom", "prior2_mpg", "prior2_gp", "prior2_usg", "prior2_ppg",
    "has_prior2",
    "delta_campom", "delta_mpg", "delta_usg",
]
ARCH_FEATURE_COLS = [f"arch_{a.lower()}" for a in ARCHETYPES]
# Numeric (37) + archetype shares (12) + recruit block (11) = 60 features.
# Recruit block order is locked in `training/recruit_features.py` and
# mirrored by `cstat-core::recruit_features::RECRUIT_FEATURE_NAMES`.
FEATURE_COLS = NUMERIC_FEATURE_COLS + ARCH_FEATURE_COLS + list(RECRUIT_FEATURE_NAMES)

# On/off features are NULL where the player_on_off rollup has no row
# (sub-rotation players, 2019's missing PBP) or where the swing has no
# OFF sample (iron-men under the >=10-possession OFF floor). The Rust
# serve path fills the same sentinel — -999 is cleanly outside every
# real range (net ratings +/-~60, share in [0,1]) and, unlike NaN, needs
# no special plumbing through the ONNX input tensor.
ONOFF_FEATURE_COLS = ["prior_on_net_rtg", "prior_net_on_off", "prior_on_poss_share"]
ONOFF_MISSING_SENTINEL = -999.0

# Multi-season history fills. Lag-2 LEVELS get the same -999 out-of-range
# sentinel as on/off (CamPom/mpg/gp/usg/ppg never reach it). SLOPE deltas
# fill 0.0 — a delta of 0 ("no change") is consistent train↔serve and the
# `has_prior2` indicator lets the tree split out the no-history cohort
# explicitly, so the exact delta value for absent rows is immaterial. Serve
# parity (sentinel vs NaN-native) cost only ~0.004 covered MAE (2026-06-27).
LAG2_LEVEL_COLS = ["prior2_campom", "prior2_mpg", "prior2_gp", "prior2_usg", "prior2_ppg"]
SLOPE_DELTA_COLS = ["delta_campom", "delta_mpg", "delta_usg"]
LAG2_LEVEL_SENTINEL = -999.0
SLOPE_DELTA_FILL = 0.0


def encode_class_year(s: Optional[str]) -> int:
    # `pd.isna` covers both `None` (postgres TEXT NULL via SQLAlchemy) and
    # `NaN` (hand-constructed DataFrames or alt drivers); plain `s is None`
    # was a latent crash for the NaN case. Empirically real ingest only
    # produces None, but we want the function to be safe to reuse.
    if s is None or pd.isna(s):
        return -1
    return CLASS_YEAR_CODES.get(s.strip(), -1)


def add_archetype_columns(df: pd.DataFrame) -> pd.DataFrame:
    """Primary class contributes 1.0; secondary contributes 0.5. Matches the
    team-level Identity/Gaps weighting in §5a so a hybrid Druid/Sorcerer
    registers presence on both axes."""
    for arch in ARCHETYPES:
        col = f"arch_{arch.lower()}"
        df[col] = (
            (df["prior_primary_class"] == arch).astype(float)
            + 0.5 * (df["prior_secondary_class"] == arch).astype(float)
        )
    return df


def build_dataset() -> pd.DataFrame:
    engine = get_engine()
    df = canonical_frame_order(
        pd.read_sql(PAIRED_QUERY, engine, params={"seasons": list(SEASONS)})
    )
    print(f"Loaded {len(df):,} paired (season_N → season_N+1) rows.")

    df["prior_class_year_code"] = df["prior_class_year"].map(encode_class_year)
    df = add_archetype_columns(df)
    df = derive_recruit_features(df, prior_season_col="s_n")

    # Drop any row missing the headline impact feature — CamPom is in the
    # WHERE clause already, but ogbpm/dgbpm/gbpm can still be NULL for the
    # rare Torvik row without GBPM components.
    pre_drop = len(df)
    df = df.dropna(subset=["prior_campom", "prior_ogbpm", "prior_dgbpm"])
    if len(df) < pre_drop:
        print(f"  dropped {pre_drop - len(df)} rows missing GBPM components")

    # On/off NULLs become the sentinel, never a dropped row — coverage is
    # structural (era + rotation gate), not data quality.
    for col in ONOFF_FEATURE_COLS:
        df[col] = df[col].fillna(ONOFF_MISSING_SENTINEL).astype(float)
    onoff_cov = float((df["prior_on_net_rtg"] != ONOFF_MISSING_SENTINEL).mean())
    print(f"  on/off feature coverage: {onoff_cov:.1%}")

    # Multi-season history block. `has_prior2` and the slope deltas are derived
    # from the still-NaN lag-2 columns (so absence is read off the real NULL
    # pattern), THEN the levels/deltas are sentinel-filled. Mirrors the Rust
    # serve path in `trajectory.rs::build_trajectory_features`.
    df["has_prior2"] = df["prior2_campom"].notna().astype(float)
    df["delta_campom"] = df["prior_campom"] - df["prior2_campom"]
    df["delta_mpg"] = df["prior_mpg"] - df["prior2_mpg"]
    df["delta_usg"] = df["prior_usg"] - df["prior2_usg"]
    for col in LAG2_LEVEL_COLS:
        df[col] = df[col].fillna(LAG2_LEVEL_SENTINEL).astype(float)
    for col in SLOPE_DELTA_COLS:
        df[col] = df[col].fillna(SLOPE_DELTA_FILL).astype(float)
    prior2_cov = float((df["has_prior2"] == 1.0).mean())
    print(f"  prior-2 season coverage: {prior2_cov:.1%}")

    print(f"After gates: {len(df):,} rows.")
    print(f"  by pair: {df.groupby(['s_n', 's_np1']).size().to_dict()}")
    print(f"  by class_year: {df['prior_class_year_code'].value_counts(dropna=False).sort_index().to_dict()}")
    return df


def lgb_params(objective: str = "regression", alpha: Optional[float] = None) -> dict:
    """Shared shape for mean + quantile models. Same conservative knobs as
    the roster model (~4k rows / ~37 features). Quantile fits use the same
    leaves/lr; only the objective differs.

    No early stopping, fixed 400-iter budget (per the roster/freshman-model
    precedent) — used verbatim by both the honest backtest (LOPO / quantile /
    k-fold) and the final served fit. Keeping one param set is what keeps them
    identical: an early-stopping variant here would have to hand the held-out
    fold to the fit as an eval_set, letting `best_iteration_` peek at the test
    labels and optimistically biasing the persisted OOF numbers (issue #199)."""
    p = {
        "objective": objective,
        "metric": "mae" if objective == "regression" else "quantile",
        "num_leaves": 24,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 25,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 400,
        # Pinned for reproducibility — without this, bagging/feature subsampling
        # re-rolls every fit. `seed=42` overrides all sub-seeds per LightGBM docs;
        # `deterministic=True` is needed for full multi-thread reproducibility.
        "seed": 42,
        "deterministic": True,
    }
    if alpha is not None:
        p["alpha"] = alpha
    return p


def naive_baseline(df: pd.DataFrame) -> dict:
    """Acceptance criterion: model must beat 'year N+1 ≈ year N CamPom'.
    If the model can't, we're not learning anything year-over-year."""
    y_true = df["target_campom"].values
    y_naive = df["prior_campom"].values
    return {
        "mae": float(mean_absolute_error(y_true, y_naive)),
        "rmse": float(np.sqrt(mean_squared_error(y_true, y_naive))),
        "r2": float(r2_score(y_true, y_naive)),
    }


def leave_one_pair_out(df: pd.DataFrame) -> tuple[dict, pd.Series]:
    """Honest-backtest analog of the roster model's LOSO. With 11 pairs
    (2015→16 .. 2025→26), train on ten pairs and predict the held-out
    eleventh — repeat.

    Returns (metrics_dict, lopo_predictions). `lopo_predictions` is a Series
    aligned to `df.index` with each row's held-out mean prediction, used
    for OOF persistence so historical-year API routes serve honest numbers
    instead of in-sample inference.
    """
    results = {}
    lopo_preds = pd.Series(index=df.index, dtype=float)
    pairs = df[["s_n", "s_np1"]].drop_duplicates().to_dict("records")
    overall_y, overall_p = [], []
    for held in pairs:
        mask = (df["s_n"] == held["s_n"]) & (df["s_np1"] == held["s_np1"])
        train = df[~mask]
        test = df[mask]
        if len(train) == 0 or len(test) == 0:
            continue
        X_tr, y_tr = train[FEATURE_COLS], train["target_campom"]
        X_te, y_te = test[FEATURE_COLS], test["target_campom"]
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X_tr, y_tr)
        preds = model.predict(X_te)
        lopo_preds.loc[mask] = preds
        key = f"{held['s_n']}->{held['s_np1']}"
        results[key] = {
            "mae": float(mean_absolute_error(y_te, preds)),
            "rmse": float(np.sqrt(mean_squared_error(y_te, preds))),
            "r2": float(r2_score(y_te, preds)),
            "n": int(len(test)),
        }
        overall_y.extend(y_te.tolist())
        overall_p.extend(preds.tolist())
        print(f"  pair {key}: MAE {results[key]['mae']:.3f}  RMSE {results[key]['rmse']:.3f}  R² {results[key]['r2']:.3f}  n={results[key]['n']}")
    overall = {
        "mae": float(mean_absolute_error(overall_y, overall_p)),
        "rmse": float(np.sqrt(mean_squared_error(overall_y, overall_p))),
        "r2": float(r2_score(overall_y, overall_p)),
    }
    print(f"  pooled:      MAE {overall['mae']:.3f}  RMSE {overall['rmse']:.3f}  R² {overall['r2']:.3f}")
    return {"per_pair": results, "pooled": overall}, lopo_preds


def lopo_quantile_predictions(df: pd.DataFrame, alpha: float) -> pd.Series:
    """Held-out quantile predictions across the same pair splits as
    `leave_one_pair_out`. Used to assemble lower/upper bands for the OOF
    table — without this, persisted historical projections would have an
    honest mean but in-sample bands."""
    preds = pd.Series(index=df.index, dtype=float)
    pairs = df[["s_n", "s_np1"]].drop_duplicates().to_dict("records")
    for held in pairs:
        mask = (df["s_n"] == held["s_n"]) & (df["s_np1"] == held["s_np1"])
        train = df[~mask]
        test = df[mask]
        if len(train) == 0 or len(test) == 0:
            continue
        X_tr, y_tr = train[FEATURE_COLS], train["target_campom"]
        X_te = test[FEATURE_COLS]
        model = lgb.LGBMRegressor(**lgb_params(objective="quantile", alpha=alpha))
        model.fit(X_tr, y_tr)
        preds.loc[mask] = model.predict(X_te)
    return preds


def kfold_cv(df: pd.DataFrame, n_splits: int = 5) -> tuple[dict, np.ndarray]:
    """Returns (summary_dict, oof_predictions). `oof_predictions` is an
    array aligned to `df.index` order with each row's prediction from the
    fold that held it out — the honest "what would the model say about
    this row if it hadn't trained on it" signal, useful for downstream
    diagnostics (per-bucket MAE, calibration plots, etc.).
    """
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)
    X = df[FEATURE_COLS].values
    y = df["target_campom"].values
    oof = np.full(len(df), np.nan)
    maes, rmses, r2s = [], [], []
    for fold, (tr, te) in enumerate(kf.split(X), 1):
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X[tr], y[tr])
        p = model.predict(X[te])
        oof[te] = p
        maes.append(float(mean_absolute_error(y[te], p)))
        rmses.append(float(np.sqrt(mean_squared_error(y[te], p))))
        r2s.append(float(r2_score(y[te], p)))
        print(f"  fold {fold}: MAE {maes[-1]:.3f}  RMSE {rmses[-1]:.3f}  R² {r2s[-1]:.3f}")
    summary = {
        "mae": float(np.mean(maes)), "rmse": float(np.mean(rmses)), "r2": float(np.mean(r2s)),
        "per_fold_mae": maes,
    }
    return summary, oof


def mae_by_class_year(df: pd.DataFrame, model: lgb.LGBMRegressor) -> dict:
    """Diagnostic per ROADMAP §5c — per-bucket MAE vs naive baseline. The
    bucket is the PRIOR class year, so 0=Fr→So projection, 3=Sr→Gr, etc."""
    preds = model.predict(df[FEATURE_COLS])
    out = {}
    for code, sub in df.groupby("prior_class_year_code"):
        sub_preds = preds[sub.index]
        out[str(int(code))] = {
            "n": int(len(sub)),
            "model_mae": float(mean_absolute_error(sub["target_campom"], sub_preds)),
            "naive_mae": float(mean_absolute_error(sub["target_campom"], sub["prior_campom"])),
        }
    return out


# Per-current-CamPom buckets for the regression-bias diagnostic. The model
# fits a roughly symmetric distribution around the bulk of training data
# (≈-3 to +10), so predictions for inputs in the elite tail (+15+) skew
# toward the conditional median of similar-but-not-as-elite returners.
# Buckets are chosen to land at least ~30 rows in each cell across the
# current 4-class corpus; widen the highest bucket because +20+ inputs
# are sparse (mostly returners-who-didn't-leave-for-NBA — selection bias
# compounds the regression).
CURRENT_CAMPOM_BUCKETS: list[tuple[str, float, float]] = [
    ("<-5",     float("-inf"),  -5.0),
    ("-5..0",   -5.0,            0.0),
    ("0..+5",    0.0,            5.0),
    ("+5..+10",  5.0,           10.0),
    ("+10..+15", 10.0,          15.0),
    ("+15..+20", 15.0,          20.0),
    (">=+20",    20.0,  float("inf")),
]


def mae_by_current_campom(df: pd.DataFrame, oof: np.ndarray) -> dict:
    """Diagnostic for the regression-to-the-mean behaviour at the elite
    tail. Bucket OOF predictions by `prior_campom` (the player's
    current-season CamPom — the model's input), report per-bucket MAE +
    mean predicted vs mean actual + bias.

    Bias = mean(pred) − mean(actual). Negative bias on the highest buckets
    is the smoking gun for regression-to-the-mean: the model
    systematically projects elite returners below their actual next-season
    value. The Q1 narrative in the ROADMAP cites these numbers; the
    tooltip on the trajectory chip should quote them so the bias is
    legible to users staring at a "+30 → +16" projection.
    """
    actual = df["target_campom"].to_numpy()
    prior = df["prior_campom"].to_numpy()
    out: dict[str, dict] = {}
    for label, lo, hi in CURRENT_CAMPOM_BUCKETS:
        mask = (prior >= lo) & (prior < hi)
        n = int(mask.sum())
        if n == 0:
            continue
        bucket_pred = oof[mask]
        bucket_actual = actual[mask]
        out[label] = {
            "n": n,
            "lo": lo if lo > float("-inf") else None,
            "hi": hi if hi < float("inf") else None,
            "mean_prior": float(prior[mask].mean()),
            "mean_pred": float(bucket_pred.mean()),
            "mean_actual": float(bucket_actual.mean()),
            "model_mae": float(np.mean(np.abs(bucket_pred - bucket_actual))),
            # Bias is the load-bearing number. Negative on +15+ buckets =
            # confirms regression-to-the-mean. Naive baseline for context:
            # `prior_campom` as the prediction (i.e. "next year = this year").
            "model_bias": float((bucket_pred - bucket_actual).mean()),
            "naive_mae": float(np.mean(np.abs(prior[mask] - bucket_actual))),
        }
    return out


def persist_trajectory_oof(
    df: pd.DataFrame,
    mean_preds: pd.Series,
    q10_preds: pd.Series,
    q90_preds: pd.Series,
) -> None:
    """Persist LOPO held-out predictions to `trajectory_oof_predictions`,
    keyed by (torvik_pid, target_season=s_np1). Replaces all rows so the
    table tracks the freshest training cohort. Wrapped in a transaction so
    concurrent readers see either the prior set or the new set, never an
    empty window."""
    from sqlalchemy import text
    oof = pd.DataFrame({
        "torvik_pid": df["torvik_pid"].astype(int).values,
        "target_season": df["s_np1"].astype(int).values,
        "mean": mean_preds.astype(float).values,
        "lower": q10_preds.astype(float).values,
        "upper": q90_preds.astype(float).values,
    })
    # Defensive: drop any row missing one of the three predictions (shouldn't
    # happen if every pair had both train + test rows, but tolerate small
    # cohorts where a pair is degenerate).
    pre = len(oof)
    oof = oof.dropna(subset=["mean", "lower", "upper"])
    if len(oof) < pre:
        print(f"  dropped {pre - len(oof)} rows with missing predictions")
    # Also drop dupes — defensive: the LOPO loop produces exactly one row
    # per (torvik_pid, s_np1) by construction, but if `build_dataset` ever
    # returns dupes the upstream invariant is what'll save us.
    oof = oof.drop_duplicates(subset=["torvik_pid", "target_season"], keep="last")
    engine = get_engine()
    with engine.begin() as conn:
        conn.execute(text("TRUNCATE TABLE trajectory_oof_predictions"))
        oof.to_sql(
            "trajectory_oof_predictions",
            conn,
            if_exists="append",
            index=False,
        )
    print(f"  persisted {len(oof):,} OOF predictions → trajectory_oof_predictions")


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    import onnxmltools
    from onnxmltools.convert.common.data_types import FloatTensorType
    initial_types = [("input", FloatTensorType([None, n_features]))]
    onnx_model = onnxmltools.convert_lightgbm(
        model.booster_, initial_types=initial_types, target_opset=15
    )
    # Deterministic graph name (issue #222) — onnxmltools otherwise stamps a
    # random UUID, so two exports of an identical model differ in bytes while
    # predicting identically. See train_roster_impact_model.export_to_onnx.
    onnx_model.graph.name = onnx_path.stem
    onnxmltools.utils.save_model(onnx_model, str(onnx_path))


def fit_final(df: pd.DataFrame, objective: str = "regression", alpha: Optional[float] = None) -> lgb.LGBMRegressor:
    """Fit on ALL paired rows. Uses the same `lgb_params` (400 iters, no early
    stopping) as the honest backtest, so the served fit and the persisted OOF
    numbers come from an identical training recipe."""
    params = lgb_params(objective=objective, alpha=alpha)
    model = lgb.LGBMRegressor(**params)
    model.fit(df[FEATURE_COLS], df["target_campom"])
    return model


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print("=" * 60)
    print("Building dataset...")
    df = build_dataset()
    df = df.reset_index(drop=True)
    print(f"Features: {len(FEATURE_COLS)}  | rows: {len(df)}")
    # Fingerprint the inputs HERE, adjacent to the read they describe — not at
    # meta-write time. A stamp taken after the fit would describe the database
    # as it is when training finishes, and a retrain that overlaps a nightly
    # `compute_all` would then claim the model trained on a snapshot it never
    # saw. That error points the wrong way: it reports a stale model as
    # current, which is the failure this whole chain exists to catch.
    stamp = input_provenance("trajectory")

    print("\n" + "=" * 60)
    print(f"Naive baseline (year N+1 ≈ year N CamPom)")
    print("=" * 60)
    naive = naive_baseline(df)
    print(f"  pooled: MAE {naive['mae']:.3f}  RMSE {naive['rmse']:.3f}  R² {naive['r2']:.3f}")

    print("\n" + "=" * 60)
    print("Leave-one-pair-out backtest")
    print("=" * 60)
    lopo, lopo_mean = leave_one_pair_out(df)

    print("\n" + "=" * 60)
    print("5-fold random CV")
    print("=" * 60)
    cv, oof_preds = kfold_cv(df)

    print("\n" + "=" * 60)
    print("LOPO quantile predictions (q=0.1, q=0.9) for OOF persistence")
    print("=" * 60)
    lopo_q10 = lopo_quantile_predictions(df, alpha=0.1)
    lopo_q90 = lopo_quantile_predictions(df, alpha=0.9)
    persist_trajectory_oof(df, lopo_mean, lopo_q10, lopo_q90)

    print("\n" + "=" * 60)
    print("Final fit on all data — mean + quantile (q=0.1, q=0.9)")
    print("=" * 60)
    mean_model = fit_final(df, objective="regression")
    lo_model = fit_final(df, objective="quantile", alpha=0.1)
    hi_model = fit_final(df, objective="quantile", alpha=0.9)

    importance = sorted(zip(FEATURE_COLS, mean_model.feature_importances_), key=lambda x: -x[1])
    print("\nTop 15 features (mean model):")
    for name, imp in importance[:15]:
        print(f"  {name:<25} {imp}")

    by_class = mae_by_class_year(df, mean_model)
    print("\nMAE by prior class_year (model vs naive):")
    for code, m in sorted(by_class.items()):
        print(f"  class_year_code={code:<4} n={m['n']:<5} model_mae={m['model_mae']:.3f}  naive_mae={m['naive_mae']:.3f}  Δ={m['naive_mae']-m['model_mae']:+.3f}")

    # Regression-to-the-mean diagnostic on OOF predictions. Documents the
    # bias surfaced as Q1 in the ROADMAP — the trajectory model
    # systematically projects elite returners lower than their
    # current-season CamPom (Boozer +30 → +16 type). Negative `bias` on
    # the highest buckets is the smoking gun.
    by_campom = mae_by_current_campom(df, oof_preds)
    print("\nMAE / bias by CURRENT (prior) CamPom bucket — OOF predictions:")
    print(f"  {'bucket':<10} {'n':>5} {'meanCur':>8} {'meanPred':>9} {'meanAct':>8} {'MAE':>6} {'bias':>7} {'naiveMAE':>9}")
    for label, _, _ in CURRENT_CAMPOM_BUCKETS:
        m = by_campom.get(label)
        if m is None:
            continue
        print(
            f"  {label:<10} {m['n']:>5} {m['mean_prior']:>+8.2f} {m['mean_pred']:>+9.2f} "
            f"{m['mean_actual']:>+8.2f} {m['model_mae']:>6.2f} {m['model_bias']:>+7.2f} "
            f"{m['naive_mae']:>9.2f}"
        )

    for name, model in (("trajectory_mean", mean_model), ("trajectory_q10", lo_model), ("trajectory_q90", hi_model)):
        path = OUT_DIR / f"{name}_model.onnx"
        export_to_onnx(model, len(FEATURE_COLS), path)
        print(f"Exported → {path}")

    onoff_coverage = float((df["prior_on_net_rtg"] != ONOFF_MISSING_SENTINEL).mean())
    prior2_coverage = float((df["has_prior2"] == 1.0).mean())

    meta = {
        "model": "trajectory_model",
        "target": "cam_gbpm_v3_psos (season N+1)",
        "join_key": "torvik_pid (cross-season stable)",
        "seasons_trained_on": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(FEATURE_COLS),
        "features": FEATURE_COLS,
        # Same gate string as roster_model so the Rust path can reuse the
        # shared QUAL_FILTER_STRING. Applied to BOTH seasons in the pair
        # at training time; the Rust serve path only needs to honor it on
        # the prior-season row (the one we're projecting forward from).
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        # Quantile model alphas, in the order the Rust loader expects them.
        "quantile_alphas": {"q10": 0.1, "q90": 0.9},
        # On/off features use this sentinel where `player_on_off` has no row
        # (sub-rotation players, 2019) or the swing lacks an OFF sample. The
        # Rust serve path (`trajectory.rs::build_trajectory_features`) fills
        # the same value for NULLs.
        "onoff_missing_sentinel": ONOFF_MISSING_SENTINEL,
        "onoff_coverage": onoff_coverage,
        # Multi-season history block: lag-2 levels fill this sentinel, slope
        # deltas fill 0.0, where the player has no N-1 season. The Rust serve
        # path (`trajectory.rs::build_trajectory_features`) fills identically.
        "lag2_level_sentinel": LAG2_LEVEL_SENTINEL,
        "prior2_coverage": prior2_coverage,
        # Set true once the LOPO held-out predictions land in
        # `trajectory_oof_predictions`. The Rust boot validator gates on
        # this so a stale meta + empty table can't silently regress the
        # historical-year API routes to in-sample serving.
        "oof_persisted": True,
        # Fingerprint of the Layer 0 snapshot this frame was built from
        # (issue #223). This model WRITES `trajectory_oof_predictions`, so a
        # Layer 0 change that goes unretrained here propagates into Layer 2
        # wearing a perfectly valid-looking `oof_provenance` stamp — the #218
        # failure one layer up, and invisible to the boot guard because that
        # guard only compares the two Layer 2 halves against each other.
        # `check_provenance.py` compares this against the live database.
        # Captured next to the frame read, not here — see main().
        "input_provenance": stamp,
        "baseline_naive": naive,
        "backtest_lopo": lopo,
        "cv_5fold": cv,
        "mae_by_prior_class_year": by_class,
        # Per-current-CamPom bucket MAE + bias on OOF predictions —
        # documents the regression-to-the-mean bias on the elite tail.
        # Tooltips on PlayerDetail / PlayerProgression / future Transfer +
        # Projection chips read this to quote MAE for the user's input
        # CamPom range. Negative `model_bias` on the +15+ buckets is the
        # regression signal.
        "mae_by_current_campom": by_campom,
        "top_features": [{"name": n, "importance": int(i)} for n, i in importance[:25]],
        "known_limitations": [
            "Destination-agnostic: cross-team transferring returners are projected against a destination-blind prior. Documented in §5c.",
            "Recruit-rank deferred: ablation experiment runs after historical recruit ingest (class-of-2021–2025 backfill).",
            "Selection bias on returners: only includes players who returned for N+1; doesn't model the leave-for-draft cohort.",
        ],
    }
    meta_path = OUT_DIR / "trajectory_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
