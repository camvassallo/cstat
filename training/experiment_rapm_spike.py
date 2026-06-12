"""
RAPM solver spike — 2026-only fit, the go/no-go gate for the Tier-3 build.

Step 2 of the PR plan in docs/rapm_methodology.md. Fits a weighted ridge
adjusted plus-minus on the 2026 paired-stint corpus (onfloor-dominant, both
lineups exactly 5, possessions > 0) and runs the single-season slice of the
acceptance suite:

  - lambda sweep with GAME-BLOCKED 5-fold CV (stints within a game share
    lineups and context; holding out stints would leak), scored on
    possession-weighted held-out stint-margin MSE, sensitivity curve printed
  - BOTH prior variants from doc section 4.2: zero prior (pure RAPM) and the
    CamPom-decomposition prior (cam_o / -cam_d, scale-calibrated per fold by
    weighted least squares since the psos scale is wider than RAPM's). The
    better CV variant becomes the final fit.
  - acceptance gate 4: CV MSE vs (a) intercept+HCA only and (b) a
    team-columns ridge — the same folds and machinery with offense/defense
    TEAM indicators instead of player indicators (its own lambda sweep, so
    the baseline gets its best shot). Beating (b) out-of-sample is the real
    test that player-level allocation carries information beyond team
    strength. A full-season AdjO/AdjD baseline is also reported, but it is
    leaky (it has seen the held-out games) — reference only, not a gate.
  - acceptance gate 2: top-25 net RAPM (rotation floor) vs CamPom, plus
    Spearman correlations against CamPom and raw on/off
  - acceptance gate 3, the Zuby test: St. John's 2026 — the starting core
    should stop reading negative once teammates/opponents are controlled

Model (doc section 4): each stint row in `lineup_stints` is already one
offense observation (the same floor-time appears twice, once per team's
perspective) —

  y = 100 * points_for / possessions_for      weight = possessions_for
  X = +1 on the 5 offensive players' O columns,
      +1 on the 5 defensive players' D columns,
      +1 home-offense indicator (0 on neutral floors)
  ridge toward 0 (v1 zero prior), intercept unpenalized (fit_intercept)

Conventions: O_j = points per 100 added on offense (higher better);
D_j = points per 100 ALLOWED while j defends (lower better);
net_j = O_j - D_j. Duplicate (game, lineups) rows are pre-collapsed
possession-weighted — identical weighted solution, smaller solve.

Year-over-year stability (acceptance gate 1) — the decisive gate, since the
single-season prediction result lands where the literature says it will
(team strength wins at stint prediction; RAPM's value is allocation +
stability) — runs via `python experiment_rapm_spike.py stability`: fits
2024/2025/2026 at the 2026-tuned lambdas, joins returners by the dual key
(natstat_id OR torvik_pid — natstat_id alone drops transfers), and compares
YoY Spearman of net RAPM (both prior variants) against raw on/off swing and
CamPom on the same returner sets. The zero-prior column is the honest
stint-only stability read; the prior variant inherits CamPom's own
stability, so it's reported but not the gate.

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
from sklearn.model_selection import GroupKFold

from db import get_engine

SEASON = 2026
LAMBDA_GRID = [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0]
CV_FOLDS = 5
ROTATION_POSS_FLOOR = 250.0  # display/report floor (doc section 5)
STABILITY_POSS_FLOOR = 500.0  # gate-1 floor, used here for correlations
EVAL_DIR = Path(__file__).parent / "eval_history"

STINT_QUERY = """
SELECT ls.game_id::text         AS game_id,
       ls.team_id::text         AS team_id,
       ls.lineup::text[]        AS lineup,
       ls.opp_lineup::text[]    AS opp_lineup,
       ls.points_for            AS points_for,
       ls.possessions_for       AS possessions_for,
       ls.source                AS source,
       (ls.team_id = g.home_team_id AND NOT g.is_neutral_site)::int AS home_offense,
       CASE WHEN ls.team_id = g.home_team_id THEN g.away_team_id::text
            ELSE g.home_team_id::text END   AS def_team_id
