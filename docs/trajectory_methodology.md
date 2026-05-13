# Trajectory Model — Phase 5c Growth Projection

## What it is

The trajectory model projects a returning player's **next-season CamPom v3** from their prior-season stats. Three LightGBM regressors trained on the same 37-feature input share one feature shape:

- `trajectory_mean_model.onnx` — regression objective (`mean` prediction)
- `trajectory_q10_model.onnx` — quantile objective at α=0.1 (`lower` band)
- `trajectory_q90_model.onnx` — quantile objective at α=0.9 (`upper` band)

Surfaced today as the "Proj YYYY-YY" badge on PlayerDetail next to the current-season CamPom chip — `mean (lower–upper)`, with an arrow showing direction vs prior year.

## Training data

| Source pair | rows after gate |
|---|---|
| 2024 → 2025 | 2,311 |
| 2025 → 2026 | 2,113 |
| **total** | **4,424** |

Cross-season same-player join is via `torvik_player_stats.torvik_pid` — the stable cross-season key (96% coverage, zero collisions). `players.natstat_id` breaks on transfers (different code per team) so we don't use it for the join.

**Qualification gate** (both seasons in the pair must pass):
- `games_played >= 5`
- `minutes_per_game >= 5`

Same string as `roster_model_meta.json::player_filter` so the Rust path can reuse `cstat_core::roster_features::QUAL_FILTER_STRING`. Boot-time validator (`inference.rs::validate_trajectory_meta`) hard-fails on drift.

## Features (37, order locked)

The order in `trajectory_model_meta.json::features` is wire-locked to `cstat_core::trajectory::TRAJECTORY_FEATURE_NAMES`. A boot-time test confirms the two match exactly.

| group | features | count |
|---|---|---|
| Volume / context | `prior_mpg`, `prior_gp`, `prior_total_min`, `prior_height_in`, `prior_class_year_code` | 5 |
| Box score per-game | `prior_ppg`, `prior_rpg`, `prior_apg`, `prior_spg`, `prior_bpg`, `prior_topg` | 6 |
| Rate stats | `prior_ts`, `prior_efg`, `prior_usg`, `prior_ast_pct`, `prior_tov_pct`, `prior_orb_pct`, `prior_drb_pct`, `prior_stl_pct`, `prior_blk_pct`, `prior_ft_rate` | 10 |
| Impact metrics | `prior_ogbpm`, `prior_dgbpm`, `prior_gbpm`, `prior_campom` | 4 |
| Archetype mixture | `arch_{wizard,sorcerer,warlock,bard,ranger,barbarian,paladin,monk,cleric,druid,rogue,fighter}` — primary 1.0× / secondary 0.5× | 12 |

`prior_class_year_code` encoding: `Fr=0, So=1, Jr=2, Sr=3, Gr=4, NULL/unknown=-1`. NULL gets a separate bucket rather than imputation — LightGBM splits can isolate the unknown cohort, and many real rows have NULL class year (Torvik bio coverage is partial).

Missing rate-stat values are filled with `0.0` at the Rust feature builder (matches `roster_features.rs` convention; for qualified players the gp/mpg gate keeps box stats populated).

## What's NOT in the feature set (and why)

- **Recruit rank** (`composite_rank`, `years_since_recruit`, `is_ranked`) — every row in the 2024–2026 training data has a recruiting class of 2021–2024, and we only have class-of-2026 in the `recruits` table today. So `composite_rank` would be NULL on 100% of training rows. Deferred to a follow-up ablation experiment, gated on the historical recruit ingest (2021–2025 backfill).
- **Destination team for transferring players** — v1 model is destination-agnostic. Cross-team transferring returners (joined via `torvik_pid`) are projected against the same prior as same-team returners; the model has no signal about how a Princeton→Duke transfer's role will change. Wider bands and a `direction` arrow that may surprise are the price; documented limitation.
- **Last-season team identity / strength** — implicitly encoded through the player's own minute/usage/CamPom but no explicit team-strength column. Adding `prior_team_adj_em` is the natural v2 feature.

