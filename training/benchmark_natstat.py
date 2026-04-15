"""
Benchmark our model against NatStat's ELO-based win probability.

Joins our backtest predictions with NatStat's game_forecasts (home_win_exp)
on the same test set, then compares accuracy, AUC, log loss, calibration,
and segment-level performance side by side.

Usage:
    python benchmark_natstat.py
"""

import numpy as np
import pandas as pd
from sklearn.metrics import accuracy_score, log_loss, mean_absolute_error, roc_auc_score

from db import get_engine
from evaluate import evaluate_backtest, calibration_analysis
from features import build_feature_matrix


def load_natstat_forecasts(engine) -> pd.DataFrame:
    """Load NatStat win expectations keyed by game_id."""
    return pd.read_sql(
        """
        SELECT game_id, home_win_exp, away_win_exp, spread
        FROM game_forecasts
        WHERE home_win_exp IS NOT NULL
        """,
        engine,
    )


def compare_metrics(test_df: pd.DataFrame) -> dict:
    """Compute side-by-side metrics for our model vs NatStat."""
    actual = test_df["home_win"]

    results = {}
    for label, prob_col in [("cstat", "pred_win_prob"), ("natstat", "ns_win_prob")]:
        probs = test_df[prob_col]
        preds = (probs >= 0.5).astype(int)
        row = {
            "accuracy": float(accuracy_score(actual, preds)),
            "log_loss": float(log_loss(actual, probs)),
            "auc": float(roc_auc_score(actual, probs)),
        }
        # Margin accuracy (NatStat has spread; our model has pred_margin)
        if label == "cstat":
            row["margin_mae"] = float(mean_absolute_error(test_df["margin"], test_df["pred_margin"]))
            margin_winner = (test_df["pred_margin"] > 0).astype(int)
            row["margin_win_acc"] = float(accuracy_score(actual, margin_winner))
        elif "ns_spread" in test_df.columns:
            has_spread = test_df["ns_spread"].notna()
            if has_spread.sum() > 0:
                sub = test_df[has_spread]
                # NatStat spread is from favorite's perspective; convert to home margin
                # spread > 0 means home is favorite by that many points
                ns_margin = sub["ns_spread"]
                row["spread_mae"] = float(mean_absolute_error(sub["margin"], ns_margin))
                row["spread_n"] = int(has_spread.sum())

        results[label] = row

    return results


def segment_comparison(test_df: pd.DataFrame) -> dict:
    """Compare performance across segments."""
    actual = test_df["home_win"]
    segments = {}

    # By predicted confidence
    for label, lo, hi in [("toss-up (<55%)", 0.45, 0.55), ("lean (55-65%)", 0.35, 0.65), ("confident (>65%)", 0.0, 0.35)]:
        mask = (test_df["pred_win_prob"].between(lo, hi)) | (test_df["pred_win_prob"].between(1 - hi, 1 - lo))
        if label == "toss-up (<55%)":
            mask = test_df["pred_win_prob"].between(0.45, 0.55)
        elif label == "lean (55-65%)":
            mask = (test_df["pred_win_prob"].between(0.55, 0.65)) | (test_df["pred_win_prob"].between(0.35, 0.45))
        else:
            mask = (test_df["pred_win_prob"] >= 0.65) | (test_df["pred_win_prob"] <= 0.35)

        sub = test_df[mask]
        if len(sub) < 20:
            continue
        seg = {"n": len(sub)}
        for lbl, col in [("cstat", "pred_win_prob"), ("natstat", "ns_win_prob")]:
            preds = (sub[col] >= 0.5).astype(int)
            seg[f"{lbl}_acc"] = float(accuracy_score(sub["home_win"], preds))
            if len(sub["home_win"].unique()) > 1:
                seg[f"{lbl}_auc"] = float(roc_auc_score(sub["home_win"], sub[col]))
        segments[label] = seg

    # By actual margin
    for label, lo, hi in [("close (<=5)", 0, 5), ("moderate (6-15)", 6, 15), ("blowout (>15)", 16, 100)]:
        mask = test_df["margin"].abs().between(lo, hi)
        sub = test_df[mask]
        if len(sub) < 20:
            continue
        seg = {"n": len(sub)}
        for lbl, col in [("cstat", "pred_win_prob"), ("natstat", "ns_win_prob")]:
            preds = (sub[col] >= 0.5).astype(int)
            seg[f"{lbl}_acc"] = float(accuracy_score(sub["home_win"], preds))
        segments[f"margin_{label}"] = seg

    return segments


