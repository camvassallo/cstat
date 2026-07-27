"""Guards for the input-fingerprint chain (issue #223).

The load-bearing one is `test_oof_digest_matches_the_218_construction`. Every
committed Layer 2 meta carries an `oof_provenance` block computed by the
original `oof_provenance.py` expression, and `Predictor::load` refuses to boot
when the two halves disagree. `provenance.py` recomputes those same digests
through a generalized code path — so if the general form ever stops reducing
exactly to the #218 construction, the next retrain stamps digests that no
longer compare equal to anything, and the failure surfaces as a **production
API that will not start**. This test is the thing that catches that at desk
time instead.

The rest guard the properties the report depends on: fingerprints are stable
across repeated reads (otherwise every check reports drift), NULL values are
distinguishable from absent rows, and the season classifier agrees with the
Rust calendar it mirrors.

Same shape as `test_frame_determinism.py`: a database-free subset that runs in
CI, and a database-backed subset that skips when no database is reachable.
Run `python test_provenance.py` (exit 0 on pass), or under pytest.
"""

from __future__ import annotations

import datetime as dt
import sys

import check_provenance as C
import provenance as P

_SKIPS: dict[str, str] = {}


def _skip(name: str, why: str) -> None:
    _SKIPS[name] = why


def _db_reachable() -> bool:
    try:
        from db import get_engine

        with get_engine().connect():
            return True
    except Exception:
        return False


# ---- Database-free ------------------------------------------------------


def test_oof_row_expr_is_the_218_expression() -> None:
    """The generated SQL for an OOF source, character for character.

    `oof_provenance.py` hard-codes:

        key::text || ':' || target_season::text || ':' || round(mean::numeric, 6)::text

    A single-value source with no text values and a NOT NULL value column must
    generate exactly that, modulo the NULL guard. Pinning the *string* rather
    than just the resulting digest is deliberate: a digest comparison needs a
    populated database, and this failure mode bricks the API boot, so it has to
    be catchable in CI where there is no database.
    """
    src = P.SOURCES["trajectory_oof_predictions"]
    expected = (
        "torvik_pid::text || ':' || target_season::text || ':' || "
        "coalesce(round(mean::numeric, 6)::text, '~')"
    )
    assert P._row_expr(src) == expected, (
        f"OOF row expression drifted from the #218 construction.\n"
        f"  got:      {P._row_expr(src)}\n"
        f"  expected: {expected}\n"
        "Committed Layer 2 metas hash under the old form; changing this makes "
        "the next retrain's stamps incomparable and the API refuses to boot."
    )


def test_keys_are_natural_not_surrogate() -> None:
    """No source may key on `id`.

    A surrogate key is not stable across a re-ingest that reproduces identical
    values, so keying on one would report drift for a genuine no-op — the exact
    false positive this design excludes `created_at` to avoid.
    """
    for name, src in P.SOURCES.items():
        assert "id" not in src.keys, (
            f"source {name} keys on the surrogate `id`; use the natural "
            f"unique key so a renumbering re-ingest is not reported as drift"
        )


def test_every_node_input_resolves() -> None:
    """`NODE_INPUTS` names must exist in `SOURCES`.

    The names are a wire format — they are written into model metas and read
    back by `check_provenance.py`. A typo here would silently fingerprint
    nothing rather than fail, which is the "can't tell, carry on" state #218
    was produced in.
    """
    for node, names in P.NODE_INPUTS.items():
        for name in names:
            assert name in P.SOURCES, f"node {node} declares unknown source {name}"
        assert node in P.NODE_META_FILES, f"node {node} has no meta file mapping"
        assert node in P.NODE_UPSTREAM, f"node {node} has no upstream declaration"


def test_layer2_halves_declare_identical_inputs() -> None:
    """`roster_impact` and `roster_adjo` share one frame via `build_dataset`.

    They differ only in target column, so any divergence in their declared
    inputs is a bug in this table — and would make the report claim one half is
    stale while the other is current, for two models that cannot disagree.
    """
    assert P.NODE_INPUTS["roster_impact"] == P.NODE_INPUTS["roster_adjo"], (
        "the two Layer 2 halves share build_dataset; their declared inputs "
        "must match or the staleness report contradicts itself"
    )


