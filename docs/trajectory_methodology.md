# Trajectory Model — Phase 5c Growth Projection

## What it is

The trajectory model projects a returning player's **next-season CamPom v3** from their prior-season stats. Three LightGBM regressors trained on the same 48-feature input share one feature shape:

- `trajectory_mean_model.onnx` — regression objective (`mean` prediction)
- `trajectory_q10_model.onnx` — quantile objective at α=0.1 (`lower` band)
- `trajectory_q90_model.onnx` — quantile objective at α=0.9 (`upper` band)

Surfaced today as the "Proj YYYY-YY" badge on PlayerDetail next to the current-season CamPom chip — `mean (lower–upper)`, with an arrow showing direction vs prior year.

## Training data

Trained on all 11 adjacent-season pairs from the 12-season ingest (2015–2026):

| Source pair | rows after gate |
|---|---|
| 2015 → 2016 | 2,023 |
| 2016 → 2017 | 2,042 |
| 2017 → 2018 | 2,061 |
| 2018 → 2019 | 2,027 |
| 2019 → 2020 | 2,048 |
| 2020 → 2021 | 2,146 |
| 2021 → 2022 | 2,582 |
| 2022 → 2023 | 2,377 |
| 2023 → 2024 | 2,438 |
| 2024 → 2025 | 2,311 |
| 2025 → 2026 | 2,113 |
| **total** | **24,168** |

Cross-season same-player join is via `torvik_player_stats.torvik_pid` — the stable cross-season key (96% coverage, zero collisions). `players.natstat_id` breaks on transfers (different code per team) so we don't use it for the join.

**Qualification gate** (both seasons in the pair must pass):
- `games_played >= 5`
- `minutes_per_game >= 5`

Same string as `roster_impact_model_meta.json::player_filter` so the Rust path can reuse `cstat_core::roster_features::QUAL_FILTER_STRING`. Boot-time validator (`inference.rs::validate_trajectory_meta`) hard-fails on drift.

## Features (48, order locked)

The order in `trajectory_model_meta.json::features` is wire-locked to `cstat_core::trajectory::TRAJECTORY_FEATURE_NAMES`. A boot-time test confirms the two match exactly.

| group | features | count |
|---|---|---|
| Volume / context | `prior_mpg`, `prior_gp`, `prior_total_min`, `prior_height_in`, `prior_class_year_code` | 5 |
| Box score per-game | `prior_ppg`, `prior_rpg`, `prior_apg`, `prior_spg`, `prior_bpg`, `prior_topg` | 6 |
| Rate stats | `prior_ts`, `prior_efg`, `prior_usg`, `prior_ast_pct`, `prior_tov_pct`, `prior_orb_pct`, `prior_drb_pct`, `prior_stl_pct`, `prior_blk_pct`, `prior_ft_rate` | 10 |
| Impact metrics | `prior_ogbpm`, `prior_dgbpm`, `prior_gbpm`, `prior_campom` | 4 |
| Archetype mixture | `arch_{wizard,sorcerer,warlock,bard,ranger,barbarian,paladin,monk,cleric,druid,rogue,fighter}` — primary 1.0× / secondary 0.5× | 12 |
| Recruit block (shared with freshman model) | `recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit` | 11 |

`prior_class_year_code` encoding: `Fr=0, So=1, Jr=2, Sr=3, Gr=4, NULL/unknown=-1`. NULL gets a separate bucket rather than imputation — LightGBM splits can isolate the unknown cohort, and many real rows have NULL class year (Torvik bio coverage is partial).

Missing rate-stat values are filled with `0.0` at the Rust feature builder (matches `roster_features.rs` convention; for qualified players the gp/mpg gate keeps box stats populated).

## What's NOT in the feature set (and why)

- ~**Recruit rank** (`composite_rank`, `years_since_recruit`, `is_ranked`) — every row in the 2024–2026 training data has a recruiting class of 2021–2024, and we only have class-of-2026 in the `recruits` table today. So `composite_rank` would be NULL on 100% of training rows. Deferred to a follow-up ablation experiment, gated on the historical recruit ingest (2021–2025 backfill).~ *(shipped — 11 recruit features now in the trained model; recruit classes 2021-2026 ingested; coverage is partial but LightGBM handles NULL via `is_ranked=0` sentinel).*
- **Destination team for transferring players** — v1 model is destination-agnostic. Cross-team transferring returners (joined via `torvik_pid`) are projected against the same prior as same-team returners; the model has no signal about how a Princeton→Duke transfer's role will change. Wider bands and a `direction` arrow that may surprise are the price; documented limitation.
- **Last-season team identity / strength** — implicitly encoded through the player's own minute/usage/CamPom but no explicit team-strength column. Adding `prior_team_adj_em` is the natural v2 feature.

## Backtest

**Naive baseline** (year N+1 ≈ year N CamPom):
- pooled MAE 2.339, RMSE 3.049, R² 0.530

**Model (mean) — leave-one-pair-out (11-pair cohort, 2015-2026):**
- pair 2015→2016: MAE 2.053, RMSE 2.672, R² 0.658, n=2,023
- pair 2016→2017: MAE 2.116, RMSE 2.787, R² 0.627, n=2,042
- pair 2017→2018: MAE 2.102, RMSE 2.766, R² 0.615, n=2,061
- pair 2018→2019: MAE 2.147, RMSE 2.824, R² 0.615, n=2,027
- pair 2019→2020: MAE 2.047, RMSE 2.646, R² 0.624, n=2,048
- pair 2020→2021: MAE 2.104, RMSE 2.748, R² 0.594, n=2,146
- pair 2021→2022: MAE 2.164, RMSE 2.834, R² 0.545, n=2,582
- pair 2022→2023: MAE 2.134, RMSE 2.770, R² 0.574, n=2,377
- pair 2023→2024: MAE 2.123, RMSE 2.780, R² 0.614, n=2,438
- pair 2024→2025: MAE 2.168, RMSE 2.858, R² 0.611, n=2,311
- pair 2025→2026: MAE 2.294, RMSE 2.989, R² 0.589, n=2,113
- **pooled: MAE 2.133, RMSE 2.792, R² 0.606**

