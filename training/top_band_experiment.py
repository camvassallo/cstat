"""Top-band experiment — can the preseason projection be made better at the
TOP of the table (membership in and ordering of the top 25, "team X is
better than team Y") without giving up the absolute AdjEM the opening-week
game predictions and CAE also consume?

Follow-up to #359, whose harness found the served blend is already the best
top-band ORDERER of every variance-weighted form tried: the EB forms win
top-25 MAE only by lifting the whole elite tier together (bias), which does
not reorder anyone. This experiment attacks the other two layers a
"care about the top" objective can enter:

  Layer 2, the roster-impact calibrator (frame from
  `train_roster_impact_model.build_dataset`, walk-forward, each variant's raw
  prediction then run through the served blend). **The calibrator variants
  are compared with each other, against `l2 blend`, not against `served`.**
  The trainer's frame composes each team's rotation from the players who
  ACTUALLY played the target season (their CAM is held-out, their presence
  is not); the backtest dump composes it ex-ante — returners less
  departures, plus commitments. Same rows, same OOF CAM values, same
  params: the ex-post composition scores 5.26 raw walk-forward where the
  ex-ante one scores 5.56, and 4.84 vs 5.41 on level-changers. That 0.30 is
  a train/serve composition skew (the rosters the calibrator learns on are
  cleaner than the ones it scores), and it is the headline of this run, not
  any variant below. See "Result" at the end of this docstring.

    l2          the trainer's own regression objective — the control, refit
                walk-forward so every calibrator variant shares one frame
    l2_topw     the same, rows weighted 3x when the team was top-50 by
                PRIOR-season AdjEM (ex-ante — no outcome in the weight)
    rank        LightGBM `lambdarank` grouped by season, relevance = the
                team's within-season AdjEM percentile in 31 bins (the default
                label gain 2^rel − 1 already concentrates the loss at the
                top); the ranking score is mapped back to AdjEM by isotonic
                regression fit on the training seasons — rank, then
                calibrate. This is the literal "enforce X > Y" objective.
    rank_l2     0.5·rank + 0.5·l2 (in AdjEM units) — does the pairwise
                objective add anything the regression lacks?

  Blend level, the program anchor:

    coach_new   for a team whose head coach is in his FIRST season there and
                has >= 2 prior head-coaching seasons in the data, the program
                level is replaced by the COACH's mean AdjEM over his prior
                three seasons, wherever he coached them (the "coach's past
                3 seasons" question). Every other team: served, unchanged.
    coach_half  the same cohort, level := ½ program + ½ coach.

Metrics, per season then averaged (the eval rows are one dump's ~330 teams a
season, so "actual top 25" means among those):

    pooled MAE / bias, and paired z vs served      the absolute number
    top-25 MAE (ex-ante, by prior-season AdjEM)    the cohort the site is
                                                   judged on
    membership@25   |pred top-25 ∩ actual top-25| / 25 (precision == recall
                    at equal k)
    rho(actual T25) Spearman between predicted and actual AdjEM restricted to
                    the ACTUAL end-of-year top 25 — "did we order the teams
                    that ended up good correctly"; rank-based, so a common
                    shift cannot game it the way actual-top-25 MAE can
    rho(pred T25)   the same restricted to OUR top 25
    concordance     among the actual top 50, the fraction of team pairs whose
                    predicted order matches their actual order — the "X > Y"
                    objective, measured directly
    worst miss T10  max |pred − actual| inside our predicted top 10
    level-changers  MAE on |baseline − level| >= 15, the cohort every
                    top-favouring form has paid on

Result (2026-09-20, walk-forward 2021-2026, n=1958):
  - lambdarank: pooled 7.80 raw / 6.17 blended vs 5.26 / 5.29 for the
    regression control; best pairwise concordance in the actual top 50
    (0.747 vs 0.730) and the WORST ordering of its own top 25 (rho 0.31 vs
    0.52). The pairwise objective buys nothing at the top and costs 0.9-2.4
    AdjEM everywhere. Dead.
  - ex-ante top-50 weighting: within noise of the control on every metric
    (pooled -0.04, top-25 -0.02, rank metrics mixed +/-0.03).
  - coach's prior-3-season AdjEM as the anchor level for new hires (101
    team-seasons): pooled z=-0.07 / -0.93, the cohort itself 5.71 -> 5.70 /
    5.57, level-changers slightly worse. A no-op.
  - Ordering inside the top 25 is a low-ceiling target: the best rho(pred
    T25) any variant reaches is 0.52, membership@25 is 0.70 for all of them.
  Follow-up, DONE the same day (v4 frame): the calibrator now trains on the
  backtest's ex-ante composition (`train_roster_impact_model.build_dataset`
  reads `frames/roster_impact_ex_ante.json`), so on a current tree the
  calibrator variants here ARE end-to-end and `l2 blend` reproduces the
  served number. End-to-end result of that change, paired on 2,961
  team-seasons: in-frame LOSO 5.39 vs backtest 5.42 (agree; v3 had 5.61 vs
  5.49), raw bias +0.24 -> +0.09, served pooled 5.410 -> 5.412 (tie, z=+0.1),
  2024+ -0.05, overhauls -0.12, top-10 (pre-portal era) +0.19 (z=+2.4,
  seed-stable), membership@25 0.676 -> 0.698. The 0.30 walk-forward gap was
  test-set optimism on ex-post rows, not a training advantage — it did not
  bank. Pass `--sql-frame` to reproduce the v3 (ex-post) numbers above.

Run:  cd training && ./.venv/bin/python top_band_experiment.py --dump <dump>.json
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from itertools import combinations
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from scipy.stats import spearmanr
from sklearn.isotonic import IsotonicRegression

from compute_cae import EVAL_DIR, load_backtest
from db import get_engine
from served_blend import served_prediction, unverified_rows
from train_roster_impact_model import build_dataset, build_sql_dataset, lgb_params

START_YEAR = 2018
WALK_FROM = 2021
RANK_BINS = 31          # lambdarank's default label_gain covers labels 0..30
TOP_WEIGHT = 3.0        # l2_topw: weight on ex-ante top-50 rows
COACH_MIN_PRIOR = 2     # coach_new needs this many prior HC seasons
COACH_WINDOW = 3

COACH_QUERY = """
WITH hc AS (
    SELECT cs.coach_id, cs.season, cs.team_natstat_id, cs.is_new_hc,
           tss.adj_efficiency_margin
    FROM coach_seasons cs
    LEFT JOIN teams t ON t.natstat_id = cs.team_natstat_id AND t.season = cs.season
    LEFT JOIN team_season_stats tss ON tss.team_id = t.id AND tss.season = cs.season
)
SELECT cur.season, cur.team_natstat_id AS natstat_id, cur.coach_id, cur.is_new_hc,
       prev.season AS prev_season, prev.adj_efficiency_margin AS prev_adj_em
