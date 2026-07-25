# Trajectory Model — Phase 5c Growth Projection

## What it is

The trajectory model projects a returning player's **next-season CamPom v3** from their prior-season stats plus a multi-season history block (so it sees a player's *progression trajectory*, not just a single snapshot). Three LightGBM regressors trained on the same 60-feature input share one feature shape:

- `trajectory_mean_model.onnx` — regression objective (`mean` prediction)
- `trajectory_q10_model.onnx` — quantile objective at α=0.1 (`lower` band)
- `trajectory_q90_model.onnx` — quantile objective at α=0.9 (`upper` band)

Surfaced today as the "Proj YYYY-YY" badge on PlayerDetail next to the current-season CamPom chip — `mean (lower–upper)`, with an arrow showing direction vs prior year.

## Training data

Trained on all 11 adjacent-season pairs from the 12-season ingest (2015–2026):

| Source pair | rows after gate |
|---|---|
| 2015 → 2016 | 2,033 |
| 2016 → 2017 | 2,078 |
| 2017 → 2018 | 2,074 |
| 2018 → 2019 | 2,066 |
| 2019 → 2020 | 2,104 |
| 2020 → 2021 | 2,166 |
| 2021 → 2022 | 2,608 |
| 2022 → 2023 | 2,394 |
| 2023 → 2024 | 2,454 |
| 2024 → 2025 | 2,434 |
| 2025 → 2026 | 2,297 |
| **total** | **24,708** |

Cross-season same-player join is via `torvik_player_stats.torvik_pid` — the stable cross-season key (96% coverage, zero collisions). `players.natstat_id` breaks on transfers (different code per team) so we don't use it for the join.

**Qualification gate** (both seasons in the pair must pass):
- `games_played >= 5`
- `minutes_per_game >= 5`

Same string as `roster_impact_model_meta.json::player_filter` so the Rust path can reuse `cstat_core::roster_features::QUAL_FILTER_STRING`. Boot-time validator (`inference.rs::validate_trajectory_meta`) hard-fails on drift.

## Features (60, order locked)

The order in `trajectory_model_meta.json::features` is wire-locked to `cstat_core::trajectory::TRAJECTORY_FEATURE_NAMES`. A boot-time test confirms the two match exactly.

| group | features | count |
|---|---|---|
| Volume / context | `prior_mpg`, `prior_gp`, `prior_total_min`, `prior_height_in`, `prior_class_year_code` | 5 |
| Box score per-game | `prior_ppg`, `prior_rpg`, `prior_apg`, `prior_spg`, `prior_bpg`, `prior_topg` | 6 |
| Rate stats | `prior_ts`, `prior_efg`, `prior_usg`, `prior_ast_pct`, `prior_tov_pct`, `prior_orb_pct`, `prior_drb_pct`, `prior_stl_pct`, `prior_blk_pct`, `prior_ft_rate` | 10 |
| Impact metrics | `prior_ogbpm`, `prior_dgbpm`, `prior_gbpm`, `prior_campom` | 4 |
| On/off splits | `prior_on_net_rtg`, `prior_net_on_off`, `prior_on_poss_share` — the `player_on_off` rollup; `-999` sentinel where the rollup has no row (sub-rotation, 2019) or the swing lacks an OFF sample (Tier-2 membership backtest 2026-06-11) | 3 |
| Multi-season history | `prior2_campom`, `prior2_mpg`, `prior2_gp`, `prior2_usg`, `prior2_ppg` (prior-PRIOR season levels) + `has_prior2` indicator + `delta_campom`, `delta_mpg`, `delta_usg` (year-over-year slope) — levels fill `-999` / deltas fill `0` where no N-1 season; the N-1 row is joined on the cross-season `torvik_pid` | 9 |
| Archetype mixture | `arch_{wizard,sorcerer,warlock,bard,ranger,barbarian,paladin,monk,cleric,druid,rogue,fighter}` — primary 1.0× / secondary 0.5× | 12 |
| Recruit block (shared with freshman model) | `recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit` | 11 |

`prior_class_year_code` encoding: `Fr=0, So=1, Jr=2, Sr=3, Gr=4, NULL/unknown=-1`. NULL gets a separate bucket rather than imputation — LightGBM splits can isolate the unknown cohort, and many real rows have NULL class year (Torvik bio coverage is partial).

Missing rate-stat values are filled with `0.0` at the Rust feature builder (matches `roster_features.rs` convention; for qualified players the gp/mpg gate keeps box stats populated).

