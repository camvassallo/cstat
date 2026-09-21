# Model dependency graph and retrain protocol

**What this doc is for.** Several of cstat's trained models consume another
model's output as training input, so retraining one can silently invalidate
another. This is the map of which depends on which, in what order to retrain,
and how the pieces reach production.

**What this doc is not.** It is not the guarantee. Prose that a human has to
remember is the weakest link in the chain, and this exact instruction was
written down and skipped three times anyway (#130, #152, #211 — see #218). The
ordering of durability is:

1. **Executable guard** — the `oof_provenance` stamp compared at boot by
   `cstat_core::inference::validate_roster_frame_provenance`. Catches drift
   regardless of who ran what, or whether they read this file.
2. **Staleness query** — `training/check_provenance.py`. Answers "which nodes
   are stale?" against the live database, across every edge of the tree rather
   than the single Layer 1 → Layer 2 edge the boot guard covers (§6).
3. **Single command** — `training/retrain_downstream.sh`. Makes the correct
   sequence the easy one.
4. **Prose** — this doc. Explains *why* the tree is shaped this way, which
   neither of the above can.

Use the script to do the work. Read this to understand what the script is doing
and why skipping a stage is not free.

---

## 1. The layer map

```
LAYER 0  data
  cstat-ingest (NatStat / Torvik / 247 / coachdict)
      -> compute_all (crates/cstat-core/src/compute.rs)
          -> team_season_stats      adj_efficiency_margin, adj_offense, adj_defense
          -> torvik_player_stats    cam_gbpm_v3_psos  (the value currency)
          -> player_season_stats    the qualification gate (>=5 GP, >=5 MPG)
          -> player_on_off          lineup rollups (trajectory features)
          -> player_archetypes      ASSIGN half — nearest frozen centroid

LAYER 0.5  archetype fit                          [annual, own cadence]
  training/archetypes.py  -> archetype_models (centroids)
      the ASSIGN half above reads these; refitting moves every downstream
      arch_* feature in Layer 1 and Layer 2

LAYER 1  player projection models                 [WRITE the OOF tables]
  train_trajectory_model.py -> trajectory_{mean,q10,q90}_model.onnx
                            -> trajectory_oof_predictions   (TRUNCATE + reload)
  train_freshman_model.py   -> freshman_{mean,q10,q90}_model.onnx
                            -> freshman_oof_predictions     (TRUNCATE + reload)

LAYER 2  team calibrators                         [TRAIN ON Layer 1's OOF]
  cstat-ingest projections-backtest --frame-only
                               -> frames/roster_impact_ex_ante.json (gitignored)
      the SERVED composition of every historical team-season (returners -
      departures + arrivals + recruits, OOF cam_v3), as 27 calibrator
      features + actual AdjEM. Both trainers read this file; each refuses
      one cut from a different OOF snapshot than the live tables.
  train_roster_impact_model.py -> roster_impact_model.onnx        (served, net AdjEM)
                               -> roster_impact_loso/*.onnx       (gitignored, feeds backtest)
  train_roster_adjo_model.py   -> roster_adjo_model.onnx          (served, AdjO half)
      both share that one frame via build_dataset;
      both stamp oof_provenance; the API refuses to boot if they disagree

LAYER 3  derived products                         [no training]
  cstat-ingest projections-backtest  -> per-team dump (gitignored)
  training/compute_cae.py            -> coach_season_cae, coach_ratings
  cstat-ingest compute-projections   -> team_preseason_projection
                                     -> player_season_projection (see caveat, §3)
      runs over the historical range PLUS the forward seasons, which have no
      actuals to backtest but are the ones actually served (§3, #263)

LAYER 4  hand-tuned serving constants             [NOT in the chain runner]
  transition_blend_diagnostic.py  reads the Layer 3 dump
      -> PROJECTION_SHRINK_WEIGHT / _OVERHAUL   (roster_projection.rs, Rust const)
  cstat-ingest measure-blend-accuracy  reads team_preseason_projection
      -> PRESEASON_PEAK_WEIGHT / _DECAY_DAYS / _HOME_COURT_ADVANTAGE
                                                (predict.rs, Rust const)
      these are READ OUT of a diagnostic and typed into source by a human;
      nothing re-derives or re-validates them on a retrain — see §3
```

### The game-model branch has no edge into this tree

`margin` / `win` / `total` and their point-in-time twins `pit_*`
(`training/train.py`, `training/features.py`) hang off Layer 0 CamPom and team
stats directly. They do **not** read `trajectory_oof_predictions`,
`freshman_oof_predictions`, or any roster-frame model — verified: nothing in
`features.py` touches the projection tables or the roster models.

**So a roster-tree retrain never requires retraining the game models.** Do not
run them "to be safe." A no-op retrain is not free: it rewrites committed ONNX
artifacts, and the game trainers were **not** part of the #222 reproducibility
work (which covered the Layer 1 and Layer 2 frames only, per
`test_frame_determinism.py`). Rerunning `train.py` on unchanged data will
produce different bytes and a diff you cannot attribute to anything.

One coupling does exist, and it is at **serve** time, not train time:
`routes/predict.rs::fetch_preseason_margin` reads `team_preseason_projection`
for the early-season preseason blend (peak weight 0.70 at the Nov 1 open,
decaying to 0 over 42 days). So a Layer 2/3 regeneration **does** change served
opening-week game predictions, without changing a single game model. That is a
data edge, closed by a sync, not a deploy.

### The legacy box-score roster model

`roster_model.onnx` is committed but **deliberately not loaded at boot** (see
the "Not loaded here" note on `Predictor::load`). No route serves it; it is
materialized lazily on first `predict_adj_em`, which only `projections-backtest`
reaches, and its meta is checked there via `validate_box_score_model_meta`
rather than at startup — so its absence or drift cannot block the API from
booting. It takes no input from this tree and feeds nothing in it. Removal is
tracked in the ROADMAP Refactor Backlog.

---

## 2. Why error absorbs rather than compounds

This is the most important idea in the tree and the one most likely to be
guessed wrong. The intuition "each model has error, so stacking them compounds
the error" is the natural reading of the diagram above, and it is **not** what
this design does.

Layer 2 does not train on actual player value. It trains on the **held-out
predictions Layer 1 actually emits** — `trajectory_oof_predictions` for
returners, `freshman_oof_predictions` for recruits (see the v2 note in
`train_roster_impact_model.py`'s module docstring).

That matters because Layer 1's predictions are *biased in a knowable
direction*: the trajectory model regresses toward the mean and under-projects
elite returners by roughly 3.4 CamPom. Version 1 of the roster-impact model
trained on actual same-season `cam_gbpm_v3_psos` — unbiased inputs — and so
learned a calibration slope for a distribution it would never be fed. It then
inherited the upstream bias raw at serve. Version 2 trains on the biased
predictions themselves, so the calibrator **learns and absorbs that bias**.

The corollary is the thing to internalize:

> **The failure mode is desynchronization, not bad data.**

A Layer 2 model is not fragile to Layer 1 being somewhat wrong — it is
calibrated against Layer 1's specific error profile. It is fragile to that
error profile **changing underneath it**. Retrain Layer 1 without retraining
Layer 2 and the calibrator is now correcting for a bias that no longer exists.
Nothing errors. The feature contract is unchanged, the model loads, the numbers
look plausible.

That is exactly what #218 was: `roster_adjo` served an OOF three generations
stale for months, wrong by ~0.65 AdjO points on average and up to 3.5 for
individual teams.

The same logic explains why Layer 1 is opt-in in the retrain script.
Regenerating the OOF is not a refresh — it `TRUNCATE`s the table and reloads
it, invalidating every Layer 2 model beneath. Run it when Layer 0 data changed
or a Layer 1 model was edited. Not to be safe.

### The cohort with no projection

Players neither OOF table covers — walk-ons, unranked freshmen, JUCO and
international arrivals, pre-2015 priors — are **dropped from the Layer 2
frame** (2026-09-20). Until then they fell back to their actual target-season
`cam_gbpm_v3_psos` on the belief that they were bench slots of little weight;
measured, they were 27–30% of the rotation's |CAM| — a slice of the target in
the features, and a roster shape the serve path never composes. Removing them
took the end-to-end served MAE 5.526 → 5.410 and the coach-grade reliability
guards from the floor (ICC 0.052 / split-half 0.050) to 0.107 / 0.213; the
in-frame LOSO MAE went 3.61 → 5.61, which is what a leak leaving looks like.
`build_dataset` prints per-source coverage and the dropped count on every
run; the meta records both. Details: `docs/projections_methodology.md`.

### The frame is the served composition (v4)

Dropping the unprojected bodies left one more difference between the frame
and the serve path: *who is on the roster*. Through v3 the frame composed each
rotation from the players who actually played the target season — CAM held
out, presence not — while the serve path composes ex-ante, before anyone has
played. The Rust backtest already composes exactly the served roster for every
historical team-season, so since 2026-09-20 it also writes that composition
out as the training frame (`projections-backtest --frame-out --frame-only`,
the `frame` stage). What this buys is agreement: the in-frame LOSO number
(5.39) and the end-to-end backtest (5.42) now measure the same thing, where
before they disagreed (5.61 / 5.49) and the in-frame one was optimistic on
rosters no served row resembles. Served accuracy is a tie pooled; the raw bias
went +0.24 → +0.09. The frame is a file, so a Layer 1 retrain that skips the
`frame` stage would train the calibrators on stale projections — the trainer
compares the frame's OOF snapshot (row count and newest `created_at` per
table) with the live tables and refuses.

---

## 3. The retrain protocol

**Rule: retrain from the highest stale node downward.** Everything below a
changed node is stale, including nodes you did not touch.

Use the script. It runs the chain in dependency order and cannot skip a step:

```bash
./training/retrain_downstream.sh --dry-run        # print the plan
./training/retrain_downstream.sh                  # Layer 2 + 3 (the common case)
./training/retrain_downstream.sh --with-layer1    # the whole tree
./training/retrain_downstream.sh --from cae       # resume after a failed stage
```

Stages, in order: `guard trajectory freshman frame roster_impact roster_adjo
backtest cae projections`. `guard` is the look-ahead check (§3c) and is put
back into any `--from` plan that contains a frame-building stage. Layer 1 is opt-in; `--from` into a Layer 1 stage implies
`--with-layer1`, because "start here and run everything after" must not
silently drop the stage immediately following.

The season range is **not** uniform across those stages. `backtest` scores
against actual AdjEM, so it runs over the historical seasons only; the
`projections` stage additionally materializes the **forward seasons** — the
current one and the one being forecast — which is where the served rows live.
See §3.

### Before you run: is local Layer 0 current, and does it match prod?

Models reach prod by **git deploy** while data reaches it by **sync**. Training
against a laptop that is behind prod ships that staleness into a committed
artifact, and no sync can fix it. There is no tooling for this check yet
(#225); do it by hand:

```bash
./scripts/sync_to_prod.sh --prod-status
```

This is not theoretical. Done by hand before the #218 retrain, it turned up a
`player_archetypes` table 2,984 rows ahead locally — a 2026-07-17 archetype
retrain that was never synced. Training on top of that would have baked a
prod/local archetype mismatch into a committed model.

### What each stage owes the next

- **`models/roster_impact_loso/*.onnx` refreshes only as a side effect of the
  `roster_impact` stage.** `projections-backtest` loads the per-target-season
  LOSO model (`projections_backtest.rs:335`) precisely so the backtest is
  honest, and `compute_cae.py` scores against that backtest's dump — so a stale
  LOSO set means CAE grades computed against a projection generation that no
  longer ships. Two things make it easy to miss: the files are gitignored, so
  they never appear in `git status`, and `roster_adjo` computes LOSO metrics
  but exports **no** per-season ONNX. Running the AdjO half alone therefore
  leaves the backtest reading the old models.
- **`roster_adjo` needs its own invocation.** It `import`s `build_dataset` from
  the net trainer, which reliably creates the intuition that retraining the net
  model updates the AdjO half. It does not. This is the single specific
  omission that caused #218.
- **`cae` needs the dump `backtest` just produced,** passed explicitly with
  `--dump`. The fallback picks the newest match by *filename*, and the
  historical dumps carry descriptive tags that sort after a plain date.
- **Layer 3 order is `backtest` -> `cae` -> `projections`.** CAE is the
  roster-impact residual, so grades shift on every Layer 2 retrain. That is
  descriptive and expected, not a defect.

### Layer 4: the constants the chain runner does not touch

Five numbers in the serving path were tuned by hand against a Layer 3 output
and are now Rust `const`s:

| Constant | Where | Tuned by |
|---|---|---|
| `PROJECTION_SHRINK_WEIGHT` (0.70 anchored / 0.30 unanchored) | `roster_projection.rs` | `transition_blend_diagnostic.py`, off the backtest dump |
| `PROJECTION_SHRINK_WEIGHT_OVERHAUL` (0.55 / 0.20) | `roster_projection.rs` | same |
| `PROGRAM_ANCHOR_SHRINK` (1.0) | `roster_projection.rs` | `program_anchor_era_diagnostic.py` |
| `PRESEASON_PEAK_WEIGHT` (0.70) | `predict.rs` | `cstat-ingest measure-blend-accuracy` |
| `PRESEASON_DECAY_DAYS` (42) | `predict.rs` | same |
| `PRESEASON_HOME_COURT_ADVANTAGE` (3.5) | `predict.rs` | same |

The shrink weights sit **inside** the loop, not after it: `compute-projections`
and the `/api/projections` route both apply them, so the `projections` stage
materializes rows using a weight tuned against a *previous* generation of the
raw projector. Their own doc comment records that they were last retuned
2026-06-27 "after the multi-season-trajectory calibrator refit," which is the
tell — a Layer 2 retrain is exactly the event that can move their optimum.

**Since #361 the three projection constants ARE re-checked on every Layer 2
retrain**, forward-chained: `train_roster_impact_model.py` re-searches
`PROJECTION_SHRINK_WEIGHT`, `_OVERHAUL` and `PROGRAM_ANCHOR_SHRINK` inside each
walk-forward fold — on earlier seasons' held-out raw predictions only — and
scores the refit against the served values on the held-out season
(`walk_forward.constants_refit` in the meta, printed by the scorecard). The
refit is fold-stable at 0.55 / 0.20 / 0.75 and scores a tie with the served
0.70 / 0.55 / 1.0 (pooled +0.016, z=+0.9, 2021–2026), which is what "the served
constants were not fit to the test set" looks like as a number. A refit that
beats the served constants out of sample is the signal to change them; a refit
that merely differs is not. The three `predict.rs` constants are still not
re-checked by the chain (#236).

**`retrain_downstream.sh` does not run either tuner** (#236). That is deliberate rather than an oversight: both tools *report* a
recommended value, they do not write code, so there is no honest way to
automate the step.
But it means the constants keep carrying their last-measured assumption until
someone re-measures. After a Layer 2 retrain that moved the projector
materially, run both (the current values were last confirmed 2026-07-27, below):

```bash
# From training/ — pass the dump the retrain just produced, by name.
./.venv/bin/python transition_blend_diagnostic.py \
    --dump eval_history/projections_backtest_per_team_full_11season_run<TAG>.json

# From the REPO ROOT — MODEL_DIR defaults to the relative `training/models`,
# so this fails with "margin_model.onnx does not exist" from anywhere else.
cargo run --release --bin cstat-ingest -- measure-blend-accuracy --years 2024,2025,2026
```

Pass `--dump` explicitly. `load_backtest()`'s fallback picks the newest dump by
**filename**, not by mtime, and the historical dumps carry descriptive tags
(`…_traj60honest211_20260725`) that sort *after* a plain `run…` name — so the
fallback can silently hand a freshly-retrained tuner a superseded dump. Tuning
a served constant against the wrong projection generation is the #218 failure
mode one layer over. `compute_cae.py`, `transition_blend_diagnostic.py`,
`pit_cae_backtest.py`, and `pit_program_calibration.py` all take `--dump`;
`load_backtest` warns when name-order and mtime-order disagree.

**Re-validated after the full-tree retrain (2026-07-27). All five constants
confirmed; no change warranted.** Both tuners were rerun against the
post-retrain state. Details below, but the headline is that Layer 4 is a real
structural gap because nothing *forces* the re-check — not because the
constants drift often.

One number *did* move, in the direction that removes a caveat.
`PROJECTION_SHRINK_WEIGHT_OVERHAUL = 0.20` had sat one grid step off its
optimum (0.25) on both the pre- and post-#218 dumps, which the previous
write-up characterised as a deliberate rounding. On the 2026-07-27 dump the
overhaul optimum **is** 0.20 — pooled MAE 5.934, with continuity optimal at
0.45 / 5.429. The shipped pair is now exactly optimal on both cohorts, and the
honest leave-one-season-out test still favours transition-conditional
weighting (flat 5.7034 → 5.6588, lift +0.045).

The preseason blend re-measured identically to the #218 check: peak weight
0.70 and HCA 3.5 exactly optimal, `PRESEASON_DECAY_DAYS = 42` versus the
grid's 49 worth nothing pooled (both 8.82) and 0.00 / 0.01 / 0.00 per season
across 2024 / 2025 / 2026. The preseason-leg-only HCA sweep again reports an
"optimum 1.5" — that is the trap documented below, not a recommendation.

The tables that follow are from the #218 re-validation and are kept because
their pre/post comparison is what established that the retrain, rather than
drift, moves these numbers.

#### Shrink weights

Checked against both the pre- and post-retrain dumps, on the same 2,632
team-seasons, so the retrain's effect could be separated from the tuning
question:

| cohort | shipped | shipped MAE | grid optimum | optimum MAE | cost of shipping |
|---|---|---|---|---|---|
| continuity (≥40% retained), post-#218 | **0.45** | 5.4274 | 0.45 | 5.4274 | 0.0000 |
| continuity, pre-#218 | 0.45 | 5.4233 | 0.45 | 5.4233 | 0.0000 |
| overhaul (<40% retained), post-#218 | **0.20** | 5.9483 | 0.25 | 5.9446 | +0.0037 |
| overhaul, pre-#218 | 0.20 | 5.9372 | 0.25 | 5.9352 | +0.0020 |

`PROJECTION_SHRINK_WEIGHT = 0.45` is exactly optimal on both generations.
`PROJECTION_SHRINK_WEIGHT_OVERHAUL = 0.20` sits one grid step off a nearly flat
optimum — the overhaul curve runs 5.9686 / 5.9483 / 5.9446 / 5.9532 across
w = 0.15 / 0.20 / 0.25 / 0.30 — so the cost is 0.0037 AdjEM on a ~5.95 metric,
far inside noise.

The more useful finding is the attribution: **the retrain did not move the
optimum.** Both dumps pick 0.25 for the overhaul cohort, so the one-step offset
predates #218 and is not calibrator drift. It is consistent with the original
tuning note ("the overhaul cohort's own backtest optimum moved 0.25 → 0.20"),
i.e. a deliberate rounding rather than a stale value.

#### Preseason blend schedule

`measure-blend-accuracy --years 2024,2025,2026`, 17,176 games scored, 11,107 in
the shared subset where both legs exist:

| schedule | pooled blended MAE |
|---|---|
| pit-only (no blend) | 9.17 |
| pre-calibration (w=1.0, end=75, HCA=3.5) | 9.01 |
| **current route (w=0.70, end=42, HCA=3.5)** | **8.82** |
| grid optimum (w=0.70, end=49, HCA=3.5) | 8.82 |
| per-week oracle (ceiling) | 8.78 |

`PRESEASON_PEAK_WEIGHT = 0.70` and `PRESEASON_HOME_COURT_ADVANTAGE = 3.5` are
both exactly optimal. `PRESEASON_DECAY_DAYS = 42` versus the grid's 49 is worth
nothing pooled — the two tie at 8.82 — and per season the gap is 0.00 / 0.00 /
0.01 (2024 / 2025 / 2026). Total remaining headroom to the per-week oracle is
0.04, so the linear-from-open shape is close to exhausted.

**One trap in that output.** The HCA sweep reports a "preseason-leg HCA optimum
1.5", well below the shipped 3.5. That sweep scores the **preseason leg alone**;
the joint grid, which optimizes the metric actually served, picks 3.5. Read in
isolation the sweep looks like it is telling you to change the constant. It is
not.

### The `player_season_projection` caveat

`compute-projections` writes **two** tables. `team_preseason_projection`
legitimately wants the full 2016..2026 range — the preseason blend and
`measure-blend-accuracy` read historical seasons. `player_season_projection`
does not: the wide range materializes ~3.5k rows per historical season that
nothing serves (`/api/projected-players/{year}` filters on `target_season`).
Those rows are inert, and their values come from the **trajectory/freshman**
models, so a Layer 2 retrain does not change them. Narrow with `--years` if you
only meant the team table.

**The forward seasons invert that.** For the current season and the one being
forecast, the player rows are the served product, not the byproduct — they are
the Future page's player board — and `team_preseason_projection`'s forward row
is the anchor `routes/predict.rs::fetch_preseason_margin` blends over opening
week. Neither is in the historical range, because neither has actuals for
`backtest` to score.

Both are materialized, not just the forecast year, and that is deliberate. The
Future page's year is **not** derived from the same rule: `upcomingProjectionSeason()`
(`web/src/components/season.ts`) is `AVAILABLE_SEASONS_FALLBACK[0] + 1`, a
hand-maintained frontend constant. The two agree today and drift between the
November flip and whoever next bumps that list, so covering both forward seasons
is what keeps the served year covered through the gap. The current season also
needs its own row on its own merits — it is the opening-week preseason anchor.

That is what #263 was: `retrain_downstream.sh` derived one season list and used
it for every stage, so a full retrain refreshed 2016..2026 and left the forecast
year — in August, the only season anyone looks at — sitting on superseded Layer 2
models, while exiting 0 and printing its success summary. `check_provenance.py`
caught it, but only because someone ran it. The script now appends the forward
seasons to the `projections` stage, and to that stage alone.

A forward season whose `teams` rows do not exist yet writes **zero** team rows
and reports them as `unresolved-target` — `resolve_base_to_target` is an INNER
JOIN onto `teams WHERE season = target` (#245). That is the expected and correct
output, not a failure; the player rows on the same pass are still written.

### Getting it to prod: deploy vs sync

This split is what made #218 unfixable by a sync, and it is worth being precise
about because the two halves of Layer 2 land differently.

| Artifact | Layer | Reaches prod by |
|---|---|---|
| `*_model.onnx` + meta (committed) | 1, 2 | **git deploy** |
| `roster_impact_loso/*.onnx` | 2 | neither — gitignored, local-only, backtest input |
| `team_preseason_projection` | 3 | **data sync** |
| `player_season_projection` | 3 | **data sync**, whenever the `projections` stage ran. Its *values* come from the trajectory/freshman models, so a Layer 2-only retrain leaves them byte-identical — but the *season set* is not fixed, and a run that materializes a new forward season writes rows prod has never held. Gating on Layer 1 would print a push that omits exactly those rows |
| `coach_season_cae`, `coach_ratings` | 3 | **data sync** |
| `player_archetypes` | 0 | data sync in the offseason; **prod-owned in-season** (the Rust assign half runs nightly) |
| `artifact_provenance` | 3 | **data sync** — not in `sync_to_prod.sh`'s EXCLUDED list, so it travels with the tables it describes. It has to: `team_preseason_projection` reaches prod by sync, and provenance recorded only on one laptop would leave prod holding rows of unknown origin |

```bash
./scripts/sync_to_prod.sh --tables team_preseason_projection,coach_season_cae,coach_ratings,artifact_provenance
# ...plus player_season_projection when the run included the projections stage
```

`retrain_downstream.sh` prints the right list for the run it just did, rather
than leaving that to be remembered.

The subtlety: **the served AdjO/AdjD split is never materialized.**
`routes/projections.rs:641` runs `roster_adjo_model.onnx` live at request time
and derives `projected_adj_d = projected_adj_o - midpoint_adj_em`.
`team_preseason_projection` has no AdjO column (`migrations/023`). So the AdjO
half reaches production **purely by git deploy** — pushing every table in the
database would not have moved it one point. That is why a stale `roster_adjo`
could survive months of routine syncs.

The net AdjEM headline is the mirror image: served from the materialized table,
so it moves on sync and not on deploy.

---

## 3b. How the tree is judged: walk-forward is the canonical number (#361)

Every model here forecasts a season from what was known before it, and until
2026-09 every one of them was judged leave-one-season-out (or its pair/class
variant), which trains on seasons *after* the one being scored. Two things
were wrong with that: the number is optimistic, and a variant can win LOSO and
lose forward-chained (#359 and #360 both found that shape). So every trainer
now also runs **walk-forward** — train on seasons strictly earlier than S,
test on S, for S = 2021..2026 — through one harness (`training/walk_forward.py`:
fold generator, the cohort table, the rank metric set) and stamps a
`walk_forward` block into its meta beside the LOSO block. `walk_forward_report.py`
prints the four as one scorecard, which is the last thing `retrain_downstream.sh`
prints and the number the methodology docs quote.

What it found on the 2026-09-21 tree (identical rows, walk-forward vs
train-on-everything-else):

| layer | model | walk-forward | LOSO-family, same rows | optimism |
|---|---|---|---|---|
| 1 | trajectory (player CamPom) | 2.030 | 2.026 | 0.004 |
| 1 | freshman | 2.120 | 2.091 | 0.029 |
| 2 | roster_impact raw | 5.615 | 5.552 | 0.063 |
| 2 | roster_adjo raw (era-relative target, #368) | 3.985 | 3.895 | −0.090 |
| 2+4 | served projection | **5.494** | 5.459 | −0.035 |

(Post-#362 tree: the backtest and the frame compose rosters exactly as served,
no-shows included; the pre-#362 served number, 5.467, scored rosters with the
recruits who never played already removed — see §3c.)

The look-ahead everyone was right to worry about is worth almost nothing at
every layer: the tree does not lean on the future. That is now a measured
fact rather than a hope, and it holds only as long as the scorecard keeps
saying so — a Layer 2 change that opens a gap between the two columns has
started fitting to later seasons. The first thing the scorecard caught was
`roster_adjo`: walk-forward 4.48 against LOSO 4.10 (2026 alone 5.75), because
`adj_offense` trends across eras and LOSO interpolates that trend while
walk-forward has to extrapolate it. Retraining it relative to the base
season's league mean (#368) closed the gap (3.96 / 3.87) — the table above is
the post-#368 tree.

## 3c. The look-ahead guard: features must not see the target season (#362)

The last three model PRs each found a leak or a train/serve skew by hand
(#358, #199, #360). `training/lookahead_guard.py` is that audit as a test. For
a target season S it creates a schema of views that hide season S from every
season-scoped table — except the target and the row-defining columns each
frame is allowed to read there (`ALLOWED`: the player's season-S CamPom and
team, the qualification gate, the team's season-S AdjEM/AdjO, program
identity) — rebuilds each frame with `search_path = lookahead_guard, public`,
and requires every feature column to be byte-identical on the rows both
builds share. Rows that need season S to exist are reported, not failed: a
player with no season-S stats is not a training row, which is target
selection the trainers document, not look-ahead in a feature. Covers the two
Layer 1 frames (Python) and the calibrator frame (the Rust backtest, run with
the masked search_path in `DATABASE_URL`). Wired as the `guard` stage of the
chain runner and `test_lookahead_guard.py` in the training guards (it skips,
audibly, without a database or the release binary).

**What it caught on its first run.** The calibrator frame was composed with
the retroactive no-show exclusion (`docs/redshirt_handling.md`, PR 1): for a
completed target season, committed recruits with no box score were dropped —
126 of 311 teams in 2026 changed composition when season 2026 was hidden.
The live August projection includes those recruits, so the calibrator was
trained on rosters with ~20% of ranked recruits already removed and scored
rosters that still had them. The backtest now composes ex-ante
(`target_season_complete = false`; `--retro-exclude-no-shows` reproduces the
old graded composition for comparisons and must not feed a frame). Scored on
identical served rosters, a calibrator trained either way is a tie (+0.015,
z=+1.1; recruit-heavy teams +0.003) — no-shows carry near-zero projected CAM
— so the served number moving 5.467 → 5.494 is the *test* becoming honest,
not the model changing. The displayed historical grade (`compute-projections`,
the route) keeps the exclusion; coach grades, which score against the
backtest dump, now use the August expectation.

## 4. Artifact policy for `training/eval_history/`

Every `cae` run writes `training/eval_history/cae_compute_*_summary.json`.
`retrain_downstream.sh` **never stages it**, deliberately.

These summaries are tracked only when they accompany the model or code change
that produced them. A deploy-time recompute emits an orphan — a summary
describing no committed change — and one such orphan was swept into unrelated
PR #215 by a broad `git add`.

The script lists everything it wrote at the end of the run. Review with
`git status` and stage deliberately, or not at all.

Related `.gitignore` policy, for context on what is and is not a source
artifact:

- Committed: the production `*.onnx` bundles (allowlisted by name), their
  `*_meta.json`, and small canonical summaries in `eval_history/`.
- Ignored: `roster_impact_loso/`, `frames/` (the calibrator's ex-ante training
  frame, 1.6MB of floats regenerated by the `frame` stage; the meta records its
  sha256), per-team backtest dumps
  (`projections_backtest_per_team_*.json` — they once accounted for 89% of one
  PR's diff), OOF CSVs, experiment side-by-sides.

---

## 5. What is machine-checked vs what is convention

The distinction matters when deciding how much care a change needs. A checked
invariant will stop you; a convention will not.

### Machine-checked (fails the API boot)

`Predictor::load` runs these before serving a single request:

| Check | Catches |
|---|---|
| `validate_roster_impact_meta` on **both** roster metas | Feature name/order/count drift against `ROSTER_IMPACT_FEATURE_NAMES` (27 features) |
| `validate_roster_frame_provenance` | The two Layer 2 halves trained on **different OOF snapshots** — the #218 failure. Compares the `oof_provenance` stamp; a **missing** stamp is a hard failure, not a skip, because "can't tell, carry on" is the state that produced #218 |
| `validate_trajectory_meta` / `validate_freshman_meta` | Feature contract, qualification gate, quantile-alpha labeling, and `oof_persisted == true` — a retrain that skipped `persist_*_oof()` would silently serve in-sample predictions for every historical row |
| `FeatureCountMismatch` on `margin_model.lgb` / `pit_margin_model.lgb` | Game feature-vector width drift against `NUM_FEATURES`. Note this checks the `.lgb` TreeSHAP mirrors, not the served ONNX sessions — the game branch has no meta-drift gate as thorough as the roster branch's |

`validate_oof_persisted` is worth calling out: the serving routes try
`*_oof_predictions` first and fall through to live inference only for IDs with
no stored row. A retrain that trained fine but never persisted would look
healthy while serving in-sample projections — elite 2024 transfers projecting
+15-20 instead of the honest +8-12.

### Machine-checked (fails a test, not the boot)

- `cargo test -p cstat-core shipped_roster_models_share_an_oof_snapshot` — the
  provenance match, run by `retrain_downstream.sh` after both Layer 2 halves
  rebuild, so you learn about a mismatch at the end of the retrain rather than
  at the next deploy.
- `crates/cstat-core/tests/archetype_assign_parity.rs` — the Rust assign half
  is byte-exact with the Python writer.
- `training/test_frame_determinism.py` — the #222 guarantees: canonical frame
  order, rotation tie-break, stable sort. Runs in the `Training Guards` CI job.
- `crates/cstat-core/tests/sync_prod_r4_invariant.rs` — the four raw
  PBP/lineup tables stay local-only, which is what lets the in-season targeted
  push safely own the derived rollups.
- `training/test_provenance.py` — the #223 fingerprint chain. Also in the
  `Training Guards` CI job; the load-bearing check is that the generalized
  digest still reduces exactly to the #218 construction, because a drift there
  makes every committed stamp incomparable and the API refuses to boot.
- `training/test_walk_forward.py` — the #361 harness: a walk-forward training
  row is strictly earlier than the season it is scored on, and the rank
  metrics' identities. In the `Training Guards` CI job.
- `training/lookahead_guard.py` / `test_lookahead_guard.py` — the #362
  look-ahead guard (§3c): every training frame's features are invariant to
  the target season's data. Database-backed; the `guard` stage of
  `retrain_downstream.sh`, and an audible skip in CI.


### Still convention (nothing will stop you)

- **Local Layer 0 being current, and matching prod, before you train.** #225.
- **Syncing Layer 3 tables after regenerating them.** A retrain that stops at
  the last local stage leaves prod on the old numbers indefinitely.
- **Re-tuning the Layer 4 serving constants after a Layer 2 retrain.** Nothing
  runs the tuners, nothing compares the shipped value to the current optimum,
  and the constants are plain `const`s that will compile and serve whatever
  they say. Tracked as #236 (low priority — all five measured optimal
  2026-07-26). See the Layer 4 section above.
- **Passing every ingested season to `archetypes.py`.** The CLI default
  (`2025,2026`) is a 2-season fit that does *not* match the shipped
  combined-cohort model and clusters differently. See CLAUDE.md and
  `docs/archetypes_methodology.md`.
- **The `eval_history` staging policy** in §4.

---

## 6. Asking which nodes are stale

```bash
cd training && ./.venv/bin/python check_provenance.py
```

Every trainer stamps its meta with an `input_provenance` block — a fingerprint
of each input it consumed, built by `training/provenance.py`. The check
recomputes those fingerprints against the live database and prints a per-node
verdict, propagating staleness downward so a Layer 2 node reads STALE when
Layer 1 above it moved, even though its own inputs match.

This is the half the boot guard structurally cannot cover. That guard compares
the two Layer 2 halves **against each other**, so a Layer 1 retrain followed by
no Layer 2 retrain leaves both halves agreeing — and both stale. The report
catches it; the boot guard reports OK, correctly, because the pair really does
share one snapshot.

```
  ✗ Layer 1  trajectory       STALE      torvik_player_stats.cam_v3: changed 2019
  ✓ Layer 1  freshman         CURRENT
  ✗ Layer 2  roster_impact    STALE      upstream trajectory is stale
  ✗ Layer 2  roster_adjo      STALE      upstream trajectory is stale
  ✓ boot guard      OK         both halves share one OOF snapshot
```

### Verdicts are not binary, and that is the whole design

`compute_all` rewrites the live season's `cam_gbpm_v3_psos`,
`player_season_stats`, `player_on_off` and `player_archetypes` every nightly,
and the Layer 1 training window includes the in-progress season. A whole-table
comparison is therefore genuinely different every morning in-season. A tool
that prints STALE 150 nights a year is one people learn to scroll past — the
same ignored-warning failure that let #218 survive three regenerations.

So the fingerprints carry per-season sub-digests, and a change is classified:

| verdict | meaning |
|---|---|
| `CURRENT` | nothing moved |
| `CHURN` | only the in-progress season moved, in a table the nightly rewrites by design. Expected; not a reason to retrain |
| `STALE` | a **closed** season moved, or a table no nightly touches moved — a recompute, a swap-repair (#140/#201), a re-ingest, or an archetype refit |
| `UNSTAMPED` | the node predates the chain. Reported, not failed — every model is unstamped until its next retrain |

The exemption is deliberately narrow: one season, only in a table flagged
`nightly=True` in the source registry, and only when nothing else moved. A
season appearing or disappearing is never churn, and `recruits` — which no
nightly writes — is never exempt whatever the calendar says. In the offseason
`mutable_season()` returns `None` and nothing is exempt at all, so the same
byte-level diff reads CHURN in January and STALE in July. That is correct: in
July there is no nightly to explain it.

Exit codes: 0 = no drift, 1 = at least one STALE node, 2 = the two Layer 2
halves disagree (the API will refuse to boot). `--strict` also fails on
UNSTAMPED, once the tree has been retrained through. `--json` for tooling,
`--as-of YYYY-MM-DD` to evaluate the in-season rule against another date.

`retrain_downstream.sh` runs this at the end of every retrain, report-only — a
partial run is a legitimate mid-flight state and `--from` exists to resume one.

### Layer 3: which model artifact produced this

Layer 3 has no meta to stamp — `team_preseason_projection`, the backtest dumps,
and `coach_season_cae` are rows and files produced by CLI runs, not fits. They
record their producing model into `artifact_provenance` (migration 047) instead,
written inside the same transaction as the data itself so a crash cannot leave
rows whose recorded origin belongs to the previous run.

| producer | artifact | keyed by |
|---|---|---|
| `cstat-ingest compute-projections` | `team_preseason_projection` | season |
| `cstat-ingest projections-backtest` | `projections_backtest_dump` | dump filename |
| `training/compute_cae.py --write` | `coach_season_cae` | `all` |

Per-season keying on the projection is load-bearing: `--years` is routinely a
subset, and a single row per run would implicitly vouch for seasons that run
never touched.

The question Layer 3 can answer is narrower than Layer 1/2's, and still the one
that matters: **the model that produced this is not the model that ships now.**
Comparison is by the ONNX content digest, which is only meaningful because #222
made export byte-stable — before that, a no-op retrain would have moved every
digest and the signal would be worthless.

The specific gap this closes is the LOSO set. `projections-backtest` scores with
the per-target-season models in `models/roster_impact_loso/`, and `compute_cae.py`
grades coaches against that backtest's dump. Those files are **gitignored**, so
they never appear in `git status`, and `roster_adjo` exports no per-season ONNX
at all — so a backtest could silently run against a set drawn from a different
frame than the committed serving model, producing plausible CAE grades scored
against a projection generation that no longer ships. The set's per-file digests
are recorded and compared file-by-file.

**The dump format gained an envelope**: `{"provenance": {...}, "teams": [...]}`.
`load_backtest()` reads both that and the historical bare array, the same
back-compat approach as the `phase_b` -> `roster_proj` shim — `eval_history/`
holds months of dumps still cited in writeups, and regenerating them to satisfy
a format change would destroy the record they exist to preserve.

There are **seven** dump readers, not the four that take `--dump`. Four go
through `load_backtest` (`compute_cae`, `transition_blend_diagnostic`,
`pit_cae_backtest`, `pit_program_calibration`); three more —
`audit_preseason_projections`, `decompose_projection_error`,
`diagnose_trajectory_attrition` — glob for the newest dump and previously called
`pd.read_json` on it directly, which raises `ValueError: Mixing dicts with
non-Series` on an envelope. They now share `compute_cae.read_dump_records`.
Three further scripts (`cae_feasibility`, `derisk_coach_quality`,
`derisk_coaching_change`) read a *pinned* historical filename, so they are
permanently on the bare-array shape and need nothing.

Layer 3 staleness is **report-only**, deliberately. Exit 2 is reserved for
conditions that genuinely stop the API starting; a stale
`team_preseason_projection` is a data-freshness problem, and prod refusing to
serve over it would be strictly worse than serving it. Guarded by
`test_provenance.py::test_layer3_never_blocks_the_boot`.

---

## Related docs

- `docs/projections_methodology.md` — how the served projection is composed and
  calibrated (the *what*, where this doc is the *when to rebuild*).
- `docs/roster_impact_retrain_plan.md` — a **design** doc for a future change to
  the Layer 2 training frame. Not an operational checklist; this doc is.
- `docs/model_performance.md` — accuracy figures per model family.
- `docs/trajectory_methodology.md`, `docs/archetypes_methodology.md`,
  `docs/campom_methodology.md`, `docs/coach_above_expectation_design.md` — per-node
  methodology.
- `docs/in_season_ingest_plan.md` — Layer 0's nightly refresh.

## Feature-contract history

The roster-impact contract is **27 features**
(`ROSTER_IMPACT_NUM_FEATURES`). It was **25** at v1 and went to 27 in
`6be84d7` (Phase B, #95), which added the two portal features
`outbound_cam_v3_sum` and `inbound_cam_v3_sum`. Older writeups describing the
v1 aggregator as 25-feature (e.g. `ROADMAP.md:471`) are historically accurate
and should be left alone; anything describing the *current* contract should say
27.
