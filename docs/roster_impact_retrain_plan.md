# Roster-impact retrain on projected (serve-consistent) rosters

**Status: scoped, not started.** A design + validation plan to close the
train/serve mismatch in the roster-impact preseason projection calibrator. This
is the principled fix for the Pierce-class returner over-credit
(`docs/redshirt_handling.md`), replacing the reverted serving-side filter.

> **This is a design doc, not the operational checklist.** For "I changed
> something upstream, what do I rerun?", use `training/retrain_downstream.sh`,
> which runs the chain in dependency order and cannot skip a step, and read
> `docs/model_dependency_graph.md` for why the tree is shaped this way.
>
> That distinction is load-bearing. Step 3 below — "`train_roster_adjo_model.py`
> rides along… retrain it too" — was for a long time the *only* place the AdjO
> retrain was written down, and being buried in a future design doc is precisely
> why it got skipped three times (#130, #152, #211). `roster_adjo` served an
> OOF three generations stale for months, wrong by ~0.65 AdjO points on average
> and up to 3.5 for individual teams, with nothing erroring because its feature
> contract never changed. See #218.
>
> Prose is the weakest link in that chain, so the guarantee is not prose: the
> two roster-frame models each stamp their meta with an `oof_provenance`
> fingerprint (`training/oof_provenance.py`), and
> `cstat_core::inference::validate_roster_frame_provenance` refuses to boot the
> API when they disagree. The script makes the right thing easy; the stamp is
> what actually holds.

## The problem (why)

The roster-impact model maps a team's roster (cam_v3 distribution + archetype
shares + portal sums + `roster_size`) to next-season AdjEM. It is trained and
served on **different roster distributions**:

- **Training** (`training/train_roster_impact_model.py`, `PLAYER_QUERY`): each
  (team, season S) example is the roster of players who **actually played** S
  (`player_season_stats ... games_played >= 5 AND minutes_per_game >= 5`), valued
  by their held-out OOF cam. Clean — no redshirts / no-shows.
- **Serving** (`cstat_core::roster_projection::compose_all_projections`): the
  roster is the base season (S−1) carried forward — returners + portal arrivals +
  recruits − departures — which **necessarily includes future no-shows** (we
  cannot know at preseason who will redshirt / quit / leave D-I).

So the model learns on clean rosters but is fed padded ones at serve time. That
padding is the mild baseline over-projection (backtest bias **+0.22**). The
reverted returner-redshirt filter tried to clean the *serving* roster to match
training; it cost 91 team-seasons of coverage and *worsened* bias to +0.54,
because (a) removing returners pushes thin teams under `MIN_QUALIFYING`, and (b)
the cam_v3-rank rotation-minute renormalization makes the projection *more*
optimistic when a low-value bench no-show is deleted. **The mismatch is
distributional, so the fix belongs in the training distribution, not a per-player
serving filter.**

## The fix (what)

