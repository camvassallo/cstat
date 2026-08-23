"""The served projection blend, mirrored once for every Python consumer.

`cstat_core::roster_projection` is the contract; this is the mirror. It exists
because the formula has now changed three times (a flat 0.50, then a
turnover-conditional ramp in #322, then a program-anchored baseline in #325)
and every change left a diagnostic behind scoring a projection nobody is
served: `pit_program_calibration.py` was still reconstructing
`0.50·baseline + 0.50·roster_proj` two regimes after that stopped being true,
which silently turns "measured against served" into "measured against a blend
that has not existed since June".

Consumers should call `served_prediction(row)` on a backtest-dump row rather
than assembling the blend themselves. Rows from dumps written before #322/#325
lack `retained` / `program_level`; both degrade to the older behaviour rather
than crashing, and `unverified_rows` reports rows this mirror cannot be confirmed
against — the signal that this file is stale against the Rust, or that the
dump predates the blend it describes.
"""

from __future__ import annotations

# --- mirrored from crates/cstat-core/src/roster_projection.rs ---------------
W_STABLE = 0.70                  # PROJECTION_SHRINK_WEIGHT
W_OVERHAUL = 0.55                # PROJECTION_SHRINK_WEIGHT_OVERHAUL
RETAINED_FULL_OVERHAUL = 0.20    # OVERHAUL_RETAINED_FULL
RETAINED_FULL_STABLE = 0.40      # STABLE_RETAINED_FULL
PROGRAM_ANCHOR_SHRINK = 1.0      # PROGRAM_ANCHOR_SHRINK
OFFSET = 0.0                     # PROJECTION_OFFSET


def served_weight(retained: float | None) -> float:
    """Baseline weight the ramp gives a roster with this retained-talent fraction."""
    if retained is None or retained >= RETAINED_FULL_STABLE:
        return W_STABLE
    if retained <= RETAINED_FULL_OVERHAUL:
        return W_OVERHAUL
    t = ((retained - RETAINED_FULL_OVERHAUL)
         / (RETAINED_FULL_STABLE - RETAINED_FULL_OVERHAUL))
    return W_OVERHAUL + t * (W_STABLE - W_OVERHAUL)


def program_anchor(baseline: float, program_level: float | None,
                   roster_proj: float) -> float:
    """Last season shrunk toward the program's multi-season level by the part
    of the move this year's roster does not corroborate (#325)."""
    if program_level is None:
        return baseline
    dev = baseline - program_level
    if abs(dev) < 1e-3:
        return baseline
    corroboration = (roster_proj - program_level) / dev
    uncorroborated = min(max(1.0 - corroboration, 0.0), 1.0)
    return baseline - PROGRAM_ANCHOR_SHRINK * uncorroborated * dev


def blend(row: dict, w: float) -> float:
    """The blend at an ARBITRARY baseline weight — for weight sweeps. Still
    anchored, because the anchor is not part of the weight being swept."""
    anchor = program_anchor(float(row["baseline"]), row.get("program_level"),
                            float(row["roster_proj"]))
    return w * anchor + (1.0 - w) * float(row["roster_proj"]) + OFFSET


def served_prediction(row: dict) -> float:
    """What the serving path would produce for this backtest-dump row."""
    return blend(row, served_weight(row.get("retained")))


def unverified_rows(rows: list[dict], tol: float = 1e-4) -> int:
    """Rows this mirror could **not be confirmed against**.

    Counts two things deliberately, because both mean the same thing to a
    caller — the numbers below are not the served projection:

    1. rows whose dumped `baseline_weight` disagrees with `served_weight`
       (the mirror has drifted from roster_projection.rs), and
    2. rows carrying no `baseline_weight` at all (a dump written before #322,
       so there is nothing to check and the older served formula applied).

    Counting only (1) is a trap, and the first version of this function fell
    into it: a pre-#322 dump has none of these fields, scored 0 mismatches,
    and reported "verified" having checked nothing — while `load_backtest`
    picks its default dump by FILENAME, which is exactly how a superseded one
    gets selected in the first place. A guard that says all-clear when it
    cannot check anything is worse than no guard."""
    n = 0
    for r in rows:
        served = r.get("baseline_weight")
        if served is None:
            n += 1
            continue
        if abs(float(served) - served_weight(r.get("retained"))) > tol:
            n += 1
    return n
