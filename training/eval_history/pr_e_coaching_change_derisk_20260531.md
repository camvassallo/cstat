# PR E de-risk — coaching-change indicator: REFUTED as a point-estimate feature

**Date:** 2026-05-31
**Script:** `training/derisk_coaching_change.py`
**Data:** `coachdict.json` (barttorvik) × `projections_backtest_per_team_5season_20260529.json` (#96, 1,326 team-seasons, 2022–2026)
**Gate:** ROADMAP §6 PR E / "(3a) New-signal hunt — coaching-change indicator". Per the
established discipline (the same gate that killed PR B), measure lift before building.

## TL;DR

A coaching change carries a **statistically significant but tiny and undirected** effect on
the roster-projection residual. The optimistic **in-sample** best-case global MAE lift is
**−0.0019** (it slightly *hurts*). **Do not build it as a point-estimate feature for the
projection model.** The only defensible use is **uncertainty bands** (new-coach teams are
~1.12× noisier). The de-risk also **debunked the roadmap's own anchor cases**.

## Method

- Offseason change flag = `coachdict[Y][team] != coachdict[Y-1][team]`. Join is clean:
  361/362 backtest team-names match coachdict directly; one alias
  (`Texas A&M Corpus Christi` → `Texas A&M Corpus Chris`). 0 team-seasons dropped.
- Served projection = `0.5·baseline + 0.5·phase_b` (matches `projections_backtest.rs:239`).
- Residual = `actual − served` (signed; + = team beat projection).

## Findings

### 1. Significant but small — and it's variance, not bias

| Group | n | MAE | resid σ | bias |
|---|---|---|---|---|
| changed coach | 218 (16.4%) | **6.53** | **8.05** | −0.38 |
| unchanged coach | 1108 | 5.78 | 7.19 | +0.06 |

- |residual| difference +0.75 MAE, Welch t=+2.19, **p≈0.028 (significant)**.
- Residual σ ratio **1.12×** — changed-coach teams are *noisier*, not biased. Signed bias is
  near zero for both groups (−0.38 vs +0.06): a coaching change makes a team harder to
  predict in **both directions**, it does not shift the point estimate systematically.

### 2. Direction is only weakly predictable (excess mean-reversion)

- `corr(served, residual | changed) = −0.135` vs `corr(... | unchanged) = −0.005`.
  The baseline projection is essentially mean-calibrated (r≈0); a coaching change induces
  **excess reversion of −0.13**: weak teams hiring a new coach bounce up (Q1 weakest bias
  **+2.47** — rebuild hires: McNeese+Will Wade +26, New Mexico+Pitino +15, UNLV+Kruger +14),
  strong teams replacing a coach drift down (Maryland 2026 Willard→Buzz −20.7, LSU 2023 −19.8).
- But r=−0.135 ⇒ r²≈0.018: the direction explains <2% of changed-coach residual variance, on
  16% of the pool. Far too weak to move a global point estimate.

### 3. Optimistic MAE ceiling ≈ 0 (slightly negative)

Applying the **in-sample** best linear correction (`resid ~ a + b·served`) to changed-coach
teams and recomputing global MAE:

| | global MAE |
|---|---|
| before | 5.9027 |
| after best in-sample correction | 5.9046 (**−0.0019**) |
| boolean mean-shift only | (**−0.0016**) |

In-sample is an upper bound; out-of-sample would be worse. This is the same pattern the
roadmap already established (recalibration is a dead lever; ~23% of team-AdjEM variance is
preseason-invisible). **A coaching-change point feature is below the noise floor.**

### 4. The roadmap's anchor cases were FALSE (the most important deliverable)

The ROADMAP PR E text and `preseason_audit_20260529.md` claimed *Auburn 2025 / Missouri 2025 /
Maryland 2025 / Florida 2025* were "coaching-turnover signatures." **4 of those 5 had no
coaching change:**

| team | season | changed? | resid | coaches |
|---|---|---|---|---|
| Auburn | 2025 | **no** | +15.9 | Bruce Pearl → Bruce Pearl |
| Missouri | 2025 | **no** | +20.4 | Dennis Gates → Dennis Gates |
| Maryland | 2025 | **no** | +15.2 | Kevin Willard → Kevin Willard |
| Florida | 2025 | **no** | +16.1 | Todd Golden → Todd Golden |
| Maryland | 2026 | yes | −20.7 | Kevin Willard → Buzz Williams |

The unexplained **+upside cluster** the audit attributed to coaching is **returning-coach
overperformance** (program momentum / roster talent the projection underrates), NOT coaching
turnover. The new-signal hunt should stop attributing that residue to coaching changes.

## Verdict & recommendation

- **REFUTE PR E as a point-estimate feature.** No retrain; the optimistic ceiling is ~0.
- **Salvage value — uncertainty bands (cheap, honest):** new-coach teams are 1.12× noisier
  with a weak rebuild-hire-bounce. Widen the floor/ceiling band for `new_hc` teams on the
  projections/Future tab, and add a "new coach — higher uncertainty" tint. This is the §6
  "(4) Projections UI regression caveat" family, not a model change.
- **Redirect the new-signal hunt (3):** with (3a) coaching-change spent, the remaining levers
  are (3b) real minutes/role projection and (3c) returning-continuity/chemistry. The debunked
  anchors suggest the unexplained Q4 upside is a *returning-coach/program* effect, worth a
  separate look (coach-tenure or program-fixed-effect), distinct from the change indicator.
- **Coachdict still earns its keep** for the descriptive **coach-above-expectation** roadmap
  item (Phase 6) — the residual *is* the coach metric; this de-risk just says the change-flag
  doesn't sharpen the *projection point estimate*.

---

## Round 2 — larger lens + coach-quality hypothesis (2026-05-31)

Pushback: pooled change-flag nets to zero because upgrades (McCollum → Iowa) and
downgrades (Buzz leaving Maryland) cancel — maybe the signal is coach **quality**, not
the boolean change; and maybe 2026 (McCollum/DeVries/Wade/Willard moves) is special.
Tested both. Script: `training/derisk_coach_quality.py`.

**(a) 2026 is not special — year-to-year bias is unstable noise.** Changed-coach signed
bias by season: 2022 **+1.28**, 2023 **−3.63**, 2024 −0.57, 2025 +0.45, 2026 **+0.54**.
The −3.6→+1.3 swing is the noise signature; 2026's +0.54 is ordinary (good hires beating
projection are offset by Maryland −20.7). More seasons would confirm ~0, not reveal a trend.

