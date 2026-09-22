"""Empirical-Bayes blend experiment (#359) — replace the hand-fit program
anchor with a variance-weighted shrinkage whose weight is DERIVED, and judge
it the way the literature says to.

The served blend is `w·anchor + (1−w)·roster_raw` with `w = 0.70`, a
corroboration-gated shrink toward the program's 3-year level, and a
turnover ramp — every knob chosen by minimising MAE on the backtest. This
is the canonical form instead (Efron–Morris; Glickman & Stern's
season-to-season term):

    m_i   = level_i + φ·(baseline_i − level_i)        prior mean: the program's
                                                      level with an estimated
                                                      AR(1) pull from last season
    τ²    = Var(actual − m)                           prior spread, estimated
    σ²_i  = Var(actual − roster_raw | retained bucket) roster-model noise,
                                                      estimated, conditioned on
                                                      roster continuity
    w_i   = σ²_i / (σ²_i + τ²)                        weight on the prior
    pred  = w_i·m_i + (1 − w_i)·roster_raw_i

Three estimated quantities, each fit on TRAINING seasons only, no MAE search.
A row with no program level uses `m = baseline` and its own τ².

Evaluation, per #359's acceptance bar:
  - walk-forward: train on seasons < S, predict S, for S = WALK_FROM..;
    LOSO reported too, for continuity with every prior diagnostic;
  - every quantity (φ, τ², σ²) refit inside each fold — nothing sees the
    held-out season;
  - MAE, bias and Spearman rank correlation (full field, and within the
    predicted top 50), pooled and 2024+, with the cohort table the anchor
    diagnostic prints; paired z against the served blend.

Variants:
  served         the shipped constants (served_blend mirror)
  eb             as above, σ² by retained bucket
  eb_global      σ² pooled over all rows (no continuity conditioning)
  eb_tau_ret     τ² also conditioned on the retained bucket
  eb_no_ar       φ pinned at 0 — prior mean is the program level alone
  eb_ar1         φ pinned at 1 — prior mean is last season alone
  eb_sigma_lvl   σ² by retained bucket × prior-strength tier (elite programs
                 get their own noise estimate)
  eb_tau_dev     τ² grows with |baseline − level|² (estimated slope) — the
                 variance form of the corroboration gate
  eb_band        σ² and τ² conditioned on whether the roster projects inside
                 or outside [level, baseline] — does the served gate fall
                 out of the variances?
  stack_ols      actual ~ level + baseline + roster_raw, least-squares weights
                 fit on training seasons (Pomeroy's shape: one regression sets
                 the program-vs-roster weight jointly)
  stack_ols_ret  the same, fit separately per retained bucket

Run:  cd training && ./.venv/bin/python experiments/eb_blend_experiment.py --dump <dump>.json
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import argparse
import datetime as dt
import json
import math
from pathlib import Path

import numpy as np
from scipy.stats import spearmanr

from compute_cae import EVAL_DIR, load_backtest
from served_blend import served_prediction, unverified_rows

START_YEAR = 2018      # first target with a full program-level window
WALK_FROM = 2021       # first walk-forward test season (≥ 3 training seasons behind it)
RET_EDGES = (0.2, 0.4)  # retained-fraction buckets, the served ramp's own breakpoints
MIN_BUCKET = 40        # fall back to the pooled σ² when a bucket is thinner than this


def ret_bucket(r):
    x = r["retained"]
    if x is None:
        return "na"
    if x < RET_EDGES[0]:
        return "overhaul"
    if x < RET_EDGES[1]:
        return "mid"
    return "stable"


def band(r):
    """Where the roster projects relative to [program level, last season] —
    the served blend's regime. 'in' collapses the served anchor onto the raw."""
    if r["program_level"] is None:
        return "na"
    lo, hi = sorted((r["baseline"], r["program_level"]))
    return "in" if lo < r["roster_proj"] < hi else "out"


def lvl_tier(r):
    lvl = r["program_level"] if r["program_level"] is not None else r["baseline"]
    return "elite" if lvl >= 20 else ("mid" if lvl >= 0 else "low")


