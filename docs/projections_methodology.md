# 2027 Projection Methodology (v3 — Phase B impact-aggregation model)

## What it is

`GET /api/projections/{year}` projects an upcoming season's team AdjEM band per team by composing a hypothetical N+1 roster from three sources and scoring it with the **Phase B impact-aggregation model** (`roster_impact_model.onnx`):

1. **Returning players** — last season's qualifying roster minus seniors, outbound portal commits, firm NBA-draft departures, and (in the floor scenario) declared-but-undecided draft entrants.
2. **Incoming portal transfers** — players committed to this team in the matching portal cycle, with their *source-team* stats carried as their PlayerRow.
3. **Incoming HS recruits** — class-of-`base_season` commits to this team. Each is given a per-recruit projected cam_v3 from the freshman-impact model; the tier-mean profile below supplies the box-stat scaffold and rank bucketing.

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

Each recruit becomes a synthetic `PlayerRow` keyed on `composite_rank`. Four tiers:

| tier | composite_rank | label | T1+T2 commits in 2026 class |
|---|---|---|---|
| T1 | 1–30 | elite (5★ tier) | 24 Signed + 6 Committed |
| T2 | 31–100 | top-100 (mostly 4★) | 54 Signed + 12 Committed |
| T3 | 101–250 | lower 4★ / mid 3★ | 82 Signed + 47 Committed |
| T4 | 251+ or NULL | walk-on equivalent / late-blooming / international | 33 Signed + 47 Committed |

Per-tier profile (mean per-game stats) was calibrated from the empirical join of class-of-2024 and class-of-2025 recruits onto their actual freshman cstat-seasons (2025 and 2026, respectively):

```sql
WITH paired AS (
  SELECT r.composite_rank, r.star_rating,
         pss.minutes_per_game, pss.games_played,
         pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
         pss.true_shooting_pct, pss.effective_fg_pct, pss.usage_rate,
         pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
         pss.stl_pct, pss.blk_pct, pss.ft_rate,
         tps.cam_gbpm_v3_psos AS cam_v3
  FROM recruits r
  JOIN players p ON p.id = r.cstat_player_id
  JOIN player_season_stats pss ON pss.player_id = p.id AND pss.season = r.year + 1
  LEFT JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = r.year + 1
  WHERE r.year IN (2024, 2025) AND r.cstat_player_id IS NOT NULL
)
SELECT
  CASE
    WHEN composite_rank <= 30 THEN 'T1'
    WHEN composite_rank <= 100 THEN 'T2'
    WHEN composite_rank <= 250 THEN 'T3'
    ELSE 'T4'
  END AS tier,
  COUNT(*), AVG(minutes_per_game), AVG(ppg), AVG(usage_rate), AVG(cam_v3), …
FROM paired GROUP BY 1 ORDER BY 1;
```

Sample sizes: T1 n=52, T2 n=114, T3 n=201, T4 n=185. **558 paired recruits total.**

Constants table is hard-coded in `roster_projection.rs::{T1,T2,T3,T4}_PROFILE`. When a new class year ingests (e.g. class-of-2026 starts playing in cstat-season 2027), re-run the calibration query and update the constants.

Headline tier signal (CamPom v3):

| tier | n | mpg | ppg | usg | cam_v3 |
|---|---|---|---|---|---|
| T1 | 52 | 24.0 | 11.8 | 0.232 | **+8.97** |
| T2 | 114 | 14.4 | 5.5 | 0.194 | +2.41 |
| T3 | 201 | 12.7 | 4.2 | 0.175 | +0.70 |
| T4 | 185 | 14.1 | 4.9 | 0.184 | −0.57 |

T3 vs T4 are nearly identical — composite rank stops being a strong signal past ~100. Don't claim more precision than the cohort supports.

**What the projection actually scores**: `synthesize_freshman_row` sets each recruit's `cam_v3` to the **freshman-impact model's per-recruit prediction**, not the tier mean — so a 5★ projected to bust and a 5★ projected to star get different rows. The tier profile above supplies only the box-stat scaffold (unused by Phase B, which reads `cam_v3`) and the `composite_rank` → tier bucketing; the tier-mean `cam_v3` column is the population baseline the per-player model is calibrated against, and the last-resort fallback when the freshman model can't score a recruit.

## Scenarios

`ProjectedRoster::for_scenario(DraftScenario)` materializes the player list for the model:

| Source | Floor | Ceiling |
|---|---|---|
| Returning | ✓ | ✓ |
| Arrivals (portal) | ✓ | ✓ |
| **Recruits** | **✓** | **✓** |
| Uncertain (declared `?`) | ✗ | ✓ |

