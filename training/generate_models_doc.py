"""Generate `docs/MODELS.md` from the model metas (#364).

The tree's *description* used to live in four methodology docs, the trainer
docstrings' v1→v4 narratives, and PR bodies — prose that drifts from the
artifacts it describes (`projections-backtest` printed "still uses live
freshman inference" for months after the compose path switched to the
held-out OOF). This script reads what the trainers actually stamped into
`training/models/*_meta.json` and writes one page: per model, its inputs
(`input_provenance`), target, rows, features, evaluation protocol with the
headline numbers, last retrain date, and a short hand-maintained
"known limits" block kept HERE, next to the code that renders it.

Everything numeric on the page comes from a meta. The only hand-written text
is the `SPECS` table below (description, protocol, known limits) — edit it,
rerun, commit the page.

    cd training && ./.venv/bin/python generate_models_doc.py           # write docs/MODELS.md
    cd training && ./.venv/bin/python generate_models_doc.py --check   # exit 1 if the page is stale

`retrain_downstream.sh` regenerates the page at the end of every run, and CI
runs `--check` so a meta committed without its page fails the build. The
output is deterministic: no wall-clock timestamps, so `--check` compares
bytes. The retrain date is the trainer's `trained_at` stamp; metas older than
that stamp are reported as such rather than guessed from git.
"""

from __future__ import annotations

import argparse
import difflib
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path

TRAINING_DIR = Path(__file__).resolve().parent
REPO_ROOT = TRAINING_DIR.parent
MODEL_DIR = Path(os.environ.get("MODEL_DIR", TRAINING_DIR / "models"))
DEFAULT_OUT = REPO_ROOT / "docs" / "MODELS.md"

# `provenance.SOURCES` carries the one-line note for each fingerprinted input,
# which is what makes the inputs table readable. Its import chain reaches
# sqlalchemy (through `db`), so this needs the training venv — deliberately a
# hard import rather than a fallback with blank notes, because `--check`
# compares bytes and a page that depends on which interpreter rendered it
# would fail CI for no reason anyone could see.
from provenance import SOURCES as _SOURCES

SOURCE_NOTES = {name: src.notes for name, src in _SOURCES.items()}
SOURCE_NIGHTLY = {name: src.nightly for name, src in _SOURCES.items()}


# ---------------------------------------------------------------------------
# What is hand-maintained. Keep each entry short and current; the history of
# how a model got here belongs in the methodology doc it links to.
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Spec:
    key: str
    title: str
    layer: str
    meta_file: str
    trainer: str
    artifacts: tuple[str, ...]
    served_by: str
    doc: str
    description: str
    known_limits: tuple[str, ...] = field(default_factory=tuple)
    #: The evaluation protocol in one or two sentences — the numbers come from
    #: the meta, but what they measure does not, and that is the part a new
    #: reader has to be told.
    protocol: str = ""
    #: For metas that carry no `target` key (the game models).
    target: str = ""


