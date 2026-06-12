"""
Multi-season pooled RAPM spike — the section-10 design, go/no-go.

Implements shape (c) from docs/rapm_methodology.md section 10 (decayed-window
refit, the literature-standard "3-year RAPM"): fit a target season on its own
paired stints plus career-matched stints from the prior 1-2 seasons at
decayed weight, with coefficients keyed by CAREER (dual-key chains:
natstat_id OR torvik_pid union-find; unresolvable players stay season-scoped
singletons — honest degradation).

The section-10.3 honest acceptance suite (pooled YoY Spearman is
mechanically inflated, so it is NOT used):

1. PREQUENTIAL (the headline): the estimate fitted through season t predicts
   season t+1's stint margins. Per row, effects = sum of the ten on-floor
   players' carried-over O/D coefficients (unknown players contribute 0 —
   college roster churn is part of the test); only a 2-parameter
   intercept+HCA recentering is fit on the target season, identically for
   every competitor. Competitors on identical rows: single-season RAPM,
   carried-over team-columns ridge (teams mapped by natstat_id), CamPom
   decomposition (scale calibrated on season t, never the target), and
   intercept+HCA alone.
2. SPLIT-HALF reliability: odd/even game split within the fit window, refit
   both halves, correlate coefficients across halves (rotation floor both
   halves). Reported for net AND the O / D blocks separately — the user
   question "is D-RAPM the noisy side, and does pooling fix it" drops out
   of this table.
3. STAR-SEPARATION: the percentile-gap-by-tier diagnostic from the
   single-season finding (CamPom >=8 tiers sat 10-18pp below their CamPom
   rank). Pooling should pull those gaps toward 0.
4. The Zuby test, unchanged.

Config grid: window length {2, 3} x decay {0.3, 0.5, 0.7, 1.0} x lambda
{1000, 2000}, swept prequentially on 2025+2026-window -> 2026, best config
validated on two more targets (-> 2025, and the all-replay-era -> 2018).

Comparison/spike script only; writes nothing to the database.
"""

from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import pandas as pd
from scipy import sparse
from scipy.stats import spearmanr
from sklearn.linear_model import Ridge

import experiment_rapm_spike as sp
from db import get_engine

EVAL_DIR = Path(__file__).parent / "eval_history"

# Sweep target: windows ending at 2025 predicting 2026. Validation targets
# fit only the winning config.
SWEEP_TARGET = 2026
VALIDATION_TARGETS = [2025, 2018]
WINDOW_LENGTHS = [2, 3]
DECAYS = [0.3, 0.5, 0.7, 1.0]
LAMBDAS = [1000.0, 2000.0]
SINGLE_LAMBDA = 1000.0  # the shipped single-season optimum (baseline)
ROTATION_POSS = 250.0
CORRUPT_SEASONS = {2019}

KEYS_QUERY = """
SELECT p.id::text AS player_id, p.natstat_id, tps.torvik_pid
FROM players p
LEFT JOIN torvik_player_stats tps
       ON tps.player_id = p.id AND tps.season = p.season
WHERE p.season = ANY(%(seasons)s)
"""

TEAM_KEYS_QUERY = """
SELECT id::text AS team_id, natstat_id, season
FROM teams WHERE season = ANY(%(seasons)s)
"""


def career_map(engine, seasons: list[int]) -> dict:
    """Union-find over season-scoped player ids: rows sharing natstat_id OR
    torvik_pid join one career. Returns player_id -> career_id (root)."""
    df = pd.read_sql(KEYS_QUERY, engine, params={"seasons": list(seasons)})
    parent: dict = {}

    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    for key in ("natstat_id", "torvik_pid"):
        sub = df.dropna(subset=[key])
        for _, grp in sub.groupby(key):
            ids = grp["player_id"].tolist()
            for other in ids[1:]:
                union(ids[0], other)
    return {pid: find(pid) for pid in df["player_id"]}


