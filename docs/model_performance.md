# Model Performance Report

Last updated: 2026-05-15
Training data: 2022-2026 seasons (5 seasons across all models). Game-prediction training set is 20,674 games after dropping early-season rows with incomplete features (26,779 raw before filter).

cstat ships four LightGBM model families, all exported to ONNX and loaded at API startup via the `ort` crate:

| Model | Task | Training rows | Where it's used |
|-------|------|---------------|------------------|
| **Game (margin / win / total)** | Per-game margin, win prob, total points | 20,674 games | `POST /api/predict`, score ticker, schedule projected scores |
| **Trajectory** | Project next-season CamPom v3 for returners | 9,239 N→N+1 player-pairs | Transfer page (ΔCP), 2027 projection page, PlayerDetail "Proj YYYY-YY" chip |
| **Freshman** | Project freshman-season CamPom v3 for new recruits | 1,154 freshmen (5 recruit classes 2021-2025) | Recruits page (Projection column + Δ247), 2027 projection page recruit cards |
| **Roster** | Project team AdjEM from roster aggregates | 1,799 team-seasons | 2027 projection page team rows, transfer-portal "what-if" |

All four models share the same training rhythm: 5-fold random CV for headline metrics, leave-one-season-out (or leave-one-pair-out / leave-one-class-out) for honest out-of-sample numbers, and a final fit on all data for the shipped artifacts.

Authoritative per-run details (top features, per-fold breakdowns, known limitations) live in each model's `model_meta.json` alongside the ONNX artifacts.

---

## 1. Game model (margin / win / total)

Three regressors share the same 49-feature point-in-time diff matrix (home minus away). The total model also reads 9 `sum_*` companion features (58 total features) because diffs throw away absolute level.

**Backtest:** chronological 80/20 split — train on 16,539 games (2021-11-21 to 2025-12-03), test on 4,135 games (2025-12-03 to 2026-04-06).

| Target | Test MAE / Acc | Test R² / AUC | 5-fold CV |
|--------|----------------|---------------|-----------|
| Margin (regression) | MAE **8.27 pts** | R² **0.459** · win-acc **74.2%** | Margin MAE 8.27 ± 0.10 |
| Win prob (classification) | Acc **73.7%** · log-loss **0.507** | AUC **0.811** | Win AUC 0.811 ± 0.008 |
| Total (regression) | MAE **13.70 pts** | R² **0.185** | Total MAE 13.36 ± 0.18 |

**Top features (margin):** `diff_w_gbpm` dominates (~3× the next feature), then `diff_w_dgbpm`, `diff_w_ogbpm`, `diff_adj_efficiency_margin`, `venue`, `diff_pythag_win_pct`. Roster-aggregate Barttorvik GBPM is doing more work than any single team-stat.

**Top features (total):** `sum_adj_tempo`, `sum_adj_offense`, `sum_adj_defense` occupy the top 3 — confirming the design intuition that totals are about absolute levels, not differences.

**Known limitations:**
- Total model is materially worse than KenPom (~9 MAE) / Vegas (~7-8). Framed as KenPom-style approximation, not betting-grade.
- No game-specific roster (missing-star teams look identical to full-strength).
- No lineup data.

---

## 2. Trajectory model (returner N → N+1)

Three LightGBM regressors (mean + q10 + q90) trained on a shared 48-feature input. Projects a returning player's next-season CamPom v3 from prior-season stats, archetype, and recruit-rank features.

**Backtest:** leave-one-pair-out across the 4 transition pairs (2022→2023, 2023→2024, 2024→2025, 2025→2026) — for each held-out pair, retrain on the other 3 and score the held-out cohort.

| Backtest | MAE | RMSE | R² | n |
|----------|-----|------|-----|----|
| LOPO 2022→2023 | 2.152 | 2.810 | 0.562 | 2,377 |
| LOPO 2023→2024 | 2.157 | 2.817 | 0.604 | 2,438 |
| LOPO 2024→2025 | 2.210 | 2.897 | 0.600 | 2,311 |
| LOPO 2025→2026 | 2.312 | 3.012 | 0.583 | 2,113 |
| **Pooled** | **2.204** | **2.881** | **0.588** | 9,239 |
| Naive baseline (year N+1 ≈ year N CamPom) | 2.392 | 3.116 | 0.518 | 9,239 |

