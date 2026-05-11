"""
Train a roster-only model: minutes-weighted player aggregates → team AdjEM.

Used by the transfer-portal Δ projection: holds destination roster, swaps in
the incoming player at their carry-over MPG (rest of roster proportionally
shrunk to fit), and reads the model's ΔAdjEM.

One row per (team_id, season). Trained on 2024 + 2025 + 2026 (~1,090 rows).
Target: team_season_stats.adj_efficiency_margin.

Features — deliberately exclude any team-outcome inputs (adj_offense,
adj_defense, ELO, four factors, W-L). If outcome features are in the input,
the model learns "good team → good rating" and a hypothetical swap leaves
those constant, killing the swap signal. The whole point is to learn
"good roster composition → good team rating" so that flipping the roster
flips the prediction.

Honest framing: this is for *ranking* hypothetical roster swaps. It does not
replace the predict model for game prediction, and absolute AdjEM is
softer-calibrated than the actual computed AdjEM.
"""

import json
from pathlib import Path

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

PLAYER_QUERY = """
SELECT
    pss.team_id, pss.season, pss.player_id,
    COALESCE(pss.minutes_per_game, 0) * COALESCE(pss.games_played, 0) AS total_min,
    pss.minutes_per_game AS mpg,
    pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
    pss.true_shooting_pct AS ts, pss.effective_fg_pct AS efg, pss.usage_rate AS usg,
    pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
    pss.stl_pct, pss.blk_pct, pss.ft_rate,
    tps.gbpm, tps.ogbpm, tps.dgbpm,
    tps.cam_gbpm_v3_psos AS campom,
    pa.primary_class
FROM player_season_stats pss
LEFT JOIN torvik_player_stats tps
    ON tps.player_id = pss.player_id AND tps.season = pss.season
LEFT JOIN player_archetypes pa
    ON pa.player_id = pss.player_id AND pa.season = pss.season
WHERE pss.season = ANY(%(seasons)s)
  AND COALESCE(pss.games_played, 0) >= 5
  AND COALESCE(pss.minutes_per_game, 0) >= 5
"""

TEAM_QUERY = """
SELECT team_id, season, adj_efficiency_margin
FROM team_season_stats
WHERE season = ANY(%(seasons)s)
  AND adj_efficiency_margin IS NOT NULL
"""

# Minutes-weighted means across players. Pure player stats only — no
# team-level outcomes.
#
# Two variants, gated by `INCLUDE_IMPACT_FEATURES`:
#   - True  → include Torvik GBPM / CamPom. These are per-player impact
#             metrics computed such that the minutes-weighted sum ≈ team
#             AdjEM by construction (regression-derived attribution).
#             Training against AdjEM with these features is partly
#             recovering a tautology; MAE drops to ~1.7 but the signal is
#             dominated by the impact-metric identity. Useful as a sanity
#             check on the aggregation, less useful as a transfer Δ engine
#             since `Δ ≈ player_campom × minutes_share` would do similar
#             work without a model.
#   - False → box-score / rate stats only. Model has to genuinely learn
#             how roster *composition* produces team performance. Higher
#             MAE expected (this is just harder), but the swap signal is
#             real (positional overlap, archetype mismatch, rate-stat
#             interactions). This is the variant we ship for transfer Δ.
import os
INCLUDE_IMPACT_FEATURES = os.environ.get("ROSTER_INCLUDE_IMPACT", "0") == "1"

W_COLS_BOX = [
    "ppg", "rpg", "apg", "spg", "bpg", "topg",
    "ts", "efg", "usg",
    "ast_pct", "tov_pct", "orb_pct", "drb_pct", "stl_pct", "blk_pct", "ft_rate",
]
W_COLS_IMPACT = ["gbpm", "ogbpm", "dgbpm", "campom"]
W_COLS = W_COLS_BOX + (W_COLS_IMPACT if INCLUDE_IMPACT_FEATURES else [])


def weighted_mean(values: pd.Series, weights: pd.Series) -> float:
    mask = values.notna() & weights.notna() & (weights > 0)
    if not mask.any():
        return np.nan
    v = values[mask].astype(float)
    w = weights[mask].astype(float)
    s = w.sum()
    return float((v * w).sum() / s) if s > 0 else np.nan


