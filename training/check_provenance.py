"""Which model nodes are stale? (issue #223)

Recomputes each node's input fingerprints against the live database, compares
them to the stamp the node carries in its meta, and prints a per-node verdict.
This is the *detection* half of the retrain-integrity work;
`retrain_downstream.sh` is the prevention half. Detection is the more robust
of the two because it catches drift regardless of how the models were produced
— including a hand-run trainer, a restored backup, or a branch switch.

    cd training && ./.venv/bin/python check_provenance.py
    ./.venv/bin/python check_provenance.py --json      # machine-readable
    ./.venv/bin/python check_provenance.py --strict    # unstamped also fails

## The rule it makes checkable

**Retrain from the highest stale node downward.** Everything below a changed
node is stale, including nodes nobody touched. The report propagates that
automatically: a Layer 2 node whose own inputs match is still reported STALE
when Layer 1 above it moved, because Layer 2 trains on Layer 1's *predictions*
and is calibrated to their specific error profile.

The inverse matters just as much. Blindly retraining the root "to be safe" is
its own churn — it rewrites committed ONNX artifacts and produces a diff nobody
can attribute to a data change. A report that says a node is current is what
lets you not retrain it.

## Why verdicts are not binary

`compute_all` rewrites the live season's `cam_gbpm_v3_psos`,
`player_season_stats`, `player_on_off` and `player_archetypes` every nightly,
and the Layer 1 training window includes the in-progress season. So in-season,
a whole-table comparison is genuinely different every single morning. A tool
that prints STALE 150 nights a year is a tool people stop reading — which is
exactly how #218 survived three regenerations of the thing it depended on.

So a digest change is classified, not just detected:

  CURRENT   nothing moved.
  CHURN     only the in-progress season moved, in a table the nightly rewrites
            by design. Expected. Not a reason to retrain.
  STALE     a *closed* season moved, or a table no nightly touches moved. That
            is a recompute, a swap-repair (#140/#201), a re-ingest, or an
            archetype refit — something that genuinely invalidates the fit.
  UNSTAMPED the node predates the fingerprint chain. Reported, not failed:
            every model on disk is unstamped until its next retrain, so
            failing here would make the tool useless the day it landed.

`--strict` promotes UNSTAMPED to a failure, for use once the tree has been
retrained through once.

## Exit codes

0 = no drift (CURRENT / CHURN / UNSTAMPED), 1 = at least one STALE node,
2 = the two Layer 2 halves disagree, which means the API will refuse to boot.

Layer 3 (#238) is included and is **report-only by design**: it can reach exit
1 but never exit 2. A stale `team_preseason_projection` is a data-freshness
problem, and prod refusing to serve over it would be strictly worse than
serving it. Guarded by `test_layer3_never_blocks_the_boot`.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path

import provenance
from provenance import (
    LAYER3_UPSTREAM,
    NODE_INPUTS,
    NODE_META_FILES,
    NODE_UPSTREAM,
    SOURCES,
    fingerprint,
    loso_file_digests,
    mutable_season,
    onnx_sha256,
    read_artifact_provenance,
)

#: Report order = dependency order. Reading top to bottom gives you the highest
#: stale node, which is the one to retrain from.
NODES = (
    "trajectory",
    "freshman",
    "roster_impact",
    "roster_adjo",
    "team_preseason_projection",
    "coach_season_cae",
)
LAYER = {
    "trajectory": 1,
    "freshman": 1,
    "roster_impact": 2,
    "roster_adjo": 2,
    "team_preseason_projection": 3,
    "coach_season_cae": 3,
}
#: Layer 3 products carry no meta; they record their producing model into
#: `artifact_provenance` instead, so they classify by a different rule.
LAYER3 = tuple(n for n, layer in LAYER.items() if layer == 3)

CURRENT, CHURN, STALE, UNSTAMPED = "current", "churn", "STALE", "unstamped"


def _load_stamp(node: str) -> dict | None:
    """The `input_provenance` block a node carries, or None if unstamped."""
    path = provenance.MODEL_DIR / NODE_META_FILES[node]
    if not path.exists():
        return None
    try:
        meta = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None
    stamp = meta.get("input_provenance")
    return stamp if isinstance(stamp, dict) else None


def classify_source(
    name: str, stamped: dict | None, live: dict, churning: int | None
) -> tuple[str, str]:
    """Compare one source's stamp against the live database.

    Returns `(verdict, reason)`. The season split is what makes this more than
    a digest comparison: it decides whether a change is the nightly doing its
    job or something that invalidated the fit.
    """
    if stamped is None:
        return UNSTAMPED, f"{name} not in stamp"
    if stamped.get("digest") == live["digest"]:
        return CURRENT, ""

    src = SOURCES[name]
    before = stamped.get("by_season") or {}
    after = live.get("by_season") or {}
    if not before or not after:
        # Pre-sub-digest stamp, or a source with no season column: the whole
        # table moved and there is no way to attribute it. Report the strong
        # verdict — under-reporting drift is the failure this tool exists for.
        delta = live["n_rows"] - stamped.get("n_rows", 0)
        return STALE, f"{name} digest changed ({delta:+,} rows, no season detail)"

    added = sorted(set(after) - set(before), key=int)
    removed = sorted(set(before) - set(after), key=int)
    changed = sorted(
        (s for s in set(before) & set(after) if before[s]["digest"] != after[s]["digest"]),
        key=int,
    )

    # The churn exemption is narrow on purpose: it applies only to the single
    # in-progress season, only in a table the nightly rewrites, and only when
    # nothing else moved. A season appearing or disappearing is never churn.
    if (
        src.nightly
        and churning is not None
        and changed == [str(churning)]
        and not added
        and not removed
    ):
        return CHURN, f"{name} {churning} only (nightly rewrite)"

    bits = []
    if changed:
        bits.append("changed " + ",".join(changed))
    if added:
        bits.append("added " + ",".join(added))
    if removed:
        bits.append("removed " + ",".join(removed))
    detail = "; ".join(bits) or "digest changed"
    if not src.nightly:
        detail += " (no nightly writes this table)"
    return STALE, f"{name}: {detail}"


def classify_layer3(artifact: str, recorded: dict | None) -> tuple[str, list[str]]:
    """Was this derived product built by the model artifact now on disk?

    Layer 3 has no `input_provenance` of its own to re-evaluate — it is rows and
    files, not a fit. The question that *is* answerable is narrower and still
    the one that matters: **the model that produced this is not the model that
    ships now.** That is the #218 shape one layer over.

    Compared file-by-file against the sha256s Rust recorded, never by
    recomputing an aggregate digest in Python — see `provenance.onnx_sha256`.
    """
    if not recorded:
        return UNSTAMPED, [
            f"no artifact_provenance row for {artifact} — rerun its producer to stamp it"
        ]

    reasons: list[str] = []
    for key, prov in sorted(recorded.items()):
        prefix = f"[{key}] " if key != "all" else ""

        # The dump CAE was scored against wraps its own model record, so unwrap
        # one level to reach the models that actually produced the numbers.
        models = (prov or {}).get("models") or {}
        nested = ((prov or {}).get("dump_provenance") or {}).get("models") or {}
        for stem, entry in {**models, **nested}.items():
            recorded_sha = (entry or {}).get("onnx_sha256")
            current_sha = onnx_sha256(stem)
            if current_sha is None:
                reasons.append(f"{prefix}{stem}.onnx is missing from disk")
            elif recorded_sha and recorded_sha != current_sha:
                reasons.append(f"{prefix}produced by a superseded {stem}")

        # The LOSO set is the specific gap #238 exists for: gitignored, so it
        # never shows in `git status`, and a set from a different frame than the
        # committed serving model yields grades against a projection generation
        # that no longer ships.
        loso = ((prov or {}).get("dump_provenance") or prov or {}).get(
            "roster_impact_loso"
        ) or {}
        recorded_files = {m["file"]: m["sha256"] for m in loso.get("models", [])}
        if recorded_files:
            current_files = loso_file_digests()
            changed = [f for f, s in recorded_files.items() if current_files.get(f) != s]
            missing = [f for f in recorded_files if f not in current_files]
            if missing:
                reasons.append(f"{prefix}LOSO models absent: {len(missing)} file(s)")
            elif changed:
                reasons.append(f"{prefix}LOSO set changed: {len(changed)} model(s)")

    return (STALE if reasons else CURRENT), reasons


def check(
    strict: bool = False,
    today: dt.date | None = None,
    live: dict | None = None,
    artifacts: dict | None = None,
) -> dict:
    """Build the full report. Pure data — printing is the caller's job.

    `live` and `artifacts` override the two database reads, which lets the
    propagation and Layer 3 rules be tested without a populated database
    (`test_provenance.py`).
    """
    churning = mutable_season(today)
    live = live if live is not None else fingerprint(tuple(SOURCES))

    report: dict = {
        "checked_at": (today or dt.date.today()).isoformat(),
        "churning_season": churning,
        "nodes": {},
    }

    recorded = read_artifact_provenance() if artifacts is None else artifacts

    stale_nodes: set[str] = set()
    for node in NODES:
        verdicts: list[tuple[str, str, str]] = []
        stamp = None if node in LAYER3 else _load_stamp(node)

        if node in LAYER3:
            # No meta to re-evaluate — classified against the model artifact
            # recorded in `artifact_provenance` instead.
            verdict, reasons = classify_layer3(node, recorded.get(node))
        elif stamp is None:
            verdict, reasons = UNSTAMPED, [
                f"no input_provenance in {NODE_META_FILES[node]} — "
                "retrain to stamp it"
            ]
        else:
            for name in NODE_INPUTS[node]:
                v, why = classify_source(name, stamp.get(name), live[name], churning)
                verdicts.append((name, v, why))
            reasons = [why for _, v, why in verdicts if v in (STALE, UNSTAMPED) and why]
            if any(v == STALE for _, v, _ in verdicts):
                verdict = STALE
            elif any(v == UNSTAMPED for _, v, _ in verdicts):
                verdict = UNSTAMPED
            elif any(v == CHURN for _, v, _ in verdicts):
                verdict = CHURN
            else:
                verdict = CURRENT

        # Downstream propagation. A Layer 2 model whose own inputs match is
        # still stale when Layer 1 moved: it trains on Layer 1's predictions,
        # so it is calibrated for an error profile that no longer exists. This
        # is the #218 failure stated as a rule.
        upstream = NODE_UPSTREAM.get(node) or LAYER3_UPSTREAM.get(node, ())
        upstream_stale = [u for u in upstream if u in stale_nodes]
        if upstream_stale:
            verdict = STALE
            reasons.append(f"upstream {' + '.join(upstream_stale)} is stale")
        if verdict == STALE:
            stale_nodes.add(node)

        report["nodes"][node] = {
            "layer": LAYER[node],
            "verdict": verdict,
            "reasons": reasons,
            "sources": {name: v for name, v, _ in verdicts},
        }

    # Preview of the boot guard. The API compares these two blocks and refuses
    # to serve a mismatched pair, so catching it here beats catching it at the
    # next deploy.
    report["boot_guard"] = _boot_guard()

    failures = [n for n, r in report["nodes"].items() if r["verdict"] == STALE]
    unstamped = [n for n, r in report["nodes"].items() if r["verdict"] == UNSTAMPED]
    # Anything other than `ok` here means the API will not start: the Rust
    # validator treats a mismatched stamp AND a missing/unreadable meta as hard
    # failures, so `missing` cannot be softer than `mismatch`.
    report["exit_code"] = (
        2
        if report["boot_guard"]["status"] != "ok"
        else 1
        if failures or (strict and unstamped)
        else 0
    )
    return report


def _boot_guard() -> dict:
    """Would `Predictor::load` accept the two Layer 2 halves as they sit?"""
    blocks = {}
    for node in ("roster_impact", "roster_adjo"):
        path = provenance.MODEL_DIR / NODE_META_FILES[node]
        if not path.exists():
            return {"status": "missing", "detail": f"{path.name} not found"}
        blocks[node] = json.loads(path.read_text()).get("oof_provenance")
    if blocks["roster_impact"] is None or blocks["roster_adjo"] is None:
        # The Rust validator treats absence as a hard failure, not a skip.
        return {"status": "mismatch", "detail": "a half carries no oof_provenance stamp"}
    if blocks["roster_impact"] != blocks["roster_adjo"]:
        return {
            "status": "mismatch",
            "detail": "roster_impact and roster_adjo carry different OOF stamps",
        }
    return {"status": "ok", "detail": "both halves share one OOF snapshot"}


_MARK = {CURRENT: "✓", CHURN: "~", STALE: "✗", UNSTAMPED: "?"}


def render(report: dict) -> str:
    out: list[str] = []
    out.append("=" * 74)
    out.append(f"model provenance — checked {report['checked_at']}")
    churning = report["churning_season"]
    out.append(
        f"in-progress season: {churning} (nightly rewrites here are expected)"
        if churning
        else "offseason — no season is being rewritten, so any change is real drift"
    )
    out.append("=" * 74)

    width = max(len(n) for n in report["nodes"]) if report["nodes"] else 16
    indent = 12 + width + 11
    for node, r in report["nodes"].items():
        out.append(
            f"  {_MARK[r['verdict']]} Layer {r['layer']}  "
            f"{node:<{width}} {r['verdict'].upper():<10} "
            + (r["reasons"][0] if r["reasons"] else "")
        )
        for extra in r["reasons"][1:]:
            out.append(" " * indent + extra)

    g = report["boot_guard"]
    out.append("")
    out.append(
        f"  {'✓' if g['status'] == 'ok' else '✗'} boot guard      "
        f"{g['status'].upper():<10} {g['detail']}"
    )

    out.append("-" * 74)
    stale = [n for n, r in report["nodes"].items() if r["verdict"] == STALE]
    unstamped = [n for n, r in report["nodes"].items() if r["verdict"] == UNSTAMPED]
    if stale:
        # NODES is in dependency order, so the first stale entry is the highest.
        highest = next(n for n in NODES if n in stale)
        out.append(f"  {len(stale)} stale node(s): {', '.join(stale)}")
        out.append("  Retrain from the highest stale node downward:")
        out.append(f"    ./training/retrain_downstream.sh --from {highest}")
        if LAYER[highest] == 1:
            out.append(
                "    (a Layer 1 stage implies --with-layer1: it TRUNCATEs the "
                "OOF tables\n     and invalidates every Layer 2 model beneath.)"
            )
    elif unstamped:
        out.append(
            f"  No drift detected, but {len(unstamped)} node(s) carry no "
            f"fingerprint yet:\n    {', '.join(unstamped)}"
        )
        out.append(
            "  Layer 1/2 stamp themselves on their next retrain; Layer 3 on the "
            "next\n  run of its producer. Until then this check cannot speak for "
            "them — it is\n  not asserting they are current."
        )
    else:
        out.append("  All nodes current. Nothing to retrain.")
    out.append("=" * 74)
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--json", action="store_true", help="emit the raw report")
    ap.add_argument(
        "--strict",
        action="store_true",
        help="treat an unstamped node as a failure (use after one full retrain)",
    )
    ap.add_argument(
        "--as-of",
        metavar="YYYY-MM-DD",
        help="evaluate the in-season/offseason rule against this date",
    )
    args = ap.parse_args()

    today = dt.date.fromisoformat(args.as_of) if args.as_of else None
    report = check(strict=args.strict, today=today)
    print(json.dumps(report, indent=2) if args.json else render(report))
    return report["exit_code"]


if __name__ == "__main__":
    sys.exit(main())