5-fold random CV: MAE 2.198 ± 0.024.

**Top features:** `prior_campom`, `prior_usg`, `prior_dgbpm`, `prior_ogbpm`, `prior_gbpm`, `prior_efg`, `prior_mpg`, `prior_ft_rate`. Prior-season CamPom alone carries the most signal; the model's value-add is the regression-to-the-mean correction and class-year × archetype interactions.

**Calibration by current CamPom bucket** (OOF):

| Bucket | n | Mean predicted | Mean actual | Bias |
|--------|---|----------------|-------------|------|
| < −5 | 551 | −3.35 | −3.56 | +0.21 |
| −5..0 | 4,811 | −1.31 | −1.35 | +0.03 |
| 0..+5 | 2,942 | +2.34 | +2.38 | −0.04 |
| +5..+10 | 775 | +7.18 | +7.38 | −0.20 |
| +10..+15 | 139 | +11.58 | +11.67 | −0.10 |
| +15..+20 | 20 | +13.67 | +15.79 | **−2.12** |

The model is well-calibrated in `[−5, +15]` and under-projects elite returners (`≥ +15`) by ~2 CamPom — empirical regression-to-the-mean + thin tail support (n=20 in the +15..+20 bucket; n=**1** in +20+, single returner whose +30 actual produced a −13 bias on a +17 prediction). Boozer-tier predictions are essentially extrapolation. The trajectory tooltip on PlayerDetail / Transfer / Projection pages surfaces this caveat conditionally.

**Known limitations:**
- Destination-agnostic for transferring returners (no destination-team archetype mix in v1 features).
- Selection bias on returners — Cooper Flagg / Boozer tier leaves for the draft, so the trained corpus skews toward returners who didn't break out.
- Extrapolation only for `current_campom ≥ +20`.

See `docs/trajectory_methodology.md` for the full methodology.

---

## 3. Freshman model (recruit class N → freshman season N+1)

Three LightGBM regressors (mean + q10 + q90) on 13 features: 11 from the shared recruit-feature extractor (composite rank/rating, star rating, position rank, rank movement, height, weight, BMI proxy, position code, ranked flag) + 2 freshman-specific (`committed_team_prior_adjem`, `peer_class_strength`).

**Corpus:** 1,154 qualified freshmen (≥ 5 GP / ≥ 5 MPG) across recruit classes 2021-2025 (freshman cstat-seasons 2022-2026).

**Leave-one-class-out CV** (every class held out, retrain on rest, score held-out vs tier-mean baseline):

| Held-out class | n | Model MAE | Baseline MAE | Δ vs baseline |
|----------------|---|-----------|--------------|---------------|
| 2021 → 2022 | 191 | **2.273** | 2.474 | +0.201 |
| 2022 → 2023 | 219 | 2.360 | 2.414 | +0.054 |
| 2023 → 2024 | 250 | 2.576 | 2.590 | +0.013 |
| 2024 → 2025 | 186 | 2.475 | 2.588 | +0.113 |
| 2025 → 2026 | 308 | 2.607 | 2.743 | +0.136 |
| **Pooled (weighted)** | 1,154 | **2.477** | **2.578** | **+0.101 (3.9%)** |

Pooled baseline above is the n-weighted average of each fold's held-out tier-mean baseline (apples-to-apples with the model's LOCO MAE). For reference, the in-sample tier-mean heuristic (whole-dataset tier means applied to every row) scores MAE 2.535 — the training script reports `2.477 vs 2.535` (~2.3% delta) which is the in-sample-baseline framing; the table above uses the stricter LOCO-aligned baseline.

5-fold random CV: MAE 2.439, R² 0.364. Gap vs LOCO: +0.038 — no severe overfit, mild fold-overlap leakage.

