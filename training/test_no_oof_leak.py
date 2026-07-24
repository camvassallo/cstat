"""Regression guard for issue #199 — the trajectory OOF early-stopping leak.

The bug: the honest-backtest fits (`leave_one_pair_out`, `lopo_quantile_predictions`,
`kfold_cv`) passed the HELD-OUT fold as the early-stopping `eval_set`, so
`best_iteration_` was chosen to minimize error on the very labels being scored.
Those predictions are persisted to `trajectory_oof_predictions` (served on
historical routes + consumed by `train_roster_impact_model.py`), so the leak
optimistically biased served projections and the preseason calibrator's inputs.

The fix has two invariants, both locked here (no DB required):

1. `lgb_params()` carries NO `early_stopping_rounds` and a FIXED `n_estimators`,
   so there is no early-stopping knob to attach a held-out eval_set to, and the
   backtest recipe is identical to the served `fit_final`.

2. None of the backtest fits (nor `fit_final`) call `.fit(...)` with an
   `eval_set=` keyword — the source-level shape of the leak. AST-checked so a
   future edit that re-adds `eval_set` to any of these functions fails loudly
   BEFORE it can re-poison the OOF table, even if it plumbs params differently.

Run: `python test_no_oof_leak.py` (exit 0 on pass), or under pytest.
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

import train_trajectory_model as T

# Functions whose `.fit()` calls must never receive a held-out eval_set. The
# three backtest fits are where the leak lived; `fit_final` is the served fit,
# guarded so it can't silently diverge back into an early-stopping variant.
LEAK_SENSITIVE_FUNCS = {
    "leave_one_pair_out",
    "lopo_quantile_predictions",
    "kfold_cv",
    "fit_final",
}

SOURCE_PATH = Path(T.__file__)


def test_lgb_params_has_no_early_stopping() -> None:
    """No early-stopping knob exists to hang a held-out eval_set on, for both
    the mean (regression) and quantile objectives."""
    for objective, alpha in [("regression", None), ("quantile", 0.1), ("quantile", 0.9)]:
        p = T.lgb_params(objective=objective, alpha=alpha)
        assert "early_stopping_rounds" not in p, (
            f"early_stopping_rounds reintroduced into lgb_params({objective}, {alpha}) "
            "— this is the #199 leak vector. Remove it."
        )
        assert p["n_estimators"] == 400, (
            f"lgb_params({objective}, {alpha}) n_estimators={p['n_estimators']} != 400. "
            "The backtest and served fit must share a fixed iteration budget so no "
            "held-out fold is ever consulted to pick the tree count."
        )


def _fit_calls_with_eval_set() -> list[str]:
    """Return 'func:line' for every `.fit(..., eval_set=...)` call inside a
    leak-sensitive function. Empty list == clean."""
    tree = ast.parse(SOURCE_PATH.read_text(), filename=str(SOURCE_PATH))
    offenders: list[str] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.FunctionDef) or node.name not in LEAK_SENSITIVE_FUNCS:
            continue
        for call in ast.walk(node):
            if (
                isinstance(call, ast.Call)
                and isinstance(call.func, ast.Attribute)
                and call.func.attr == "fit"
                and any(kw.arg == "eval_set" for kw in call.keywords)
            ):
                offenders.append(f"{node.name}:{call.lineno}")
    return offenders


def test_no_eval_set_in_leak_sensitive_fits() -> None:
    """No backtest/served fit hands a held-out fold to `.fit()` as an eval_set."""
    offenders = _fit_calls_with_eval_set()
    assert not offenders, (
        "eval_set= reintroduced into leak-sensitive fit(s): "
        + ", ".join(offenders)
        + ". Passing the held-out fold as an early-stopping eval_set is the #199 "
        "leak — remove it (train on a fixed n_estimators instead)."
    )


def main() -> int:
    checks = [test_lgb_params_has_no_early_stopping, test_no_eval_set_in_leak_sensitive_fits]
    failures: list[str] = []
    for check in checks:
        try:
            check()
            print(f"  ok: {check.__name__}")
        except AssertionError as e:
            failures.append(f"{check.__name__}: {e}")
    print()
    if failures:
        print(f"{len(failures)} failure(s):")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("all #199 leak-guard checks pass.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