SPECS: tuple[Spec, ...] = (
    Spec(
        key="trajectory",
        title="Trajectory model (returner CamPom, season N+1)",
        layer="1",
        meta_file="trajectory_model_meta.json",
        trainer="`training/train_trajectory_model.py`",
        artifacts=("trajectory_mean_model.onnx", "trajectory_q10_model.onnx", "trajectory_q90_model.onnx"),
        served_by="`cstat_core::trajectory` — PlayerDetail projection band, the roster projection's returner and arrival channel, the transfers page",
        doc="docs/trajectory_methodology.md",
        description=(
            "Three LightGBMs (mean, q10, q90) mapping a player's season-N features "
            "to his season-N+1 `cam_gbpm_v3_psos`. One row per consecutive-season "
            "pair keyed on `torvik_pid`, transfers included. The feature block is "
            "prior-season box/rate stats, GBPM components, a two-season history "
            "block (lag-2 levels + slope), prior-season on/off, archetype mixture, "
            "the recruit-rank block, and a five-feature destination block (where the "
            "player plays in N+1). Missing values are sentinels, never NaN, because "
            "the ONNX serve path has no NaN plumbing."
        ),
        protocol=(
            "Walk-forward (train on pairs whose target season is strictly earlier "
            "than S, test on S, S = `walk_from`..) is the headline; leave-one-pair-out "
            "(LOPO) is kept for continuity with older numbers. Both are per-player "
            "CamPom MAE. The naive baseline is `cam(N+1) = cam(N)`."
        ),
        known_limits=(
            "The destination block carries the destination program's strength, not the role the player will fill there: a mid-major star who becomes the go-to option at an elite program (Lendeborg 2026, Knecht 2024, Boyd 2025) is still under-projected. It closes about half the elite-team gap.",
            "Selection bias on returners: the training rows are players who came back for N+1; the leave-for-the-draft cohort is not modelled.",
            "Portal-era sign flip: elite-destination returners were over-projected 2021–23 and under-projected 2024+, and an era feature is a tie walk-forward — three seasons of the new regime are not enough to learn it (`training/experiments/experiment_elite_gap.py`).",
            "The trainer writes its held-out predictions to `trajectory_oof_predictions` (TRUNCATE + reload); a retrain that skipped that write would serve in-sample projections, which is why the boot validator requires `oof_persisted`.",
        ),
    ),
    Spec(
        key="freshman",
        title="Freshman model (recruit first-season CamPom)",
        layer="1",
        meta_file="freshman_model_meta.json",
        trainer="`training/train_freshman_model.py`",
        artifacts=("freshman_mean_model.onnx", "freshman_q10_model.onnx", "freshman_q90_model.onnx"),
        served_by="`roster_projection::freshman_row` — the only freshman signal in the roster projection; the recruits page",
        doc="docs/projections_methodology.md",
        description=(
            "Per-recruit LightGBM (mean, q10, q90) from the shared 11-feature "
            "`recruit_features` block plus two signing-time context features "
            "(`committed_team_prior_adjem`, `peer_class_strength`) to the recruit's "
            "first-season `cam_gbpm_v3_psos`. The mean model carries monotone "
            "constraints on composite rating and star rating so a better-rated "
            "recruit never projects lower with everything else fixed."
        ),
        protocol=(
            "Walk-forward by class (train on classes strictly earlier than the "
            "test season) is the headline; leave-one-class-out (LOCO) is kept for "
            "continuity. Per-player CamPom MAE. The baseline is the rank-tier mean."
        ),
        known_limits=(
            "Selection bias at the top: elite freshmen leave for the draft, so the calibrated cohort skews toward those who stayed and played meaningful minutes. Top-30 projections come from a thinner, more variable population than the headline MAE suggests.",
            "Unrated internationals and generational 5-stars are the 2024+ misses; a destination pre-check found no destination signal to add (the freshman model already reads the signing team's prior AdjEM).",
            "Sample size below about the 30th-ranked recruit drops fast; surface the q10–q90 band, not the mean alone.",
        ),
    ),
    Spec(
        key="roster_impact",
        title="Roster-impact calibrator (team AdjEM, served net)",
        layer="2",
        meta_file="roster_impact_model_meta.json",
        trainer="`training/train_roster_impact_model.py`",
        artifacts=("roster_impact_model.onnx",),
        served_by="`routes/projections.rs` and `cstat-ingest compute-projections` — the Future page's projected AdjEM, the opening-week preseason anchor in `/api/predict`, the denominator of the coach grades",
        doc="docs/projections_methodology.md",
        description=(
            "Ordinary least squares on three CAM aggregates of the projected "
            "roster (`cam_wmean`, `cam_top3_mean`, `cam_top1`), exported on the "
            "27-slot ONNX contract with zero coefficients on the 24 unused "
            "features so the serve path and boot validator are untouched (#363). "
            "It trains on the SERVED composition of every historical team-season "
            "— returners less departures, plus portal arrivals and ranked "
            "recruits, each carrying its Layer 1 held-out projection — cut by "
            "`cstat-ingest projections-backtest --frame-out`. The blend on top "
            "(program anchor, turnover ramp) is Layer 4 and lives in Rust."
        ),
        protocol=(
            "Walk-forward (train < S, test S) on the raw calibrator and through the "
            "served blend; leave-one-season-out on the same rows for the optimism "
            "column. Team AdjEM MAE. The per-target-season LOSO models are also "
            "exported (gitignored) so `projections-backtest` scores honestly."
        ),
        known_limits=(
            "The remaining elite gap is upstream: ex-ante top-10 programs in 2024+ are still under-projected by about 4 AdjEM, and it decomposes to −0.4 per player at elite destinations plus a handful of generational recruits (`training/experiments/experiment_elite_gap.py`). Nothing on the calibrator moves it walk-forward.",
            "The frame is a file (`training/frames/roster_impact_ex_ante.json`, gitignored). A Layer 1 retrain that skips the `frame` stage trains the calibrators on stale projections; the trainer compares the frame's OOF snapshot to the live tables and refuses, but only if it is run.",
            "Both Layer 2 halves must carry the same `oof_provenance` stamp or the API refuses to boot (#218). Retraining this without `roster_adjo` is a hard failure, not a silent one.",
            "The served Layer 4 constants (`PROJECTION_SHRINK_WEIGHT`, `_OVERHAUL`, `PROGRAM_ANCHOR_SHRINK`) are re-searched inside each walk-forward fold and stamped as `walk_forward.constants_refit`; the three `predict.rs` blend constants are not (#236).",
        ),
    ),
    Spec(
        key="roster_adjo",
        title="Roster-impact AdjO half (display split)",
        layer="2",
        meta_file="roster_adjo_model_meta.json",
        trainer="`training/train_roster_adjo_model.py`",
        artifacts=("roster_adjo_model.onnx",),
        served_by="`routes/projections.rs` — projected AdjO on the Future page, run live per request; AdjD is derived as AdjO − AdjEM",
        doc="docs/projections_methodology.md",
        description=(
            "Same 27-feature frame as the net calibrator, target = next-season "
            "`adj_offense` RELATIVE to the base season's league mean (#368); the "
            "serve path adds the base season's mean back. NET + SPLIT: the net "
            "headline is never touched by this model. Display-only."
        ),
        protocol=(
            "Walk-forward (train < S, test S) through the served blend for the AdjO "
            "half; leave-one-season-out kept for continuity. Team AdjO MAE; the "
            "naive baseline is last season's AdjO."
        ),
        known_limits=(
            "Reaches prod by git deploy only — `team_preseason_projection` has no AdjO column, so no data sync can move it. That is how a stale copy survived months of routine syncs (#218).",
            "Needs its own invocation. It imports `build_dataset` from the net trainer, which reads as 'the AdjO half updates itself'; it does not.",
            "Exports no per-season LOSO ONNX, so running it alone leaves `projections-backtest` reading the previous net models.",
        ),
    ),
    Spec(
        key="game",
        title="Game models (margin / win / total)",
        layer="game",
        meta_file="model_meta.json",
        trainer="`training/train.py` + `training/export_onnx.py`",
        artifacts=("margin_model.onnx", "win_model.onnx", "total_model.onnx", "margin_model.lgb"),
        served_by="`cstat_core::inference::Predictor` — `/api/predict` without `as_of_date`, the leaky whole-season path",
        doc="docs/model_performance.md",
        description=(
            "LightGBM margin regression, win-probability classifier and total-points "
            "regression on the 49 `diff_*` features `features.py` builds from team "
            "stats, roster aggregates (season-aggregate CamPom), rolling form and "
            "context; the total model adds `sum_*` level companions. The `.lgb` "
            "mirror of the margin model is kept for TreeSHAP explanations."
        ),
        protocol=(
            "Chronological backtest: train on the first 80% of the corpus by date, "
            "score the last 20%. Margin/total in points; win in accuracy, log loss, AUC."
        ),
        target="home − away margin; P(home win); home + away total",
        known_limits=(
            "Not in the retrain chain and not fingerprinted: no `input_provenance`, no `trained_at`, and `train.py` was not part of the #222 reproducibility work, so rerunning it on unchanged data produces different bytes. Do not retrain it 'to be safe'.",
            "Trains on season-aggregate CamPom, which leaks the season being predicted; the `pit_*` twins below are the honest in-season path.",
            "The only boot gate is the feature-vector width (`FeatureCountMismatch` on the `.lgb`); there is no meta-drift validator as thorough as the roster branch's.",
        ),
    ),
    Spec(
        key="pit",
        title="Point-in-time game models (pit_margin / pit_win / pit_total)",
        layer="game",
        meta_file="pit_model_meta.json",
        trainer="`training/train.py` with `GBPM_VARIANT=pit_cam_v3` (into `models_experiments/pit_cam_v3/`, then copied here with the `pit_` prefix)",
        artifacts=("pit_margin_model.onnx", "pit_win_model.onnx", "pit_total_model.onnx", "pit_margin_model.lgb"),
        served_by="`Predictor` — `/api/predict?as_of_date=`, the preseason×pit blend over opening week, `game_projections` and the TeamDetail projected column",
        doc="docs/model_performance.md",
        description=(
            "The same three models trained with the CamPom channel swapped for a "
            "point-in-time grid (`build_pit_lookup.py`, asof-merged), so an "
            "in-season prediction reads only what was known on the date."
        ),
        protocol="Same chronological 80/20 backtest as the whole-season models, on the pit feature matrix.",
        target="home − away margin; P(home win); home + away total",
        known_limits=(
            "Only the CamPom channel is point-in-time; the other 41 features are season aggregates, a train/serve skew tracked in #274.",
            "Promoted by copying files out of `models_experiments/pit_cam_v3/` with a `pit_` prefix — there is no trainer flag that writes these names, so a retrain is a manual step.",
            "The point-in-time map is flattened into the SQL `UNNEST` in sorted `player_id` order and the sort is load-bearing: `HashMap` iteration order moved the last bits of the minutes-weighted aggregates and flipped splits (#266).",
        ),
    ),
    Spec(
        key="roster_model",
        title="Legacy box-score roster model (dead)",
        layer="legacy",
        meta_file="roster_model_meta.json",
        trainer="`training/train_roster_model.py`",
        artifacts=("roster_model.onnx",),
        served_by="nothing — deliberately NOT loaded at boot; materialized lazily only by `projections-backtest`'s box-score comparison, which was dropped",
        doc="docs/projections_methodology.md",
        description=(
            "LightGBM from 36 minutes-weighted box-score aggregates to team AdjEM, "
            "with CamPom deliberately excluded (Σ cam × minute share ≈ AdjEM would "
            "collapse it to the identity). Its consumer, the freshman statline, was "
            "deprecated; removal is tracked in the ROADMAP Refactor Backlog."
        ),
        protocol="Leave-one-season-out and 5-fold CV, team AdjEM MAE.",
        known_limits=(
            "Committed but unused. Its meta is checked lazily (`validate_box_score_model_meta`) rather than at boot, so its absence or drift cannot block the API.",
        ),
    ),
)

