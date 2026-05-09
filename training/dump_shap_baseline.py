"""
Dump LightGBM TreeSHAP values for a fixed sample of feature vectors.

Output: training/models/shap_baseline.json — consumed by the Rust parity
test in `crates/cstat-core/src/treeshap.rs::treeshap_matches_lightgbm_baseline`.
Asserts our Rust port of TreeSHAP matches LightGBM's own `pred_contrib`
output to within 1e-3 across every (sample, feature) cell.

Re-run after retraining the margin model.
"""

import json
from pathlib import Path

import lightgbm as lgb
import numpy as np

MODEL_DIR = Path(__file__).parent / "models"


def main() -> None:
    booster = lgb.Booster(model_file=str(MODEL_DIR / "margin_model.lgb"))
    n_features = booster.num_feature()
    feature_names = booster.feature_name()

    rng = np.random.default_rng(42)
    samples: list[np.ndarray] = []

    # Sample 0: all zeros (matched matchup baseline).
    samples.append(np.zeros(n_features))

    # Sample 1: home-favored archetype.
    home_strong = np.zeros(n_features)
    home_strong[feature_names.index("venue")] = 1.0
    home_strong[feature_names.index("diff_adj_efficiency_margin")] = 18.0
    home_strong[feature_names.index("diff_elo")] = 220.0
    home_strong[feature_names.index("diff_w_gbpm")] = 4.5
    home_strong[feature_names.index("diff_w_ogbpm")] = 2.0
    home_strong[feature_names.index("diff_w_dgbpm")] = 2.5
    home_strong[feature_names.index("diff_win_pct")] = 0.35
    samples.append(home_strong)

    # Sample 2: away-favored archetype (mirror of sample 1).
    samples.append(-home_strong)

    # Sample 3: neutral-site, even matchup but with a star-player edge.
    star_diff = np.zeros(n_features)
    star_diff[feature_names.index("diff_star_gbpm")] = 6.0
    star_diff[feature_names.index("diff_star_ogbpm")] = 4.0
    star_diff[feature_names.index("diff_star_dgbpm")] = 2.0
    star_diff[feature_names.index("diff_star_ppg")] = 8.0
    samples.append(star_diff)

    # Samples 4-19: random vectors covering varied magnitudes.
    for _ in range(16):
        x = rng.normal(0.0, 1.0, n_features) * 5.0
        # Flag features stay 0/1.
        x[feature_names.index("venue")] = float(rng.integers(0, 2))
        x[feature_names.index("is_conference_game")] = float(rng.integers(0, 2))
        # Win-pct, pythag, road-win — bounded fractions in [-1, 1].
        for fname in ("diff_win_pct", "diff_pythag_win_pct", "diff_road_win_pct"):
            x[feature_names.index(fname)] = float(rng.uniform(-0.6, 0.6))
        samples.append(x)

    samples_arr = np.stack(samples).astype(np.float64)

    # LightGBM `pred_contrib`: shape (n_samples, n_features + 1) where the
    # final column is the per-sample base value (E[f(x)]). The remaining
    # columns are SHAP values aligned with `feature_names`.
    contrib = booster.predict(samples_arr, pred_contrib=True)
    shap_values = contrib[:, :-1]
    base_values = contrib[:, -1]
    predictions = booster.predict(samples_arr)

    # Cross-check: base + sum(shap) ≈ pred (within float precision).
    reconstructed = base_values + shap_values.sum(axis=1)
    max_recon_diff = float(np.abs(reconstructed - predictions).max())
    print(f"reconstruction max |Δ|: {max_recon_diff:.2e}")
    assert max_recon_diff < 1e-5, "lightgbm pred_contrib doesn't reconstruct cleanly"

    out = {
        "feature_names": feature_names,
        "samples": samples_arr.tolist(),
        "shap_values": shap_values.tolist(),
        "base_values": base_values.tolist(),
        "predictions": predictions.tolist(),
    }

    out_path = MODEL_DIR / "shap_baseline.json"
    with out_path.open("w") as f:
        json.dump(out, f, indent=2)

    print(f"wrote {len(samples)} samples to {out_path}")
    print(f"base value: {base_values[0]:.4f}")
    print(f"sample 0 (all zeros) prediction: {predictions[0]:.4f}")
    print(f"sample 1 (home-favored) prediction: {predictions[1]:.4f}")
    print(f"sample 2 (away-favored) prediction: {predictions[2]:.4f}")


if __name__ == "__main__":
    main()
