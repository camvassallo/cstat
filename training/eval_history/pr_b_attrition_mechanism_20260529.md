# PR B mechanism check — trajectory regression on thin rosters

**Date:** 2026-05-29
**Script:** `training/diagnose_trajectory_attrition.py`
**Artifact:** `training/eval_history/trajectory_attrition_20260529_summary.json`

## Question (the ROADMAP gate)

> *"Decompose trajectory OOF bias by (prior-team-attrition-quartile,
> prior-campom-bucket) to confirm the mechanism before picking an
> approach."*

The PR B hypothesis (from `decompose_projection_error.py`): the Q1 team
over-projection (+5.62 upstream bias) is driven by the **trajectory model
projecting the surviving returners of high-attrition (thin) rosters closer
to their class-year-archetype average than to their actual modest current
selves**. If true, a roster-attrition feature (approach a) should fix it.

## Method

- **Attrition** of a team-season = `1 − retained_pos_cam / total_pos_cam`,
  where "retained" means the same `torvik_pid` plays for the program
  (resolved via `teams.natstat_id`) the next season. Positive-clamped
  cam_v3, so the ratio reads as *share of productive talent lost*.
- **Player cut:** every persisted trajectory OOF (LOPO held-out)
  prediction joined to its player's season-N team attrition + prior CamPom
  + actual N+1 CamPom. `bias = pred − actual` (+ = over-projected). 22,116
  rows after the attrition gate (17,213 returners / 4,903 transfers).
- **Team cut:** the projections backtest dump (`phase_b − actual`) joined
  to base-roster attrition (433/495 teams matched).

## Result — mechanism **NOT** confirmed

### Player level: trajectory OOF bias is ≈0 across all attrition quartiles

Returners-only, mean bias by (prior CamPom × attrition quartile):

```
attr_q    Q1 low(≤0.21)  Q2(0.21–0.50)  Q3(0.50–0.82)  Q4 high(>0.82)
<0            +0.07         -0.44          -0.42           +0.17
0..+5         +0.42         -0.22          -0.24           +0.02
+5..+10       +0.47         -0.15          -0.69           -0.21
>=+10         +0.17         -0.56          -0.96           +3.50  (n=1)
```

The hypothesis predicts **rising positive bias as attrition increases**
within each CamPom band (the thin-roster returner getting pulled up toward
the archetype mean). The data shows the opposite-to-flat: bias is within
±0.5 everywhere, and if anything is *highest at LOW attrition* (the model
nudges up returners on intact rosters) and *near-zero or negative at high
attrition*. The modest-returner cohort the mechanism is about — high
attrition (>0.82) × `0..+5` CamPom (n=558) — has bias **+0.02**. No
systematic over-projection of thin-roster returners exists to fix.

The only cell with material positive bias is elite (`≥+10`) × very-high
attrition, at `+3.50` on **n=1** — noise, and in any case the elite-tail
regression is already documented (`mae_by_current_campom`, PR C territory)
and drives Q4 (top teams), not Q1 (bust teams).

### Team level: bust teams are over-projected regardless of attrition

(Numbers below regenerated on the 5-season / 1,326-team dump with the fixed
bust-split; see `trajectory_attrition_20260530_summary.json`.)

```
Bust teams (bottom actual quartile): n=292  bias +7.33  mean attrition 0.67
  corr(attrition, err) within busts = −0.05
  bust + KEPT talent (attr<0.5):  n=92   bias +7.50
  bust + LOST talent (attr≥0.5):  n=200  bias +7.25
```

The killer slice: bust teams that **kept** most of their talent (attrition
< 0.5) are over-projected by **+7.50** — statistically identical to the
teams that gutted their roster (+7.25) and to the pooled bust cohort
(+7.33). The within-bust correlation between attrition and over-projection
is **−0.05** (zero). Roster attrition has **zero discriminative power** for
the team-level over-projection. (>50% of bust teams have attrition = 1.0 —
they retained ~zero positive cam_v3 — so the cohort can't be split on a
median; the fixed-threshold "kept vs lost talent" split plus the
correlation is the honest comparison.)

## Conclusion

The trajectory model is **per-player well-calibrated** (bias ≈ 0, MAE ≈
2.0–2.3) including for the exact cohort PR B targets — modest-CamPom
returners on high-attrition rosters. The roadmap's hypothesized mechanism
is **refuted**. Approaches (a)/(b)/(c) all modify the trajectory model;
none can move a bias that isn't there.

