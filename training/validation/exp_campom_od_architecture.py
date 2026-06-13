"""
Empirical check: which projection architecture is most accurate for
next-season CamPom and its o/d split?

Compares three ways to project season N+1 cam_v3 / cam_o / cam_d from the
same prior-season features (matched 5-fold folds, identical feature set):

  (D) DIRECT      — one LightGBM per target (cam_v3, cam_o, cam_d) independently.
                    Net-by-OD = cam_o + cam_d (reconstructed net).
  (S) NET+SPLIT   — predict cam_v3 + cam_o; derive cam_d = cam_v3 - cam_o.
  (P) PRIMITIVES  — predict the 5 CamPom formula inputs for N+1
                    (ogbpm, dgbpm, usg, min_per, gp), then run the REAL
                    compute_campom formula with ORACLE N+1 SOS + pop-mean
                    to derive cam_o / cam_d / cam_v3. Oracle SOS gives the
                    structured approach its best-case (isolates "does the
                    formula-as-inductive-bias help" from "can we project SOS").

Reports 5-fold CV MAE on cam_v3, cam_o, cam_d for each, plus the
net-reconstruction residual and a formula-validation sanity check.

Mirrors compute.rs:CAMPOM_* and the v3_psos o/d split — the FIXED
magnitude-share SOS allocation (|adj_o| / (|adj_o| + |adj_d|)), not the
original signed shares. The first run of this experiment (2026-06-08,
"primitives beat direct by 8-32%") predates the fix: its DIRECT baseline
trained on targets containing the signed-share junk (tails to ±17,558),
so that margin needs this re-validation before anything builds on it.

Second question (RAPM arm): do pooled RAPM-O/D (prior-season, from
player_rapm — decayed 3-season window, so season-N values use no N+1
information) add anything as FEATURES for projecting next-season
cam/cam_o/cam_d? §10.7 closed RAPM as a model feature for game models
and the trajectory net target; this is the O/D-projection variant of
that question, run cheaply here on matched folds.
"""
from __future__ import annotations

# Allow running from training/validation/ — put parent training/ on the
# path so prod modules (db, train_roster_impact_model, ...) import.
import os, sys as _sys
_sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.model_selection import KFold
from sklearn.metrics import mean_absolute_error

from db import get_engine


def mae(a, b):
    a = np.asarray(a, float); b = np.asarray(b, float)
    m = np.isfinite(a) & np.isfinite(b)
    return float(np.mean(np.abs(a[m] - b[m])))


def mae_trim(a, b, trim=0.01):
    """MAE after dropping the top `trim` fraction of abs errors — robust to
    the CamPom SOS-split division degeneracy (near-zero adj_gbpm rows)."""
    a = np.asarray(a, float); b = np.asarray(b, float)
    m = np.isfinite(a) & np.isfinite(b)
    e = np.abs(a[m] - b[m])
    cut = np.quantile(e, 1 - trim)
    return float(np.mean(e[e <= cut]))

# compute.rs:1118-1129
USG_REF = 17.873_577_08
OFF_EXP = 0.7
DEF_DISC = 0.1
MIN_EXP = 0.5
GP_K = 8.0
PSOS_RATE = 0.15

SEASONS = list(range(2015, 2026))  # s_n; pairs 2015->16 .. 2025->26