#: Layer headings, in the order the page lists them.
LAYERS: tuple[tuple[str, str, str], ...] = (
    ("1", "Layer 1 — player projection models", "These WRITE the OOF tables Layer 2 trains on. Retraining one TRUNCATEs its OOF table and invalidates every Layer 2 model beneath it."),
    ("2", "Layer 2 — team calibrators", "Trained on Layer 1's held-out PREDICTIONS, not on actual player value, so they absorb the upstream bias rather than compounding it. The failure mode is desynchronization: a Layer 1 retrain with no Layer 2 retrain."),
    ("game", "Game-outcome branch", "Hangs off Layer 0 directly — no edge into the roster tree. A roster-tree retrain never requires retraining these."),
    ("legacy", "Legacy", ""),
)


# ---------------------------------------------------------------------------
# Rendering helpers
# ---------------------------------------------------------------------------

def _f(x, nd: int = 3) -> str:
    """Fixed-precision float, or `—` for None."""
    if x is None:
        return "—"
    return f"{x:.{nd}f}"


def _signed(x, nd: int = 2) -> str:
    return "—" if x is None else f"{x:+.{nd}f}"


def _int(x) -> str:
    return "—" if x is None else f"{int(x):,}"


def _load_meta(spec: Spec) -> dict | None:
    path = MODEL_DIR / spec.meta_file
    if not path.exists():
        return None
    return json.loads(path.read_text())


