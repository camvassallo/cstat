# Model Performance Report

Last updated: 2026-06-05
Training data: cstat NatStat seasons 2015-2026 ingested as of 2026-05-17 (12 seasons total; 2015-2020 added via the `bootstrap-csv` path). All four model families now train on the full 12-season cohort. Game model retrained 2026-05-18 (20,674 → 47,502 games); freshman corpus 1,154 → 3,252 rows after pre-2021 recruit-class ingest (2014-2020); roster corpus 1,799 → 4,248 team-seasons retrained 2026-05-18.

> **2026-06-05 — honest-AUC correction.** The game model's headline accuracy is now reported from the **point-in-time `pit_cam_v3`** retrain, the model the in-season production path actually serves (`/api/predict?as_of_date=…`). The previously-published **5-fold-CV AUC 0.816 / chronological-backtest 0.818** were inflated by **intra-season lookahead** — `torvik_player_stats.cam_gbpm_v3` is a *full-season* aggregate, so a December game's features encode March form for the same teams. The honest figure is **AUC 0.785 / margin MAE 8.69**, triangulated four ways (see §1). cstat still beats NatStat ELO honestly (0.785 vs 0.722). The leaky numbers are struck as headlines below.

cstat ships four LightGBM model families, all exported to ONNX and loaded at API startup via the `ort` crate:

| Model | Task | Training rows | Where it's used |
|-------|------|---------------|------------------|
| **Game (margin / win / total)** | Per-game margin, win prob, total points | 47,502 games (12 seasons 2015-2026) | `POST /api/predict`, score ticker, schedule projected scores |
| **Trajectory** | Project next-season CamPom v3 for returners | 24,168 N→N+1 player-pairs (11 paired classes 2015→2016 through 2025→2026) | Transfer page (ΔCP), 2027 projection page, PlayerDetail "Proj YYYY-YY" chip |
| **Freshman** | Project freshman-season CamPom v3 for new recruits | 3,253 freshmen (12 recruit classes 2014-2025) | Recruits page (Projection column + Δ247), 2027 projection page recruit cards |
| **Roster-impact** (served projection calibrator) | Project team AdjEM from minutes-weighted CamPom-v3 roster aggregates | 4,255 team-seasons (12 seasons 2015-2026), 27 features | 2027 projection page team rows, the served `/api/projections` AdjEM band |

> **Note on the roster family.** The served preseason projection runs the **27-feature roster-impact model** (cam_v3-distribution calibrator) — methodology and *end-to-end* projection accuracy (blended MAE **5.86**, r≈0.88) live in `docs/projections_methodology.md`. The roster-impact model's own standalone LOSO (~3.67 CV MAE) is a *calibrator-fit* metric, not projection accuracy — most of the upstream error lives in the projected cam_v3 inputs, by design. §4 below documents the **legacy 36-feature box-score roster model**, now **deprecated and unloaded** (its last consumer, the freshman statline, was deleted in #108/#109); its numbers are kept for the record. The archetype-based transfer "what-if" (`roster_fit`) replaced the box-score swap-Δ tool.

All four models follow the same rhythm: 5-fold random CV for headline metrics, an out-of-sample backtest (chronological 80/20 for the game model; leave-one-pair-out / leave-one-class-out / leave-one-season-out for trajectory / freshman / roster), and a final fit on all data for the shipped artifacts.

Authoritative per-run details (top features, per-fold breakdowns, known limitations) live in each model's `model_meta.json` alongside the ONNX artifacts.

---

## 1. Game model (margin / win / total)

Three regressors share the same 49-feature point-in-time diff matrix (home minus away). The total model also reads 9 `sum_*` companion features (58 total features) because diffs throw away absolute level.

### Headline (honest, point-in-time)

These are the numbers cstat serves in production for in-season and historical predictions (`/api/predict?as_of_date=…`), where every team's CamPom is recomputed from box scores **up to the game date only** — no end-of-season lookahead. Source: the `pit_cam_v3` retrain on all 12 seasons (n=44,338 games), `training/models_experiments/pit_cam_v3/model_meta.json`.

| Target | MAE / Acc | R² / AUC |
|--------|-----------|----------|
| Margin (regression) | MAE **8.69 pts** | R² **0.382** · win-acc **72.1%** |
| Win prob (classification) | Acc **72.2%** · log-loss **0.531** | AUC **0.785** |
| Total (regression) | MAE **13.49 pts** | R² **0.184** |

**ATS vs Vegas closing lines** (honest, n=11,102 games with both moneylines): **67.7%** against the spread (7,418–3,541, 143 pushes) / **+29.2% ROI** at -110 vig. Internal measurement only — never surfaced on the site.

