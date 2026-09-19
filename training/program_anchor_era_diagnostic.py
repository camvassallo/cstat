"""Program-anchor era diagnostic — is the #326 anchor fit to a regime the
modern portal era no longer follows, and does it hurt the top of the board?

#326 anchored the blend on last season shrunk toward the program's 3-year
level by the part of the move the roster does not corroborate, and refit
the baseline weight 0.30 -> 0.70. It was validated on pooled MAE and on
risers/fallers cohorts. It was never scored on the ELITE cohort or split by
era, and two things hide inside it:

  1. Whenever `roster_proj` lands between `program_level` and `baseline`
     (0 < corroboration < 1) the algebra gives `anchor == roster_proj`, so
     the served number is the raw roster-model output with no anchoring at
     all — 43% of the live board.
  2. The constants were fit on pooled 2018-2026. Pre-2024 the top of the
     table regressed (old blend over-projected top-25 by +1.4); 2024-2026 it
     inverted (raw under-projects top-25 by -3.9) and the anchor, by handing
     elite one-great-year teams to the raw regime, removed the baseline's
     partial correction of that.

This scores the served blend against alternatives, every one fit
leave-one-season-out, and reports the cohorts the PR did not: top-N by
last-season AdjEM, the era split, and the three shapes named in the audit
(an elite team whose roster gutted with last season at/below its level; the
same with last season above its level; an elite one-great-year team whose
roster lands inside the [level, baseline] band).

Variants
  served            the shipped constants (s=1.0, w=0.70/0.55; un-anchored 0.30/0.20)
  pre_326           anchor = baseline, w=0.30/0.20
  fit_all           (w, s) refit LOSO on every other season — reproduces the PR fit
  fit_s{X}          s pinned at X, w refit LOSO — can the anchor be stopped from
                    collapsing to raw?
  fit_modern        (w, s) refit LOSO on other seasons >= MODERN_FROM only,
                    evaluated on the modern folds; pre-modern folds keep fit_all
  fit_recency       (w, s) refit LOSO with rows weighted exp(-(latest - season)/TAU)
  flat_shrink       the PR's rejected un-gated shrink: anchor = baseline - s*dev
  coach_level       program level replaced by the mean over the prior seasons the
                    SAME head coach ran this program (>= 2 of the last 3);
                    falls back to the program level when the coach is newer
  coach_level_strict  as above, but a new-coach team is un-anchored instead

Run:  cd training && ./.venv/bin/python program_anchor_era_diagnostic.py --dump <dump>.json

Needs a post-#326 dump (carries `program_level` / `retained`); the 2016-2017
targets are un-anchored by construction (their 3-season window predates the
ingested data) and take the un-anchored pair in every variant.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from pathlib import Path

from sqlalchemy import text

from compute_cae import EVAL_DIR, load_backtest
from db import get_engine
from served_blend import (
    RETAINED_FULL_OVERHAUL,
    RETAINED_FULL_STABLE,
    W_OVERHAUL,
    W_STABLE,
    W_STABLE_UNANCHORED,
    served_weight,
    unverified_rows,
)

START_YEAR = 2018          # first target with a full 3-season window behind it
MODERN_FROM = 2022         # first season the portal + 5-in-5 churn is the norm
TAU = 3.0                  # recency e-folding, seasons
W_GRID = [round(0.05 * i, 2) for i in range(2, 19)]      # 0.10 .. 0.90
S_GRID = [0.0, 0.25, 0.5, 0.75, 1.0]
TOP_N = (10, 25, 50)
NAMED = [
    ("McNeese St.", 2025), ("Kennesaw St.", 2023), ("Liberty", 2020),
    ("Buffalo", 2025), ("Duke", 2026), ("Illinois", 2026), ("Houston", 2026),
    ("Purdue", 2026), ("Arizona", 2026), ("Michigan", 2026),
]


# --- blend ---------------------------------------------------------------
def anchor_gated(baseline, level, raw, s):
    """#326's corroboration-gated anchor with the shrink parametrised."""
    if level is None:
        return baseline
    dev = baseline - level
    if abs(dev) < 1e-3:
        return baseline
    u = min(max(1.0 - (raw - level) / dev, 0.0), 1.0)
    return baseline - s * u * dev


def anchor_flat(baseline, level, raw, s):
    if level is None:
        return baseline
    return baseline - s * (baseline - level)


def weight_for(r, w_stable, level):
    """The turnover ramp at an arbitrary stable weight (overhaul end scaled)."""
    if level is None:
        return served_weight(r["retained"], anchored=False)
    return served_weight(r["retained"], anchored=True) * (w_stable / W_STABLE)


def in_band(r, level):
    """Is the roster projection between the program level and last season?
    That is the regime in which the gated anchor collapses to `roster_proj`."""
    if level is None:
        return False
    lo, hi = sorted((r["baseline"], level))
    return lo < r["roster_proj"] < hi


