"""
Train the Phase B impact-aggregation projection model.

Phase B vs. the box-score roster model (`train_roster_model.py`):
the box-score model deliberately EXCLUDES cam_v3 / GBPM because
`Σ(cam_v3 × minute_share) ≈ team AdjEM` by construction — feeding it
those features collapses the model to the player-impact identity and
kills the swap-Δ signal it exists to produce.

Phase B is the opposite use case. We *want* that identity: a roster
projection is exactly `AdjEM ≈ f(Σ projected cam_v3)`. This model is a
clean calibrator from a roster's cam_v3 distribution (plus archetype /
experience structure) to team AdjEM. At serve time the projections
route feeds *projected* cam_v3 (the trajectory model for returners /
arrivals, the freshman model for recruits); all projection error then
lives in those upstream models — honest and decomposable. The
box-score `roster_model.onnx` is untouched and still serves swap-Δ.

v1 (this script) trains on *actual* same-season `cam_gbpm_v3_psos`. The
v2 follow-up named in ROADMAP §5b retrains on held-out OOF cam_v3 so
the model absorbs the trajectory / freshman projection bias directly.

Rotation normalization — train/serve parity. Both this script and the
Rust `roster_impact::build_roster_impact_features` rank each roster by
cam_v3, take the top 13, and assign canonical MPG by rank
(`CANONICAL_ROTATION_MPG`, sourced from
`crates/cstat-core/src/roster_features.rs::CANONICAL_ROTATION_MPG`).
Every minutes-weighted aggregate therefore uses the SAME
projected-rotation weighting in training and at serve — no
out-of-distribution minutes, which was the Phase A failure mode.

Deliberate deviation from the ROADMAP feature list: literal "minutes
concentration" features (top-1/top-5 minute share, minutes stddev,
total_minutes) are dropped. After rotation normalization every team's
minute vector IS `CANONICAL_ROTATION_MPG[..roster_size]`, so those
features are a deterministic function of `roster_size` and carry no
extra cross-team signal at serve. Talent concentration is instead
captured by the cam_v3 distribution shape (cam_top1 / top3 / top7 /
sum). `roster_size` is kept as the depth feature.
"""

from __future__ import annotations

import json
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score
from sklearn.model_selection import KFold

from db import get_engine

OUT_DIR = Path(__file__).parent / "models"
SEASONS = (2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024, 2025, 2026)
ARCHETYPES = (
    "Wizard", "Sorcerer", "Warlock", "Bard", "Ranger", "Barbarian",
    "Paladin", "Monk", "Cleric", "Druid", "Rogue", "Fighter",
)

# Canonical MPG by rotation rank (rank 0 = most minutes). MUST stay
# byte-identical to `roster_features.rs::CANONICAL_ROTATION_MPG` — the
# Rust source is the contract, this is a mirror. 13 slots; players
# ranked past slot 12 fall out of the rotation.
CANONICAL_ROTATION_MPG = (
    32.0, 29.8, 27.8, 25.5, 23.0, 20.1, 17.2, 14.4, 11.9, 9.6, 8.2, 7.3, 6.9,
)

# cam_v3 ranking sentinel — players with no Torvik coverage sort to the
# bottom of the rotation (same convention as the Rust `cam_v3 = None`
# → NEG_INFINITY sort in `project_rotation`).
_NEG = -1.0e9