Beats naive by **−0.21 MAE pooled**.

**5-fold random CV:** MAE 2.130, RMSE 2.788, R² 0.607.

**Per–prior-class-year MAE** (model vs naive):

| class_year_code | n | model MAE | naive MAE | Δ |
|---|---|---|---|---|
| 0 (Fr→So) | 6,543 | 1.827 | 2.302 | +0.48 |
| 1 (So→Jr) | 7,098 | 1.967 | 2.328 | +0.36 |
| 2 (Jr→Sr) | 8,354 | 1.991 | 2.346 | +0.36 |
| 3 (Sr→Gr) | 2,171 | 2.062 | 2.459 | +0.40 |

Model beats naive in every bucket by **0.36–0.48 MAE**. Per-bucket MAE on the full-data fit (~1.8–2.1) is better than the LOPO MAE (~2.13) — the gap is the honest generalization cost across the pair-folds.

Top features (mean model, by split count):
1. `prior_campom` (739)
2. `prior_usg` (572)
3. `prior_dgbpm` (458)
4. `prior_ogbpm` (427)
5. `prior_gbpm` (426)
6. `prior_efg` (408)
7. `prior_mpg` (402)
8. `prior_ft_rate` (389)

Prior CamPom is the dominant signal, which is intuitive — it's the most-aggregated impact metric. The interesting non-obvious result: `prior_usg` is #2, suggesting role-on-team is a meaningful growth differentiator beyond raw impact.

## Honesty framing for UI consumers

- **Pooled LOPO MAE is ~2.2 CamPom points.** Render projections as directional, not point estimates. The 80% band width (q90 − q10) is what users should read for confidence.
- **Bands wider on freshmen and low-minute returners** — the model correctly flags thin-signal cases. Don't try to tighten them.
- **Selection bias on returners** (per ROADMAP §5c caveat): top-ranked freshmen who *return* are negatively selected (the Cooper Flagg / Boozer cohort leaves for the draft; the 5-stars who stay are disproportionately those whose freshman year disappointed). The model doesn't see the leave-for-draft cohort, so projections for "5-star high-impact freshman" → year 2 will systematically underestimate the elite ceiling.
- **Transferring returners get destination-agnostic projections.** The arrow direction may be misleading if the player is joining a roster with a very different usage / role profile.

## Retraining playbook

Run when:

1. A new cstat-season ingests (e.g. cstat-season 2027 lands → 2026→2027 becomes a new training pair).
2. Pre-2021 recruit-class backfill completes (the current model already includes recruit features for classes 2021-2026; older classes would lift recruit coverage for the upperclassmen in our older training pairs — e.g. a senior in the 2022→2023 pair was recruited in ~2018, currently in the `is_ranked=0` sentinel branch).
3. The CamPom v3 formula changes (e.g. CamPom v4) — target shifts, so retrain.

```bash
# From repo root:
cd training && source .venv/bin/activate
python train_trajectory_model.py
```

Outputs:
- `training/models/trajectory_mean_model.onnx`
- `training/models/trajectory_q10_model.onnx`
- `training/models/trajectory_q90_model.onnx`
- `training/models/trajectory_model_meta.json`

The boot-time validator in `crates/cstat-core/src/inference.rs::validate_trajectory_meta` will hard-fail at API startup if:
- `player_filter` drifts from `cstat_core::roster_features::QUAL_FILTER_STRING`
- `n_features` ≠ compiled `TRAJECTORY_NUM_FEATURES`
- feature-name order drifts from `TRAJECTORY_FEATURE_NAMES`
- `quantile_alphas` aren't exactly `{q10: 0.1, q90: 0.9}`

When changing the feature set:
1. Update `NUMERIC_FEATURE_COLS` / `ARCH_FEATURE_COLS` in `train_trajectory_model.py`.
2. Mirror the order in `TRAJECTORY_FEATURE_NAMES` (Rust) and update `TRAJECTORY_NUM_FEATURES`.
3. Update `build_trajectory_features` in Rust to populate the new slots.
4. Retrain — boot will fail loudly if anything's out of sync.

When adding a new pair-fold (new season ingested):
- Update `SEASONS` tuple in `train_trajectory_model.py`.
- No Rust changes needed — pair count doesn't affect feature shape.

## Open questions

- **Walk-forward CV** vs the current LOPO. With 11 pairs (2015→2016 … 2025→2026) we have ample folds to do walk-forward CV: train on `≤ N`, predict `N+1`, advance. Currently the LOPO holds 1 pair out anywhere in the timeline, including pairs that come *after* training rows — walk-forward would tighten the honesty story by predicting only with prior-season data. Implementation lift is small; the existing `leave_one_pair_out` loop just needs an order constraint.
- **Destination-aware projection** for transferring players. Easy feature add: `dest_team_adj_em` (target team AdjEM at season N, since season N+1 doesn't exist yet at projection time). Requires the projection caller to also supply the destination team — fine for the 2027 projection page consumer, but `/api/players/:id` doesn't know the destination, so this would either need a separate endpoint or a "no projection available for transfers" gate.
- **Calibration over time**: once we have OOF predictions from this model on a real future season, plot predicted vs actual binned by predicted-CamPom — the model should be well-calibrated near the mean and progressively over-/under-confident at the tails.
