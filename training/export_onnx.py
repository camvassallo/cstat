"""
Export trained LightGBM models to ONNX format for Rust inference via the `ort` crate.
"""

import json
from pathlib import Path

import lightgbm as lgb
import numpy as np
import onnxmltools
from onnxmltools.convert.common.data_types import FloatTensorType

MODEL_DIR = Path(__file__).parent / "models"


def export_model(lgb_path: str, onnx_path: str, n_features: int, is_classifier: bool):
    """Convert a saved LightGBM model to ONNX."""
    booster = lgb.Booster(model_file=lgb_path)

    if is_classifier:
        # Fit on tiny dummy data to initialize all internal attributes,
        # then swap in the real booster
        model = lgb.LGBMClassifier(n_estimators=2, verbose=-1)
        dummy_X = np.zeros((4, n_features))
        dummy_y = np.array([0, 1, 0, 1])
        model.fit(dummy_X, dummy_y)
        model._Booster = booster
        model._n_features = n_features
    else:
        model = lgb.LGBMRegressor()
        model._Booster = booster
        model.fitted_ = True
        model._n_features = n_features

    initial_type = [("features", FloatTensorType([None, n_features]))]
    onnx_model = onnxmltools.convert_lightgbm(
        model,
        initial_types=initial_type,
        target_opset=15,
    )

    onnxmltools.utils.save_model(onnx_model, onnx_path)
    print(f"Exported: {onnx_path}")


def main():
    meta_path = MODEL_DIR / "model_meta.json"
    if not meta_path.exists():
        print("No model_meta.json found. Run train.py first.")
        return

    with open(meta_path) as f:
        meta = json.load(f)

    n_features = meta["n_features"]

    export_model(
        str(MODEL_DIR / "margin_model.lgb"),
        str(MODEL_DIR / "margin_model.onnx"),
        n_features,
        is_classifier=False,
    )

    export_model(
        str(MODEL_DIR / "win_model.lgb"),
        str(MODEL_DIR / "win_model.onnx"),
        n_features,
        is_classifier=True,
    )

    print(f"\nONNX models ready for Rust inference ({n_features} features)")


if __name__ == "__main__":
    main()