def fit_eb(train, *, phi=None, tau_by_ret=False, sigma_by=("ret",)):
    """Estimate φ, τ² and σ² on training rows. Returns a dict of estimates."""
    anch = [r for r in train if r["program_level"] is not None]
    x = np.array([r["baseline"] - r["program_level"] for r in anch])
    y = np.array([r["actual"] - r["program_level"] for r in anch])
    if phi is None:
        phi_hat = float((x * y).sum() / (x * x).sum()) if (x * x).sum() > 0 else 0.0
    else:
        phi_hat = phi
    resid_m = y - phi_hat * x
    est = {"phi": phi_hat, "tau2": float(resid_m.var()), "tau2_ret": {}, "sigma2": {}, "sigma2_pooled": None}
    # Heteroskedastic prior: τ²(dev) = a + k·dev², fit on the squared residuals
    # of the prior mean. A program that just moved far from its level has a
    # less certain level — the principled form of the corroboration gate.
    d2 = x * x
    e2 = resid_m * resid_m
    k = float(((d2 - d2.mean()) * (e2 - e2.mean())).sum() / ((d2 - d2.mean()) ** 2).sum()) if d2.var() > 0 else 0.0
    k = max(k, 0.0)
    est["tau_a"] = max(float(e2.mean() - k * d2.mean()), 1.0)
    est["tau_k"] = k
    if tau_by_ret:
        for b in ("overhaul", "mid", "stable", "na"):
            rs = [(r, e) for r, e in zip(anch, resid_m) if ret_bucket(r) == b]
            est["tau2_ret"][b] = float(np.var([e for _, e in rs])) if len(rs) >= MIN_BUCKET else est["tau2"]
    est["tau2_band"] = {}
    for b in ("in", "out"):
        rs = [e for r, e in zip(anch, resid_m) if band(r) == b]
        est["tau2_band"][b] = float(np.var(rs)) if len(rs) >= MIN_BUCKET else est["tau2"]
    # un-anchored rows: prior is the baseline itself
    un = [r["actual"] - r["baseline"] for r in train if r["program_level"] is None]
    est["tau2_unanchored"] = float(np.var(un)) if len(un) >= MIN_BUCKET else est["tau2"]
    res = np.array([r["actual"] - r["roster_proj"] for r in train])
    est["sigma2_pooled"] = float(res.var())
    keys = {}
    for r, e in zip(train, res):
        k = tuple(ret_bucket(r) if s == "ret" else (band(r) if s == "band" else lvl_tier(r)) for s in sigma_by)
        keys.setdefault(k, []).append(e)
    for k, es in keys.items():
        est["sigma2"][k] = float(np.var(es)) if len(es) >= MIN_BUCKET else est["sigma2_pooled"]
    return est


def predict_eb(r, est, *, sigma_by=("ret",), tau_by_ret=False, tau_by_dev=False, tau_by_band=False):
    k = tuple(ret_bucket(r) if s == "ret" else (band(r) if s == "band" else lvl_tier(r)) for s in sigma_by)
    s2 = est["sigma2"].get(k, est["sigma2_pooled"])
    if r["program_level"] is None:
        m, t2 = r["baseline"], est["tau2_unanchored"]
    else:
        m = r["program_level"] + est["phi"] * (r["baseline"] - r["program_level"])
        t2 = est["tau2_ret"].get(ret_bucket(r), est["tau2"]) if tau_by_ret else est["tau2"]
        if tau_by_dev:
            dev = r["baseline"] - r["program_level"]
            t2 = est["tau_a"] + est["tau_k"] * dev * dev
        if tau_by_band:
            t2 = est["tau2_band"].get(band(r), est["tau2"])
    w = s2 / (s2 + t2)
    return w * m + (1 - w) * r["roster_proj"], w


VARIANTS = {
    "eb": dict(),
    "eb_global": dict(sigma_by=()),
    "eb_tau_ret": dict(tau_by_ret=True),
    "eb_no_ar": dict(phi=0.0),
    "eb_ar1": dict(phi=1.0),
    "eb_sigma_lvl": dict(sigma_by=("ret", "lvl")),
    "eb_tau_dev": dict(tau_by_dev=True),
    "eb_band": dict(sigma_by=("band",), tau_by_band=True),
}


STACK_VARIANTS = {"stack_ols": None, "stack_ols_ret": "ret"}


def _stack_x(r):
    lvl = r["program_level"] if r["program_level"] is not None else r["baseline"]
    return [1.0, lvl, r["baseline"], r["roster_proj"]]


def fit_stack(train, by=None):
    """OLS of actual on [1, level, baseline, roster_raw], optionally per bucket."""
    groups = {}
    for r in train:
        groups.setdefault(ret_bucket(r) if by == "ret" else "all", []).append(r)
    coefs = {}
    X_all = np.array([_stack_x(r) for r in train]); y_all = np.array([r["actual"] for r in train])
    pooled = np.linalg.lstsq(X_all, y_all, rcond=None)[0]
    for g, rs in groups.items():
        if len(rs) < MIN_BUCKET * 3:
            coefs[g] = pooled
            continue
        X = np.array([_stack_x(r) for r in rs]); y = np.array([r["actual"] for r in rs])
        coefs[g] = np.linalg.lstsq(X, y, rcond=None)[0]
    coefs["all"] = coefs.get("all", pooled)
    return coefs