def aggregate_team_season(group: pd.DataFrame) -> pd.Series:
    """Reduce one team-season's player rows to a single feature vector."""
    w = group["total_min"]
    total = float(w.sum())
    row = {
        "roster_size": int(len(group)),
        "total_minutes": total,
    }
    # Depth: top-1 and top-5 minutes share. Low top1 = balanced roster.
    sorted_min = group["total_min"].sort_values(ascending=False).reset_index(drop=True)
    row["top1_min_share"] = float(sorted_min.iloc[0] / total) if total > 0 else np.nan
    row["top5_min_share"] = float(sorted_min.head(5).sum() / total) if total > 0 else np.nan
    row["minutes_stddev"] = float(group["mpg"].std()) if len(group) > 1 else 0.0

    for col in W_COLS:
        row[f"w_{col}"] = weighted_mean(group[col], w)

    if INCLUDE_IMPACT_FEATURES:
        # Star = top player by CamPom (impact metric). Skipped in the
        # box-score-only variant since the star features would smuggle the
        # tautology back in via the top-player identity.
        pool = group.dropna(subset=["campom"])
        if len(pool) == 0:
            pool = group
            star_key = "total_min"
        else:
            star_key = "campom"
        star_idx = pool[star_key].idxmax()
        star = pool.loc[star_idx]
        for col in ("gbpm", "ogbpm", "dgbpm", "campom"):
            row[f"star_{col}"] = float(star[col]) if pd.notna(star[col]) else np.nan
    else:
        # Box-score-only star: top by minutes. The signal is "what's the
        # team's headline producer's box profile" — not an impact metric.
        star_idx = group["total_min"].idxmax()
        star = group.loc[star_idx]
        for col in ("ppg", "ts", "usg"):
            row[f"star_{col}"] = float(star[col]) if pd.notna(star[col]) else np.nan

    # Archetype minutes share (12 columns).
    arch_minutes = group.groupby("primary_class")["total_min"].sum()
    for arch in ARCHETYPES:
        share = float(arch_minutes.get(arch, 0.0)) / total if total > 0 else 0.0
        row[f"arch_{arch.lower()}"] = share

    return pd.Series(row)


def build_dataset() -> tuple[pd.DataFrame, list[str]]:
    engine = get_engine()
    players = pd.read_sql(PLAYER_QUERY, engine, params={"seasons": list(SEASONS)})
    teams = pd.read_sql(TEAM_QUERY, engine, params={"seasons": list(SEASONS)})

    print(f"Loaded {len(players):,} player-season rows, {len(teams):,} team-seasons.")

    agg = (
        players.groupby(["team_id", "season"], as_index=False)
        .apply(aggregate_team_season, include_groups=False)
        .reset_index(drop=True)
    )
    df = agg.merge(teams, on=["team_id", "season"], how="inner")
    print(f"After join: {len(df):,} team-seasons with target.")

    feature_cols = [c for c in df.columns if c not in ("team_id", "season", "adj_efficiency_margin")]
    return df, feature_cols


def lgb_params() -> dict:
    # Conservative for ~1k rows + ~40 features. The full predict model uses
    # similar shape on 12k rows; we shrink leaves a touch and bump min_child
    # to fight the small-N overfitting risk.
    return {
        "objective": "regression",
        "metric": "mae",
        "num_leaves": 16,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 20,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 1500,
        "early_stopping_rounds": 80,
    }


def leave_one_season_out(df: pd.DataFrame, feature_cols: list[str]) -> dict:
    """Honest backtest: predict each season using a model trained on the other two.
    With only 3 seasons, leave-one-season-out is the right cross-val."""
    results = {}
    overall_y, overall_p = [], []
    for season in SEASONS:
        train = df[df["season"] != season]
        test = df[df["season"] == season]
        X_train, y_train = train[feature_cols], train["adj_efficiency_margin"]
        X_test, y_test = test[feature_cols], test["adj_efficiency_margin"]
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X_train, y_train, eval_set=[(X_test, y_test)], eval_metric="mae")
        preds = model.predict(X_test)
        mae = mean_absolute_error(y_test, preds)
        rmse = float(np.sqrt(mean_squared_error(y_test, preds)))
        r2 = r2_score(y_test, preds)
        results[season] = {"mae": mae, "rmse": rmse, "r2": r2, "n": int(len(test))}
        overall_y.extend(y_test.tolist())
        overall_p.extend(preds.tolist())
        print(f"  season {season}: MAE {mae:.2f}  RMSE {rmse:.2f}  R² {r2:.3f}  n={len(test)}")
    overall = {
        "mae": mean_absolute_error(overall_y, overall_p),
        "rmse": float(np.sqrt(mean_squared_error(overall_y, overall_p))),
        "r2": r2_score(overall_y, overall_p),
    }
    print(f"  pooled:      MAE {overall['mae']:.2f}  RMSE {overall['rmse']:.2f}  R² {overall['r2']:.3f}")
    return {"per_season": results, "pooled": overall}


