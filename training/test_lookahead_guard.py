"""Database-backed wrapper around `lookahead_guard.py` for the training guards
(#362): every frame's features must be invariant to the target season.

Skips — and says so — when no database is reachable or the release
`cstat-ingest` binary is not built, so the CI job (which has no database)
reports the skip rather than a silent pass. Locally, and in
`retrain_downstream.sh`, it runs for real.

Run:  cd training && ./.venv/bin/python test_lookahead_guard.py [--seasons 2026] [--frames ...]
"""

from __future__ import annotations

import sys

import lookahead_guard as G


def _db_reachable() -> bool:
    try:
        from sqlalchemy import text

        with G.db.get_engine().connect() as conn:
            conn.execute(text("SELECT 1"))
        return True
    except Exception:
        return False


def main() -> int:
    if not _db_reachable():
        print("  SKIP: test_lookahead_guard — no database reachable")
        print("\n0/1 #362 look-ahead checks ran (1 skipped).")
        return 0
    frames = list(G.FRAMES)
    if "calibrator" in frames and not (G.REPO_ROOT / "target" / "release" / "cstat-ingest").exists():
        print("  SKIP: calibrator frame — release cstat-ingest not built (cargo build --release -p cstat-ingest)")
        frames.remove("calibrator")
    args = sys.argv[1:]
    if not any(a.startswith("--frames") for a in args):
        args += ["--frames", ",".join(frames)]
    sys.argv = [sys.argv[0], *args]
    rc = G.main()
    print(f"\n{'1/1' if rc == 0 else '0/1'} #362 look-ahead checks pass.")
    return rc


if __name__ == "__main__":
    sys.exit(main())