def window_seasons(target_fit_end: int, length: int) -> list[int]:
    """The fit window ending at `target_fit_end`, skipping corrupt seasons
    (2019 has no paired stints; the window just reaches one season further
    is NOT done — the doc's shape (c) simply lacks that season)."""
    out = []
    s = target_fit_end
    while len(out) < length and s >= 2015:
        if s not in CORRUPT_SEASONS:
            out.append(s)
        s -= 1
    return sorted(out)


def build_career_design(df: pd.DataFrame, pid2career: dict):
    """Sparse design over CAREER columns: O block | D block | HCA."""
    careers = sorted(
        {pid2career.get(p, p) for key in df["lineup_key"] for p in key.split("|")}
        | {pid2career.get(p, p) for key in df["opp_key"] for p in key.split("|")}
    )
    cidx = {c: i for i, c in enumerate(careers)}
    n = len(careers)
    rows, cols = [], []
    for i, (lk, ok, home) in enumerate(
        zip(df["lineup_key"], df["opp_key"], df["home_offense"])
    ):
        for p in lk.split("|"):
            rows.append(i)
            cols.append(cidx[pid2career.get(p, p)])
        for p in ok.split("|"):
            rows.append(i)
            cols.append(n + cidx[pid2career.get(p, p)])
        if home:
            rows.append(i)
            cols.append(2 * n)
    x = sparse.csr_matrix(
        (np.ones(len(rows)), (rows, cols)), shape=(len(df), 2 * n + 1)
    )
    return x, careers


def fit_window(stints: dict, seasons: list[int], decay: float, lam: float,
               pid2career: dict):
    """Decayed-window ridge over career columns. Most recent season weight 1,
    each season back multiplied by `decay`. Returns (o_eff, d_eff) dicts
    keyed by career, plus the fit frame for diagnostics."""
    frames = []
    newest = max(seasons)
    for s in seasons:
        f = stints[s].copy()
        f["wmult"] = decay ** (newest - s)
        frames.append(f)
    df = pd.concat(frames, ignore_index=True)
    x, careers = build_career_design(df, pid2career)
    y = df["y"].to_numpy()
    w = (df["possessions_for"] * df["wmult"]).to_numpy()
    model = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
    model.fit(x, y, sample_weight=w)
    n = len(careers)
    o_eff = dict(zip(careers, model.coef_[:n]))
    d_eff = dict(zip(careers, model.coef_[n:2 * n]))
    return o_eff, d_eff, df


def row_effects(df: pd.DataFrame, o_eff: dict, d_eff: dict,
                pid2career: dict) -> tuple[np.ndarray, float]:
    """Sum of carried-over effects per target row + known-slot coverage
    (possession-weighted share of the ten slots with a known coefficient)."""
    eff = np.zeros(len(df))
    known = 0.0
    total = 0.0
    for i, (lk, ok, pf) in enumerate(
        zip(df["lineup_key"], df["opp_key"], df["possessions_for"])
    ):
        e = 0.0
        k = 0
        for p in lk.split("|"):
            c = pid2career.get(p, p)
            if c in o_eff:
                e += o_eff[c]
                k += 1
        for p in ok.split("|"):
            c = pid2career.get(p, p)
            if c in d_eff:
                e += d_eff[c]
                k += 1
        eff[i] = e
        known += pf * k
        total += pf * 10
    return eff, known / total


def preq_mse(df: pd.DataFrame, eff: np.ndarray) -> float:
    """Weighted MSE after the shared 2-parameter intercept+HCA recentering:
    y - eff ~ alpha + h*hca, weighted least squares (identical treatment for
    every competitor — only two global params touch target data)."""
    y = df["y"].to_numpy()
    w = df["possessions_for"].to_numpy()
    hca = df["home_offense"].to_numpy(dtype=float)
    resid = y - eff
    xm = np.column_stack([np.ones(len(df)), hca])
    wsq = np.sqrt(w)
    beta, *_ = np.linalg.lstsq(xm * wsq[:, None], resid * wsq, rcond=None)
    pred = eff + xm @ beta
    return float(np.average((y - pred) ** 2, weights=w))