FROM lineup_stints ls
JOIN games g ON g.id = ls.game_id
WHERE ls.season = %(season)s
  AND array_length(ls.lineup, 1) = 5
  AND array_length(ls.opp_lineup, 1) = 5
  AND ls.possessions_for > 0
"""

PLAYER_QUERY = """
SELECT p.id::text AS player_id, p.name, t.name AS team_name,
       tps.cam_gbpm_v3_psos AS campom,
       tps.cam_o_gbpm_v3_psos AS cam_o,
       tps.cam_d_gbpm_v3_psos AS cam_d,
       oo.net_on_off, oo.on_net_rtg
FROM players p
JOIN teams t ON t.id = p.team_id
LEFT JOIN torvik_player_stats tps
       ON tps.player_id = p.id AND tps.season = p.season
LEFT JOIN player_on_off oo
       ON oo.player_id = p.id AND oo.season = p.season
WHERE p.season = %(season)s
"""

ADJ_QUERY = """
SELECT team_id::text AS team_id, adj_offense, adj_defense
FROM team_season_stats WHERE season = %(season)s
"""


def load_stints(engine, season: int = SEASON) -> pd.DataFrame:
    df = pd.read_sql(STINT_QUERY, engine, params={"season": season})
    print(f"Paired stints loaded: {len(df):,} "
          f"({df['possessions_for'].sum():,.0f} weighted possessions)")
    print(df["source"].value_counts().to_string())

    # Collapse duplicate (game, lineups, side) rows — possession-weighted
    # identical solution, smaller solve.
    df["lineup_key"] = df["lineup"].map(lambda a: "|".join(sorted(a)))
    df["opp_key"] = df["opp_lineup"].map(lambda a: "|".join(sorted(a)))
    grouped = (
        df.groupby(
            ["game_id", "team_id", "def_team_id", "lineup_key", "opp_key",
             "home_offense"],
            as_index=False,
        )
        .agg(points_for=("points_for", "sum"),
             possessions_for=("possessions_for", "sum"))
    )
    print(f"After duplicate-lineup collapse: {len(grouped):,} rows")
    grouped["y"] = 100.0 * grouped["points_for"] / grouped["possessions_for"]
    return grouped


def build_design(df: pd.DataFrame):
    """Sparse offense-row design: O block | D block | HCA column."""
    all_players = sorted(
        {p for key in df["lineup_key"] for p in key.split("|")}
        | {p for key in df["opp_key"] for p in key.split("|")}
    )
    pidx = {p: i for i, p in enumerate(all_players)}
    n_players = len(all_players)
    hca_col = 2 * n_players

    rows, cols = [], []
    for i, (lk, ok, home) in enumerate(
        zip(df["lineup_key"], df["opp_key"], df["home_offense"])
    ):
        for p in lk.split("|"):
            rows.append(i)
            cols.append(pidx[p])
        for p in ok.split("|"):
            rows.append(i)
            cols.append(n_players + pidx[p])
        if home:
            rows.append(i)
            cols.append(hca_col)

    x = sparse.csr_matrix(
        (np.ones(len(rows)), (rows, cols)), shape=(len(df), hca_col + 1)
    )
    print(f"Design: {x.shape[0]:,} rows x {x.shape[1]:,} cols "
          f"({n_players:,} players), {x.nnz:,} nonzeros")
    return x, all_players


def weighted_mse(y_true, y_pred, w) -> float:
    return float(np.average((y_true - y_pred) ** 2, weights=w))


def build_prior(players: list[str], meta: pd.DataFrame) -> np.ndarray:
    """CamPom-decomposition prior (doc section 4.2, the v2 candidate).

    O block prior = cam_o (points per 100 added, higher better); D block
    prior = -cam_d (cam_d is positive-good GBPM convention, our D is points
    ALLOWED, lower better). Players without a CamPom row get 0. The raw psos
    scale is wider than RAPM's natural range, so the prior enters through a
    per-fold least-squares scale calibration, not raw."""
    n = len(players)
    cam_o = dict(zip(meta["player_id"], meta["cam_o"]))
    cam_d = dict(zip(meta["player_id"], meta["cam_d"]))
    beta0 = np.zeros(2 * n + 1)
    covered = 0
    for i, p in enumerate(players):
        o, d = cam_o.get(p), cam_d.get(p)
        if o is not None and not pd.isna(o):
            beta0[i] = float(o)
            covered += 1
        if d is not None and not pd.isna(d):
            beta0[n + i] = -float(d)
    print(f"Prior coverage: {covered}/{n} players have a CamPom decomposition")
    return beta0


def fit_prior_fold(x_tr, y_tr, w_tr, hca_tr, beta0, lam):
    """Calibrate the prior scale on train, then ridge the residual.

    Returns (base_model, resid_model); the fitted prior scale is
    base_model.coef_[0], and prediction = base_model on [X @ beta0, hca]
    + resid_model on X."""
    z_tr = x_tr @ beta0
    base_x = np.column_stack([z_tr, hca_tr])
    base = Ridge(alpha=0.0, fit_intercept=True)
    base.fit(base_x, y_tr, sample_weight=w_tr)
    resid = y_tr - base.predict(base_x)
    m = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
    m.fit(x_tr, resid, sample_weight=w_tr)
    return base, m


def predict_prior(x_te, hca_te, beta0, base, m):
    z_te = x_te @ beta0
    return base.predict(np.column_stack([z_te, hca_te])) + m.predict(x_te)


def cv_sweep_prior(x, y, w, hca, splits, beta0) -> dict:
    results = {}
    for lam in LAMBDA_GRID:
        fold_mses = []
        for train_idx, test_idx in splits:
            base, m = fit_prior_fold(
                x[train_idx], y[train_idx], w[train_idx], hca[train_idx],
                beta0, lam)
            pred = predict_prior(x[test_idx], hca[test_idx], beta0, base, m)
            fold_mses.append(weighted_mse(y[test_idx], pred, w[test_idx]))
        results[lam] = float(np.mean(fold_mses))
        print(f"  lambda {lam:>8.0f}: held-out weighted MSE {results[lam]:.3f}"
              f"  (prior scale {base.coef_[0]:.3f})")
    return results


def cv_sweep(x, y, w, groups) -> tuple[dict, list[np.ndarray]]:
    """Game-blocked CV over the lambda grid. Returns per-lambda MSE and the
    fold index splits (reused for the baselines so everything shares folds)."""
    gkf = GroupKFold(n_splits=CV_FOLDS)
    splits = list(gkf.split(np.zeros(len(y)), groups=groups))
    results = {}
    for lam in LAMBDA_GRID:
        fold_mses = []
        for train_idx, test_idx in splits:
            model = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
            model.fit(x[train_idx], y[train_idx], sample_weight=w[train_idx])
            pred = model.predict(x[test_idx])
            fold_mses.append(weighted_mse(y[test_idx], pred, w[test_idx]))
        results[lam] = float(np.mean(fold_mses))
        print(f"  lambda {lam:>8.0f}: held-out weighted MSE {results[lam]:.3f}")
    return results, splits


def baseline_mses(df: pd.DataFrame, y, w, splits, engine) -> dict:
    out = {}

    # (a) intercept + HCA only — same ridge machinery on the lone HCA column.
    x_hca = sparse.csr_matrix(df[["home_offense"]].to_numpy(dtype=float))
    fold_mses = []
    for train_idx, test_idx in splits:
        m = Ridge(alpha=1.0, fit_intercept=True)
        m.fit(x_hca[train_idx], y[train_idx], sample_weight=w[train_idx])
        fold_mses.append(
            weighted_mse(y[test_idx], m.predict(x_hca[test_idx]), w[test_idx])
        )
    out["intercept_hca"] = float(np.mean(fold_mses))

    # (b) team-columns ridge — identical observation model with team
    # indicators (off team, def team, HCA) instead of player indicators.
    # Fair out-of-sample comparison: same folds, own lambda sweep.
    teams = sorted(set(df["team_id"]) | set(df["def_team_id"]))
    tidx = {t: i for i, t in enumerate(teams)}
    n_teams = len(teams)
    rows, cols = [], []
    for i, (off_t, def_t, home) in enumerate(
        zip(df["team_id"], df["def_team_id"], df["home_offense"])
    ):
        rows.extend([i, i])
        cols.extend([tidx[off_t], n_teams + tidx[def_t]])
        if home:
            rows.append(i)
            cols.append(2 * n_teams)
    x_team = sparse.csr_matrix(
        (np.ones(len(rows)), (rows, cols)), shape=(len(df), 2 * n_teams + 1)
    )
    team_cv = {}
    for lam in LAMBDA_GRID:
        fold_mses = []
        for train_idx, test_idx in splits:
            m = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
            m.fit(x_team[train_idx], y[train_idx], sample_weight=w[train_idx])
            fold_mses.append(weighted_mse(
                y[test_idx], m.predict(x_team[test_idx]), w[test_idx]))
        team_cv[lam] = float(np.mean(fold_mses))
    best_team_lam = min(team_cv, key=team_cv.get)
    out["team_ridge"] = team_cv[best_team_lam]
    out["team_ridge_lambda"] = best_team_lam
    print(f"  team-columns ridge: best lambda {best_team_lam:.0f}, "
          f"held-out weighted MSE {out['team_ridge']:.3f}")

    # (c) full-season AdjO/AdjD strength reference — deliberately leaky (it
    # has seen the held-out games), evaluated on the same fold rows. Additive
    # expectation: AdjO_off + AdjD_def - league mean. Reference, not a gate.
    adj = pd.read_sql(ADJ_QUERY, engine, params={"season": SEASON})
    league_mean = float(adj["adj_offense"].mean())
    adj_o = dict(zip(adj["team_id"], adj["adj_offense"]))
    adj_d = dict(zip(adj["team_id"], adj["adj_defense"]))
    pred = np.array([
        adj_o.get(t, league_mean) + adj_d.get(d, league_mean) - league_mean
        for t, d in zip(df["team_id"], df["def_team_id"])
    ])
    fold_mses = [
        weighted_mse(y[test_idx], pred[test_idx], w[test_idx])
        for _, test_idx in splits
    ]
    out["adjem_fullseason"] = float(np.mean(fold_mses))
    return out


def paired_possessions(players: list[str], df: pd.DataFrame) -> dict:
    """Per-player sample: offensive poss while on floor (rows where he's in
    the lineup) + defensive poss (rows where he's in the opposing lineup;
    that row's possessions_for is the opponent's)."""
    poss = {p: 0.0 for p in players}
    for lk, ok, pf in zip(df["lineup_key"], df["opp_key"],
                          df["possessions_for"]):
        for p in lk.split("|"):
            poss[p] += pf
        for p in ok.split("|"):
            poss[p] += pf
    return poss


def assemble_table(players: list[str], coef: np.ndarray, df: pd.DataFrame
                   ) -> pd.DataFrame:
    n = len(players)
    poss = paired_possessions(players, df)
    return pd.DataFrame({
        "player_id": players,
        "o_rapm": coef[:n],
        "d_rapm": coef[n:2 * n],
        "net_rapm": coef[:n] - coef[n:2 * n],
        "paired_poss": [poss[p] for p in players],
    })


def final_fit(x, y, w, lam: float, players: list[str], df: pd.DataFrame
              ) -> pd.DataFrame:
    model = Ridge(alpha=lam, fit_intercept=True, solver="sparse_cg")
    model.fit(x, y, sample_weight=w)
    coef = model.coef_
    print(f"Final fit (zero prior): intercept {model.intercept_:.2f}, "
          f"HCA {coef[2 * len(players)]:+.2f} pts/100")
    return assemble_table(players, coef, df)


def final_fit_prior(x, y, w, hca, lam: float, beta0, players: list[str],
                    df: pd.DataFrame) -> pd.DataFrame:
    base, m = fit_prior_fold(x, y, w, hca, beta0, lam)
    s = float(base.coef_[0])
    coef = s * beta0 + m.coef_
    n = len(players)
    hca_total = float(base.coef_[1] + m.coef_[2 * n])
    print(f"Final fit (CamPom prior): prior scale {s:.3f}, "
          f"intercept {base.intercept_:.2f}, HCA {hca_total:+.2f} pts/100")
    return assemble_table(players, coef, df)


def report(rapm: pd.DataFrame, meta: pd.DataFrame) -> dict:
    df = rapm.merge(meta, on="player_id", how="left")
    rot = df[df["paired_poss"] >= ROTATION_POSS_FLOOR].copy()
    stab = df[df["paired_poss"] >= STABILITY_POSS_FLOOR].copy()
    out = {}

    print(f"\n{'=' * 72}\nTOP 25 NET RAPM (paired_poss >= {ROTATION_POSS_FLOOR:.0f})"
          f"\n{'=' * 72}")
    top = rot.nlargest(25, "net_rapm")
    for _, r in top.iterrows():
        cam = f"{r['campom']:+.1f}" if pd.notna(r["campom"]) else "  n/a"
        print(f"  {r['net_rapm']:+6.2f}  (O {r['o_rapm']:+5.2f} / D {r['d_rapm']:+5.2f})"
              f"  cam {cam:>6}  poss {r['paired_poss']:>6.0f}"
              f"  {r['name']}, {r['team_name']}")

    # Overlap of RAPM top 25 with the CamPom top 50 (rotation pool).
    cam_pool = rot.dropna(subset=["campom"])
    cam_top50 = set(cam_pool.nlargest(50, "campom")["player_id"])
    out["top25_in_campom_top50"] = int(
        sum(p in cam_top50 for p in top["player_id"]))
    print(f"\nRAPM top-25 appearing in CamPom top-50: "
          f"{out['top25_in_campom_top50']}/25")

    sub = stab.dropna(subset=["campom"])
    out["spearman_vs_campom"] = float(
        spearmanr(sub["net_rapm"], sub["campom"]).statistic)
    sub2 = stab.dropna(subset=["net_on_off"])
    out["spearman_vs_on_off"] = float(
        spearmanr(sub2["net_rapm"], sub2["net_on_off"]).statistic)
    print(f"Spearman (>= {STABILITY_POSS_FLOOR:.0f} poss): "
          f"vs CamPom {out['spearman_vs_campom']:+.3f} (n={len(sub)}), "
          f"vs raw on/off {out['spearman_vs_on_off']:+.3f} (n={len(sub2)})")

    # The Zuby test — St. John's 2026 starting core under opponent control.
    sj = df[df["team_name"] == "Saint John`s Red Storm"].copy()
    sj = sj.sort_values("paired_poss", ascending=False)
    print(f"\n{'=' * 72}\nZUBY TEST — Saint John`s Red Storm {SEASON}"
          f"\n{'=' * 72}")
    print(f"  {'player':<28} {'poss':>6} {'cam':>6} {'on/off':>7} "
          f"{'O':>6} {'D':>6} {'net':>6}")
    for _, r in sj.head(10).iterrows():
        cam = f"{r['campom']:+.1f}" if pd.notna(r["campom"]) else "n/a"
        onoff = f"{r['net_on_off']:+.1f}" if pd.notna(r["net_on_off"]) else "n/a"
        print(f"  {r['name']:<28} {r['paired_poss']:>6.0f} {cam:>6} {onoff:>7} "
              f"{r['o_rapm']:+6.2f} {r['d_rapm']:+6.2f} {r['net_rapm']:+6.2f}")

    core = sj.head(5)  # the 5 highest-possession players = the starting core
    out["zuby_core_net_positive"] = int((core["net_rapm"] > 0).sum())
    zuby = sj[sj["name"].str.contains("Ejiofor", na=False)]
    if not zuby.empty:
        z = zuby.iloc[0]
        out["zuby_net_rapm"] = float(z["net_rapm"])
        out["zuby_net_on_off"] = (
            float(z["net_on_off"]) if pd.notna(z["net_on_off"]) else None)
        print(f"\n  Ejiofor: net RAPM {z['net_rapm']:+.2f} vs raw on/off "
              f"{out['zuby_net_on_off']}")
    print(f"  Starting core (top-5 poss) with net RAPM > 0: "
          f"{out['zuby_core_net_positive']}/5")
    return out


STABILITY_SEASONS = [2024, 2025, 2026]
# 2026-tuned optima reused across seasons (similar corpus sizes; the CV
# curves are flat-topped around these, so per-season re-tuning is noise).
STABILITY_LAMBDA_ZERO = 1000.0
STABILITY_LAMBDA_PRIOR = 2000.0

KEY_QUERY = """
SELECT p.id::text AS player_id, p.natstat_id, tps.torvik_pid
FROM players p
LEFT JOIN torvik_player_stats tps
       ON tps.player_id = p.id AND tps.season = p.season