def test_null_values_are_distinguishable_from_absent_rows() -> None:
    """A NULL value must hash to the sentinel, not annihilate the row.

    `||` propagates NULL and `string_agg` skips NULL rows, so without the
    guard a table whose values went NULL would hash identically to one where
    those rows were deleted. `player_on_off.net_on_off` is NULL for every
    player with no OFF sample, so this is a live case, not a hypothetical.
    """
    src = P.SOURCES["player_on_off"]
    expr = P._row_expr(src)
    assert expr.count("coalesce(") == len(src.values) + len(src.text_values), (
        "every value column needs a NULL guard; without it NULL values and "
        "deleted rows produce the same digest"
    )


def test_mutable_season_tracks_the_rust_calendar() -> None:
    """`mutable_season` must agree with `cstat_ingest::season_for_date`.

    This is what separates expected nightly churn from real drift, so a
    mismatch with the ingest calendar makes the report wrong in the direction
    that matters — calling a genuinely stale model current.
    """
    # Season rolls at November.
    assert P.season_for_date(dt.date(2026, 11, 1)) == 2027
    assert P.season_for_date(dt.date(2026, 10, 31)) == 2026
    assert P.season_for_date(dt.date(2027, 3, 1)) == 2027

    # In-season: the current season is being rewritten nightly.
    assert P.mutable_season(dt.date(2027, 1, 15)) == 2027
    assert P.mutable_season(dt.date(2026, 11, 20)) == 2027
    assert P.mutable_season(dt.date(2027, 4, 10)) == 2027

    # Offseason: nothing is churning, so every digest change is real drift.
    assert P.mutable_season(dt.date(2026, 7, 26)) is None
    assert P.mutable_season(dt.date(2026, 4, 20)) is None
    assert P.mutable_season(dt.date(2026, 10, 1)) is None


# ---- The classifier -----------------------------------------------------
#
# These carry the report's whole value. A classifier that under-reports lets
# #218 happen again; one that over-reports gets ignored, which lets #218 happen
# again by a different route. Both directions are pinned here.


def _entry(rows_by_season: dict[int, str]) -> dict:
    """A fingerprint entry with the given per-season digests."""
    return {
        "n_rows": len(rows_by_season),
        "digest": "total-" + "|".join(f"{s}:{d}" for s, d in sorted(rows_by_season.items())),
        "by_season": {
            str(s): {"n_rows": 1, "digest": d} for s, d in rows_by_season.items()
        },
    }


def test_unchanged_source_is_current() -> None:
    e = _entry({2025: "a", 2026: "b"})
    verdict, _ = C.classify_source("torvik_player_stats.cam_v3", e, e, churning=2026)
    assert verdict == C.CURRENT


def test_live_season_churn_is_not_drift() -> None:
    """The in-progress season moving in a nightly-rewritten table is expected.

    This is the exemption the whole design turns on. Without it the report
    prints STALE every morning for ~150 nights, and a report that always says
    STALE is one nobody reads.
    """
    before = _entry({2025: "a", 2026: "b"})
    after = _entry({2025: "a", 2026: "CHANGED"})
    verdict, why = C.classify_source(
        "torvik_player_stats.cam_v3", before, after, churning=2026
    )
    assert verdict == C.CHURN, f"expected churn, got {verdict}: {why}"


def test_closed_season_change_is_drift() -> None:
    """A *closed* season moving is a recompute or swap-repair — real staleness.

    This is the case the exemption must not swallow: #140/#201-style repairs
    rewrite historical box rows, which genuinely invalidates a Layer 1 fit.
    """
    before = _entry({2019: "a", 2026: "b"})
    after = _entry({2019: "REPAIRED", 2026: "b"})
    verdict, why = C.classify_source(
        "torvik_player_stats.cam_v3", before, after, churning=2026
    )
    assert verdict == C.STALE, f"a closed season moved but got {verdict}"
    assert "2019" in why


