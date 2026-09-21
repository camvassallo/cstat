"""Guards for the walk-forward harness (#361). Database-free.

The whole point of `walk_forward.py` is the direction of time: a training row
must be strictly earlier than the season it is scored on. If that invariant
slips — a `<=`, an off-by-one on the first fold — every meta in the tree
quietly reports a number that is not forward-chained, and nothing downstream
can tell. So the fold generator is pinned here, along with the rank metrics'
identities (a perfect prediction scores 1.0 on every one of them, a reversed
one scores 0 where 0 is defined) so a refactor cannot flip a sign.

Run:  cd training && ./.venv/bin/python test_walk_forward.py
"""

from __future__ import annotations

import sys

import numpy as np
import pandas as pd

import walk_forward as W


def _rows(n_seasons: int = 3, per_season: int = 60, seed: int = 0) -> list[dict]:
    rng = np.random.default_rng(seed)
    rows = []
    for s in range(2021, 2021 + n_seasons):
        actual = rng.normal(0, 10, per_season)
        for i in range(per_season):
            rows.append({"season": s, "actual": float(actual[i]), "baseline": float(actual[i] + rng.normal(0, 3)),
                         "retained": 0.5, "program_level": float(actual[i])})
    return rows


def test_folds_train_strictly_earlier() -> None:
    seasons = pd.Series([2016] * 3 + [2017] * 3 + [2021] * 2 + [2022] * 2 + [2023] * 2)
    seen = []
    for s, tr, te in W.folds(seasons, walk_from=2021):
        assert (seasons[tr] < s).all(), f"fold {s}: a training row is not strictly earlier"
        assert (seasons[te] == s).all(), f"fold {s}: test rows are not season {s}"
        assert not (tr & te).any(), "train and test overlap"
        seen.append(s)
    assert seen == [2021, 2022, 2023], f"expected folds for 2021-2023, got {seen}"


def test_folds_skip_seasons_with_nothing_earlier() -> None:
    seasons = pd.Series([2021] * 3 + [2022] * 3)
    assert [s for s, _, _ in W.folds(seasons, walk_from=2021)] == [2022], "2021 has nothing earlier and must be skipped"


def test_regression_walk_forward_scores_only_test_rows() -> None:
    seasons = pd.Series([2018] * 5 + [2019] * 5 + [2021] * 5 + [2022] * 5)
    X = pd.DataFrame({"x": np.arange(20, dtype=float)})
    y = X["x"] * 2

    def fit_predict(x_tr, y_tr, x_te):
        # Records what it trained on so the invariant is checked from the
        # callback's side too.
        assert (seasons[x_tr.index] < seasons[x_te.index].iloc[0]).all()
        return x_te["x"].to_numpy() * 2

    block, preds = W.regression_walk_forward(X, seasons, y, fit_predict, walk_from=2021, label="t")
    assert preds[seasons < 2021].isna().all(), "rows before walk_from must not be scored"
    assert preds[seasons >= 2021].notna().all()
    assert block["pooled"]["mae"] == 0.0 and block["pooled"]["n"] == 10
    assert set(block["per_season"]) == {"2021", "2022"}


def test_rank_metrics_identities() -> None:
    rows = _rows()
    perfect = {id(r): r["actual"] for r in rows}
    reversed_ = {id(r): -r["actual"] for r in rows}
    for label, fn in W.RANK_METRICS:
        v = W.per_season(rows, perfect, fn)
        if label == "worst miss T10":
            assert v == 0.0, label
        else:
            assert abs(v - 1.0) < 1e-9, f"{label} of a perfect prediction should be 1, got {v}"
    assert W.per_season(rows, reversed_, W.concordance_top50) == 0.0
    assert abs(W.per_season(rows, reversed_, W.rho_field) + 1.0) < 1e-9


def test_paired_z_sign() -> None:
    rows = _rows()
    good = {id(r): r["actual"] + 1.0 for r in rows}
    bad = {id(r): r["actual"] + 3.0 for r in rows}
    d, z = W.paired_z(rows, good, bad)
    assert d < 0 and z < 0, "negative = first argument better"


def test_team_table_shape() -> None:
    rows = _rows()
    preds = {"a": {id(r): r["baseline"] for r in rows}, "b": {id(r): r["actual"] for r in rows}}
    t = W.team_table(rows, preds, reference="a", print_it=False)
    assert t["n"] == len(rows)
    assert "all" in t["cohorts"] and "top25 (ex-ante)" in t["cohorts"]
    assert t["cohorts"]["all"]["b"]["mae"] == 0.0
    assert set(t["paired"]) == {"b"} and t["paired"]["b"]["pooled"]["z"] < 0


def main() -> int:
    checks = [
        test_folds_train_strictly_earlier,
        test_folds_skip_seasons_with_nothing_earlier,
        test_regression_walk_forward_scores_only_test_rows,
        test_rank_metrics_identities,
        test_paired_z_sign,
        test_team_table_shape,
    ]
    failed = 0
    for c in checks:
        try:
            c()
            print(f"  ok:   {c.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL: {c.__name__} — {e}")
    print(f"\n{len(checks) - failed}/{len(checks)} #361 walk-forward checks pass.")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
