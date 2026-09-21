"""Look-ahead guard — a training frame's FEATURES must be invariant to the
target season's data (#362).

The last three model PRs each found a leak or a train/serve skew by hand:
the calibrator frame carried each unprojected player's target-season CAM
(#358), the trajectory backtest early-stopped on its held-out fold (#199),
and the calibrator trained on rosters composed from who actually played
rather than who was projected to (#360). This turns that audit into a test
that runs on every retrain.

How it works. For a target season S, a schema `lookahead_guard` is created
holding one VIEW per season-scoped table, and the frame is built a second
time with `search_path = lookahead_guard, public` so every unqualified table
name resolves to its view:

  - a table the frame may not read at season S is HIDDEN there
    (`WHERE season <> S`) — a join at the target season then finds nothing,
    the feature it fed goes NULL or sentinel, and the frames differ;
  - a table the frame legitimately reads at season S — for the TARGET and
    for the columns that decide which rows exist at all — keeps those
    columns and has every other column NULLED at season S (`ALLOWED`
    below). A trajectory row needs the player's season-S CamPom (target),
    his season-S team (destination, replaced by the portal commitment at
    serve) and the qualification gate; it must not see his season-S
    archetype, box score, on/off or team results.

The two frames are then compared on the rows they share, keyed by each
frame's natural key: every feature column byte-identical (NaN == NaN), or
the guard fails naming the columns that moved. Rows present only in the
unmasked build are reported, not failed — a row that needs season S to
exist (a player with no season-S stats is not a training row) is target
selection, which the trainers document, not look-ahead in a feature.

Both Python frames (`train_trajectory_model`, `train_freshman_model`) and the
Rust-cut calibrator frame (`projections-backtest --frame-only`, run with the
masked search_path in `DATABASE_URL`) are covered. The schema is created and
dropped around the run; it holds views only, never data, and a crash leaves
nothing behind that the next run does not drop first.

Two things it will not catch, by design: a leak through a table with no
`season` column (none of the frame inputs are such), and a leak through the
allowed columns themselves — those are the target and the row key, and a
model that read them as features would show up in the feature list.

Run:  cd training && ./.venv/bin/python lookahead_guard.py [--seasons 2026] [--frames trajectory,freshman,calibrator]
Exit 1 on any feature drift.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import create_engine, text

import db

SCHEMA = "lookahead_guard"
REPO_ROOT = Path(__file__).resolve().parent.parent
FRAMES = ("trajectory", "freshman", "calibrator")

# Columns a frame may read at the TARGET season: the target itself, and what
# decides which rows exist and which team they belong to. Everything else in
# these tables — and every column of every other season-scoped table — is
# invisible at season S. Keys are (table, season column).
ALLOWED: dict[str, tuple[str, tuple[str, ...]]] = {
    # Layer 1 target; player_id / torvik_pid identify the row.
    "torvik_player_stats": ("season", ("torvik_pid", "player_id", "season", "cam_gbpm_v3_psos")),
    # Row existence + the qualification gate + the destination team.
    "player_season_stats": ("season", ("player_id", "team_id", "season", "games_played", "minutes_per_game")),
    # Layer 2 targets (net, AdjO; AdjD for the O/D experiment).
    "team_season_stats": ("season", ("team_id", "season", "adj_efficiency_margin", "adj_offense", "adj_defense")),
    # Program identity across season-scoped UUIDs; name/conference are not.
    "teams": ("season", ("id", "natstat_id", "season")),
}
# Season-scoped tables the guard never touches: their season-S rows are
# predictions FOR season S made from earlier data (Layer 1 output, the
# calibrator's input) — ex-ante by construction.
EXEMPT = {"trajectory_oof_predictions", "freshman_oof_predictions"}


def _season_tables(conn) -> list[str]:
    rows = conn.execute(text(
        "SELECT table_name FROM information_schema.columns "
        "WHERE table_schema = 'public' AND column_name = 'season' ORDER BY table_name"
    )).fetchall()
    return [r[0] for r in rows if r[0] not in EXEMPT and not r[0].startswith("_")]


def _columns(conn, table: str) -> list[str]:
    return [r[0] for r in conn.execute(text(
        "SELECT column_name FROM information_schema.columns "
        "WHERE table_schema = 'public' AND table_name = :t ORDER BY ordinal_position"
    ), {"t": table}).fetchall()]


@contextmanager
def masked_schema(season: int):
    """Create the view schema for `season`, yield the masked DATABASE_URL,
    drop the schema afterwards whatever happens."""
    engine = create_engine(db.DATABASE_URL)
    with engine.begin() as conn:
        conn.execute(text(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE"))
        conn.execute(text(f"CREATE SCHEMA {SCHEMA}"))
        # sqlx's migrator does `CREATE TABLE IF NOT EXISTS _sqlx_migrations`
        # in the first schema on the path; a view of the real ledger keeps it
        # reading "everything applied" instead of re-running the migrations
        # against the views.
        conn.execute(text(f"CREATE VIEW {SCHEMA}._sqlx_migrations AS SELECT * FROM public._sqlx_migrations"))
        hidden, nulled = [], []
        for table in _season_tables(conn):
            cols = _columns(conn, table)
            if table in ALLOWED:
                season_col, keep = ALLOWED[table]
                select = ", ".join(
                    c if c in keep else f"CASE WHEN {season_col} = {season} THEN NULL ELSE {c} END AS {c}"
                    for c in cols
                )
                conn.execute(text(f"CREATE VIEW {SCHEMA}.{table} AS SELECT {select} FROM public.{table}"))
                nulled.append(table)
            else:
                conn.execute(text(
                    f"CREATE VIEW {SCHEMA}.{table} AS SELECT * FROM public.{table} "
                    f"WHERE season IS DISTINCT FROM {season}"
                ))
                hidden.append(table)
    print(f"  masked season {season}: {len(hidden)} tables hidden, {len(nulled)} tables nulled outside "
          f"{{{', '.join(nulled)}}} allowed columns")
    sep = "&" if "?" in db.DATABASE_URL else "?"
    masked_url = f"{db.DATABASE_URL}{sep}options=-c%20search_path%3D{SCHEMA},public"
    try:
        yield masked_url
    finally:
        with engine.begin() as conn:
            conn.execute(text(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE"))
        engine.dispose()


@contextmanager
def _database_url(url: str):
    """Point every `db.get_engine()` call at `url` for the duration."""
    saved = db.DATABASE_URL
    db.DATABASE_URL = url
    try:
        yield
    finally:
        db.DATABASE_URL = saved


# ------------------------------------------------------------ frame builders
def _build_python(module: str) -> tuple[pd.DataFrame, list[str], list[str]]:
    """(frame, feature columns, natural key) for a Layer 1 trainer."""
    if module == "trajectory":
        import train_trajectory_model as T
        df = T.build_dataset().reset_index(drop=True)
        return df, list(T.FEATURE_COLS), ["torvik_pid", "s_n", "s_np1"]
    import train_freshman_model as F
    df = F.build_dataset().reset_index(drop=True)
    return df, list(F.FEATURE_COLS), ["cstat_player_id", "recruit_year"]


def _build_calibrator(url: str, seasons: list[int]) -> tuple[pd.DataFrame, list[str], list[str]]:
    """Cut the ex-ante calibrator frame with the Rust backtest against `url`."""
    binary = REPO_ROOT / "target" / "release" / "cstat-ingest"
    if not binary.exists():
        raise SystemExit(f"{binary} missing — `cargo build --release -p cstat-ingest` first")
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tmp:
        out = Path(tmp.name)
    env = dict(os.environ, DATABASE_URL=url, MODEL_DIR=str(REPO_ROOT / "training" / "models"))
    r = subprocess.run(
        [str(binary), "projections-backtest", "--years", ",".join(map(str, seasons)),
         "--frame-out", str(out), "--frame-only"],
        cwd=REPO_ROOT, env=env, capture_output=True, text=True,
    )
    if r.returncode != 0:
        raise SystemExit(f"frame cut failed:\n{r.stderr[-2000:]}")
    raw = json.loads(out.read_text())
    out.unlink()
    cols = list(raw["feature_names"])
    df = pd.DataFrame([r["features"] for r in raw["rows"]], columns=cols, dtype="float64")
    df.insert(0, "team_id", [str(r["team_id"]) for r in raw["rows"]])
    df.insert(1, "season", [int(r["season"]) for r in raw["rows"]])
    return df, cols, ["team_id", "season"]


# ----------------------------------------------------------------- compare
def compare(base: pd.DataFrame, masked: pd.DataFrame, cols: list[str], key: list[str], season: int) -> dict:
    """Feature drift on shared rows targeting `season`, plus row accounting."""
    b = base[base[_season_col(base, key)] == season].set_index(key)
    m = masked[masked[_season_col(masked, key)] == season].set_index(key)
    shared = b.index.intersection(m.index)
    drift: dict[str, int] = {}
    for c in cols:
        x = b.loc[shared, c].to_numpy(dtype="float64")
        y = m.loc[shared, c].to_numpy(dtype="float64")
        n = int((~((x == y) | (np.isnan(x) & np.isnan(y)))).sum())
        if n:
            drift[c] = n
    return {
        "season": season,
        "rows_unmasked": int(len(b)),
        "rows_masked": int(len(m)),
        "rows_shared": int(len(shared)),
        "rows_only_unmasked": int(len(b.index.difference(m.index))),
        "rows_only_masked": int(len(m.index.difference(b.index))),
        "drift_columns": drift,
    }


def _season_col(df: pd.DataFrame, key: list[str]) -> str:
    # The target season is the last key for the two Layer 1 frames
    # (`s_np1`; `recruit_year` + 1 for freshmen) and `season` for the calibrator.
    if "s_np1" in df.columns:
        return "s_np1"
    if "recruit_year" in df.columns:
        if "_target_season" not in df.columns:
            df["_target_season"] = df["recruit_year"].astype(int) + 1
        return "_target_season"
    return "season"


def run(frames: list[str], seasons: list[int]) -> tuple[dict, bool]:
    report: dict = {"frames": {}}
    ok = True
    calib_seasons = list(range(2016, max(seasons) + 1))
    print("unmasked frames …")
    base: dict[str, tuple] = {}
    for f in frames:
        base[f] = _build_calibrator(db.DATABASE_URL, calib_seasons) if f == "calibrator" else _build_python(f)
        print(f"  {f}: {len(base[f][0]):,} rows, {len(base[f][1])} features")
    for season in seasons:
        print(f"\nmasked build, target season {season} …")
        with masked_schema(season) as masked_url:
            for f in frames:
                if f == "calibrator":
                    masked = _build_calibrator(masked_url, calib_seasons)
                else:
                    with _database_url(masked_url):
                        masked = _build_python(f)
                df_b, cols, key = base[f]
                res = compare(df_b, masked[0], cols, key, season)
                report["frames"].setdefault(f, []).append(res)
                verdict = "OK " if not res["drift_columns"] else "LEAK"
                if res["drift_columns"]:
                    ok = False
                print(f"  {verdict} {f:<11} S={season}: {res['rows_shared']:,} shared rows identical on "
                      f"{len(cols)} features"
                      + (f"; {res['rows_only_unmasked']:,} rows need season {season} to exist (target selection)"
                         if res["rows_only_unmasked"] else "")
                      + (f"; DRIFT in {res['drift_columns']}" if res["drift_columns"] else ""))
    return report, ok


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seasons", default="2026", help="target seasons to mask, comma-separated")
    ap.add_argument("--frames", default=",".join(FRAMES))
    ap.add_argument("--out", type=Path, default=None, help="write the JSON report here")
    args = ap.parse_args()
    seasons = [int(s) for s in args.seasons.split(",")]
    frames = [f for f in args.frames.split(",") if f]
    unknown = set(frames) - set(FRAMES)
    if unknown:
        raise SystemExit(f"unknown frames {unknown}; choose from {FRAMES}")
    report, ok = run(frames, seasons)
    if args.out:
        args.out.write_text(json.dumps(report, indent=2))
        print(f"\nwrote {args.out}")
    print("\n" + ("✓ no feature reads the target season" if ok else "✗ LOOK-AHEAD: a feature changed when the target season was hidden"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
