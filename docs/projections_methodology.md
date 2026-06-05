# 2027 Projection Methodology (v3 — roster-impact v2: OOF-trained impact-aggregation model)

## What it is

`GET /api/projections/{year}` projects an upcoming season's team AdjEM band per team by composing a hypothetical N+1 roster from three sources and scoring it with the **roster-impact model** (`roster_impact_model.onnx`):

1. **Returning players** — last season's qualifying roster minus seniors, outbound portal commits, firm NBA-draft departures, and (in the floor scenario) declared-but-undecided draft entrants.
2. **Incoming portal transfers** — players committed to this team in the matching portal cycle, with their *source-team* stats carried as their PlayerRow.
3. **Incoming HS recruits** — class-of-`base_season` commits to this team. Each is given a per-recruit projected cam_v3 from the freshman-impact model (see *Recruit synthesis* below).

Each scenario's roster is scored from its *projected* cam_v3 distribution (see *Feature extraction* and *Projected cam_v3* below); the raw model output is then blended with the team's actual baseline AdjEM (see *Scoring & calibration* below).

## Composition (`crates/cstat-core/src/roster_projection.rs`)

`compose_all_projections(pool, base_season, draft_entrants) -> Vec<ProjectedRoster>` runs three concurrent SQL fetches:

```sql
-- 1. roster_rows: every qualified player from base_season (≥5 GP, ≥5 MPG)
SELECT p.id, p.name, pss.team_id, p.class_year, …
FROM player_season_stats pss
JOIN players p ON p.id = pss.player_id
LEFT JOIN player_archetypes pa ON …
LEFT JOIN torvik_player_stats tps ON …
WHERE pss.season = $base_season AND pss.games_played >= 5 AND pss.minutes_per_game >= 5

-- 2. transfers: every portal commit for the matching cycle
SELECT cstat_player_id, destination_institution
FROM transfers WHERE year = $base_season

-- 3. recruits: every HS recruit committed to a cstat-resolved team
SELECT id, full_name, composite_rank, star_rating, committed_team_id, commit_status
FROM recruits
WHERE year = $base_season
  AND committed_team_id IS NOT NULL
  AND COALESCE(commit_status, '') <> 'Uncommitted'
```

Per-team buckets then partition each row into `returning` / `arrivals` / `recruits` / `uncertain` (declared `?` cohort) / `departures` (audit-only).

## Recruit synthesis

Each recruit becomes a minimal synthetic `PlayerRow` via `roster_projection.rs::freshman_row`, carrying exactly three load-bearing fields: the **freshman-impact model's per-recruit projected `cam_v3`**, `class_year = "Fr"`, and `primary_class = None`. Nothing else (mpg, box-score, rate stats) is synthesized — the served roster-impact model ranks the roster by `cam_v3` and weights every feature by canonical-rotation MPG, so it reads only those three fields off a recruit row. A 5★ projected to bust and a 5★ projected to star therefore get genuinely different rows.

**Tiers were deprecated** (mid-2026). The projection used to bucket `composite_rank` into four tiers (T1 ≤30 / T2 ≤100 / T3 ≤250 / T4 rest) and synthesize a tier-mean box-score statline. That statline reached no served model (verified: deleting it left `/api/projections` byte-identical), so the 4-tier scaffold, the `{T1..T4}_PROFILE` constants, and the `composite_rank → tier` bucketing were removed. The per-recruit model — which was always rank/stars-*granular*, never bucketed — is the sole freshman signal.

**Freshman-impact model** (`training/train_freshman_model.py`, ONNX in `training/models/freshman_*_model.onnx`): a LightGBM regression on 13 continuous features (the shared 11-element recruit block — `composite_rank`, `composite_rating`, `star_rating`, `position_rank`, `rank_movement`, physicals — plus `committed_team_prior_adjem` and `peer_class_strength`) targeting the recruit's first-college-season `cam_gbpm_v3_psos`. Trained on the full class-of-2014→2025 paired history (**n ≈ 3253**, gate ≥5 GP / ≥5 MPG). LOCO pooled MAE ≈ **2.25**, beating a rank-bucket-mean yardstick (≈2.42) by ~6.6%. The mean model carries a sentinel-safe `monotone_constraints` (non-decreasing in `composite_rating` + `star_rating`) so that, holding the other inputs fixed, a better-rated recruit never projects lower — a narrow legibility guarantee, since `composite_rank` (the stronger feature) stays unconstrained. q10/q90 band models are unconstrained (LightGBM forbids monotone + quantile). Re-run the script when a new class year's freshmen finish a season.

