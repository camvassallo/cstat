# Archetype balance ≠ team strength — investigation and decision

**Status**: investigated 2026-05-27. A v1 roster-fit chip shipped to
TransferPortal in PR #89; a v2 successor was built in the same branch but
never deployed. Both were removed after this analysis. Archetypes remain a
*description* layer on TeamDetail; they are no longer used as a *scoring*
signal anywhere on the site.

## Background

Phase 5b added a "Fit" column to the TransferPortal page that scored each
candidate's archetype against the destination roster's archetype distribution.
- **v1** (PR #89): baseline = destination's source-season distribution.
- **v2** (this branch, pre-revert): baseline = destination's projected
  next-season distribution, with the candidate's own contribution
  subtracted out for marginal-effect framing.

Both versions encoded the same prior: **balance is good, density is bad.**
A team over-indexed in Wizards penalized incoming Wizards; a team missing
a Cleric promoted incoming Clerics. That prior was never empirically
tested before shipping.

## The test

`training/validate_archetype_balance.py` runs across 12 seasons of D-I
(2015–2026, n=4,216 qualified team-seasons with ≥80% CamPom coverage).

**Q1 — Per-archetype mean CamPom (minutes-weighted, qualified players):**

| Class | Mean CamPom | n |
|---|---:|---:|
| Druid | **+4.66** | 3,232 |
| Sorcerer | +4.41 | 3,516 |
| Wizard | +4.31 | 3,099 |
| Paladin | +3.33 | 1,833 |
| Rogue | +2.04 | 2,166 |
| Monk | −0.28 | 3,496 |
| Bard | −0.90 | 4,007 |
| Warlock | −0.92 | 3,643 |
| Barbarian | −1.39 | 3,034 |
| Cleric | −2.83 | 2,665 |
| Ranger | −3.15 | 3,519 |
| Fighter | **−3.54** | 2,222 |

There's a real ~8-CamPom spread from best to worst class. "Druid is good,
Fighter is hard to win with" is not stereotype — it's data. The k-means
clustering surfaces archetypes that systematically differ in per-player
value.

**Q2 — Does balance predict overperformance vs. the talent identity?**

The player-impact identity says `AdjEM ≈ Σ(cam_v3 × minute_share)`. We
verified: `r(talent_identity, AdjEM) = 0.978` (R² = 0.957). 95.6% of
AdjEM variance is the per-player CamPom sum.

We then asked whether the *residual* (AdjEM − talent identity) correlates
with balance metrics:

| Metric | r with residual | Direction |
|---|---:|---|
| `max_share` | **+0.114** | More concentration → better |
| `gini` | +0.113 | More inequality → better |
| `eff_classes` | −0.077 | More balance → worse |

All three correlations are mild but consistently the **opposite** sign
from what the chip assumed.

The decisive cut is the quadrant test on dominant-class value × concentration:

| Dominant class | Concentration | n | Mean residual |
|---|---|---:|---:|
| Low-value | Low | 1,135 | −6.77 |
| Low-value | High | 1,064 | **−9.94** ← worst |
| High-value | Low | 973 | +2.62 |
| High-value | High | 1,044 | **+8.48** ← best |

Concentration **amplifies** the dominant-archetype effect. Stacking
high-value classes (Druid/Sorcerer/Wizard) at high concentration produces
the largest positive residual; stacking low-value classes
(Cleric/Ranger/Fighter) at high concentration produces the largest
negative residual.

Within-quartile correlation of `residual ~ max_share`:
- Q1 (dominant is low-value): r = **−0.20** — concentrating in a weak class makes it worse.
- Q3 (dominant is high-value): r = **+0.38** — concentrating in a strong class makes it better.

Per-dominant-class mean residual (sorted by class value):

```
Sorcerer  +8.27   Wizard  +1.88   Druid    +2.78   ← positive
Paladin   +7.52   Rogue   +3.60
Monk      −2.96   Bard    −8.64   Warlock  −5.48   ← negative
Cleric   −10.84   Ranger −12.48   Fighter −10.36
```

A team dominated by Sorcerers beats its talent-identity prediction by
8.27 AdjEM on average; a Ranger-dominated team loses to it by 12.48.

## Why the chip was wrong

The v1/v2 fit chip used class-agnostic balance: "you're stacked in X →
bad, regardless of which X." The data says the chip should have been
class-aware:

- "Stacks Druid rotation" → genuinely positive
- "Stacks Fighter rotation" → genuinely negative
- "Fills missing Druid" → positive
- "Fills missing Fighter" → mostly neutral; you don't need a Fighter

A class-aware chip would have looked very different from what shipped.
At that point the chip stops being "balance scoring" and starts being
"a categorical, low-resolution version of CamPom" — which is worse than
CamPom on every axis (less precise, harder to interpret, more error
modes).

## Decision

**Rank transfers by projected CamPom**; that's the cleanest value
signal we have and the trajectory model already produces it per-player.
Archetypes are kept as a *visualization* layer:

- TeamDetail's "Roster Archetypes" panel (Identity / Gaps) — *describes*
  what a team is concentrated in without making a value claim.
- The upcoming 12-axis radial roster plot + Team Compare view
  (Phase 5b) — same descriptive role.

The fit scoring helpers (`cstat_core::roster_fit::{compute_fit_score,
fit_score_against_projected, build_projected_class_minutes}`) and the
distribution queries (`queries::{get_team_archetype_index,
get_archetype_distributions_for_teams, get_d1_archetype_shares}`) stay
in cstat-core. They were proven correct for what they measure
(archetype-balance scoring); the finding is that archetype balance
isn't the thing we should be measuring. If a future visualization
surface needs e.g. per-team archetype distribution comparisons, the
plumbing is ready.

## On using data-derived roles to predict performance

Worth naming as a methodological point: archetypes are k-means clusters
over the same player rate stats that feed CamPom and the trajectory
model. Routing rosters through a categorical bottleneck and then
scoring is *strictly worse* than scoring over the continuous features
directly. The roster-impact model confirmed this — including
archetype-share features on top of `Σ(cam_v3 × share)` contributes
marginal lift but no foundational signal.

Roles earn their keep through interpretability ("Duke has 3 Wizards" is
a sentence a coach can act on) and visualization (radial plots,
distribution chips). They lose when they pretend to be a scorer.

## Caveat

The dominant-class-value effect is partially confounded with school
quality: low-value archetypes are disproportionately rostered by
weaker D-I programs (the Clerics/Fighters/Rangers tend to play at
sub-major schools), so some of the −12 residual on Ranger-dominated
teams is "school-level underperformance the per-player ratings don't
capture" rather than pure archetype causation. The directional finding
(concentration in high-value classes helps; concentration in low-value
classes hurts) survives the confound, but the magnitudes should be
treated as upper bounds.

## Reproducing

```bash
cd training
DATABASE_URL='postgresql://cstat:cstat@localhost:5432/cstat' \
    python3 validate_archetype_balance.py
```

Outputs the per-archetype value table, the residual-vs-balance
correlations, the dominant-class-value quartile breakdown, and the
quadrant test. Rerun whenever the archetype model is retrained — class
quality could drift if rule changes or era effects shift player styles
(see `archetypes_methodology.md` "Era horizon").
