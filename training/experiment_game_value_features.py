"""Head-to-head backtest: do value-weighted roster-shape features (diff_rv_*)
improve the game-margin model over the current 49-feature baseline?

Motivation. The production roster aggregate (features.compute_cumulative_roster_stats)
is a minutes-weighted MEAN plus a "star" slot keyed off highest MINUTES. It is
blind to (a) the best player by VALUE when he isn't the minutes leader and (b)
value CONCENTRATION (top-heavy vs balanced roster with the same mean). The
candidate `diff_rv_*` features (gated behind ROSTER_VALUE_FEATURES in features.py)
express that shape: rv_top1_gbpm, rv_top3_gbpm, rv_gbpm_gap12, rv_gbpm_std.

Method. Build the feature matrix ONCE with rv columns present, then train margin
(+win) LightGBM models on the IDENTICAL rows (rv excluded from the completeness
dropna) with vs without the rv features, leave-one-season-out, seed-averaged.
Same LGBM params as train_loso.py. This is the standard "gated feature +
LOSO A/B" pattern used by experiment_game_pbp.py / experiment_game_lineups.py.

Companion to the injury investigation (docs/injury_availability_investigation.md):
that study showed the availability effect is already absorbed by the served model
and surfaced this roster-shape gap as the one buildable, no-external-feed lever.

ACCEPT BAR (ironclad, no overfit): candidate must lower pooled margin MAE AND
lower MAE in >= 5 of 6 folds, with >= 1 rv feature carrying non-trivial gain.
Anything mixed-sign or within seed noise => REJECT (revert the features.py gate;
CamPom's mean already carries the shape). A pass here is only a SCREEN on
season-aggregate CamPom; productionizing additionally requires re-confirming on
the point-in-time (pit_cam_v3) LOSO before touching the served path.

Run:  cd training && ROSTER_VALUE_FEATURES=1 .venv/bin/python experiment_game_value_features.py
Writes: training/eval_history/value_features_backtest_<date>.json  (+ stdout table)
Matrix is cached to a temp parquet (CACHE_DIR env, default the system temp) so
re-runs / ablations skip the slow build.
"""
import os, sys, json, time, tempfile
os.environ.setdefault("ROSTER_VALUE_FEATURES", "1")
os.environ.setdefault("DATABASE_URL", "postgresql://cstat:cstat@localhost:5432/cstat")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import numpy as np, pandas as pd, lightgbm as lgb
from sklearn.metrics import mean_absolute_error, roc_auc_score

HERE = os.path.dirname(os.path.abspath(__file__))
CACHE_DIR = os.environ.get("CACHE_DIR", tempfile.gettempdir())
MATRIX = os.path.join(CACHE_DIR, "cstat_value_feature_matrix.pkl")
COLS = os.path.join(CACHE_DIR, "cstat_value_feature_cols.json")
RESULT = os.environ.get("RESULT_JSON", os.path.join(HERE, "eval_history",
                      f"value_features_backtest_{time.strftime('%Y%m%d')}.json"))

HOLDOUTS = [2021, 2022, 2023, 2024, 2025, 2026]
SEEDS = [0, 1, 2, 3, 4]
BASE_PARAMS = dict(num_leaves=24, learning_rate=0.03, feature_fraction=0.7,
                   bagging_fraction=0.7, bagging_freq=5, min_child_samples=30,
                   lambda_l1=0.1, lambda_l2=1.0, verbose=-1, n_estimators=1000)

def log(m): print(m, flush=True)

# ---- 1. build or load matrix ----
if os.path.exists(MATRIX) and os.path.exists(COLS):
    log(f"[{time.strftime('%H:%M:%S')}] loading cached matrix {MATRIX}")
    df = pd.read_pickle(MATRIX)
    feature_cols = json.load(open(COLS))["feature_cols"]
else:
    from db import get_engine
    from features import build_feature_matrix, completeness_subset, ROSTER_VALUE_FEATURES
    if not ROSTER_VALUE_FEATURES:
        log("ERROR: ROSTER_VALUE_FEATURES is off; set it to 1 and re-run."); sys.exit(2)
    log(f"[{time.strftime('%H:%M:%S')}] building feature matrix (slow, ~1-2h)...")
    t = time.time()
    df, feature_cols, _ = build_feature_matrix(get_engine())
    log(f"[{time.strftime('%H:%M:%S')}] BUILT in {time.time()-t:.0f}s: {len(df)} rows x {len(feature_cols)} feats")
    df = df.dropna(subset=completeness_subset(feature_cols)).reset_index(drop=True)
    keep = list(dict.fromkeys(feature_cols + ["season", "game_date", "game_id",
                "home_team_id", "away_team_id", "margin", "home_win"]))
    try:  # caching is best-effort — a cache failure must NOT waste the ~2h build
        df[keep].to_pickle(MATRIX)
        json.dump({"feature_cols": feature_cols}, open(COLS, "w"))
        log(f"[{time.strftime('%H:%M:%S')}] cached -> {MATRIX}")
    except Exception as e:
        log(f"[{time.strftime('%H:%M:%S')}] WARN: cache write failed ({e}); continuing in-memory")