**Fallback**: when whole-batch inference fails (degraded, warn-logged), `freshman_row` falls back to `FRESHMAN_FALLBACK_CAM_V3` = **+1.20**, the unconditional mean of the training target — the least-biased point estimate when the model is unavailable. The normal path gives every recruit a model prediction, so this is rarely hit.

## Scenarios

`ProjectedRoster::for_scenario(DraftScenario)` materializes the player list for the model:

| Source | Floor | Ceiling |
|---|---|---|
| Returning | ✓ | ✓ |
| Arrivals (portal) | ✓ | ✓ |
| **Recruits** | **✓** | **✓** |
| Uncertain (declared `?`) | ✗ | ✓ |

Recruits are unconditional: a 5★ HS commit to Duke shows up in both the floor and ceiling projections. The band width reflects only the draft `?` cohort; recruit projection uncertainty (the freshman model's q10–q90, widest for elite recruits) is surfaced per-player on the team-detail page, not in the team-level floor/ceiling band.

## Feature extraction (`roster_impact::build_roster_impact_features`)

Before feature extraction, every returner / arrival has its `cam_v3` overwritten with a *projected* next-season value (see *Projected cam_v3* below); recruits already carry the freshman model's per-player prediction.

`build_roster_impact_features` ranks the roster by `cam_v3` descending, takes the top 13 as the rotation, and assigns each rank a canonical MPG calibrated from qualified team-seasons:

`[32.0, 29.8, 27.8, 25.5, 23.0, 20.1, 17.2, 14.4, 11.9, 9.6, 8.2, 7.3, 6.9]`

It emits 27 features: the cam_v3 distribution (Σ, minutes-weighted mean, top-1/3/7, counts over +5/+10/+15), a minute-weighted experience mix (Fr/So/Jr/Sr share — returners aged up one season), a minute-weighted archetype balance (12 D&D-class shares), and the portal `outbound`/`inbound` cam_v3 sums (added in PR A — largely redundant with `cam_sum`, retained for the small residual signal). Every aggregate uses the canonical-MPG weighting, *identical* in training (`train_roster_impact_model.py`) and at serve — so no out-of-distribution minutes, which was the box-score failure mode. the roster-impact model reads no per-game counting stats, so unlike the box-score model there is no stat rescaling.

Why this is the right consumer for cam_v3: `train_roster_model.py` (the box-score model) deliberately *excludes* cam_v3 because `Σ(cam_v3 × minute_share) ≈ AdjEM` collapses it to the player-impact identity — fatal for a roster-*swap* model. For a *projection* that identity is exactly the goal, so the roster-impact model is a clean calibrator and all projection error lives in the upstream cam_v3 predictions — honest and decomposable. The box-score `roster_model.onnx` is now fully dead — the swap-Δ tool moved to archetype-based `roster_fit`, and the projections backtest's box-score comparison was dropped when tiers were deprecated (the box-score model reads a freshman statline that no longer exists). The model artifacts remain in `training/models/` pending a separate removal.

## Model training (v2 — "train on what you serve")

`training/train_roster_impact_model.py` trains the model on 4,255 team-seasons (2015–2026), target `team_season_stats.adj_efficiency_margin`. The load-bearing detail is *which* `cam_v3` the training aggregator sees:

- **v1** trained on each player's *actual* same-season `cam_gbpm_v3_psos`.
- **v2** (current) trains on the *projected* `cam_v3` — the held-out OOF predictions the upstream models emit: `trajectory_oof_predictions` for returners, `freshman_oof_predictions` for recruits, falling back to actual `cam_gbpm_v3_psos` for the cohort neither table covers (true walk-on freshmen, JUCO arrivals, pre-2015 priors, season 2015 itself). Coverage: ~58% of player-rows OOF, ~38% actual fallback, ~3% with no cam_v3 at all (held a rotation slot but skipped in the cam aggregates, same as v1). The fallback cohort skews to low-minute bench slots, so its weight in the load-bearing minute-weighted aggregates (`cam_sum`, `cam_wmean`) is well under 38%. `build_dataset` prints the per-source breakdown and it lands in `roster_impact_model_meta.json::cam_v3_coverage`.

Why v2: at serve the route *only ever* feeds projected `cam_v3`, and those projections are regression-biased — the trajectory model under-projects elite returners by ≈3.4 CamPom. v1 learned a calibration slope for *unbiased* inputs and then inherited that upstream bias raw. v2 trains on the same regression-biased inputs it serves, so the calibrator absorbs the bias instead of passing it through. Net effect on the end-to-end backtest: raw projector MAE 6.58 → 6.39, raw bias +0.44 → +0.62, optimal blend weight 0.55 → 0.50 (a better-calibrated raw projector earns marginally more trust).

The end-to-end `cstat-ingest projections-backtest` scores each target season with a **leave-one-season-out** model (`models/roster_impact_loso/roster_impact_model_{year}.onnx`, trained on every season *except* the one scored) — so the documented 5.86 MAE carries no in-sample leak from the roster model. The live route keeps the all-seasons `roster_impact_model.onnx`: for the live forward year the target is genuinely unseen, so there is nothing to leak. LOSO models are gitignored diagnostic artifacts, regenerated by rerunning the training script (only the backtestable seasons — 2025 / 2026 — are exported).

## Projected cam_v3 (`roster_projection::project_returner_cam_v3`)

Each returner / arrival gets a forward-looking `cam_v3`:
- **OOF-first**: for a historical target season the trajectory model trained on, `trajectory_oof_predictions` holds leave-one-pair-out (honest, not in-sample) predictions.
- **Live trajectory inference** for everyone else — the forward year, and transitions the model didn't train on.
- Recruits get the freshman-impact model's per-recruit prediction, baked into the synthesized PlayerRow by `freshman_row`.

This is what makes returner growth and freshman upside count: a junior projected to break out, or an elite freshman, moves the team projection through their `cam_v3`.

## Scoring & calibration

Each scenario's feature vector is scored by `predict_roster_impact`, then blended with the team's base-season AdjEM:

```
midpoint  = p̄·shrink(ceiling_raw) + (1−p̄)·shrink(floor_raw)
shrink(r) = w·baseline + (1−w)·r + 0.0
w         = 0.50 for continuity rosters, ramping to 0.25 for roster overhauls
```

- **0.50 baseline weight, 0.0 offset (continuity teams)** — tuned on the end-to-end `cstat-ingest projections-backtest` against actual 2025 + 2026 AdjEM (496 pooled team-years), scored with the leave-one-season-out models so the figure is leak-free. roster-impact v2's raw output is a genuine projector (raw MAE 6.39, bias +0.62), not a same-season descriptive model used out of context, so the blend leans far less on baseline persistence than the box-score pipeline did (0.50 vs 0.80) and needs **no calibration offset** — the box-score pipeline's `+2.0` corrected a structural −4.8 bias the box-score model had as a projector; the roster-impact model doesn't have it. The MAE curve is flat across weight 0.40–0.60; 0.50 is the optimum. **Blended pipeline MAE 5.86**, beating both baseline-persistence (6.53) and the old box-score pipeline (6.23). (v1's 0.55 / 5.88 was the same pipeline before the v2 OOF retrain, and carried a small in-sample leak the LOSO backtest now removes — the headline gain is the honest measurement, not the 0.02 MAE.)
- **Turnover-conditional weight (overhaul teams)** — `baseline` (last season's AdjEM) is a *stale anchor* when a roster turns over wholesale, so the weight `w` ramps from 0.50 down to **0.25** as the team's *retained talent fraction* (`Σ base-season cam_v3 of returners / (returners + departures)`) falls from 0.40 to 0.20. Validated on the 2019–2026 LOSO backtest (`training/transition_blend_diagnostic.py`): a turnover-conditional weight beats the flat 0.50 by **~+0.04 MAE pooled** (concentrated on the ~25% of teams with heavy turnover, and ~+0.20 on new-coach teams) and corrects the **≈+0.7 AdjEM over-projection** of overhaul teams. Keyed on roster turnover alone, **not** `is_new_hc` — turnover is the stronger signal (it directly measures baseline staleness, and subsumes 61% of new-HC teams), has no false positives from same-roster coaching changes, and lives on `ProjectedRoster` so the route and `compute-projections` stay in lockstep with no DB fetch. Constants + the ramp live in `roster_projection::{PROJECTION_SHRINK_WEIGHT_OVERHAUL, transition_shrink_weight, retained_talent_fraction}`; the per-team weight is surfaced on `ProjectedTeam.baseline_weight`.
- **p̄** is the mean probability the team's uncertain (declared-draft) cohort returns, from the Tankathon mock board: pick ≤30 → 0.05, 31–60 → 0.50, off-board → 0.85. It replaces a flat 50/50 floor/ceiling average, which over-penalized draft-talent-heavy (i.e. top) teams.

**Re-tuning playbook**: run `cargo run --bin cstat-ingest -- projections-backtest` — it composes the full pipeline for 2025 / 2026 with held-out OOF cam_v3, prints the roster-impact model raw vs baseline-persistence, and sweeps the blend weight. Set `PROJECTION_SHRINK_WEIGHT` / `PROJECTION_OFFSET` in `roster_projection.rs` from the sweep's optimum (the route's `predict_team` and the shared `score_projection_adj_em` both read those constants). The turnover ramp is re-validated separately via `training/transition_blend_diagnostic.py`. The backtest needs the per-season LOSO models in `models/roster_impact_loso/`; rerun `train_roster_impact_model.py` first if they are absent (it fails with a clear message if so).

