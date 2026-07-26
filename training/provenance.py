"""Input fingerprints — the provenance chain across the model dependency tree.

Why this exists (issue #223). The models form a dependency tree, and the
failure mode is not bad data, it is **desynchronization between layers**:
Layer 2 is calibrated against a specific version of Layer 1's error profile,
so when Layer 1 is regenerated and Layer 2 is not, the calibrator corrects for
a bias that no longer exists. Nothing errors — the feature contract never
changed. That is #218, where `roster_adjo` served an OOF three generations
stale for months.

`oof_provenance.py` (#218) closed exactly one edge of that tree, Layer 1 ->
Layer 2, by stamping both Layer 2 metas with a fingerprint of the OOF tables
as they were at training time. This module generalizes the same mechanism to
every edge: each trainer records a fingerprint of *its own* inputs, so "is this
model current?" becomes a query against the database rather than something a
human has to remember. `check_provenance.py` is that query.

## What a fingerprint is

Per source: a row count plus an order-stable md5 over the identity columns and
the value columns **the consumer actually reads**. Three exclusions are
deliberate, carried over from `oof_provenance.py`:

- **Only the consumed value columns.** The roster frame reads `mean`, not
  `lower`/`upper`, so a band-only regen must not trip a mismatch it cannot have
  caused. Fingerprinting a superset of what a model consumes manufactures false
  staleness; fingerprinting a subset hides real staleness. Match consumption.
- **`created_at`/`updated_at` excluded.** A regen that reproduces identical
  values IS the same snapshot for training purposes. Stamping the clock would
  flag a deterministic re-run as drift, which — now that the trainers are
  reproducible (#222) — is precisely the case worth being able to prove.
- **Numeric values rounded to 6dp** so REAL-to-text formatting cannot make an
  identical snapshot hash differently.

## Per-season sub-digests, and why they are not an optimization

Cost was the open question on #223 and it is a non-issue: the whole-table md5
over `torvik_player_stats` (58k rows) measures 188ms, `player_archetypes` 65ms.
No per-season rollup is needed to make this cheap.

The sub-digests exist for **resolution**, which is a correctness property of
the report rather than a performance one. `compute_all` rewrites
`cam_gbpm_v3_psos` for the live season every nightly, and the Rust assign half
rewrites `player_archetypes` nightly too — while the Layer 1 training window
includes the in-progress season. A whole-table digest is therefore genuinely
different every morning in-season, and a check that prints `trajectory STALE`
150 nights a year is a check people learn to scroll past. That is the same
ignored-warning failure this tree already produced once.

Splitting the digest by season lets the report separate "the in-progress season
moved, as it does every night" from "a *closed* season moved, which means a
recompute or a swap-repair (#140/#201) silently invalidated Layer 1." Only the
second one is drift. See `check_provenance.py::classify`.

## Compatibility with the #218 stamp

`oof_provenance()` remains the boot contract: `Predictor::load` hard-fails when
the two Layer 2 halves disagree, and every committed meta carries digests
computed by the original construction. The general form here reduces **exactly**
to that construction for a single-value, two-key source, so the digests are
byte-identical and no committed model is invalidated by this module landing.
`training/test_provenance.py::test_oof_digest_matches_the_218_construction`
pins that equivalence — if you change `_digest_sql`, that test is the one that
tells you the API will refuse to boot.
"""
from __future__ import annotations

import datetime as _dt
from dataclasses import dataclass

from sqlalchemy import text

from db import get_engine

# Sentinel substituted for a NULL value column. Without it, `||` propagates the
# NULL through the whole row expression and `string_agg` skips the row outright
# — so a table whose values went NULL would hash identically to one where those
# rows were deleted. Not a hypothetical: `player_on_off.net_on_off` is NULL for
# every player without an OFF sample, and `prior_secondary_class` is NULL on
# unlabelled rows.
_NULL = "~"


