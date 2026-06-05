# training/validation/

One-off **validation / de-risk / audit** scripts — analyses that answered a
"should we build X?" question, not part of the production training pipeline.
Nothing here is imported by prod code or run by CI. Prod trainers, shared libs
(`db.py`, `features.py`, `recruit_features.py`), re-runnable acceptance gates
(`decompose_projection_error.py`, `attribute_q1_overprojection.py`), and the
regression test (`test_cross_season_joins.py`) stay in `training/`.

**Findings live elsewhere** — durable conclusions are in the agent memory
(`.claude/.../memory/`) and `training/eval_history/` (summary JSONs + `.md`
write-ups). These scripts are the *reproductions*, kept so a conclusion can be
re-checked, not re-derived.

## Running

Each script has a path shim (parent `training/` on `sys.path`) so prod-module
imports resolve. Run from the repo with `DATABASE_URL` set:

```bash
cd training && set -a && . ../.env && set +a
python3 validation/<script>.py
```

## Index (2026-06-05 session — projection/coach lever de-risks)

| script | question | verdict |
|---|---|---|
| `exp_campom_od_architecture.py` | predict CamPom primitives vs cam_o/cam_d directly? | primitives win the o/d split 8–32%, SOS-robust; tie on net |
| `derisk_cae_od_signal.py` | is offense/defense over-perf a stable coach trait? | looks stable (r≈0.55) — but confounded with program |
| `derisk_cae_od_school_changers.py` | does the o/d signature travel when a coach switches schools? | **no** — defense is program (r=0.46) not coach (0.08); tilt ~0 |
| `derisk_cae_od_power.py` | enough coaching movement to trust that? | yes — 145 movers, method detects 0.46; tilt confidently ~0 |
| `derisk_served_cae_travel.py` | is the served CAE ranking real coaching or just projection residual? | real & portable (r≈0.28–0.37); ~half tracks team quality |
| `exp_oracle_minutes_ceiling.py` | does fixing canonical-rotation minutes help? | **no** — oracle minutes only −0.065 MAE (1.8%) |
| `exp_chemistry_signal.py` | does roster continuity/chemistry carry residual signal? | **no** (−0.061 MAE leak-free); the −1.0 first pass was Σcam≈AdjEM leakage |

Net: CAE-O/D, minutes/role (ROADMAP 3b), and continuity (3c) all refuted as
model levers. The served coach ranking is sound. The preseason roster
projection is at its honest floor (~23% irreducible variance).

## Candidates to relocate here (pre-existing one-off de-risks, follow-up)

Tracked scripts in `training/` that are also one-off validation (not prod, not
gates): `derisk_coach_quality.py`, `derisk_coaching_change.py`,
`diagnose_trajectory_attrition.py`, `audit_preseason_projections.py`,
`campom_sensitivity_sweep.py`, `compare_train_windows.py`,
`honest_predict_via_subtraction.py`, `measure_pit_drift.py`,
`pit_cae_backtest.py`, `pit_program_calibration.py`, `post_audit_validate.py`,
`quantify_leakage.py`, `spike_247_baseline.py`,
`transition_blend_diagnostic.py`, `benchmark_natstat.py`, `dump_shap_baseline.py`.
Moving these is a larger tracked-file rename — left for a follow-up so this PR's
diff stays reviewable. (Each would need the same path shim.)