The Q1 over-projection lives **upstream of the trajectory model**, in
roster **composition** (which players are assumed to fill the rotation and
at what minutes) and/or the **freshman/portal** projections — not in the
trajectory model's per-player accuracy. That bust teams are over-projected
*independent of how much they lost* points at a team-level mean-reversion /
composition failure: the projection assembles a plausible-looking rotation
of qualified contributors, but teams that actually finish at the bottom
tend to have thinner *real* rotations (busts, injuries, walk-on minutes)
than any composition of projected-qualified players can express.

**Recommendation:** do not ship a trajectory attrition feature. Run the
clean composition-vs-value-vs-calibrator attribution before building
anything (below).

---

## Follow-up — clean three-way attribution

**Script:** `training/attribute_q1_overprojection.py`
**Artifact:** `training/eval_history/q1_attribution_20260529_summary.json`

The existing `decompose_projection_error.py` feeds its "oracle" roster the
same *projected* (OOF) cam_v3 as the pipeline (the
`COALESCE(traj.mean, fresh.mean, …)` in `fetch_team_roster`), so its
`upstream` bucket is really pure COMPOSITION and the cam-projection error
hides inside its `calibrator` bucket. Adding a TRUE oracle (actual roster +
**actual** cam_v3) splits the team error into three disjoint pieces:

```
                    total = composition + cam_value + calibrator_floor
  A phase_b          = projected roster + projected cam
  B oracle_proj_cam  = actual roster    + projected cam   (= old "oracle")
  C oracle_actual_cam= actual roster    + actual cam      (true oracle)
  composition      = A − B    cam_value = B − C    calibrator_floor = C − actual
```

Bias by actual quartile (+ = over-projected):

```
  bucket          n    total  composit  cam_val   calib
  Q1 bottom     124    +8.31    +5.62    +3.56    -0.87
  Q2            124    +0.47    +0.36    +0.38    -0.27
  Q3            123    -2.15    -1.46    -2.04    +1.35
  Q4 top        124    -5.03    -2.55    -4.56    +2.08
```

**Findings:**

1. **The calibrator floor is clean.** Given a perfect roster AND perfect
   cam_v3, the calibrator *under*-shoots bust teams (Q1 −0.87) and slightly
   over-shoots top teams (Q4 +2.08) — the opposite sign of the pipeline
   error at both tails. So PR C's calibrator/monotone-constraint work
   **cannot move Q1** (and only partially offsets Q4). The calibrator is
   not the lever.

2. **cam_value is +3.56 at Q1 — but it's outcome conditioning, not a
   fixable model bias.** The attrition check proved trajectory per-player
   bias is ≈0 *pooled*. Conditioned on a team *finishing* in the bottom
   quartile, the players on that roster are ex-post exactly the ones who
   declined relative to projection — and the model has no honest
   prospective feature that says "this player's team will bust." The
   mirror image at Q4 (cam_value −4.56) is the documented elite
   over-performance / under-projection. This is regression-to-the-mean
   conditioned on the realized outcome, not a learnable signal.

3. **composition +5.62 at Q1** is the other half: bust teams ran thinner
   *actual* rotations (busts, injuries, walk-on minutes) than any
   composition of projected-qualified players can express. Also largely
   outcome-conditioned — you can't prospectively know which "qualified"
   projected contributors won't pan out.

**Net:** the Q1 over-projection is dominated by composition (+5.62) and
cam-value (+3.56), both substantially **regression-to-the-mean conditioned
on the realized outcome**, with the calibrator essentially blameless
(−0.87). None of PR B's three trajectory approaches, nor PR C's calibrator
work, can move it without prospective information the pipeline does not
have. The pipeline is **mean-calibrated overall** (headline bias +0.41);
the per-quartile tails are the irreducible variance of team outcomes around
an honest projection.

**Decision:** PR B (and the "Q1 upstream bias closes by ≥2 AdjEM"
acceptance bar) is **refuted as scoped**. Forcing a model change here would
fit noise. Recommend: surface the per-quartile regression honestly in the
projections UI (a "projections regress to the mean — bottom teams trend up,
top teams trend down" caveat, analogous to the existing trajectory
elite-tail tooltip) rather than chasing an unlearnable bias.

---

## Follow-up 2 — calibration test + blend sweep (corrects the record)

The per-quartile bias above buckets by *actual* outcome, which mechanically
induces regression-to-the-mean bias even for a perfect model (regression
fallacy). The honest test is to bucket by *predicted* value:

```
bias by PREDICTED quartile (phase_b raw − actual):
  Q1 (pred −13.7): bias +2.16    Q3 (pred +2.6): bias −1.00
  Q2 (pred  −6.5): bias +1.94    Q4 (pred +19.4): bias −1.48
```

Bucketing on predicted collapses the bias from ±8 to ~+2/−1.5 → most of the
Q1/Q4 "bias" was the analysis artifact, confirming there is no large
fixable conditional bias. The residual monotone pattern (low-pred finish a
bit lower, high-pred a bit higher) is a mild ~11% under-dispersion:
`actual = −0.46 + 1.111·predicted_raw`.

**Correction to the record:** the headline "phase_b MAE 6.45" is the **raw**
roster-impact model. The projections backtest does NOT blend phase_b
(`projections_backtest.rs:239`), but the live `/api/projections` route
serves `0.50·baseline + 0.50·raw`. Measuring what's actually served:

```
raw phase_b            6.45     <- what the dump reports
baseline (persistence) 6.53
phase_a (shipped blend) 6.26
LIVE served (0.50/0.50) 5.92     <- the real served accuracy, never reported
blend sweep optimum    w*=0.50 → 5.92  (already optimal; w∈[0.4,0.6] all ≈5.92)
de-shrink of blend (in-sample) 5.87  (+0.05 only — dead lever)
```

So the served projection is **MAE 5.92, r=0.88 (r²=0.77)** — comfortably
beating persistence (6.53) and phase_a (6.26). The 0.50 blend is already
optimal; de-shrink is a dead lever (the audit was right). All three
predictors share r≈0.88 → **~23% of team-AdjEM variance is preseason-
invisible** (injuries, breakouts, busts, in-season development); that's the
irreducible floor. Lowering it needs NEW signal, not recalibration.

**Feasibility of "more backtest seasons":** the projection backtest covers
only 2 seasons (2025, 2026 = 495 teams) because the roster_impact LOSO
models were only generated for those two. The DATA supports far more:
`transfers` has solid coverage 2021→2026 (621/734/943/1224/1636/1511) and
`recruits` covers class-of-2014→2026. Projecting season S needs
`transfers(S−1)` + recruits(S−1) + trajectory OOF (all seasons) — so
**2022–2026 (≈5 seasons, ~1,800 team-seasons) is reconstructable today**,
3.6× the current sample. That's the prerequisite for testing any new
feature with statistical power.

**Revised recommendation (supersedes "nothing to do"):**
1. Expand the LOSO roster_impact backtest to 2022–2026 — power + robustness.
2. The shippable high-value use is the **preseason × pit blend** (ROADMAP
   §6): the preseason projection (r=0.88) is far more reliable than the
   in-season honest-pit model in November (Army–Duke +1.5 disaster), so it
   is the right early-season anchor.
3. Future r² gains need NEW signal (coaching changes / minutes-role
   projection), not recalibration of existing signal.

---

## Follow-up 3 — 5-season validation (1,326 team-seasons)

Expanded the LOSO roster_impact backtest from 2 seasons (495 teams) to
**2022–2026 (1,326 teams, 3.6×)** — `LOSO_EXPORT_SEASONS` +
`projections-backtest --years` default bumped, calibrator retrained.
Artifact: `projections_backtest_per_team_5season_20260529.json`. Every
finding holds with tighter data:

- **Served (blended w=0.45) projection: MAE 5.90, r=0.88 (r²≈0.77).** Blend
  sweep optimum w=0.45 (live route at 0.50 is within noise of optimal).
- **Calibration in predicted space is near-perfect** — blended bias by
  *predicted* quartile: −0.44 / +1.04 / −0.21 / **+0.02** (top bucket dead
  on); calibration slope **0.994** (the 0.45 blend incidentally fixes the
  raw model's 1.03 under-dispersion almost exactly). The actual-bucket view
  still shows the ±5/±3 artifact → confirms regression fallacy, not bias.
- **Three-way attribution holds** (per actual quartile): Q1 total +7.77 =
  composition +5.13 / cam_value +3.58 / calibrator_floor **−0.94**; Q4 total
  −3.31 = composition −1.56 / cam_value −3.99 / calibrator_floor **+2.24**.
  Calibrator floor clean/wrong-signed at both tails on 5 seasons too — PR C
  confirmed not the Q1/Q4 lever.

**Net:** PR B refutation + the "served projection is well-calibrated, the
remaining error is ~23% irreducible variance" conclusion are validated at
3.6× sample. The expanded backtest is now the standing power base for
testing new-signal features (coaching / minutes-role) — the only remaining
r² lever.