### Multi-season history block (validated 2026-06-18)

The model is otherwise anchored on a single season (N), so it's blind to a player's *progression slope* — the Caden Pierce problem: 10.34 CamPom (2024) → 0.34 (2025), where a single-prior-season model projecting off 2025 has no idea 2024 was elite. The history block adds the prior-PRIOR (N-1) season:

- **Lag-2 levels** — `prior2_campom/mpg/gp/usg/ppg`, the player's level two years back.
- **Slope deltas** — `delta_campom/mpg/usg` = prior_N − prior_{N-1}, the recent year-over-year trajectory.
- **`has_prior2`** indicator — lets the tree split out the no-history cohort explicitly.

The N-1 row is joined on the cross-season `torvik_pid` (so a transfer's prior-prior season at a different school still links, and a gap-year player like Pierce links 2025 → 2024). Coverage is **52.8%** of pairs; the other 47.2% are true freshmen and careers starting before the 2015 data floor — for them the levels fill `-999`, the deltas fill `0`, and `has_prior2=0`. Each column fills independently, so a torvik N-1 row with no matching `player_season_stats` row degrades per-column.

**Backtest (leave-one-pair-out):** pooled MAE 2.121 → **2.088** full; on the prior-2-covered subset 2.206 → **2.141** (+0.065), winning 10/11 season folds. Lift concentrates on upperclassmen (Jr +0.074, Sr +0.061, So +0.059) with no regression on the no-history half. Serve parity (the ONNX path fills sentinels rather than NaN-native LightGBM routing) was re-confirmed 2026-06-27 at +0.0625 covered — only ~0.004 below the NaN-native gain. The training fill (`LAG2_LEVEL_SENTINEL` / `SLOPE_DELTA_FILL`) and the Rust serve fill (`trajectory.rs::build_trajectory_features`) are kept in lockstep.

**A third season (N-2) was tested and rejected** (`experiment_trajectory_lag3.py`, 2026-06-27): a lag-3 level block and an acceleration term added only +0.004–0.008 covered MAE on the 29%-covered 3-consecutive-season cohort — roughly an order of magnitude below the lag-2 win and under the on/off acceptance bar (+0.011). The most recent season plus a 1-year slope captures essentially all the recoverable trajectory signal.

## What's NOT in the feature set (and why)

- ~**Recruit rank** (`composite_rank`, `years_since_recruit`, `is_ranked`) — every row in the 2024–2026 training data has a recruiting class of 2021–2024, and we only have class-of-2026 in the `recruits` table today. So `composite_rank` would be NULL on 100% of training rows. Deferred to a follow-up ablation experiment, gated on the historical recruit ingest (2021–2025 backfill).~ *(shipped — 11 recruit features now in the trained model; recruit classes 2021-2026 ingested; coverage is partial but LightGBM handles NULL via `is_ranked=0` sentinel).*
- **Destination team for transferring players** — v1 model is destination-agnostic. Cross-team transferring returners (joined via `torvik_pid`) are projected against the same prior as same-team returners; the model has no signal about how a Princeton→Duke transfer's role will change. Wider bands and a `direction` arrow that may surprise are the price; documented limitation.
- **Last-season team identity / strength** — implicitly encoded through the player's own minute/usage/CamPom but no explicit team-strength column. Adding `prior_team_adj_em` is the natural v2 feature.

## Backtest

Numbers below are the 60-feature model (incl. the multi-season history block). For the single-prior-season model these were pooled MAE 2.133 — the history block took it to 2.088 (see the history-block subsection for the covered-subset breakdown).

**Naive baseline** (year N+1 ≈ year N CamPom):
- pooled MAE 2.342, RMSE 3.053, R² 0.526

**Model (mean) — leave-one-pair-out (11-pair cohort, 2015-2026):**
- pair 2015→2016: MAE 2.047, RMSE 2.673, R² 0.658, n=2,033
- pair 2016→2017: MAE 2.071, RMSE 2.725, R² 0.645, n=2,078
- pair 2017→2018: MAE 2.064, RMSE 2.707, R² 0.633, n=2,074
- pair 2018→2019: MAE 2.080, RMSE 2.718, R² 0.632, n=2,066
- pair 2019→2020: MAE 1.983, RMSE 2.559, R² 0.644, n=2,104
- pair 2020→2021: MAE 2.067, RMSE 2.685, R² 0.611, n=2,166
- pair 2021→2022: MAE 2.080, RMSE 2.732, R² 0.576, n=2,608
- pair 2022→2023: MAE 2.054, RMSE 2.658, R² 0.613, n=2,394
- pair 2023→2024: MAE 2.107, RMSE 2.750, R² 0.624, n=2,454
- pair 2024→2025: MAE 2.130, RMSE 2.787, R² 0.626, n=2,434
- pair 2025→2026: MAE 2.262, RMSE 2.937, R² 0.597, n=2,297
- **pooled: MAE 2.088, RMSE 2.725, R² 0.623**

Beats naive by **−0.25 MAE pooled**.

**5-fold random CV:** MAE 2.084, RMSE 2.719, R² 0.624.

**Per–prior-class-year MAE** (model vs naive):

| class_year_code | n | model MAE | naive MAE | Δ |
|---|---|---|---|---|
| 0 (Fr→So) | 6,629 | 1.836 | 2.303 | +0.47 |
| 1 (So→Jr) | 7,243 | 1.882 | 2.329 | +0.45 |
| 2 (Jr→Sr) | 8,554 | 1.916 | 2.346 | +0.43 |
| 3 (Sr→Gr) | 2,245 | 1.971 | 2.502 | +0.53 |

Model beats naive in every bucket by **0.43–0.53 MAE**. Per-bucket MAE on the full-data fit (~1.8–2.0) is better than the LOPO MAE (~2.09) — the gap is the honest generalization cost across the pair-folds. (The `class_year_code=-1` unknown bucket, n=37, is too small to be meaningful.)

Top features (mean model, by split count):
1. `prior_campom` (714)
2. `prior_on_net_rtg` (479)
3. `prior_usg` (427)
4. `delta_campom` (388)
5. `prior_net_on_off` (345)
6. `prior_efg` (312)
7. `prior2_campom` (311)
8. `prior_gbpm` (307)

Prior CamPom is the dominant signal, which is intuitive — it's the most-aggregated impact metric. Two non-obvious results: the on/off splits (`prior_on_net_rtg` #2, `prior_net_on_off` #5) carry real signal, and the **multi-season history block lands `delta_campom` at #4 and `prior2_campom` at #7** — the slope and the prior-prior level are among the most-split features, confirming the block is load-bearing rather than decorative.

## Honesty framing for UI consumers

- **Pooled LOPO MAE is ~2.2 CamPom points.** Render projections as directional, not point estimates. The 80% band width (q90 − q10) is what users should read for confidence.
- **Bands wider on freshmen and low-minute returners** — the model correctly flags thin-signal cases. Don't try to tighten them.
- **Selection bias on returners** (per ROADMAP §5c caveat): top-ranked freshmen who *return* are negatively selected (the Cooper Flagg / Boozer cohort leaves for the draft; the 5-stars who stay are disproportionately those whose freshman year disappointed). The model doesn't see the leave-for-draft cohort, so projections for "5-star high-impact freshman" → year 2 will systematically underestimate the elite ceiling.
- **Transferring returners get destination-agnostic projections.** The arrow direction may be misleading if the player is joining a roster with a very different usage / role profile.
- **Sat-out transfers project from their last played season, not the portal year** (issue #146). The serving helper `fetch_player_trajectory_rows` keys off each player's *own* (season-scoped) source season rather than a fixed year, so a player who skipped a season to preserve eligibility (e.g. Caden Pierce: Princeton 2025 → sat out 2026 → Purdue 2027) still gets a projection — built from his 2025 line. **The model is still strictly one-season-forward**, so this is a 2025 → 2026 projection shown on a 2027 arrival ("his level one year past his last game"). It is **no longer blind to a non-monotonic history**, however: the multi-season history block (above) feeds the 2024 level (CamPom 10.34) and the −10.0 `delta_campom` slope alongside the 2025 dip (0.34), so the model sees the prior peak rather than only the down year. (Verified: the serve-path N-1 join resolves `prior2_campom=10.34, delta_campom=-10.00` for Pierce's 2025 row.)

## Retraining playbook

Run when:

1. A new cstat-season ingests (e.g. cstat-season 2027 lands → 2026→2027 becomes a new training pair).
2. Pre-2021 recruit-class backfill completes (the current model already includes recruit features for classes 2021-2026; older classes would lift recruit coverage for the upperclassmen in our older training pairs — e.g. a senior in the 2022→2023 pair was recruited in ~2018, currently in the `is_ranked=0` sentinel branch).
3. The CamPom v3 formula changes (e.g. CamPom v4) — target shifts, so retrain.

**Do not run this trainer on its own.** It `TRUNCATE`s and repopulates `trajectory_oof_predictions`, which is the training input for *both* roster-frame calibrators, so a bare trajectory retrain leaves everything downstream fit on an OOF snapshot that no longer exists. Use the chain runner, which runs the stages in dependency order and cannot skip one:

```bash
# From repo root:
./training/retrain_downstream.sh --from trajectory --dry-run   # confirm the plan
./training/retrain_downstream.sh --from trajectory
```

`--from trajectory` implies `--with-layer1`, so the plan is the full tree: `trajectory freshman roster_impact roster_adjo backtest cae projections`.

Trajectory-stage outputs:
- `training/models/trajectory_mean_model.onnx`
- `training/models/trajectory_q10_model.onnx`
- `training/models/trajectory_q90_model.onnx`
- `training/models/trajectory_model_meta.json`
- repopulates the `trajectory_oof_predictions` table (held-out LOPO mean + q10/q90)

**Why the rest of the chain is not optional.** `roster_impact` trains on `trajectory_oof_predictions` (train-on-what-you-serve), so a trajectory retrain shifts its inputs — and so does `roster_adjo`, the display-only AdjO half, which shares the same training frame via `build_dataset` but **needs its own invocation**. An earlier version of this playbook named only `roster_impact` here; that omission is the documented root cause of #218, where `roster_adjo` served an OOF three generations stale for months, wrong by ~0.65 AdjO points on average and up to 3.5 for individual teams. The two now stamp their meta with an `oof_provenance` fingerprint and `Predictor::load` refuses to boot when they disagree, so retraining one without the other is a hard failure rather than a silent one — but the guard only compares the two halves against *each other*, and nothing yet catches a Layer 1 retrain with no Layer 2 retrain at all (#223). Run the chain.

Full layer map, the retrain protocol, and what reaches prod by git deploy vs by data sync: `docs/model_dependency_graph.md`.

The boot-time validator in `crates/cstat-core/src/inference.rs::validate_trajectory_meta` will hard-fail at API startup if:
- `player_filter` drifts from `cstat_core::roster_features::QUAL_FILTER_STRING`
- `n_features` ≠ compiled `TRAJECTORY_NUM_FEATURES`
- feature-name order drifts from `TRAJECTORY_FEATURE_NAMES`
- `quantile_alphas` aren't exactly `{q10: 0.1, q90: 0.9}`

When changing the feature set:
1. Update `NUMERIC_FEATURE_COLS` / `ARCH_FEATURE_COLS` in `train_trajectory_model.py`.
2. Mirror the order in `TRAJECTORY_FEATURE_NAMES` (Rust) and update `TRAJECTORY_NUM_FEATURES`.
3. Update `build_trajectory_features` in Rust to populate the new slots.
4. Retrain the chain (`./training/retrain_downstream.sh --from trajectory`) — a feature change reshapes the OOF, so Layer 2 is stale too. Boot will fail loudly if anything's out of sync.

When adding a new pair-fold (new season ingested):
- Update `SEASONS` tuple in `train_trajectory_model.py`.
- No Rust changes needed — pair count doesn't affect feature shape.

## Open questions

- **Walk-forward CV** vs the current LOPO. With 11 pairs (2015→2016 … 2025→2026) we have ample folds to do walk-forward CV: train on `≤ N`, predict `N+1`, advance. Currently the LOPO holds 1 pair out anywhere in the timeline, including pairs that come *after* training rows — walk-forward would tighten the honesty story by predicting only with prior-season data. Implementation lift is small; the existing `leave_one_pair_out` loop just needs an order constraint.
- **Destination-aware projection** for transferring players. Easy feature add: `dest_team_adj_em` (target team AdjEM at season N, since season N+1 doesn't exist yet at projection time). Requires the projection caller to also supply the destination team — fine for the 2027 projection page consumer, but `/api/players/:id` doesn't know the destination, so this would either need a separate endpoint or a "no projection available for transfers" gate.
- **Calibration over time**: once we have OOF predictions from this model on a real future season, plot predicted vs actual binned by predicted-CamPom — the model should be well-calibrated near the mean and progressively over-/under-confident at the tails.
