# Model Performance Report

Last updated: 2026-05-18
Training data: cstat NatStat seasons 2015-2026 ingested as of 2026-05-17 (12 seasons total; 2015-2020 added via the `bootstrap-csv` path). Game-prediction model still on the 5-season window (2022-2026) pending the high-leverage retrain backtest in ROADMAP §6. Trajectory + archetypes already retrained on the full 12-season cohort; freshman model corpus expanded from 1,154 → 3,252 rows after pre-2021 recruit-class ingest (2014-2020).

cstat ships four LightGBM model families, all exported to ONNX and loaded at API startup via the `ort` crate:

| Model | Task | Training rows | Where it's used |
|-------|------|---------------|------------------|
| **Game (margin / win / total)** | Per-game margin, win prob, total points | 20,674 games | `POST /api/predict`, score ticker, schedule projected scores |
| **Trajectory** | Project next-season CamPom v3 for returners | 24,168 N→N+1 player-pairs (11 paired classes 2015→2016 through 2025→2026) | Transfer page (ΔCP), 2027 projection page, PlayerDetail "Proj YYYY-YY" chip |
| **Freshman** | Project freshman-season CamPom v3 for new recruits | 3,252 freshmen (12 recruit classes 2014-2025) | Recruits page (Projection column + Δ247), 2027 projection page recruit cards |
| **Roster** | Project team AdjEM from roster aggregates | 1,799 team-seasons | 2027 projection page team rows, transfer-portal "what-if" |

All four models follow the same rhythm: 5-fold random CV for headline metrics, an out-of-sample backtest (chronological 80/20 for the game model; leave-one-pair-out / leave-one-class-out / leave-one-season-out for trajectory / freshman / roster), and a final fit on all data for the shipped artifacts.

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