def team_predictor(engine, fit_season: int, target_season: int,
                   stints: dict) -> tuple[dict, dict]:
    """Team-columns ridge fit on season t, coefficients carried to the
    target's team UUIDs via teams.natstat_id."""
    df = stints[fit_season]
    teams = sorted(set(df["team_id"]) | set(df["def_team_id"]))
    tidx = {t: i for i, t in enumerate(teams)}
    n = len(teams)
    rows, cols = [], []
    for i, (ot, dt, home) in enumerate(
        zip(df["team_id"], df["def_team_id"], df["home_offense"])
    ):
        rows.extend([i, i])
        cols.extend([tidx[ot], n + tidx[dt]])
        if home:
            rows.append(i)
            cols.append(2 * n)
    x = sparse.csr_matrix((np.ones(len(rows)), (rows, cols)),
                          shape=(len(df), 2 * n + 1))
    m = Ridge(alpha=100.0, fit_intercept=True, solver="sparse_cg")
    m.fit(x, df["y"].to_numpy(), sample_weight=df["possessions_for"].to_numpy())

    keys = pd.read_sql(TEAM_KEYS_QUERY, engine,
                       params={"seasons": [fit_season, target_season]})
    by_season = {s: dict(zip(g["natstat_id"], g["team_id"]))
                 for s, g in keys.groupby("season")}
    fit_uuid_to_nat = {v: k for k, v in by_season[fit_season].items()}
    o_eff, d_eff = {}, {}
    for t, i in tidx.items():
        nat = fit_uuid_to_nat.get(t)
        tgt = by_season.get(target_season, {}).get(nat) if nat else None
        if tgt:
            o_eff[tgt] = m.coef_[i]
            d_eff[tgt] = m.coef_[n + i]
    return o_eff, d_eff


def team_row_effects(df: pd.DataFrame, o_eff: dict, d_eff: dict) -> np.ndarray:
    return np.array([
        o_eff.get(ot, 0.0) + d_eff.get(dt, 0.0)
        for ot, dt in zip(df["team_id"], df["def_team_id"])
    ])


def campom_predictor(engine, fit_season: int, stints: dict,
                     pid2career: dict) -> tuple[dict, dict]:
    """CamPom O/D decomposition as carried-over effects, scale-calibrated on
    the FIT season's stints (never the target).

    The o/d split is quantile-clipped (0.5%..99.5%) before use: the psos
    rescale explodes on sub-minute players (cam_o up to +1680 with cam_d
    ~ -1680 — the halves cancel so the NET stays sane, but the split is
    garbage), and without the clip a handful of junk rows dominate the
    summed-effect variance and collapse the OLS scale to ~0."""
    meta = pd.read_sql(sp.PLAYER_QUERY, engine, params={"season": fit_season})
    o_lo, o_hi = meta["cam_o"].quantile([0.005, 0.995])
    d_lo, d_hi = meta["cam_d"].quantile([0.005, 0.995])
    o_raw, d_raw = {}, {}
    for _, r in meta.iterrows():
        c = pid2career.get(r["player_id"], r["player_id"])
        if pd.notna(r["cam_o"]):
            o_raw[c] = float(np.clip(r["cam_o"], o_lo, o_hi))
        if pd.notna(r["cam_d"]):
            d_raw[c] = -float(np.clip(r["cam_d"], d_lo, d_hi))
    # Scale calibration on fit-season stints (same construction as the
    # single-season spike's prior): y ~ alpha + h*hca + s*(cam effects).
    df = stints[fit_season]
    eff, _ = row_effects(df, o_raw, d_raw, pid2career)
    y = df["y"].to_numpy()
    w = df["possessions_for"].to_numpy()
    hca = df["home_offense"].to_numpy(dtype=float)
    xm = np.column_stack([np.ones(len(df)), hca, eff])
    wsq = np.sqrt(w)
    beta, *_ = np.linalg.lstsq(xm * wsq[:, None], y * wsq, rcond=None)
    s = float(beta[2])
    print(f"  campom predictor scale (fit on {fit_season}): {s:.3f}")
    return ({c: s * v for c, v in o_raw.items()},
            {c: s * v for c, v in d_raw.items()})