PLAYER_QUERY = """
SELECT
    pss.team_id, pss.season, pss.player_id,
    tps.cam_gbpm_v3_psos AS campom,
    pa.primary_class,
    p.class_year
FROM player_season_stats pss
JOIN players p ON p.id = pss.player_id
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


def normalize_class(cy) -> str | None:
    """Fold the inconsistently-stored class_year vocab ('Sr' / 'Senior' /
    'SR' / …) to a 4-value code. Anything unrecognized → None (unknown),
    which contributes to no experience bucket."""
    if cy is None or (isinstance(cy, float) and pd.isna(cy)):
        return None
    s = str(cy).strip().lower()
    if s.startswith("fr"):
        return "Fr"
    if s.startswith("so"):
        return "So"
    if s.startswith("jr") or s.startswith("ju"):
        return "Jr"
    if s.startswith("sr") or s.startswith("se"):
        return "Sr"
    return None


def aggregate_team_season(group: pd.DataFrame) -> pd.Series:
    """Reduce one team-season's player rows to a Phase B feature vector.

    Ranks by cam_v3, keeps the top 13 (the rotation), assigns canonical
    MPG by rank, and aggregates. Mirrors `build_roster_impact_features`
    on the Rust side exactly."""
    g = group.copy()
    # Rank by cam_v3 desc; missing coverage sorts last (bench slots).
    g["_rank_key"] = g["campom"].fillna(_NEG)
    g = g.sort_values("_rank_key", ascending=False).reset_index(drop=True)
    g = g.head(len(CANONICAL_ROTATION_MPG))
    g["proj_mpg"] = [CANONICAL_ROTATION_MPG[i] for i in range(len(g))]
    total_w = float(g["proj_mpg"].sum())

    row: dict[str, float] = {"roster_size": int(len(g))}

    # cam_v3 distribution — over rotation players with Torvik coverage.
    cam = g.dropna(subset=["campom"])
    if len(cam) == 0:
        for k in ("cam_wmean", "cam_sum", "cam_top1", "cam_top3_mean",
                  "cam_top7_mean", "cam_count_gt5", "cam_count_gt10",
                  "cam_count_gt15"):
            row[k] = np.nan
    else:
        vals = cam["campom"].astype(float)
        w = cam["proj_mpg"].astype(float)
        ws = float(w.sum())
        row["cam_wmean"] = float((vals * w).sum() / ws) if ws > 0 else np.nan
        row["cam_sum"] = float(vals.sum())
        sorted_cam = vals.sort_values(ascending=False).reset_index(drop=True)
        row["cam_top1"] = float(sorted_cam.iloc[0])
        row["cam_top3_mean"] = float(sorted_cam.head(3).mean())
        row["cam_top7_mean"] = float(sorted_cam.head(7).mean())
        row["cam_count_gt5"] = float((vals > 5.0).sum())
        row["cam_count_gt10"] = float((vals > 10.0).sum())
        row["cam_count_gt15"] = float((vals > 15.0).sum())

    # Experience mix — canonical-MPG-weighted class shares.
    cls = g["class_year"].map(normalize_class)
    for code, key in (("Fr", "exp_fr_share"), ("So", "exp_so_share"),
                      ("Jr", "exp_jr_share"), ("Sr", "exp_sr_share")):
        w = float(g.loc[cls == code, "proj_mpg"].sum())
        row[key] = w / total_w if total_w > 0 else 0.0

    # Archetype balance — canonical-MPG-weighted primary-class shares.
    # Players without an archetype (e.g. synthesized recruits at serve
    # time) contribute to no bucket, so a recruit-heavy roster's shares
    # sum to < 1 — that dilution is intentional, not a bug.
    for arch in ARCHETYPES:
        w = float(g.loc[g["primary_class"] == arch, "proj_mpg"].sum())
        row[f"arch_{arch.lower()}"] = w / total_w if total_w > 0 else 0.0

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
    # Drop team-seasons with zero Torvik coverage across the whole
    # rotation — the cam_v3 features are NaN and the row can't be scored.
    pre = len(df)
    df = df.dropna(subset=["cam_wmean", "adj_efficiency_margin"]).reset_index(drop=True)
    if len(df) < pre:
        print(f"Dropped {pre - len(df)} team-seasons with no cam_v3 coverage / target.")
    print(f"After join: {len(df):,} team-seasons with target.")

    feature_cols = [
        c for c in df.columns
        if c not in ("team_id", "season", "adj_efficiency_margin")
    ]
    return df, feature_cols


def lgb_params() -> dict:
    # Conservative for ~4k rows + ~25 features. cam_sum / cam_wmean carry
    # most of the signal (the intended near-identity), so the trees stay
    # shallow and well-regularized — the model's job is calibration, not
    # squeezing variance out of a small N.
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
        "seed": 42,
        "deterministic": True,
    }


def leave_one_season_out(df: pd.DataFrame, feature_cols: list[str]) -> dict:
    """Honest backtest: predict each season from a model trained on the
    other N-1. Same harness as `train_roster_model.py`."""
    results = {}
    overall_y, overall_p = [], []
    best_iters: list[int] = []
    for season in SEASONS:
        train = df[df["season"] != season]
        test = df[df["season"] == season]
        if len(test) == 0:
            continue
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(
            train[feature_cols], train["adj_efficiency_margin"],
            eval_set=[(test[feature_cols], test["adj_efficiency_margin"])],
            eval_metric="mae",
        )
        # Where early stopping settled on this fold — averaged across
        # folds to set the final-fit iteration budget (see `main`).
        bi = model.best_iteration_
        best_iters.append(bi if bi and bi > 0 else lgb_params()["n_estimators"])
        preds = model.predict(test[feature_cols])
        y = test["adj_efficiency_margin"]
        mae = mean_absolute_error(y, preds)
        rmse = float(np.sqrt(mean_squared_error(y, preds)))
        r2 = r2_score(y, preds)
        results[season] = {"mae": mae, "rmse": rmse, "r2": r2, "n": int(len(test))}
        overall_y.extend(y.tolist())
        overall_p.extend(preds.tolist())
        print(f"  season {season}: MAE {mae:.2f}  RMSE {rmse:.2f}  R² {r2:.3f}  n={len(test)}")
    overall = {
        "mae": mean_absolute_error(overall_y, overall_p),
        "rmse": float(np.sqrt(mean_squared_error(overall_y, overall_p))),
        "r2": r2_score(overall_y, overall_p),
    }
    print(f"  pooled:      MAE {overall['mae']:.2f}  RMSE {overall['rmse']:.2f}  R² {overall['r2']:.3f}")
    return {"per_season": results, "pooled": overall, "best_iterations": best_iters}


def random_kfold(df: pd.DataFrame, feature_cols: list[str], n_splits: int = 5) -> dict:
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
    print(f"Features: {len(feature_cols)}  | rows: {len(df)}")
    print(f"Feature order: {feature_cols}")

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
    # No held-out set on the final fit — set the iteration budget to the
    # mean of where the LOSO folds early-stopped. Calibrated for THIS
    # model rather than copied from train_roster_model.py; floored at 50
    # to guard against a degenerate fold.
    final_params.pop("early_stopping_rounds", None)
    best_iters = loso["best_iterations"]
    final_n = max(50, round(sum(best_iters) / len(best_iters)))
    final_params["n_estimators"] = final_n
    print(f"Final-fit n_estimators = {final_n}  (LOSO best-iterations: {best_iters})")
    final = lgb.LGBMRegressor(**final_params)
    final.fit(X, y)

    print("\nFeature importances:")
    importance = sorted(zip(feature_cols, final.feature_importances_), key=lambda x: -x[1])
    for name, imp in importance:
        print(f"  {name:<22} {imp}")

    onnx_path = OUT_DIR / "roster_impact_model.onnx"
    export_to_onnx(final, len(feature_cols), onnx_path)
    print(f"\nExported ONNX → {onnx_path}")

    meta = {
        "model": "roster_impact_model",
        "target": "adj_efficiency_margin",
        "seasons": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(feature_cols),
        "features": feature_cols,
        # Honored verbatim by the Rust boot validator (`validate_model_meta`
        # checks this equals `QUAL_FILTER_STRING`).
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        # v1 = actual same-season cam_v3. v2 follow-up retrains on OOF.
        "cam_v3_source": "actual",
        "final_n_estimators": final_n,
        "canonical_rotation_mpg": list(CANONICAL_ROTATION_MPG),
        "backtest_loso": loso,
        "cv_5fold": cv,
        "feature_importance": [
            {"name": n, "importance": int(i)} for n, i in importance
        ],
    }
    meta_path = OUT_DIR / "roster_impact_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
