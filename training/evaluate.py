"""
Model accuracy tracking and evaluation framework.

Runs a comprehensive evaluation of the trained models and saves timestamped
results to training/eval_history/ for tracking improvement across runs.

Usage:
    python evaluate.py

Evaluation includes:
  - Chronological backtest (train 80% → predict 20%)
  - Calibration analysis (binned predicted probability vs. actual win rate)
  - Margin error distribution (median, p75, p90, p95)
  - Segment breakdowns: home/neutral, conference/non-conference, favorites/underdogs,
    early-season vs. late-season, blowout vs. close games
  - Feature importance ranking
  - Comparison with previous evaluation runs
"""

import json
from datetime import datetime
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

from db import get_engine
from features import SEASONS, build_feature_matrix

MODEL_DIR = Path(__file__).parent / "models"
EVAL_DIR = Path(__file__).parent / "eval_history"


# ---------------------------------------------------------------------------
# Core evaluation
# ---------------------------------------------------------------------------

def evaluate_backtest(df, feature_cols):
    """
    Chronological backtest: train on first 80%, evaluate on last 20%.
    Returns (margin_model, win_model, test_df_with_predictions, metrics_dict).
    """
    df_sorted = df.sort_values("game_date").reset_index(drop=True)
    split_idx = int(len(df_sorted) * 0.8)

    train = df_sorted.iloc[:split_idx]
    test = df_sorted.iloc[split_idx:].copy()

    X_train, X_test = train[feature_cols], test[feature_cols]

    # Margin model
    margin_model = lgb.LGBMRegressor(
        objective="regression", num_leaves=24, learning_rate=0.03,
        feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
        min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
        n_estimators=1000, early_stopping_rounds=80, verbose=-1,
    )
    margin_model.fit(
        X_train, train["margin"],
        eval_set=[(X_test, test["margin"])],
        eval_metric="mae",
    )
    test["pred_margin"] = margin_model.predict(X_test)

    # Win model
    win_model = lgb.LGBMClassifier(
        objective="binary", num_leaves=24, learning_rate=0.03,
        feature_fraction=0.7, bagging_fraction=0.7, bagging_freq=5,
        min_child_samples=30, lambda_l1=0.1, lambda_l2=1.0,
        n_estimators=1000, early_stopping_rounds=80, verbose=-1,
    )
    win_model.fit(
        X_train, train["home_win"],
        eval_set=[(X_test, test["home_win"])],
        eval_metric="binary_logloss",
    )
    test["pred_win_prob"] = win_model.predict_proba(X_test)[:, 1]
    test["pred_winner"] = (test["pred_win_prob"] >= 0.5).astype(int)

    # Overall metrics
    metrics = compute_metrics(test)
    metrics["train_games"] = len(train)
    metrics["test_games"] = len(test)
    metrics["train_date_range"] = [
        str(train["game_date"].min()), str(train["game_date"].max()),
    ]
    metrics["test_date_range"] = [
        str(test["game_date"].min()), str(test["game_date"].max()),
    ]
    metrics["margin_best_iter"] = margin_model.best_iteration_
    metrics["win_best_iter"] = win_model.best_iteration_

    return margin_model, win_model, test, metrics


def compute_metrics(df):
    """Compute all core metrics from a df with pred_margin, pred_win_prob, margin, home_win."""
    margin_errors = (df["pred_margin"] - df["margin"]).abs()
    pred_winner = (df["pred_margin"] > 0).astype(int)

    return {
        "margin_mae": mean_absolute_error(df["margin"], df["pred_margin"]),
        "margin_rmse": float(np.sqrt(mean_squared_error(df["margin"], df["pred_margin"]))),
        "margin_r2": r2_score(df["margin"], df["pred_margin"]),
        "margin_median_ae": float(margin_errors.median()),
        "margin_p75_ae": float(margin_errors.quantile(0.75)),
        "margin_p90_ae": float(margin_errors.quantile(0.90)),
        "margin_p95_ae": float(margin_errors.quantile(0.95)),
        "margin_win_accuracy": float(accuracy_score(df["home_win"], pred_winner)),
        "win_accuracy": float(accuracy_score(df["home_win"], df["pred_winner"])),
        "win_log_loss": float(log_loss(df["home_win"], df["pred_win_prob"])),
        "win_auc": float(roc_auc_score(df["home_win"], df["pred_win_prob"])),
    }


# ---------------------------------------------------------------------------
# Calibration analysis
# ---------------------------------------------------------------------------

def calibration_analysis(df, n_bins=10):
    """
    Bin predictions by predicted win probability, compare to actual win rate.
    Returns list of dicts with bin info and a calibration error score.
    """
    df = df.copy()
    df["prob_bin"] = pd.cut(df["pred_win_prob"], bins=n_bins, labels=False)

    bins = []
    for b in range(n_bins):
        subset = df[df["prob_bin"] == b]
        if len(subset) == 0:
            continue
        low = b / n_bins
        high = (b + 1) / n_bins
        bins.append({
            "bin": f"{low:.1f}-{high:.1f}",
            "count": len(subset),
            "mean_predicted": round(float(subset["pred_win_prob"].mean()), 4),
            "actual_win_rate": round(float(subset["home_win"].mean()), 4),
            "abs_error": round(abs(float(subset["pred_win_prob"].mean()) - float(subset["home_win"].mean())), 4),
        })

    # Expected Calibration Error (ECE)
    total = len(df)
    ece = sum(b["abs_error"] * b["count"] / total for b in bins) if total > 0 else 0.0

    return bins, round(ece, 4)


