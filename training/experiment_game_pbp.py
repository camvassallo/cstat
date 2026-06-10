"""
Tier-1 PBP features → margin/win/total game models: accept/reject experiment.

Head-to-head on a shared holdout (chronological last 20% of 2026, the
compare_train_windows.py convention): the production 49-diff feature set vs
the same set + 7 leak-free cumulative team PBP tag-rate diffs
(features.py::compute_cumulative_team_pbp, PBP_FEATURES=1).

The feature matrix is built ONCE with PBP on; the baseline is the same
matrix restricted to the production columns — so the only variable is the
feature list. The completeness dropna uses BASELINE columns only (PBP rates
are NaN for pre-2020 seasons by coverage; dropping those rows would nuke
7 seasons from the corpus — LightGBM routes NaN natively instead).

Comparison script only; production train.py and the Rust NUM_FEATURES
contract are untouched. Acceptance bar per ROADMAP: ≥0.5 MAE would be a
slam-dunk accept; anything not clearly positive on the holdout is a reject
given the Rust features.rs + pit-twin + SHAP-baseline blast radius.
"""

import os

os.environ["PBP_FEATURES"] = "1"

import numpy as np
import lightgbm as lgb
from sklearn.metrics import (
    accuracy_score, log_loss, mean_absolute_error, mean_squared_error,
    roc_auc_score, r2_score,
)

import features as features_mod
from db import get_engine

ALL_SEASONS = [2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024, 2025, 2026]
features_mod.SEASONS = ALL_SEASONS

PBP_DIFFS = [
    "diff_pbp_paint_rate", "diff_pbp_paint_fg_pct", "diff_pbp_perimeter_fg_pct",
    "diff_pbp_transition_rate", "diff_pbp_second_chance_rate",
    "diff_pbp_off_to_rate", "diff_pbp_fouls_drawn_per100",
]


def _common_params(objective: str) -> dict:
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
    print(f"\n{'=' * 64}\nFeature set: {name}  ({len(feature_cols)} diff / {len(total_feature_cols)} total)\n{'=' * 64}")

    X_tr, X_te = train_df[feature_cols], test_df[feature_cols]
    X_tr_t, X_te_t = train_df[total_feature_cols], test_df[total_feature_cols]

    p = _common_params("regression"); p["metric"] = "mae"
    m = lgb.LGBMRegressor(**p)
    m.fit(X_tr, train_df["margin"], eval_set=[(X_te, test_df["margin"])], eval_metric="mae")
    m_pred = m.predict(X_te)
    margin = {
        "mae": float(mean_absolute_error(test_df["margin"], m_pred)),
        "rmse": float(np.sqrt(mean_squared_error(test_df["margin"], m_pred))),
        "win_acc": float(accuracy_score((test_df["margin"] > 0).astype(int), (m_pred > 0).astype(int))),
        "best_iter": m.best_iteration_,
    }

    p = _common_params("binary"); p["metric"] = "binary_logloss"
    w = lgb.LGBMClassifier(**p)
    w.fit(X_tr, train_df["home_win"], eval_set=[(X_te, test_df["home_win"])], eval_metric="binary_logloss")
    w_prob = w.predict_proba(X_te)[:, 1]
    win = {
        "accuracy": float(accuracy_score(test_df["home_win"], (w_prob >= 0.5).astype(int))),
        "log_loss": float(log_loss(test_df["home_win"], w_prob)),
        "auc": float(roc_auc_score(test_df["home_win"], w_prob)),
        "best_iter": w.best_iteration_,
    }

    p = _common_params("regression"); p["metric"] = "mae"
    t = lgb.LGBMRegressor(**p)
    t.fit(X_tr_t, train_df["total"], eval_set=[(X_te_t, test_df["total"])], eval_metric="mae")
    t_pred = t.predict(X_te_t)
    total = {
        "mae": float(mean_absolute_error(test_df["total"], t_pred)),
        "r2": float(r2_score(test_df["total"], t_pred)),
        "best_iter": t.best_iteration_,
    }

    print(f"  Margin:  MAE {margin['mae']:.3f}  win_acc {margin['win_acc']:.3f}  best_iter {margin['best_iter']}")
    print(f"  Win:     acc {win['accuracy']:.3f}  AUC {win['auc']:.3f}  logloss {win['log_loss']:.4f}")
    print(f"  Total:   MAE {total['mae']:.3f}  R² {total['r2']:.3f}")

    # PBP feature importances when present (margin model)
    imp = sorted(zip(feature_cols, m.feature_importances_), key=lambda x: -x[1])
    pbp_imp = [(n, i) for n, i in imp if n.startswith("diff_pbp_")]
    if pbp_imp:
        ranks = {n: r for r, (n, _) in enumerate(imp, 1)}
        print("  PBP importances (margin model):")
        for n, i in pbp_imp:
            print(f"    {n:<32} imp={i:<5} rank {ranks[n]}/{len(feature_cols)}")

    return {"margin": margin, "win": win, "total": total}