def predict(r, w_stable, s, level_key="program_level", anchor_fn=anchor_gated,
            mode="ramp", extra=None):
    """`mode` selects how the baseline weight is formed:
      ramp     the served turnover ramp at `w_stable`
      dist     the ramp, decayed by how far the roster disagrees with the
               anchor: w / (1 + (|raw - anchor| / lam)^2), lam = extra
      regime   w_stable inside the [level, baseline] band, extra outside it
      steep    the ramp with the overhaul end at `extra` (not scaled)"""
    level = r.get(level_key)
    a = anchor_fn(r["baseline"], level, r["roster_proj"], s)
    if mode == "steep" and level is not None and r["retained"] is not None:
        ret = r["retained"]
        if ret >= RETAINED_FULL_STABLE:
            w = w_stable
        elif ret <= RETAINED_FULL_OVERHAUL:
            w = extra
        else:
            t = (ret - RETAINED_FULL_OVERHAUL) / (RETAINED_FULL_STABLE - RETAINED_FULL_OVERHAUL)
            w = extra + t * (w_stable - extra)
    elif mode == "regime" and level is not None:
        w = weight_for(r, w_stable if in_band(r, level) else extra, level)
    else:
        w = weight_for(r, w_stable, level)
    if mode == "dist" and level is not None:
        w = w / (1.0 + (abs(r["roster_proj"] - a) / extra) ** 2)
    return w * a + (1.0 - w) * r["roster_proj"]


def fit(rows, s_grid, w_grid, level_key="program_level", anchor_fn=anchor_gated,
        weights=None, mode="ramp", extra_grid=(None,)):
    best = None
    for s in s_grid:
        for w in w_grid:
            for x in extra_grid:
                if weights is None:
                    loss = sum(abs(predict(r, w, s, level_key, anchor_fn, mode, x) - r["actual"])
                               for r in rows) / len(rows)
                else:
                    num = sum(wt * abs(predict(r, w, s, level_key, anchor_fn, mode, x) - r["actual"])
                              for r, wt in zip(rows, weights))
                    loss = num / sum(weights)
                if best is None or loss < best[0]:
                    best = (loss, w, s, x)
    return best[1], best[2], best[3]


# --- data ----------------------------------------------------------------
def load_rows(conn, dump: Path) -> list[dict]:
    bt = load_backtest(dump)
    stale = unverified_rows(bt)
    if stale:
        raise SystemExit(f"{stale} rows could not be confirmed against served_blend — "
                         f"this dump predates #326 or the mirror has drifted.")
    tid2ns = {r.id: r.natstat_id
              for r in conn.execute(text("SELECT id::text AS id, natstat_id FROM teams"))}
    # Coach-tenure level: mean AdjEM over the prior 3 seasons in which the
    # base-season head coach was ALSO the head coach here. The program level
    # answers "what is this program"; this answers "what is this program under
    # this coach", and resets on a coaching change.
    coach_level = {}
    for r in conn.execute(text("""
        WITH base AS (
            SELECT t.id AS team_id, t.natstat_id AS ns, t.season, cs.coach_id
            FROM teams t
            JOIN coach_seasons cs ON cs.team_natstat_id = t.natstat_id AND cs.season = t.season
        )
        SELECT b.team_id::text AS team_id, AVG(tss.adj_efficiency_margin) AS level, count(*) AS n
        FROM base b
        JOIN coach_seasons ch ON ch.team_natstat_id = b.ns AND ch.coach_id = b.coach_id
                             AND ch.season BETWEEN b.season - 3 AND b.season - 1
        JOIN teams th ON th.natstat_id = b.ns AND th.season = ch.season
        JOIN team_season_stats tss ON tss.team_id = th.id AND tss.adj_efficiency_margin IS NOT NULL
        GROUP BY b.team_id HAVING count(*) >= 2
    """)):
        coach_level[r.team_id] = float(r.level)
    new_hc = {(r.ns, r.season): r.flag for r in conn.execute(text(
        "SELECT team_natstat_id AS ns, season, is_new_hc AS flag FROM coach_seasons "
        "WHERE team_natstat_id IS NOT NULL"))}

    rows = []
    for r in bt:
        if r["season"] < START_YEAR:
            continue
        ns = tid2ns.get(r["team_id"])
        pl = r.get("program_level")
        cl = coach_level.get(r["team_id"])
        rows.append({
            "team": r["team_name"], "season": int(r["season"]), "ns": ns,
            "actual": float(r["actual"]), "baseline": float(r["baseline"]),
            "roster_proj": float(r["roster_proj"]),
            "retained": None if r.get("retained") is None else float(r["retained"]),
            "program_level": None if pl is None else float(pl),
            "coach_level": cl if cl is not None else pl,
            "coach_level_strict": cl,
            "is_new_hc": new_hc.get((ns, r["season"])),
        })
    return rows