@dataclass(frozen=True)
class Source:
    """One fingerprintable input: a table, its identity, and what is read.

    `keys` doubles as the sort order inside `string_agg`, so it must determine
    a row — otherwise two runs over identical data can hash differently. Use a
    natural unique key, never the surrogate `id`: a re-ingest that reproduces
    the same values can renumber `id` and would then look like drift.
    """

    table: str
    keys: tuple[str, ...]
    #: Numeric value columns, rounded to 6dp before hashing.
    values: tuple[str, ...] = ()
    #: Value columns hashed as text verbatim (class labels, categoricals).
    text_values: tuple[str, ...] = ()
    #: Column to split sub-digests on. Required for anything season-scoped —
    #: without it the report cannot tell nightly churn from real drift.
    season_column: str | None = None
    #: Restricts the scan to the rows the consumer actually reads.
    where: str | None = None
    #: True when `compute_all` rewrites this table for the live season every
    #: nightly. Only these sources earn the in-progress-season churn exemption
    #: in `check_provenance.py` — a change to `recruits`, which no nightly
    #: touches, is a real edit whatever the calendar says.
    nightly: bool = False
    notes: str = ""


# The registry. Names are stable identifiers written into model metas and read
# back by `check_provenance.py`; renaming one silently orphans every stamp that
# carries the old name, so treat them as a wire format.
SOURCES: dict[str, Source] = {
    # ---- Layer 1 output / Layer 2 input -------------------------------
    # `mean` only: `build_dataset` COALESCEs the mean into the projected cam
    # channel and never reads the q10/q90 band.
    "trajectory_oof_predictions": Source(
        table="trajectory_oof_predictions",
        keys=("torvik_pid", "target_season"),
        values=("mean",),
        season_column="target_season",
        notes="Layer 1 held-out returner projections; the roster frame's returner channel.",
    ),
    "freshman_oof_predictions": Source(
        table="freshman_oof_predictions",
        keys=("cstat_player_id", "target_season"),
        values=("mean",),
        season_column="target_season",
        notes="Layer 1 held-out recruit projections; the roster frame's newcomer channel.",
    ),
    # ---- Layer 0 -------------------------------------------------------
    # The value currency. Split from the GBPM components below because the
    # freshman trainer reads only this column, and a source it does not consume
    # must not be able to mark it stale.
    "torvik_player_stats.cam_v3": Source(
        table="torvik_player_stats",
        keys=("torvik_pid", "season"),
        values=("cam_gbpm_v3_psos",),
        season_column="season",
        # Matches the trainers' own `torvik_pid IS NOT NULL` gate. Rows without
        # a pid cannot join cross-season, so they reach no model here, and
        # including them would also break the key's row-determining property.
        where="torvik_pid IS NOT NULL",
        nightly=True,
        notes="CamPom — the value currency every model downstream is denominated in.",
    ),
    "torvik_player_stats.gbpm": Source(
        table="torvik_player_stats",
        keys=("torvik_pid", "season"),
        values=("ogbpm", "dgbpm", "gbpm"),
        season_column="season",
        where="torvik_pid IS NOT NULL",
        nightly=True,
        notes="Raw GBPM components; trajectory prior-season features.",
    ),
    "player_archetypes": Source(
        table="player_archetypes",
        keys=("player_id", "season"),
        text_values=("primary_class", "secondary_class"),
        season_column="season",
        nightly=True,
        notes="ASSIGN half. Class labels only — the trainers read the mixture, not the scores.",
    ),
    "player_season_stats": Source(
        table="player_season_stats",
        keys=("player_id", "team_id", "season"),
        values=(
            # Qualification gate (>=5 GP / >=5 MPG) — changes here move the row
            # set, not just feature values.
            "games_played",
            "minutes_per_game",
            # Box per-game
            "ppg", "rpg", "apg", "spg", "bpg", "topg",
            # Rate stats
            "true_shooting_pct", "effective_fg_pct", "usage_rate",
            "ast_pct", "tov_pct", "orb_pct", "drb_pct",
            "stl_pct", "blk_pct", "ft_rate",
        ),
        season_column="season",
        nightly=True,
        notes="Trajectory box/rate features, and the gate that decides the row set.",
    ),
    "player_on_off": Source(
        table="player_on_off",
        keys=("season", "player_id"),
        values=(
            "on_net_rtg",
            "net_on_off",
            "on_possessions_for",
            "off_possessions_for",
        ),
        season_column="season",
        nightly=True,
        notes="Tier-2 membership features (3 of the trajectory model's 60).",
    ),
    "team_season_stats.adj": Source(
        table="team_season_stats",
        keys=("team_id", "season"),
        values=("adj_efficiency_margin", "adj_offense"),
        season_column="season",
        nightly=True,
        notes="Layer 2 targets; also the freshman model's signing-team prior.",
    ),
    "recruits": Source(
        table="recruits",
        keys=("year", "recruit_key"),
        values=(
            "composite_rank",
            "composite_rating",
            "star_rating",
            "position_rank",
            "previous_rank",
            "weight",
        ),
        text_values=("height", "position", "cstat_player_id", "committed_team_id"),
        season_column="year",
        notes="247 recruit ratings; the freshman model's entire feature block.",
    ),
}

