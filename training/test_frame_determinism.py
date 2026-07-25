"""Regression guard for issue #222 — nondeterministic training frames.

The bug had two independent halves, both of which made a retrain against an
UNCHANGED database produce a different model:

1. **Undefined rotation cut.** `aggregate_team_season` ranked a team's players
   by cam_v3 and truncated to the top 13, using pandas' default *unstable*
   quicksort with no tiebreak. Ties are routine — every player with no Torvik
   coverage collides at the `_NEG` sentinel — so which player made the rotation
   at all depended on the order the rows happened to arrive in. That moved the
   minutes-weighted class/archetype shares on ~5 of 4,255 team-seasons.

2. **Undefined row order.** No trainer's query had an `ORDER BY`, and Postgres
   guarantees no ordering without one. Every trainer uses `bagging_fraction`,
   which subsamples by row *position*, so the fit moved with the read order.

Measured blast radius before the fix: served preseason AdjEM moved by MAE 0.19
and up to 1.02 points, with the LOSO early-stopping budget landing on 259 vs
266 estimators — pure noise, indistinguishable from a real data change.

The tie-break checks need no database. The frame checks do, and skip without
one. Run: `python test_frame_determinism.py` (exit 0 on pass), or under pytest.
"""

from __future__ import annotations

import hashlib
import sys
import uuid

import pandas as pd

import train_roster_impact_model as R

# Deterministic stand-in UUIDs; the tie-break sorts on the string form, so
# these are chosen to be unambiguous rather than random.
_UUID_A = uuid.UUID("00000000-0000-0000-0000-0000000000aa")
_UUID_B = uuid.UUID("ffffffff-ffff-ffff-ffff-ffffffffffbb")


def _tied_rotation_group(order: str) -> pd.DataFrame:
    """A team whose rotation cut lands exactly on a tie.

    13 rotation slots. Twelve players hold distinct descending cam_v3, then
    TWO tie for the last slot — so exactly one of them makes the rotation and
    the other is dropped. The two carry different class years and archetypes,
    so picking the wrong one visibly changes the aggregated shares.
    """
    rows = [
        {
            "player_id": uuid.UUID(int=i),
            "campom": 20.0 - i,
            "campom_source": "actual_fallback",
            "primary_class": "Wizard",
            "class_year": "Jr",
        }
        for i in range(12)
    ]
    tied = [
        {
            "player_id": _UUID_A,
            "campom": 1.0,
            "campom_source": "actual_fallback",
            "primary_class": "Rogue",
            "class_year": "Fr",
        },
        {
            "player_id": _UUID_B,
            "campom": 1.0,
            "campom_source": "actual_fallback",
            "primary_class": "Cleric",
            "class_year": "Sr",
        },
    ]
    if order == "reversed":
        tied.reverse()
    return pd.DataFrame(rows + tied)


def test_rotation_cut_is_order_independent() -> None:
    """The tie-break, not the input order, decides who makes the rotation."""
    a = R.aggregate_team_season(_tied_rotation_group("forward"))
    b = R.aggregate_team_season(_tied_rotation_group("reversed"))
    diffs = [k for k in a.index if not _close(a[k], b[k])]
    assert not diffs, (
        "aggregate_team_season depends on input row order for these features: "
        f"{diffs}. A tie at the rotation cut must be broken by player_id, not "
        "by whatever order the rows arrived in (issue #222)."
    )


def test_rotation_cut_prefers_the_lower_uuid() -> None:
    """Pin the direction, so the rule can't silently invert.

    Ties sort ascending by the UUID's canonical lowercase hex, which is the
    same order Rust's `Uuid: Ord` produces bytewise — that is what keeps
    `build_roster_impact_features` breaking ties the same way at serve time.
    """
    out = R.aggregate_team_season(_tied_rotation_group("forward"))
    # _UUID_A ("0000…aa") sorts before _UUID_B ("ffff…bb"), so the Fr/Rogue
    # player takes the last slot and the Sr/Cleric player is cut.
    assert out["exp_fr_share"] > 0.0, (
        "expected the lower-UUID tied player (Fr) to hold the last rotation "
        "slot; the tie-break direction changed (issue #222)."
    )
    assert out["exp_sr_share"] == 0.0, (
        "expected the higher-UUID tied player (Sr) to be cut from the "
        "rotation; the tie-break direction changed (issue #222)."
    )


def _close(x, y) -> bool:
    if isinstance(x, float) and isinstance(y, float):
        return abs(x - y) < 1e-12 or (pd.isna(x) and pd.isna(y))
    return bool(x == y)


