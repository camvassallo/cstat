"""
Leave-one-season-out backtest of the game-prediction LightGBM models.

For each holdout year in HOLDOUT_YEARS, trains margin + win models on every
other season and evaluates cold on the holdout. Produces:

  models/loso/predict_{year}/{margin,win}_model.lgb
  models/loso/predict_{year}/oof_predictions.csv
  models/loso/loso_summary.json
  models/loso/loso_summary.csv

Purpose: directly answers the AUC-0.764-is-it-real question from the
overfitting audit in ROADMAP §"CamPom overfitting audit & point-in-time
predict backtest". Different from the existing temporal-split eval because
no in-season info leaks — every prediction comes from a model that never
saw any game in the holdout season.

CAVEAT: still uses full-season CamPom (`cam_gbpm_v3` is a season aggregate
in `torvik_player_stats`), so this measures cross-season generalization but
*not* intra-season lookahead. The point-in-time CamPom step in the roadmap
is the next layer of honesty.
"""

import json
import time
from pathlib import Path

import lightgbm as lgb
import pandas as pd
from sklearn.metrics import (
    accuracy_score,
    brier_score_loss,
    log_loss,
    mean_absolute_error,
    mean_squared_error,
    r2_score,
    roc_auc_score,
)

from db import get_engine
from features import SEASONS, build_feature_matrix, completeness_subset

HOLDOUT_YEARS = [2021, 2022, 2023, 2024, 2025, 2026]
OUTPUT_DIR = Path(__file__).parent / "models" / "loso"
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)


def train_one(holdout_year, df, feature_cols):
    """Train margin + win models holding out one season; evaluate cold on it."""
    train_df = df[df["season"] != holdout_year].copy()
    test_df = df[df["season"] == holdout_year].copy()

    if len(test_df) == 0:
        print(f"  [{holdout_year}] no rows — skipping")
        return None

    # Internal early-stopping val: last 15% of train chronologically.
    train_df = train_df.sort_values("game_date").reset_index(drop=True)
    val_split = int(len(train_df) * 0.85)
    fit_df = train_df.iloc[:val_split]
    val_df = train_df.iloc[val_split:]

    print(
        f"  [{holdout_year}] fit={len(fit_df)} val={len(val_df)} "
        f"test={len(test_df)}  "
        f"(test {test_df['game_date'].min()} → {test_df['game_date'].max()})"
    )

    common_params = dict(
        num_leaves=24,
        learning_rate=0.03,
        feature_fraction=0.7,
        bagging_fraction=0.7,
        bagging_freq=5,
        min_child_samples=30,
        lambda_l1=0.1,
        lambda_l2=1.0,
        verbose=-1,
        n_estimators=1000,
    )

    # Margin model (regression)
    margin = lgb.LGBMRegressor(objective="regression", metric="mae", **common_params)
    margin.fit(
        fit_df[feature_cols], fit_df["margin"],
        eval_set=[(val_df[feature_cols], val_df["margin"])],
        eval_metric="mae",
        callbacks=[lgb.early_stopping(stopping_rounds=80, verbose=False)],
    )
    margin_pred = margin.predict(test_df[feature_cols])

    margin_metrics = {
        "mae": float(mean_absolute_error(test_df["margin"], margin_pred)),
        "rmse": float(mean_squared_error(test_df["margin"], margin_pred) ** 0.5),
        "r2": float(r2_score(test_df["margin"], margin_pred)),
        "win_accuracy_from_margin": float(
            ((margin_pred > 0) == (test_df["margin"] > 0)).mean()
        ),
        "best_iter": int(margin.best_iteration_ or 0),
    }

    # Win model (classification)
    win = lgb.LGBMClassifier(objective="binary", metric="binary_logloss", **common_params)
    win.fit(
        fit_df[feature_cols], fit_df["home_win"],
        eval_set=[(val_df[feature_cols], val_df["home_win"])],
        eval_metric="binary_logloss",
        callbacks=[lgb.early_stopping(stopping_rounds=80, verbose=False)],
    )
    win_prob = win.predict_proba(test_df[feature_cols])[:, 1]
    win_pred = (win_prob >= 0.5).astype(int)

    win_metrics = {
        "accuracy": float(accuracy_score(test_df["home_win"], win_pred)),
        "auc": float(roc_auc_score(test_df["home_win"], win_prob)),
        "log_loss": float(log_loss(test_df["home_win"], win_prob)),
        "brier": float(brier_score_loss(test_df["home_win"], win_prob)),
        "best_iter": int(win.best_iteration_ or 0),
    }

    # Persist artifacts
    out = OUTPUT_DIR / f"predict_{holdout_year}"
    out.mkdir(exist_ok=True)
    margin.booster_.save_model(str(out / "margin_model.lgb"))
    win.booster_.save_model(str(out / "win_model.lgb"))

    oof = pd.DataFrame({
        "game_id": test_df["game_id"].values,
        "season": holdout_year,
        "game_date": test_df["game_date"].values,
        "home_team_id": test_df["home_team_id"].values,
        "away_team_id": test_df["away_team_id"].values,
        "actual_margin": test_df["margin"].values,
        "actual_home_win": test_df["home_win"].values,
        "pred_margin": margin_pred,
        "pred_home_win_prob": win_prob,
    })
    oof.to_csv(out / "oof_predictions.csv", index=False)

    return {
        "holdout_year": holdout_year,
        "n_train": int(len(train_df)),
        "n_test": int(len(test_df)),
        "test_date_min": str(test_df["game_date"].min()),
        "test_date_max": str(test_df["game_date"].max()),
        "margin": margin_metrics,
        "win": win_metrics,
    }


