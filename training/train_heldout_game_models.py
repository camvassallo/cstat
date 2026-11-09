"""Walk-forward point-in-time game models for STORED historical projections (#468).

`train.py` does its final fit on every season in `SEASONS`, which is right for
a live game and wrong for a stored projection of a game inside that range: the
model scoring it has already seen the result. On 2026 that was worth 4.6 ATS
points (54.4% held out vs 59.0% seen), on top of the feature leak in #274.

This script fits, for every trained-on season S after the first, the same
point-in-time margin and total models `train.py` would ship if `SEASONS`
stopped at S-1 (same frame, same `fit_final_models`, iteration counts picked
by the same chronological backtest on the training rows only). Nothing from
season S or later reaches the bundle for S, not even through early stopping.

Only margin and total are exported. The sweep never runs the win classifier:
a served win probability is derived from the margin
(`projection::margin_to_win_prob`), so a held-out win model would be dead
weight in the repo.

Writes `models/heldout/pit_{S}_{margin,total}_model.onnx` and
`models/heldout/heldout_meta.json`. The meta stamps the digest of the full-fit
artifacts it accompanies; `cstat_core::heldout` refuses to route a season
through a bundle set stamped for a different full fit, so retraining the game
models without re-running this is a loud error in the `game-projections`
sweep, never a silent mix of generations.

The first trained-on season (2015) has nothing earlier to train on and gets
no bundle; the sweep stores no projection for it.

Usage (the frame is the pickle `train.py`'s point-in-time run was fitted on;
without --frame it is rebuilt, about 90 minutes and ~13 GB peak):

  cd training && GBPM_VARIANT=pit_cam_v3 ./.venv/bin/python train_heldout_game_models.py \\
      [--frame /path/frame.pkl] [--out-dir models/heldout]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

import numpy as np
import pandas as pd

HERE = Path(__file__).parent
MODELS = HERE / "models"

# Full-fit artifacts the held-out set stands in for. The digests of these go
# into the meta; `crates/cstat-core/src/heldout.rs` recomputes them at load.
FULL_FIT_META = "pit_model_meta.json"
FULL_FIT_ARTIFACTS = ("pit_margin_model.onnx", "pit_total_model.onnx")

META_NAME = "heldout_meta.json"
PROTOCOL = "walk_forward"


def bundle_files(season: int) -> dict[str, str]:
    """The artifact names for one season's bundle. Mirrored in heldout.rs."""
    return {
        "margin": f"pit_{season}_margin_model.onnx",
        "total": f"pit_{season}_total_model.onnx",
    }


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def full_fit_digests(models_dir: Path) -> dict[str, str]:
    return {name: sha256_file(models_dir / name) for name in FULL_FIT_ARTIFACTS}


def scorable_seasons(seasons: list[int]) -> list[int]:
    """Trained-on seasons with at least one earlier season to train on."""
    s = sorted(seasons)
    return s[1:]


def load_frame(frame: str | None) -> tuple[pd.DataFrame, list[str], list[str]]:
    if frame:
        df = pd.read_pickle(frame)
        cols, sums = json.load(open(frame + ".json"))
        return df, list(cols), list(sums)
    from features import GBPM_VARIANT, build_feature_matrix
    from db import get_engine

    if GBPM_VARIANT != "pit_cam_v3":
        sys.exit("set GBPM_VARIANT=pit_cam_v3: these stand in for the point-in-time bundle")
    return build_feature_matrix(get_engine())


def onnx_max_abs_diff(onnx_path: Path, booster, X: np.ndarray) -> float:
    """Largest gap between the exported graph and the booster it came from, on
    the rows it will be judged on. A converter regression shows up here rather
    than as a quietly different stored projection."""
    import onnxruntime as ort

    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    name = sess.get_inputs()[0].name
    out = sess.run(None, {name: X.astype(np.float32)})[0].reshape(-1)
    return float(np.max(np.abs(out - booster.predict(X))))