def _trained_at(meta: dict, short: bool = False) -> str:
    """The retrain date a reader can trust.

    `trained_at` is stamped by the trainers since #364. Before that the only
    timestamp any meta carried was the Layer 2 frame's `generated_at`, which is
    the same run in a chain retrain, so it is accepted as a labelled fallback.
    Anything older is reported as unstamped — the git date of the meta is not
    used because a shallow CI checkout would print a different one and break
    `--check`.
    """
    if meta.get("trained_at"):
        return str(meta["trained_at"])[:10]
    frame = (meta.get("cam_v3_coverage") or {}).get("frame_provenance") or {}
    if frame.get("generated_at"):
        day = str(frame["generated_at"])[:10]
        return day if short else f"{day} (frame cut; `trained_at` not stamped)"
    return "not stamped" if short else "not stamped — predates the `trained_at` stamp (#364); the next retrain records it"


def _target(spec: Spec, meta: dict) -> str:
    return str(meta.get("target") or spec.target or "—")


def _seasons(meta: dict) -> str:
    for key in ("seasons_trained_on", "seasons", "training_classes"):
        if key in meta and meta[key]:
            vals = meta[key]
            label = "classes" if key == "training_classes" else "seasons"
            return f"{len(vals)} {label}, {vals[0]}–{vals[-1]}"
    return "—"


def _headline(spec: Spec, meta: dict) -> str:
    """The one number for the scorecard row."""
    wf = meta.get("walk_forward") or {}
    if spec.key == "roster_impact" and "served" in wf:
        served = wf["served"]["cohorts"]["all"]["wf served"]["mae"]
        raw = wf["served"]["cohorts"]["all"]["wf raw"]["mae"]
        return f"walk-forward served MAE {_f(served)} (raw {_f(raw)})"
    if "pooled" in wf:
        return f"walk-forward MAE {_f(wf['pooled']['mae'])}"
    if "backtest_margin" in meta:
        return f"margin MAE {_f(meta['backtest_margin']['mae'], 2)}, win acc {_f(meta['backtest_win']['accuracy'], 3)}"
    if "backtest_loso" in meta and "pooled" in meta["backtest_loso"]:
        return f"LOSO MAE {_f(meta['backtest_loso']['pooled']['mae'])}"
    return "—"