def test_churn_exemption_does_not_cover_row_count_changes() -> None:
    """A season appearing or disappearing is never churn.

    The nightly updates values within the live season; it does not add or drop
    whole seasons. A new season key means a backfill or a bootstrap, which
    changes the training row set.
    """
    before = _entry({2025: "a", 2026: "b"})
    after = _entry({2025: "a", 2026: "b", 2027: "new"})
    verdict, _ = C.classify_source(
        "torvik_player_stats.cam_v3", before, after, churning=2026
    )
    assert verdict == C.STALE


def test_churn_exemption_is_limited_to_nightly_tables() -> None:
    """`recruits` is never rewritten by a nightly, so a change there is real.

    Without the per-source `nightly` flag, a 247 re-ingest during the season
    would be waved through as expected churn.
    """
    assert not P.SOURCES["recruits"].nightly
    before = _entry({2026: "a"})
    after = _entry({2026: "REINGESTED"})
    verdict, why = C.classify_source("recruits", before, after, churning=2026)
    assert verdict == C.STALE, f"a non-nightly table changed but got {verdict}"
    assert "no nightly writes this table" in why


def test_offseason_grants_no_exemption() -> None:
    """With no season in progress, every change is drift.

    `mutable_season` returns None outside Nov–Apr 15, and a change to a
    finished season during the offseason is unambiguously a recompute.
    """
    before = _entry({2026: "a"})
    after = _entry({2026: "CHANGED"})
    verdict, _ = C.classify_source(
        "torvik_player_stats.cam_v3", before, after, churning=None
    )
    assert verdict == C.STALE


def test_missing_season_detail_reports_the_strong_verdict() -> None:
    """A stamp with no `by_season` cannot be attributed, so it reports STALE.

    Under-reporting drift is the failure this tool exists to prevent, so the
    unattributable case must resolve toward STALE, not toward CURRENT.
    """
    before = {"n_rows": 10, "digest": "old"}
    after = {"n_rows": 12, "digest": "new"}
    verdict, why = C.classify_source(
        "torvik_player_stats.cam_v3", before, after, churning=2026
    )
    assert verdict == C.STALE
    assert "+2" in why


def test_every_node_is_actually_reported() -> None:
    """`check_provenance.NODES` must cover every node in the registry.

    The report iterates `NODES`, not `NODE_INPUTS`. A node added to the
    registry but forgotten here would be fingerprinted and then silently left
    out of the verdict — a tool that quietly under-reports, which is the exact
    class of failure this whole chain is meant to remove.
    """
    # Layer 1/2 come from NODE_INPUTS (meta-stamped); Layer 3 from
    # LAYER3_UPSTREAM (artifact_provenance-stamped). Together they must be
    # exactly what the report iterates.
    assert set(C.NODES) == set(P.NODE_INPUTS) | set(P.LAYER3_UPSTREAM), (
        f"NODES {sorted(C.NODES)} does not match the registries; a node missing "
        f"here is fingerprinted and then silently never reported"
    )
    assert set(C.LAYER) == set(C.NODES), "every reported node needs a layer"
    assert set(C.LAYER3) == set(P.LAYER3_UPSTREAM), (
        "LAYER3 must match the registry, or a Layer 3 node would be classified "
        "by the meta-stamp rule it has no meta for"
    )


def test_nodes_are_in_dependency_order() -> None:
    """`NODES` order decides which node the report tells you to retrain from.

    `render()` picks the *first* stale entry as the highest stale node and
    prints it into a `--from` command. If the order were wrong that advice
    would skip a stale upstream node, leaving the tree desynced after a retrain
    the operator believes fixed it. Layer 3 must come last for the same reason:
    it is downstream of everything.
    """
    seen: set[str] = set()
    for node in C.NODES:
        upstream = P.NODE_UPSTREAM.get(node) or P.LAYER3_UPSTREAM.get(node, ())
        for up in upstream:
            assert up in seen, (
                f"{node} is listed before its upstream {up}; the report would "
                f"recommend retraining from too far down the tree"
            )
        seen.add(node)
    layers = [C.LAYER[n] for n in C.NODES]
    assert layers == sorted(layers), f"NODES is not in layer order: {layers}"


