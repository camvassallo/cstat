"""Transition-conditional blend diagnostic — is the flat 0.5·baseline + 0.5·roster_proj
served weight mis-tuned for new-coach / roster-overhaul teams?

Follows the program-persistence refutation ([[project_pit_cae_program_null]]):
a flat program prior is redundant with `baseline` because baseline (last
season's AdjEM) already encodes program persistence. But baseline assumes
CONTINUITY — it's a stale anchor exactly when a team changes coach or overhauls
its roster. baseline cannot encode its own reliability, so a blend weight that
LEANS OFF baseline for transition teams uses information orthogonal to baseline's
value — the one place there may be room to beat `served`.

Two transition signals, joined to the 11-season backtest dump:
  - is_new_hc  : coach_seasons flag (new head coach this offseason).
  - returning  : fraction of last season's roster TALENT (Σ cam_v3) retained,
                 matched across seasons by torvik_pid (the stable key). Low =
                 portal overhaul. roster_proj sees the new roster; baseline doesn't.

For each cohort we report standalone baseline/talent MAE, the served(0.5) MAE +
signed bias, and the IN-SAMPLE optimal weight on baseline. Then an honest
leave-one-season-out test: does a cohort-conditional weight (fit on the other
years) beat the flat 0.5 out-of-sample?

Run:  python3 transition_blend_diagnostic.py
"""

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

from sqlalchemy import text

from compute_cae import EVAL_DIR, load_backtest
from db import get_engine

START_YEAR = 2019
OVERHAUL_RETURNING = 0.40   # < this fraction of talent retained = "overhaul"
MIN_QUAL = 5                # player qualifying gate (mirror the roster model)


def load_rows(conn, dump: Path | None = None) -> list[dict]:
    """Backtest dump rows + team_natstat_id + is_new_hc + returning-talent frac."""
    bt = load_backtest(dump)
    tid2ns = {r.id: r.natstat_id
              for r in conn.execute(text("SELECT id::text AS id, natstat_id FROM teams"))}

    new_hc = {(r.ns, r.season): r.flag for r in conn.execute(text(
        """SELECT team_natstat_id AS ns, season, is_new_hc AS flag
           FROM coach_seasons WHERE team_natstat_id IS NOT NULL"""))}

    # Per (program, season): {torvik_pid: cam_v3} for qualifying players, to
    # measure season-over-season talent retention by the stable torvik_pid key.
    roster: dict[tuple, dict] = defaultdict(dict)
    for r in conn.execute(text(
        f"""SELECT t.natstat_id AS ns, pss.season AS season,
                   tps.torvik_pid AS tpid,
                   COALESCE(tps.cam_gbpm_v3_psos, 0) AS cam
            FROM player_season_stats pss
            JOIN teams t ON t.id = pss.team_id
            LEFT JOIN torvik_player_stats tps
              ON tps.player_id = pss.player_id AND tps.season = pss.season
            WHERE COALESCE(pss.minutes_per_game,0) >= {MIN_QUAL}
              AND COALESCE(pss.games_played,0) >= {MIN_QUAL}""")):
        if r.tpid is not None:
            roster[(r.ns, r.season)][r.tpid] = float(r.cam)

    def returning_frac(ns, target_year):
        base = roster.get((ns, target_year - 1), {})
        tgt = roster.get((ns, target_year), {})
        tot = sum(abs(v) for v in base.values())
        if tot <= 0 or not tgt:
            return None
        kept = sum(abs(v) for pid, v in base.items() if pid in tgt)
        return kept / tot

    rows = []
    for r in bt:
        if r["season"] < START_YEAR:
            continue
        ns = tid2ns.get(r["team_id"])
        if ns is None:
            continue
        rows.append({
            "ns": ns, "team": r["team_name"], "season": r["season"],
            "actual": float(r["actual"]), "baseline": float(r["baseline"]),
            "roster_proj": float(r["roster_proj"]),
            "is_new_hc": new_hc.get((ns, r["season"])),
            "returning": returning_frac(ns, r["season"]),
        })
    return rows


def mae(rows, w):
    """MAE of the blend w·baseline + (1-w)·roster_proj."""
    return sum(abs(r["actual"] - (w * r["baseline"] + (1 - w) * r["roster_proj"]))
               for r in rows) / len(rows)


def best_weight(rows):
    """In-sample optimal weight on baseline (grid 0..1 step .05) + its MAE."""
    grid = [(w / 20, mae(rows, w / 20)) for w in range(21)]
    w, m = min(grid, key=lambda t: t[1])
    return w, m


def bias(rows, w):
    """Mean SIGNED error of the blend (pred − actual): + = over-projected."""
    return sum((w * r["baseline"] + (1 - w) * r["roster_proj"]) - r["actual"]
               for r in rows) / len(rows)