QUERY = """
WITH base AS (
    SELECT a.torvik_pid, a.season AS s_n, a.player_id AS pid_n,
           b.season AS s_np1, b.player_id AS pid_np1
    FROM torvik_player_stats a
    JOIN torvik_player_stats b
      ON a.torvik_pid = b.torvik_pid AND b.season = a.season + 1
    WHERE a.torvik_pid IS NOT NULL AND a.season = ANY(%(seasons)s)
      AND a.cam_gbpm_v3_psos IS NOT NULL AND b.cam_gbpm_v3_psos IS NOT NULL
      AND b.cam_o_gbpm_v3_psos IS NOT NULL AND b.cam_d_gbpm_v3_psos IS NOT NULL
)
SELECT base.torvik_pid, base.s_n, base.s_np1,
  -- prior-season features (the predictors)
  pN.minutes_per_game AS f_mpg, pN.games_played AS f_gp,
  pN.ppg AS f_ppg, pN.rpg AS f_rpg, pN.apg AS f_apg, pN.spg AS f_spg,
  pN.bpg AS f_bpg, pN.topg AS f_topg,
  pN.true_shooting_pct AS f_ts, pN.effective_fg_pct AS f_efg, pN.usage_rate AS f_usg,
  pN.ast_pct AS f_ast, pN.tov_pct AS f_tov, pN.orb_pct AS f_orb,
  pN.drb_pct AS f_drb, pN.stl_pct AS f_stl, pN.blk_pct AS f_blk, pN.ft_rate AS f_ftr,
  tN.ogbpm AS f_ogbpm, tN.dgbpm AS f_dgbpm, tN.gbpm AS f_gbpm,
  tN.cam_gbpm_v3_psos AS f_cam, tN.cam_o_gbpm_v3_psos AS f_cam_o,
  tN.cam_d_gbpm_v3_psos AS f_cam_d,
  -- N+1 CamPom targets
  tB.cam_gbpm_v3_psos AS y_cam, tB.cam_o_gbpm_v3_psos AS y_cam_o,
  tB.cam_d_gbpm_v3_psos AS y_cam_d,
  -- N+1 formula primitives (targets for the PRIMITIVES approach)
  tB.ogbpm AS y_ogbpm, tB.dgbpm AS y_dgbpm, tB.usage_rate AS y_usg,
  tB.min_per AS y_min_per, tB.games_played AS y_gp,
  pB.player_sos AS y_psos, pN.player_sos AS f_psos,
  -- prior-season pooled RAPM (player_rapm; decayed window ending at s_n,
  -- so no N+1 information). NULL where no fit (e.g. 2019) — LGBM-native.
  rN.o_rapm AS f_rapm_o, rN.d_rapm AS f_rapm_d, rN.net_rapm AS f_rapm_net,
  rN.paired_possessions AS f_rapm_poss,
  -- prior-season raw on/off (the production trajectory contract's trio plus
  -- the O/D-decomposed swings) — the "already served" baseline RAPM must
  -- beat to earn a slot.
  ooN.on_net_rtg AS f_on_net, ooN.net_on_off AS f_net_onoff,
  CASE WHEN ooN.on_possessions_for + ooN.off_possessions_for > 0
       THEN ooN.on_possessions_for
            / (ooN.on_possessions_for + ooN.off_possessions_for)
  END AS f_on_share,
  (ooN.on_ortg - ooN.off_ortg) AS f_oo_ortg,
  (ooN.on_drtg - ooN.off_drtg) AS f_oo_drtg
FROM base
JOIN player_season_stats pN ON pN.player_id = base.pid_n AND pN.season = base.s_n
JOIN player_season_stats pB ON pB.player_id = base.pid_np1 AND pB.season = base.s_np1
JOIN torvik_player_stats tN ON tN.player_id = base.pid_n AND tN.season = base.s_n
JOIN torvik_player_stats tB ON tB.player_id = base.pid_np1 AND tB.season = base.s_np1
LEFT JOIN player_rapm rN ON rN.player_id = base.pid_n AND rN.season = base.s_n
LEFT JOIN player_on_off ooN ON ooN.player_id = base.pid_n AND ooN.season = base.s_n
WHERE pN.minutes_per_game >= 5 AND pN.games_played >= 5
  AND pB.minutes_per_game >= 5 AND pB.games_played >= 5
  AND tB.ogbpm IS NOT NULL AND tB.dgbpm IS NOT NULL
  AND tB.usage_rate IS NOT NULL AND tB.min_per IS NOT NULL AND tB.min_per > 0
  AND pB.player_sos IS NOT NULL
"""

FEATURES = [
    "f_mpg", "f_gp", "f_ppg", "f_rpg", "f_apg", "f_spg", "f_bpg", "f_topg",
    "f_ts", "f_efg", "f_usg", "f_ast", "f_tov", "f_orb", "f_drb", "f_stl",
    "f_blk", "f_ftr", "f_ogbpm", "f_dgbpm", "f_gbpm", "f_cam", "f_cam_o", "f_cam_d",
]