**Four independent methods triangulate the honest win AUC at ≈0.785** (full detail in `training/eval_history/honest_audit_findings_20260529.md`):

| Method | Honest AUC | Source |
|--------|-----------:|--------|
| **pit_cam_v3 retrain (ground truth)** | **0.785** | full point-in-time refit |
| Linear leak subtraction (`pred − β·leak_diff`) | 0.786 | `honest_predict_via_subtraction.py` |
| March-tournament games (no leak left) | 0.794 | season-progression validation |
| No-leak bucket (\|leak\|<0.3 CamPom) | 0.760 | bucketed segmentation |

### Why the older 0.816 / 0.818 are struck

The previously-published **5-fold-CV AUC 0.816** and **chronological-backtest 0.818** were inflated by **~0.031 AUC of intra-season lookahead**. `torvik_player_stats.cam_gbpm_v3` is a *full-season* aggregate; when a random-fold or chronological split puts a December game in test, the model has already trained on the same teams' March form. The leakage decomposition (`pred_margin ~ pit_diff + leak_diff`) is the smoking gun: the leaky model weights lookahead (`leak_diff` coef 3.46) as heavily as legitimate pre-game info (`pit_diff` coef 3.34); the honest pit retrain cuts the leak coefficient to 1.88 — and most of that residual is `diff_adj_efficiency_margin` legitimately tracking team improvement, not lookahead. The cross-season leak channel is ≈0 (a LOSO retrain came in at 0.815, essentially identical to the leaky 5-fold), so the entire budget was intra-season. **Do not cite 0.816 / 0.818 as headline accuracy.**

> The CamPom constants themselves are **not** overfit — all five `CAMPOM_*` knobs are stable under ±20% perturbation (Pearson r ≥ 0.997 vs baseline). The inflation was a feature-leakage artifact, not tuning overfit. See the audit doc's constants-sensitivity sweep.

**Head-to-head 12 vs 5 seasons** (`training/compare_train_windows.py`, shared chronological-last-20%-of-2026 holdout, n_test=920): the 12-season cohort wins every metric over the 5-season model — margin MAE 8.237 → **8.161** (−0.9%), win accuracy 71.3% → **72.9%** (+1.6pp), win AUC 0.793 → **0.797**, total MAE 13.41 → **13.33**. (These are leaky-feature comparisons; they remain valid as a *relative* corpus-size A/B even though the absolute AUC is inflated.) No distribution-shift damage from the 2015–2017 era; best_iter roughly doubled across all heads (model uses the extra data, not memorizing).

**Top features (margin):** `diff_w_gbpm` dominates (~2.2× the next feature, importance 767), then `diff_w_dgbpm` (341), `diff_w_ogbpm` (264), `diff_opp_effective_fg_pct` (222), `diff_adj_efficiency_margin` (206), `diff_roster_size` (177), `diff_w_player_sos` (171). Roster-aggregate Barttorvik GBPM is doing more work than any single team-stat.

**Top features (total):** `sum_adj_tempo` (568), `sum_adj_offense` (536), `sum_adj_defense` (477) occupy the top 3 — confirming the design intuition that totals are about absolute levels, not differences.

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
- Predictions for `current_campom ≥ +20` are effectively extrapolation — only n=2 training rows in that bucket (up from n=1 pre-retrain).

See `docs/trajectory_methodology.md` for the full methodology.

---

## 3. Freshman model (recruit class N → freshman season N+1)

Three LightGBM regressors (mean + q10 + q90) on 13 features: 11 from the shared recruit-feature extractor (`recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit` — degenerate at 0 for all freshmen but kept for shape parity with the trajectory model) + 2 freshman-specific (`committed_team_prior_adjem`, `peer_class_strength`).

The mean regressor carries a sentinel-safe `monotone_constraints` (non-decreasing in `composite_rating` + `star_rating`, holding other inputs fixed) — added with the freshman-tier deprecation. It moves headline accuracy within noise (LOCO 2.258 → 2.255) but materially suppresses `composite_rating`'s feature importance (382 → 220, now well below `rank_movement`/`position_rank`), since the constraint forbids the non-monotone splits the model previously found there.

**Corpus:** 3,253 qualified freshmen (≥ 5 GP / ≥ 5 MPG) across recruit classes 2014–2025 (freshman cstat-seasons 2015–2026). Pre-2021 classes added 2026-05-18 via 247 historical backfill — confirmed 247's HTML pages serve clean composite rankings back to at least 2014 (probed; pre-2014 untested).

**Leave-one-class-out CV** (every class held out, retrain on rest, score held-out vs tier-mean baseline):