def predict_stack(r, coefs, by=None):
    c = coefs.get(ret_bucket(r) if by == "ret" else "all", coefs["all"])
    return float(np.dot(c, _stack_x(r)))


def run_scheme(rows, scheme):
    """Return {variant: {id(row): pred}} plus per-fold estimates, for LOSO or walk-forward."""
    seasons = sorted({r["season"] for r in rows})
    preds = {v: {} for v in list(VARIANTS) + list(STACK_VARIANTS)}
    preds["served"] = {id(r): served_prediction(r) for r in rows}
    weights = {v: {} for v in VARIANTS}
    fold_est = {v: {} for v in list(VARIANTS) + list(STACK_VARIANTS)}
    for s in seasons:
        if scheme == "walk" and s < WALK_FROM:
            continue
        test = [r for r in rows if r["season"] == s]
        train = [r for r in rows if (r["season"] < s if scheme == "walk" else r["season"] != s)]
        for v, kw in VARIANTS.items():
            fit_kw = {k: kw[k] for k in ("phi", "tau_by_ret", "sigma_by") if k in kw}
            est = fit_eb(train, **fit_kw)
            fold_est[v][s] = {"phi": round(est["phi"], 3), "tau2": round(est["tau2"], 2),
                              "tau_a": round(est["tau_a"], 2), "tau_k": round(est["tau_k"], 4),
                              "sigma2": {"/".join(k) or "pooled": round(x, 2) for k, x in est["sigma2"].items()} or round(est["sigma2_pooled"], 2)}
            pk = {k: kw[k] for k in ("tau_by_ret", "sigma_by", "tau_by_dev", "tau_by_band") if k in kw}
            for r in test:
                p, w = predict_eb(r, est, **pk)
                preds[v][id(r)] = p
                weights[v][id(r)] = w
        for v, by in STACK_VARIANTS.items():
            coefs = fit_stack(train, by)
            fold_est[v][s] = {g: [round(float(x), 3) for x in c] for g, c in coefs.items()}
            for r in test:
                preds[v][id(r)] = predict_stack(r, coefs, by)
    return preds, weights, fold_est


def mae_bias(rows, pred):
    e = [pred[id(r)] - r["actual"] for r in rows if id(r) in pred]
    return (sum(abs(x) for x in e) / len(e), sum(e) / len(e)) if e else (float("nan"), float("nan"))


def paired_z(rows, a, b):
    d = [abs(a[id(r)] - r["actual"]) - abs(b[id(r)] - r["actual"]) for r in rows if id(r) in a and id(r) in b]
    m = sum(d) / len(d)
    sd = math.sqrt(sum((x - m) ** 2 for x in d) / max(len(d) - 1, 1))
    return m, (m / (sd / math.sqrt(len(d))) if sd > 0 else 0.0)


def spearman_by_season(rows, pred, top=None):
    out = []
    for s in sorted({r["season"] for r in rows}):
        rs = [r for r in rows if r["season"] == s and id(r) in pred]
        if len(rs) < 20:
            continue
        if top:
            rs = sorted(rs, key=lambda r: -pred[id(r)])[:top]
        out.append(spearmanr([pred[id(r)] for r in rs], [r["actual"] for r in rs]).correlation)
    return float(np.mean(out)) if out else float("nan")


def top_n(rows, n):
    out = []
    for s in sorted({r["season"] for r in rows}):
        out += sorted((r for r in rows if r["season"] == s), key=lambda r: -r["baseline"])[:n]
    return out