def test_staleness_propagates_to_layer2() -> None:
    """A Layer 2 node with matching inputs is still STALE when Layer 1 moved.

    This is #218 stated as a rule, and it is the one verdict that cannot be
    derived from a digest comparison: Layer 2's own inputs genuinely still
    match, because it consumes the OOF tables and those have not changed. It is
    stale because it is calibrated for an error profile that no longer exists.

    Deliberately end-to-end through `check()` rather than asserting on the
    `NODE_UPSTREAM` table — the table being right is worth nothing if the
    propagation loop does not read it.
    """
    import json
    import tempfile
    from pathlib import Path

    live = {
        name: {
            "n_rows": 1,
            "digest": f"d-{name}",
            "by_season": {"2026": {"n_rows": 1, "digest": f"s-{name}"}},
        }
        for name in P.SOURCES
    }

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        # Layer 1 `trajectory` trained on an older Layer 0; everything else,
        # including both Layer 2 halves, matches the live database exactly.
        for node in P.NODE_INPUTS:
            stamp = {n: json.loads(json.dumps(live[n])) for n in P.NODE_INPUTS[node]}
            if node == "trajectory":
                cam = stamp["torvik_player_stats.cam_v3"]
                cam["digest"] = "older"
                cam["by_season"]["2026"] = {"n_rows": 1, "digest": "older-2026"}
            meta = {"input_provenance": stamp}
            if node.startswith("roster"):
                meta["oof_provenance"] = P.oof_provenance_from(live)
            (d / P.NODE_META_FILES[node]).write_text(json.dumps(meta))

        # Layer 3 was produced by exactly the model artifact on disk, so its
        # OWN check passes — it can only be caught by propagation.
        sha = P.onnx_sha256("roster_impact_model") or "x"
        artifacts = {
            "team_preseason_projection": {
                "2026": {"models": {"roster_impact_model": {"onnx_sha256": sha}}}
            }
        }

        orig_dir = C.MODEL_DIR
        try:
            C.MODEL_DIR = d
            report = C.check(
                live=live, artifacts=artifacts, today=dt.date(2026, 7, 26)
            )
        finally:
            C.MODEL_DIR = orig_dir

    nodes = report["nodes"]
    assert nodes["trajectory"]["verdict"] == C.STALE
    assert nodes["freshman"]["verdict"] == C.CURRENT, (
        "freshman does not consume the changed source and must stay current — "
        "over-propagating would make the report recommend needless retrains"
    )
    for half in ("roster_impact", "roster_adjo"):
        assert nodes[half]["verdict"] == C.STALE, (
            f"{half}'s own inputs match, but Layer 1 above it moved; without "
            f"propagation this is the #218 blind spot"
        )
        assert any("upstream" in r for r in nodes[half]["reasons"])
    assert nodes["team_preseason_projection"]["verdict"] == C.STALE, (
        "the projection was built by exactly the model artifact on disk, so its "
        "own check passes — but that model is now calibrated against a Layer 1 "
        "generation that no longer exists, so staleness has to reach Layer 3"
    )
    assert any(
        "upstream" in r for r in nodes["team_preseason_projection"]["reasons"]
    )
    assert report["boot_guard"]["status"] == "ok", (
        "the two halves genuinely share one OOF snapshot — the boot guard is "
        "right to pass, which is exactly why it cannot catch this case"
    )
    assert report["exit_code"] == 1


# ---- Layer 3 ------------------------------------------------------------
#
# Layer 3 has no meta to re-evaluate, so it classifies on a different rule:
# was this built by the model artifact that ships now? These pin that rule and
# the one thing it must never do — block anything.


def _model_entry(stem: str, sha: str) -> dict:
    return {stem: {"onnx_sha256": sha, "input_provenance": None, "oof_provenance": None}}