Recruits are unconditional: a 5★ HS commit to Duke shows up in both the floor and ceiling projections. The band width reflects only the draft `?` cohort, not the recruit uncertainty (which is folded into the tier-mean profile's variance — wider for T1 because elite freshmen vary the most year to year).

## Feature extraction (`roster_impact::build_roster_impact_features`)

Before feature extraction, every returner / arrival has its `cam_v3` overwritten with a *projected* next-season value (see *Projected cam_v3* below); recruits already carry the freshman model's per-player prediction.

`build_roster_impact_features` ranks the roster by `cam_v3` descending, takes the top 13 as the rotation, and assigns each rank a canonical MPG calibrated from qualified team-seasons:

`[32.0, 29.8, 27.8, 25.5, 23.0, 20.1, 17.2, 14.4, 11.9, 9.6, 8.2, 7.3, 6.9]`

It emits 25 features: the cam_v3 distribution (Σ, minutes-weighted mean, top-1/3/7, counts over +5/+10/+15), a minute-weighted experience mix (Fr/So/Jr/Sr share — returners aged up one season), and a minute-weighted archetype balance (12 D&D-class shares). Every aggregate uses the canonical-MPG weighting, *identical* in training (`train_roster_impact_model.py`) and at serve — so no out-of-distribution minutes, which was the Phase A failure mode. Phase B reads no per-game counting stats, so unlike the box-score model there is no stat rescaling.

Why this is the right consumer for cam_v3: `train_roster_model.py` (the box-score model) deliberately *excludes* cam_v3 because `Σ(cam_v3 × minute_share) ≈ AdjEM` collapses it to the player-impact identity — fatal for the swap-Δ tool. For a *projection* that identity is exactly the goal, so Phase B is a clean calibrator and all projection error lives in the upstream cam_v3 predictions — honest and decomposable. The box-score `roster_model.onnx` is untouched and still serves swap-Δ.

## Projected cam_v3 (`roster_projection::project_returner_cam_v3`)

Each returner / arrival gets a forward-looking `cam_v3`:
- **OOF-first**: for a historical target season the trajectory model trained on, `trajectory_oof_predictions` holds leave-one-pair-out (honest, not in-sample) predictions.
- **Live trajectory inference** for everyone else — the forward year, and transitions the model didn't train on.
- Recruits get the freshman-impact model's per-recruit prediction, baked into the synthesized PlayerRow by `synthesize_freshman_row`.

This is what makes returner growth and freshman upside count: a junior projected to break out, or an elite freshman, moves the team projection through their `cam_v3`.

## Scoring & calibration

Each scenario's feature vector is scored by `predict_roster_impact`, then blended with the team's base-season AdjEM:

```
midpoint  = p̄·shrink(ceiling_raw) + (1−p̄)·shrink(floor_raw)
shrink(r) = 0.55·baseline + 0.45·r + 0.0
```

- **0.55 baseline weight, 0.0 offset** — tuned on the end-to-end `cstat-ingest projections-backtest` against actual 2025 + 2026 AdjEM (496 pooled team-years). Phase B's raw output is a genuine projector (raw MAE 6.58, bias +0.44), not a same-season descriptive model used out of context, so the blend leans far less on baseline persistence than Phase A's box-score pipeline did (0.55 vs 0.80) and needs **no calibration offset** — Phase A's `+2.0` corrected a structural −4.8 bias the box-score model had as a projector; Phase B doesn't have it. The MAE curve is flat across weight 0.50–0.60; 0.55 is the optimum. **Blended pipeline MAE 5.88**, beating both baseline-persistence (6.53) and the old Phase A pipeline (6.23).
- **p̄** is the mean probability the team's uncertain (declared-draft) cohort returns, from the Tankathon mock board: pick ≤30 → 0.05, 31–60 → 0.50, off-board → 0.85. It replaces a flat 50/50 floor/ceiling average, which over-penalized draft-talent-heavy (i.e. top) teams.

**Re-tuning playbook**: run `cargo run --bin cstat-ingest -- projections-backtest` — it composes the full pipeline for 2025 / 2026 with held-out OOF cam_v3, prints Phase B raw vs Phase A vs baseline-persistence, and sweeps the blend weight. Set `SHRINK_WEIGHT` / `PROJECTION_OFFSET` in `routes/projections.rs` from the sweep's optimum.

## Limitations and upgrade paths

- **Tier-mean is population average, not per-player.** A 5★ who busts and a 5★ All-American both project as +8.97 CamPom. The Phase 6 freshman-impact prior model is the upgrade.
- **T4 includes everyone the 247 composite ranking doesn't reach.** True walk-ons (high school stars who walked on at a high-major) are projected with the same profile as low-major freshmen with light recruiting. The minute share they actually get is governed by the roster model's minutes-weighted aggregation — a heavy T4 cohort will see their MPG cap their team-level contribution naturally.
- **Returner growth and freshman upside now count** (Phase B, shipped). The projection scores *projected* `cam_v3` — the trajectory model for returners / arrivals, the freshman model for recruits — so a junior breaking out as a senior, or an elite freshman, moves the number. Residual caveat: the trajectory model's documented elite-tail regression (≈−3.4 bias on +15–20 prior CamPom, pure extrapolation above +20) flows straight through the cam_v3 inputs. Phase B makes that error *attributable to the trajectory model* rather than masking it behind a roster-composition artifact + offset; more training seasons (Phase 6) is the remedy. See ROADMAP §5b "Projection bias fix".
- **Recruit-to-team commit resolution is name + alias-based.** 305/305 of class-of-2026 committed recruits resolved; 303/307 of class-of-2024; 465/472 of class-of-2025. Residue is non-D1 schools (Le Moyne, NCAA-D2 / NAIA destinations).
- **Walk-on freshmen not on 247's composite rankings are invisible.** Their teams get a slightly pessimistic projection, but the impact is small.

## Calibration refresh playbook

Run when:
1. A new HS recruiting class is ingested AND its freshman cstat-season has finished.
2. CamPom v3 formula changes (target shifts).
3. The 4-tier breakdown stops fitting (e.g. 247 changes their rank cutoffs).

```sql
-- See calibration query above. Update T*_PROFILE constants from the output.
```

Then rebuild + retest:
```bash
cargo test -p cstat-core roster_projection
curl -s http://localhost:8080/api/projections/2027 | jq '.teams[0]'
```