## Limitations and upgrade paths

- **Recruit cam_v3 is per-player, but pre-college signal is weak.** `freshman_row` scores each recruit through the freshman-impact model — a 5★ projected to bust and one projected to star get different rows. But composite rank stops separating past ~100, so spread among lower-ranked recruits is modest and elite-recruit bands (q10–q90) are wide; the per-recruit point estimate is honest only as a directional read.
- **Low/unranked recruits all sit near replacement.** True walk-ons (high school stars who walked on at a high-major) project similarly to lightly-recruited low-major freshmen. Their team-level contribution is capped naturally: a low projected `cam_v3` ranks them into a low-minute canonical slot, so they barely move the team number.
- **Returner growth and freshman upside now count** (the roster-impact model, shipped). The projection scores *projected* `cam_v3` — the trajectory model for returners / arrivals, the freshman model for recruits — so a junior breaking out as a senior, or an elite freshman, moves the number. Residual caveat: the trajectory model's documented elite-tail regression (≈−3.4 bias on +15–20 prior CamPom, pure extrapolation above +20) flows into the cam_v3 inputs. The v2 OOF retrain ("train on what you serve") lets the roster-impact calibrator absorb the *systematic* component of that bias — it now trains on the same regression-biased inputs it serves — but the *per-team* variance still flows through, and a freshman / portal class the trajectory model misjudges still moves the wrong way. More training seasons (Phase 6) is the real remedy. See ROADMAP §5b "Projection bias fix".
- **Recruit-to-team commit resolution is name + alias-based.** 305/305 of class-of-2026 committed recruits resolved; 303/307 of class-of-2024; 465/472 of class-of-2025. Residue is non-D1 schools (Le Moyne, NCAA-D2 / NAIA destinations).
- **Walk-on freshmen not on 247's composite rankings are invisible.** Their teams get a slightly pessimistic projection, but the impact is small.

## Calibration refresh playbook

Run when:
1. A new HS recruiting class is ingested AND its freshman cstat-season has finished.
2. CamPom v3 formula changes (target shifts).

Retrain the freshman-impact model on the expanded paired history:

```bash
cd training && python train_freshman_model.py
```

It re-validates via LOCO CV, re-emits the 3 ONNX models + `freshman_oof_predictions`, and rewrites `freshman_model_meta.json`. The Rust boot validator hard-fails on feature/alpha drift. Then rebuild + retest:
```bash
cargo test -p cstat-core roster_projection
curl -s http://localhost:8080/api/projections/2027 | jq '.teams[0]'
```
