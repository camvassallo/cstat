"""
Head-to-head: 5-season vs 12-season training cohorts on a shared 2026 holdout.

Tests whether expanding the training window from 2022-2026 (current prod) to
2015-2026 actually improves out-of-sample accuracy on the most recent
basketball, or whether 2015-2017 distribution shift (pre-3PT-volume era)
hurts more than the extra data helps.

Setup:
  - Build the full 12-season feature matrix once.
  - Held-out test set: chronological last 20% of 2026 (~1.2k games).
  - Train A: 2022-2026 minus holdout (matches current prod cohort).
  - Train B: 2015-2026 minus holdout (12-season cohort).
  - Evaluate both on the same test set with identical hyperparameters.

This is a comparison script, not a swap. Production models are NOT touched.
ROADMAP §6 "Margin/win ML model retrain" item drives this work.
"""

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.metrics import (
    accuracy_score,
    log_loss,
    mean_absolute_error,
    mean_squared_error,
    roc_auc_score,
    r2_score,
)

import features as features_mod
from db import get_engine

ALL_SEASONS = [2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024, 2025, 2026]
BASELINE_SEASONS = [2022, 2023, 2024, 2025, 2026]
# Make sure module-level SEASONS reflects the wider window for any callers
# that fall back to it (build_feature_matrix passes seasons explicitly).
features_mod.SEASONS = ALL_SEASONS


def _common_params(objective: str) -> dict:
    """Hyperparameters identical to train.py so the only variable is data."""
    return {
        "objective": objective,
        "num_leaves": 24,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 30,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "n_estimators": 1000,
        "early_stopping_rounds": 80,
        "verbose": -1,
        "seed": 42,
        "deterministic": True,
    }


def fit_and_eval(name, train_df, test_df, feature_cols, total_feature_cols):
    print(f"\n{'='*64}")
    print(f"Cohort: {name}  (n_train={len(train_df)})")
    print(f"{'='*64}")

    X_tr = train_df[feature_cols]
    X_tr_total = train_df[total_feature_cols]
    X_te = test_df[feature_cols]
    X_te_total = test_df[total_feature_cols]
    y_tr_margin = train_df["margin"]
    y_tr_win = train_df["home_win"]
    y_tr_total = train_df["total"]
    y_te_margin = test_df["margin"]
    y_te_win = test_df["home_win"]
    y_te_total = test_df["total"]

    # Margin
    p = _common_params("regression")
    p["metric"] = "mae"
    m_model = lgb.LGBMRegressor(**p)
    m_model.fit(X_tr, y_tr_margin, eval_set=[(X_te, y_te_margin)], eval_metric="mae")
    m_pred = m_model.predict(X_te)
    margin = {
        "mae": float(mean_absolute_error(y_te_margin, m_pred)),
        "rmse": float(np.sqrt(mean_squared_error(y_te_margin, m_pred))),
        "r2": float(r2_score(y_te_margin, m_pred)),
        "win_acc_from_margin": float(
            accuracy_score((y_te_margin > 0).astype(int), (m_pred > 0).astype(int))
        ),
        "best_iter": m_model.best_iteration_,
    }

    # Win
    p = _common_params("binary")
    p["metric"] = "binary_logloss"
    w_model = lgb.LGBMClassifier(**p)
    w_model.fit(X_tr, y_tr_win, eval_set=[(X_te, y_te_win)], eval_metric="binary_logloss")
    w_prob = w_model.predict_proba(X_te)[:, 1]
    win = {
        "accuracy": float(accuracy_score(y_te_win, (w_prob >= 0.5).astype(int))),
        "log_loss": float(log_loss(y_te_win, w_prob)),
        "auc": float(roc_auc_score(y_te_win, w_prob)),
        "best_iter": w_model.best_iteration_,
    }

    # Total
    p = _common_params("regression")
    p["metric"] = "mae"
    t_model = lgb.LGBMRegressor(**p)
    t_model.fit(X_tr_total, y_tr_total, eval_set=[(X_te_total, y_te_total)], eval_metric="mae")
    t_pred = t_model.predict(X_te_total)
    total = {
        "mae": float(mean_absolute_error(y_te_total, t_pred)),
        "rmse": float(np.sqrt(mean_squared_error(y_te_total, t_pred))),
        "r2": float(r2_score(y_te_total, t_pred)),
        "best_iter": t_model.best_iteration_,
    }

    print(f"  Margin:  MAE {margin['mae']:.3f}  win_acc {margin['win_acc_from_margin']:.3f}  best_iter {margin['best_iter']}")
    print(f"  Win:     acc {win['accuracy']:.3f}  AUC {win['auc']:.3f}  logloss {win['log_loss']:.4f}  best_iter {win['best_iter']}")
    print(f"  Total:   MAE {total['mae']:.3f}  R² {total['r2']:.3f}  best_iter {total['best_iter']}")

    return {"margin": margin, "win": win, "total": total}


