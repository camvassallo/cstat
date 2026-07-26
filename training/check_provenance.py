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
Layer 3 staleness is deliberately out of scope here and report-only by design:
a stale `team_preseason_projection` is a data-freshness problem, and prod
refusing to boot over it would be worse than serving it.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path

from provenance import (
    NODE_INPUTS,
    NODE_META_FILES,
    NODE_UPSTREAM,
    SOURCES,
    fingerprint,
    mutable_season,
)

MODEL_DIR = Path(__file__).parent / "models"

#: Report order = dependency order. Reading top to bottom gives you the highest
#: stale node, which is the one to retrain from.
NODES = ("trajectory", "freshman", "roster_impact", "roster_adjo")
LAYER = {"trajectory": 1, "freshman": 1, "roster_impact": 2, "roster_adjo": 2}

CURRENT, CHURN, STALE, UNSTAMPED = "current", "churn", "STALE", "unstamped"


def _load_stamp(node: str) -> dict | None:
    """The `input_provenance` block a node carries, or None if unstamped."""
    path = MODEL_DIR / NODE_META_FILES[node]
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
    moved = sorted(set(added) | set(removed) | set(changed), key=int)

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


def check(strict: bool = False, today: dt.date | None = None) -> dict:
    """Build the full report. Pure data — printing is the caller's job."""
    churning = mutable_season(today)
    live = fingerprint(tuple(SOURCES))

    report: dict = {
        "checked_at": (today or dt.date.today()).isoformat(),
        "churning_season": churning,
        "nodes": {},
    }

    stale_nodes: set[str] = set()
    for node in NODES:
        stamp = _load_stamp(node)
        verdicts: list[tuple[str, str, str]] = []
        if stamp is None:
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
        upstream_stale = [u for u in NODE_UPSTREAM[node] if u in stale_nodes]
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
    report["exit_code"] = (
        2
        if report["boot_guard"]["status"] == "mismatch"
        else 1
        if failures or (strict and unstamped)
        else 0
    )
    return report


def _boot_guard() -> dict:
    """Would `Predictor::load` accept the two Layer 2 halves as they sit?"""
    blocks = {}
    for node in ("roster_impact", "roster_adjo"):
        path = MODEL_DIR / NODE_META_FILES[node]
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

    for node, r in report["nodes"].items():
        out.append(
            f"  {_MARK[r['verdict']]} Layer {r['layer']}  "
            f"{node:<16} {r['verdict'].upper():<10} "
            + (r["reasons"][0] if r["reasons"] else "")
        )
        for extra in r["reasons"][1:]:
            out.append(" " * 41 + extra)

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
        out.append(f"  Retrain from the highest stale node downward:")
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
            "  They stamp themselves on their next retrain. Until then this "
            "check\n  cannot speak for them — it is not asserting they are current."
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
