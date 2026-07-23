# Value-weighted roster-shape features — tested and rejected

**Status: tested, REJECTED (2026-07-23). No serving change.** A leave-one-season-out
A/B could not clear the accept bar; the candidate features are left gated OFF
(`ROSTER_VALUE_FEATURES` in `training/features.py`, default `"0"`), exactly as the
similarly-rejected `PBP_FEATURES` / `LINEUP_FEATURES` gates are. This note records
why, so it is not re-attempted.

## Motivation

Spun out of the injury investigation (`docs/injury_availability_investigation.md`),
which found the availability effect already priced by the served model and surfaced
one buildable, no-external-feed lever: the game model's roster aggregate
(`features.compute_cumulative_roster_stats`) is a **minutes-weighted mean** plus a
**"star" slot keyed off highest minutes** — structurally blind to (a) the best
player by *value* when he isn't the minutes leader, and (b) value *concentration*
(a top-heavy roster vs a balanced one with the same mean). Hypothesis: features that
express that shape improve margin prediction.

## Candidate features

Computed in `weighted_agg` from the same per-player GBPM already loaded, gated into
the vector as `diff_rv_*`:

- `rv_top1_gbpm` — best player by value (not necessarily the minutes star)
- `rv_top3_gbpm` — mean of the top-3 by value (on-floor elite production)
- `rv_gbpm_gap12` — #1 − #2 value gap (star separation)
- `rv_gbpm_std` — dispersion of value (top-heavy vs balanced)

## Method

`training/experiment_game_value_features.py`. Build the feature matrix once with the
`rv` columns present, then train margin (+win) LightGBM on the **identical rows**
(rv excluded from the completeness dropna) with vs without the 4 features,
leave-one-season-out over 2021–2026, **5-seed-averaged**, same params as
`train_loso.py`. Screen used season-aggregate CamPom (`GBPM_VARIANT=raw`); a pass
would then have required re-confirmation on the point-in-time `pit_cam_v3` LOSO.

**Accept bar (ironclad, no overfit aggregated across the season):** lower pooled
margin MAE AND lower MAE in ≥5 of 6 folds, with ≥1 rv feature carrying non-trivial
gain.

## Result

| Holdout | n | MAE base | MAE cand | ΔMAE |
|---|---|---|---|---|
| 2021 | 2659 | 8.258 | 8.261 | +0.003 |
| 2022 | 4201 | 8.011 | 7.981 | −0.030 |
| 2023 | 4422 | 8.210 | 8.191 | −0.019 |
| 2024 | 4436 | 8.257 | 8.222 | −0.036 |
| 2025 | 4704 | 8.139 | 8.148 | +0.010 |
| 2026 | 4793 | 8.350 | 8.360 | +0.010 |

Pooled MAE **8.2035 → 8.1929 (−0.0106, ~0.13%)**. Folds improved: **3/6**.
AUC **+0.0005** (noise). Full JSON:
`training/eval_history/value_features_backtest_20260723.json`.

**Verdict: REJECT.** Fails the bar on fold-consistency (3/6, mixed sign — helps
2022–24, hurts 2021/25/26) and the pooled gain is a rounding error (~0.01 pts on an
8.2 base).

## Why it fails

The `rv` features draw **high gain-importance** (`rv_top3_gbpm` ranked 2nd of 53) —
LightGBM splits on them heavily — yet buy ~zero out-of-fold accuracy. That gap
between in-sample usage and out-of-fold lift is the redundancy/overfit signature:
**CamPom's minutes-weighted mean and star slot already carry the durable
roster-shape signal**, so value-star / concentration / dispersion mostly re-fit
noise. Same outcome, same cause as the Tier-1 PBP and Tier-2 lineup-quality game
features — CamPom absorbs the value.

## What would re-open this

- A roster-shape signal genuinely *orthogonal* to CamPom's mean/star (this one was
  not), demonstrated to lower MAE in ≥5/6 LOSO folds.
- Only re-test with the point-in-time `pit_cam_v3` variant if the cheap
  season-aggregate screen passes first — it did not here.

## Reproduction

```
cd training && ROSTER_VALUE_FEATURES=1 .venv/bin/python experiment_game_value_features.py
```

The built matrix is cached (pickle, `CACHE_DIR`/`cstat_value_feature_matrix.pkl`), so
re-runs and single-feature ablations skip the ~2h build. Note: the build cost is
entirely the pre-existing per-date adjusted-efficiency loop in `build_feature_matrix`
(1698 dates), not this experiment.
