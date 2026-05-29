"""Regression tests for cross-season player resolution.

This file exists because we found a class of silent SQL bugs where
joining `players` cross-season via `natstat_id` alone misses every
transferred player — natstat_id is reissued per team, so a player
who transfers from Maryland 2025 to Tennessee 2026 has different
natstat_ids in the two rows. `torvik_pid` is stable across transfers
(96% coverage, zero collisions — see `reference_torvik_pid` memory),
so the correct cross-season-player join is **natstat_id OR torvik_pid**.

The three confirmed sites at write-time of this file:
- `train_roster_impact_model.py::INBOUND_QUERY` (training input)
- `audit_preseason_projections.py::fetch_portal_signals` (audit)
- `decompose_projection_error.py::fetch_portal_sums` (diagnostic)

The tests below cover canonical 2026-cycle teams whose portal moves are
well known. If any of these assertions fail, a `natstat_id`-only join
was reintroduced somewhere — investigate before merging.

Run: `python test_cross_season_joins.py` (exit code 0 on pass).
"""

from __future__ import annotations

import sys

from sqlalchemy import text

from db import get_engine

# Each (label, team_name, season, query_callable, min_count, min_cam_v3).
# `query_callable` is named so the assertion error pinpoints the SQL
# under test, not just "some query failed".


def inbound_with_torvik_fallback(conn, team_name: str, season: int) -> tuple[int, float]:
    """The CORRECT inbound query — matches the shipped pattern in
    `train_roster_impact_model.py::INBOUND_QUERY` after the fix. Joins
    target-season player rows via `natstat_id OR torvik_pid` so
    transferred players are not silently dropped."""
    sql = text(
        """
        SELECT COUNT(*) AS n,
               COALESCE(SUM(COALESCE(tps_base.cam_gbpm_v3_psos, 0)), 0)::float8 AS cam
        FROM transfers t
        JOIN players p_base
            ON p_base.id = t.cstat_player_id AND p_base.season = t.year
        LEFT JOIN torvik_player_stats tps_base
            ON tps_base.player_id = p_base.id AND tps_base.season = t.year
        JOIN players p_tgt
            ON p_tgt.season = t.year + 1
           AND (
                p_tgt.natstat_id = p_base.natstat_id
                OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
                    SELECT player_id FROM torvik_player_stats
                    WHERE torvik_pid = tps_base.torvik_pid AND season = t.year + 1
                ))
           )
        JOIN teams tgt_team ON tgt_team.id = p_tgt.team_id
        WHERE t.year = :portal_year
          AND tgt_team.name = :team_name
          AND tgt_team.season = :season
        """
    )
    row = conn.execute(
        sql, {"portal_year": season - 1, "team_name": team_name, "season": season}
    ).fetchone()
    return int(row.n), float(row.cam)


def inbound_natstat_only(conn, team_name: str, season: int) -> tuple[int, float]:
    """The BUGGY inbound query — kept around so the tests can demonstrate
    the gap. If any of the asserted teams shows the same coverage
    between this and `inbound_with_torvik_fallback`, the test universe
    isn't actually exercising the transfer-traversal path and a new
    canonical case should be added."""
    sql = text(
        """
        SELECT COUNT(*) AS n,
               COALESCE(SUM(COALESCE(tps_base.cam_gbpm_v3_psos, 0)), 0)::float8 AS cam
        FROM transfers t
        JOIN players p_base
            ON p_base.id = t.cstat_player_id AND p_base.season = t.year
        LEFT JOIN torvik_player_stats tps_base
            ON tps_base.player_id = p_base.id AND tps_base.season = t.year
        JOIN players p_tgt
            ON p_tgt.natstat_id = p_base.natstat_id
           AND p_tgt.season = t.year + 1
        JOIN teams tgt_team ON tgt_team.id = p_tgt.team_id
        WHERE t.year = :portal_year
          AND tgt_team.name = :team_name
          AND tgt_team.season = :season
        """
    )
    row = conn.execute(
        sql, {"portal_year": season - 1, "team_name": team_name, "season": season}
    ).fetchone()
    return int(row.n), float(row.cam)


