# training/experiments/

Accept/reject experiments, diagnostics and spikes against the model tree —
the scripts that answered "would X help?" and, mostly, "no". Nothing here is
in the retrain chain, imported by a trainer, or run by prod. Each script is
kept so its conclusion can be **re-checked, not re-derived**: the tree moves
(a new season, a retrain, a frame change) and a verdict is only as current as
the tree it was measured on.

**Read this table before re-running one.** The verdicts below already exist
in the methodology docs and the PR bodies; this puts them where the next
person looks. Where a verdict changed the served model, the row says what
shipped and where.

What it is not: `training/validation/` holds the older one-off de-risk
scripts (CAE-O/D, chemistry, oracle minutes) with its own index. The two
Layer 4 tuners (`transition_blend_diagnostic.py`,
`program_anchor_era_diagnostic.py`) live here because they are diagnostics,
but they are **not one-offs** — they are re-run after a Layer 2 retrain to
re-check the served constants (`docs/model_dependency_graph.md` §3, Layer 4).

## Running

Every script carries a path shim (parent `training/` on `sys.path`) so the
trainers and shared libs import. Run from `training/` with `DATABASE_URL`
set; most need the database, several need a backtest dump (`--dump`, always
pass it — the filename-order fallback can hand you a superseded generation),
and the game-model ones need the feature matrix (slow).

```bash
cd training && set -a && . ../.env && set +a
./.venv/bin/python experiments/<script>.py --help
```

Summary artifacts go to `training/eval_history/` (committed only when they
accompany the change they justify — see `docs/model_dependency_graph.md` §4).
`check_imports.py` imports every script here and is run by CI; a trainer that
renames a symbol an experiment reads fails there rather than at the next
re-run.

## Index

