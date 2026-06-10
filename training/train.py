"""
Train game outcome prediction models using LightGBM.

Three models:
  1. Margin model (regression): predicts point spread (home - away)
  2. Win probability model (classification): predicts P(home win)
  3. Total model (regression): predicts home_score + away_score

Margin and win run on the 49 diff_* features. Total adds level-sensitive
sum_* companions (combined tempo, combined adj_off/def, etc.) — diffs
alone cannot distinguish a slow-vs-slow matchup from fast-vs-fast.

Evaluation:
  - Margin/total: MAE, RMSE, R²
  - Win prob: accuracy, log loss, AUC
  - Backtest: chronological split (first 80% of season → predict last 20%)

Side-effect: writes 5-fold out-of-fold predictions to
`models/oof_predictions.csv` — one row per game with leak-free
predictions from a model that didn't see that game. Foundation for the
historical-prediction (`game_forecasts` backfill) PR + immediate
calibration check on the totals model.
"""

import json
import os
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.metrics import (
    accuracy_score,
    log_loss,
    mean_absolute_error,
    mean_squared_error,
    r2_score,
    roc_auc_score,
)
from sklearn.model_selection import KFold

from db import get_engine
from features import GBPM_VARIANT, SEASONS, build_feature_matrix, completeness_subset

# Override target dir per-experiment with MODEL_DIR=...; default is the
# production location. Used together with GBPM_VARIANT (see features.py) to
# keep experimental artifacts side-by-side without clobbering production —
# e.g. `MODEL_DIR=models_experiments/cam_v3 GBPM_VARIANT=cam_v3 python train.py`.
MODEL_DIR = Path(os.environ.get("MODEL_DIR") or (Path(__file__).parent / "models"))
MODEL_DIR.mkdir(parents=True, exist_ok=True)


def train_margin_model(X_train, y_train, X_test, y_test, feature_cols):
    """Train LightGBM regression model for point margin prediction."""
    params = {
        "objective": "regression",
        "metric": "mae",
        "num_leaves": 24,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 30,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 1000,
        "early_stopping_rounds": 80,
    }

    model = lgb.LGBMRegressor(**params)
    model.fit(
        X_train,
        y_train,
        eval_set=[(X_test, y_test)],
        eval_metric="mae",
    )

    preds = model.predict(X_test)
    mae = mean_absolute_error(y_test, preds)
    rmse = np.sqrt(mean_squared_error(y_test, preds))
    r2 = r2_score(y_test, preds)

    # Also check classification accuracy from margin sign
    pred_winner = (preds > 0).astype(int)
    actual_winner = (y_test > 0).astype(int)
    accuracy = accuracy_score(actual_winner, pred_winner)

    print(f"\n{'='*50}")
    print("MARGIN MODEL (regression)")
    print(f"{'='*50}")
    print(f"  MAE:       {mae:.2f} points")
    print(f"  RMSE:      {rmse:.2f} points")
    print(f"  R²:        {r2:.3f}")
    print(f"  Win acc:   {accuracy:.3f} ({accuracy*100:.1f}%)")
    print(f"  Best iter: {model.best_iteration_}")

    return model, {"mae": mae, "rmse": rmse, "r2": r2, "accuracy": accuracy}


def train_total_model(X_train, y_train, X_test, y_test, feature_cols):
    """Train LightGBM regression model for game total (home_score + away_score)."""
    params = {
        "objective": "regression",
        "metric": "mae",
        "num_leaves": 24,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 30,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 1000,
        "early_stopping_rounds": 80,
    }

    model = lgb.LGBMRegressor(**params)
    model.fit(
        X_train,
        y_train,
        eval_set=[(X_test, y_test)],
        eval_metric="mae",
    )

    preds = model.predict(X_test)
    mae = mean_absolute_error(y_test, preds)
    rmse = np.sqrt(mean_squared_error(y_test, preds))
    r2 = r2_score(y_test, preds)

    print(f"\n{'='*50}")
    print("TOTAL MODEL (regression)")
    print(f"{'='*50}")
    print(f"  MAE:       {mae:.2f} points")
    print(f"  RMSE:      {rmse:.2f} points")
    print(f"  R²:        {r2:.3f}")
    print(f"  Best iter: {model.best_iteration_}")

    return model, {"mae": mae, "rmse": rmse, "r2": r2}