FROM hc cur
LEFT JOIN hc prev ON prev.coach_id = cur.coach_id
                 AND prev.season BETWEEN cur.season - %(window)s AND cur.season - 1
ORDER BY cur.season, cur.team_natstat_id, prev.season
"""


def natstat_map() -> dict[str, str]:
    """team UUID (any season) -> natstat_id. UUIDs are season-scoped: the dump
    keys a target-season row by its BASE-season team id, the calibrator frame
    by the target-season id, so both are re-keyed on the cross-season id."""
    df = pd.read_sql("SELECT id, natstat_id FROM teams", get_engine())
    return dict(zip(df["id"].astype(str), df["natstat_id"].astype(str)))


def load_coach_levels(seasons: list[int]) -> dict[tuple[str, int], dict]:
    """(natstat_id, season) -> {new: bool, level: mean prior AdjEM or None, n: prior seasons}."""
    df = pd.read_sql(COACH_QUERY, get_engine(), params={"window": COACH_WINDOW})
    out = {}
    for (tid, s), g in df.groupby(["natstat_id", "season"]):
        prior = g["prev_adj_em"].dropna()
        # is_new_hc is coachdict's flag; back it up with "no row for this
        # coach at this team last season" so a missing flag cannot hide a hire.
        new = bool(g["is_new_hc"].iloc[0])
        out[(str(tid), int(s))] = {"new": new, "n": int(len(prior)),
                                   "level": float(prior.mean()) if len(prior) else None}
    return out


# ---------------------------------------------------------------- calibrator
def _fit_l2(train: pd.DataFrame, cols, weight=None):
    p = lgb_params()
    p.pop("early_stopping_rounds", None)
    # Walk-forward: early-stop on the most recent training season, which is
    # ex-ante for the test season (the trainer stops on the held-out fold
    # itself, which #199 documents as by-design for its in-frame number).
    last = train["season"].max()
    va = train[train["season"] == last]
    tr = train[train["season"] < last]
    m = lgb.LGBMRegressor(**p)
    m.fit(tr[cols], tr["adj_efficiency_margin"],
          sample_weight=None if weight is None else weight.loc[tr.index],
          eval_set=[(va[cols], va["adj_efficiency_margin"])],
          callbacks=[lgb.early_stopping(80, verbose=False)])
    return m


def _rank_labels(train: pd.DataFrame) -> pd.Series:
    return (train.groupby("season")["adj_efficiency_margin"]
            .rank(pct=True).mul(RANK_BINS - 1e-9).astype(int).clip(0, RANK_BINS - 1))


def _fit_rank(train: pd.DataFrame, cols):
    p = lgb_params()
    p.pop("early_stopping_rounds", None)
    p.update({"objective": "lambdarank", "metric": "ndcg", "eval_at": [25, 50]})
    train = train.sort_values("season")
    labels = _rank_labels(train)
    last = train["season"].max()
    tr, va = train[train["season"] < last], train[train["season"] == last]
    m = lgb.LGBMRanker(**p)
    m.fit(tr[cols], labels.loc[tr.index], group=tr.groupby("season").size().tolist(),
          eval_set=[(va[cols], labels.loc[va.index])], eval_group=[[len(va)]],
          callbacks=[lgb.early_stopping(80, verbose=False)])
    # Rank, then calibrate: the ranker's score has no units; map it to AdjEM
    # with a monotone fit on the training seasons.
    iso = IsotonicRegression(out_of_bounds="clip").fit(m.predict(train[cols]), train["adj_efficiency_margin"])
    return m, iso


def calibrator_walk(frame: pd.DataFrame, cols, top50: set) -> dict[str, dict]:
    """{variant: {(team_id, season): raw pred}} walk-forward from WALK_FROM."""
    out = {v: {} for v in ("l2", "l2_topw", "rank", "rank_l2")}
    w = pd.Series(np.where([(t, s) in top50 for t, s in zip(frame["team_id"], frame["season"])],
                           TOP_WEIGHT, 1.0), index=frame.index)
    for s in sorted(frame["season"].unique()):
        if s < WALK_FROM:
            continue
        train, test = frame[frame["season"] < s], frame[frame["season"] == s]
        keys = list(zip(test["team_id"], test["season"]))
        l2 = _fit_l2(train, cols).predict(test[cols])
        l2w = _fit_l2(train, cols, w).predict(test[cols])
        rk, iso = _fit_rank(train, cols)
        rank = iso.predict(rk.predict(test[cols]))
        for k, a, b, c in zip(keys, l2, l2w, rank):
            out["l2"][k] = float(a)
            out["l2_topw"][k] = float(b)
            out["rank"][k] = float(c)
            out["rank_l2"][k] = 0.5 * float(a) + 0.5 * float(c)
        print(f"  fold {s}: n_train {len(train)}, n_test {len(test)}, rank iters {rk.best_iteration_}")
    return out


# ------------------------------------------------------------------- metrics
def per_season(rows, pred, fn):
    vals = []
    for s in sorted({r["season"] for r in rows}):
        rs = [r for r in rows if r["season"] == s and id(r) in pred]
        if len(rs) >= 50:
            vals.append(fn(rs, pred))
    return float(np.mean(vals)) if vals else float("nan")


def membership25(rs, pred):
    p = {id(r) for r in sorted(rs, key=lambda r: -pred[id(r)])[:25]}
    a = {id(r) for r in sorted(rs, key=lambda r: -r["actual"])[:25]}
    return len(p & a) / 25


def rho_actual25(rs, pred):
    top = sorted(rs, key=lambda r: -r["actual"])[:25]
    return spearmanr([pred[id(r)] for r in top], [r["actual"] for r in top]).correlation


def rho_pred25(rs, pred):
    top = sorted(rs, key=lambda r: -pred[id(r)])[:25]
    return spearmanr([pred[id(r)] for r in top], [r["actual"] for r in top]).correlation


def rho_field(rs, pred):
    return spearmanr([pred[id(r)] for r in rs], [r["actual"] for r in rs]).correlation


def concordance50(rs, pred):
    top = sorted(rs, key=lambda r: -r["actual"])[:50]
    ok = tot = 0
    for a, b in combinations(top, 2):
        d = (pred[id(a)] - pred[id(b)]) * (a["actual"] - b["actual"])
        if d != 0:
            tot += 1
            ok += d > 0
    return ok / tot


def worst_miss10(rs, pred):
    top = sorted(rs, key=lambda r: -pred[id(r)])[:10]
    return max(abs(pred[id(r)] - r["actual"]) for r in top)


def mae_bias(rows, pred):
    e = [pred[id(r)] - r["actual"] for r in rows if id(r) in pred]
    return (sum(abs(x) for x in e) / len(e), sum(e) / len(e)) if e else (float("nan"), float("nan"))


def paired_z(rows, a, b):
    d = [abs(a[id(r)] - r["actual"]) - abs(b[id(r)] - r["actual"]) for r in rows if id(r) in a and id(r) in b]
    m = sum(d) / len(d)
    sd = math.sqrt(sum((x - m) ** 2 for x in d) / max(len(d) - 1, 1))
    return m, (m / (sd / math.sqrt(len(d))) if sd > 0 else 0.0)


def top_n(rows, n):
    out = []
    for s in sorted({r["season"] for r in rows}):
        out += sorted((r for r in rows if r["season"] == s), key=lambda r: -r["baseline"])[:n]
    return out


METRICS = (("membership@25", membership25), ("rho(actual T25)", rho_actual25),
           ("rho(pred T25)", rho_pred25), ("concordance T50", concordance50),
           ("worst miss T10", worst_miss10), ("rho(field)", rho_field))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", type=Path, required=True)
    ap.add_argument("--sql-frame", action="store_true",
                    help="use the v3 ex-post SQL frame (who actually played) instead of the served ex-ante frame")
    args = ap.parse_args()
    bt = load_backtest(args.dump)
    if unverified_rows(bt):
        raise SystemExit("dump predates the served blend or served_blend.py drifted")
    nsid = natstat_map()
    rows = [{"season": int(r["season"]), "team": r["team_name"], "team_id": nsid[str(r["team_id"])],
             "actual": float(r["actual"]), "baseline": float(r["baseline"]),
             "roster_proj": float(r["roster_proj"]),
             "retained": None if r.get("retained") is None else float(r["retained"]),
             "program_level": None if r.get("program_level") is None else float(r["program_level"])}
            for r in bt if int(r["season"]) >= START_YEAR]
    by_key = {(r["team_id"], r["season"]): r for r in rows}
    top50 = {(r["team_id"], r["season"]) for r in top_n(rows, 50)}
    print(f"dump rows {len(rows)}")

    print("calibrator frame …")
    frame, cols, _ = (build_sql_dataset if args.sql_frame else build_dataset)()
    frame = frame[frame["season"] >= START_YEAR - 3].reset_index(drop=True)
    frame["team_id"] = frame["team_id"].astype(str).map(nsid)
    overlap = sum((t, s) in by_key for t, s in zip(frame["team_id"], frame["season"]))
    print(f"frame rows {len(frame)}, {len(cols)} features; joinable to dump: {overlap}")
    cal = calibrator_walk(frame, cols, top50)

    coach = load_coach_levels(sorted({r["season"] for r in rows}))
    eval_rows = [r for r in rows if r["season"] >= WALK_FROM]

    # Assemble predictions. Calibrator variants: raw and served-blended.
    preds: dict[str, dict] = {"served": {id(r): served_prediction(r) for r in eval_rows}}
    for v, raw in cal.items():
        preds[f"{v} raw"] = {}
        preds[f"{v} blend"] = {}
        for r in eval_rows:
            k = (r["team_id"], r["season"])
            if k in raw:
                preds[f"{v} raw"][id(r)] = raw[k]
                preds[f"{v} blend"][id(r)] = served_prediction({**r, "roster_proj": raw[k]})
    n_new = n_used = 0
    for v, mix in (("coach_new", 1.0), ("coach_half", 0.5)):
        preds[v] = {}
        for r in eval_rows:
            c = coach.get((r["team_id"], r["season"]))
            if c and c["new"]:
                n_new += v == "coach_new"
            if c and c["new"] and c["n"] >= COACH_MIN_PRIOR and r["program_level"] is not None:
                n_used += v == "coach_new"
                lvl = mix * c["level"] + (1 - mix) * r["program_level"]
                preds[v][id(r)] = served_prediction({**r, "program_level": lvl})
            else:
                preds[v][id(r)] = preds["served"][id(r)]
    print(f"new-HC team-seasons in eval: {n_new}; with >= {COACH_MIN_PRIOR} prior HC seasons and a program level: {n_used}")

    # Restrict every comparison to rows every variant covers (the frame can
    # miss a dump team-season, and vice versa).
    common = [r for r in eval_rows if all(id(r) in p for p in preds.values())]
    print(f"eval rows with every variant: {len(common)} of {len(eval_rows)}")
    names = list(preds)
    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "dump": args.dump.name,
               "n": len(common), "walk_from": WALK_FROM, "variants": names, "metrics": {}, "cohorts": {}, "paired_vs_served": {}}

    cohorts = {"all": common, "top25 (ex-ante)": top_n(common, 25), "top10 (ex-ante)": top_n(common, 10),
               "era>=2024": [r for r in common if r["season"] >= 2024],
               "level-changers |dev|>=15": [r for r in common if r["program_level"] is not None and abs(r["baseline"] - r["program_level"]) >= 15],
               "new HC (coach cohort)": [r for r in common if (c := coach.get((r["team_id"], r["season"]))) and c["new"] and c["n"] >= COACH_MIN_PRIOR and r["program_level"] is not None]}
    print(f"\n{'cohort':<26}{'n':>5}" + "".join(f"{v:>18}" for v in names))
    for cname, crows in cohorts.items():
        line = f"{cname:<26}{len(crows):>5}"
        summary["cohorts"][cname] = {}
        for v in names:
            m, b = mae_bias(crows, preds[v])
            summary["cohorts"][cname][v] = {"mae": m, "bias": b}
            line += f"{m:>10.3f}{b:>+8.2f}"
        print(line)
    print("(cell: MAE  bias)")

    print(f"\n{'metric (per-season mean)':<26}{'':>5}" + "".join(f"{v:>18}" for v in names))
    for label, fn in METRICS:
        summary["metrics"][label] = {v: per_season(common, preds[v], fn) for v in names}
        print(f"{label:<31}" + "".join(f"{summary['metrics'][label][v]:>18.3f}" for v in names))

    summary["paired_vs_l2_blend"] = {}
    for ref, key, vs in (("served", "paired_vs_served", [v for v in names[1:] if v.startswith("coach")]),
                         ("l2 blend", "paired_vs_l2_blend", [v for v in names[1:] if not v.startswith("coach") and v != "l2 blend"])):
        print(f"\npaired |err| vs {ref} (delta, z; negative = better): pooled | top-25 | level-changers")
        for v in vs:
            summary[key][v] = {}
            cells = []
            for lab, rs in (("pooled", common), ("top25", cohorts["top25 (ex-ante)"]), ("levelchg", cohorts["level-changers |dev|>=15"])):
                m, z = paired_z(rs, preds[v], preds[ref])
                summary[key][v][lab] = {"delta": m, "z": z}
                cells.append(f"{m:+.3f} z={z:+.2f}")
            print(f"  {v:<14}" + " | ".join(cells))
    m, z = paired_z(common, preds["l2 raw"], {id(r): r["roster_proj"] for r in common})
    summary["composition_skew"] = {"frame_raw_mae": mae_bias(common, preds["l2 raw"])[0],
                                   "dump_raw_mae": mae_bias(common, {id(r): r["roster_proj"] for r in common})[0], "delta": m, "z": z}
    print(f"\ncomposition skew — same rows, ex-post frame raw vs ex-ante dump raw: {m:+.3f} z={z:+.2f}")

    out = EVAL_DIR / f"top_band_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