def _frame_digest(df: pd.DataFrame) -> str:
    return hashlib.md5(
        pd.util.hash_pandas_object(df, index=False).values.tobytes()
    ).hexdigest()


def test_onnx_export_is_byte_stable() -> None:
    """Exporting the same fitted model twice must produce identical bytes.

    `onnxmltools` names the graph with a fresh UUID on every conversion, so
    before the fix two exports of a bit-identical model differed in bytes
    while predicting identically. That defeats the point: the reason to make
    training deterministic is to be able to look at a model diff and conclude
    "nothing changed", which a random graph name makes impossible.

    Fast enough for CI — a tiny synthetic fit, no database.
    """
    import tempfile
    from pathlib import Path

    import lightgbm as lgb
    import numpy as np

    import train_freshman_model as F
    import train_trajectory_model as T

    rng = np.random.RandomState(0)
    X = rng.rand(200, 6)
    y = X[:, 0] * 3.0 + rng.rand(200)
    model = lgb.LGBMRegressor(n_estimators=10, num_leaves=4, verbose=-1)
    model.fit(X, y)

    for mod in (R, T, F):
        with tempfile.TemporaryDirectory() as d:
            # Same destination both times: the graph name is derived from the
            # artifact's stem, so re-exporting to one path is the case that
            # must be stable.
            a = Path(d) / "m.onnx"
            mod.export_to_onnx(model, 6, a)
            first = a.read_bytes()
            mod.export_to_onnx(model, 6, a)
            second = a.read_bytes()
            assert first == second, (
                f"{mod.__name__}.export_to_onnx is not byte-stable — the "
                "ONNX graph name must be deterministic (issue #222)."
            )


# Checks that couldn't run, and why. A skipped check must never report as a
# pass — CI runs this without a database on purpose, so "5 ok" when only 3
# actually executed is precisely the silent-no-op this whole issue is about.
_SKIPS: dict[str, str] = {}


def _skip(name: str, reason: str) -> None:
    _SKIPS[name] = reason


def _db_reachable() -> bool:
    try:
        from db import get_engine

        with get_engine().connect():
            return True
    except Exception:
        return False


def test_roster_impact_frame_is_deterministic() -> None:
    """Two reads of the roster-impact frame must be byte-identical."""
    if not _db_reachable():
        return _skip(
            "test_roster_impact_frame_is_deterministic", "no database reachable"
        )
    a, cols, _ = R.build_dataset()
    b, _, _ = R.build_dataset()
    assert _frame_digest(a[cols]) == _frame_digest(b[cols]), (
        "build_dataset() returned two different frames from the same "
        "database. The read is not deterministic (issue #222)."
    )


def test_layer1_frames_are_deterministic() -> None:
    """Same for the trajectory / freshman frames, which feed the OOF tables.

    These matter most: their fits produce `trajectory_oof_predictions` and
    `freshman_oof_predictions`, so nondeterminism here propagates into every
    downstream model *and* changes its provenance fingerprint, making a no-op
    retrain look like a genuine snapshot change.
    """
    if not _db_reachable():
        return _skip("test_layer1_frames_are_deterministic", "no database reachable")
    import train_freshman_model as F
    import train_trajectory_model as T

    for name, mod in (("trajectory", T), ("freshman", F)):
        first = mod.build_dataset()
        second = mod.build_dataset()
        assert _frame_digest(first) == _frame_digest(second), (
            f"{name} build_dataset() is not deterministic (issue #222)."
        )


def main() -> int:
    checks = [
        test_rotation_cut_is_order_independent,
        test_rotation_cut_prefers_the_lower_uuid,
        test_onnx_export_is_byte_stable,
        test_roster_impact_frame_is_deterministic,
        test_layer1_frames_are_deterministic,
    ]
    failures: list[str] = []
    for check in checks:
        try:
            check()
            if check.__name__ in _SKIPS:
                print(f"  SKIP: {check.__name__} — {_SKIPS[check.__name__]}")
            else:
                print(f"  ok:   {check.__name__}")
        except AssertionError as e:
            failures.append(f"{check.__name__}: {e}")
    print()
    if failures:
        print(f"{len(failures)} failure(s):")
        for f in failures:
            print(f"  - {f}")
        return 1
    ran = len(checks) - len(_SKIPS)
    summary = f"{ran}/{len(checks)} #222 determinism checks pass"
    if _SKIPS:
        summary += f" ({len(_SKIPS)} skipped: {', '.join(sorted(_SKIPS))})"
    print(summary + ".")
    return 0


if __name__ == "__main__":
    sys.exit(main())