WHERE p.season = %(season)s
"""


def fit_season(engine, season: int) -> pd.DataFrame:
    """Fit both prior variants for one season at the fixed spike lambdas.
    Returns player_id, net_zero, net_prior, paired_poss."""
    print(f"\n--- season {season} ---")
    meta = pd.read_sql(PLAYER_QUERY, engine, params={"season": season})
    df = load_stints(engine, season)
    x, players = build_design(df)
    y = df["y"].to_numpy()
    w = df["possessions_for"].to_numpy()
    hca = df["home_offense"].to_numpy(dtype=float)
    n = len(players)

    zero = Ridge(alpha=STABILITY_LAMBDA_ZERO, fit_intercept=True,
                 solver="sparse_cg")
    zero.fit(x, y, sample_weight=w)

    beta0 = build_prior(players, meta)
    base, m = fit_prior_fold(x, y, w, hca, beta0, STABILITY_LAMBDA_PRIOR)
    prior_coef = float(base.coef_[0]) * beta0 + m.coef_

    poss = paired_possessions(players, df)
    return pd.DataFrame({
        "player_id": players,
        "net_zero": zero.coef_[:n] - zero.coef_[n:2 * n],
        "net_prior": prior_coef[:n] - prior_coef[n:2 * n],
        "paired_poss": [poss[p] for p in players],
    })


def returner_pairs(a: pd.DataFrame, b: pd.DataFrame) -> pd.DataFrame:
    """Cross-season returner join on the dual key (natstat_id OR torvik_pid
    — natstat_id alone silently drops transfers)."""
    out = []
    for key in ("natstat_id", "torvik_pid"):
        ka = a.dropna(subset=[key])
        kb = b.dropna(subset=[key])
        out.append(ka.merge(kb, on=key, suffixes=("_1", "_2"))
                   [["player_id_1", "player_id_2"]])
    return pd.concat(out).drop_duplicates(subset=["player_id_1", "player_id_2"])


def stability_main() -> None:
    engine = get_engine()
    fits, keys, metas = {}, {}, {}
    for s in STABILITY_SEASONS:
        fits[s] = fit_season(engine, s)
        keys[s] = pd.read_sql(KEY_QUERY, engine, params={"season": s})
        metas[s] = pd.read_sql(PLAYER_QUERY, engine, params={"season": s})

    results = {}
    print(f"\n{'=' * 72}\nYEAR-OVER-YEAR STABILITY "
          f"(returners, paired_poss >= {STABILITY_POSS_FLOOR:.0f} both seasons)"
          f"\n{'=' * 72}")
    for s1, s2 in zip(STABILITY_SEASONS, STABILITY_SEASONS[1:]):
        def season_frame(s):
            f = fits[s].merge(keys[s], on="player_id")
            return f.merge(
                metas[s][["player_id", "campom", "net_on_off"]],
                on="player_id")
        a = season_frame(s1)
        b = season_frame(s2)
        pairs = returner_pairs(a, b)
        j = (pairs
             .merge(a.add_suffix("_1"), on="player_id_1")
             .merge(b.add_suffix("_2"), on="player_id_2"))
        j = j[(j["paired_poss_1"] >= STABILITY_POSS_FLOOR)
              & (j["paired_poss_2"] >= STABILITY_POSS_FLOOR)]
        row = {}
        for metric in ("net_zero", "net_prior", "campom", "net_on_off"):
            sub = j.dropna(subset=[f"{metric}_1", f"{metric}_2"])
            row[metric] = {
                "spearman": float(spearmanr(
                    sub[f"{metric}_1"], sub[f"{metric}_2"]).statistic),
                "n": int(len(sub)),
            }
        results[f"{s1}->{s2}"] = row
        print(f"\n{s1} -> {s2}  (returners kept: {len(j)})")
        for metric, r in row.items():
            print(f"  {metric:<12} rho {r['spearman']:+.3f}  (n={r['n']})")

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = EVAL_DIR / f"rapm_spike_stability_{stamp}_summary.json"
    out_path.write_text(json.dumps({
        "seasons": STABILITY_SEASONS,
        "lambda_zero": STABILITY_LAMBDA_ZERO,
        "lambda_prior": STABILITY_LAMBDA_PRIOR,
        "poss_floor": STABILITY_POSS_FLOOR,
        "yoy_spearman": results,
    }, indent=2))
    print(f"\nSummary written: {out_path}")


def main() -> None:
    engine = get_engine()
    meta = pd.read_sql(PLAYER_QUERY, engine, params={"season": SEASON})
    df = load_stints(engine)
    x, players = build_design(df)
    y = df["y"].to_numpy()
    w = df["possessions_for"].to_numpy()
    hca = df["home_offense"].to_numpy(dtype=float)
    groups = df["game_id"].to_numpy()

    print(f"\nZero-prior lambda sweep ({CV_FOLDS}-fold game-blocked CV):")
    cv, splits = cv_sweep(x, y, w, groups)
    best_lam = min(cv, key=cv.get)
    base = baseline_mses(df, y, w, splits, engine)

    beta0 = build_prior(players, meta)
    print(f"\nCamPom-prior lambda sweep (same folds):")
    cv_prior = cv_sweep_prior(x, y, w, hca, splits, beta0)
    best_lam_prior = min(cv_prior, key=cv_prior.get)

    print(f"\nBest lambda: zero prior {best_lam:.0f} (MSE {cv[best_lam]:.3f}) | "
          f"CamPom prior {best_lam_prior:.0f} (MSE {cv_prior[best_lam_prior]:.3f})")
    print(f"Baselines: intercept+HCA {base['intercept_hca']:.3f} | "
          f"team-columns ridge {base['team_ridge']:.3f} | "
          f"AdjEM full-season (leaky reference) {base['adjem_fullseason']:.3f}")

    prior_wins = cv_prior[best_lam_prior] < cv[best_lam]
    best_mse = cv_prior[best_lam_prior] if prior_wins else cv[best_lam]
    variant = "campom_prior" if prior_wins else "zero_prior"
    beats_intercept = best_mse < base["intercept_hca"]
    beats_team = best_mse < base["team_ridge"]
    beats_adjem = best_mse < base["adjem_fullseason"]
    print(f"Best variant: {variant} (MSE {best_mse:.3f})")
    print(f"Gate 4: beats intercept-only {'PASS' if beats_intercept else 'FAIL'}, "
          f"beats team-columns ridge {'PASS' if beats_team else 'FAIL'}; "
          f"vs leaky AdjEM reference "
          f"{'beats' if beats_adjem else 'does not beat'}")

    if prior_wins:
        rapm = final_fit_prior(x, y, w, hca, best_lam_prior, beta0, players, df)
    else:
        rapm = final_fit(x, y, w, best_lam, players, df)
    rep = report(rapm, meta)

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d")
    summary = {
        "season": SEASON,
        "n_stints_collapsed": int(len(df)),
        "n_players": len(players),
        "weighted_possessions": float(df["possessions_for"].sum()),
        "zero_prior_cv_mse": cv,
        "zero_prior_best_lambda": best_lam,
        "campom_prior_cv_mse": cv_prior,
        "campom_prior_best_lambda": best_lam_prior,
        "final_variant": variant,
        "baseline_cv_mse": base,
        "gate4_beats_intercept_hca": bool(beats_intercept),
        "gate4_beats_team_ridge": bool(beats_team),
        "leaky_adjem_reference_beaten": bool(beats_adjem),
        **rep,
    }
    out_path = EVAL_DIR / f"rapm_spike_{SEASON}_{stamp}_summary.json"
    out_path.write_text(json.dumps(summary, indent=2))
    print(f"\nSummary written: {out_path}")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "stability":
        sys.exit(stability_main())
    sys.exit(main())
