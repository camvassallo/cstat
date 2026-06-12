# RAPM: Regularized Adjusted Plus-Minus — Design

**Status: COMPLETE 2026-06-12. REJECTED as a standalone value metric (§8);
the narrowed display scope was approved and SHIPPED the same day as
"Adj on/off (RAPM)" (§8.1) — `player_rapm` table, `training/rapm.py`, the
`replay_shadow` compute change, API fields, and the PlayerDetail companion
line. The trajectory-slot test also resolved: both add and swap variants
rejected (§8.2) — the raw on/off block keeps its contract slot.** This is
the Tier-3 membership item from `docs/pbp_utilization_scope.md`, and the
merged form of two ROADMAP items: "Adjusted +/- (RAPM)" (PBP
feature-incorporation list) and "Native cstat player impact metric" (folded
in — same goal, stint resolution made it tractable).

## 1. Why RAPM

Raw on/off is a contextual team-result stat, not an individual-skill stat: a
player's OFF sample is "whatever happened to the team while he sat," which
conflates his teammates, his opponents, and *when* he plays. The shipped
`player_on_off` panel makes the failure vivid — **Zuby Ejiofor (St. John's
2026) posts CamPom +20.8 but on/off −8.4** because the team was +22 when he
sat: a deep, dominant team's bench pads its net rating in garbage time,
dragging every starter's off-court number down (the whole starting core reads
negative while the bench reads positive).

The literature's fix is regularized adjusted plus-minus: regress per-stint
point margin on the on-floor indicator vector for *all ten players*, with an
L2 (ridge) prior shrinking coefficients toward a prior. Each coefficient is
the player's partial effect *holding teammates and opponents constant* — the
thing raw on/off cannot do.

Two priors sharpen the case for building it now:

- **The Tier-2 split verdict (2026-06-11):** player-level membership signal is
  real (prior-season on/off shipped into the trajectory model — the first
  positive PBP-feature verdict), while team-level membership summaries are
  absorbed by existing features. RAPM is player-level by construction.
- **The roster-projection maturity finding (ROADMAP §2):** the preseason
  pipeline is information-limited, not model-limited. New signal has to come
  from new data — stint-level attribution is exactly that.

## 2. What RAPM is (and is not) in cstat

**v1 is a descriptive, display-grade metric** — "what is this player's
on-court value with teammates and opponents controlled" — in the same spirit
as CAE: computed offline, persisted, surfaced in the UI, **not** wired into
any served model.

It is *not* a predictor, and it does not replace CamPom. The path into ML
(e.g. `prior_rapm` as a trajectory feature alongside the shipped on/off
block) goes through the established experiment-harness → backtest-verdict →
contract-change pipeline as its own later PR, and only if a held-out win
materializes. Tier-1 taught us importance is not value; on/off taught us the
verdict can be positive. RAPM gets the same trial, after it exists.

## 3. Data substrate

RAPM consumes `lineup_stints` rows where **both** lineups are full 5-man sets
and the stint has possessions. Current inventory (2026-06-12, mid-backfill):

| Season | Usable paired stints | Possessions | Dominant source |
|--------|---------------------:|------------:|-----------------|
| 2015 | 158,183 | 366k | replay |
| 2016 | 184,211 | 437k | replay |
| 2017 | 203,873 | 488k | replay |
| 2018 | 180,430 | 477k | replay |
| 2019 | 0 | 0 | — (corrupt-replay gate; see §3.3) |
| 2020 | 173,655 | 395k | replay |
| 2021 | 162,253 | 381k | replay |
| 2022 | 229,918 | 543k | replay |
| 2023 | 228,618 | 543k | replay |
| 2024 | 213,899 | 513k | replay |
| 2025 | 140,848 | 342k | replay |
| 2026 | 180,373 | 505k | onfloor |

("Usable" = `array_length(lineup,1)=5 AND array_length(opp_lineup,1)=5 AND
possessions_for > 0`.) ~4,900 distinct players appear in a season's usable
stints, so a per-season design matrix is roughly **180k rows × 9,800 columns**
(O and D coefficient per player), ~10 nonzeros per row — comfortably sparse.

Three substrate facts drive the design:

### 3.1 Opponent pairing exists for all replay seasons

The SUB-replay reconstruction emits both sides: 70–76% of replay stints carry
a full 5-man `opp_lineup` (onfloor 2026: 71%). RAPM is therefore **not
blocked on the lineups-object backfill or on onfloor exactness** — every
season 2015–2026 except 2019 has a usable paired corpus today. Note this also
means RAPM covers 2015–2018, eras the Tier-1 contextual tags could not.

### 3.2 The natstat source swap removes paired stints — RAPM needs a shadow