rv_cols = [c for c in feature_cols if c.startswith("diff_rv_")]
base_cols = [c for c in feature_cols if not c.startswith("diff_rv_")]
if not rv_cols:
    log("ERROR: no diff_rv_ columns in matrix (rebuild with ROSTER_VALUE_FEATURES=1)."); sys.exit(2)
log(f"rows={len(df)}  base={len(base_cols)}  rv=+{len(rv_cols)} {rv_cols}\n")

# ---- 2. A/B ----
def fit_eval(fit, val, te, cols, seed):
    p = dict(BASE_PARAMS, random_state=seed, bagging_seed=seed, feature_fraction_seed=seed)
    m = lgb.LGBMRegressor(objective="regression", metric="mae", **p)
    m.fit(fit[cols], fit["margin"], eval_set=[(val[cols], val["margin"])],
          eval_metric="mae", callbacks=[lgb.early_stopping(80, verbose=False)])
    w = lgb.LGBMClassifier(objective="binary", metric="binary_logloss", **p)
    w.fit(fit[cols], fit["home_win"], eval_set=[(val[cols], val["home_win"])],
          eval_metric="binary_logloss", callbacks=[lgb.early_stopping(80, verbose=False)])
    return (mean_absolute_error(te["margin"], m.predict(te[cols])),
            roc_auc_score(te["home_win"], w.predict_proba(te[cols])[:, 1]),
            m.booster_.feature_importance("gain"))

folds = []; gain = np.zeros(len(feature_cols))
log(f"{'yr':>6}{'n':>7}{'MAE_base':>10}{'MAE_cand':>10}{'dMAE':>9}{'AUC_base':>10}{'AUC_cand':>10}{'dAUC':>9}")
for yr in HOLDOUTS:
    tr = df[df.season != yr].sort_values("game_date").reset_index(drop=True)
    te = df[df.season == yr]
    if len(te) == 0:
        continue
    vs = int(len(tr) * 0.85); fit, val = tr.iloc[:vs], tr.iloc[vs:]
    mb = mc = ab = ac = 0.0
    for s in SEEDS:
        rb = fit_eval(fit, val, te, base_cols, s); rc = fit_eval(fit, val, te, feature_cols, s)
        mb += rb[0]; ab += rb[1]; mc += rc[0]; ac += rc[1]; gain += rc[2]
    k = len(SEEDS); mb /= k; mc /= k; ab /= k; ac /= k
    folds.append(dict(year=yr, n=int(len(te)), mae_base=mb, mae_cand=mc, dmae=mc-mb,
                      auc_base=ab, auc_cand=ac, dauc=ac-ab))
    log(f"{yr:>6}{len(te):>7}{mb:>10.4f}{mc:>10.4f}{mc-mb:>+9.4f}{ab:>10.4f}{ac:>10.4f}{ac-ab:>+9.4f}")

nt = sum(f["n"] for f in folds)
wavg = lambda k: sum(f[k]*f["n"] for f in folds)/nt
pooled = {k: wavg(k) for k in ["mae_base","mae_cand","auc_base","auc_cand"]}
pooled["dmae"] = pooled["mae_cand"]-pooled["mae_base"]; pooled["dauc"] = pooled["auc_cand"]-pooled["auc_base"]
improved = sum(1 for f in folds if f["dmae"] < 0)
gorder = np.argsort(gain)[::-1]; rank = {feature_cols[j]: i for i, j in enumerate(gorder)}
rv_gain = {c: {"gain": float(gain[feature_cols.index(c)]), "rank": rank[c]+1,
               "of": len(feature_cols)} for c in rv_cols}

accept = (pooled["dmae"] < 0 and improved >= 5 and
          any(rank[c]+1 <= 40 and gain[feature_cols.index(c)] > 0 for c in rv_cols))
verdict = "ACCEPT (screen passed; next: confirm on pit_cam_v3 LOSO)" if accept else \
          "REJECT (fails ironclad bar; revert the features.py gate)"

log(f"\nPOOLED MAE {pooled['mae_base']:.4f} -> {pooled['mae_cand']:.4f}  (delta {pooled['dmae']:+.4f})")
log(f"POOLED AUC {pooled['auc_base']:.4f} -> {pooled['auc_cand']:.4f}  (delta {pooled['dauc']:+.4f})")
log(f"folds MAE-improved: {improved}/{len(folds)}")
log("rv gain importance:")
for c, g in rv_gain.items():
    log(f"  {c:<20} gain={g['gain']:>12.0f}  rank {g['rank']}/{g['of']}")
log(f"\nVERDICT: {verdict}")

out = dict(experiment="value_weighted_roster_features",
           timestamp=time.strftime("%Y-%m-%d %H:%M:%S"),
           gbpm_variant=os.environ.get("GBPM_VARIANT", "raw"),
           rv_features=rv_cols, seeds=SEEDS, holdouts=HOLDOUTS, n_rows=int(len(df)),
           folds=folds, pooled=pooled, folds_mae_improved=improved,
           rv_gain_importance=rv_gain, accept=accept, verdict=verdict)
os.makedirs(os.path.dirname(RESULT), exist_ok=True)
json.dump(out, open(RESULT, "w"), indent=2)
log(f"[{time.strftime('%H:%M:%S')}] DONE -> {RESULT}")