def main():
    print(f"LOSO holdouts: {HOLDOUT_YEARS}")
    print(f"Training pool seasons: {SEASONS}")
    print(f"Output: {OUTPUT_DIR}")
    print()

    engine = get_engine()
    print("Building feature matrix once across all seasons...")
    t0 = time.time()
    df, feature_cols, _sum_cols = build_feature_matrix(engine)
    print(f"  matrix built in {time.time()-t0:.1f}s; {len(df)} rows × {len(feature_cols)} feats")

    # Drop rows missing any of the margin/win features (49 diffs). PBP diff
    # columns are excluded — NaN there is pre-2020 source coverage, not row
    # incompleteness (see features.completeness_subset).
    before = len(df)
    df = df.dropna(subset=completeness_subset(feature_cols)).reset_index(drop=True)
    print(f"  {before} → {len(df)} after dropping rows with missing features")

    summary = []
    for holdout_year in HOLDOUT_YEARS:
        print(f"\n=== holdout {holdout_year} ===")
        t1 = time.time()
        result = train_one(holdout_year, df, feature_cols)
        if result is None:
            continue
        result["wall_time_s"] = round(time.time() - t1, 1)
        summary.append(result)

        m = result["margin"]
        w = result["win"]
        print(
            f"  margin: MAE={m['mae']:.3f}  R²={m['r2']:.3f}  acc_from_margin={m['win_accuracy_from_margin']:.3f}  "
            f"({m['best_iter']} iters)"
        )
        print(
            f"  win:    AUC={w['auc']:.3f}  LogLoss={w['log_loss']:.3f}  Brier={w['brier']:.3f}  "
            f"acc={w['accuracy']:.3f}  ({w['best_iter']} iters)"
        )
        print(f"  wall:   {result['wall_time_s']}s")

    # Write summary
    summary_path_json = OUTPUT_DIR / "loso_summary.json"
    with open(summary_path_json, "w") as f:
        json.dump({"seasons_pool": SEASONS, "holdouts": HOLDOUT_YEARS, "rows": summary}, f, indent=2)

    rows = []
    for r in summary:
        rows.append({
            "holdout": r["holdout_year"],
            "n_train": r["n_train"],
            "n_test": r["n_test"],
            "margin_mae": r["margin"]["mae"],
            "margin_r2": r["margin"]["r2"],
            "win_auc": r["win"]["auc"],
            "win_logloss": r["win"]["log_loss"],
            "win_brier": r["win"]["brier"],
            "win_acc": r["win"]["accuracy"],
            "wall_s": r["wall_time_s"],
        })
    sdf = pd.DataFrame(rows)
    sdf.to_csv(OUTPUT_DIR / "loso_summary.csv", index=False)

    print("\n" + "=" * 60)
    print("LOSO SUMMARY")
    print("=" * 60)
    print(sdf.to_string(index=False))
    print()
    print(f"  AUC pooled (n-weighted): {(sdf['win_auc']*sdf['n_test']).sum()/sdf['n_test'].sum():.4f}")
    print(f"  margin MAE pooled:       {(sdf['margin_mae']*sdf['n_test']).sum()/sdf['n_test'].sum():.3f}")
    print(f"\nArtifacts in {OUTPUT_DIR}/")


if __name__ == "__main__":
    main()
