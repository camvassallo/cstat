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
  - returning  : fraction of last season's roster TALENT retained. Since #322
                 this is read straight off the dump (`retained`), which is the
                 EX-ANTE fraction `roster_projection::retained_talent_fraction`
                 computes from the four departure channels — i.e. the exact key
                 the serving ramp uses. Older dumps have no such field, so we
                 fall back to the hindsight proxy this tool used to compute
                 (which of last season's qualifying players appear in the
                 target season, matched by torvik_pid) and say so in the
                 header. The proxy is not the same measurement: it can only be
                 taken after the season it is predicting, and a midseason
                 injury moves it for reasons that have nothing to do with how
                 the roster was built. Tuning a served constant against a
                 cohort the serving code cannot reproduce is the failure this
                 field closes.

For each cohort we report the standalone baseline/talent MAE, a flat-0.5
reference, the MAE and signed bias of the ramp that is ACTUALLY SERVED, and the
cohort's own IN-SAMPLE optimal flat weight. Then an honest leave-one-season-out
test: does a cohort-conditional weight (fit on the other years) beat the flat
0.5 out-of-sample?

Run:  ./.venv/bin/python transition_blend_diagnostic.py --dump eval_history/<dump>.json

Always pass `--dump`. This tool re-tunes PROJECTION_SHRINK_WEIGHT{,_OVERHAUL},
which are SERVED constants, and the no-flag fallback picks the newest dump by
FILENAME rather than by mtime — so it can quietly hand you a superseded
projection generation to tune against.
"""

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path

from sqlalchemy import text

from compute_cae import EVAL_DIR, load_backtest
from db import get_engine
from served_blend import (
    RETAINED_FULL_OVERHAUL,
    RETAINED_FULL_STABLE,
    W_OVERHAUL,
    W_STABLE,
    blend,
    unverified_rows,
    served_weight,
)

START_YEAR = 2019
OVERHAUL_RETURNING = 0.40   # < this fraction of talent retained = "overhaul"
MIN_QUAL = 5                # player qualifying gate (mirror the roster model)

# The served blend lives in `served_blend` — one mirror of
# `roster_projection.rs` for every Python consumer, so a retune touches one
# file instead of however many diagnostics happen to reconstruct the formula.
# `unverified_rows` is what catches that mirror drifting from the Rust.


def load_rows(conn, dump: Path | None = None) -> tuple[list[dict], str]:
    """Backtest dump rows + team_natstat_id + is_new_hc + returning-talent frac.

    Returns `(rows, retention_source)`, where the source is "ex-ante (dump)"
    when the dump carries the served `retained` key and "hindsight (proxy)"
    for pre-#322 dumps.
    """
    bt = load_backtest(dump)
    # Prefer the served ex-ante fraction the dump carries; fall back to the
    # hindsight proxy for older dumps. Decided per-dump, not per-row, so a
    # single run never mixes two different definitions of "overhaul".
    ex_ante = any("retained" in r for r in bt)
    source = "ex-ante (dump `retained`)" if ex_ante else "hindsight (torvik_pid proxy)"

    tid2ns = {r.id: r.natstat_id
              for r in conn.execute(text("SELECT id::text AS id, natstat_id FROM teams"))}

    new_hc = {(r.ns, r.season): r.flag for r in conn.execute(text(
        """SELECT team_natstat_id AS ns, season, is_new_hc AS flag
           FROM coach_seasons WHERE team_natstat_id IS NOT NULL"""))}

    # Per (program, season): {torvik_pid: cam_v3} for qualifying players, to
    # measure season-over-season talent retention by the stable torvik_pid key.
    # Only for the fallback path — this scans every qualifying player-season in
    # the DB, and an ex-ante dump answers the same question without it.
    roster: dict[tuple, dict] = defaultdict(dict)
    if not ex_ante:
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
        if ex_ante:
            retained = r.get("retained")
            retained = None if retained is None else float(retained)
        else:
            retained = returning_frac(ns, r["season"])
        rows.append({
            "ns": ns, "team": r["team_name"], "season": r["season"],
            "actual": float(r["actual"]), "baseline": float(r["baseline"]),
            "roster_proj": float(r["roster_proj"]),
            "is_new_hc": new_hc.get((ns, r["season"])),
            "returning": retained,
            "program_level": r.get("program_level"),
        })
    # The dump records the weight the Rust actually derived, so a drift between
    # `served_blend`'s mirror and the served constants surfaces here — before it
    # is baked into a retuned constant.
    stale = unverified_rows(bt)
    if stale:
        print(f"  ** WARNING: {stale} rows this mirror could not be confirmed "
              f"against (drifted or missing `baseline_weight`). Either the mirror is "
              f"stale against roster_projection.rs or this dump predates the current "
              f"blend — fix that before reading anything below.")
    return rows, source


def mae(rows, w):
    """MAE of the blend w·anchor + (1-w)·roster_proj."""
    return sum(abs(r["actual"] - blend(r, w)) for r in rows) / len(rows)


def ramp_pred(r):
    """Prediction under the SERVED transition ramp (not a flat weight)."""
    return blend(r, served_weight(r["returning"]))


def ramp_mae(rows):
    return sum(abs(r["actual"] - ramp_pred(r)) for r in rows) / len(rows)


def ramp_bias(rows):
    return sum(ramp_pred(r) - r["actual"] for r in rows) / len(rows)


def best_weight(rows):
    """In-sample optimal weight on baseline (grid 0..1 step .05) + its MAE."""
    grid = [(w / 20, mae(rows, w / 20)) for w in range(21)]
    w, m = min(grid, key=lambda t: t[1])
    return w, m


def bias(rows, w):
    """Mean SIGNED error of the blend (pred − actual): + = over-projected."""
    return sum(blend(r, w) - r["actual"] for r in rows) / len(rows)


def report_cohort(name, rows):
    """One cohort's line: the two endpoints (all-baseline / all-roster), the
    flat 0.5 reference, **what this cohort actually gets served**, and its own
    in-sample optimal flat weight.

    The served-ramp column is the one to read when deciding whether a constant
    needs moving — a cohort whose served MAE sits well above its own optimum is
    a cohort the ramp is mis-weighting, and its signed bias says which way.
    Before #322 this line only carried the flat 0.5, which has not been what we
    serve since the ramp shipped."""
    if not rows:
        print(f"  {name:22s} n=0")
        return
    w, m = best_weight(rows)
    print(f"  {name:22s} n={len(rows):4d}  "
          f"base-only {mae(rows,1.0):5.3f}  talent-only {mae(rows,0.0):5.3f}  "
          f"flat(.5) {mae(rows,0.5):5.3f}  "
          f"SERVED {ramp_mae(rows):5.3f} (bias {ramp_bias(rows):+5.2f})  "
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
            cond_ae.append(abs(r["actual"] - blend(r, w)))
            flat_ae.append(abs(r["actual"] - blend(r, 0.5)))
    return sum(flat_ae) / len(flat_ae), sum(cond_ae) / len(cond_ae), w_by


def main():
    # `--dump` matters more here than in the other backtest readers: this tool
    # is what re-tunes PROJECTION_SHRINK_WEIGHT{,_OVERHAUL}, which are SERVED
    # constants in roster_projection.rs. load_backtest()'s fallback picks by
    # filename, not recency, so without this flag a retrain's fresh dump can
    # lose the sort to a months-old descriptively-tagged one and the weights
    # get tuned against a superseded projection generation — the #218 failure
    # mode, one layer over. Pass the dump the retrain just produced.
    ap = argparse.ArgumentParser(
        description="Transition-conditional blend diagnostic: re-tune "
                    "PROJECTION_SHRINK_WEIGHT{,_OVERHAUL} against a backtest dump.")
    ap.add_argument("--dump", type=Path, default=None,
                    help="per-team backtest dump to read (default: newest by "
                         "filename, which is not always newest on disk)")
    args = ap.parse_args()

    with get_engine().connect() as conn:
        rows, source = load_rows(conn, args.dump)
    print(f"\nn = {len(rows)} team-seasons ({START_YEAR}-2026); retention key: {source}")
    print(f"  flat w=0.5 MAE {mae(rows,0.5):.3f}, global best flat w={best_weight(rows)[0]:.2f} "
          f"(MAE {best_weight(rows)[1]:.3f})")
    print(f"  SERVED ramp ({W_STABLE}→{W_OVERHAUL} over retained "
          f"{RETAINED_FULL_STABLE}→{RETAINED_FULL_OVERHAUL})  MAE {ramp_mae(rows):.3f} "
          f"(bias {ramp_bias(rows):+.2f})")
    undefined = sum(1 for r in rows if r["returning"] is None)
    print(f"  retention undefined on {undefined} of {len(rows)} rows "
          f"({undefined/len(rows):.1%}) → served the stable weight by default")

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
    print(f"  flat 0.5             pooled MAE {flat:.4f}")
    print(f"  transition-cond wt   pooled MAE {cond:.4f}  (lift vs flat {flat-cond:+.4f})")
    print(f"  SERVED ramp          pooled MAE {ramp_mae(rows):.4f}  "
          f"(lift vs flat {flat-ramp_mae(rows):+.4f})")
    print(f"  last-fold weights: stable w={w_last.get('S')}, transition w={w_last.get('T')}")
    print("  (the served ramp's two constants were themselves fit on earlier runs of this "
          "corpus, so its line is not fully out-of-sample — read it as the number to beat, "
          "not as a held-out score.)")

    # utcnow() is deprecated; now(UTC) is aware, so drop the tzinfo before
    # formatting to keep the naive `…Z` shape the existing artifacts use.
    now = dt.datetime.now(dt.UTC).replace(tzinfo=None)
    out = EVAL_DIR / f"transition_blend_diagnostic_{now:%Y%m%d}_summary.json"
    out.write_text(json.dumps({
        "generated_at": now.isoformat() + "Z",
        "n": len(rows), "flat_served_mae": mae(rows, 0.5),
        "retention_source": source,
        "served_ramp_mae": ramp_mae(rows), "served_ramp_bias": ramp_bias(rows),
        "retention_undefined_frac": undefined / len(rows),
        "global_best_w": best_weight(rows)[0],
        "loso_flat_mae": flat, "loso_conditional_mae": cond, "loso_lift": flat - cond,
        "overhaul_threshold": OVERHAUL_RETURNING,
    }, indent=2, default=float))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