# ---- Per-node input declarations ---------------------------------------
# What each trainer consumes, by source name. This is the edge list of the
# dependency graph in `docs/model_dependency_graph.md`, in executable form:
# `check_provenance.py` walks it to propagate staleness downward, and each
# trainer stamps exactly its own entry.
NODE_INPUTS: dict[str, tuple[str, ...]] = {
    "trajectory": (
        "torvik_player_stats.cam_v3",
        "torvik_player_stats.gbpm",
        "player_season_stats",
        "player_archetypes",
        "player_on_off",
        "recruits",
    ),
    "freshman": (
        "torvik_player_stats.cam_v3",
        "player_season_stats",
        "team_season_stats.adj",
        "recruits",
    ),
    "roster_impact": (
        "trajectory_oof_predictions",
        "freshman_oof_predictions",
        "torvik_player_stats.cam_v3",
        "player_archetypes",
        "team_season_stats.adj",
    ),
    "roster_adjo": (
        "trajectory_oof_predictions",
        "freshman_oof_predictions",
        "torvik_player_stats.cam_v3",
        "player_archetypes",
        "team_season_stats.adj",
    ),
}

#: Which model meta on disk carries each node's stamp.
NODE_META_FILES: dict[str, str] = {
    "trajectory": "trajectory_model_meta.json",
    "freshman": "freshman_model_meta.json",
    "roster_impact": "roster_impact_model_meta.json",
    "roster_adjo": "roster_adjo_model_meta.json",
}

#: Layer 1 writes the OOF tables Layer 2 trains on. Used to explain a Layer 2
#: mismatch as "upstream moved" rather than reporting it as an unrelated node.
NODE_UPSTREAM: dict[str, tuple[str, ...]] = {
    "trajectory": (),
    "freshman": (),
    "roster_impact": ("trajectory", "freshman"),
    "roster_adjo": ("trajectory", "freshman"),
}

_OOF_SOURCES = ("trajectory_oof_predictions", "freshman_oof_predictions")


def _value_exprs(src: Source) -> list[str]:
    """SQL text for each consumed value, NULL-safe and rounded."""
    out = [
        f"coalesce(round({c}::numeric, 6)::text, '{_NULL}')" for c in src.values
    ]
    out += [f"coalesce({c}::text, '{_NULL}')" for c in src.text_values]
    return out


def _row_expr(src: Source) -> str:
    """The per-row string hashed into the digest: identity then values.

    For a two-key single-value source this is character-for-character the
    expression `oof_provenance.py` has always used, which is what keeps the
    committed Layer 2 stamps valid.
    """
    parts = [f"{k}::text" for k in src.keys] + _value_exprs(src)
    return " || ':' || ".join(parts)