**(b) Coach-quality hypothesis FAILS.** For the 64/218 changes whose incoming coach has a
prior D-I season in the 2022–2026 window, `corr(incoming coach prior CAE, new-team residual)
= −0.061, r²=0.004` — essentially zero, wrong sign. Counter-examples dominate: Matt McMahon
+15.1 prior → LSU 2023 **−19.8**; Pitino +10.1 → Xavier 2026 −3.4; Lamont Paris +10.1 →
South Carolina 2023 −12.4. For every McCollum (+9.6) that carries over, an equally-pedigreed
hire craters. Caveat: "prior CAE" is a noisy (mostly n=1), outcome-conditioned proxy for
quality — garbage-in — so this refutes the *naive* quality feature, not a properly-built
multi-year shrunk coach rating.

**(c) Auburn 2026 not in the backtest** (only 2022–2025, all Bruce Pearl); the Steven Pearl
case is untestable here, and the 2026 slice is slightly thin (41 changes, Auburn absent) —
a coverage caveat on the 2026 cohort.

**(d) Sample size is not the binding constraint.** n=218 pooled is adequately powered for the
bias question (SE on the signed bias ≈ 0.55, so a ~1-pt directional bias would show). The
binding fact is that the effect is variance-not-bias; expanding to pre-2022 would need the
roster-projection backtest regenerated on degraded (pre-2021) transfer coverage, lowering
projection quality and muddying the test — poor trade.

### Revised recommendation

Both the boolean change-flag **and** a naive coach-quality proxy are dead as projection-model
features. The *only* version of "coach signal" worth pursuing is the **Phase 6 descriptive
multi-year coach-above-expectation rating** — built with real history + shrinkage as a STABLE
coach quality estimate — and even then it must clear the same lift gate before touching the
projection point estimate. Sequence: build the descriptive rating first (it stands on its own
as a product surface), then test predictive lift; do not bolt a 1-year proxy onto the model.