def train_win_model(X_train, y_train, X_test, y_test, feature_cols):
    """Train LightGBM classification model for win probability."""
    params = {
        "objective": "binary",
        "metric": "binary_logloss",
        "num_leaves": 24,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 30,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 1000,
        "early_stopping_rounds": 80,
    }

    model = lgb.LGBMClassifier(**params)
    model.fit(
        X_train,
        y_train,
        eval_set=[(X_test, y_test)],
        eval_metric="binary_logloss",
    )

    probs = model.predict_proba(X_test)[:, 1]
    preds = (probs >= 0.5).astype(int)
    accuracy = accuracy_score(y_test, preds)
    logloss = log_loss(y_test, probs)
    auc = roc_auc_score(y_test, probs)

    print(f"\n{'='*50}")
    print("WIN PROBABILITY MODEL (classification)")
    print(f"{'='*50}")
    print(f"  Accuracy:  {accuracy:.3f} ({accuracy*100:.1f}%)")
    print(f"  Log loss:  {logloss:.4f}")
    print(f"  AUC:       {auc:.3f}")
    print(f"  Best iter: {model.best_iteration_}")

    return model, {"accuracy": accuracy, "log_loss": logloss, "auc": auc}


def cross_validate(df, feature_cols, total_feature_cols, n_splits=5):
    """K-fold cross-validation for all three models.

    Returns the OOF prediction DataFrame so train.py can persist it
    alongside the trained models. KFold with shuffle covers every row
    exactly once across folds, so this is a leak-free per-game record:
    each prediction comes from a model that didn't see that game. Used
    for (a) immediate calibration check on the totals model and (b) a
    head start on the historical-prediction (`game_forecasts` backfill)
    PR.
    """
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)

    X_margin = df[feature_cols]
    X_total = df[total_feature_cols]
    y_margin = df["margin"]
    y_win = df["home_win"]
    y_total = df["total"]

    margin_maes, margin_accs = [], []
    win_accs, win_aucs = [], []
    total_maes = []

    # Pre-allocate OOF prediction columns; KFold writes each row exactly
    # once across folds.
    oof_margin = np.full(len(df), np.nan)
    oof_total = np.full(len(df), np.nan)
    oof_win_prob = np.full(len(df), np.nan)

    for fold, (train_idx, test_idx) in enumerate(kf.split(df), 1):
        # Margin model (49 diff features)
        m_model = lgb.LGBMRegressor(
            objective="regression", num_leaves=24, learning_rate=0.03,
            feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
            min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
            n_estimators=500, verbose=-1,
        )
        m_model.fit(X_margin.iloc[train_idx], y_margin.iloc[train_idx])
        m_preds = m_model.predict(X_margin.iloc[test_idx])
        oof_margin[test_idx] = m_preds
        margin_maes.append(mean_absolute_error(y_margin.iloc[test_idx], m_preds))
        margin_accs.append(accuracy_score(
            (y_margin.iloc[test_idx] > 0).astype(int),
            (m_preds > 0).astype(int),
        ))

        # Win model (49 diff features)
        w_model = lgb.LGBMClassifier(
            objective="binary", num_leaves=24, learning_rate=0.03,
            feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
            min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
            n_estimators=500, verbose=-1,
        )
        w_model.fit(X_margin.iloc[train_idx], y_win.iloc[train_idx])
        w_probs = w_model.predict_proba(X_margin.iloc[test_idx])[:, 1]
        oof_win_prob[test_idx] = w_probs
        win_accs.append(accuracy_score(y_win.iloc[test_idx], (w_probs >= 0.5).astype(int)))
        win_aucs.append(roc_auc_score(y_win.iloc[test_idx], w_probs))

        # Total model (49 diff + sum features)
        t_model = lgb.LGBMRegressor(
            objective="regression", num_leaves=24, learning_rate=0.03,
            feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
            min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
            n_estimators=500, verbose=-1,
        )
        t_model.fit(X_total.iloc[train_idx], y_total.iloc[train_idx])
        t_preds = t_model.predict(X_total.iloc[test_idx])
        oof_total[test_idx] = t_preds
        total_maes.append(mean_absolute_error(y_total.iloc[test_idx], t_preds))

    print(f"\n{'='*50}")
    print(f"{n_splits}-FOLD CROSS-VALIDATION")
    print(f"{'='*50}")
    print(f"  Margin MAE:  {np.mean(margin_maes):.2f} ± {np.std(margin_maes):.2f}")
    print(f"  Margin Acc:  {np.mean(margin_accs):.3f} ± {np.std(margin_accs):.3f}")
    print(f"  Win Acc:     {np.mean(win_accs):.3f} ± {np.std(win_accs):.3f}")
    print(f"  Win AUC:     {np.mean(win_aucs):.3f} ± {np.std(win_aucs):.3f}")
    print(f"  Total MAE:   {np.mean(total_maes):.2f} ± {np.std(total_maes):.2f}")

    # Derive home/away score predictions from (total, margin) so the
    # OOF dump is the same shape the API will eventually serve.
    oof_home_score = (oof_total + oof_margin) / 2.0
    oof_away_score = (oof_total - oof_margin) / 2.0

    oof = pd.DataFrame({
        "game_id": df["game_id"].values,
        "game_date": df["game_date"].values,
        "season": df["season"].values,
        "home_team_id": df["home_team_id"].values,
        "away_team_id": df["away_team_id"].values,
        "actual_margin": y_margin.values,
        "actual_total": y_total.values,
        "actual_home_win": y_win.values,
        "pred_margin": oof_margin,
        "pred_total": oof_total,
        "pred_home_win_prob": oof_win_prob,
        "pred_home_score": oof_home_score,
        "pred_away_score": oof_away_score,
    })
    return oof