def test_layer3_matching_artifact_is_current() -> None:
    real = P.onnx_sha256("roster_impact_model")
    if real is None:
        return _skip("test_layer3_matching_artifact_is_current", "no model on disk")
    verdict, reasons = C.classify_layer3(
        "team_preseason_projection",
        {"2026": {"models": _model_entry("roster_impact_model", real)}},
    )
    assert verdict == C.CURRENT, reasons


def test_layer3_superseded_model_is_stale() -> None:
    """The rows are untouched; the model that made them has been retrained.

    Nothing about the projection table itself is wrong-looking — this is the
    #218 shape one layer over, and the only signal is the artifact identity.
    """
    verdict, reasons = C.classify_layer3(
        "team_preseason_projection",
        {"2026": {"models": _model_entry("roster_impact_model", "0" * 64)}},
    )
    assert verdict == C.STALE
    assert "superseded" in reasons[0] and "2026" in reasons[0]


def test_layer3_is_keyed_per_season() -> None:
    """A partial `--years` run must not vouch for seasons it did not write.

    `compute-projections --years 2026` refreshes one season. If provenance were
    stored once per run, that row would implicitly claim every other season was
    regenerated by the same model.
    """
    verdict, reasons = C.classify_layer3(
        "team_preseason_projection",
        {
            "2025": {"models": _model_entry("roster_impact_model", "0" * 64)},
            "2026": {"models": _model_entry("roster_impact_model", P.onnx_sha256("roster_impact_model") or "x")},
        },
    )
    assert verdict == C.STALE
    assert any("2025" in r for r in reasons)
    assert not any("2026" in r for r in reasons), "2026 matches and must not be flagged"


def test_layer3_detects_a_drifted_loso_set() -> None:
    """The specific gap #238 exists for.

    `projections-backtest` scores with the gitignored LOSO models and
    `compute_cae.py` grades against that dump. Those files never appear in
    `git status`, so a set from a different frame than the committed serving
    model produces plausible grades against a projection generation that no
    longer ships.
    """
    current = P.loso_file_digests()
    if not current:
        return _skip("test_layer3_detects_a_drifted_loso_set", "no LOSO models on disk")
    files = sorted(current.items())
    drifted = [{"file": f, "sha256": ("f" * 64 if i == 0 else s)} for i, (f, s) in enumerate(files)]
    verdict, reasons = C.classify_layer3(
        "coach_season_cae",
        {"all": {"dump_provenance": {"models": {}, "roster_impact_loso": {"models": drifted}}}},
    )
    assert verdict == C.STALE
    assert "LOSO set changed" in reasons[0]


def test_layer3_missing_row_is_unstamped_not_stale() -> None:
    """Nothing has run its producer yet — that is not evidence of drift."""
    verdict, reasons = C.classify_layer3("team_preseason_projection", None)
    assert verdict == C.UNSTAMPED
    assert "rerun its producer" in reasons[0]


def test_layer3_never_blocks_the_boot() -> None:
    """Report-only, deliberately (the parent issue's open question).

    A stale `team_preseason_projection` is a data-freshness problem. Exit 2 is
    reserved for conditions that genuinely stop the API starting, so Layer 3
    drift must produce exit 1 at most — prod refusing to serve over a stale
    projection would be strictly worse than serving it.
    """
    live = {
        name: {"n_rows": 1, "digest": f"d-{name}",
               "by_season": {"2026": {"n_rows": 1, "digest": f"s-{name}"}}}
        for name in P.SOURCES
    }
    report = C.check(
        live=live,
        artifacts={"team_preseason_projection": {
            "2026": {"models": _model_entry("roster_impact_model", "0" * 64)}}},
        today=dt.date(2026, 7, 26),
    )
    assert report["nodes"]["team_preseason_projection"]["verdict"] == C.STALE
    assert report["exit_code"] == 1, (
        "Layer 3 drift must never reach exit 2 — that code means the API will "
        "not start, and a stale projection table does not stop it"
    )