def split_half(stints: dict, seasons: list[int], decay: float, lam: float,
               pid2career: dict) -> dict:
    """Odd/even game split within the window; refit halves; correlate
    coefficients (players >=250*window-share poss in BOTH halves) for net,
    O, and D separately."""
    halves = []
    for parity in (0, 1):
        sub = {
            s: stints[s][stints[s]["game_id"].map(
                lambda g: int(g.replace("-", ""), 16) % 2) == parity]
            for s in seasons
        }
        o, d, fit_df = fit_window(sub, seasons, decay, lam, pid2career)
        poss = {}
        for lk, ok, pf in zip(fit_df["lineup_key"], fit_df["opp_key"],
                              fit_df["possessions_for"]):
            for p in lk.split("|"):
                c = pid2career.get(p, p)
                poss[c] = poss.get(c, 0.0) + pf
            for p in ok.split("|"):
                c = pid2career.get(p, p)
                poss[c] = poss.get(c, 0.0) + pf
        halves.append((o, d, poss))
    (o1, d1, p1), (o2, d2, p2) = halves
    floor = ROTATION_POSS / 2  # half the data -> half the floor
    common = [c for c in o1 if c in o2
              and p1.get(c, 0) >= floor and p2.get(c, 0) >= floor]
    out = {"n": len(common)}
    for name, e1, e2 in (
        ("net", {c: o1[c] - d1[c] for c in common}, {c: o2[c] - d2[c] for c in common}),
        ("o", o1, o2),
        ("d", d1, d2),
    ):
        a = np.array([e1[c] for c in common])
        b = np.array([e2[c] for c in common])
        out[name] = float(spearmanr(a, b).statistic)
    return out


def star_gap(engine, season: int, net_eff: dict, pid2career: dict,
             poss: dict) -> dict:
    """Percentile-gap-by-CamPom-tier (the compression diagnostic)."""
    meta = pd.read_sql(sp.PLAYER_QUERY, engine, params={"season": season})
    rows = []
    for _, r in meta.iterrows():
        c = pid2career.get(r["player_id"], r["player_id"])
        if c in net_eff and pd.notna(r["campom"]) and poss.get(c, 0) >= 500:
            rows.append((net_eff[c], float(r["campom"])))
    df = pd.DataFrame(rows, columns=["net", "cam"])
    df["net_pct"] = df["net"].rank(pct=True)
    df["cam_pct"] = df["cam"].rank(pct=True)
    df["tier"] = pd.cut(df["cam"], [-np.inf, 3, 8, 15, np.inf],
                        labels=["<3", "3..8", "8..15", ">=15"])
    out = {}
    for tier, g in df.groupby("tier", observed=True):
        out[str(tier)] = {"n": int(len(g)),
                          "gap": round(float((g["net_pct"] - g["cam_pct"]).mean()), 3)}
    return out