## Backtest

**Naive baseline** (year N+1 ≈ year N CamPom):
- pooled MAE 2.444, RMSE 3.195, R² 0.522

**Model (mean) — leave-one-pair-out:**
- pair 2024→2025 (test): MAE 2.277, RMSE 2.984, R² 0.576, n=2,311
- pair 2025→2026 (test): MAE 2.354, RMSE 3.073, R² 0.567, n=2,113
- **pooled: MAE 2.314, RMSE 3.027, R² 0.571**

Beats naive by **−0.13 MAE pooled**.

**5-fold random CV:** MAE 2.28, RMSE 2.99, R² 0.58.

**Per–prior-class-year MAE** (model vs naive):

| class_year_code | n | model MAE | naive MAE | Δ |
|---|---|---|---|---|
| 0 (Fr→So) | 1,021 | 1.573 | 2.357 | +0.784 |
| 1 (So→Jr) | 1,211 | 1.642 | 2.508 | +0.865 |
| 2 (Jr→Sr) | 1,506 | 1.542 | 2.428 | +0.886 |
| 3 (Sr→Gr) | 686 | 1.585 | 2.498 | +0.914 |

Model beats naive in every bucket by **0.78–0.91 MAE**. Per-bucket MAE on training data (~1.6) is materially better than the LOPO MAE (~2.3) — the gap is the honest generalization cost across the two pair-folds.

Top features (mean model, by split count):
1. `prior_campom` (703)
2. `prior_usg` (500)
3. `prior_orb_pct` (475)
4. `prior_dgbpm` (458)
5. `prior_gbpm` (453)
6. `prior_ts` (435)
7. `prior_ogbpm` (417)
8. `prior_ft_rate` (411)

Prior CamPom is the dominant signal, which is intuitive — it's the most-aggregated impact metric. The interesting non-obvious result: `prior_usg` is #2, suggesting role-on-team is a meaningful growth differentiator beyond raw impact.

## Honesty framing for UI consumers

- **Pooled LOPO MAE is ~2.3 CamPom points.** Render projections as directional, not point estimates. The 80% band width (q90 − q10) is what users should read for confidence.
- **Bands wider on freshmen and low-minute returners** — the model correctly flags thin-signal cases. Don't try to tighten them.
- **Selection bias on returners** (per ROADMAP §5c caveat): top-ranked freshmen who *return* are negatively selected (the Cooper Flagg / Boozer cohort leaves for the draft; the 5-stars who stay are disproportionately those whose freshman year disappointed). The model doesn't see the leave-for-draft cohort, so projections for "5-star high-impact freshman" → year 2 will systematically underestimate the elite ceiling.
- **Transferring returners get destination-agnostic projections.** The arrow direction may be misleading if the player is joining a roster with a very different usage / role profile.

## Retraining playbook

Run when:

1. A new cstat-season ingests (e.g. cstat-season 2027 lands → 2026→2027 becomes a new training pair).
2. Recruit rank backfill completes (then turn on `composite_rank` / `years_since_recruit` / `is_ranked` as features and re-train; documented as the §5c ablation follow-up).
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

- **Walk-forward CV** vs the current LOPO. With 2 pairs we only have 2 folds; once we have ≥3 pairs (2026→2027 ingested, 2025→2026 + 2024→2025 already present) we can do walk-forward where each season is predicted only from earlier-season pairs.
- **Destination-aware projection** for transferring players. Easy feature add: `dest_team_adj_em` (target team AdjEM at season N, since season N+1 doesn't exist yet at projection time). Requires the projection caller to also supply the destination team — fine for the 2027 projection page consumer, but `/api/players/:id` doesn't know the destination, so this would either need a separate endpoint or a "no projection available for transfers" gate.
- **Calibration over time**: once we have OOF predictions from this model on a real future season, plot predicted vs actual binned by predicted-CamPom — the model should be well-calibrated near the mean and progressively over-/under-confident at the tails.