# ---- Database-backed ----------------------------------------------------


def test_oof_digest_matches_the_218_construction() -> None:
    """The generalized helper reproduces `oof_provenance.py` bit for bit.

    The end-to-end version of the expression test above: same database, both
    code paths, identical output. If this fails, the committed Layer 2 metas
    and the next retrain's stamps are computed under different rules and the
    boot guard will reject a correctly-retrained pair.
    """
    if not _db_reachable():
        return _skip(
            "test_oof_digest_matches_the_218_construction", "no database reachable"
        )
    from oof_provenance import oof_provenance

    general = P.oof_provenance_from(
        P.fingerprint(("trajectory_oof_predictions", "freshman_oof_predictions"))
    )
    assert general == oof_provenance(), (
        "provenance.py and oof_provenance.py disagree — the generalized form "
        "no longer reduces to the #218 construction, so every committed Layer "
        "2 stamp is now incomparable and the API will refuse to boot."
    )


def test_fingerprints_are_stable_across_reads() -> None:
    """Two fingerprints of an unchanged database must be identical.

    An unstable fingerprint reports drift on every run, and a staleness report
    that always says STALE is one people stop reading — which is how the
    original bug survived three regenerations.
    """
    if not _db_reachable():
        return _skip("test_fingerprints_are_stable_across_reads", "no database reachable")
    names = tuple(P.SOURCES)
    assert P.fingerprint(names) == P.fingerprint(names), (
        "fingerprint() is not stable across two reads of the same database; "
        "check that every source's `keys` determine a row"
    )


def test_season_subdigests_cover_every_row() -> None:
    """Per-season row counts must sum to the whole-table count.

    The report's in-season/closed-season classification is only sound if the
    split is a partition. A NULL season column would silently drop rows from
    the per-season view while leaving the total digest intact, so a season
    could change without any sub-digest moving.
    """
    if not _db_reachable():
        return _skip("test_season_subdigests_cover_every_row", "no database reachable")
    fp = P.fingerprint(tuple(P.SOURCES))
    for name, entry in fp.items():
        if "by_season" not in entry:
            continue
        total = sum(s["n_rows"] for s in entry["by_season"].values())
        assert total == entry["n_rows"], (
            f"{name}: per-season counts sum to {total:,} but the table has "
            f"{entry['n_rows']:,} rows — the season split is not a partition, "
            f"so a change in the missing rows would go unreported"
        )


def main() -> int:
    checks = [
        test_oof_row_expr_is_the_218_expression,
        test_keys_are_natural_not_surrogate,
        test_every_node_input_resolves,
        test_layer2_halves_declare_identical_inputs,
        test_null_values_are_distinguishable_from_absent_rows,
        test_mutable_season_tracks_the_rust_calendar,
        test_unchanged_source_is_current,
        test_live_season_churn_is_not_drift,
        test_closed_season_change_is_drift,
        test_churn_exemption_does_not_cover_row_count_changes,
        test_churn_exemption_is_limited_to_nightly_tables,
        test_offseason_grants_no_exemption,
        test_missing_season_detail_reports_the_strong_verdict,
        test_every_node_is_actually_reported,
        test_nodes_are_in_dependency_order,
        test_staleness_propagates_to_layer2,
        test_layer3_matching_artifact_is_current,
        test_layer3_superseded_model_is_stale,
        test_layer3_is_keyed_per_season,
        test_layer3_detects_a_drifted_loso_set,
        test_layer3_missing_row_is_unstamped_not_stale,
        test_layer3_never_blocks_the_boot,
        test_oof_digest_matches_the_218_construction,
        test_fingerprints_are_stable_across_reads,
        test_season_subdigests_cover_every_row,
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
    summary = f"{ran}/{len(checks)} #223 provenance checks pass"
    if _SKIPS:
        summary += f" ({len(_SKIPS)} skipped: {', '.join(sorted(_SKIPS))})"
    print(summary + ".")
    return 0


if __name__ == "__main__":
    sys.exit(main())