def random_kfold(df: pd.DataFrame, feature_cols: list[str], n_splits: int = 5) -> dict:
    """Random K-fold for a second look. Weaker honesty than LOSO when the
    target has season-level drift, but a useful upper-bound comparison."""
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)
    X = df[feature_cols].values
    y = df["adj_efficiency_margin"].values
    maes, rmses, r2s = [], [], []
    for fold, (tr, te) in enumerate(kf.split(X), 1):
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X[tr], y[tr], eval_set=[(X[te], y[te])], eval_metric="mae")
        p = model.predict(X[te])
        maes.append(mean_absolute_error(y[te], p))
        rmses.append(float(np.sqrt(mean_squared_error(y[te], p))))
        r2s.append(r2_score(y[te], p))
        print(f"  fold {fold}: MAE {maes[-1]:.2f}  RMSE {rmses[-1]:.2f}  R² {r2s[-1]:.3f}")
    out = {"mae": float(np.mean(maes)), "rmse": float(np.mean(rmses)), "r2": float(np.mean(r2s))}
    print(f"  mean:    MAE {out['mae']:.2f}  RMSE {out['rmse']:.2f}  R² {out['r2']:.3f}")
    return out


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    import onnxmltools
    from onnxmltools.convert.common.data_types import FloatTensorType

    initial_types = [("input", FloatTensorType([None, n_features]))]
    onnx_model = onnxmltools.convert_lightgbm(
        model.booster_, initial_types=initial_types, target_opset=15
    )
    onnxmltools.utils.save_model(onnx_model, str(onnx_path))


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    print("=" * 60)
    print("Building dataset...")
    df, feature_cols = build_dataset()
    df = df.dropna(subset=["adj_efficiency_margin"]).reset_index(drop=True)
    print(f"Features: {len(feature_cols)}  | rows: {len(df)}")

    print("\n" + "=" * 60)
    print("Leave-one-season-out backtest")
    print("=" * 60)
    loso = leave_one_season_out(df, feature_cols)

    print("\n" + "=" * 60)
    print("5-fold random CV")
    print("=" * 60)
    cv = random_kfold(df, feature_cols, n_splits=5)

    print("\n" + "=" * 60)
    print("Final fit on all data")
    print("=" * 60)
    X, y = df[feature_cols], df["adj_efficiency_margin"]
    final_params = lgb_params()
    # No early stopping on the final fit — there's no held-out set.
    final_params.pop("early_stopping_rounds", None)
    final_params["n_estimators"] = max(200, int(np.mean([
        v.get("best_iter", 300) for v in loso["per_season"].values() if isinstance(v, dict)
    ])) if False else 400)
    final = lgb.LGBMRegressor(**final_params)
    final.fit(X, y)

    print("\nTop 15 features by importance:")
    importance = sorted(zip(feature_cols, final.feature_importances_), key=lambda x: -x[1])
    for name, imp in importance[:15]:
        print(f"  {name:<25} {imp}")

    # ONNX is the production artifact — Rust inference loads it via ort.
    # No .lgb / .txt sidecar: we don't need TreeSHAP for this model the way
    # margin_model.lgb does, so skip the dual export.
    onnx_path = OUT_DIR / "roster_model.onnx"
    export_to_onnx(final, len(feature_cols), onnx_path)
    print(f"\nExported ONNX → {onnx_path}")

    meta = {
        "model": "roster_model",
        "target": "adj_efficiency_margin",
        "seasons": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(feature_cols),
        "features": feature_cols,
        "backtest_loso": loso,
        "cv_5fold": cv,
        "top_features": [{"name": n, "importance": int(i)} for n, i in importance[:25]],
    }
    meta_path = OUT_DIR / "roster_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
