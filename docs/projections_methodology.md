# 2027 Projection Methodology (v2 — holistic roster)

## What it is

`GET /api/projections/{year}` projects an upcoming season's team AdjEM band per team by composing a hypothetical N+1 roster from three sources and scoring it with the roster-only AdjEM ONNX model:

1. **Returning players** — last season's qualifying roster minus seniors, outbound portal commits, firm NBA-draft departures, and (in the floor scenario) declared-but-undecided draft entrants.
2. **Incoming portal transfers** — players committed to this team in the matching portal cycle, with their *source-team* stats carried as their PlayerRow.
3. **Incoming HS recruits** — class-of-`base_season` commits to this team, synthesized into a PlayerRow from a tier-mean freshman profile.

Before scoring, each scenario's roster is re-cast as a realistic cam_v3-ranked rotation (see *Rotation normalization* below); the raw model output is then blended with the team's actual baseline AdjEM and a calibration offset (see *Scoring & calibration* below).

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

## Scenarios

`ProjectedRoster::for_scenario(DraftScenario)` materializes the player list for the model:

| Source | Floor | Ceiling |
|---|---|---|
| Returning | ✓ | ✓ |
| Arrivals (portal) | ✓ | ✓ |
| **Recruits** | **✓** | **✓** |
| Uncertain (declared `?`) | ✗ | ✓ |

Recruits are unconditional: a 5★ HS commit to Duke shows up in both the floor and ceiling projections. The band width reflects only the draft `?` cohort, not the recruit uncertainty (which is folded into the tier-mean profile's variance — wider for T1 because elite freshmen vary the most year to year).

## Rotation normalization (`roster_features::project_rotation`)

The composed roster carries every player's *prior* minutes — returners at last year's role, recruits at a tier-fixed MPG, arrivals at their *source-team* MPG. Nobody is promoted into minutes vacated by departed seniors / drafted players, so a gutted roster sums to ~150 player-minutes and a portal-stacked one past 230 — both outside the ~221 Σmpg the roster model trained on. Feeding that to `predict_adj_em` is out-of-distribution extrapolation and was the dominant driver of top teams projecting absurdly low (Purdue, a +36 AdjEM team, scored at raw +3).

`project_rotation` fixes this before feature extraction: it ranks the scenario's players by `cam_v3` descending and assigns each rank a canonical MPG calibrated from 1,090 qualified team-seasons (2024–26):

`[32.0, 29.8, 27.8, 25.5, 23.0, 20.1, 17.2, 14.4, 11.9, 9.6, 8.2, 7.3, 6.9]`

Rank ≥13 falls out of the rotation (0 MPG). Per-game counting stats (ppg/rpg/apg/spg/bpg/topg) are rescaled by `new_mpg / old_mpg` (clamped to ×0.4–×2.5); rate stats (TS/eFG/USG/`*_pct`/ft_rate) are minutes-invariant and pass through unchanged. This makes `cam_v3` — including the freshman model's per-recruit prediction — load-bearing for the projection *without* adding it as a roster-model feature (the train script forbids that: it collapses the model to the player-impact identity).

## Scoring & calibration

Each scenario's normalized roster is scored by `predict_adj_em`, then blended:

```
midpoint  = p̄·shrink(ceiling_raw) + (1−p̄)·shrink(floor_raw)
shrink(r) = 0.80·baseline + 0.20·r + 2.0
```

- **0.80 baseline weight + 2.0 offset** — tuned on a 496-team-year backtest of the whole pipeline against actual 2025 + 2026 AdjEM. The roster model is a same-season *descriptive* model; as a *projector* it is both noisy and biased low (raw MAE 9.97, bias −4.8) — last year's AdjEM alone is the better predictor (MAE 6.53). The blend leans on the baseline accordingly; the `+2.0` offset zeroes the blended pipeline's bias. Final pipeline MAE **6.23**, beating baseline-persistence; the MAE curve is flat over weight 0.75–0.82. An earlier continuity-weighted scheme (baseline weight scaled by returning-minutes share) was tried and reverted — the backtest showed it shifted weight away from the better predictor and widened the top-team error.
- **p̄** is the mean probability the team's uncertain (declared-draft) cohort returns, from the Tankathon mock board: pick ≤30 → 0.05, 31–60 → 0.50, off-board → 0.85. It replaces a flat 50/50 floor/ceiling average, which over-penalized draft-talent-heavy (i.e. top) teams.

**Re-tuning playbook**: temporarily set `SHRINK_WEIGHT = 0.0` / `PROJECTION_OFFSET = 0.0` in `routes/projections.rs`, capture `/api/projections/2025` and `/api/projections/2026` (their `midpoint_adj_em` is then raw model output), join to actual `team_season_stats.adj_efficiency_margin`, and grid-search `(weight, offset)` to minimize pooled MAE.

## Limitations and upgrade paths

- **Tier-mean is population average, not per-player.** A 5★ who busts and a 5★ All-American both project as +8.97 CamPom. The Phase 6 freshman-impact prior model is the upgrade.
- **T4 includes everyone the 247 composite ranking doesn't reach.** True walk-ons (high school stars who walked on at a high-major) are projected with the same profile as low-major freshmen with light recruiting. The minute share they actually get is governed by the roster model's minutes-weighted aggregation — a heavy T4 cohort will see their MPG cap their team-level contribution naturally.
- **Returning players use frozen prior-season rate stats.** `project_rotation` re-casts each player's *minutes* (and rescales counting stats to match), but the efficiency/usage profile is still last season's — no growth model. The Phase 5c trajectory model predicts next-season `cam_v3` but the box-score roster model can't consume it (it's box-score-only by design). The Phase B impact-aggregation model — which scores a roster from aggregated projected `cam_v3` — is the path to making returner growth and freshman upside count. See ROADMAP §5b "Projection bias fix".
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