def chronological_backtest(df, feature_cols, total_feature_cols):
    """
    Backtest: train on first 80% of season (by date), predict last 20%.
    This simulates real-world usage where we predict future games.

    Margin/win use feature_cols (49 diffs); total uses total_feature_cols
    (diffs + level-sensitive sums).
    """
    df_sorted = df.sort_values("game_date").reset_index(drop=True)
    split_idx = int(len(df_sorted) * 0.8)

    train = df_sorted.iloc[:split_idx]
    test = df_sorted.iloc[split_idx:]

    print(f"\n{'='*50}")
    print("CHRONOLOGICAL BACKTEST (train first 80%, test last 20%)")
    print(f"{'='*50}")
    print(f"  Train: {len(train)} games ({train['game_date'].min()} to {train['game_date'].max()})")
    print(f"  Test:  {len(test)} games ({test['game_date'].min()} to {test['game_date'].max()})")

    margin_model, margin_metrics = train_margin_model(
        train[feature_cols], train["margin"],
        test[feature_cols], test["margin"], feature_cols,
    )
    win_model, win_metrics = train_win_model(
        train[feature_cols], train["home_win"],
        test[feature_cols], test["home_win"], feature_cols,
    )
    total_model, total_metrics = train_total_model(
        train[total_feature_cols], train["total"],
        test[total_feature_cols], test["total"], total_feature_cols,
    )

    return (
        margin_model, win_model, total_model,
        margin_metrics, win_metrics, total_metrics,
    )


def print_feature_importance(model, feature_cols, top_n=15):
    """Print top feature importances."""
    importance = model.feature_importances_
    feat_imp = sorted(zip(feature_cols, importance), key=lambda x: -x[1])

    print(f"\nTop {top_n} features:")
    for name, imp in feat_imp[:top_n]:
        bar = "█" * int(imp / max(importance) * 30)
        print(f"  {name:30s} {imp:5d}  {bar}")