# ---------------------------------------------------------------------------
# Segment breakdowns
# ---------------------------------------------------------------------------

def segment_analysis(df):
    """Break down model performance by meaningful game segments."""
    segments = {}

    # Home vs neutral venue
    if "venue" in df.columns:
        for label, mask in [("home", df["venue"] == 1), ("neutral", df["venue"] == 0)]:
            sub = df[mask]
            if len(sub) >= 20:
                segments[f"venue_{label}"] = _segment_stats(sub)

    # Conference vs non-conference
    if "is_conference_game" in df.columns:
        for label, val in [("conference", 1), ("non_conference", 0)]:
            sub = df[df["is_conference_game"] == val]
            if len(sub) >= 20:
                segments[f"matchup_{label}"] = _segment_stats(sub)

    # Favorites vs underdogs (by predicted margin)
    big_fav = df[df["pred_margin"].abs() >= 10]
    close_pick = df[df["pred_margin"].abs() < 5]
    mid_fav = df[(df["pred_margin"].abs() >= 5) & (df["pred_margin"].abs() < 10)]
    for label, sub in [("big_favorite_10pt", big_fav), ("lean_under5pt", close_pick), ("moderate_5_10pt", mid_fav)]:
        if len(sub) >= 20:
            segments[f"spread_{label}"] = _segment_stats(sub)

    # Actual game closeness
    blowout = df[df["margin"].abs() >= 15]
    close = df[df["margin"].abs() <= 5]
    for label, sub in [("blowout_15pt", blowout), ("close_5pt", close)]:
        if len(sub) >= 20:
            segments[f"actual_{label}"] = _segment_stats(sub)

    # Early vs late season (by game date quartile within test set)
    dates = df["game_date"]
    mid = dates.quantile(0.5)
    for label, mask in [("first_half", dates <= mid), ("second_half", dates > mid)]:
        sub = df[mask]
        if len(sub) >= 20:
            segments[f"season_{label}"] = _segment_stats(sub)

    return segments


def _segment_stats(sub):
    """Compute summary stats for a segment."""
    pred_winner = (sub["pred_margin"] > 0).astype(int)
    stats = {
        "count": len(sub),
        "margin_mae": round(float(mean_absolute_error(sub["margin"], sub["pred_margin"])), 2),
        "win_accuracy": round(float(accuracy_score(sub["home_win"], sub["pred_winner"])), 3),
        "margin_win_accuracy": round(float(accuracy_score(sub["home_win"], pred_winner)), 3),
    }
    if len(sub["home_win"].unique()) > 1:
        stats["win_auc"] = round(float(roc_auc_score(sub["home_win"], sub["pred_win_prob"])), 3)
    return stats


# ---------------------------------------------------------------------------
# Feature importance
# ---------------------------------------------------------------------------

def feature_importance_table(model, feature_cols, top_n=20):
    """Return ranked feature importance as list of dicts."""
    importance = model.feature_importances_
    ranked = sorted(zip(feature_cols, importance.tolist()), key=lambda x: -x[1])
    return [{"feature": name, "importance": imp} for name, imp in ranked[:top_n]]


# ---------------------------------------------------------------------------
# History comparison
# ---------------------------------------------------------------------------

def load_previous_eval():
    """Load the most recent previous evaluation for comparison."""
    if not EVAL_DIR.exists():
        return None
    evals = sorted(EVAL_DIR.glob("eval_*.json"))
    if not evals:
        return None
    with open(evals[-1]) as f:
        return json.load(f)


def format_comparison(current, previous):
    """Format a comparison between current and previous eval metrics."""
    if previous is None:
        return None

    prev_m = previous["metrics"]
    lines = []

    comparisons = [
        ("margin_mae", "MAE", "pts", True),  # lower is better
        ("margin_rmse", "RMSE", "pts", True),
        ("win_accuracy", "Win Accuracy", "", False),  # higher is better
        ("win_auc", "AUC", "", False),
        ("win_log_loss", "Log Loss", "", True),
    ]

    for key, label, unit, lower_better in comparisons:
        curr_val = current[key]
        prev_val = prev_m.get(key)
        if prev_val is None:
            continue
        delta = curr_val - prev_val
        improved = (delta < 0) if lower_better else (delta > 0)
        arrow = "+" if delta > 0 else ""
        marker = " *" if improved else ""
        unit_str = f" {unit}" if unit else ""
        lines.append(f"  {label:18s} {prev_val:.4f} -> {curr_val:.4f}  ({arrow}{delta:.4f}{unit_str}){marker}")

    return lines