def cohorts(rows):
    c = {"all": rows, "era>=2024": [r for r in rows if r["season"] >= 2024]}
    for n in (10, 25, 50):
        c[f"top{n}"] = top_n(rows, n)
        c[f"top{n} era>=2024"] = [r for r in top_n(rows, n) if r["season"] >= 2024]
    t50 = {id(r) for r in top_n(rows, 50)}

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
    for name in ("houston", "purdue", "illinois"):
        c[f"shape:{name}"] = [r for r in rows if shape(r) == name]
    c["overhaul (<0.2)"] = [r for r in rows if ret_bucket(r) == "overhaul"]
    c["level-changers |dev|>=15"] = [r for r in rows if r["program_level"] is not None and abs(r["baseline"] - r["program_level"]) >= 15]
    return c


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", type=Path, required=True)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    bt = load_backtest(args.dump)
    if unverified_rows(bt):
        raise SystemExit("dump predates the served blend or served_blend.py drifted")
    rows = [{"season": int(r["season"]), "team": r["team_name"], "actual": float(r["actual"]),
             "baseline": float(r["baseline"]), "roster_proj": float(r["roster_proj"]),
             "retained": None if r.get("retained") is None else float(r["retained"]),
             "program_level": None if r.get("program_level") is None else float(r["program_level"])}
            for r in bt if int(r["season"]) >= START_YEAR]
    print(f"rows {len(rows)} ({START_YEAR}-{max(r['season'] for r in rows)})")
    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "n": len(rows), "dump": args.dump.name}
    names = ["served"] + list(VARIANTS) + list(STACK_VARIANTS)
    for scheme in ("walk", "loso"):
        preds, weights, fold_est = run_scheme(rows, scheme)
        eval_rows = [r for r in rows if r["season"] >= (WALK_FROM if scheme == "walk" else START_YEAR)]
        C = cohorts(eval_rows)
        title = f"WALK-FORWARD (test {WALK_FROM}-{max(r['season'] for r in rows)}, fit on earlier seasons only)" if scheme == "walk" else "LEAVE-ONE-SEASON-OUT"
        print(f"\n{'=' * 24} {title} {'=' * 24}")
        print("per-fold estimates (eb):", {s: (e["phi"], e["tau2"]) for s, e in fold_est["eb"].items()})
        print("σ² by retained bucket, last fold (eb):", fold_est["eb"][max(fold_est["eb"])]["sigma2"])
        print("σ² by band, last fold (eb_band):", fold_est["eb_band"][max(fold_est["eb_band"])]["sigma2"])
        wm = {v: float(np.mean([weights[v][id(r)] for r in eval_rows])) for v in VARIANTS}
        print("mean weight on prior:", {v: round(w, 2) for v, w in wm.items()})
        last = max(fold_est["stack_ols"])
        print("stack_ols coefs [1, level, baseline, roster], last fold:", fold_est["stack_ols"][last]["all"])
        print("stack_ols_ret coefs, last fold:", {g: c for g, c in fold_est["stack_ols_ret"][last].items() if g != "all"})
        print(f"\n{'cohort':<20}{'n':>5}" + "".join(f"{v:>17}" for v in names))
        sc = {}
        for cname, crows in C.items():
            line = f"{cname:<20}{len(crows):>5}"
            sc[cname] = {}
            for v in names:
                m, b = mae_bias(crows, preds[v])
                sc[cname][v] = {"mae": m, "bias": b}
                line += f"{m:>9.3f}{b:>+8.2f}"
            print(line)
        print("(cell: MAE  bias)")
        print(f"\n{'Spearman':<20}{'':>5}" + "".join(f"{v:>17}" for v in names))
        sp = {}
        for lab, top in (("field", None), ("pred top-50", 50), ("pred top-25", 25)):
            sp[lab] = {v: spearman_by_season(eval_rows, preds[v], top) for v in names}
            print(f"{lab:<25}" + "".join(f"{sp[lab][v]:>17.3f}" for v in names))
        sp["field 2024+"] = {v: spearman_by_season([r for r in eval_rows if r["season"] >= 2024], preds[v]) for v in names}
        print(f"{'field 2024+':<25}" + "".join(f"{sp['field 2024+'][v]:>17.3f}" for v in names))
        print("\npaired vs served (mean |err| delta, z): pooled | 2024+ | top-25")
        pz = {}
        for v in list(VARIANTS) + list(STACK_VARIANTS):
            pz[v] = {}
            for lab, rs in (("pooled", eval_rows), ("2024+", [r for r in eval_rows if r["season"] >= 2024]), ("top25", top_n(eval_rows, 25))):
                m, z = paired_z(rs, preds[v], preds["served"])
                pz[v][lab] = {"delta": m, "z": z}
            print(f"  {v:<13}" + " | ".join(f"{pz[v][l]['delta']:+.4f} z={pz[v][l]['z']:+.2f}" for l in ("pooled", "2024+", "top25")))
        named = [("McNeese St.", 2025), ("Kennesaw St.", 2023), ("Liberty", 2020), ("Buffalo", 2025), ("Duke", 2026), ("Houston", 2026), ("Illinois", 2026), ("Michigan", 2026)]
        by = {(r["team"], r["season"]): r for r in eval_rows}
        print(f"\n{'named (pred−actual)':<22}" + "".join(f"{v:>13}" for v in names))
        for t, y in named:
            r = by.get((t, y))
            if r and all(id(r) in preds[v] for v in names):
                print(f"{t + ' ' + str(y):<22}" + "".join(f"{preds[v][id(r)] - r['actual']:>+13.2f}" for v in names))
        summary[scheme] = {"cohorts": sc, "spearman": sp, "paired_vs_served": pz, "fold_estimates": fold_est, "mean_weight": wm}

    out = args.out or EVAL_DIR / f"eb_blend_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