**Top features:** `peer_class_strength` (#1, importance 449), `committed_team_prior_adjem` (#2, 441), `recruit_bmi_proxy` (#3, 397), then recruit composite rating/rank/position-rank. School-context features dominate the recruit-direct block.

**Notes on the 2021 class:**
- Added 2026-05-15 to lift the corpus from 963 → 1,154 rows.
- The 2021 class has the lowest LOCO MAE — fits cleanly into the learned distribution.
- `committed_team_prior_adjem` is NULL for all 191 rows (we don't have cstat-season 2021 ingested), so those rows are trained on the remaining 12 features. LightGBM handles the NaN natively via a dedicated split direction. Full feature parity for that fold will land if/when 2021 cstat-season is ingested.

**Known limitations:**
- Selection bias is *sharper* than for the trajectory model — elite freshmen leave for the draft, so the calibrated cohort skews toward returners.
- Sample size below ~30th rank drops fast; bands widen accordingly. Surface with the q10–q90 band, not just the mean.
- Bootstrap-from snapshots: see `docs/projections_methodology.md` for the tier-mean baseline and the per-tier centroid reassignment logic in `synthesize_freshman_row`.

---

## 4. Roster model (team AdjEM from roster)

Single LightGBM regressor on 36 features: roster shape (size, total minutes, top-1/top-5 min share, minutes stddev), minutes-weighted player rate stats, star indicators, and one-hot archetype counts.

**Backtest:** leave-one-season-out across 2022-2026.

| Held-out season | n | MAE | RMSE | R² |
|-----------------|---|-----|------|-----|
| 2022 | 350 | 6.30 | 8.09 | 0.688 |
| 2023 | 360 | 5.81 | 7.39 | 0.726 |
| 2024 | 361 | 5.92 | 7.51 | 0.751 |
| 2025 | 364 | 6.38 | 8.00 | 0.754 |
| 2026 | 364 | 7.84 | 9.60 | 0.670 |
| **Pooled** | 1,799 | **6.45** | 8.16 | **0.717** |

5-fold random CV: MAE 6.38, R² 0.731.

**Top features:** `w_stl_pct`, `w_drb_pct`, `w_topg`, `total_minutes`, `w_bpg`, `arch_warlock`, `w_spg`, `w_ts`, `arch_druid`, `arch_ranger`. Defensive event-rate features and archetype dummies do most of the work.

**Notes:**
- 2026 has the largest LOSO MAE (7.84) — partial-season noise (in-flight season vs full-season target).
- Used inside `Predictor::predict_adj_em` for the 2027 projection page; transfer "what-if" infrastructure stays warm but isn't currently surfaced.

---

## Benchmark vs NatStat ELO

Previous benchmark (47-feature model, 2-season training): cstat +2.1pp accuracy, +0.014 AUC, 3× better calibration. A re-benchmark on the current 5-season model is pending; expect cstat's lead to widen given the AUC jump (0.795 → 0.811).

---

## Historical context

For reference, public CBB game-prediction models typically achieve:

| Model | Win accuracy | Notes |
|-------|--------------|-------|
| Home team always wins | ~58% | Naive baseline |
| AP / Coaches poll | ~65% | Higher-ranked team wins |
| Basic ELO | ~67% | Where NatStat sits |
| KenPom / Barttorvik | ~70-72% | Full-season adjusted efficiency |
| **cstat (current)** | **73.7%** | 5 seasons, 49 features (incl. Barttorvik GBPM), point-in-time |
| Vegas closing lines | ~73-74% | Incorporates injury reports + betting market |

cstat is now level with public-tier baselines. Remaining gaps to Vegas are dominated by lineup / injury signal, not feature engineering — see ROADMAP §6 Phase 6 "Full historical data" and §4b "Predict follow-up — point-in-time historical predictions" for the next levers.

---

## How to refresh this doc

After any model retrain, copy the headline numbers from:
- `training/models/model_meta.json` — game models (margin / win / total)
- `training/models/trajectory_model_meta.json` — trajectory
- `training/models/freshman_model_meta.json` — freshman
- `training/models/roster_model_meta.json` — roster

Each `*_meta.json` is the authoritative source; this doc is a curated summary. If you only retrain a subset of models, update only the affected section and bump the "Last updated" stamp.