def main() -> None:
    engine = get_engine()
    all_seasons = sorted(
        set(window_seasons(SWEEP_TARGET - 1, 3))
        | {SWEEP_TARGET}
        | set().union(*[set(window_seasons(t - 1, 3)) | {t}
                        for t in VALIDATION_TARGETS])
        | {SWEEP_TARGET}  # current-season estimate window below
        | set(window_seasons(SWEEP_TARGET, 3))
    )
    print(f"Loading stints for seasons: {all_seasons}")
    stints = {s: sp.load_stints(engine, s) for s in all_seasons}
    pid2career = career_map(engine, all_seasons)
    n_chains = len(set(pid2career.values()))
    print(f"Career chains: {len(pid2career):,} player-seasons -> "
          f"{n_chains:,} careers")

    summary: dict = {"career_player_seasons": len(pid2career),
                     "careers": n_chains}

    # ---- Stage 1: config sweep, prequential on SWEEP_TARGET -------------
    target_df = stints[SWEEP_TARGET]
    fit_end = SWEEP_TARGET - 1
    print(f"\n{'=' * 72}\nPREQUENTIAL SWEEP: fit through {fit_end} -> "
          f"predict {SWEEP_TARGET}\n{'=' * 72}")

    # Baselines (shared rows, shared recentering).
    zero_mse = preq_mse(target_df, np.zeros(len(target_df)))
    o1, d1, fit1 = fit_window(stints, [fit_end], 1.0, SINGLE_LAMBDA, pid2career)
    eff1, cov1 = row_effects(target_df, o1, d1, pid2career)
    single_mse = preq_mse(target_df, eff1)
    t_o, t_d = team_predictor(engine, fit_end, SWEEP_TARGET, stints)
    team_mse = preq_mse(target_df, team_row_effects(target_df, t_o, t_d))
    c_o, c_d = campom_predictor(engine, fit_end, stints, pid2career)
    ceff, ccov = row_effects(target_df, c_o, c_d, pid2career)
    cam_mse = preq_mse(target_df, ceff)
    print(f"  baselines: intercept+HCA {zero_mse:.3f} | single-season RAPM "
          f"{single_mse:.3f} (cov {cov1:.0%}) | team ridge carryover "
          f"{team_mse:.3f} | CamPom carryover {cam_mse:.3f} (cov {ccov:.0%})")

    sweep = {}
    best = None
    for wl in WINDOW_LENGTHS:
        seasons = window_seasons(fit_end, wl)
        for decay in DECAYS:
            for lam in LAMBDAS:
                o, d, _ = fit_window(stints, seasons, decay, lam, pid2career)
                eff, cov = row_effects(target_df, o, d, pid2career)
                mse = preq_mse(target_df, eff)
                key = f"w{wl}_d{decay}_l{int(lam)}"
                sweep[key] = {"mse": round(mse, 3), "coverage": round(cov, 3)}
                print(f"  window {seasons} decay {decay} lambda {int(lam)}: "
                      f"MSE {mse:.3f} (cov {cov:.0%})")
                if best is None or mse < best[1]:
                    best = ((wl, decay, lam), mse)
    (best_wl, best_decay, best_lam), best_mse = best
    print(f"\nBest config: window {best_wl}, decay {best_decay}, "
          f"lambda {int(best_lam)} -> MSE {best_mse:.3f}")
    summary["sweep"] = {
        "baselines": {"intercept_hca": round(zero_mse, 3),
                      "single_season": round(single_mse, 3),
                      "single_coverage": round(cov1, 3),
                      "team_ridge": round(team_mse, 3),
                      "campom": round(cam_mse, 3)},
        "configs": sweep,
        "best": {"window": best_wl, "decay": best_decay, "lambda": best_lam,
                 "mse": round(best_mse, 3)},
    }

    # ---- Stage 2: validate best config on the other targets -------------
    summary["validation"] = {}
    for tgt in VALIDATION_TARGETS:
        fe = tgt - 1 if tgt - 1 not in CORRUPT_SEASONS else tgt - 2
        seasons = window_seasons(fe, best_wl)
        tdf = stints[tgt]
        so, sd, _ = fit_window(stints, [fe], 1.0, SINGLE_LAMBDA, pid2career)
        seff, _ = row_effects(tdf, so, sd, pid2career)
        po, pdd, _ = fit_window(stints, seasons, best_decay, best_lam, pid2career)
        peff, pcov = row_effects(tdf, po, pdd, pid2career)
        to, td2 = team_predictor(engine, fe, tgt, stints)
        res = {
            "single": round(preq_mse(tdf, seff), 3),
            "pooled": round(preq_mse(tdf, peff), 3),
            "pooled_coverage": round(pcov, 3),
            "team_ridge": round(preq_mse(tdf, team_row_effects(tdf, to, td2)), 3),
        }
        summary["validation"][str(tgt)] = res
        print(f"\nValidation -> {tgt} (window {seasons}): single "
              f"{res['single']} | pooled {res['pooled']} | team "
              f"{res['team_ridge']}")

    # ---- Stage 3: current-season estimate diagnostics --------------------
    cur_window = window_seasons(SWEEP_TARGET, best_wl)
    print(f"\n{'=' * 72}\nCURRENT-SEASON ESTIMATE ({SWEEP_TARGET}, window "
          f"{cur_window})\n{'=' * 72}")
    po, pdd, pfit = fit_window(stints, cur_window, best_decay, best_lam,
                               pid2career)
    poss = {}
    for lk, ok, pf in zip(pfit["lineup_key"], pfit["opp_key"],
                          pfit["possessions_for"]):
        for p in lk.split("|"):
            c = pid2career.get(p, p)
            poss[c] = poss.get(c, 0.0) + pf
        for p in ok.split("|"):
            c = pid2career.get(p, p)
            poss[c] = poss.get(c, 0.0) + pf
    pooled_net = {c: po[c] - pdd[c] for c in po}
    so, sd, _ = fit_window(stints, [SWEEP_TARGET], 1.0, SINGLE_LAMBDA,
                           pid2career)
    single_net = {c: so[c] - sd[c] for c in so}

    print("Star-separation gap by CamPom tier (rank-percentile, RAPM - CamPom):")
    gaps_single = star_gap(engine, SWEEP_TARGET, single_net, pid2career, poss)
    gaps_pooled = star_gap(engine, SWEEP_TARGET, pooled_net, pid2career, poss)
    for tier in ("<3", "3..8", "8..15", ">=15"):
        s = gaps_single.get(tier, {})
        p = gaps_pooled.get(tier, {})
        print(f"  {tier:>6}: single {s.get('gap'):+.3f} -> pooled "
              f"{p.get('gap'):+.3f} (n={p.get('n')})")
    summary["star_gap"] = {"single": gaps_single, "pooled": gaps_pooled}

    print("\nSplit-half reliability (Spearman across halves):")
    sh_single = split_half(stints, [SWEEP_TARGET], 1.0, SINGLE_LAMBDA,
                           pid2career)
    sh_pooled = split_half(stints, cur_window, best_decay, best_lam,
                           pid2career)
    for name, sh in (("single", sh_single), ("pooled", sh_pooled)):
        print(f"  {name:<7} net {sh['net']:+.3f}  O {sh['o']:+.3f}  "
              f"D {sh['d']:+.3f}  (n={sh['n']})")
    summary["split_half"] = {"single": sh_single, "pooled": sh_pooled}

    # Zuby test on the pooled current-season estimate.
    meta = pd.read_sql(sp.PLAYER_QUERY, engine, params={"season": SWEEP_TARGET})
    sj = meta[meta["team_name"] == "Saint John`s Red Storm"].copy()
    sj["career"] = sj["player_id"].map(lambda p: pid2career.get(p, p))
    sj["pooled_net"] = sj["career"].map(pooled_net)
    sj["poss"] = sj["career"].map(poss)
    sj = sj.dropna(subset=["pooled_net"]).sort_values("poss", ascending=False)
    print("\nZuby test (pooled):")
    for _, r in sj.head(7).iterrows():
        cam = f"{r['campom']:+.1f}" if pd.notna(r["campom"]) else "n/a"
        print(f"  {r['name']:<26} cam {cam:>6}  pooled net {r['pooled_net']:+.2f}")
    core = sj.head(5)
    summary["zuby_core_positive"] = int((core["pooled_net"] > 0).sum())
    z = sj[sj["name"].str.contains("Ejiofor", na=False)]
    if not z.empty:
        summary["zuby_pooled_net"] = round(float(z.iloc[0]["pooled_net"]), 2)
        print(f"  Ejiofor pooled net: {summary['zuby_pooled_net']:+.2f}; "
              f"core positive {summary['zuby_core_positive']}/5")

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"rapm_pooled_spike_{stamp}_summary.json"
    out_path.write_text(json.dumps(summary, indent=2))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    sys.exit(main())