def main():
    engine = get_engine()

    print("Building 12-season feature matrix (this is the heavy step)...")
    df, feature_cols, sum_cols = features_mod.build_feature_matrix(engine, seasons=ALL_SEASONS)
    total_feature_cols = feature_cols + sum_cols

    before = len(df)
    df = df.dropna(subset=total_feature_cols).reset_index(drop=True)
    print(f"\nGames with complete features: {len(df)} / {before}")
    print(f"Features: {len(feature_cols)} diff + {len(sum_cols)} sum = {len(total_feature_cols)}")

    # Per-season row counts after the feature-completeness filter (so we know
    # what each cohort actually has to learn from)
    counts = df.groupby("season").size().sort_index()
    print("\nGames per season (after feature filter):")
    for s, n in counts.items():
        print(f"  {s}: {n}")

    # Holdout = chronological last 20% of 2026
    s2026 = df[df["season"] == 2026].sort_values("game_date").reset_index(drop=True)
    cutoff = int(len(s2026) * 0.8)
    holdout_ids = set(s2026.iloc[cutoff:]["game_id"])
    test_df = df[df["game_id"].isin(holdout_ids)].copy()
    cutoff_date = s2026.iloc[cutoff]["game_date"]
    print(f"\nHoldout: 2026 games on/after {cutoff_date}  → {len(test_df)} games")

    train_5 = df[
        df["season"].isin(BASELINE_SEASONS) & ~df["game_id"].isin(holdout_ids)
    ].copy()
    train_12 = df[~df["game_id"].isin(holdout_ids)].copy()
    print(f"Train 5-season  (2022-2026 minus holdout): {len(train_5)}")
    print(f"Train 12-season (2015-2026 minus holdout): {len(train_12)}")

    results_5 = fit_and_eval("5-season  (2022-2026)", train_5, test_df, feature_cols, total_feature_cols)
    results_12 = fit_and_eval("12-season (2015-2026)", train_12, test_df, feature_cols, total_feature_cols)

    # Side-by-side
    print(f"\n{'='*64}")
    print(f"SHARED-HOLDOUT COMPARISON (n_test={len(test_df)})")
    print(f"{'='*64}")
    print(f"{'metric':<24} {'5-season':>12} {'12-season':>12} {'Δ':>10}")
    print(f"{'-'*60}")

    def row(label, key_path, lower_is_better=True):
        d = results_5
        for k in key_path:
            d = d[k]
        a = d
        d = results_12
        for k in key_path:
            d = d[k]
        b = d
        delta = b - a
        marker = ""
        if lower_is_better:
            marker = "  ✓" if delta < 0 else ("  ✗" if delta > 0 else "")
        else:
            marker = "  ✓" if delta > 0 else ("  ✗" if delta < 0 else "")
        print(f"{label:<24} {a:>12.4f} {b:>12.4f} {delta:>+10.4f}{marker}")

    row("margin.mae",         ["margin", "mae"],                  lower_is_better=True)
    row("margin.rmse",        ["margin", "rmse"],                 lower_is_better=True)
    row("margin.r2",          ["margin", "r2"],                   lower_is_better=False)
    row("margin.win_acc",     ["margin", "win_acc_from_margin"],  lower_is_better=False)
    row("win.accuracy",       ["win", "accuracy"],                lower_is_better=False)
    row("win.auc",            ["win", "auc"],                     lower_is_better=False)
    row("win.log_loss",       ["win", "log_loss"],                lower_is_better=True)
    row("total.mae",          ["total", "mae"],                   lower_is_better=True)
    row("total.r2",           ["total", "r2"],                    lower_is_better=False)

    return results_5, results_12


if __name__ == "__main__":
    main()
