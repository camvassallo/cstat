#!/usr/bin/env bash
#
# Retrain the model tree from a chosen node downward, in dependency order.
#
# WHY THIS EXISTS (#218). The roster-frame models form a chain, and the
# operational habit was to run the steps people remembered. `roster_adjo`
# reuses `build_dataset` from `train_roster_impact_model.py`, which created a
# false intuition that retraining the net model updates the AdjO half too. It
# does not — it needs its own invocation, and it was skipped three times
# (#130, #152, #211). Because the feature contract never changed, the stale
# model loaded and served happily for months while being wrong by ~0.65 AdjO
# points on average and up to 3.5 for individual teams.
#
# So: one command that cannot skip a step. This is the *prevention* half of
# #218. The *detection* half is the `oof_provenance` stamp compared at boot by
# `cstat_core::inference::validate_roster_frame_provenance`, which catches the
# drift no matter how the artifacts were produced. Prevention makes the right
# thing easy; detection is what actually holds. Both, deliberately.
#
# THE CHAIN
#
#   Layer 1 (opt-in, --with-layer1) — these WRITE the OOF tables
#     trajectory     TRUNCATEs + reloads trajectory_oof_predictions
#     freshman       TRUNCATEs + reloads freshman_oof_predictions
#   Layer 2 — the roster-frame calibrators, trained on Layer 1's OOF output
#     roster_impact  served net AdjEM + the gitignored LOSO export set
#     roster_adjo    display-only AdjO half (the step that kept getting missed)
#   Layer 3 — derived products, no training
#     backtest       projections-backtest, using the LOSO models
#     cae            compute_cae.py, scoring against the backtest dump
#     projections    compute-projections -> team_preseason_projection
#
# Layer 1 is opt-in because regenerating the OOF invalidates every Layer 2
# model beneath it. Run it when Layer 0 data changed or a Layer 1 model was
# edited; not "to be safe". A no-op retrain is not free — it moves artifacts.
#
# BEFORE YOU RUN: is local Layer 0 actually current, and does it match prod?
# Models reach prod by git deploy while data reaches it by sync, so training
# against a laptop that is behind prod ships that staleness with no sync able
# to fix it. There is no tooling for this yet (#225); check by hand:
#   ./scripts/sync_to_prod.sh --prod-status
#
# ARTIFACT POLICY. `cae` writes training/eval_history/cae_compute_*_summary.json
# on every run. This script NEVER stages it. Those summaries are tracked only
# when they accompany the model/code change that produced them — a deploy-time
# recompute emits an orphan, and one such orphan was swept into unrelated PR
# #215 by a broad `git add`. Everything written is listed at the end; commit
# deliberately or not at all.
#
# USAGE
#   ./training/retrain_downstream.sh                     # Layer 2 + 3
#   ./training/retrain_downstream.sh --with-layer1       # the whole tree
#   ./training/retrain_downstream.sh --only roster_adjo,backtest
#   ./training/retrain_downstream.sh --from cae          # resume after a failure
#   ./training/retrain_downstream.sh --dry-run           # print the plan
#   ./training/retrain_downstream.sh --years 2016,…,2026 # override season range
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRAINING_DIR="$REPO_ROOT/training"
VENV_PY="$TRAINING_DIR/.venv/bin/python"
EVAL_DIR="$TRAINING_DIR/eval_history"

# 2016..2026 — the range every downstream product is already materialized for
# (team_preseason_projection and coach_season_cae both hold exactly these
# seasons) and the range `LOSO_EXPORT_SEASONS` exports models for. 2016 is the
# floor: it needs base-season 2015 player data plus trajectory OOF.
YEARS="2016,2017,2018,2019,2020,2021,2022,2023,2024,2025,2026"

ALL_STAGES=(trajectory freshman roster_impact roster_adjo backtest cae projections)
LAYER1=(trajectory freshman)

WITH_LAYER1=0
DRY_RUN=0
ASSUME_YES=0
ONLY=""
FROM=""