`natstat_lineups`-sourced synthetic stints are per-game aggregated units with
**no opponent lineup and no clock** — they are valid for the served
aggregates/on-off but are *not* RAPM observations. And the coherence-gated
swap in `compute_pbp_lineups` **skips replay entirely** for covered
team-games (zero double-count, by design). Today that's negligible (39
team-games in 2020), but the backfill's coherent middle era passes the gate
at high rates (2020 ~78% of sampled team-games), so post-backfill recomputes
would silently evaporate a large fraction of the mid-era paired corpus.

**Decision: emit replay rows for covered team-games under a shadow source
label** (`source = 'replay_shadow'`) instead of `continue`-ing past them.
The served aggregates and on/off ignore the shadow label (no double-count,
exactly as today); RAPM reads `replay | onfloor | replay_shadow`. The rows
are regenerated every compute like the rest of the table, so the cost is
storage only. Alternatives considered and rejected: (a) RAPM drops covered
games — loses most of the best-coverage era; (b) mixed design with
opponent-*team* controls for unpaired natstat units — a second, weaker
observation model inside one regression; revisit only for 2019 (§3.3).

### 3.3 2019 stays out of v1

2019's PBP feed is corrupt for replay (the standing gate) and has no paired
alternative; its natstat units (when the backfill reaches it) are unpaired.
v1 fits 11 seasons and leaves 2019 absent — same posture as `player_on_off`.
A 2019-only variant with opponent-team fixed effects is a possible follow-up,
clearly labeled, never silently mixed.

### 3.4 Possession scale

