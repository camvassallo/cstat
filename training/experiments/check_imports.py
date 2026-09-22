"""Import every experiment script in this directory; fail on the first that cannot be.

The scripts here import the trainers and shared libs from `training/` through
a path shim (#364). This is the acceptance check for that move — "no experiment
script imports fail" — and it stays useful afterwards: a trainer that renames a
symbol an experiment reads (`lgb_params`, `build_dataset`, `DEST_FEATURE_COLS`,
…) breaks the reproduction silently otherwise, and the verdict in `README.md`
then points at a script that no longer runs.

Import only. Nothing here touches the database: every script builds its engine
lazily, so this runs in CI with an unreachable `DATABASE_URL`.

    cd training && ./.venv/bin/python experiments/check_imports.py
"""

from __future__ import annotations

import importlib
import sys
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))


def main() -> int:
    failures = 0
    scripts = sorted(p for p in HERE.glob("*.py") if p.name not in {Path(__file__).name, "__init__.py"})
    for script in scripts:
        try:
            importlib.import_module(script.stem)
            print(f"  ✓ {script.name}")
        except Exception:  # noqa: BLE001 - report every failure, then exit non-zero
            failures += 1
            print(f"  ✗ {script.name}")
            traceback.print_exc()
    print(f"{len(scripts) - failures}/{len(scripts)} experiment scripts import")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