die() { echo "✗ $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --with-layer1) WITH_LAYER1=1; shift ;;
    --dry-run|-n)  DRY_RUN=1; shift ;;
    --yes|-y)      ASSUME_YES=1; shift ;;
    --only)        ONLY="${2:?--only needs a comma-separated stage list}"; shift 2 ;;
    --only=*)      ONLY="${1#--only=}"; shift ;;
    --from)        FROM="${2:?--from needs a stage name}"; shift 2 ;;
    --from=*)      FROM="${1#--from=}"; shift ;;
    --years)       YEARS="${2:?--years needs a comma-separated list}"; shift 2 ;;
    --years=*)     YEARS="${1#--years=}"; shift ;;
    -h|--help)     sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'; exit 0 ;;
    *)             die "unknown flag: $1 (try --help)" ;;
  esac
done

[[ -x "$VENV_PY" ]] || die "no venv at $VENV_PY — see CLAUDE.md for training/ setup"
[[ -n "${DATABASE_URL:-}" ]] || die "DATABASE_URL is unset; every stage reads the database"

is_in() { local n="$1"; shift; for x in "$@"; do [[ "$x" == "$n" ]] && return 0; done; return 1; }

# ── Resolve which stages run ────────────────────────────
PLAN=()
if [[ -n "$ONLY" ]]; then
  IFS=',' read -ra want <<< "$ONLY"
  for s in "${want[@]}"; do
    is_in "$s" "${ALL_STAGES[@]}" || die "unknown stage '$s'. Known: ${ALL_STAGES[*]}"
    PLAN+=("$s")
  done
else
  started=0
  [[ -z "$FROM" ]] && started=1
  if [[ -n "$FROM" ]]; then
    is_in "$FROM" "${ALL_STAGES[@]}" || die "unknown --from stage '$FROM'"
    # `--from` into Layer 1 implies --with-layer1. "Start here and run
    # everything after" is what the flag means, so `--from trajectory` must
    # not silently drop `freshman` — the stage immediately after it.
    is_in "$FROM" "${LAYER1[@]}" && WITH_LAYER1=1
  fi
  for s in "${ALL_STAGES[@]}"; do
    [[ "$s" == "$FROM" ]] && started=1
    (( started )) || continue
    is_in "$s" "${LAYER1[@]}" && (( ! WITH_LAYER1 )) && continue
    PLAN+=("$s")
  done