def score(y_margin, y_total, p_margin, p_total) -> dict:
    return {
        "margin_mae": float(np.mean(np.abs(p_margin - y_margin))),
        "margin_winner_acc": float(np.mean((p_margin > 0) == (y_margin > 0))),
        "total_mae": float(np.mean(np.abs(p_total - y_total))),
        "n": int(len(y_margin)),
    }


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--frame", help="pickled feature frame (+ .json column lists)")
    ap.add_argument("--out-dir", default=str(MODELS / "heldout"))
    ap.add_argument("--models-dir", default=str(MODELS))
    args = ap.parse_args()

    # Imported here so `--help` and the database-free test need no LightGBM.
    import lightgbm as lgb

    from export_onnx import export_model
    from features import completeness_subset
    from train import fit_final_models

    models_dir = Path(args.models_dir)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    full_meta = json.load(open(models_dir / FULL_FIT_META))

    df, feature_cols, sum_cols = load_frame(args.frame)
    total_cols = feature_cols + sum_cols
    df = df.dropna(subset=completeness_subset(total_cols)).reset_index(drop=True)

    # The frame must be the one the full fit was trained on, or the held-out
    # bundles describe a different model family from the one they stand in
    # for. Seasons, row count and both feature contracts must all agree.
    seasons = sorted(int(s) for s in df["season"].unique())
    problems = []
    if seasons != sorted(full_meta["seasons"]):
        problems.append(f"seasons {seasons} != full fit {full_meta['seasons']}")
    if len(df) != full_meta["n_games"]:
        problems.append(f"{len(df)} complete games != full fit n_games {full_meta['n_games']}")
    if feature_cols != full_meta["features"]:
        problems.append("margin feature list differs from the full fit")
    if total_cols != full_meta["total_features"]:
        problems.append("total feature list differs from the full fit")
    if problems:
        sys.exit("frame does not match the full fit:\n  " + "\n  ".join(problems))

    full_margin = lgb.Booster(model_file=str(models_dir / "pit_margin_model.lgb"))

    bundles = []
    predictions = []
    for season in scorable_seasons(seasons):
        train = df[df["season"] < season].reset_index(drop=True)
        test = df[df["season"] == season].reset_index(drop=True)
        train_seasons = sorted(int(s) for s in train["season"].unique())
        assert max(train_seasons) < season, "a held-out bundle saw its own season"
        print(f"\n######## season {season}: train {train_seasons[0]}-{train_seasons[-1]} "
              f"({len(train)} games), test {len(test)} games", flush=True)

        fit = fit_final_models(train, feature_cols, total_cols)
        files = bundle_files(season)
        X_diff = test[feature_cols].to_numpy(dtype=np.float64)
        X_total = test[total_cols].to_numpy(dtype=np.float64)
        diffs = {}
        with tempfile.TemporaryDirectory() as tmp:
            for head, model, n in (
                ("margin", fit["margin"], len(feature_cols)),
                ("total", fit["total"], len(total_cols)),
            ):
                lgb_path = Path(tmp) / f"{head}.lgb"
                model.booster_.save_model(str(lgb_path))
                export_model(str(lgb_path), str(out_dir / files[head]), n, is_classifier=False)
                X = X_diff if head == "margin" else X_total
                diffs[head] = onnx_max_abs_diff(out_dir / files[head], model.booster_, X)

        p_margin = fit["margin"].predict(test[feature_cols])
        p_total = fit["total"].predict(test[total_cols])
        held = score(test["margin"].to_numpy(), test["total"].to_numpy(), p_margin, p_total)
        seen_margin = full_margin.predict(X_diff)
        seen = {
            "margin_mae": float(np.mean(np.abs(seen_margin - test["margin"].to_numpy()))),
            "margin_winner_acc": float(np.mean((seen_margin > 0) == (test["margin"].to_numpy() > 0))),
        }
        print(f"  held out: margin MAE {held['margin_mae']:.3f}, total MAE {held['total_mae']:.3f}; "
              f"full fit (has seen {season}): margin MAE {seen['margin_mae']:.3f}", flush=True)

        bundles.append({
            "season": season,
            "train_seasons": train_seasons,
            "n_games_train": int(len(train)),
            "iterations": {
                "margin": int(fit["margin"].n_estimators),
                "total": int(fit["total"].n_estimators),
            },
            "artifacts": {head: {"file": f, "sha256": sha256_file(out_dir / f)}
                          for head, f in files.items()},
            "onnx_max_abs_diff": diffs,
            "test": {"held_out": held, "full_fit_seen": seen},
        })
        predictions.append(pd.DataFrame({
            "game_id": test["game_id"].astype(str),
            "season": season,
            "pred_margin": p_margin,
            "pred_total": p_total,
        }))

    meta = {
        "protocol": PROTOCOL,
        "description": (
            "Point-in-time margin and total models trained strictly on seasons "
            "before the one they score. Used only by the game-projections sweep "
            "for seasons the full fit was trained on (#468)."
        ),
        "full_fit": {
            "meta": FULL_FIT_META,
            "seasons": full_meta["seasons"],
            "n_games": full_meta["n_games"],
            "artifacts": full_fit_digests(models_dir),
        },
        "features": feature_cols,
        "total_features": total_cols,
        "unscorable_seasons": [s for s in seasons if s not in scorable_seasons(seasons)],
        "bundles": bundles,
    }
    with open(out_dir / META_NAME, "w") as f:
        json.dump(meta, f, indent=2)
        f.write("\n")
    # Python-frame predictions, for experiments only (gitignored). They are the
    # raw model leg on the TRAINING features, not what the sweep stores: the
    # sweep builds features in Rust and blends in the preseason leg.
    pd.concat(predictions).to_csv(out_dir / "heldout_predictions.csv", index=False)
    print(f"\nwrote {len(bundles)} bundles and {META_NAME} to {out_dir}")


if __name__ == "__main__":
    main()
