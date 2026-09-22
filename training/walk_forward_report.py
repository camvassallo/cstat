"""Print the tree's walk-forward scorecard from the model metas (#361).

Every trainer stamps a `walk_forward` block into its meta beside the LOSO /
LOPO / LOCO block it always carried. This reads the five metas and prints
them as one table — the number `retrain_downstream.sh` ends on, and the one
the methodology docs quote — so "what does the tree score, forward-chained?"
is a single command rather than four files.

Exit 1 when a meta has no `walk_forward` block: that is a model trained by a
script older than #361, and its headline number is not comparable.

Run:  cd training && ./.venv/bin/python walk_forward_report.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

MODEL_DIR = Path(__file__).parent / "models"
LAYER1 = (
    ("trajectory", "trajectory_model_meta.json", "backtest_lopo", "lopo_same_rows"),
    ("freshman", "freshman_model_meta.json", "loco_cv", "loco_same_rows"),
)


def _load(name: str) -> dict:
    path = MODEL_DIR / name
    if not path.exists():
        raise SystemExit(f"{path} missing — the model is not trained")
    meta = json.loads(path.read_text())
    if "walk_forward" not in meta:
        raise SystemExit(f"{name} carries no `walk_forward` block — retrain it (its trainer predates #361)")
    return meta


def main() -> int:
    print("=" * 72)
    print("walk-forward scorecard — train < S, test S; the canonical judge (#361)")
    print("=" * 72)

    # "optimism" = walk-forward − LOSO-family on identical rows: how much the
    # train-on-everything-else number flattered. Positive = it did.
    print("\nLayer 1 (per player, CamPom MAE; test target seasons from walk_from)")
    print(f"  {'model':<12}{'walk-forward':>14}{'same rows, LOPO/LOCO':>22}{'optimism':>10}{'n':>8}")
    for name, file, _, same_key in LAYER1:
        w = _load(file)["walk_forward"]
        wf = w["pooled"]["mae"]
        same = w.get(same_key, {}).get("mae")
        opt = f"{wf - same:+.3f}" if same is not None else "   —"
        print(f"  {name:<12}{wf:>14.3f}{(f'{same:.3f}' if same is not None else '—'):>22}{opt:>10}{w['pooled']['n']:>8}")

    ri = _load("roster_impact_model_meta.json")["walk_forward"]
    adjo = _load("roster_adjo_model_meta.json")["walk_forward"]
    adjd = _load("roster_adjd_model_meta.json")["walk_forward"]
    print("\nLayer 2 (per team; raw calibrator, fixed-iteration export protocol)")
    print(f"  {'model':<12}{'walk-forward':>14}{'same rows, LOSO':>22}{'optimism':>10}{'n':>8}")
    # ri["raw"] starts at raw_walk_from; the reported raw number is the
    # served table's "wf raw" column, which is on the walk_from+ rows the
    # same-rows LOSO covers.
    raw_loso = ri["raw_loso_same_rows"].get("mae")
    served = ri["served"]
    wf_raw_reported = served["cohorts"]["all"]["wf raw"]["mae"]
    print(f"  {'roster_impact':<12}{wf_raw_reported:>14.3f}{raw_loso:>22.3f}{wf_raw_reported - raw_loso:>+10.3f}{served['n']:>8}")
    print(f"  {'roster_adjo':<12}{adjo['pooled']['mae']:>14.3f}{'—':>22}{'—':>10}{adjo['pooled']['n']:>8}")
    print(f"  {'roster_adjd':<12}{adjd['pooled']['mae']:>14.3f}{'—':>22}{'—':>10}{adjd['pooled']['n']:>8}")

    print(f"\nServed projection (Layer 2 + Layer 4 blend), test {ri['walk_from']}+, n={served['n']}")
    names = served["variants"]
    print(f"  {'cohort':<26}{'n':>5}" + "".join(f"{v:>18}" for v in names))
    for cname, block in served["cohorts"].items():
        print(f"  {cname:<26}{block['n']:>5}" + "".join(f"{block[v]['mae']:>10.3f}{block[v]['bias']:>+8.2f}" for v in names))
    print("  (cell: MAE  bias)")
    print(f"\n  {'metric (per-season mean)':<31}" + "".join(f"{v:>18}" for v in names))
    for label, vals in served["rank_metrics"].items():
        print(f"  {label:<31}" + "".join(f"{vals[v]:>18.3f}" for v in names))
    print("\n  paired |err| vs wf served (delta, z; negative = better): pooled | top-25 | top-10 | level-changers")
    for v, blocks in served["paired"].items():
        print(f"    {v:<16}" + " | ".join(f"{b['delta']:+.3f} z={b['z']:+.2f}" for b in blocks.values()))

    c = ri["constants_refit"]
    print("\nLayer 4 constants refit inside each fold (on earlier walk-forward rows) vs served")
    s = c["served"]
    print(f"  served: w_stable {s['w_stable']:.2f}  w_overhaul {s['w_overhaul']:.2f}  shrink {s['shrink']:.2f}")
    for season, f in c["per_fold"].items():
        print(f"  {season}: w_stable {f['w_stable']:.2f}  w_overhaul {f['w_overhaul']:.2f}  shrink {f['shrink']:.2f}"
              f"   test MAE refit {f['test_mae_refit']:.3f} vs served {f['test_mae_served']:.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