# --- cohorts -------------------------------------------------------------
def top_n(rows, n):
    out = []
    for s in sorted({r["season"] for r in rows}):
        out += sorted((r for r in rows if r["season"] == s),
                      key=lambda r: -r["baseline"])[:n]
    return out


def cohorts(rows):
    t50 = set(id(r) for r in top_n(rows, 50))
    anchored = [r for r in rows if r["program_level"] is not None]

    def shape(r):
        b, L, x = r["baseline"], r["program_level"], r["roster_proj"]
        if L is None or id(r) not in t50:
            return None
        if x < b - 5 and b <= L:
            return "houston"
        if x < b - 5 and b > L and x < L:
            return "purdue"
        if b > L + 5 and L < x < b:
            return "illinois"
        return None

    c = {
        "all": rows,
        "anchored": anchored,
        f"era<={MODERN_FROM - 1}": [r for r in rows if r["season"] < MODERN_FROM],
        f"era>={MODERN_FROM}": [r for r in rows if r["season"] >= MODERN_FROM],
        "era>=2024": [r for r in rows if r["season"] >= 2024],
    }
    for n in TOP_N:
        c[f"top{n}"] = top_n(rows, n)
        c[f"top{n} era>=2024"] = [r for r in top_n(rows, n) if r["season"] >= 2024]
    for name in ("houston", "purdue", "illinois"):
        c[f"shape:{name}"] = [r for r in rows if shape(r) == name]
    c["new_hc"] = [r for r in rows if r["is_new_hc"]]
    return c


def score(rows, preds):
    n = len(rows)
    if n == 0:
        return {"n": 0}
    err = [preds[id(r)] - r["actual"] for r in rows]
    return {"n": n, "mae": sum(abs(e) for e in err) / n, "bias": sum(err) / n}


def paired_z(rows, preds_a, preds_b):
    d = [abs(preds_a[id(r)] - r["actual"]) - abs(preds_b[id(r)] - r["actual"]) for r in rows]
    n = len(d)
    m = sum(d) / n
    sd = math.sqrt(sum((x - m) ** 2 for x in d) / max(n - 1, 1))
    return m, (m / (sd / math.sqrt(n)) if sd > 0 else 0.0)