**Backtest:** leave-one-pair-out across the 11 transition pairs (2015→2016 through 2025→2026) — for each held-out pair, retrain on the other 10 and score the held-out cohort. Pre-2021 pairs were added 2026-05-17 via the 12-season NatStat backfill; recruit features per row are richer since 2026-05-18 with the pre-2021 247 recruit ingest (LEFT JOIN, doesn't add new pairs).

| Pooled metric | This model | Prior 4-pair model |
|---------------|-----------:|-------------------:|
| LOPO MAE | **2.136** | 2.204 |
| LOPO RMSE | 2.798 | 2.881 |
| LOPO R² | 0.604 | 0.588 |
| Naive baseline (N+1 ≈ N) | 2.339 | 2.392 |
| n | 24,168 | 9,239 |

Per-pair LOPO MAE: 2015→2016: 2.05 / 2016→2017: 2.13 / 2017→2018: 2.09 / 2018→2019: 2.16 / 2019→2020: 2.05 / 2020→2021: 2.10 / 2021→2022: 2.18 / 2022→2023: 2.13 / 2023→2024: 2.12 / 2024→2025: 2.17 / 2025→2026: 2.30. Older seasons fit slightly better than newer ones — likely cohort-stability rather than era effects, since per-feature distributions match closely across the window.

5-fold random CV: MAE 2.134 (gap vs LOPO = +0.002 — no fold-overlap leakage).

**Top features:** `prior_campom`, `prior_usg`, `prior_dgbpm`, `prior_ogbpm`, `prior_gbpm`, `prior_efg`, `prior_mpg`, `prior_ft_rate`. Prior-season CamPom alone carries the most signal; the model's value-add is the regression-to-the-mean correction and class-year × archetype interactions.

**Calibration by current CamPom bucket** (OOF, post-12-season retrain):

| Bucket | n | Mean predicted | Mean actual | Bias |
|--------|---|----------------|-------------|------|
| < −5 | 1,647 | −3.76 | −3.83 | +0.07 |
| −5..0 | 12,378 | −1.34 | −1.35 | +0.01 |
| 0..+5 | 7,612 | +2.43 | +2.43 | −0.01 |
| +5..+10 | 2,139 | +7.13 | +7.18 | −0.04 |
| +10..+15 | 349 | +11.26 | +11.32 | −0.06 |
| +15..+20 | 41 | +14.34 | +15.02 | **−0.68** |
| ≥ +20 | 2 | +17.29 | +26.74 | **−9.45** |

Elite-tier bias improved meaningfully with the 12-season corpus: `+15..+20` went from n=20 / bias −2.12 (pre-retrain) → n=41 / bias **−0.68** (~3× better calibration in this bucket). The `≥+20` bucket still has only n=2 training rows and the model under-predicts by ~10 — genuine extrapolation, not a fixable bias. The trajectory tooltip on PlayerDetail / Transfer / Projection pages surfaces this caveat conditionally.

**Known limitations:**
- Destination-agnostic for transferring returners (no destination-team archetype mix in v1 features).
- Selection bias on returners — Cooper Flagg / Boozer tier leaves for the draft, so the trained corpus skews toward returners who didn't break out.
- Predictions for `current_campom ≥ +20` are effectively extrapolation — only n=1 training row in that bucket.

See `docs/trajectory_methodology.md` for the full methodology.

---

## 3. Freshman model (recruit class N → freshman season N+1)

Three LightGBM regressors (mean + q10 + q90) on 13 features: 11 from the shared recruit-feature extractor (`recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit` — degenerate at 0 for all freshmen but kept for shape parity with the trajectory model) + 2 freshman-specific (`committed_team_prior_adjem`, `peer_class_strength`).

**Corpus:** 3,252 qualified freshmen (≥ 5 GP / ≥ 5 MPG) across recruit classes 2014–2025 (freshman cstat-seasons 2015–2026). Pre-2021 classes added 2026-05-18 via 247 historical backfill — confirmed 247's HTML pages serve clean composite rankings back to at least 2014 (probed; pre-2014 untested).

**Leave-one-class-out CV** (every class held out, retrain on rest, score held-out vs tier-mean baseline):

| Held-out class | n | Model MAE | Baseline MAE | Δ vs baseline |
|----------------|---|-----------|--------------|---------------|
| 2014 → 2015 | 297 | 2.167 | 2.264 | +0.097 |
| 2015 → 2016 | 310 | 2.174 | 2.314 | +0.140 |
| 2016 → 2017 | 281 | 2.322 | 2.413 | +0.092 |
| 2017 → 2018 | 309 | 2.323 | 2.506 | +0.183 |
| 2018 → 2019 | 281 | 2.248 | 2.431 | +0.183 |
| 2019 → 2020 | 326 | 2.164 | 2.249 | +0.085 |
| 2020 → 2021 | 294 | 2.084 | 2.205 | +0.121 |
| 2021 → 2022 | 191 | 2.274 | 2.518 | +0.244 |
| 2022 → 2023 | 219 | 2.276 | 2.449 | +0.173 |
| 2023 → 2024 | 250 | 2.336 | 2.557 | +0.220 |
| 2024 → 2025 | 186 | 2.314 | 2.613 | +0.299 |
| 2025 → 2026 | 308 | 2.457 | 2.706 | +0.250 |
| **Pooled (weighted)** | 3,252 | **2.258** | **2.416** | **+0.158 (6.5%)** |

Pooled baseline above is the n-weighted average of each fold's held-out tier-mean baseline (apples-to-apples with the model's LOCO MAE). Wins on every single held-out class — per-class deltas range from +0.085 (2019, weakest fold) to +0.299 (2024, strongest fold). The 2021 fold quirk from the prior 5-class model (NULL `committed_team_prior_adjem` because cstat-season 2021 wasn't ingested) is gone — 2021 now has full feature parity.

5-fold random CV: MAE 2.255, R² 0.367. Gap vs LOCO: +0.003 — clean, no fold-overlap leakage.

**Top features:** `peer_class_strength` (importance 554), `committed_team_prior_adjem` (536), `recruit_bmi_proxy` (474), `recruit_composite_rank` (392), `recruit_composite_rating` (382), `recruit_rank_movement` (371), `recruit_position_rank` (296). Note `composite_rank` is now above `composite_rating` — the opposite order from the 5-class model. With the wider corpus the model is leaning on the discrete rank ladder more than the continuous rating.

### 3.1 Ablation: how much of the 6.5% lift is "our model" vs "just 247"?

A LOCO-aligned ablation (`training/spike_247_baseline.py`) trains the same LightGBM on progressively smaller feature subsets to isolate the marginal value of each block:

| Variant | Features | Pooled LOCO MAE | Lift |
|---------|----------|-----------------|------|
| Predict cohort mean (floor) | 0 | 2.859 | — |
| 247 composite_rating linear regression (in-sample) | 1 | 2.519 | +0.340 vs floor |
| 247 tier-mean baseline (4 buckets by rank) | 1 binned | 2.416 | +0.103 vs linear |
| **247-only LightGBM** (raw 247 fields) | 10 | **2.349** | **+0.067 vs tier-mean** |
| 247-only + `peer_class_strength` | 11 | 2.306 | +0.043 vs 247-only |
| **Full freshman model** | 12 | **2.257** † | **+0.049 vs +peer** |

† The full-model row in the ablation table is 2.257; the production meta `freshman_model_meta.json` reports 2.258 for the same model. The 0.001 gap is methodology: the production pipeline fits mean + q10 + q90 quantile heads in the same run (slightly different LightGBM internal state ordering), while the spike fits only the mean regressor. Functionally identical predictions; harmonize to 2.258 for cross-doc comparisons.

**Decomposition of the 6.5% headline lift (vs tier-mean):**
- **~2.8% from model architecture alone** — feeding the raw 10 247 fields into LightGBM beats the 4-bucket tier-mean baseline by 0.067 MAE (2.416 → 2.349). Tier-mean throws away per-recruit information; the gradient-boosted model captures the smooth signal in `composite_rank`, `composite_rating`, `rank_movement`, BMI, position-rank.
- **~3.9% from our feature engineering** on top of the 247-only LightGBM (2.349 → 2.257):
  - `peer_class_strength` (mean composite rating across the recruit's committed-team class — 247-derived but novel aggregation): +0.043 MAE.
  - `committed_team_prior_adjem` (committed team's AdjEM in season `r.year` — the only cstat-sourced, non-247-derivable feature): +0.049 MAE.

**Takeaway:** Our value-add over "what 247 already publishes" is roughly half architectural (better functional form on the same data) and half informational (we add two contextual features that 247 doesn't expose per-recruit). The single most marginally valuable feature is `committed_team_prior_adjem` — the team-strength prior matters more than any individual 247 field beyond `composite_rank` itself.

**vs 247 directly** (no LightGBM):
- 247 composite_rating alone (Pearson r vs actual freshman CamPom): **0.50** (R² 0.25)
- Our freshman model (Pearson r): **0.60** (R² 0.36) — ~44% more variance explained

**Known limitations:**
- Selection bias is *sharper* than for the trajectory model — elite freshmen leave for the draft, so the calibrated cohort skews toward returners.
- Sample size below ~30th rank drops fast; bands widen accordingly. Surface with the q10–q90 band, not just the mean.
- The legacy tier-mean heuristic (4-bucket discretisation on composite rank) lives in parallel and is used as the per-tier centroid for `synthesize_freshman_row` reassignment — see `docs/projections_methodology.md` for that path.
- ~30–70 marquee Duke/UNC freshmen across 2018–2020 missing from corpus because of the `players.team_id` Champions Classic stamping bug (see ROADMAP Known Bugs). Fixing this upstream would add ~1–2% more training data; ablation MAE unlikely to move materially.

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