fi
(( ${#PLAN[@]} )) || die "empty plan"

echo "→ Repo:     $REPO_ROOT"
echo "→ Seasons:  $YEARS"
echo "→ Plan:     ${PLAN[*]}"
for s in "${PLAN[@]}"; do
  if is_in "$s" "${LAYER1[@]}"; then
    echo "→ WARNING:  '$s' TRUNCATEs and reloads an OOF table. Every Layer 2 model"
    echo "            beneath it becomes stale until roster_impact + roster_adjo rerun."
  fi
done

if (( DRY_RUN )); then
  echo "→ Dry run — nothing executed."
  exit 0
fi

if (( ! ASSUME_YES )); then
  read -r -p "→ Run these stages? Models and database rows will be rewritten. [y/N] " reply
  [[ "$reply" == "y" || "$reply" == "Y" ]] || { echo "aborted."; exit 1; }
fi

# Stamped once so every artifact from one run shares a tag. Captured up front:
# calling $(date) mid-pipeline clobbers $? and has bitten this repo before.
RUN_TAG="$(date -u +%Y%m%d_%H%M%S)"
N_SEASONS="$(awk -F',' '{print NF}' <<< "$YEARS")"
BT_DUMP="$EVAL_DIR/projections_backtest_per_team_full_${N_SEASONS}season_run${RUN_TAG}.json"
WROTE=()

stage_banner() { echo; echo "══ $1 ═══════════════════════════════════════"; }

run_trajectory() {
  stage_banner "trajectory — rewrites trajectory_oof_predictions"
  ( cd "$TRAINING_DIR" && "$VENV_PY" train_trajectory_model.py )
  WROTE+=("training/models/trajectory_{mean,q10,q90}_model.onnx + meta")
}

run_freshman() {
  stage_banner "freshman — rewrites freshman_oof_predictions"
  ( cd "$TRAINING_DIR" && "$VENV_PY" train_freshman_model.py )
  WROTE+=("training/models/freshman_{mean,q10,q90}_model.onnx + meta")
}

run_roster_impact() {
  stage_banner "roster_impact — served net AdjEM calibrator"
  ( cd "$TRAINING_DIR" && "$VENV_PY" train_roster_impact_model.py )
  WROTE+=("training/models/roster_impact_model.onnx + meta")
  WROTE+=("training/models/roster_impact_loso/*.onnx (gitignored, feeds backtest)")
}

run_roster_adjo() {
  stage_banner "roster_adjo — AdjO half (the step #218 was filed about)"
  ( cd "$TRAINING_DIR" && "$VENV_PY" train_roster_adjo_model.py )
  WROTE+=("training/models/roster_adjo_model.onnx + meta")
}

run_backtest() {
  stage_banner "backtest — projections-backtest -> per-team dump"
  ( cd "$REPO_ROOT" && cargo run --release --bin cstat-ingest -- \
      projections-backtest --years "$YEARS" --output "$BT_DUMP" )
  WROTE+=("${BT_DUMP#"$REPO_ROOT"/} (gitignored)")
}

run_cae() {
  stage_banner "cae — coach-above-expectation grades"
  # --dump is load-bearing: compute_cae's fallback picks the newest match by
  # FILENAME, and the historical dumps carry descriptive tags that sort after
  # a plain date. Naming the file we just produced removes the ambiguity about
  # which projection generation these grades were scored against.
  if [[ ! -f "$BT_DUMP" ]]; then
    die "cae needs $BT_DUMP — run the 'backtest' stage first, or pass --from backtest"
  fi
  ( cd "$TRAINING_DIR" && "$VENV_PY" compute_cae.py --dump "$BT_DUMP" --write )
  WROTE+=("coach_season_cae + coach_ratings (database)")
  WROTE+=("training/eval_history/cae_compute_*_summary.json  <-- DO NOT auto-stage")
}

run_projections() {
  stage_banner "projections — materialize team_preseason_projection"
  ( cd "$REPO_ROOT" && cargo run --release --bin cstat-ingest -- \
      compute-projections --years "$YEARS" )
  WROTE+=("team_preseason_projection (database)")
}

for s in "${PLAN[@]}"; do
  "run_${s}"
done

# ── Provenance check ────────────────────────────────────
# Only meaningful once both Layer 2 halves have been rebuilt in this run; a
# partial run legitimately leaves them mismatched mid-flight.
if is_in roster_impact "${PLAN[@]}" && is_in roster_adjo "${PLAN[@]}"; then
  stage_banner "verify — roster-frame provenance stamps agree"
  ( cd "$REPO_ROOT" && cargo test -p cstat-core --lib \
      shipped_roster_models_share_an_oof_snapshot -- --exact --nocapture ) \
    || die "roster_impact and roster_adjo disagree on their OOF snapshot — the API will refuse to boot"
  echo "✓ stamps match"
elif is_in roster_impact "${PLAN[@]}" || is_in roster_adjo "${PLAN[@]}"; then
  echo
  echo "→ NOTE: only one roster-frame half was rebuilt. Their provenance stamps"
  echo "        now disagree and \`Predictor::load\` will refuse to boot until the"
  echo "        other half is retrained. That refusal is the #218 guardrail doing"
  echo "        its job — rerun with both stages."
fi

# ── What happened ───────────────────────────────────────
echo
echo "══ done ═══════════════════════════════════════"
echo "Stages run: ${PLAN[*]}"
echo "Artifacts written:"
for w in "${WROTE[@]}"; do echo "  - $w"; done
cat <<'NOTE'

Artifact policy: nothing above was staged. eval_history summaries are tracked
only when they ride along with the model/code change that produced them; a
deploy-time recompute emits an orphan (that is how one leaked into unrelated
PR #215). Review with `git status` and stage deliberately.

If roster_impact moved, the downstream products built from it did too — commit
the model artifacts and push the regenerated database tables to prod:
  ./scripts/sync_to_prod.sh --tables team_preseason_projection,coach_season_cae,coach_ratings
NOTE