def _render_inputs(meta: dict) -> list[str]:
    prov = meta.get("input_provenance")
    if not prov:
        return ["No `input_provenance` stamp — this model is outside the fingerprint chain (`check_provenance.py` cannot say whether it is stale)."]
    out = [
        "Fingerprinted at fit time (`training/provenance.py`); `check_provenance.py` recomputes these against the live database.",
        "",
        "| source | what it is | rows | digest | nightly-rewritten |",
        "|---|---|---:|---|---|",
    ]
    for name, entry in prov.items():
        note = SOURCE_NOTES.get(name, "")
        nightly = "yes" if SOURCE_NIGHTLY.get(name) else "no"
        out.append(f"| `{name}` | {note} | {_int(entry.get('n_rows'))} | `{str(entry.get('digest', ''))[:12]}` | {nightly} |")
    oof = meta.get("oof_provenance")
    if oof:
        out += ["", "OOF snapshot the frame was cut from (the #218 boot stamp; both Layer 2 halves must agree):", ""]
        for name, entry in oof.items():
            out.append(f"- `{name}`: {_int(entry.get('n_rows'))} rows, `{str(entry.get('digest', ''))[:12]}`")
    cov = meta.get("cam_v3_coverage") or {}
    if cov.get("frame"):
        fp = cov.get("frame_provenance") or {}
        out += [
            "",
            f"Training frame: `training/frames/{cov['frame']}` (sha256 `{str(cov.get('frame_sha256', ''))[:12]}…`, "
            f"{_int(cov.get('n_rows'))} team-seasons, produced by `{fp.get('produced_by', '?')}`). "
            f"Composition: {fp.get('composition', '?')}.",
        ]
    return out


def _render_features(meta: dict) -> list[str]:
    feats = meta.get("features") or []
    out = [f"{len(feats)} features."]
    if meta.get("model_family") == "linear":
        lf = meta.get("linear_features") or []
        coef = meta.get("coefficients") or {}
        out += [
            "",
            f"Linear: only {len(lf)} carry weight; the other {len(feats) - len(lf)} slots are exported with zero coefficients to keep the ONNX contract.",
            "",
            "| term | coefficient |",
            "|---|---:|",
            f"| intercept | {_f(coef.get('intercept'), 4)} |",
        ]
        for name in lf:
            out.append(f"| `{name}` | {_f(coef.get(name), 4)} |")
    top = meta.get("top_features") or []
    if top:
        out += ["", "Top gain importance (from the fit):", ""]
        out += [f"- `{t['name']}` ({int(t['importance'])})" for t in top[:8]]
    out += [
        "",
        "<details><summary>Full feature list (serve-order contract)</summary>",
        "",
        ", ".join(f"`{f}`" for f in feats),
        "",
        "</details>",
    ]
    if meta.get("total_features"):
        out += [
            "",
            f"The total model adds level companions: {len(meta['total_features'])} features in all.",
        ]
    sentinels = {
        k: v for k, v in meta.items() if k.endswith("_sentinel")
    }
    notes = []
    if sentinels:
        notes.append("- Missing-value sentinels: " + ", ".join(f"`{k}` = {v}" for k, v in sentinels.items()) + ".")
    for k, label in (("onoff_coverage", "on/off block coverage"), ("prior2_coverage", "lag-2 history coverage")):
        if k in meta:
            notes.append(f"- {label}: {meta[k] * 100:.1f}% of rows.")
    if notes:
        out += [""] + notes
    return out


def _per_season_table(per: dict, cols: tuple[tuple[str, str, int], ...]) -> list[str]:
    head = "| season | " + " | ".join(c[1] for c in cols) + " |"
    sep = "|---|" + "|".join("---:" for _ in cols) + "|"
    rows = [head, sep]
    for season in sorted(per, key=lambda s: str(s)):
        block = per[season]
        cells = []
        for key, _, nd in cols:
            v = block.get(key)
            cells.append(_int(v) if key == "n" else (_signed(v, nd) if key == "bias" else _f(v, nd)))
        rows.append(f"| {season} | " + " | ".join(cells) + " |")
    return rows


