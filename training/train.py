"""
Train game outcome prediction models using LightGBM.

Two models:
  1. Margin model (regression): predicts point spread (home - away)
  2. Win probability model (classification): predicts P(home win)

Evaluation:
  - Margin: MAE, RMSE, R²
  - Win prob: accuracy, log loss, AUC
  - Backtest: chronological split (first 80% of season → predict last 20%)
"""

import json
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
from features import build_feature_matrix

MODEL_DIR = Path(__file__).parent / "models"
MODEL_DIR.mkdir(exist_ok=True)


def train_margin_model(X_train, y_train, X_test, y_test, feature_cols):
    """Train LightGBM regression model for point margin prediction."""
    params = {
        "objective": "regression",
        "metric": "mae",
        "num_leaves": 31,
        "learning_rate": 0.05,
        "feature_fraction": 0.8,
        "bagging_fraction": 0.8,
        "bagging_freq": 5,
        "verbose": -1,
        "n_estimators": 500,
        "early_stopping_rounds": 50,
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


def train_win_model(X_train, y_train, X_test, y_test, feature_cols):
    """Train LightGBM classification model for win probability."""
    params = {
        "objective": "binary",
        "metric": "binary_logloss",
        "num_leaves": 31,
        "learning_rate": 0.05,
        "feature_fraction": 0.8,
        "bagging_fraction": 0.8,
        "bagging_freq": 5,
        "verbose": -1,
        "n_estimators": 500,
        "early_stopping_rounds": 50,
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


def cross_validate(X, y_margin, y_win, feature_cols, n_splits=5):
    """K-fold cross-validation for both models."""
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)

    margin_maes, margin_accs = [], []
    win_accs, win_aucs = [], []

    for fold, (train_idx, test_idx) in enumerate(kf.split(X), 1):
        X_tr, X_te = X.iloc[train_idx], X.iloc[test_idx]

        # Margin model
        m_model = lgb.LGBMRegressor(
            objective="regression", num_leaves=31, learning_rate=0.05,
            n_estimators=300, verbose=-1,
        )
        m_model.fit(X_tr, y_margin.iloc[train_idx])
        m_preds = m_model.predict(X_te)
        margin_maes.append(mean_absolute_error(y_margin.iloc[test_idx], m_preds))
        margin_accs.append(accuracy_score(
            (y_margin.iloc[test_idx] > 0).astype(int),
            (m_preds > 0).astype(int),
        ))

        # Win model
        w_model = lgb.LGBMClassifier(
            objective="binary", num_leaves=31, learning_rate=0.05,
            n_estimators=300, verbose=-1,
        )
        w_model.fit(X_tr, y_win.iloc[train_idx])
        w_probs = w_model.predict_proba(X_te)[:, 1]
        win_accs.append(accuracy_score(y_win.iloc[test_idx], (w_probs >= 0.5).astype(int)))
        win_aucs.append(roc_auc_score(y_win.iloc[test_idx], w_probs))

    print(f"\n{'='*50}")
    print(f"{n_splits}-FOLD CROSS-VALIDATION")
    print(f"{'='*50}")
    print(f"  Margin MAE:  {np.mean(margin_maes):.2f} ± {np.std(margin_maes):.2f}")
    print(f"  Margin Acc:  {np.mean(margin_accs):.3f} ± {np.std(margin_accs):.3f}")
    print(f"  Win Acc:     {np.mean(win_accs):.3f} ± {np.std(win_accs):.3f}")
    print(f"  Win AUC:     {np.mean(win_aucs):.3f} ± {np.std(win_aucs):.3f}")


def chronological_backtest(df, feature_cols):
    """
    Backtest: train on first 80% of season (by date), predict last 20%.
    This simulates real-world usage where we predict future games.
    """
    df_sorted = df.sort_values("game_date").reset_index(drop=True)
    split_idx = int(len(df_sorted) * 0.8)

    train = df_sorted.iloc[:split_idx]
    test = df_sorted.iloc[split_idx:]

    X_train = train[feature_cols]
    X_test = test[feature_cols]

    print(f"\n{'='*50}")
    print("CHRONOLOGICAL BACKTEST (train first 80%, test last 20%)")
    print(f"{'='*50}")
    print(f"  Train: {len(train)} games ({train['game_date'].min()} to {train['game_date'].max()})")
    print(f"  Test:  {len(test)} games ({test['game_date'].min()} to {test['game_date'].max()})")

    margin_model, margin_metrics = train_margin_model(
        X_train, train["margin"], X_test, test["margin"], feature_cols,
    )
    win_model, win_metrics = train_win_model(
        X_train, train["home_win"], X_test, test["home_win"], feature_cols,
    )

    return margin_model, win_model, margin_metrics, win_metrics


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
    print("Loading features...")
    df, feature_cols = build_feature_matrix(engine)

    # Drop rows with missing features
    before = len(df)
    df = df.dropna(subset=feature_cols)
    print(f"Games: {before} total, {len(df)} with complete features")
    print(f"Features: {len(feature_cols)}")
    print(f"Home win rate: {df['home_win'].mean():.3f}")

    X = df[feature_cols]
    y_margin = df["margin"]
    y_win = df["home_win"]

    # 1. Cross-validation
    cross_validate(X, y_margin, y_win, feature_cols)

    # 2. Chronological backtest
    margin_model, win_model, margin_metrics, win_metrics = chronological_backtest(
        df, feature_cols,
    )

    # 3. Feature importance
    print_feature_importance(margin_model, feature_cols)

    # 4. Train final models on all data
    print(f"\n{'='*50}")
    print("TRAINING FINAL MODELS ON ALL DATA")
    print(f"{'='*50}")

    final_margin = lgb.LGBMRegressor(
        objective="regression", num_leaves=31, learning_rate=0.05,
        n_estimators=margin_model.best_iteration_ or 300, verbose=-1,
    )
    final_margin.fit(X, y_margin)

    final_win = lgb.LGBMClassifier(
        objective="binary", num_leaves=31, learning_rate=0.05,
        n_estimators=win_model.best_iteration_ or 300, verbose=-1,
    )
    final_win.fit(X, y_win)

    # Save models
    final_margin.booster_.save_model(str(MODEL_DIR / "margin_model.lgb"))
    final_win.booster_.save_model(str(MODEL_DIR / "win_model.lgb"))

    # Save feature list and metrics
    meta = {
        "season": 2026,
        "n_games": len(df),
        "n_features": len(feature_cols),
        "features": feature_cols,
        "backtest_margin": margin_metrics,
        "backtest_win": win_metrics,
    }
    with open(MODEL_DIR / "model_meta.json", "w") as f:
        json.dump(meta, f, indent=2)

    print(f"\nModels saved to {MODEL_DIR}/")
    print("  margin_model.lgb")
    print("  win_model.lgb")
    print("  model_meta.json")

    return final_margin, final_win


if __name__ == "__main__":
    main()