def print_benchmark(metrics: dict, cal_cstat, cal_ns, segments: dict, n_games: int):
    """Print side-by-side benchmark report."""
    print(f"\n{'=' * 65}")
    print("BENCHMARK: cstat vs NatStat ELO Win Probability")
    print(f"{'=' * 65}")
    print(f"\n  Test games with both predictions: {n_games}")

    # Core metrics
    c, n = metrics["cstat"], metrics["natstat"]
    print(f"\n{'Metric':<22s}  {'cstat':>10s}  {'NatStat':>10s}  {'Delta':>10s}  {'Winner':>8s}")
    print(f"  {'-' * 62}")

    rows = [
        ("Win Accuracy", c["accuracy"], n["accuracy"], False),
        ("AUC", c["auc"], n["auc"], False),
        ("Log Loss", c["log_loss"], n["log_loss"], True),
    ]
    for label, cv, nv, lower_better in rows:
        delta = cv - nv
        better = (delta < 0) if lower_better else (delta > 0)
        winner = "cstat" if better else "NatStat" if delta != 0 else "tie"
        print(f"  {label:<20s}  {cv:>10.4f}  {nv:>10.4f}  {delta:>+10.4f}  {winner:>8s}")

    if "margin_mae" in c:
        print(f"\n  cstat margin MAE:     {c['margin_mae']:.2f} pts")
        print(f"  cstat margin win acc: {c['margin_win_acc']:.1%}")
    if "spread_mae" in n:
        print(f"  NatStat spread MAE:   {n['spread_mae']:.2f} pts  (n={n['spread_n']})")

    # Calibration comparison
    print(f"\n--- Calibration ---")
    cal_bins_c, ece_c = cal_cstat
    cal_bins_n, ece_n = cal_ns
    print(f"  ECE:  cstat={ece_c:.4f}  NatStat={ece_n:.4f}  {'cstat' if ece_c < ece_n else 'NatStat'} better")

    print(f"\n  {'Bin':>10s}  {'N':>5s}  {'cstat Pred':>10s}  {'NS Pred':>10s}  {'Actual':>8s}")
    # Merge calibration bins by index
    for bc, bn in zip(cal_bins_c, cal_bins_n):
        print(f"  {bc['bin']:>10s}  {bc['count']:>5d}  {bc['mean_predicted']:>10.3f}  {bn['mean_predicted']:>10.3f}  {bc['actual_win_rate']:>8.3f}")

    # Segments
    print(f"\n--- Segment Breakdown ---")
    print(f"  {'Segment':<22s}  {'N':>5s}  {'cstat Acc':>10s}  {'NS Acc':>10s}  {'Winner':>8s}")
    for name, seg in sorted(segments.items()):
        ca = seg.get("cstat_acc", 0)
        na = seg.get("natstat_acc", 0)
        winner = "cstat" if ca > na else "NatStat" if na > ca else "tie"
        print(f"  {name:<22s}  {seg['n']:>5d}  {ca:>10.1%}  {na:>10.1%}  {winner:>8s}")

    # Where does cstat disagree with NatStat and who's right?
    print(f"\n--- Disagreement Analysis ---")

    print(f"\n{'=' * 65}")


def main():
    engine = get_engine()

    print("Building feature matrix...")
    df, feature_cols = build_feature_matrix(engine)
    df = df.dropna(subset=feature_cols)
    print(f"Games with complete features: {len(df)}")

    print("Running backtest...")
    _, _, test_df, _ = evaluate_backtest(df, feature_cols)

    print("Loading NatStat forecasts...")
    forecasts = load_natstat_forecasts(engine)
    print(f"NatStat forecasts loaded: {len(forecasts)}")

    # Join on game_id
    test_df = test_df.merge(forecasts, on="game_id", how="inner")
    test_df["ns_win_prob"] = test_df["home_win_exp"] / 100.0
    test_df["ns_spread"] = test_df["spread"]
    print(f"Test games with both predictions: {len(test_df)}")

    if len(test_df) < 50:
        print("ERROR: Too few overlapping games for meaningful comparison")
        return

    # Core metrics
    metrics = compare_metrics(test_df)

    # Calibration for both
    cal_cstat = calibration_analysis(test_df)

    # For NatStat calibration, swap the prob column temporarily
    ns_test = test_df.copy()
    ns_test["pred_win_prob"] = ns_test["ns_win_prob"]
    cal_ns = calibration_analysis(ns_test)

    # Segments
    segments = segment_comparison(test_df)

    # Disagreement analysis
    test_df["cstat_correct"] = ((test_df["pred_win_prob"] >= 0.5) == test_df["home_win"].astype(bool))
    test_df["ns_correct"] = ((test_df["ns_win_prob"] >= 0.5) == test_df["home_win"].astype(bool))
    test_df["disagree"] = ((test_df["pred_win_prob"] >= 0.5) != (test_df["ns_win_prob"] >= 0.5))

    n_disagree = test_df["disagree"].sum()
    cstat_right = (test_df["disagree"] & test_df["cstat_correct"]).sum()
    ns_right = (test_df["disagree"] & test_df["ns_correct"]).sum()

    print_benchmark(metrics, cal_cstat, cal_ns, segments, len(test_df))

    # Print disagreement at the end
    print(f"  Games where models disagree: {n_disagree} ({n_disagree/len(test_df):.1%})")
    print(f"  When they disagree, cstat correct: {cstat_right} ({cstat_right/n_disagree:.1%})" if n_disagree else "")
    print(f"  When they disagree, NatStat correct: {ns_right} ({ns_right/n_disagree:.1%})" if n_disagree else "")
    print(f"\n{'=' * 65}")


if __name__ == "__main__":
    main()