def _digest_sql(src: Source, group_by_season: bool) -> str:
    row = _row_expr(src)
    order = ", ".join(src.keys)
    where = f" WHERE {src.where}" if src.where else ""
    season = src.season_column
    select = f"{season} AS season, " if group_by_season else "NULL::int AS season, "
    group = f" GROUP BY {season}" if group_by_season else ""
    return (
        f"SELECT {select}count(*) AS n, "
        f"md5(coalesce(string_agg({row}, ',' ORDER BY {order}), '')) AS digest "
        f"FROM {src.table}{where}{group}"
    )


def fingerprint(names: tuple[str, ...] | list[str], conn=None) -> dict:
    """Fingerprint each named source against the live database.

    Returns `{name: {"n_rows": int, "digest": str, "by_season": {season: …}}}`,
    a plain dict safe to embed in a model meta and compare verbatim against
    another stamp.
    """
    own = conn is None
    conn = get_engine().connect() if own else conn
    try:
        stamp: dict = {}
        for name in names:
            src = SOURCES[name]
            total = conn.execute(text(_digest_sql(src, group_by_season=False))).one()
            entry: dict = {"n_rows": int(total.n), "digest": total.digest}
            if src.season_column:
                rows = conn.execute(
                    text(_digest_sql(src, group_by_season=True))
                ).fetchall()
                entry["by_season"] = {
                    str(r.season): {"n_rows": int(r.n), "digest": r.digest}
                    for r in sorted(rows, key=lambda r: r.season)
                }
            stamp[name] = entry
        return stamp
    finally:
        if own:
            conn.close()


def input_provenance(node: str) -> dict:
    """The `input_provenance` block a trainer stamps into its meta.

    Call this **immediately after the frame is read**, not at meta-write time.
    The stamp is a claim about what the model trained on, so it has to be taken
    adjacent to the read it describes. Taken after the fit instead, a retrain
    that overlaps a nightly `compute_all` would stamp the post-compute snapshot
    onto a model built from the pre-compute one — and `check_provenance.py`
    would then compare that stamp to a matching database and report CURRENT.
    A false "current" is strictly worse than a false "stale" here: it is the
    exact silence this chain exists to break.
    """
    stamp = fingerprint(NODE_INPUTS[node])
    print(f"  input provenance ({node}):")
    for name, e in stamp.items():
        print(f"    {name:38s} n={e['n_rows']:>7,}  md5={e['digest'][:12]}…")
    return stamp


def oof_provenance_from(stamp: dict) -> dict:
    """Project an `input_provenance` block down to the #218 boot stamp.

    The Rust boot guard compares `oof_provenance` between the two Layer 2
    halves and treats a missing block as a hard failure. Deriving it from the
    general stamp rather than recomputing it means the two can never disagree
    about the same run — and stripping `by_season` keeps the block
    byte-identical to what `oof_provenance.py` has always written, so no
    committed model meta is invalidated.
    """
    return {
        name: {"n_rows": stamp[name]["n_rows"], "digest": stamp[name]["digest"]}
        for name in _OOF_SOURCES
        if name in stamp
    }


def season_for_date(today: _dt.date) -> int:
    """Mirror of `cstat_ingest::season_for_date` (crates/cstat-ingest/src/lib.rs)."""
    return today.year + 1 if today.month >= 11 else today.year


def in_season(today: _dt.date) -> bool:
    """Mirror of `cstat_ingest::in_season_now` — Nov–Mar, plus Apr 1–15."""
    return today.month >= 11 or today.month <= 3 or (today.month == 4 and today.day <= 15)


def mutable_season(today: _dt.date | None = None) -> int | None:
    """The one season Layer 0 is still rewriting nightly, if any.

    Returns `None` in the offseason: the nightly is not appending games, so
    every season is final and *any* digest change is real drift worth
    reporting. In-season, the current season churns by design and a change
    there is not evidence of anything.
    """
    today = today or _dt.date.today()
    return season_for_date(today) if in_season(today) else None


if __name__ == "__main__":
    import json
    import sys

    names = sys.argv[1:] or list(SOURCES)
    print(json.dumps(fingerprint(tuple(names)), indent=2))
