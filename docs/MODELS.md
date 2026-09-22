# Models

**GENERATED — do not edit by hand.** Written by `training/generate_models_doc.py` from `training/models/*_meta.json`; `retrain_downstream.sh` regenerates it at the end of every run and CI fails when it is stale (`generate_models_doc.py --check`). Every number on this page was stamped by a trainer. The prose under **Known limits** and each model's description is the one hand-maintained part, and it lives in the script, next to the renderer.

For the shape of the tree — why Layer 2 trains on Layer 1's predictions, what reaches prod by deploy versus sync, the retrain protocol — read `docs/model_dependency_graph.md`. For what was tried and rejected, `training/experiments/README.md`. To ask which of these is stale against the live database: `cd training && ./.venv/bin/python check_provenance.py`.

## Scorecard

| model | layer | target | rows | features | headline | last retrain |
|---|---|---|---:|---:|---|---|
| [Trajectory model (returner CamPom, season N+1)](#trajectory-model-returner-campom-season-n1) | 1 | `cam_gbpm_v3_psos` | 25,325 | 65 | walk-forward MAE 2.030 | 2026-09-22 |
| [Freshman model (recruit first-season CamPom)](#freshman-model-recruit-first-season-campom) | 1 | `cam_gbpm_v3_psos` | 5,995 | 13 | walk-forward MAE 2.119 | 2026-09-22 |
| [Roster-impact calibrator (team AdjEM, served net)](#roster-impact-calibrator-team-adjem-served-net) | 2 | `adj_efficiency_margin` | 3,625 | 27 | walk-forward served MAE 5.409 (raw 5.487) | 2026-09-22 |
| [Roster-impact AdjO half (display split)](#roster-impact-adjo-half-display-split) | 2 | `adj_offense_relative_to_base_league_mean` | 3,625 | 27 | walk-forward MAE 3.976 | 2026-09-22 |
| [Roster-impact AdjD half (display split)](#roster-impact-adjd-half-display-split) | 2 | `adj_defense_relative_to_base_league_mean` | 3,625 | 27 | walk-forward MAE 3.659 | 2026-09-22 |
| [Game models (margin / win / total)](#game-models-margin--win--total) | game | `home − away margin; P(home win); home + away total` | 47,502 | 49 | margin MAE 8.25, win acc 0.746 | not stamped |
| [Point-in-time game models (pit_margin / pit_win / pit_total)](#point-in-time-game-models-pit_margin--pit_win--pit_total) | game | `home − away margin; P(home win); home + away total` | 44,338 | 49 | margin MAE 8.69, win acc 0.722 | not stamped |
| [Legacy box-score roster model (dead)](#legacy-box-score-roster-model-dead) | legacy | `adj_efficiency_margin` | 4,248 | 36 | LOSO MAE 5.887 | not stamped |

Player-level numbers are CamPom (`cam_gbpm_v3_psos`) MAE; team-level numbers are AdjEM / AdjO MAE; game numbers are points. Walk-forward means trained on seasons strictly earlier than the one scored.

## Not on this page

- **Archetypes** (Layer 0.5). The fit lives in the database (`archetype_models`), not in a meta file; the Rust assign half runs nightly. `docs/archetypes_methodology.md`.
- **Layer 3 derived products** (`team_preseason_projection`, the backtest dumps, `coach_season_cae`). Rows and files, not fits; they record their producing artifact in `artifact_provenance` (migration 047) and `check_provenance.py` compares ONNX digests.
- **Layer 4 serving constants** (`PROJECTION_SHRINK_WEIGHT`, `PRESEASON_PEAK_WEIGHT`, …). Rust `const`s tuned by hand off a diagnostic; the roster-impact section below shows the in-fold refit the chain now runs for three of them. `docs/model_dependency_graph.md` §3.

## Layer 1 — player projection models

These WRITE the OOF tables Layer 2 trains on. Retraining one TRUNCATEs its OOF table and invalidates every Layer 2 model beneath it.

### Trajectory model (returner CamPom, season N+1)

Three LightGBMs (mean, q10, q90) mapping a player's season-N features to his season-N+1 `cam_gbpm_v3_psos`. One row per consecutive-season pair keyed on `torvik_pid`, transfers included. The feature block is prior-season box/rate stats, GBPM components, a two-season history block (lag-2 levels + slope), prior-season on/off, archetype mixture, the recruit-rank block, and a five-feature destination block (where the player plays in N+1). Missing values are sentinels, never NaN, because the ONNX serve path has no NaN plumbing.

- **Trainer:** `training/train_trajectory_model.py`
- **Artifacts:** `trajectory_mean_model.onnx`, `trajectory_q10_model.onnx`, `trajectory_q90_model.onnx`
- **Meta:** `training/models/trajectory_model_meta.json`
- **Served by:** `cstat_core::trajectory` — PlayerDetail projection band, the roster projection's returner and arrival channel, the transfers page
- **Methodology:** `docs/trajectory_methodology.md`
- **Last retrain:** 2026-09-22

#### Target and rows

- **Target:** `cam_gbpm_v3_psos (season N+1)`
- **Join key:** `torvik_pid (cross-season stable)`
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 25,325
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`
- **Band models:** quantiles q10=0.1, q90=0.9
- **Held-out predictions persisted:** yes

#### Inputs

Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.

| source | what it is | rows | digest | nightly-rewritten |
|---|---|---:|---|---|
| `torvik_player_stats.cam_v3` | CamPom — the value currency every model downstream is denominated in. | 58,400 | `d464b895dbd7` | yes |
| `torvik_player_stats.gbpm` | Raw GBPM components; trajectory prior-season features. | 58,400 | `6035752a9903` | yes |
| `player_season_stats` | Trajectory box/rate features, and the gate that decides the row set. | 58,798 | `717fdb394980` | yes |
| `player_archetypes` | ASSIGN half. Class labels only — the trainers read the mixture, not the scores. | 40,814 | `7c6a5d8e3da1` | yes |
| `player_on_off` | Tier-2 membership features (3 of the trajectory model's features). | 46,047 | `c89e3d918f61` | yes |
| `recruits` | 247 recruit ratings; the freshman model's entire feature block. | 12,615 | `c4288159440b` | no |

#### Features

65 features.

Top gain importance (from the fit):

- `prior_campom` (575)
- `dest_program_level` (427)
- `dest_prior_adj_em` (378)
- `prior_usg` (368)
- `delta_campom` (323)
- `prior2_campom` (285)
- `prior_gbpm` (255)
- `prior_drb_pct` (250)

<details><summary>Full feature list (serve-order contract)</summary>

`prior_mpg`, `prior_gp`, `prior_total_min`, `prior_height_in`, `prior_class_year_code`, `prior_ppg`, `prior_rpg`, `prior_apg`, `prior_spg`, `prior_bpg`, `prior_topg`, `prior_ts`, `prior_efg`, `prior_usg`, `prior_ast_pct`, `prior_tov_pct`, `prior_orb_pct`, `prior_drb_pct`, `prior_stl_pct`, `prior_blk_pct`, `prior_ft_rate`, `prior_ogbpm`, `prior_dgbpm`, `prior_gbpm`, `prior_campom`, `prior_on_net_rtg`, `prior_net_on_off`, `prior_on_poss_share`, `prior2_campom`, `prior2_mpg`, `prior2_gp`, `prior2_usg`, `prior2_ppg`, `has_prior2`, `delta_campom`, `delta_mpg`, `delta_usg`, `arch_wizard`, `arch_sorcerer`, `arch_warlock`, `arch_bard`, `arch_ranger`, `arch_barbarian`, `arch_paladin`, `arch_monk`, `arch_cleric`, `arch_druid`, `arch_rogue`, `arch_fighter`, `recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit`, `dest_prior_adj_em`, `dest_program_level`, `src_prior_adj_em`, `dest_minus_src`, `is_transfer`

</details>

- Missing-value sentinels: `onoff_missing_sentinel` = -999.0, `lag2_level_sentinel` = -999.0.
- on/off block coverage: 97.2% of rows.
- lag-2 history coverage: 53.3% of rows.

#### Evaluation

Walk-forward (train on pairs whose target season is strictly earlier than S, test on S, S = `walk_from`..) is the headline; leave-one-pair-out (LOPO) is kept for continuity with older numbers. Both are per-player CamPom MAE. The naive baseline is `cam(N+1) = cam(N)`.

**Walk-forward** (test seasons from 2021; the canonical judge, #361).

| pooled | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| walk-forward | 2.030 | 2.650 | 0.640 | +0.05 | 14,772 |
| LOPO, same rows | 2.026 | 2.641 | 0.642 | +0.04 | 14,772 |
| optimism (walk-forward − same rows) | +0.004 | | | | |

| season | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| 2021 | 1.966 | 2.553 | 0.648 | +0.08 | 2,213 |
| 2022 | 2.044 | 2.671 | 0.601 | +0.30 | 2,671 |
| 2023 | 1.984 | 2.595 | 0.628 | -0.02 | 2,477 |
| 2024 | 1.993 | 2.630 | 0.654 | -0.01 | 2,551 |
| 2025 | 2.065 | 2.691 | 0.655 | -0.00 | 2,541 |
| 2026 | 2.126 | 2.750 | 0.648 | -0.07 | 2,319 |

**Leave-one-pair-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 2.008, RMSE 2.620, R² 0.652.

Naive baseline (`value(N+1) = value(N)`): MAE 2.338, RMSE 3.048, R² 0.529.

5-fold CV (random folds, in-sample seasons): MAE 2.004, RMSE 2.617, R² 0.652.

MAE by prior-season CamPom bucket (LOPO; where the regression toward the mean lives):

| bucket | n | mean prior | mean pred | mean actual | model MAE | model bias | naive MAE |
|---|---:|---:|---:|---:|---:|---:|---:|
| <-5 | 1,756 | -6.38 | -3.78 | -3.85 | 1.795 | +0.07 | 2.989 |
| -5..0 | 13,042 | -2.13 | -1.37 | -1.36 | 1.747 | -0.01 | 2.066 |
| 0..+5 | 7,909 | 1.98 | 2.46 | 2.46 | 2.221 | +0.00 | 2.456 |
| +5..+10 | 2,211 | 6.88 | 7.14 | 7.15 | 2.719 | -0.01 | 2.855 |
| +10..+15 | 363 | 11.66 | 11.38 | 11.29 | 2.917 | +0.09 | 3.043 |
| +15..+20 | 42 | 16.60 | 14.31 | 15.17 | 3.798 | -0.85 | 4.143 |
| >=+20 | 2 | 23.16 | 17.03 | 26.73 | 9.693 | -9.69 | 3.570 |

#### Known limits

- The destination block carries the destination program's strength, not the role the player will fill there: a mid-major star who becomes the go-to option at an elite program (Lendeborg 2026, Knecht 2024, Boyd 2025) is still under-projected. It closes about half the elite-team gap.
- Selection bias on returners: the training rows are players who came back for N+1; the leave-for-the-draft cohort is not modelled.
- Portal-era sign flip: elite-destination returners were over-projected 2021–23 and under-projected 2024+, and an era feature is a tie walk-forward — three seasons of the new regime are not enough to learn it (`training/experiments/experiment_elite_gap.py`).
- The trainer writes its held-out predictions to `trajectory_oof_predictions` (TRUNCATE + reload); a retrain that skipped that write would serve in-sample projections, which is why the boot validator requires `oof_persisted`.

### Freshman model (recruit first-season CamPom)

Per-recruit LightGBM (mean, q10, q90) from the shared 11-feature `recruit_features` block plus two signing-time context features (`committed_team_prior_adjem`, `peer_class_strength`) to the recruit's first-season `cam_gbpm_v3_psos`. The mean model carries monotone constraints on composite rating and star rating so a better-rated recruit never projects lower with everything else fixed.

- **Trainer:** `training/train_freshman_model.py`
- **Artifacts:** `freshman_mean_model.onnx`, `freshman_q10_model.onnx`, `freshman_q90_model.onnx`
- **Meta:** `training/models/freshman_model_meta.json`
- **Served by:** `roster_projection::freshman_row` — the only freshman signal in the roster projection; the recruits page
- **Methodology:** `docs/projections_methodology.md`
- **Last retrain:** 2026-09-22

#### Target and rows

- **Target:** `cam_gbpm_v3_psos (freshman season = recruit.year + 1)`
- **Join key:** `recruits.cstat_player_id → torvik_player_stats.player_id`
- **Training span:** 12 classes, 2014–2025
- **Rows:** 5,995
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`
- **Band models:** quantiles q10=0.1, q90=0.9
- **Held-out predictions persisted:** yes

#### Inputs

Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.

| source | what it is | rows | digest | nightly-rewritten |
|---|---|---:|---|---|
| `torvik_player_stats.cam_v3` | CamPom — the value currency every model downstream is denominated in. | 58,400 | `d464b895dbd7` | yes |
| `player_season_stats` | Trajectory box/rate features, and the gate that decides the row set. | 58,798 | `717fdb394980` | yes |
| `team_season_stats.adj` | Layer 2 targets; also the freshman model's signing-team prior. | 4,268 | `ad4a9438ef44` | yes |
| `recruits` | 247 recruit ratings; the freshman model's entire feature block. | 12,615 | `c4288159440b` | no |

#### Features

13 features.

Top gain importance (from the fit):

- `committed_team_prior_adjem` (745)
- `peer_class_strength` (639)
- `recruit_bmi_proxy` (471)
- `recruit_composite_rank` (339)
- `recruit_rank_movement` (327)
- `recruit_position_rank` (301)
- `recruit_composite_rating` (277)
- `recruit_height_in` (230)

<details><summary>Full feature list (serve-order contract)</summary>

`recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit`, `committed_team_prior_adjem`, `peer_class_strength`

</details>

#### Evaluation

Walk-forward by class (train on classes strictly earlier than the test season) is the headline; leave-one-class-out (LOCO) is kept for continuity. Per-player CamPom MAE. The baseline is the rank-tier mean.

**Walk-forward** (test seasons from 2021; the canonical judge, #361).

| pooled | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| walk-forward | 2.119 | 2.856 | 0.417 | +0.15 | 2,987 |
| LOCO, same rows | 2.089 | 2.819 | 0.432 | +0.12 | 2,987 |
| optimism (walk-forward − same rows) | +0.030 | | | | |

| season | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| 2021 | 2.118 | 2.781 | 0.352 | +0.46 | 514 |
| 2022 | 2.162 | 2.762 | 0.385 | +0.39 | 398 |
| 2023 | 2.046 | 2.744 | 0.380 | +0.42 | 522 |
| 2024 | 2.044 | 2.663 | 0.306 | +0.28 | 556 |
| 2025 | 2.119 | 2.896 | 0.483 | -0.08 | 508 |
| 2026 | 2.249 | 3.265 | 0.475 | -0.56 | 489 |

**Leave-one-class-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 2.149, RMSE 2.883, R² 0.408, n 5,995.

Rank-tier mean baseline (tiers at ranks 30, 100, 250): MAE 2.563, RMSE 3.583, R² 0.086.

5-fold CV (random folds, in-sample seasons): MAE 2.127, RMSE 2.848, R² 0.419.

#### Known limits

- Selection bias at the top: elite freshmen leave for the draft, so the calibrated cohort skews toward those who stayed and played meaningful minutes. Top-30 projections come from a thinner, more variable population than the headline MAE suggests.
- Unrated internationals and generational 5-stars are the 2024+ misses; a destination pre-check found no destination signal to add (the freshman model already reads the signing team's prior AdjEM).
- Sample size below about the 30th-ranked recruit drops fast; surface the q10–q90 band, not the mean alone.

## Layer 2 — team calibrators

Trained on Layer 1's held-out PREDICTIONS, not on actual player value, so they absorb the upstream bias rather than compounding it. The failure mode is desynchronization: a Layer 1 retrain with no Layer 2 retrain. Three halves share one frame — net (served headline), AdjO and AdjD (display, reconciled to the net at serve time).

### Roster-impact calibrator (team AdjEM, served net)

Ordinary least squares on three CAM aggregates of the projected roster (`cam_wmean`, `cam_top3_mean`, `cam_top1`), exported on the 27-slot ONNX contract with zero coefficients on the 24 unused features so the serve path and boot validator are untouched (#363). It trains on the SERVED composition of every historical team-season — returners less departures, plus portal arrivals and ranked recruits, each carrying its Layer 1 held-out projection — cut by `cstat-ingest projections-backtest --frame-out`. The blend on top (program anchor, turnover ramp) is Layer 4 and lives in Rust.

- **Trainer:** `training/train_roster_impact_model.py`
- **Artifacts:** `roster_impact_model.onnx`
- **Meta:** `training/models/roster_impact_model_meta.json`
- **Served by:** `routes/projections.rs` and `cstat-ingest compute-projections` — the Future page's projected AdjEM, the opening-week preseason anchor in `/api/predict`, the denominator of the coach grades
- **Methodology:** `docs/projections_methodology.md`
- **Last retrain:** 2026-09-22

#### Target and rows

- **Target:** `adj_efficiency_margin`
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 3,625
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`

#### Inputs

Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.

| source | what it is | rows | digest | nightly-rewritten |
|---|---|---:|---|---|
| `trajectory_oof_predictions` | Layer 1 held-out returner projections; the roster frame's returner channel. | 25,325 | `73240ca03c1b` | no |
| `freshman_oof_predictions` | Layer 1 held-out recruit projections; the roster frame's newcomer channel. | 5,986 | `9a74e6da0507` | no |
| `torvik_player_stats.cam_v3` | CamPom — the value currency every model downstream is denominated in. | 58,400 | `d464b895dbd7` | yes |
| `player_archetypes` | ASSIGN half. Class labels only — the trainers read the mixture, not the scores. | 40,814 | `7c6a5d8e3da1` | yes |
| `team_season_stats.adj` | Layer 2 targets; also the freshman model's signing-team prior. | 4,268 | `ad4a9438ef44` | yes |
| `recruits` | 247 recruit ratings; the freshman model's entire feature block. | 12,615 | `c4288159440b` | no |
| `transfers` | Portal moves: the arrival and outbound channels of the composed roster. | 7,208 | `ff4df21b998b` | no |
| `draft_entrants` | Firm draft departures (Ceiling scenario) — the composed roster's draft channel. | 545 | `4389876ad4bf` | no |
| `player_departures` | Curated non-portal, non-draft exits. | 1 | `d2c73486cde8` | no |
| `player_returns` | Curated 5-in-5 eligibility returns (granted stays, contested widens the band). | 236 | `392799f17c28` | no |

OOF snapshot the frame was cut from (the #218 boot stamp; all Layer 2 halves must agree):

- `trajectory_oof_predictions`: 25,325 rows, `73240ca03c1b`
- `freshman_oof_predictions`: 5,986 rows, `9a74e6da0507`

Training frame: `training/frames/roster_impact_ex_ante.json` (sha256 `3474323d51c6…`, 3,625 team-seasons, produced by `cstat-ingest projections-backtest --frame-out`). Composition: ex-ante: returners - departures + portal arrivals + recruits, Ceiling draft scenario, OOF cam_v3.

#### Features

27 features.

Linear: only 3 carry weight; the other 24 slots are exported with zero coefficients to keep the ONNX contract.

| term | coefficient |
|---|---:|
| intercept | -1.4915 |
| `cam_wmean` | 6.1473 |
| `cam_top3_mean` | -1.0880 |
| `cam_top1` | -0.1443 |

<details><summary>Full feature list (serve-order contract)</summary>

`roster_size`, `cam_wmean`, `cam_sum`, `cam_top1`, `cam_top3_mean`, `cam_top7_mean`, `cam_count_gt5`, `cam_count_gt10`, `cam_count_gt15`, `exp_fr_share`, `exp_so_share`, `exp_jr_share`, `exp_sr_share`, `arch_wizard`, `arch_sorcerer`, `arch_warlock`, `arch_bard`, `arch_ranger`, `arch_barbarian`, `arch_paladin`, `arch_monk`, `arch_cleric`, `arch_druid`, `arch_rogue`, `arch_fighter`, `outbound_cam_v3_sum`, `inbound_cam_v3_sum`

</details>

#### Evaluation

Walk-forward (train < S, test S) on the raw calibrator and through the served blend; leave-one-season-out on the same rows for the optimism column. Team AdjEM MAE. The per-target-season LOSO models are also exported (gitignored) so `projections-backtest` scores honestly.

**Walk-forward** (test seasons from 2021; the canonical judge, #361).
Protocol: train < S, test S; fixed n_estimators (export protocol); served blend on top; constants refit on earlier walk-forward rows.

Raw calibrator (before the blend), walk-forward from 2019:

| pooled | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| walk-forward raw | 5.433 | 6.869 | 0.794 | +0.02 | 2,626 |
| LOSO raw, same rows (2021+) | 5.489 | 6.952 | 0.794 | +0.08 | 1,961 |

| season | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| 2019 | 5.546 | 6.817 | 0.789 | +0.34 | 327 |
| 2020 | 5.010 | 6.423 | 0.806 | -0.11 | 338 |
| 2021 | 5.614 | 7.151 | 0.774 | +1.33 | 334 |
| 2022 | 5.424 | 6.749 | 0.783 | -0.13 | 326 |
| 2023 | 5.365 | 6.850 | 0.769 | -0.11 | 328 |
| 2024 | 5.526 | 7.116 | 0.779 | +0.07 | 337 |
| 2025 | 5.293 | 6.624 | 0.829 | -0.54 | 324 |
| 2026 | 5.703 | 7.199 | 0.815 | -0.78 | 312 |

**Served projection** (raw + the Layer 4 blend), test 2021+, n=1,961. Cell: MAE / bias. `wf served` is the number the site is judged on.

| cohort | n | loso served | wf raw | wf served | wf refit |
|---|---:|---:|---:|---:|---:|
| all | 1,961 | 5.408 / +0.05 | 5.487 / -0.02 | 5.409 / -0.01 | 5.411 / -0.09 |
| era>=2024 | 973 | 5.426 / -0.27 | 5.505 / -0.40 | 5.436 / -0.32 | 5.435 / -0.41 |
| top50 (ex-ante) | 300 | 4.919 / +0.31 | 5.078 / +0.02 | 4.887 / +0.08 | 4.899 / -0.07 |
| top25 (ex-ante) | 150 | 5.269 / +0.04 | 5.478 / -0.32 | 5.230 / -0.21 | 5.236 / -0.33 |
| top10 (ex-ante) | 60 | 5.651 / -0.47 | 5.643 / -1.17 | 5.573 / -0.78 | 5.588 / -0.89 |
| top10 era>=2024 | 30 | 5.679 / -3.87 | 5.883 / -4.48 | 5.734 / -4.08 | 5.761 / -4.24 |
| overhaul (<0.2) | 197 | 5.575 / -0.26 | 5.457 / -1.22 | 5.559 / -0.37 | 5.430 / -0.91 |
| level-changers \|dev\|>=15 | 100 | 5.215 / +0.27 | 5.367 / +0.45 | 5.221 / +0.20 | 5.244 / +0.16 |

| ordering metric (per-season mean) | loso served | wf raw | wf served | wf refit |
|---|---:|---:|---:|---:|
| membership@25 | 0.720 | 0.693 | 0.720 | 0.713 |
| rho(actual T25) | 0.523 | 0.473 | 0.527 | 0.507 |
| rho(pred T25) | 0.468 | 0.485 | 0.477 | 0.475 |
| concordance T50 | 0.733 | 0.728 | 0.734 | 0.734 |
| worst miss T10 | 11.188 | 12.651 | 11.082 | 10.353 |
| rho(field) | 0.883 | 0.883 | 0.883 | 0.884 |

Paired |error| against `wf served` (delta, z; negative = the variant is better):

| variant | pooled | top-25 | top-10 | level-changers |
|---|---:|---:|---:|---:|
| loso served | -0.002 (z -0.5) | +0.038 (z +1.5) | +0.078 (z +1.6) | -0.006 (z -0.3) |
| wf raw | +0.078 (z +2.1) | +0.247 (z +1.8) | +0.070 (z +0.4) | +0.145 (z +0.9) |
| wf refit | +0.002 (z +0.2) | +0.005 (z +0.1) | +0.015 (z +0.2) | +0.023 (z +0.5) |

Layer 4 constants re-searched inside each fold on earlier walk-forward rows, against the served `w_stable` 0.70 / `w_overhaul` 0.55 / `shrink` 1.00. A refit that beats the served values out of sample is the signal to change them; one that merely differs is not.

| test season | refit w_stable | refit w_overhaul | refit shrink | test MAE refit | test MAE served |
|---|---:|---:|---:|---:|---:|
| 2021 | 0.50 | 0.20 | 1.00 | 5.506 | 5.521 |
| 2022 | 0.50 | 0.20 | 0.75 | 5.346 | 5.287 |
| 2023 | 0.65 | 0.20 | 1.00 | 5.311 | 5.337 |
| 2024 | 0.60 | 0.20 | 1.00 | 5.459 | 5.465 |
| 2025 | 0.60 | 0.20 | 1.00 | 5.253 | 5.277 |
| 2026 | 0.55 | 0.20 | 1.00 | 5.598 | 5.570 |

**Leave-one-season-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 5.412, RMSE 6.837, R² 0.796.

5-fold CV (random folds, in-sample seasons): MAE 5.412, RMSE 6.831, R² 0.795.

#### Known limits

- The remaining elite gap is upstream: ex-ante top-10 programs in 2024+ are still under-projected by about 4 AdjEM, and it decomposes to −0.4 per player at elite destinations plus a handful of generational recruits (`training/experiments/experiment_elite_gap.py`). Nothing on the calibrator moves it walk-forward.
- The frame is a file (`training/frames/roster_impact_ex_ante.json`, gitignored). A Layer 1 retrain that skips the `frame` stage trains the calibrators on stale projections; the trainer compares the frame's OOF snapshot to the live tables and refuses, but only if it is run.
- All three Layer 2 halves must carry the same `oof_provenance` stamp or the API refuses to boot (#218). Retraining this without `roster_adjo` and `roster_adjd` is a hard failure, not a silent one.
- The served Layer 4 constants (`PROJECTION_SHRINK_WEIGHT`, `_OVERHAUL`, `PROGRAM_ANCHOR_SHRINK`) are re-searched inside each walk-forward fold and stamped as `walk_forward.constants_refit`; the three `predict.rs` blend constants are not (#236).

### Roster-impact AdjO half (display split)

Same 27-feature frame as the net calibrator, target = next-season `adj_offense` RELATIVE to the base season's league mean (#368); the serve path adds the base season's mean back. NET + O + D: this half and the AdjD half are each nudged by half the net residual at serve time so `AdjO − AdjD` equals the served net exactly; the net headline is never touched by either. Display-only.

- **Trainer:** `training/train_roster_adjo_model.py`
- **Artifacts:** `roster_adjo_model.onnx`
- **Meta:** `training/models/roster_adjo_model_meta.json`
- **Served by:** `routes/projections.rs` — projected AdjO on the Future page, run live per request and reconciled with the AdjD half to the net (#378)
- **Methodology:** `docs/projections_methodology.md`
- **Last retrain:** 2026-09-22

#### Target and rows

- **Target:** `adj_offense_relative_to_base_league_mean`; add-back at serve: league mean adj_offense of the base season (team_season_stats, every team with an AdjO)
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 3,625
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`
- **Decomposition:** NET+O+D reconciled: O' = O + r/2, D' = D - r/2, r = net - (O - D) at serve time (#378)

#### Inputs

Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.

| source | what it is | rows | digest | nightly-rewritten |
|---|---|---:|---|---|
| `trajectory_oof_predictions` | Layer 1 held-out returner projections; the roster frame's returner channel. | 25,325 | `73240ca03c1b` | no |
| `freshman_oof_predictions` | Layer 1 held-out recruit projections; the roster frame's newcomer channel. | 5,986 | `9a74e6da0507` | no |
| `torvik_player_stats.cam_v3` | CamPom — the value currency every model downstream is denominated in. | 58,400 | `d464b895dbd7` | yes |
| `player_archetypes` | ASSIGN half. Class labels only — the trainers read the mixture, not the scores. | 40,814 | `7c6a5d8e3da1` | yes |
| `team_season_stats.adj` | Layer 2 targets; also the freshman model's signing-team prior. | 4,268 | `ad4a9438ef44` | yes |
| `recruits` | 247 recruit ratings; the freshman model's entire feature block. | 12,615 | `c4288159440b` | no |
| `transfers` | Portal moves: the arrival and outbound channels of the composed roster. | 7,208 | `ff4df21b998b` | no |
| `draft_entrants` | Firm draft departures (Ceiling scenario) — the composed roster's draft channel. | 545 | `4389876ad4bf` | no |
| `player_departures` | Curated non-portal, non-draft exits. | 1 | `d2c73486cde8` | no |
| `player_returns` | Curated 5-in-5 eligibility returns (granted stays, contested widens the band). | 236 | `392799f17c28` | no |

OOF snapshot the frame was cut from (the #218 boot stamp; all Layer 2 halves must agree):

- `trajectory_oof_predictions`: 25,325 rows, `73240ca03c1b`
- `freshman_oof_predictions`: 5,986 rows, `9a74e6da0507`

Training frame: `training/frames/roster_impact_ex_ante.json` (sha256 `3474323d51c6…`, 3,625 team-seasons, produced by `cstat-ingest projections-backtest --frame-out`). Composition: ex-ante: returners - departures + portal arrivals + recruits, Ceiling draft scenario, OOF cam_v3.

#### Features

27 features.

<details><summary>Full feature list (serve-order contract)</summary>

`roster_size`, `cam_wmean`, `cam_sum`, `cam_top1`, `cam_top3_mean`, `cam_top7_mean`, `cam_count_gt5`, `cam_count_gt10`, `cam_count_gt15`, `exp_fr_share`, `exp_so_share`, `exp_jr_share`, `exp_sr_share`, `arch_wizard`, `arch_sorcerer`, `arch_warlock`, `arch_bard`, `arch_ranger`, `arch_barbarian`, `arch_paladin`, `arch_monk`, `arch_cleric`, `arch_druid`, `arch_rogue`, `arch_fighter`, `outbound_cam_v3_sum`, `inbound_cam_v3_sum`

</details>

#### Evaluation

Walk-forward (train < S, test S) through the served blend for the AdjO half; leave-one-season-out kept for continuity. Team AdjO MAE; the naive baseline is last season's AdjO.

**Walk-forward** (test seasons from 2021; the canonical judge, #361).

| pooled | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| walk-forward | 3.976 | 4.966 | 0.682 | -1.20 | 1,961 |

| season | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| 2021 | 3.850 | 4.854 | 0.688 | -0.06 | 334 |
| 2022 | 4.035 | 4.932 | 0.633 | -1.67 | 326 |
| 2023 | 4.020 | 4.896 | 0.615 | -1.45 | 328 |
| 2024 | 4.008 | 5.101 | 0.665 | -1.52 | 337 |
| 2025 | 3.739 | 4.707 | 0.738 | -0.77 | 324 |
| 2026 | 4.214 | 5.294 | 0.704 | -1.80 | 312 |

**Leave-one-season-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 3.897, naive (last season) MAE 6.981.

#### Known limits

- Reaches prod by git deploy only — `team_preseason_projection` has no AdjO column, so no data sync can move it. That is how a stale copy survived months of routine syncs (#218).
- Needs its own invocation. It imports `build_dataset` from the net trainer, which reads as 'the AdjO half updates itself'; it does not.
- Exports no per-season LOSO ONNX, so running it alone leaves `projections-backtest` reading the previous net models.

### Roster-impact AdjD half (display split)

Mirror of the AdjO half with the target swapped: next-season `adj_defense` RELATIVE to the base season's league mean (lower is better), anchored on the program's own defensive baseline and 3-year level. Replaces deriving AdjD as `AdjO − AdjEM`, which gave the model no defensive information at all and handed the whole program premium to whichever half history said. Display-only.

- **Trainer:** `training/train_roster_adjd_model.py`
- **Artifacts:** `roster_adjd_model.onnx`
- **Meta:** `training/models/roster_adjd_model_meta.json`
- **Served by:** `routes/projections.rs` — projected AdjD on the Future page, run live per request and reconciled with the AdjO half to the net (#378)
- **Methodology:** `docs/projections_methodology.md`
- **Last retrain:** 2026-09-22

#### Target and rows

- **Target:** `adj_defense_relative_to_base_league_mean`; add-back at serve: league mean adj_defense of the base season (team_season_stats, every team with an AdjD)
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 3,625
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`
- **Decomposition:** NET+O+D reconciled: O' = O + r/2, D' = D - r/2, r = net - (O - D) at serve time (#378)

#### Inputs

Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.

| source | what it is | rows | digest | nightly-rewritten |
|---|---|---:|---|---|
| `trajectory_oof_predictions` | Layer 1 held-out returner projections; the roster frame's returner channel. | 25,325 | `73240ca03c1b` | no |
| `freshman_oof_predictions` | Layer 1 held-out recruit projections; the roster frame's newcomer channel. | 5,986 | `9a74e6da0507` | no |
| `torvik_player_stats.cam_v3` | CamPom — the value currency every model downstream is denominated in. | 58,400 | `d464b895dbd7` | yes |
| `player_archetypes` | ASSIGN half. Class labels only — the trainers read the mixture, not the scores. | 40,814 | `7c6a5d8e3da1` | yes |
| `team_season_stats.adj` | Layer 2 targets; also the freshman model's signing-team prior. | 4,268 | `ad4a9438ef44` | yes |
| `team_season_stats.adj_d` | The AdjD half's target. | 4,268 | `c6f02a08b655` | yes |
| `recruits` | 247 recruit ratings; the freshman model's entire feature block. | 12,615 | `c4288159440b` | no |
| `transfers` | Portal moves: the arrival and outbound channels of the composed roster. | 7,208 | `ff4df21b998b` | no |
| `draft_entrants` | Firm draft departures (Ceiling scenario) — the composed roster's draft channel. | 545 | `4389876ad4bf` | no |
| `player_departures` | Curated non-portal, non-draft exits. | 1 | `d2c73486cde8` | no |
| `player_returns` | Curated 5-in-5 eligibility returns (granted stays, contested widens the band). | 236 | `392799f17c28` | no |

OOF snapshot the frame was cut from (the #218 boot stamp; all Layer 2 halves must agree):

- `trajectory_oof_predictions`: 25,325 rows, `73240ca03c1b`
- `freshman_oof_predictions`: 5,986 rows, `9a74e6da0507`

Training frame: `training/frames/roster_impact_ex_ante.json` (sha256 `3474323d51c6…`, 3,625 team-seasons, produced by `cstat-ingest projections-backtest --frame-out`). Composition: ex-ante: returners - departures + portal arrivals + recruits, Ceiling draft scenario, OOF cam_v3.

#### Features

27 features.

<details><summary>Full feature list (serve-order contract)</summary>

`roster_size`, `cam_wmean`, `cam_sum`, `cam_top1`, `cam_top3_mean`, `cam_top7_mean`, `cam_count_gt5`, `cam_count_gt10`, `cam_count_gt15`, `exp_fr_share`, `exp_so_share`, `exp_jr_share`, `exp_sr_share`, `arch_wizard`, `arch_sorcerer`, `arch_warlock`, `arch_bard`, `arch_ranger`, `arch_barbarian`, `arch_paladin`, `arch_monk`, `arch_cleric`, `arch_druid`, `arch_rogue`, `arch_fighter`, `outbound_cam_v3_sum`, `inbound_cam_v3_sum`

</details>

#### Evaluation

Walk-forward (train < S, test S) on the raw relative target; leave-one-season-out kept for continuity. Team AdjD MAE; the naive baseline is last season's AdjD. The served-blend, cohort-level judgement is `training/experiments/experiment_od_anchor.py`.

**Walk-forward** (test seasons from 2021; the canonical judge, #361).

| pooled | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| walk-forward | 3.659 | 4.606 | 0.644 | -0.96 | 1,961 |

| season | MAE | RMSE | R² | bias | n |
|---|---:|---:|---:|---:|---:|
| 2021 | 4.080 | 5.065 | 0.558 | -2.08 | 334 |
| 2022 | 3.586 | 4.496 | 0.645 | -0.01 | 326 |
| 2023 | 3.636 | 4.578 | 0.630 | -1.13 | 328 |
| 2024 | 3.683 | 4.612 | 0.611 | -1.79 | 337 |
| 2025 | 3.268 | 4.187 | 0.719 | +0.17 | 324 |
| 2026 | 3.691 | 4.642 | 0.676 | -0.82 | 312 |

**Leave-one-season-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 3.637, naive (last season) MAE 6.400.

#### Known limits

- Reaches prod by git deploy only, like the AdjO half; no data sync moves it.
- Needs its own invocation — a third half to forget. All three Layer 2 stamps must agree or the API refuses to boot.
- Both halves still run ~1.2 low walk-forward (net unaffected): the relative target removes the level of the scoring-environment drift, not its slope.

## Game-outcome branch

Hangs off Layer 0 directly — no edge into the roster tree. A roster-tree retrain never requires retraining these.

### Game models (margin / win / total)

LightGBM margin regression, win-probability classifier and total-points regression on the 49 `diff_*` features `features.py` builds from team stats, roster aggregates (season-aggregate CamPom), rolling form and context; the total model adds `sum_*` level companions. The `.lgb` mirror of the margin model is kept for TreeSHAP explanations.

- **Trainer:** `training/train.py` + `training/export_onnx.py`
- **Artifacts:** `margin_model.onnx`, `win_model.onnx`, `total_model.onnx`, `margin_model.lgb`
- **Meta:** `training/models/model_meta.json`
- **Served by:** `cstat_core::inference::Predictor` — `/api/predict` without `as_of_date`, the leaky whole-season path
- **Methodology:** `docs/model_performance.md`
- **Last retrain:** not stamped — predates the `trained_at` stamp (#364); the next retrain records it

#### Target and rows

- **Target:** `home − away margin; P(home win); home + away total`
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 47,502 games

#### Inputs

No `input_provenance` stamp — this model is outside the fingerprint chain (`check_provenance.py` cannot say whether it is stale).

#### Features

49 features.

<details><summary>Full feature list (serve-order contract)</summary>

`venue`, `is_conference_game`, `diff_win_pct`, `diff_adj_offense`, `diff_adj_defense`, `diff_adj_efficiency_margin`, `diff_effective_fg_pct`, `diff_turnover_pct`, `diff_off_rebound_pct`, `diff_ft_rate`, `diff_opp_effective_fg_pct`, `diff_opp_turnover_pct`, `diff_def_rebound_pct`, `diff_opp_ft_rate`, `diff_adj_tempo`, `diff_sos`, `diff_elo`, `diff_point_diff`, `diff_pythag_win_pct`, `diff_road_win_pct`, `diff_roster_size`, `diff_w_ppg`, `diff_w_rpg`, `diff_w_apg`, `diff_w_spg`, `diff_w_bpg`, `diff_w_topg`, `diff_w_ts_pct`, `diff_w_efg_pct`, `diff_w_usage`, `diff_w_player_sos`, `diff_w_ortg`, `diff_w_ast_pct`, `diff_w_tov_pct`, `diff_w_stl_pct`, `diff_w_blk_pct`, `diff_w_gbpm`, `diff_w_ogbpm`, `diff_w_dgbpm`, `diff_star_ppg`, `diff_star_gbpm`, `diff_star_ogbpm`, `diff_star_dgbpm`, `diff_star_ortg`, `diff_minutes_stddev`, `diff_w_rolling_gs`, `diff_w_rolling_ts`, `diff_w_ppg_trend`, `diff_w_gs_trend`

</details>

The total model adds level companions: 58 features in all.

#### Evaluation

Chronological backtest: train on the first 80% of the corpus by date, score the last 20%. Margin/total in points; win in accuracy, log loss, AUC.

Chronological holdout, 47,502 games, CamPom variant `raw`:

| model | MAE | RMSE | R² | accuracy | log loss | AUC |
|---|---:|---:|---:|---:|---:|---:|
| margin | 8.25 | 10.46 | 0.459 | 0.748 | | |
| win | | | | 0.746 | 0.498 | 0.818 |
| total | 13.56 | 17.28 | 0.179 | | | |

#### Known limits

- Not in the retrain chain and not fingerprinted: no `input_provenance`, no `trained_at`, and `train.py` was not part of the #222 reproducibility work, so rerunning it on unchanged data produces different bytes. Do not retrain it 'to be safe'.
- Trains on season-aggregate CamPom, which leaks the season being predicted; the `pit_*` twins below are the honest in-season path.
- The only boot gate is the feature-vector width (`FeatureCountMismatch` on the `.lgb`); there is no meta-drift validator as thorough as the roster branch's.

### Point-in-time game models (pit_margin / pit_win / pit_total)

The same three models trained with the CamPom channel swapped for a point-in-time grid (`build_pit_lookup.py`, asof-merged), so an in-season prediction reads only what was known on the date.

- **Trainer:** `training/train.py` with `GBPM_VARIANT=pit_cam_v3` (into `models_experiments/pit_cam_v3/`, then copied here with the `pit_` prefix)
- **Artifacts:** `pit_margin_model.onnx`, `pit_win_model.onnx`, `pit_total_model.onnx`, `pit_margin_model.lgb`
- **Meta:** `training/models/pit_model_meta.json`
- **Served by:** `Predictor` — `/api/predict?as_of_date=`, the preseason×pit blend over opening week, `game_projections` and the TeamDetail projected column
- **Methodology:** `docs/model_performance.md`
- **Last retrain:** not stamped — predates the `trained_at` stamp (#364); the next retrain records it

#### Target and rows

- **Target:** `home − away margin; P(home win); home + away total`
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 44,338 games

#### Inputs

No `input_provenance` stamp — this model is outside the fingerprint chain (`check_provenance.py` cannot say whether it is stale).

#### Features

49 features.

<details><summary>Full feature list (serve-order contract)</summary>

`venue`, `is_conference_game`, `diff_win_pct`, `diff_adj_offense`, `diff_adj_defense`, `diff_adj_efficiency_margin`, `diff_effective_fg_pct`, `diff_turnover_pct`, `diff_off_rebound_pct`, `diff_ft_rate`, `diff_opp_effective_fg_pct`, `diff_opp_turnover_pct`, `diff_def_rebound_pct`, `diff_opp_ft_rate`, `diff_adj_tempo`, `diff_sos`, `diff_elo`, `diff_point_diff`, `diff_pythag_win_pct`, `diff_road_win_pct`, `diff_roster_size`, `diff_w_ppg`, `diff_w_rpg`, `diff_w_apg`, `diff_w_spg`, `diff_w_bpg`, `diff_w_topg`, `diff_w_ts_pct`, `diff_w_efg_pct`, `diff_w_usage`, `diff_w_player_sos`, `diff_w_ortg`, `diff_w_ast_pct`, `diff_w_tov_pct`, `diff_w_stl_pct`, `diff_w_blk_pct`, `diff_w_gbpm`, `diff_w_ogbpm`, `diff_w_dgbpm`, `diff_star_ppg`, `diff_star_gbpm`, `diff_star_ogbpm`, `diff_star_dgbpm`, `diff_star_ortg`, `diff_minutes_stddev`, `diff_w_rolling_gs`, `diff_w_rolling_ts`, `diff_w_ppg_trend`, `diff_w_gs_trend`

</details>

The total model adds level companions: 58 features in all.

#### Evaluation

Same chronological 80/20 backtest as the whole-season models, on the pit feature matrix.

Chronological holdout, 44,338 games, CamPom variant `pit_cam_v3`:

| model | MAE | RMSE | R² | accuracy | log loss | AUC |
|---|---:|---:|---:|---:|---:|---:|
| margin | 8.69 | 11.03 | 0.382 | 0.721 | | |
| win | | | | 0.722 | 0.531 | 0.785 |
| total | 13.49 | 17.19 | 0.184 | | | |

#### Known limits

- Only the CamPom channel is point-in-time; the other 41 features are season aggregates, a train/serve skew tracked in #274.
- Promoted by copying files out of `models_experiments/pit_cam_v3/` with a `pit_` prefix — there is no trainer flag that writes these names, so a retrain is a manual step.
- The point-in-time map is flattened into the SQL `UNNEST` in sorted `player_id` order and the sort is load-bearing: `HashMap` iteration order moved the last bits of the minutes-weighted aggregates and flipped splits (#266).

## Legacy

### Legacy box-score roster model (dead)

LightGBM from 36 minutes-weighted box-score aggregates to team AdjEM, with CamPom deliberately excluded (Σ cam × minute share ≈ AdjEM would collapse it to the identity). Its consumer, the freshman statline, was deprecated; removal is tracked in the ROADMAP Refactor Backlog.

- **Trainer:** `training/train_roster_model.py`
- **Artifacts:** `roster_model.onnx`
- **Meta:** `training/models/roster_model_meta.json`
- **Served by:** nothing — deliberately NOT loaded at boot; materialized lazily only by `projections-backtest`'s box-score comparison, which was dropped
- **Methodology:** `docs/projections_methodology.md`
- **Last retrain:** not stamped — predates the `trained_at` stamp (#364); the next retrain records it

#### Target and rows

- **Target:** `adj_efficiency_margin`
- **Training span:** 12 seasons, 2015–2026
- **Rows:** 4,248
- **Qualification gate:** `games_played >= 5 AND minutes_per_game >= 5`

#### Inputs

No `input_provenance` stamp — this model is outside the fingerprint chain (`check_provenance.py` cannot say whether it is stale).

#### Features

36 features.

Top gain importance (from the fit):

- `w_stl_pct` (330)
- `w_topg` (287)
- `total_minutes` (282)
- `w_ts` (256)
- `w_drb_pct` (243)
- `arch_rogue` (240)
- `arch_wizard` (233)
- `arch_druid` (220)

<details><summary>Full feature list (serve-order contract)</summary>

`roster_size`, `total_minutes`, `top1_min_share`, `top5_min_share`, `minutes_stddev`, `w_ppg`, `w_rpg`, `w_apg`, `w_spg`, `w_bpg`, `w_topg`, `w_ts`, `w_efg`, `w_usg`, `w_ast_pct`, `w_tov_pct`, `w_orb_pct`, `w_drb_pct`, `w_stl_pct`, `w_blk_pct`, `w_ft_rate`, `star_ppg`, `star_ts`, `star_usg`, `arch_wizard`, `arch_sorcerer`, `arch_warlock`, `arch_bard`, `arch_ranger`, `arch_barbarian`, `arch_paladin`, `arch_monk`, `arch_cleric`, `arch_druid`, `arch_rogue`, `arch_fighter`

</details>

#### Evaluation

Leave-one-season-out and 5-fold CV, team AdjEM MAE.

**Leave-one-season-out** (trains on later seasons too; optimistic, kept for continuity with older numbers).

Pooled: MAE 5.887, RMSE 7.502, R² 0.754.

5-fold CV (random folds, in-sample seasons): MAE 5.803, RMSE 7.352, R² 0.763.

#### Known limits

- Committed but unused. Its meta is checked lazily (`validate_box_score_model_meta`) rather than at boot, so its absence or drift cannot block the API.
