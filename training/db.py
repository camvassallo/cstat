"""Database connection utilities for the training pipeline."""

import os

import pandas as pd
from sqlalchemy import create_engine

DATABASE_URL = os.getenv(
    "DATABASE_URL",
    "postgresql://cstat:cstat@localhost:5432/cstat",
)

# SQLAlchemy 1.4+ rejects the bare `postgres://` scheme; the rest of the
# repo (Rust sqlx, the Heroku-style .env.example) uses it freely.
if DATABASE_URL.startswith("postgres://"):
    DATABASE_URL = "postgresql://" + DATABASE_URL[len("postgres://"):]


def get_engine():
    return create_engine(DATABASE_URL)


def canonical_frame_order(df: pd.DataFrame) -> pd.DataFrame:
    """Put a training frame in a content-determined row order (issue #222).

    Every trainer's LightGBM params use `bagging_fraction`, which subsamples
    by row *position*. Postgres makes no ordering guarantee without an
    `ORDER BY`, so an unordered read silently produced a different model on
    every run — measured at up to 1.02 AdjEM of movement on the served
    preseason projection for a retrain against unchanged data.

    The trainers' queries do carry an `ORDER BY` on their natural key, which
    does the bulk of the work in the database. That alone is **not provable**
    though: `player_season_stats` is unique on `(player_id, team_id, season)`
    and `recruits` can hold more than one row per player, so several joins
    fan out and the natural key does not determine a row. Empirically both
    Layer 1 queries stayed unstable after ordering on their keys and again
    after adding the team ids.

    Sorting on the full column tuple is the version that *is* provable: two
    rows that compare equal across every selected column are byte-identical,
    so their relative order cannot affect the frame's content — which makes
    the result a pure function of the query result set, whatever the join
    fan-out does. Cheap at these sizes (~25k rows), and it stays correct if
    someone later edits a query and introduces a new fan-out.

    `kind="stable"` is required: pandas defaults to an unstable quicksort.
    """
    if df.empty:
        return df.reset_index(drop=True)
    return df.sort_values(
        list(df.columns), kind="stable", na_position="last"
    ).reset_index(drop=True)