PARAMS = dict(objective="regression", metric="mae", num_leaves=24,
              learning_rate=0.05, feature_fraction=0.8, bagging_fraction=0.8,
              bagging_freq=5, min_child_samples=25, lambda_l2=1.0,
              n_estimators=300, verbose=-1, seed=42, deterministic=True)


def compose(ogbpm, dgbpm, usg, min_per, gp, psos, mean_min_per):
    """Real compute.rs v3_psos formula -> (cam_o, cam_d, cam_v3).

    SOS split uses the FIXED magnitude-share allocation
    (compute.rs:1383-1406): share = |adj_o| / (|adj_o| + |adj_d|),
    bounded [0,1], 50/50 fallback only when both halves are ~0.
    """
    usg_ratio = np.clip(usg, 0, None) / USG_REF
    o_part = ogbpm * (usg_ratio ** OFF_EXP)
    d_part = dgbpm * (1.0 - DEF_DISC * usg_ratio)
    psos_adj = psos * PSOS_RATE
    mp = (np.clip(min_per, 0, None) / mean_min_per) ** MIN_EXP
    gpw = gp / (gp + GP_K)
    denom = np.abs(o_part) + np.abs(d_part)
    safe = denom > 1e-9
    o_share = np.where(safe, np.abs(o_part) / np.where(safe, denom, 1.0), 0.5)
    cam_o = (o_part + psos_adj * o_share) * mp * gpw
    cam_d = (d_part + psos_adj * (1.0 - o_share)) * mp * gpw
    return cam_o, cam_d, cam_o + cam_d


def fit_predict(Xtr, ytr, Xte):
    m = lgb.LGBMRegressor(**PARAMS)
    m.fit(Xtr, ytr)
    return m.predict(Xte)


