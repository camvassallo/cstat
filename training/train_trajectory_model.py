"""
Phase 5c growth model: project a returning player's next-season CamPom v3.

One row per (torvik_pid, season_N, season_N+1) pair. Trained on every
consecutive-season player in DB (currently 2024→2025 and 2025→2026,
~4,400 rows after the qualification gate).

Target: next-season `torvik_player_stats.cam_gbpm_v3_psos`.

Features come from the prior season only — rate stats + impact metrics
(CamPom + GBPM components) + archetype mixture (primary 1.0× / secondary
0.5×) + volume + class_year + height. Recruit-rank as a feature is
**deferred to a follow-up ablation** — for every row in today's training
set the player's recruiting class is 2021–2024, and cstat only has the
class-of-2026 recruits ingested, so `composite_rank` would be NULL on
100% of training rows. The ablation lands once historical recruit ingest
covers 2021/2022/2023/2024/2025 classes.

Cross-season pairing is via `torvik_pid` (per memory: stable cross-season
key; `natstat_id` breaks on transfers — different code per team).
Transfers ARE included; the model is destination-agnostic in v1
(documented limitation).

Three LightGBMs trained per run: mean + q=0.1 + q=0.9. Three ONNX files
shipped so the Rust inference path can return (predicted, lower, upper)
as a single floor/ceiling band on PlayerDetail.

Honest framing: 2 paired classes is thin. The per-class-year bucket is
even thinner (a few hundred Fr→So pairs, even fewer Jr→Sr). Document MAE
per bucket in the meta JSON; surface the headline MAE in the UI so users
understand the projection is directional, not a point estimate.
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

OUT_DIR = Path(__file__).parent / "models"
SEASONS = (2024, 2025, 2026)
ARCHETYPES = (
    "Wizard", "Sorcerer", "Warlock", "Bard", "Ranger", "Barbarian",
    "Paladin", "Monk", "Cleric", "Druid", "Rogue", "Fighter",
)

# Pull the prior-season feature row joined to the next-season target.
# Cross-season join via torvik_pid (stable across team changes).
# Qualification gate: ≥5 GP / ≥5 MPG in BOTH seasons — matches the
# roster_model gate so the Rust inference path can share the QUAL_FILTER
# string and we don't need a second gate for trajectory inputs.
PAIRED_QUERY = """
WITH base AS (
    SELECT
        a.torvik_pid,
        a.season AS s_n,
        a.player_id AS pid_n,
        b.season AS s_np1,
        b.player_id AS pid_np1,
        b.cam_gbpm_v3_psos AS target_campom
    FROM torvik_player_stats a
    JOIN torvik_player_stats b
        ON a.torvik_pid = b.torvik_pid
        AND b.season = a.season + 1
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
    -- Archetype mixture (primary + secondary)
    paN.primary_class AS prior_primary_class,
    paN.secondary_class AS prior_secondary_class
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
WHERE pssN.minutes_per_game >= 5
  AND pssN.games_played >= 5
  AND pssNP1.minutes_per_game >= 5
  AND pssNP1.games_played >= 5
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
]
ARCH_FEATURE_COLS = [f"arch_{a.lower()}" for a in ARCHETYPES]
FEATURE_COLS = NUMERIC_FEATURE_COLS + ARCH_FEATURE_COLS  # 25 + 12 = 37


def encode_class_year(s: Optional[str]) -> int:
    if s is None:
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
    df = pd.read_sql(PAIRED_QUERY, engine, params={"seasons": list(SEASONS)})
    print(f"Loaded {len(df):,} paired (season_N → season_N+1) rows.")

    df["prior_class_year_code"] = df["prior_class_year"].map(encode_class_year)
    df = add_archetype_columns(df)

    # Drop any row missing the headline impact feature — CamPom is in the
    # WHERE clause already, but ogbpm/dgbpm/gbpm can still be NULL for the
    # rare Torvik row without GBPM components.
    pre_drop = len(df)
    df = df.dropna(subset=["prior_campom", "prior_ogbpm", "prior_dgbpm"])
    if len(df) < pre_drop:
        print(f"  dropped {pre_drop - len(df)} rows missing GBPM components")

    print(f"After gates: {len(df):,} rows.")
    print(f"  by pair: {df.groupby(['s_n', 's_np1']).size().to_dict()}")
    print(f"  by class_year: {df['prior_class_year_code'].value_counts(dropna=False).sort_index().to_dict()}")
    return df


