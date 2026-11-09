"""Guards on the walk-forward game models behind stored historical projections (#468).

Database-free; reads only the committed artifacts. The Rust loader
(`crates/cstat-core/src/heldout.rs`) enforces the same rules at sweep time;
these catch a bad set before it is committed.

    cd training && ./.venv/bin/python test_heldout_game_models.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

from train_heldout_game_models import (
    FULL_FIT_META,
    META_NAME,
    PROTOCOL,
    bundle_files,
    full_fit_digests,
    scorable_seasons,
    sha256_file,
)

MODELS = Path(__file__).parent / "models"
HELDOUT = MODELS / "heldout"


def load():
    return json.load(open(HELDOUT / META_NAME)), json.load(open(MODELS / FULL_FIT_META))


def test_every_bundle_trains_strictly_before_its_season():
    meta, _ = load()
    assert meta["protocol"] == PROTOCOL
    for b in meta["bundles"]:
        assert b["train_seasons"], f"{b['season']} has no training seasons"
        assert max(b["train_seasons"]) < b["season"], (
            f"bundle for {b['season']} trained on {b['train_seasons']}"
        )


def test_covers_every_trained_season_but_the_first():
    meta, full = load()
    have = sorted(b["season"] for b in meta["bundles"])
    assert have == scorable_seasons(full["seasons"]), (have, full["seasons"])
    assert meta["unscorable_seasons"] == [min(full["seasons"])]


def test_stamped_for_the_committed_full_fit():
    # Fails when the game models are retrained without re-running
    # train_heldout_game_models.py. The sweep would refuse every trained-on
    # season; this says so before the merge instead.
    meta, full = load()
    assert meta["full_fit"]["seasons"] == full["seasons"]
    assert meta["full_fit"]["n_games"] == full["n_games"]
    assert meta["full_fit"]["artifacts"] == full_fit_digests(MODELS), (
        "held-out set is stamped for different full-fit artifacts; "
        "re-run train_heldout_game_models.py"
    )
    assert meta["features"] == full["features"]
    assert meta["total_features"] == full["total_features"]


def test_artifacts_match_their_digests_and_load():
    import onnxruntime as ort

    meta, full = load()
    widths = {"margin": len(full["features"]), "total": len(full["total_features"])}
    for b in meta["bundles"]:
        names = bundle_files(b["season"])
        for head, art in b["artifacts"].items():
            assert art["file"] == names[head]
            path = HELDOUT / art["file"]
            assert sha256_file(path) == art["sha256"], f"{path.name} changed after stamping"
            sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
            x = np.zeros((1, widths[head]), dtype=np.float32)
            out = sess.run(None, {sess.get_inputs()[0].name: x})[0]
            assert np.isfinite(out).all(), path.name


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"✓ {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"✗ {t.__name__}: {e}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