def _render_eval(spec: Spec, meta: dict) -> list[str]:
    out: list[str] = []
    if spec.protocol:
        out += [spec.protocol, ""]
    wf = meta.get("walk_forward")
    if wf:
        pooled = wf.get("pooled")
        out.append(f"**Walk-forward** (test seasons from {wf.get('walk_from')}; the canonical judge, #361).")
        if wf.get("protocol"):
            out.append(f"Protocol: {wf['protocol']}.")
        out.append("")
        if pooled:
            out += [
                "| pooled | MAE | RMSE | R² | bias | n |",
                "|---|---:|---:|---:|---:|---:|",
                f"| walk-forward | {_f(pooled['mae'])} | {_f(pooled['rmse'])} | {_f(pooled['r2'])} | {_signed(pooled.get('bias'))} | {_int(pooled['n'])} |",
            ]
            for key, label in (("lopo_same_rows", "LOPO, same rows"), ("loco_same_rows", "LOCO, same rows")):
                same = wf.get(key)
                if same:
                    out.append(f"| {label} | {_f(same['mae'])} | {_f(same['rmse'])} | {_f(same['r2'])} | {_signed(same.get('bias'))} | {_int(same['n'])} |")
                    out.append(f"| optimism (walk-forward − same rows) | {_signed(pooled['mae'] - same['mae'], 3)} | | | | |")
            out.append("")
        if wf.get("per_season"):
            out += _per_season_table(
                wf["per_season"],
                (("mae", "MAE", 3), ("rmse", "RMSE", 3), ("r2", "R²", 3), ("bias", "bias", 2), ("n", "n", 0)),
            )
            out.append("")
        # The net calibrator's block is shaped differently: a raw table plus the
        # served-blend cohort table that is the number the site is judged on.
        if "raw" in wf and "served" in wf:
            raw = wf["raw"]["pooled"]
            same = wf.get("raw_loso_same_rows") or {}
            out += [
                f"Raw calibrator (before the blend), walk-forward from {wf['raw'].get('walk_from')}:",
                "",
                "| pooled | MAE | RMSE | R² | bias | n |",
                "|---|---:|---:|---:|---:|---:|",
                f"| walk-forward raw | {_f(raw['mae'])} | {_f(raw['rmse'])} | {_f(raw['r2'])} | {_signed(raw.get('bias'))} | {_int(raw['n'])} |",
            ]
            if same:
                out.append(f"| LOSO raw, same rows ({wf.get('walk_from')}+) | {_f(same['mae'])} | {_f(same['rmse'])} | {_f(same['r2'])} | {_signed(same.get('bias'))} | {_int(same['n'])} |")
            out.append("")
            out += _per_season_table(
                wf["raw"]["per_season"],
                (("mae", "MAE", 3), ("rmse", "RMSE", 3), ("r2", "R²", 3), ("bias", "bias", 2), ("n", "n", 0)),
            )
            served = wf["served"]
            names = served["variants"]
            out += [
                "",
                f"**Served projection** (raw + the Layer 4 blend), test {wf['walk_from']}+, n={_int(served['n'])}. "
                "Cell: MAE / bias. `wf served` is the number the site is judged on.",
                "",
                "| cohort | n | " + " | ".join(names) + " |",
                "|---|---:|" + "|".join("---:" for _ in names) + "|",
            ]
            for cname, block in served["cohorts"].items():
                out.append(
                    f"| {cname.replace('|', chr(92) + '|')} | {_int(block['n'])} | "
                    + " | ".join(f"{_f(block[v]['mae'])} / {_signed(block[v]['bias'])}" for v in names)
                    + " |"
                )
            out += [
                "",
                "| ordering metric (per-season mean) | " + " | ".join(names) + " |",
                "|---|" + "|".join("---:" for _ in names) + "|",
            ]
            for label, vals in served["rank_metrics"].items():
                out.append(f"| {label} | " + " | ".join(_f(vals[v]) for v in names) + " |")
            out += [
                "",
                "Paired |error| against `wf served` (delta, z; negative = the variant is better):",
                "",
                "| variant | pooled | top-25 | top-10 | level-changers |",
                "|---|---:|---:|---:|---:|",
            ]
            for v, blocks in served["paired"].items():
                out.append(
                    f"| {v} | " + " | ".join(f"{_signed(b['delta'], 3)} (z {_signed(b['z'], 1)})" for b in blocks.values()) + " |"
                )
            c = wf.get("constants_refit")
            if c:
                s = c["served"]
                out += [
                    "",
                    f"Layer 4 constants re-searched inside each fold on earlier walk-forward rows, against the served "
                    f"`w_stable` {s['w_stable']:.2f} / `w_overhaul` {s['w_overhaul']:.2f} / `shrink` {s['shrink']:.2f}. "
                    "A refit that beats the served values out of sample is the signal to change them; one that merely differs is not.",
                    "",
                    "| test season | refit w_stable | refit w_overhaul | refit shrink | test MAE refit | test MAE served |",
                    "|---|---:|---:|---:|---:|---:|",
                ]
                for season, f in c["per_fold"].items():
                    out.append(
                        f"| {season} | {f['w_stable']:.2f} | {f['w_overhaul']:.2f} | {f['shrink']:.2f} | {_f(f['test_mae_refit'])} | {_f(f['test_mae_served'])} |"
                    )
            out.append("")

    # The train-on-everything-else family, kept for continuity.
    for key, label in (
        ("backtest_lopo", "Leave-one-pair-out"),
        ("loco_cv", "Leave-one-class-out"),
        ("backtest_loso", "Leave-one-season-out"),
    ):
        block = meta.get(key)
        if not block:
            continue
        pooled = block.get("pooled") or {}
        if "pooled_mae" in block:  # roster_adjo's shape
            pooled = {"mae": block["pooled_mae"]}
        out.append(f"**{label}** (trains on later seasons too; optimistic, kept for continuity with older numbers).")
        out.append("")
        cells = [f"MAE {_f(pooled.get('mae'))}"]
        if pooled.get("rmse") is not None:
            cells.append(f"RMSE {_f(pooled['rmse'])}")
        if pooled.get("r2") is not None:
            cells.append(f"R² {_f(pooled['r2'])}")
        if pooled.get("n") is not None:
            cells.append(f"n {_int(pooled['n'])}")
        if block.get("naive_mae") is not None:
            cells.append(f"naive (last season) MAE {_f(block['naive_mae'])}")
        out.append("Pooled: " + ", ".join(cells) + ".")
        out.append("")

    base = meta.get("baseline_naive")
    if base:
        out.append(f"Naive baseline (`value(N+1) = value(N)`): MAE {_f(base['mae'])}, RMSE {_f(base['rmse'])}, R² {_f(base['r2'])}.")
        out.append("")
    tier = meta.get("tier_mean_baseline")
    if tier:
        out.append(
            f"Rank-tier mean baseline (tiers at ranks {', '.join(str(t) for t in meta.get('tier_thresholds', []))}): "
            f"MAE {_f(tier['mae'])}, RMSE {_f(tier['rmse'])}, R² {_f(tier['r2'])}."
        )
        out.append("")
    cv = meta.get("cv_5fold")
    if cv and cv.get("mae") is not None:
        out.append(f"5-fold CV (random folds, in-sample seasons): MAE {_f(cv['mae'])}, RMSE {_f(cv.get('rmse'))}, R² {_f(cv.get('r2'))}.")
        out.append("")

    # Game-model backtests.
    if "backtest_margin" in meta:
        m, w, t = meta["backtest_margin"], meta["backtest_win"], meta["backtest_total"]
        out += [
            f"Chronological holdout, {_int(meta.get('n_games'))} games, CamPom variant `{meta.get('gbpm_variant')}`:",
            "",
            "| model | MAE | RMSE | R² | accuracy | log loss | AUC |",
            "|---|---:|---:|---:|---:|---:|---:|",
            f"| margin | {_f(m['mae'], 2)} | {_f(m['rmse'], 2)} | {_f(m['r2'])} | {_f(m.get('accuracy'))} | | |",
            f"| win | | | | {_f(w['accuracy'])} | {_f(w['log_loss'])} | {_f(w['auc'])} |",
            f"| total | {_f(t['mae'], 2)} | {_f(t['rmse'], 2)} | {_f(t['r2'])} | | | |",
            "",
        ]

    # Trajectory's bucket diagnostics.
    by_cam = meta.get("mae_by_current_campom")
    if by_cam:
        out += [
            "MAE by prior-season CamPom bucket (LOPO; where the regression toward the mean lives):",
            "",
            "| bucket | n | mean prior | mean pred | mean actual | model MAE | model bias | naive MAE |",
            "|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
        for label, b in by_cam.items():
            out.append(
                f"| {label} | {_int(b['n'])} | {_f(b['mean_prior'], 2)} | {_f(b['mean_pred'], 2)} | {_f(b['mean_actual'], 2)} | "
                f"{_f(b['model_mae'])} | {_signed(b['model_bias'])} | {_f(b['naive_mae'])} |"
            )
        out.append("")
    return out


def _render_model(spec: Spec, meta: dict | None) -> list[str]:
    out = [f"### {spec.title}", ""]
    if meta is None:
        out += [f"**`{MODEL_DIR / spec.meta_file}` is missing — the model is not trained on this checkout.**", ""]
        return out
    present = [a for a in spec.artifacts if (MODEL_DIR / a).exists()]
    missing = [a for a in spec.artifacts if a not in present]
    out += [
        spec.description,
        "",
        f"- **Trainer:** {spec.trainer}",
        f"- **Artifacts:** " + ", ".join(f"`{a}`" for a in present) + (f" (missing on this checkout: {', '.join(f'`{a}`' for a in missing)})" if missing else ""),
        f"- **Meta:** `training/models/{spec.meta_file}`",
        f"- **Served by:** {spec.served_by}",
        f"- **Methodology:** `{spec.doc}`",
        f"- **Last retrain:** {_trained_at(meta)}",
        "",
        "#### Target and rows",
        "",
        f"- **Target:** `{_target(spec, meta)}`" + (f"; add-back at serve: {meta['serve_add_back']}" if meta.get("serve_add_back") else ""),
    ]
    if meta.get("join_key"):
        out.append(f"- **Join key:** `{meta['join_key']}`")
    out.append(f"- **Training span:** {_seasons(meta)}")
    if meta.get("n_rows") is not None:
        out.append(f"- **Rows:** {_int(meta['n_rows'])}")
    if meta.get("n_games") is not None:
        out.append(f"- **Rows:** {_int(meta['n_games'])} games")
    if meta.get("player_filter"):
        out.append(f"- **Qualification gate:** `{meta['player_filter']}`")
    if meta.get("quantile_alphas"):
        qa = meta["quantile_alphas"]
        out.append(f"- **Band models:** quantiles {', '.join(f'{k}={v}' for k, v in qa.items())}")
    if meta.get("decomposition"):
        out.append(f"- **Decomposition:** {meta['decomposition']}")
    if meta.get("oof_persisted") is not None:
        out.append(f"- **Held-out predictions persisted:** {'yes' if meta['oof_persisted'] else 'NO — the boot validator will refuse this meta'}")
    out += ["", "#### Inputs", ""] + _render_inputs(meta)
    out += ["", "#### Features", ""] + _render_features(meta)
    out += ["", "#### Evaluation", ""] + _render_eval(spec, meta)
    out += ["#### Known limits", ""]
    if spec.known_limits:
        out += [f"- {k}" for k in spec.known_limits]
    else:
        out.append("- (none recorded)")
    out.append("")
    return out


def render() -> str:
    metas = {spec.key: _load_meta(spec) for spec in SPECS}
    lines = [
        "# Models",
        "",
        "**GENERATED — do not edit by hand.** Written by `training/generate_models_doc.py` "
        "from `training/models/*_meta.json`; `retrain_downstream.sh` regenerates it at the end of "
        "every run and CI fails when it is stale (`generate_models_doc.py --check`). Every number "
        "on this page was stamped by a trainer. The prose under **Known limits** and each model's "
        "description is the one hand-maintained part, and it lives in the script, next to the "
        "renderer.",
        "",
        "For the shape of the tree — why Layer 2 trains on Layer 1's predictions, what reaches prod "
        "by deploy versus sync, the retrain protocol — read `docs/model_dependency_graph.md`. For "
        "what was tried and rejected, `training/experiments/README.md`. To ask which of these is "
        "stale against the live database: `cd training && ./.venv/bin/python check_provenance.py`.",
        "",
        "## Scorecard",
        "",
        "| model | layer | target | rows | features | headline | last retrain |",
        "|---|---|---|---:|---:|---|---|",
    ]
    for spec in SPECS:
        meta = metas[spec.key]
        if meta is None:
            lines.append(f"| {spec.title} | {spec.layer} | — | — | — | meta missing | — |")
            continue
        rows = meta.get("n_rows", meta.get("n_games"))
        lines.append(
            f"| [{spec.title}](#{_anchor(spec.title)}) | {spec.layer} | `{_target(spec, meta).split(' (')[0]}` | "
            f"{_int(rows)} | {_int(meta.get('n_features'))} | {_headline(spec, meta)} | {_trained_at(meta, short=True)} |"
        )
    lines += [
        "",
        "Player-level numbers are CamPom (`cam_gbpm_v3_psos`) MAE; team-level numbers are AdjEM / AdjO MAE; "
        "game numbers are points. Walk-forward means trained on seasons strictly earlier than the one scored.",
        "",
        "## Not on this page",
        "",
        "- **Archetypes** (Layer 0.5). The fit lives in the database (`archetype_models`), not in a meta file; the "
        "Rust assign half runs nightly. `docs/archetypes_methodology.md`.",
        "- **Layer 3 derived products** (`team_preseason_projection`, the backtest dumps, `coach_season_cae`). Rows "
        "and files, not fits; they record their producing artifact in `artifact_provenance` (migration 047) and "
        "`check_provenance.py` compares ONNX digests.",
        "- **Layer 4 serving constants** (`PROJECTION_SHRINK_WEIGHT`, `PRESEASON_PEAK_WEIGHT`, …). Rust `const`s "
        "tuned by hand off a diagnostic; the roster-impact section below shows the in-fold refit the chain now "
        "runs for three of them. `docs/model_dependency_graph.md` §3.",
        "",
    ]
    for layer_key, heading, blurb in LAYERS:
        lines += [f"## {heading}", ""]
        if blurb:
            lines += [blurb, ""]
        for spec in SPECS:
            if spec.layer == layer_key:
                lines += _render_model(spec, metas[spec.key])
    return "\n".join(lines).rstrip() + "\n"


def _anchor(title: str) -> str:
    """GitHub's heading-anchor rule, close enough for the titles here."""
    keep = []
    for ch in title.lower():
        if ch.isalnum() or ch in " -_":
            keep.append(ch)
    return "".join(keep).strip().replace(" ", "-")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT, help=f"where to write (default {DEFAULT_OUT})")
    ap.add_argument("--check", action="store_true", help="exit 1 if the page on disk differs from what the metas generate")
    args = ap.parse_args()

    page = render()
    if args.check:
        if not args.out.exists():
            print(f"✗ {args.out} does not exist — run generate_models_doc.py", file=sys.stderr)
            return 1
        current = args.out.read_text()
        if current == page:
            print(f"✓ {args.out.relative_to(REPO_ROOT) if args.out.is_relative_to(REPO_ROOT) else args.out} matches the metas")
            return 0
        diff = difflib.unified_diff(
            current.splitlines(keepends=True), page.splitlines(keepends=True),
            fromfile=str(args.out), tofile="generated from training/models/*_meta.json", n=1,
        )
        sys.stdout.writelines(list(diff)[:200])
        print(f"\n✗ {args.out} is stale — regenerate with `cd training && ./.venv/bin/python generate_models_doc.py`", file=sys.stderr)
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(page)
    print(f"Wrote {args.out} ({len(page.splitlines())} lines)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