# --- variants ------------------------------------------------------------
def run_variant(rows, name, *, s_grid=S_GRID, w_grid=W_GRID, level_key="program_level",
                anchor_fn=anchor_gated, fixed=None, fold_filter=None, recency=False,
                mode="ramp", extra_grid=(None,)):
    """LOSO: fit on every other season (optionally filtered / weighted),
    predict the held-out one. Returns (preds, per-fold params)."""
    seasons = sorted({r["season"] for r in rows})
    latest = max(seasons)
    preds, params = {}, {}
    for held in seasons:
        test = [r for r in rows if r["season"] == held]
        if fixed is not None:
            w, s = fixed
            x = None
        else:
            train = [r for r in rows if r["season"] != held]
            if fold_filter is not None:
                train = [r for r in train if fold_filter(r)]
            wts = None
            if recency:
                wts = [math.exp(-(latest - r["season"]) / TAU) for r in train]
            w, s, x = fit(train, s_grid, w_grid, level_key, anchor_fn, wts, mode, extra_grid)
        params[held] = (w, s, x)
        for r in test:
            preds[id(r)] = predict(r, w, s, level_key, anchor_fn, mode, x)
    return preds, params


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--dump", type=Path, required=True)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    with get_engine().connect() as conn:
        rows = load_rows(conn, args.dump)
    print(f"rows: {len(rows)}  targets {min(r['season'] for r in rows)}-"
          f"{max(r['season'] for r in rows)}; anchored "
          f"{sum(r['program_level'] is not None for r in rows)}; coach-tenure level "
          f"available {sum(r['coach_level_strict'] is not None for r in rows)}")

    variants = {}
    variants["served"] = run_variant(rows, "served", fixed=(W_STABLE, 1.0))
    variants["pre_326"] = run_variant(rows, "pre_326", fixed=(W_STABLE_UNANCHORED, 0.0))
    variants["raw_only"] = run_variant(rows, "raw_only", fixed=(0.0, 0.0))
    variants["fit_all"] = run_variant(rows, "fit_all")
    for s in (0.25, 0.5, 0.75):
        variants[f"fit_s{s}"] = run_variant(rows, f"fit_s{s}", s_grid=[s])
    modern = run_variant(rows, "fit_modern", fold_filter=lambda r: r["season"] >= MODERN_FROM)
    # Pre-modern folds are not what fit_modern is about; splice fit_all there
    # so the pooled number is comparable and the modern rows carry the test.
    spliced = dict(variants["fit_all"][0])
    for r in rows:
        if r["season"] >= MODERN_FROM:
            spliced[id(r)] = modern[0][id(r)]
    variants["fit_modern"] = (spliced, modern[1])
    variants["fit_recency"] = run_variant(rows, "fit_recency", recency=True)
    variants["flat_shrink"] = run_variant(rows, "flat_shrink", anchor_fn=anchor_flat)
    variants["coach_level"] = run_variant(rows, "coach_level", level_key="coach_level")
    variants["coach_level_strict"] = run_variant(rows, "coach_level_strict",
                                                 level_key="coach_level_strict")
    # History counts LESS the further the roster disagrees with it.
    variants["dist_decay"] = run_variant(rows, "dist_decay", mode="dist",
                                         extra_grid=(4.0, 6.0, 8.0, 10.0, 15.0, 20.0, 30.0))
    variants["dist_decay_s"] = run_variant(rows, "dist_decay_s", mode="dist", s_grid=[0.5, 0.75, 1.0],
                                           extra_grid=(6.0, 8.0, 10.0, 15.0, 20.0))
    # A separate weight for a roster projecting OUTSIDE the [level, baseline] band.
    variants["regime_w"] = run_variant(rows, "regime_w", mode="regime", s_grid=[1.0],
                                       extra_grid=tuple(W_GRID))
    variants["regime_w_s"] = run_variant(rows, "regime_w_s", mode="regime", s_grid=[0.5, 0.75, 1.0],
                                         extra_grid=tuple(W_GRID))
    # A steeper turnover ramp: the overhaul end fit freely instead of scaled.
    variants["steep_ramp"] = run_variant(rows, "steep_ramp", mode="steep", s_grid=[1.0],
                                         extra_grid=tuple(W_GRID))

    C = cohorts(rows)
    names = list(variants)
    print("\nper-fold params (w_stable, s):")
    for v in names:
        p = variants[v][1]
        print(f"  {v:<20}", "  ".join(
            f"{k}:{w:.2f}/{s:.2f}" + (f"/{x:g}" if x is not None else "")
            for k, (w, s, x) in sorted(p.items())))

    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
               "dump": args.dump.name, "n": len(rows), "start_year": START_YEAR,
               "modern_from": MODERN_FROM, "tau": TAU, "cohorts": {}, "named": {},
               "params": {v: {str(k): list(p) for k, p in variants[v][1].items()} for v in names}}

    print()
    hdr = f"{'cohort':<24}{'n':>6} " + "".join(f"{v:>19}" for v in names)
    print(hdr)
    for cname, crows in C.items():
        line = f"{cname:<24}{len(crows):>6} "
        summary["cohorts"][cname] = {}
        for v in names:
            sc = score(crows, variants[v][0])
            summary["cohorts"][cname][v] = sc
            line += f"{sc['mae']:>10.3f}{sc['bias']:>+8.2f} " if sc["n"] else f"{'-':>19}"
        print(line)
    print("(each cell: MAE  bias)")

    print("\npaired vs served, pooled (mean |err| delta, z; negative = better than served):")
    for v in names:
        if v == "served":
            continue
        m, z = paired_z(rows, variants[v][0], variants["served"][0])
        summary.setdefault("paired_vs_served", {})[v] = {"delta": m, "z": z}
        print(f"  {v:<20} {m:+.4f}  z={z:+.2f}")
    print("paired vs served, era>=2024:")
    mod = [r for r in rows if r["season"] >= 2024]
    for v in names:
        if v == "served":
            continue
        m, z = paired_z(mod, variants[v][0], variants["served"][0])
        summary["paired_vs_served"][v]["era>=2024"] = {"delta": m, "z": z}
        print(f"  {v:<20} {m:+.4f}  z={z:+.2f}")

    print("\nnamed cases (signed error = pred - actual):")
    print(f"{'team':<16}{'yr':>5}{'base':>7}{'lvl':>7}{'raw':>7}{'act':>7} " +
          "".join(f"{v:>13}" for v in names))
    by_key = {(r["team"], r["season"]): r for r in rows}
    for team, yr in NAMED:
        r = by_key.get((team, yr))
        if r is None:
            continue
        lvl = r["program_level"]
        print(f"{team:<16}{yr:>5}{r['baseline']:>7.1f}{(lvl if lvl is not None else float('nan')):>7.1f}"
              f"{r['roster_proj']:>7.1f}{r['actual']:>7.1f} " +
              "".join(f"{variants[v][0][id(r)] - r['actual']:>+13.2f}" for v in names))
        summary["named"][f"{team} {yr}"] = {v: variants[v][0][id(r)] - r["actual"] for v in names}

    out = args.out or EVAL_DIR / f"program_anchor_era_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