Judge column: **WF** = walk-forward (train strictly earlier than the test
season, the canonical judge since #361); **LOSO/LOPO/LOCO** = leave one
season / pair / class out (trains on later seasons too; optimistic). Shipped
verdicts name the PR.

### Layer 2 calibrator and the Layer 4 blend (team AdjEM)

| script | question | date | verdict | summary |
|---|---|---|---|---|
| `experiment_elite_gap.py` | After the linear calibrator (#363), where does the remaining top-10 under-projection live, and does anything move it? | 2026-09-21 | **Measured, left alone** (#372). Per player, ex-ante at elite destinations: −0.4 returners/transfers, −1.3 ranked freshmen, −2.1 top-10 recruits in 2024+, *over*-projected +0.1..+0.5 in 2021–23 — a portal-era sign flip. Era features on the trajectory model tie WF; convex/era terms on the calibrator lose at the top-25. Re-run when 2027 lands. | `eval_history/elite_gap_20260921_summary.json` |
| `experiment_od_decomposition.py` | Should the served net be O − D instead of a direct net model, and should the AdjO split be trained relative to the league mean? WF, through the served blend. | 2026-09-21 | **Direct net stays; relative AdjO SHIPPED** (#369). O−D loses on net (team O/D errors cancel in the direct model); the era-relative AdjO target closes the WF gap: AdjO 4.30 → 3.92, derived AdjD 3.94 → 3.65. | `eval_history/od_decomposition_20260921_summary.json` |
| `top_band_experiment.py` | Can the projection be made better at the *top* (membership and ordering of the top 25) without giving up absolute AdjEM? Lambdarank, top-weighting, coach-anchor variants; WF. | 2026-09-20 | **All REJECTED**; lambdarank is dead (7.80 raw). The headline was the **composition skew** it exposed: the ex-post training frame scored 5.26 where the ex-ante one scored 5.56 — test-set optimism, not a training advantage. Led to the v4 ex-ante frame the same day (`--sql-frame` reproduces the v3 numbers). | `eval_history/top_band_20260920_summary.json` |
| `eb_blend_experiment.py` | Replace the MAE-searched program anchor with an empirical-Bayes shrinkage whose weight is derived from estimated variances (Efron–Morris / Glickman–Stern), plus an OLS stack; WF. | 2026-09-20 | **REJECTED** (#359). EB +0.031 (z=+1.3) and the stack +0.037 (z=+1.5) vs the served blend pooled; EB better at the top but worse on level-changers. The served gate is a fitted nonlinearity that does not fall out of the variances. Served blend kept. | `eval_history/eb_blend_20260920_summary.json` |
| `roster_impact_target_experiment.py` | Does the tree calibrator's ceiling (max output 32–35 while actual maxima reached 46) come from regressing on the level, and does a residual target remove it? Same-season frame screen, LOSO. | 2026-09-18/19 | **Three fixes measured, none shipped.** Residual target lifts the ceiling but is worse end-to-end (z=+2.4); level features do not move the ceiling; linear stage + residual tree is a pooled tie with a +0.9 raw bias. Superseded by #363, which replaced the tree with OLS and removed the ceiling outright. | `eval_history/roster_impact_target_20260919_summary.json` |
| `program_anchor_era_diagnostic.py` | Is the #326 program anchor fit to a regime the portal era no longer follows, and does it hurt the top of the board? Cohorts by era and by prior-season tier; LOSO. | 2026-09-18 | **Constants era-stable; no change.** The gate serves 43% of the board as pure raw (0 < corroboration < 1 ⇒ anchor = raw); top-10 programs that lose their core reload (in-sample they want w=0.80) while ranks 11–50 run +1 over; a tier weight is fold-unstable and worse LOSO. Also the Layer 4 tuner for `PROGRAM_ANCHOR_SHRINK`. | `eval_history/program_anchor_era_20260918_summary.json` |
| `transition_blend_diagnostic.py` | Is a flat blend weight mis-tuned for new-coach / roster-overhaul teams? Cohorts by the served `retained` fraction; LOSO refit of the ramp constants. | 2026-06-03 → 2026-08-23 (re-run after every Layer 2 retrain) | **SHIPPED** as the turnover ramp (2026-06-03), retuned 2026-06-27 (0.45/0.20), re-keyed 2026-08-23 (#322: the tuner had cohorted on a hindsight proxy; on the served key the ramp is worth +0.003, not +0.05) and refit with the program anchor (#325: 0.70/0.55). The Layer 4 tuner for `PROJECTION_SHRINK_WEIGHT{,_OVERHAUL}`. | `eval_history/transition_blend_diagnostic_2026*_summary.json` |

### Layer 1 trajectory model (player CamPom)

| script | question | date | verdict | summary |
|---|---|---|---|---|
| `experiment_trajectory_destination.py` | Does telling the model *where* a player plays next season fix its team-context bias (elite-destination returners −0.95, transfers in −1.13 on 2024+)? LOPO. | 2026-09-19 | **SHIPPED** (#354) as the five-feature destination block. LOPO MAE 2.083 → 2.008, better in all 11 folds (z=−16); destination-tier bias → ≈0. The ablation shows it is destination *strength*, not a transfer flag (prior-team strength alone leaves transfers-into-elite at −1.33). | `eval_history/trajectory_destination_20260919_summary.json` |
| `experiment_trajectory_history.py` | The model saw one prior season; does the prior-prior season (lag-2 levels) and/or the year-over-year slope help? LOPO, NaN-native encoding. | 2026-06-18 | **SHIPPED** (2026-06-27, 51 → 60 features). Full MAE 2.121 → 2.088; covered subset (53%) 2.206 → 2.141, 10/11 pairs, lift on upperclassmen. Fixes the Pierce-type dip. | `eval_history/trajectory_history_experiment_20260618_summary.json` |
| `experiment_trajectory_history_sentinel.py` | Does the history-block win survive the −999/0 sentinel encoding the ONNX serve path actually uses (LightGBM had routed NaN natively)? | 2026-06-27 | **Confirmed**: +0.0625 covered under sentinels vs +0.065 NaN-native. The contract was locked on this result; the training fill and the Rust fill are kept in lockstep. | `eval_history/trajectory_history_sentinel_20260627_summary.json` |
| `experiment_trajectory_lag3.py` | Does a third season (N-2: lag-3 levels, an acceleration term) add anything on top of the shipped lag-2 block? | 2026-06-27 | **REJECTED**: +0.004–0.008 covered MAE on the 29%-covered cohort, an order of magnitude below the lag-2 win and under the on/off bar. One season plus a one-year slope is the recoverable signal. | `eval_history/trajectory_lag3_experiment_20260627_summary.json` |
| `experiment_trajectory_onoff.py` | Tier-2 membership features: do prior-season on/off splits (on-court net, on/off swing, possession share) help? LOPO. | 2026-06-11 | **SHIPPED** (48 → 51 features) — the first positive PBP-feature verdict. Covered MAE −0.011, 9/11 pairs, all class buckets improve. Kept as the re-test harness; its "baseline" is the pre-accept 48-feature model. | `eval_history/tier2_membership_models_20260611_summary.json` |
| `experiment_trajectory_rapm.py` | Per-season RAPM is 2–5× more stable year-over-year than raw on/off; is it the better trajectory feature? Swap and add variants. | 2026-06-12 | **REJECTED**: swapping is decisively worse (−0.010, 1/11 pairs), adding reads +0.0009 — raw on/off's team-context "contamination" is signal for a team-contextual target. On/off keeps the slot. | `eval_history/trajectory_rapm_experiment_20260612_summary.json` |
| `experiment_trajectory_pbp.py` | Tier-1 PBP tag rates (paint, transition, second-chance, fouls drawn) as prior-season features, raw and percentile-encoded. | 2026-06-10 | **REJECTED** (+0.003). CamPom absorbs the value signal; tags exist 2020+ only. | `eval_history/tier1_pbp_models_20260610_summary.json` |

### Game models (margin / win / total)

| script | question | date | verdict | summary |
|---|---|---|---|---|
| `experiment_game_value_features.py` | Do value-weighted roster-shape features (`diff_rv_*`: top-1/top-3 value, gap, spread — the star by *value* rather than minutes) beat the 49-feature baseline? LOSO, seed-averaged. | 2026-07-23 | **REJECTED** — fails the ironclad bar (pooled MAE down *and* ≥5/6 folds); the gate stays default-off. CamPom's mean already carries the shape. Companion to `docs/injury_availability_investigation.md`. | `eval_history/value_features_backtest_20260723.json` |
| `experiment_game_lineups.py` | Tier-2 lineup-quality features (possession HHI, top-lineup share and net) as expanding team diffs. Shared 2026 holdout. | 2026-06-11 | **REJECTED**: degraded all 7 holdout metrics (margin MAE +0.030) while ranking 10–15/52 by importance — the importance-is-not-value trap. `LINEUP_FEATURES` plumbing kept gated off. | `eval_history/tier2_membership_models_20260611_summary.json` |
| `experiment_game_pbp.py` | Tier-1 PBP tag-rate team diffs (7 features). Shared 2026 holdout. | 2026-06-10 | **REJECTED** (margin MAE +0.075). `PBP_FEATURES` plumbing kept gated off in `features.py`. | `eval_history/tier1_pbp_models_20260610_summary.json` |

### Archetypes

| script | question | date | verdict | summary |
|---|---|---|---|---|
| `experiment_archetype_stability.py` | How many games until an in-season archetype label matches the full-season one? Assignment only, frozen centroids. **Re-run after every archetype retrain** — the curve is a property of the fit. | 2026-07-17 | **No stabilisation point exists.** 65.7% primary match at the ≥10 GP gate, 81.9% at 20, still climbing; the control (all games) is 93.2%. Top-2 is far more forgiving (85.6% at 10). Turned the cold start from a tuning problem into a presentation one: provisional labels, prior-season carry-over, live newcomer inference (all shipped 2026-07-17). | `eval_history/archetype_stability_20260717_summary.json` |
| `experiment_archetypes_pbp.py` | Tier-1 PBP style signals added to the archetype feature matrix and re-clustered. | 2026-06-10 | **REJECTED**: 9 signature-alignment violations; the 2015–2019 cohort (no tags) pins to the centroid on the new axes. | `eval_history/tier1_pbp_models_20260610_summary.json` |

### RAPM

| script | question | date | verdict | summary |
|---|---|---|---|---|
| `experiment_rapm_spike.py` | Single-season (2026) ridge adjusted plus-minus: does player-level allocation out-predict team strength on game-blocked held-out stints, and is it more stable year-over-year than raw on/off? | 2026-06-12 | **REJECTED as a value metric** (`docs/rapm_methodology.md` §8). Gate 4(b) fails — a team-columns ridge beats player RAPM on held-out stint MSE; YoY stability beats raw on/off decisively (+0.24 vs +0.11) but the CamPom prior variant inherits CamPom's. The Zuby test passes. | `eval_history/rapm_spike_2026_20260612_summary.json`, `rapm_spike_stability_20260612_summary.json` |
| `experiment_rapm_pooled.py` | Multi-season pooled RAPM (career chains via `natstat_id` ∪ `torvik_pid`, decayed prior seasons): prequential, split-half, star separation. | 2026-06-12 | **Method VALIDATED, model use not pursued** (§10.7). Pooled beats single-season and the carried-over team ridge out-of-year, but a clipped CamPom carry-over still edges it — CamPom absorbs the value signal. Split-half net 0.30 → 0.38; D-RAPM stays the noisy side. Shipped as the *display* "Adj on/off" at w3 / decay 0.7 / λ2000 (`training/rapm.py`). | `eval_history/rapm_pooled_spike_20260612_summary.json` |

## Adding one

Copy the shim from any script here, write the question and the decision
metric into the module docstring before the code, write the summary JSON to
`training/eval_history/`, and add a row above with the verdict the day you
have it. A verdict that lives only in a PR body is the drift this index
exists to stop.