`possessions_for/against` are float estimates on the box-estimate scale
(replay/onfloor count directly; natstat-sourced rows are rescaled — but those
aren't RAPM rows anyway). Stints are short (mean 1.2–2.1 possessions), which
is fine: observations are possession-weighted, and identical
(game, lineup, opp_lineup) rows can be pre-collapsed for solver speed without
changing the weighted solution.

## 4. Model specification

### 4.1 Observations

Each paired stint contributes **two offense rows**, one per side:

- response `y` = points scored per 100 offensive possessions
  (`100 * points_for / possessions_for`)
- weight `w` = `possessions_for`
- columns: `+1` on each of the 5 offensive players' **O** coefficients,
  `+1` on each of the 5 defensive players' **D** coefficients,
  an intercept (league scoring level, ~100), and a home-offense indicator
  (HCA; 0 on neutral floors).

Conventions: `O_j` = points per 100 added on offense (higher better);
`D_j` = points per 100 *allowed* while j defends (lower better);
`net RAPM_j = O_j − D_j`. This two-block formulation gives ORAPM/DRAPM
natively — the split raw on/off can't isolate.

### 4.2 Regularization — the prior IS the method

Weighted ridge: minimize `Σ w_i (y_i − x_i·β)² + λ‖β − β₀‖²` (intercept and
HCA unpenalized).

- **v1 baseline: β₀ = 0** (pure RAPM). Honest, assumption-free, the standard
  reference point.
- **v2 candidate: box-informed prior** (BBR-style) — β₀ centered on a CamPom
  decomposition: cam_o/cam_d splits exist at every CamPom tier and GBPM is
  already on a per-100 scale, so the prior costs no new modeling. Ship only
  if it beats the zero-prior on the §6 acceptance suite; report both during
  evaluation.
- **λ tuning:** game-blocked cross-validation (hold out whole games, never
  stints within a game — stints within a game share lineups and context),
  scored on held-out weighted stint-margin MSE. Log-grid sweep (λ ∈
  ~[10², 10⁴] on the per-100 scale); report the sensitivity curve, not just
  the argmin — RAPM rankings should be stable across a λ neighborhood, and
  instability is itself a red flag.

### 4.3 Dimensionality hygiene

- **Replacement pooling (decide in the spike):** players below a possession
  floor (~50 paired possessions) optionally collapse into one per-team
  "replacement" column. Ridge alone handles them (they shrink to the prior),
  so this is a noise/interpretability lever, not a correctness one.
- **Garbage time:** none in v1. RAPM already attributes blowout bench minutes
  *to the bench players against the opposing bench* — the mechanism that
  poisons raw on/off mostly disappears under opponent control. If the Zuby
  test (§6) still fails, add a leverage down-weight as v1.1, not a row drop.
- **Per-season fits.** Matches every season-scoped table; players are
  `players.id` (season-scoped UUIDs). Cross-season evaluation joins via
  `natstat_id` OR `torvik_pid` (the dual-key rule —
  `training/test_cross_season_joins.py`). Multi-season pooled fits (shared
  coefficients, more sample) are a later experiment, not v1.

### 4.4 Solver

`scipy.sparse` CSR + a weighted ridge solve (sklearn `Ridge` accepts sparse X
and sample weights; lsqr as fallback). At 360k rows × 9.8k columns this is
seconds per (season, λ) — the full grid over 11 seasons is minutes. No Rust
solver work: like archetypes, **the fit lives in Python** (`training/rapm.py`),
and Rust/API only ever read the persisted table.

## 5. Persistence and serving

New table `player_rapm` (one row per season × player), migration-added:

```
season, player_id, o_rapm, d_rapm, rapm, possessions (paired sample),
stint_count, lambda, prior ('zero' | 'campom'), fitted_at
```

Same access pattern as `player_on_off`: computed offline by
`training/rapm.py`, swapped atomically, read by the API. UI surface (Phase 4,
conditional on acceptance): a RAPM line in the PlayerDetail on/off panel and
an optional leaderboard column — always displayed *next to* raw on/off, since
the divergence between them is the interesting story. Display gate: a minimum
paired-possession floor (~250, tune on the noise profile) so single-stint
players don't headline; the table keeps all rows, the UI filters.

## 6. Acceptance criteria

From the ROADMAP item, made concrete. RAPM v1 ships to display only if:

1. **Stability** — among returning players with ≥500 paired possessions in
   consecutive seasons (dual-key join), year-over-year Spearman ρ of net RAPM
   **beats raw on/off swing** and is at least competitive with CamPom.
2. **External sanity** — top-25 net RAPM (rotation-level floor) overlaps AP
   All-Americans / KenPom POY-tier players at a rate comparable to CamPom's
   top-25; no inexplicable nobody at #1.
3. **The Zuby test** — St. John's 2026's starting core stops reading negative
   once teammates/opponents are controlled; bench inflation collapses.
4. **Held-out signal** — game-blocked held-out stint-margin MSE beats (a) the
   intercept+HCA-only model and (b) a team-strength baseline (AdjEM diff),
   confirming player coefficients carry real information rather than
   redistributing team strength.

Failing 1 or 4 kills the metric (write the verdict, keep the harness — the
Tier-1/Tier-2 pattern). Failing only 3 means revisiting the leverage weight
(§4.3) before judging.

## 7. Known risks

- **Replay membership noise.** SUB-replay is a reconstruction (~86% game
  fidelity); membership errors become regression noise on specific players.
  Mitigation: it's the same substrate the accepted on/off features were built
  on; and 2026's onfloor-exact corpus gives one clean season to compare noise
  profiles against (a replay-vs-onfloor divergence check on 2026 is not
  possible — onfloor displaced replay there — but coefficient-noise
  diagnostics by source-era are).
- **Collinearity.** Players who always share the floor (or never overlap with
  the starters) are weakly identified; ridge handles it by shrinking toward
  the prior, which with the CamPom prior re-introduces box dependence.
  Report per-player coefficient stability (jackknife over game blocks) so
  the UI can carry an uncertainty cue if needed.
- **One-season players / low samples** are display-gated, not model-dropped.

## 8. Spike verdict (2026-06-12) — REJECTED as a standalone value metric

The step-2 spike ran (`training/experiment_rapm_spike.py`; summaries
`rapm_spike_2026_20260612` and `rapm_spike_stability_20260612` in
`training/eval_history/`). Both kill-gates failed as written:

- **Gate 4(b) FAIL.** Game-blocked held-out stint MSE: player RAPM 5156.7
  (zero prior, λ=1000) / 5152.5 (CamPom prior, λ=2000, fitted prior scale
  0.19) vs **5147.0 for a team-columns ridge** on the same folds (the fair
  baseline — the doc's original AdjEM-diff baseline is leaky and is reported
  as reference only, 5119.2). Single-season player-level allocation does not
  out-predict team strength, which is where the literature said it would land.
- **Gate 1 SPLIT → FAIL.** YoY Spearman among ≥500-possession returners
  (dual-key join): net RAPM **decisively beats raw on/off swing** — zero
  prior +0.241 vs +0.112 (2024→2025, n=1,639) and +0.163 vs +0.047
  (2025→2026, across the replay→onfloor source change, n=1,539); the CamPom
  prior variant reads +0.267/+0.242 but inherits the prior's own stability.
  Against the "competitive with CamPom" clause it is not close: CamPom
  ρ = +0.766/+0.716.
- Gates 2 and 3 passed: the prior-variant top-25 is sane (15/25 in CamPom's
  top-50, Spearman vs CamPom +0.72), and the **Zuby test passes emphatically**
  — Ejiofor raw on/off −10.6 → net RAPM +4.8 (+2.0 zero-prior), all five
  St. John's core players flip positive, the bench inflation collapses.

**Reading:** the *allocation mechanism works* (gate 3, and the 2–5× stability
edge over raw on/off — the stat it was scoped to fix), but one college season
of stints cannot identify ~4,900 players to value-metric precision. RAPM must
not ship as a CamPom rival, a player-value grade, or an ML feature.

**Re-test triggers:** a multi-season pooled design (shared cross-season
coefficients via the dual key — the standard lever for RAPM stability, listed
in §4.3 as out of v1 scope), or materially better stint data. The lineups
backfill does NOT improve this corpus (natstat units are unpaired, §3.2).

### 8.1 Product decision + ship (2026-06-12): "Adj on/off (RAPM)"

The narrowed scope was approved: per-season RAPM ships strictly as the
*context-adjusted companion line in the on/off display* — its comparison
baseline is raw on/off (which it beats on every measure), not CamPom. Named
"Adj on/off (RAPM)": the plain phrase tells a casual user what it is in the
panel's context, the suffix gives the literature-precise term; the method IS
canonical RAPM, so no cstat-novel coinage. **The zero-prior variant ships**,
not the marginally-better-CV CamPom prior — a CamPom-flavored number next to
CamPom on the same page would destroy its value as independent evidence.

What shipped:

- **`replay_shadow`** (`compute_pbp_lineups`): covered team-games keep their
  replay rows under a rollup-excluded shadow label, so this corpus survives
  the Tier-2 source swap as the backfill lands. Verified on 2020: all three
  served surfaces checksum-identical, 1,727 shadow stints covering exactly
  the 39 covered team-games; possession-parity suite green.
- **Migration 038 `player_rapm`** + **`training/rapm.py`** (zero prior,
  λ=1000): 52,421 rows across 11 seasons (~4.6–5.0k players each; 2019
  legitimately absent). Intercepts track league scoring eras (96–107),
  HCA +4 to +7 per 100. Not in `sync_to_prod.sh`'s exclusion list, so it
  ships to prod with the other rollups.
- **API**: `get_player_on_off` LEFT JOINs `player_rapm`; the on-off route
  carries `rapm_o` / `rapm_d` / `rapm_net` / `rapm_paired_possessions`.
- **UI**: an "adj on/off (RAPM)" line in the PlayerDetail on/off panel
  (≥250 paired-possession display floor), with the panel's team-result
  caveat updated to point at it. The TeamDetail roster column **replaces**
  raw on/off with "Adj On/Off" (same floor; sorts by the displayed value);
  the raw swing and its breakdown move into the cell tooltip. The Players /
  Transfers grids keep raw on/off for now.

### 8.2 Trajectory-slot test (2026-06-12): REJECTED, on/off keeps the slot

The natural follow-on question — the shipped trajectory on/off block is raw
on/off, and RAPM is 2–5× more stable year-over-year; is it the better
*feature*? No (`training/experiment_trajectory_rapm.py`, RAPM coverage 91%
of paired rows vs 89% on/off): **swapping** the on/off block for RAPM is
decisively worse (pooled LOPO MAE 2.1282 → 2.1382, −0.0100, 1/11 pairs) —
raw on/off's team-context "contamination" is *signal* for projecting
next-season CamPom, which is itself team-contextual. **Adding** RAPM on top
(51→54) reads +0.0009 pooled (8/11 pairs) — ~6× smaller than the on/off
block's accepted win and inside noise; not worth a contract change. Verdict
recorded in `eval_history/trajectory_rapm_experiment_20260612_summary.json`.
Stability and feature value are different questions; the harness answered
the second one.

## 9. PR plan (as executed)

1. **This design doc** + ROADMAP pointer. *(Done.)*
2. **Solver spike** — `training/experiment_rapm_spike.py`: 2026-only fit
   (onfloor corpus, zero prior), λ sweep with game-blocked CV, acceptance
   metrics 2–4 on the single season, Zuby test. *(Done — the §8 verdict;
   plus a `stability` mode for gate 1 across 2024–2026.)*
3. **Production v1** — the `replay_shadow` compute change (§3.2),
   `training/rapm.py`, the `player_rapm` migration, 11-season fit. *(Done,
   under the narrowed §8.1 scope — the §6 evaluation happened in the spike
   and set that scope.)*
4. **UI surface** — PlayerDetail panel line. *(Done; the leaderboard column
   was dropped — a value-metric surface would contradict the §8 verdict.)*
5. **ML trial** — `prior_rapm` vs the shipped on/off block in the trajectory
   harness. *(Done — REJECTED both ways, §8.2; the contract stays 51.)*

All five steps landed in one PR on 2026-06-12; none waited on the lineups
backfill, and step 3's shadow change exists precisely so the backfill's
continued landing never degrades the RAPM corpus.