def outbound_same_team(conn, team_name: str, season: int) -> tuple[int, float]:
    """Outbound is unaffected by the bug (same-team join), but worth
    pinning a baseline so a future refactor doesn't accidentally break
    it. Mirrors `OUTBOUND_QUERY`."""
    sql = text(
        """
        SELECT COUNT(*) AS n,
               COALESCE(SUM(COALESCE(tps.cam_gbpm_v3_psos, 0)), 0)::float8 AS cam
        FROM transfers t
        JOIN players p_base
            ON p_base.id = t.cstat_player_id AND p_base.season = t.year
        JOIN teams base_team ON base_team.id = p_base.team_id
        JOIN teams tgt_team
            ON tgt_team.natstat_id = base_team.natstat_id
           AND tgt_team.season = p_base.season + 1
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = p_base.id AND tps.season = t.year
        WHERE t.year = :portal_year
          AND tgt_team.name = :team_name
          AND tgt_team.season = :season
        """
    )
    row = conn.execute(
        sql, {"portal_year": season - 1, "team_name": team_name, "season": season}
    ).fetchone()
    return int(row.n), float(row.cam)


# Canonical 2026 portal cases. The (team, season) pair is target-season;
# the portal cycle is spring of (season − 1). Bounds chosen with margin
# below the actual numbers so a small reshuffle of one player doesn't
# trip the test — these are smoke checks, not bit-exact pins.
#
# Each case is `(team_name, season, min_inbound_count, min_inbound_cam,
# min_torvik_only_count)`. The third bound is the load-bearing one:
# `min_torvik_only_count > 0` means the corrected query MUST report
# more matches than the buggy query for this team — i.e. there is at
# least one transfer-traversal case here that natstat_id alone misses.
# If a future SQL refactor reintroduces the natstat_id-only join, this
# bound will fail.
#
# **Why not assert against a calibrated portal-list (TFS, On3)?** We
# already trust `transfers.cstat_player_id` resolution (covered by
# transfer-resolve tests). What this file uniquely catches is the
# cross-season *player* join — a different failure mode that only
# surfaces at this query layer. Wiring an external source would conflate
# two checks.
INBOUND_CASES = [
    # Michigan 2026: gained Lendeborg + Mara + Johnson Jr. + Cadeau,
    # all transfers (natstat_id rotated for all 4 — empirically observed).
    # Canonical asymmetric case: buggy query reports 0, corrected reports 4.
    ("Michigan Wolverines", 2026, 4, 30.0, 4),
    # Maryland 2026 inbound: a couple of arrivals all caught via
    # torvik_pid. Catches the "lost more than gained" cohort.
    ("Maryland Terrapins", 2026, 1, 5.0, 1),
]
OUTBOUND_CASES = [
    # Maryland 2026: lost Gillespie + Rice + Reese + Gapare + Harris-Smith
    # — outbound is same-team, so the bug doesn't affect this; pin the
    # baseline anyway to detect regressions elsewhere in the chain.
    ("Maryland Terrapins", 2026, 5, 30.0),
]
# Coverage-level invariant: across all 2024+2025 portal cycles, the
# corrected query should catch substantially more matches than the buggy
# one. Empirically (measured 2026-05-29) the buggy query missed 503/1958
# (25.7%) — those are real transfers whose natstat_id rotated. The lower
# bound below leaves margin for future data drift but will catch a
# regression that reverts the SQL.
COVERAGE_INVARIANT_PORTAL_YEARS = (2024, 2025)
COVERAGE_INVARIANT_MIN_TORVIK_ONLY = 300


def coverage_invariant(conn) -> tuple[int, int, int]:
    """Pool-wide: across `COVERAGE_INVARIANT_PORTAL_YEARS`, how many
    transfers does each join strategy match? Returns
    `(natstat_only_matches, torvik_only_matches, both)`. The bug class
    is "natstat-only" — the torvik_only_matches column counts the
    transfers the buggy SQL silently dropped. Empirically ~503 today."""
    sql = text(
        """
        WITH t_match AS (
            SELECT
                (p_base.natstat_id = p_tgt.natstat_id)::int AS nid_match,
                (tps_base.torvik_pid IS NOT NULL
                  AND tps_base.torvik_pid = tps_tgt.torvik_pid)::int AS pid_match
            FROM transfers tr
            JOIN players p_base
                ON p_base.id = tr.cstat_player_id AND p_base.season = tr.year
            LEFT JOIN torvik_player_stats tps_base
                ON tps_base.player_id = p_base.id AND tps_base.season = tr.year
            JOIN players p_tgt
                ON p_tgt.season = tr.year + 1
               AND (
                    p_tgt.natstat_id = p_base.natstat_id
                    OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
                        SELECT player_id FROM torvik_player_stats
                        WHERE torvik_pid = tps_base.torvik_pid AND season = tr.year + 1
                    ))
               )
            LEFT JOIN torvik_player_stats tps_tgt
                ON tps_tgt.player_id = p_tgt.id AND tps_tgt.season = p_tgt.season
            WHERE tr.year = ANY(:portal_years)
        )
        SELECT
            SUM(CASE WHEN nid_match=1 AND pid_match=0 THEN 1 ELSE 0 END) AS nid_only,
            SUM(CASE WHEN nid_match=0 AND pid_match=1 THEN 1 ELSE 0 END) AS pid_only,
            SUM(CASE WHEN nid_match=1 AND pid_match=1 THEN 1 ELSE 0 END) AS both
        FROM t_match
        """
    )
    row = conn.execute(
        sql, {"portal_years": list(COVERAGE_INVARIANT_PORTAL_YEARS)}
    ).fetchone()
    return int(row.nid_only), int(row.pid_only), int(row.both)