def main():
    engine = get_engine()
    print("Building 12-season feature matrix WITH PBP features (heavy step)...")
    df, feature_cols, sum_cols = features_mod.build_feature_matrix(engine, seasons=ALL_SEASONS)

    base_cols = [c for c in feature_cols if c not in PBP_DIFFS]
    pbp_cols = base_cols + PBP_DIFFS
    base_total = base_cols + sum_cols
    pbp_total = pbp_cols + sum_cols
    assert len(pbp_cols) - len(base_cols) == 7, feature_cols

    # Completeness filter over BASELINE columns only — PBP NaN is coverage,
    # not row incompleteness.
    before = len(df)
    df = df.dropna(subset=base_total).reset_index(drop=True)
    print(f"\nGames with complete baseline features: {len(df)} / {before}")
    cov = df[PBP_DIFFS[0]].notna().groupby(df["season"]).mean()
    print("PBP diff coverage by season (of kept rows):")
    print((cov * 100).round(1).to_string())

    s2026 = df[df["season"] == 2026].sort_values("game_date").reset_index(drop=True)
    cutoff = int(len(s2026) * 0.8)
    holdout_ids = set(s2026.iloc[cutoff:]["game_id"])
    test_df = df[df["game_id"].isin(holdout_ids)].copy()
    train_df = df[~df["game_id"].isin(holdout_ids)].copy()
    print(f"\nHoldout: 2026 games on/after {s2026.iloc[cutoff]['game_date']} → {len(test_df)}; train {len(train_df)}")

    base = fit_and_eval("baseline (production 49)", train_df, test_df, base_cols, base_total)
    pbp = fit_and_eval("baseline + 7 PBP diffs", train_df, test_df, pbp_cols, pbp_total)

    print(f"\n{'=' * 64}\nSHARED-HOLDOUT COMPARISON (n_test={len(test_df)})\n{'=' * 64}")
    print(f"{'metric':<20} {'baseline':>10} {'+PBP':>10} {'Δ':>9}")
    rows = [
        ("margin.mae", base["margin"]["mae"], pbp["margin"]["mae"], True),
        ("margin.win_acc", base["margin"]["win_acc"], pbp["margin"]["win_acc"], False),
        ("win.accuracy", base["win"]["accuracy"], pbp["win"]["accuracy"], False),
        ("win.auc", base["win"]["auc"], pbp["win"]["auc"], False),
        ("win.log_loss", base["win"]["log_loss"], pbp["win"]["log_loss"], True),
        ("total.mae", base["total"]["mae"], pbp["total"]["mae"], True),
        ("total.r2", base["total"]["r2"], pbp["total"]["r2"], False),
    ]
    for label, a, b, lower_better in rows:
        d = b - a
        better = (d < 0) if lower_better else (d > 0)
        mark = "  ✓" if better else ("  ✗" if d != 0 else "")
        print(f"{label:<20} {a:>10.4f} {b:>10.4f} {d:>+9.4f}{mark}")


if __name__ == "__main__":
    main()