| Held-out class | n | Model MAE | Baseline MAE | Δ vs baseline |
|----------------|---|-----------|--------------|---------------|
| 2014 → 2015 | 297 | 2.173 | 2.264 | +0.092 |
| 2015 → 2016 | 309 | 2.162 | 2.309 | +0.147 |
| 2016 → 2017 | 281 | 2.345 | 2.413 | +0.069 |
| 2017 → 2018 | 309 | 2.311 | 2.506 | +0.195 |
| 2018 → 2019 | 281 | 2.245 | 2.431 | +0.185 |
| 2019 → 2020 | 324 | 2.169 | 2.258 | +0.089 |
| 2020 → 2021 | 294 | 2.075 | 2.205 | +0.130 |
| 2021 → 2022 | 191 | 2.253 | 2.520 | +0.267 |
| 2022 → 2023 | 219 | 2.264 | 2.451 | +0.187 |
| 2023 → 2024 | 250 | 2.343 | 2.557 | +0.214 |
| 2024 → 2025 | 190 | 2.275 | 2.578 | +0.304 |
| 2025 → 2026 | 308 | 2.470 | 2.706 | +0.236 |
| **Pooled (weighted)** | 3,253 | **2.255** | **2.423** | **+0.168 (6.9%)** |

Pooled baseline above is the n-weighted average of each fold's held-out rank-bucket baseline (apples-to-apples with the model's LOCO MAE). Wins on every single held-out class — per-class deltas range from +0.069 (2016, weakest fold) to +0.304 (2024, strongest fold). The 2021 fold quirk from the prior 5-class model (NULL `committed_team_prior_adjem` because cstat-season 2021 wasn't ingested) is gone — 2021 now has full feature parity.

5-fold random CV: MAE 2.254, R² 0.391. Gap vs LOCO: −0.000 — clean, no fold-overlap leakage.

**Top features:** `peer_class_strength` (importance 611), `committed_team_prior_adjem` (562), `recruit_bmi_proxy` (477), `recruit_composite_rank` (450), `recruit_rank_movement` (406), `recruit_position_rank` (323), `recruit_weight_lb` (225), `recruit_composite_rating` (220). `composite_rank` sits well above `composite_rating` — the model leans on the discrete rank ladder, and the monotone constraint (above) further suppressed `composite_rating` by forbidding its non-monotone splits.

### 3.1 Ablation: how much of the 6.5% lift is "our model" vs "just 247"?

> **Caveat:** this ablation was run by `spike_247_baseline.py` on the *pre-monotone* fit and against the full-corpus rank-bucket baseline (2.416), so its 6.5% framing differs slightly from §3's per-fold-weighted 6.9%. The monotone constraint doesn't change the decomposition's shape; re-run the spike if you need the exact monotone-consistent figures.

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
- The per-recruit prediction is the sole freshman signal: `roster_projection.rs::freshman_row` carries the model's projected `cam_v3` straight onto the synthesized PlayerRow. The former 4-tier-mean scaffold was deprecated and deleted — see `docs/projections_methodology.md` for that path.
- ~30–70 marquee Duke/UNC freshmen across 2018–2020 missing from corpus because of the `players.team_id` Champions Classic stamping bug (see ROADMAP Known Bugs). Fixing this upstream would add ~1–2% more training data; ablation MAE unlikely to move materially.

---

## 4. Roster model (team AdjEM from roster) — LEGACY / DEPRECATED

> **⚠️ This section documents the box-score roster model (`roster_model.onnx`), which is DEAD — deprecated and no longer loaded at API boot (#108/#109 deleted its last consumer).** The served preseason projection uses the **27-feature roster-impact model** instead; see `docs/projections_methodology.md` for it and for the *end-to-end* projection accuracy (blended MAE 5.86). The box-score numbers below are retained as a historical record of the 12-season retrain (#79); they are no longer a live model family.

Single LightGBM regressor on 36 features: roster shape (size, total minutes, top-1/top-5 min share, minutes stddev), minutes-weighted player rate stats, star indicators, and one-hot archetype counts.

**Backtest:** leave-one-season-out across 2015-2026 (12 folds).

| Held-out season | n | MAE | RMSE | R² |
|-----------------|---|-----|------|-----|
| 2015 | 351 | 5.47 | 7.16 | 0.777 |
| 2016 | 351 | 4.73 | 6.10 | 0.835 |
| 2017 | 351 | 4.82 | 6.23 | 0.834 |
| 2018 | 351 | 4.91 | 6.23 | 0.823 |
| 2019 | 353 | 5.16 | 6.38 | 0.813 |
| 2020 | 353 | 6.33 | 8.04 | 0.691 |
| 2021 | 339 | 6.90 | 8.81 | 0.663 |
| 2022 | 350 | 5.85 | 7.37 | 0.741 |
| 2023 | 360 | 5.94 | 7.55 | 0.713 |
| 2024 | 361 | 6.07 | 7.62 | 0.744 |
| 2025 | 364 | 6.67 | 8.24 | 0.739 |
| 2026 | 364 | 7.71 | 9.39 | 0.684 |
| **Pooled** | 4,248 | **5.89** | 7.50 | **0.754** |

5-fold random CV: MAE 5.80, R² 0.763. Gap to LOSO: +0.09 — clean, no fold-overlap leakage.

| Pooled metric | This model (12 seasons) | Prior 5-season model |
|---------------|------------------------:|---------------------:|
| LOSO MAE | **5.89** | 6.45 |
| LOSO R² | **0.754** | 0.717 |
| 5-fold CV MAE | **5.80** | 6.38 |
| n | 4,248 | 1,799 |

**Top features:** `w_stl_pct` (importance 330), `w_topg` (287), `total_minutes` (282), `w_ts` (256), `w_drb_pct` (243), `arch_rogue` (240), `arch_wizard` (233), `arch_druid` (220), `w_bpg` (215), `arch_warlock` (200), `w_orb_pct` (193), `w_spg` (175), `arch_sorcerer` (175), `arch_ranger` (175), `w_usg` (171). Defensive event rates and archetype dummies still dominate; archetypes got slightly more prominent with more seasons of stable labels.

**Notes:**
- 2026 has the largest LOSO MAE (7.71, down from 7.84 in the prior 5-season fit) — partial-season noise (in-flight season vs full-season target). The dropping number is the most honest signal that the wider corpus is doing real work.
- Pre-portal seasons (2015-2019) fit notably easier than recent ones (MAE 4.73-5.47 vs 5.85-7.71 for 2022-2026). Less roster volatility before the 2018-19 transfer-portal rule change is the most plausible driver; not investigated further.
- COVID seasons (2020 / 2021) are the hardest folds in the historical window (MAE 6.33 / 6.90) — schedule disruption, eligibility waivers, partial seasons.
- The pooled MAE drop (6.45 → 5.89, −8.7%) is a mix of real model lift on the recent window and easier pre-portal seasons pulling the average down. Apples-to-apples on 2022-2026 only: 2022/2026 better, 2023/2024/2025 slightly worse — roughly neutral on the recent overlap, with the new historical seasons providing the headline gain.
- Used inside `Predictor::predict_adj_em` for the 2027 projection page; transfer "what-if" infrastructure stays warm but isn't currently surfaced.

---

## Benchmark vs NatStat ELO

On the honest point-in-time footing, cstat's win AUC is **0.785** vs NatStat ELO's **0.722** on the same game window — a **+6.3 AUC-point** edge (`game_forecasts`, audit doc). This is smaller than the leaky comparison suggested (which put cstat ~9 points ahead) but it is the real, lookahead-free margin. Don't lead with "cstat beats NatStat by 0.04 AUC" off the old leaky numbers — lead with **0.785 vs 0.722 honest**.

---

## Historical context

For reference, public CBB game-prediction models typically achieve:

| Model | Win accuracy | Notes |
|-------|--------------|-------|
| Home team always wins | ~58% | Naive baseline |
| AP / Coaches poll | ~65% | Higher-ranked team wins |
| Basic ELO | ~67% | Where NatStat sits (0.722 AUC on cstat's window) |
| KenPom / Barttorvik | ~70-72% | Full-season adjusted efficiency |
| **cstat (honest, point-in-time)** | **72.2%** | 12 seasons, 49 features (incl. Barttorvik GBPM), no lookahead — AUC 0.785 |
| Vegas closing lines | ~73-74% | Incorporates injury reports + betting market |

On the honest point-in-time footing cstat sits right at the KenPom/Barttorvik public tier and a touch below Vegas. Remaining gaps to Vegas are dominated by lineup / injury signal, not feature engineering — see ROADMAP §6 Phase 6 "PBP-derived features" (lineup/stint data) for the next lever. (The earlier "74.6%" figure was the leaky full-season-aggregate number — struck; see §1.)

---

## How to refresh this doc

After any model retrain, copy the headline numbers from:
- `training/models/model_meta.json` — game models (margin / win / total)
- `training/models/trajectory_model_meta.json` — trajectory
- `training/models/freshman_model_meta.json` — freshman
- `training/models/roster_model_meta.json` — roster

Each `*_meta.json` is the authoritative source; this doc is a curated summary. If you only retrain a subset of models, update only the affected section and bump the "Last updated" stamp.