def main():
    eng = get_engine()
    df = pd.read_sql(QUERY, eng, params={"seasons": SEASONS}).reset_index(drop=True)
    print(f"Loaded {len(df):,} paired rows ({df['s_n'].min()}->{df['s_n'].max()+1})")
    # Targets + primitives must be finite for the CV; features may keep NaN (LGBM ok).
    req = ["y_cam", "y_cam_o", "y_cam_d", "y_ogbpm", "y_dgbpm", "y_usg",
           "y_min_per", "y_gp", "y_psos"]
    pre = len(df)
    df = df[np.isfinite(df[req].to_numpy(float)).all(axis=1)].reset_index(drop=True)
    print(f"  dropped {pre - len(df)} rows with non-finite target/primitive; {len(df):,} kept")

    # population mean min_per per N+1 season, for the formula's mp_factor.
    mean_min_by_season = df.groupby("s_np1")["y_min_per"].transform("mean").values

    # --- formula validation: compose ACTUAL N+1 primitives, compare to stored cam ---
    co, cd, cv = compose(df.y_ogbpm.values, df.y_dgbpm.values, df.y_usg.values,
                         df.y_min_per.values, df.y_gp.values, df.y_psos.values,
                         mean_min_by_season)
    print("\n=== formula-composition validation (actual primitives vs stored) ===")
    for nm, comp, stored in [("cam_v3", cv, df.y_cam), ("cam_o", co, df.y_cam_o),
                             ("cam_d", cd, df.y_cam_d)]:
        a = np.asarray(comp, float); b = np.asarray(stored.values, float)
        msk = np.isfinite(a) & np.isfinite(b)
        r = np.corrcoef(a[msk], b[msk])[0, 1]
        print(f"  {nm:<7} r={r:.4f}  recompose-MAE={mae(b, a):.3f}  "
              f"(n_finite={msk.sum()}/{len(a)}; should be ~0 / r~1)")

    X = df[FEATURES].values
    kf = KFold(n_splits=5, shuffle=True, random_state=42)

    # OOF predictions for each approach + stored primitive preds for SOS variants
    oof = {k: np.full(len(df), np.nan) for k in
           ["D_cam", "D_camo", "D_camd", "P_camo", "P_camd", "P_cam"]}
    pp = {k: np.full(len(df), np.nan) for k in ["og", "dg", "ug", "mp", "gp"]}

    for tr, te in kf.split(X):
        Xtr, Xte = X[tr], X[te]
        # DIRECT
        oof["D_cam"][te]  = fit_predict(Xtr, df.y_cam.values[tr], Xte)
        oof["D_camo"][te] = fit_predict(Xtr, df.y_cam_o.values[tr], Xte)
        oof["D_camd"][te] = fit_predict(Xtr, df.y_cam_d.values[tr], Xte)
        # PRIMITIVES
        pp["og"][te] = fit_predict(Xtr, df.y_ogbpm.values[tr], Xte)
        pp["dg"][te] = fit_predict(Xtr, df.y_dgbpm.values[tr], Xte)
        pp["ug"][te] = fit_predict(Xtr, df.y_usg.values[tr], Xte)
        pp["mp"][te] = fit_predict(Xtr, df.y_min_per.values[tr], Xte)
        pp["gp"][te] = fit_predict(Xtr, df.y_gp.values[tr], Xte)
        co, cd, cv = compose(pp["og"][te], pp["dg"][te], pp["ug"][te], pp["mp"][te],
                             pp["gp"][te], df.y_psos.values[te], mean_min_by_season[te])
        oof["P_camo"][te], oof["P_camd"][te], oof["P_cam"][te] = co, cd, cv

    yc, yo, yd = df.y_cam.values, df.y_cam_o.values, df.y_cam_d.values
    naive_c = mae(yc, df.f_cam.values)
    naive_o = mae(yo, df.f_cam_o.values)
    naive_d = mae(yd, df.f_cam_d.values)

    print("\n=== 5-fold CV MAE (lower=better) ===")
    print(f"  {'target':<8}{'naive':>8}{'DIRECT':>9}{'NET+SPL':>9}{'PRIMTV':>9}")
    # cam_v3
    s_cam = oof["D_cam"]                       # net+split uses direct net
    print(f"  {'cam_v3':<8}{naive_c:>8.3f}{mae(yc,oof['D_cam']):>9.3f}"
          f"{mae(yc,s_cam):>9.3f}{mae(yc,oof['P_cam']):>9.3f}")
    # cam_o
    print(f"  {'cam_o':<8}{naive_o:>8.3f}{mae(yo,oof['D_camo']):>9.3f}"
          f"{mae(yo,oof['D_camo']):>9.3f}{mae(yo,oof['P_camo']):>9.3f}")
    # cam_d: DIRECT=independent, NET+SPLIT=cam_net - cam_o, PRIMTV=composed
    s_camd = oof["D_cam"] - oof["D_camo"]
    print(f"  {'cam_d':<8}{naive_d:>8.3f}{mae(yd,oof['D_camd']):>9.3f}"
          f"{mae(yd,s_camd):>9.3f}{mae(yd,oof['P_camd']):>9.3f}")
    # net reconstructed from O+D
    netD = oof["D_camo"] + oof["D_camd"]
    print(f"  {'O+D->net':<8}{'':>8}{mae(yc,netD):>9.3f}{mae(yc,s_cam):>9.3f}"
          f"{mae(yc,oof['P_cam']):>9.3f}")

    print("\n=== reconciliation residual: |cam_o + cam_d - cam_v3| (team-relevant) ===")
    print(f"  DIRECT (independent):   mean |O+D - net_direct| = "
          f"{np.mean(np.abs(netD - oof['D_cam'])):.3f}")
    print(f"  NET+SPLIT:              exact by construction (0.000)")
    print(f"  PRIMITIVES:             exact by construction (0.000)")

    print("\n=== PRIMITIVES robustness: SOS source (you lack oracle N+1 SOS at serve) ===")
    print("    [mean MAE | trimmed-1% MAE in brackets — split is degeneracy-robust]")
    print(f"  {'sos_source':<14}{'cam_v3':>16}{'cam_o':>16}{'cam_d':>16}")
    for label, psos_vec in [("oracle N+1", df.y_psos.values),
                            ("prior (N)", df.f_psos.values),
                            ("zero", np.zeros(len(df)))]:
        co, cd, cv = compose(pp["og"], pp["dg"], pp["ug"], pp["mp"], pp["gp"],
                             psos_vec, mean_min_by_season)
        print(f"  {label:<14}"
              f"{mae(yc,cv):>8.3f}[{mae_trim(yc,cv):>5.3f}]"
              f"{mae(yo,co):>8.3f}[{mae_trim(yo,co):>5.3f}]"
              f"{mae(yd,cd):>8.3f}[{mae_trim(yd,cd):>5.3f}]")
    print(f"  {'DIRECT (ref)':<14}"
          f"{mae(yc,oof['D_cam']):>8.3f}[{mae_trim(yc,oof['D_cam']):>5.3f}]"
          f"{mae(yo,oof['D_camo']):>8.3f}[{mae_trim(yo,oof['D_camo']):>5.3f}]"
          f"{mae(yd,oof['D_camd']):>8.3f}[{mae_trim(yd,oof['D_camd']):>5.3f}]")

    print("\n=== per-primitive prediction quality (PRIMITIVES approach, CV MAE) ===")
    for nm, col in [("ogbpm", "y_ogbpm"), ("dgbpm", "y_dgbpm"), ("usg", "y_usg"),
                    ("min_per", "y_min_per"), ("gp", "y_gp")]:
        oofp = np.full(len(df), np.nan)
        for tr, te in kf.split(X):
            oofp[te] = fit_predict(X[tr], df[col].values[tr], X[te])
        print(f"  {nm:<8} CV-MAE={mae(df[col].values, oofp):.3f}"
              f"  (sd={df[col].std():.2f})")

    # ---------------- RAPM-O/D feature arm ----------------
    # Same matched folds, feature set extended with prior-season pooled RAPM.
    rapm_feats = ["f_rapm_o", "f_rapm_d", "f_rapm_net", "f_rapm_poss"]
    n_rapm = int(df["f_rapm_o"].notna().sum())
    print("\n=== RAPM-O/D as features (matched folds, base vs base+RAPM) ===")
    print(f"  coverage: {n_rapm:,}/{len(df):,} rows with prior-season RAPM "
          f"({100 * n_rapm / len(df):.1f}%; gaps = 2019 + no-stint players)")
    Xr = df[FEATURES + rapm_feats].values
    folds = list(kf.split(X))
    oof_r = {k: np.full(len(df), np.nan) for k in ["cam", "camo", "camd"]}
    ppr = {k: np.full(len(df), np.nan) for k in ["og", "dg", "ug", "mp", "gp"]}
    for tr, te in folds:
        oof_r["cam"][te]  = fit_predict(Xr[tr], df.y_cam.values[tr], Xr[te])
        oof_r["camo"][te] = fit_predict(Xr[tr], df.y_cam_o.values[tr], Xr[te])
        oof_r["camd"][te] = fit_predict(Xr[tr], df.y_cam_d.values[tr], Xr[te])
        for nm, col in [("og", "y_ogbpm"), ("dg", "y_dgbpm"), ("ug", "y_usg"),
                        ("mp", "y_min_per"), ("gp", "y_gp")]:
            ppr[nm][te] = fit_predict(Xr[tr], df[col].values[tr], Xr[te])
    cor, cdr, cvr = compose(ppr["og"], ppr["dg"], ppr["ug"], ppr["mp"], ppr["gp"],
                            df.y_psos.values, mean_min_by_season)

    print(f"  {'target':<8}{'DIRECT':>9}{'+RAPM':>9}{'delta':>9}   "
          f"{'PRIMTV':>9}{'+RAPM':>9}{'delta':>9}")
    comps = [("cam_v3", yc, oof["D_cam"], oof_r["cam"], oof["P_cam"], cvr),
             ("cam_o", yo, oof["D_camo"], oof_r["camo"], oof["P_camo"], cor),
             ("cam_d", yd, oof["D_camd"], oof_r["camd"], oof["P_camd"], cdr)]
    for nm, y, base_d, with_d, base_p, with_p in comps:
        bd, wd = mae(y, base_d), mae(y, with_d)
        bp, wp = mae(y, base_p), mae(y, with_p)
        print(f"  {nm:<8}{bd:>9.3f}{wd:>9.3f}{wd - bd:>+9.4f}   "
              f"{bp:>9.3f}{wp:>9.3f}{wp - bp:>+9.4f}")

    print("  per-fold DIRECT delta (negative = RAPM helps):")
    for nm, y, base_d, with_d, _, _ in comps:
        ds = [mae(y[te], with_d[te]) - mae(y[te], base_d[te]) for _, te in folds]
        wins = sum(1 for d in ds if d < 0)
        print(f"    {nm:<8} {' '.join(f'{d:+.4f}' for d in ds)}  "
              f"({wins}/5 folds helped)")

    msk = df["f_rapm_o"].notna().values
    print(f"  RAPM-covered rows only (n={int(msk.sum()):,}), DIRECT:")
    for nm, y, base_d, with_d, _, _ in comps:
        b, w = mae(y[msk], base_d[msk]), mae(y[msk], with_d[msk])
        print(f"    {nm:<8} {b:.3f} -> {w:.3f} ({w - b:+.4f})")

    for nm, col in [("cam_o", "y_cam_o"), ("cam_d", "y_cam_d"), ("cam_v3", "y_cam")]:
        m = lgb.LGBMRegressor(**PARAMS)
        m.fit(Xr, df[col].values)
        imp = pd.Series(m.booster_.feature_importance("gain"),
                        index=FEATURES + rapm_feats)
        share = imp[rapm_feats].sum() / imp.sum()
        order = imp.sort_values(ascending=False).index.tolist()
        ranks = {f: order.index(f) + 1 for f in rapm_feats}
        print(f"  {nm} full-fit gain share of RAPM feats: {100 * share:.1f}%  "
              f"ranks(of {len(order)})={ranks}")

    # ---------------- incremental test: RAPM beyond raw on/off ----------------
    # The production trajectory contract already carries raw on/off
    # (prior_on_net_rtg / prior_net_on_off / prior_on_poss_share); §10.7's
    # add-test showed RAPM adds nothing on top for the NET target. This is
    # the O/D-target version of that question: base+onoff vs base+onoff+RAPM.
    onoff_feats = ["f_on_net", "f_net_onoff", "f_on_share", "f_oo_ortg", "f_oo_drtg"]
    n_oo = int(df["f_on_net"].notna().sum())
    print("\n=== incremental: RAPM beyond raw on/off (base+onoff vs +RAPM) ===")
    print(f"  on/off coverage: {n_oo:,}/{len(df):,} ({100 * n_oo / len(df):.1f}%)")
    Xo = df[FEATURES + onoff_feats].values
    Xor = df[FEATURES + onoff_feats + rapm_feats].values
    oof_o = {k: np.full(len(df), np.nan) for k in ["cam", "camo", "camd"]}
    oof_or = {k: np.full(len(df), np.nan) for k in ["cam", "camo", "camd"]}
    for tr, te in folds:
        for key, ycol in [("cam", "y_cam"), ("camo", "y_cam_o"), ("camd", "y_cam_d")]:
            oof_o[key][te] = fit_predict(Xo[tr], df[ycol].values[tr], Xo[te])
            oof_or[key][te] = fit_predict(Xor[tr], df[ycol].values[tr], Xor[te])
    print(f"  {'target':<8}{'base':>9}{'+onoff':>9}{'+oo+RAPM':>10}"
          f"{'RAPM incr':>11}{'folds':>7}")
    for nm, y, base_d, key in [("cam_v3", yc, oof["D_cam"], "cam"),
                               ("cam_o", yo, oof["D_camo"], "camo"),
                               ("cam_d", yd, oof["D_camd"], "camd")]:
        mo, mor = mae(y, oof_o[key]), mae(y, oof_or[key])
        ds = [mae(y[te], oof_or[key][te]) - mae(y[te], oof_o[key][te])
              for _, te in folds]
        wins = sum(1 for d in ds if d < 0)
        print(f"  {nm:<8}{mae(y, base_d):>9.3f}{mo:>9.3f}{mor:>10.3f}"
              f"{mor - mo:>+11.4f}{wins:>5}/5")


if __name__ == "__main__":
    main()