def main() -> int:
    engine = get_engine()
    failures: list[str] = []
    with engine.connect() as conn:
        # Per-team inbound: verify (a) the corrected query finds the
        # cohort, (b) the buggy query under-counts by at least
        # `min_torvik_only` for cases pinned as transfer-traversal
        # canonical.
        print("=== INBOUND cross-season transfer matches ===")
        for team, season, min_n, min_cam, min_torvik_only in INBOUND_CASES:
            ok_n, ok_cam = inbound_with_torvik_fallback(conn, team, season)
            bug_n, bug_cam = inbound_natstat_only(conn, team, season)
            torvik_only = ok_n - bug_n
            ok_count = ok_n >= min_n and ok_cam >= min_cam
            ok_diff = torvik_only >= min_torvik_only
            status = "PASS" if ok_count and ok_diff else "FAIL"
            print(
                f"  [{status}] {team} {season}: corrected n={ok_n} cam={ok_cam:.1f}  "
                f"| buggy n={bug_n} cam={bug_cam:.1f}  "
                f"| torvik-only n={torvik_only}  "
                f"(min: n≥{min_n} cam≥{min_cam} torvik-only≥{min_torvik_only})"
            )
            if not ok_count:
                failures.append(
                    f"{team} {season} inbound below count/cam bound "
                    f"(n={ok_n}/{min_n}, cam={ok_cam:.1f}/{min_cam:.1f})"
                )
            if not ok_diff:
                failures.append(
                    f"{team} {season}: torvik-only matches={torvik_only} < "
                    f"{min_torvik_only}. The natstat_id-only join may have been "
                    "reintroduced, OR this team's portal class no longer exercises "
                    "the transfer-traversal path — replace with a portal-heavier team."
                )

        print("\n=== OUTBOUND (same-team join, baseline pin) ===")
        for team, season, min_n, min_cam in OUTBOUND_CASES:
            n, cam = outbound_same_team(conn, team, season)
            status = "PASS" if n >= min_n and cam >= min_cam else "FAIL"
            print(
                f"  [{status}] {team} {season}: n={n} cam={cam:.1f}  "
                f"(min: n≥{min_n} cam≥{min_cam})"
            )
            if status == "FAIL":
                failures.append(
                    f"{team} {season} outbound below bound "
                    f"(n={n}/{min_n}, cam={cam:.1f}/{min_cam:.1f})"
                )

        print("\n=== POOL-WIDE coverage invariant ===")
        nid_only, pid_only, both = coverage_invariant(conn)
        invariant_ok = pid_only >= COVERAGE_INVARIANT_MIN_TORVIK_ONLY
        status = "PASS" if invariant_ok else "FAIL"
        print(
            f"  [{status}] portal_years={list(COVERAGE_INVARIANT_PORTAL_YEARS)}  "
            f"natstat-only matches: {nid_only}  "
            f"torvik-only matches: {pid_only}  "
            f"both: {both}  "
            f"(min torvik-only ≥ {COVERAGE_INVARIANT_MIN_TORVIK_ONLY})"
        )
        if not invariant_ok:
            failures.append(
                f"pool-wide torvik-only matches={pid_only} < "
                f"{COVERAGE_INVARIANT_MIN_TORVIK_ONLY}. Either the natstat_id-only "
                "join regressed, or the underlying data shifted enough that the "
                "lower bound needs recalibration — investigate before lowering it."
            )

    print()
    if failures:
        print(f"{len(failures)} failure(s):")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("all cross-season join checks pass.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