def lgb_params(objective: str = "regression", alpha: Optional[float] = None) -> dict:
    """Shared shape for mean + quantile models. Same conservative knobs as
    the roster model (~4k rows / ~37 features). Quantile fits use the same
    leaves/lr; only the objective differs."""
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
        "n_estimators": 1500,
        "early_stopping_rounds": 80,
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


def leave_one_pair_out(df: pd.DataFrame) -> dict:
    """Honest-backtest analog of the roster model's LOSO. With 2 pairs
    (2024→2025, 2025→2026), train on one pair and predict the other."""
    results = {}
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
        model.fit(X_tr, y_tr, eval_set=[(X_te, y_te)], eval_metric="mae")
        preds = model.predict(X_te)
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
    return {"per_pair": results, "pooled": overall}


def kfold_cv(df: pd.DataFrame, n_splits: int = 5) -> dict:
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)
    X = df[FEATURE_COLS].values
    y = df["target_campom"].values
    maes, rmses, r2s = [], [], []
    for fold, (tr, te) in enumerate(kf.split(X), 1):
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X[tr], y[tr], eval_set=[(X[te], y[te])], eval_metric="mae")
        p = model.predict(X[te])
        maes.append(float(mean_absolute_error(y[te], p)))
        rmses.append(float(np.sqrt(mean_squared_error(y[te], p))))
        r2s.append(float(r2_score(y[te], p)))
        print(f"  fold {fold}: MAE {maes[-1]:.3f}  RMSE {rmses[-1]:.3f}  R² {r2s[-1]:.3f}")
    return {
        "mae": float(np.mean(maes)), "rmse": float(np.mean(rmses)), "r2": float(np.mean(r2s)),
        "per_fold_mae": maes,
    }


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


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    import onnxmltools
    from onnxmltools.convert.common.data_types import FloatTensorType
    initial_types = [("input", FloatTensorType([None, n_features]))]
    onnx_model = onnxmltools.convert_lightgbm(
        model.booster_, initial_types=initial_types, target_opset=15
    )
    onnxmltools.utils.save_model(onnx_model, str(onnx_path))


def fit_final(df: pd.DataFrame, objective: str = "regression", alpha: Optional[float] = None) -> lgb.LGBMRegressor:
    """Fit on ALL paired rows. No held-out set, so no early stopping —
    lock the budget at 400 iters per roster_model precedent."""
    params = lgb_params(objective=objective, alpha=alpha)
    params.pop("early_stopping_rounds", None)
    params["n_estimators"] = 400
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

    print("\n" + "=" * 60)
    print(f"Naive baseline (year N+1 ≈ year N CamPom)")
    print("=" * 60)
    naive = naive_baseline(df)
    print(f"  pooled: MAE {naive['mae']:.3f}  RMSE {naive['rmse']:.3f}  R² {naive['r2']:.3f}")

    print("\n" + "=" * 60)
    print("Leave-one-pair-out backtest")
    print("=" * 60)
    lopo = leave_one_pair_out(df)

    print("\n" + "=" * 60)
    print("5-fold random CV")
    print("=" * 60)
    cv = kfold_cv(df)

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

    for name, model in (("trajectory_mean", mean_model), ("trajectory_q10", lo_model), ("trajectory_q90", hi_model)):
        path = OUT_DIR / f"{name}_model.onnx"
        export_to_onnx(model, len(FEATURE_COLS), path)
        print(f"Exported → {path}")

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
        "baseline_naive": naive,
        "backtest_lopo": lopo,
        "cv_5fold": cv,
        "mae_by_prior_class_year": by_class,
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