def main():
    engine = get_engine()
    print(f"GBPM variant: {GBPM_VARIANT}")
    print(f"Model dir:    {MODEL_DIR}")
    print("Loading features...")
    df, feature_cols, sum_cols = build_feature_matrix(engine)
    total_feature_cols = feature_cols + sum_cols

    # Drop rows with missing features (use the union — totals model needs
    # both, and dropping a row from one model but not the other would
    # split the OOF prediction frame). PBP diff columns are excluded from
    # the completeness check: NaN there is source coverage (no contextual
    # tags pre-2020), not row incompleteness — see features.completeness_subset.
    before = len(df)
    df = df.dropna(subset=completeness_subset(total_feature_cols)).reset_index(drop=True)
    print(f"Games: {before} total, {len(df)} with complete features")
    print(f"Features: {len(feature_cols)} diff (margin/win), +{len(sum_cols)} sum (total only)")
    print(f"Home win rate: {df['home_win'].mean():.3f}")
    print(f"Avg total:     {df['total'].mean():.1f} (σ {df['total'].std():.1f})")

    # 1. Cross-validation (also produces leak-free OOF predictions)
    oof = cross_validate(df, feature_cols, total_feature_cols)
    oof_path = MODEL_DIR / "oof_predictions.csv"
    oof.to_csv(oof_path, index=False)
    print(f"  OOF predictions → {oof_path} ({len(oof)} games)")

    # 2. Chronological backtest
    (
        margin_model, win_model, total_model,
        margin_metrics, win_metrics, total_metrics,
    ) = chronological_backtest(df, feature_cols, total_feature_cols)

    # 3. Feature importance
    print_feature_importance(margin_model, feature_cols)
    print("\nTotal model feature importance:")
    print_feature_importance(total_model, total_feature_cols)

    # 4. Train final models on all data
    print(f"\n{'='*50}")
    print("TRAINING FINAL MODELS ON ALL DATA")
    print(f"{'='*50}")

    X_diff = df[feature_cols]
    X_total = df[total_feature_cols]
    y_margin = df["margin"]
    y_win = df["home_win"]
    y_total = df["total"]

    final_margin = lgb.LGBMRegressor(
        objective="regression", num_leaves=24, learning_rate=0.03,
        feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
        min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
        n_estimators=margin_model.best_iteration_ or 300, verbose=-1,
    )
    final_margin.fit(X_diff, y_margin)

    final_win = lgb.LGBMClassifier(
        objective="binary", num_leaves=24, learning_rate=0.03,
        feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
        min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
        n_estimators=win_model.best_iteration_ or 300, verbose=-1,
    )
    final_win.fit(X_diff, y_win)

    final_total = lgb.LGBMRegressor(
        objective="regression", num_leaves=24, learning_rate=0.03,
        feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
        min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
        n_estimators=total_model.best_iteration_ or 300, verbose=-1,
    )
    final_total.fit(X_total, y_total)

    # Save models
    final_margin.booster_.save_model(str(MODEL_DIR / "margin_model.lgb"))
    final_win.booster_.save_model(str(MODEL_DIR / "win_model.lgb"))
    final_total.booster_.save_model(str(MODEL_DIR / "total_model.lgb"))

    # Save feature list and metrics. `n_features` / `features` are the
    # margin/win contract (49 diffs) — preserved for export_onnx.py and
    # the existing Rust Predictor (NUM_FEATURES=49). `total_*` keys
    # describe the totals model's larger feature set.
    meta = {
        "seasons": SEASONS,
        "gbpm_variant": GBPM_VARIANT,
        "n_games": len(df),
        "n_features": len(feature_cols),
        "features": feature_cols,
        "total_n_features": len(total_feature_cols),
        "total_features": total_feature_cols,
        "backtest_margin": margin_metrics,
        "backtest_win": win_metrics,
        "backtest_total": total_metrics,
    }
    with open(MODEL_DIR / "model_meta.json", "w") as f:
        json.dump(meta, f, indent=2)

    print(f"\nModels saved to {MODEL_DIR}/")
    print("  margin_model.lgb")
    print("  win_model.lgb")
    print("  total_model.lgb")
    print("  model_meta.json")
    print("  oof_predictions.csv")

    return final_margin, final_win, final_total


if __name__ == "__main__":
    main()