def report_cohort(name, rows):
    if not rows:
        print(f"  {name:22s} n=0")
        return
    w, m = best_weight(rows)
    print(f"  {name:22s} n={len(rows):4d}  "
          f"base-only {mae(rows,1.0):5.3f}  talent-only {mae(rows,0.0):5.3f}  "
          f"served(.5) {mae(rows,0.5):5.3f} (bias {bias(rows,0.5):+5.2f})  "
          f"→ best w={w:.2f} MAE {m:5.3f}")


def loso_conditional(rows, cohort_fn, cohorts):
    """Honest out-of-sample test: a cohort-conditional baseline weight, the
    per-cohort optimum fit on all OTHER seasons, applied to the held-out season.
    Compares pooled MAE vs the flat 0.5 served weight on the SAME rows."""
    years = sorted({r["season"] for r in rows})
    flat_ae, cond_ae = [], []
    for y in years:
        train = [r for r in rows if r["season"] != y]
        test = [r for r in rows if r["season"] == y]
        w_by = {}
        for c in cohorts:
            grp = [r for r in train if cohort_fn(r) == c]
            w_by[c] = best_weight(grp)[0] if len(grp) >= 30 else 0.5
        for r in test:
            w = w_by.get(cohort_fn(r), 0.5)
            cond_ae.append(abs(r["actual"] - (w * r["baseline"] + (1 - w) * r["roster_proj"])))
            flat_ae.append(abs(r["actual"] - (0.5 * r["baseline"] + 0.5 * r["roster_proj"])))
    return sum(flat_ae) / len(flat_ae), sum(cond_ae) / len(cond_ae), w_by


def main():
    # `--dump` matters more here than in the other backtest readers: this tool
    # is what re-tunes PROJECTION_SHRINK_WEIGHT{,_OVERHAUL}, which are SERVED
    # constants in roster_projection.rs. load_backtest()'s fallback picks by
    # filename, not recency, so without this flag a retrain's fresh dump can
    # lose the sort to a months-old descriptively-tagged one and the weights
    # get tuned against a superseded projection generation — the #218 failure
    # mode, one layer over. Pass the dump the retrain just produced.
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dump", type=Path, default=None,
                    help="per-team backtest dump to read (default: newest by "
                         "filename, which is not always newest on disk)")
    args = ap.parse_args()

    with get_engine().connect() as conn:
        rows = load_rows(conn, args.dump)
    print(f"\nn = {len(rows)} team-seasons ({START_YEAR}-2026); "
          f"flat served MAE {mae(rows,0.5):.3f}, global best w={best_weight(rows)[0]:.2f}")

    print("\n— by coaching change (is_new_hc) —")
    report_cohort("returning HC", [r for r in rows if r["is_new_hc"] is False])
    report_cohort("NEW HC", [r for r in rows if r["is_new_hc"] is True])

    print("\n— by roster turnover (talent retained vs last season) —")
    has_t = [r for r in rows if r["returning"] is not None]
    report_cohort(f"overhaul (<{OVERHAUL_RETURNING:.0%})",
                  [r for r in has_t if r["returning"] < OVERHAUL_RETURNING])
    report_cohort(f"continuity (≥{OVERHAUL_RETURNING:.0%})",
                  [r for r in has_t if r["returning"] >= OVERHAUL_RETURNING])

    print("\n— combined transition cohort (new HC OR overhaul) —")
    def is_transition(r):
        return (r["is_new_hc"] is True) or (r["returning"] is not None and r["returning"] < OVERHAUL_RETURNING)
    report_cohort("stable", [r for r in rows if not is_transition(r)])
    report_cohort("transition", [r for r in rows if is_transition(r)])

    print("\n— HONEST out-of-sample test (leave-one-season-out) —")
    flat, cond, w_last = loso_conditional(
        rows, lambda r: "T" if is_transition(r) else "S", ["S", "T"])
    print(f"  flat 0.5 served      pooled MAE {flat:.4f}")
    print(f"  transition-cond wt   pooled MAE {cond:.4f}  (lift {flat-cond:+.4f})")
    print(f"  last-fold weights: stable w={w_last.get('S')}, transition w={w_last.get('T')}")

    out = EVAL_DIR / f"transition_blend_diagnostic_{dt.datetime.utcnow():%Y%m%d}_summary.json"
    out.write_text(json.dumps({
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "n": len(rows), "flat_served_mae": mae(rows, 0.5),
        "global_best_w": best_weight(rows)[0],
        "loso_flat_mae": flat, "loso_conditional_mae": cond, "loso_lift": flat - cond,
        "overhaul_threshold": OVERHAUL_RETURNING,
    }, indent=2, default=float))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