# ---------------------------------------------------------------------------
# Report printing
# ---------------------------------------------------------------------------

def print_report(metrics, calibration_bins, cal_ece, segments, feat_imp, comparison_lines):
    """Print the full evaluation report to stdout."""
    print(f"\n{'='*60}")
    print("MODEL EVALUATION REPORT")
    print(f"{'='*60}")

    print(f"\n  Seasons:     {SEASONS}")
    print(f"  Train games: {metrics['train_games']}")
    print(f"  Test games:  {metrics['test_games']}")
    print(f"  Train dates: {metrics['train_date_range'][0]} to {metrics['train_date_range'][1]}")
    print(f"  Test dates:  {metrics['test_date_range'][0]} to {metrics['test_date_range'][1]}")

    # Core metrics
    print(f"\n--- Margin Model (regression) ---")
    print(f"  MAE:        {metrics['margin_mae']:.2f} pts")
    print(f"  RMSE:       {metrics['margin_rmse']:.2f} pts")
    print(f"  R2:         {metrics['margin_r2']:.3f}")
    print(f"  Median AE:  {metrics['margin_median_ae']:.2f} pts")
    print(f"  P75 AE:     {metrics['margin_p75_ae']:.2f} pts")
    print(f"  P90 AE:     {metrics['margin_p90_ae']:.2f} pts")
    print(f"  P95 AE:     {metrics['margin_p95_ae']:.2f} pts")
    print(f"  Win acc:    {metrics['margin_win_accuracy']:.1%}")

    print(f"\n--- Win Probability Model (classification) ---")
    print(f"  Accuracy:   {metrics['win_accuracy']:.1%}")
    print(f"  Log loss:   {metrics['win_log_loss']:.4f}")
    print(f"  AUC:        {metrics['win_auc']:.3f}")

    # Calibration
    print(f"\n--- Calibration (ECE: {cal_ece:.4f}) ---")
    print(f"  {'Bin':>10s}  {'Count':>6s}  {'Predicted':>10s}  {'Actual':>8s}  {'Error':>7s}")
    for b in calibration_bins:
        print(f"  {b['bin']:>10s}  {b['count']:>6d}  {b['mean_predicted']:>10.3f}  {b['actual_win_rate']:>8.3f}  {b['abs_error']:>7.3f}")

    # Segments
    print(f"\n--- Segment Breakdown ---")
    print(f"  {'Segment':>25s}  {'N':>5s}  {'MAE':>6s}  {'WinAcc':>7s}  {'AUC':>6s}")
    for name, stats in sorted(segments.items()):
        auc_str = f"{stats['win_auc']:.3f}" if "win_auc" in stats else "  n/a"
        print(f"  {name:>25s}  {stats['count']:>5d}  {stats['margin_mae']:>6.2f}  {stats['win_accuracy']:>7.1%}  {auc_str:>6s}")

    # Feature importance
    print(f"\n--- Top Features (margin model) ---")
    max_imp = feat_imp[0]["importance"] if feat_imp else 1
    for f in feat_imp[:15]:
        bar = "#" * int(f["importance"] / max_imp * 25)
        print(f"  {f['feature']:>30s}  {f['importance']:>5d}  {bar}")

    # Comparison
    if comparison_lines:
        print(f"\n--- vs. Previous Run ---")
        for line in comparison_lines:
            print(line)
    else:
        print(f"\n  (no previous evaluation to compare against)")

    print(f"\n{'='*60}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    EVAL_DIR.mkdir(exist_ok=True)
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")

    # Load previous eval for comparison
    previous = load_previous_eval()

    # Build features
    engine = get_engine()
    print("Building feature matrix (point-in-time)...")
    df, feature_cols = build_feature_matrix(engine)

    before = len(df)
    df = df.dropna(subset=feature_cols)
    print(f"Games: {before} total, {len(df)} with complete features")

    # Run backtest
    print("\nRunning chronological backtest...")
    margin_model, win_model, test_df, metrics = evaluate_backtest(df, feature_cols)

    # Calibration
    calibration_bins, cal_ece = calibration_analysis(test_df)
    metrics["calibration_ece"] = cal_ece

    # Segments
    segments = segment_analysis(test_df)

    # Feature importance
    feat_imp = feature_importance_table(margin_model, feature_cols)

    # Comparison
    comparison_lines = format_comparison(metrics, previous)

    # Print report
    print_report(metrics, calibration_bins, cal_ece, segments, feat_imp, comparison_lines)

    # Save evaluation
    eval_result = {
        "timestamp": timestamp,
        "seasons": SEASONS,
        "n_features": len(feature_cols),
        "features": feature_cols,
        "metrics": metrics,
        "calibration": {"bins": calibration_bins, "ece": cal_ece},
        "segments": segments,
        "feature_importance": feat_imp,
    }

    eval_path = EVAL_DIR / f"eval_{timestamp}.json"
    with open(eval_path, "w") as f:
        json.dump(eval_result, f, indent=2, default=str)

    print(f"\nEvaluation saved to {eval_path}")


if __name__ == "__main__":
    main()