Train the calibrator on the **same base-carried-forward projected rosters that
serving produces** (including the ~20% who won't pan out), so it learns to price
in expected attrition. Same features, same architecture, same LOSO discipline —
only the training roster *membership* changes from "who played" to "who was
projected to play."

## Approach — Rust dumps the training frame (drift-proof)

Do **not** re-implement the base-carried-forward composition in Python SQL — the
partition logic (class-year advance, portal, draft, recruit synthesis, projected
cam application, rotation renorm) is intricate Rust that would drift from serving.
Instead, generate the training frame from the **exact serving code path**:

1. **New Rust dump** — extend `cstat-ingest projections-backtest` with a
   `--dump-features <path>` flag (or a sibling `dump-roster-impact-training`
   subcommand). For each `target_season` in the LOSO range (2016–2026) and each
   team, run `compose_all_projections(base = target − 1, target_season_complete =
   false)` — **`false` is load-bearing**: train on the roster as it would be
   projected *preseason*, with attrition included, never a hindsight-cleaned one.
   Extract the `build_roster_impact_features` vector from
   `for_scenario(DraftScenario::Ceiling)` (the scenario the backtest already
   scores, `projections_backtest.rs:227`) and write rows:
   `(team_id, target_season, f0..f26, actual_adj_em, actual_adj_offense)`.
   Feature extraction needs only the freshman/trajectory models (for player cam
   values via `apply_projected_cam_v3`) — **not** the roster-impact model itself,
   so there is no chicken-and-egg.
2. **`build_dataset` reads the dump** — `train_roster_impact_model.py::build_dataset`
   loads the CSV instead of `PLAYER_QUERY` + `aggregate_team_season`. The 27-feature
   contract is unchanged (same `build_roster_impact_features`), so
   `roster_impact_model_meta.json` and the Rust boot check
   (`inference.rs::validate_roster_impact_meta`) need no change. **Bonus:**
   deleting the Python roster-aggregation mirror (`aggregate_team_season`,
   `PLAYER_QUERY`, `CANONICAL_ROTATION_MPG` copy) also removes a standing
   Rust/Python parity risk.
3. **`train_roster_adjo_model.py` needs its own invocation** — it `import`s
   `build_dataset` from the impact model, so it picks up the new frame *when it
   runs*, but sharing the loader does NOT mean it retrains itself. That false
   intuition is the root cause of #218. `retrain_downstream.sh` runs both halves
   as adjacent stages so they cannot drift apart.
4. Keep LOSO (`LOSO_EXPORT_SEASONS = 2016..2026`) export unchanged.

## Leakage discipline (preserve)

- The dumped rosters' player cam values already come from `trajectory_oof` /
  `freshman_oof` (held-out) via compose's `apply_projected_cam_v3` — leak-free by
  construction, identical to serving.
- The roster-impact model stays **leave-one-season-out**.
- `actual_adj_em` / `actual_adj_offense` are targets, merged **after** the
  feature columns are fixed (the existing `train_roster_adjo_model.py:60-61`
  guard), so the target can never leak into a feature.

## Validation — accept/reject gates

This is a **hypothesis to validate, not a guaranteed win.** Ship only if:

- **Bias moves toward 0.** The +0.22 (raw) over-projection should shrink — the
  model now expects the no-show padding. This is the primary success signal.
- **MAE ≤ current 6.13** pooled (LOSO `projections-backtest`), per-season no worse
  than noise.
- **Coverage unchanged** — no `MIN_QUALIFYING` regression (we add no filter; the
  serving roster is untouched).
- **Blend re-tuned** — re-run `measure-blend-accuracy`; the preseason×pit weight
  may shift as the raw projection's bias changes.

If MAE degrades, the hypothesis is wrong (the played-roster frame was better) and
we **do not ship** — keep the current model, and the Pierce case stays a known,
documented limitation.

Sanity check on success: Princeton's 2026 projection comes down not because
Pierce is surgically removed, but because the model discounts every roster for
expected attrition — the Pierce over-credit dissolves into calibration.

## Downstream regeneration (same as prior roster-impact retrains)

- Re-export `roster_impact_model.onnx` + `roster_adjo_model.onnx` (+ meta);
  boot-validate via the `Predictor` meta-drift check.
- Regenerate `team_preseason_projection` (`cstat-ingest compute-projections`).
- Regenerate coach-above-expectation (`training/compute_cae.py`) — CAE is the
  roster-impact residual, so grades shift (descriptive; expected and fine).
- Re-run the 11-season projection backtest dump.

## Risks / open questions

- **Scenario choice.** The backtest scores `Ceiling`; the served midpoint blends
  floor/ceiling by `p_return`. Recommend training on `Ceiling` for parity with
  the existing backtest and documenting it; revisit if a p_return-weighted roster
  validates better.
- **Noisier rosters.** Projected rosters have larger `roster_size` and a longer
  low-cam tail (the no-shows). The intended effect is a blunter, attrition-aware
  mapping; the risk is the extra noise hurts more than the alignment helps —
  which the MAE gate catches.
- **CAE shift** is expected (roster-impact-relative); note it in release, not a
  defect.

## Effort

Medium–large, its own PR: new Rust dump command (reuses the backtest
composition), `build_dataset` rewrite (simpler — reads a CSV), retrain + export
both models, full downstream regeneration + revalidation against the gates above.

## Key files

- `training/train_roster_impact_model.py` — `PLAYER_QUERY` / `aggregate_team_season`
  (to be replaced by the dump reader), `build_dataset`, LOSO export.
- `training/train_roster_adjo_model.py` — imports `build_dataset`; retrains with it.
- `crates/cstat-ingest/src/projections_backtest.rs` — the composition + Ceiling
  feature path to extend with the dump.
- `crates/cstat-core/src/roster_projection.rs` — `compose_all_projections`
  (`target_season_complete = false` for the dump).
- `crates/cstat-core/src/roster_features.rs` — `build_roster_impact_features`,
  `CANONICAL_ROTATION_MPG` (the feature contract, unchanged).
- `crates/cstat-core/src/inference.rs` — `predict_roster_impact`,
  `validate_roster_impact_meta` (boot contract, unchanged).
- `training/compute_cae.py` — regenerate after retrain.
