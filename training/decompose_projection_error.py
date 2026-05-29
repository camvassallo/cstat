"""Decompose Phase B projection error into upstream-projection vs.
calibrator-residual components.

Question the script answers: when the pipeline projects Maryland at +21
and they finish at +5 (over by 16), how much of that miss is the
trajectory/freshman model getting cam_v3 wrong for the projected roster
vs. the roster-impact calibrator itself under-shooting given accurate
cam_v3 inputs?

Decomposition (signs are `pred − actual`, so positive = over-projected):

  err_pipeline  = phase_b − actual         (current pipeline output)
  err_calibrator = oracle_roster − actual  (model given the ACTUAL N-season
                                            roster + ACTUAL cam_v3 — what the
                                            calibrator could do with perfect
                                            inputs)
  upstream_err = err_pipeline − err_calibrator
              = phase_b − oracle_roster   (the "projection error budget":
                                           trajectory + freshman + composition
                                           combined)

If the calibrator-only MAE is tight (≈ same-season LOSO ≈ 3.7) and the
upstream gap is wide, the next investment is in the upstream models
(trajectory / freshman cam_v3 honesty), not the calibrator.

Inputs:
- `training/eval_history/projections_backtest_per_team_*.json`
  (latest; produced by `cstat-ingest projections-backtest --output`).
- `training/models/roster_impact_loso/roster_impact_model_{season}.onnx`
  (the LOSO model that did NOT see this target season — same model the
  backtest uses, so the comparison is apples-to-apples).
- Live Postgres for the actual N-season roster + cam_v3 + portal sums.

Outputs:
- `training/eval_history/decomposition_{date}_summary.json` —
  machine-readable per-quartile + per-team breakdowns.
- Console: headline pooled / per-quartile attribution + Maryland +
  Michigan case studies (the canonical lost-a-lot vs. gained-a-lot pair).
"""

from __future__ import annotations

import datetime as dt
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
import pandas as pd
from sqlalchemy import text

from db import get_engine
from train_roster_impact_model import aggregate_team_season

REPO_ROOT = Path(__file__).resolve().parents[1]
EVAL_DIR = REPO_ROOT / "training" / "eval_history"
MODELS_DIR = REPO_ROOT / "training" / "models"
LOSO_MODELS_DIR = MODELS_DIR / "roster_impact_loso"
META_PATH = MODELS_DIR / "roster_impact_model_meta.json"
PER_TEAM_DUMP_GLOB = "projections_backtest_per_team_*.json"


def load_feature_contract() -> list[str]:
    """Read the ONNX-input feature names from the model meta JSON, in
    the order the model expects.

    The training script writes this list verbatim into
    `roster_impact_model_meta.json::features`; the Rust boot validator
    pins `ROSTER_IMPACT_FEATURE_NAMES` against it. Reading from disk
    here keeps Python parity with both — a feature added in training
    propagates here automatically rather than silently drifting and
    producing wrong oracle predictions."""
    return json.loads(META_PATH.read_text())["features"]


def load_per_team() -> pd.DataFrame:
    dumps = sorted(EVAL_DIR.glob(PER_TEAM_DUMP_GLOB))
    if not dumps:
        raise SystemExit(
            f"no per-team dump in {EVAL_DIR}; run `cargo run --release --bin "
            f"cstat-ingest -- projections-backtest --output PATH` first"
        )
    dump_path = dumps[-1]
    print(f"loading per-team dump: {dump_path.name}")
    return pd.read_json(dump_path)


def load_loso_models() -> dict[int, ort.InferenceSession]:
    """One ONNX session per LOSO-target season."""
    sessions = {}
    for path in LOSO_MODELS_DIR.glob("roster_impact_model_*.onnx"):
        year = int(path.stem.split("_")[-1])
        sessions[year] = ort.InferenceSession(
            str(path), providers=["CPUExecutionProvider"]
        )
    if not sessions:
        raise SystemExit(
            f"no LOSO models in {LOSO_MODELS_DIR}; rerun `python "
            f"train_roster_impact_model.py` to regenerate"
        )
    print(f"loaded LOSO models for seasons: {sorted(sessions)}")
    return sessions


def resolve_target_team_id(conn, base_team_id: str, season: int) -> str | None:
    """The backtest dump records the *base-season* team UUID (the only
    one that existed when `compose_all_projections` ran). To fetch the
    actual target-season roster we hop via `teams.natstat_id` — same
    cross-season UUID resolution the projections route uses.

    Returns None when the team didn't exist in the target season
    (D-I transition, defunct program, etc.) — those rows get a NaN
    oracle prediction and drop out of the diagnostic."""
    sql = text(
        """
        SELECT tgt.id
        FROM teams base
        JOIN teams tgt
          ON tgt.natstat_id = base.natstat_id
         AND tgt.season = :target
        WHERE base.id = :base_id
        """
    )
    row = conn.execute(sql, {"base_id": base_team_id, "target": season}).fetchone()
    return str(row.id) if row else None


def fetch_team_roster(conn, target_team_id: str, season: int) -> pd.DataFrame:
    """Same PLAYER_QUERY shape (returners-OOF / freshmen-OOF / actual
    fallback) but filtered to one (team, season). `target_team_id` MUST
    be the season-N team UUID (use `resolve_target_team_id` to hop from
    the backtest's base-season UUID). Uses the SAME cam_v3 source as
    training so the "oracle" comparison is apples-to-apples: the only
    thing that differs between oracle and pipeline is *which players
    are in the roster*."""
    # Build the parameterized query directly. Avoids the PLAYER_QUERY
    # string surgery (the original is psycopg-style; sqlalchemy text
    # binding doesn't expand a Python list into ANY without extra
    # plumbing). One team-season, one bound int, one bound UUID string.
    sql = text(
        """
        SELECT
            pss.team_id, pss.season, pss.player_id,
            COALESCE(traj.mean, fresh.mean, tps.cam_gbpm_v3_psos) AS campom,
            CASE
                WHEN traj.mean IS NOT NULL THEN 'trajectory_oof'
                WHEN fresh.mean IS NOT NULL THEN 'freshman_oof'
                WHEN tps.cam_gbpm_v3_psos IS NOT NULL THEN 'actual_fallback'
                ELSE 'none'
            END AS campom_source,
            pa.primary_class,
            p.class_year
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        LEFT JOIN trajectory_oof_predictions traj
            ON traj.torvik_pid = tps.torvik_pid AND traj.target_season = pss.season
        LEFT JOIN freshman_oof_predictions fresh
            ON fresh.cstat_player_id = pss.player_id AND fresh.target_season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        WHERE pss.season = :season
          AND pss.team_id = CAST(:team_id AS uuid)
          AND COALESCE(pss.games_played, 0) >= 5
          AND COALESCE(pss.minutes_per_game, 0) >= 5
        """
    )
    return pd.read_sql(sql, conn, params={"season": season, "team_id": target_team_id})


def fetch_portal_sums(conn, target_team_id: str, season: int) -> tuple[float, float]:
    """Compute the (outbound, inbound) cam_v3 sums for one team-season.
    Mirrors `train_roster_impact_model.py::OUTBOUND_QUERY` /
    `INBOUND_QUERY` — `target_team_id` is the season-N team UUID
    (matching what training uses as the merge key after the natstat_id
    hop)."""
    portal_year = season - 1
    out_row = conn.execute(
        text(
            """
            SELECT COALESCE(SUM(COALESCE(tps.cam_gbpm_v3_psos, 0)), 0)::float8 AS s
            FROM transfers t
            JOIN players p_base
                ON p_base.id = t.cstat_player_id AND p_base.season = t.year
            JOIN teams base_team ON base_team.id = p_base.team_id
            JOIN teams tgt_team
                ON tgt_team.natstat_id = base_team.natstat_id
               AND tgt_team.season = p_base.season + 1
            LEFT JOIN torvik_player_stats tps
                ON tps.player_id = p_base.id AND tps.season = t.year
            WHERE t.year = :portal_year
              AND tgt_team.id = CAST(:team_id AS uuid)
            """
        ),
        {"portal_year": portal_year, "team_id": target_team_id},
    ).fetchone()
    in_row = conn.execute(
        text(
            # Cross-season player identity uses natstat_id OR
            # torvik_pid. See `train_roster_impact_model.py::INBOUND_QUERY`
            # for the full rationale: natstat_id is reissued per team
            # (broken for transfers); torvik_pid is stable across
            # transfers (96% coverage, zero collisions). The OR catches
            # both stable cohorts and transfers without double-counting.
            """
            SELECT COALESCE(SUM(COALESCE(tps_base.cam_gbpm_v3_psos, 0)), 0)::float8 AS s
            FROM transfers t
            JOIN players p_base
                ON p_base.id = t.cstat_player_id AND p_base.season = t.year
            LEFT JOIN torvik_player_stats tps_base
                ON tps_base.player_id = p_base.id AND tps_base.season = t.year
            JOIN players p_tgt
                ON p_tgt.season = t.year + 1
               AND (
                    p_tgt.natstat_id = p_base.natstat_id
                    OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
                        SELECT player_id FROM torvik_player_stats
                        WHERE torvik_pid = tps_base.torvik_pid
                          AND season = t.year + 1
                    ))
               )
            JOIN teams tgt_team ON tgt_team.id = p_tgt.team_id
            WHERE t.year = :portal_year
              AND tgt_team.id = CAST(:team_id AS uuid)
            """
        ),
        {"portal_year": portal_year, "team_id": target_team_id},
    ).fetchone()
    return float(out_row.s), float(in_row.s)


def build_feature_vector(
    roster: pd.DataFrame,
    outbound_sum: float,
    inbound_sum: float,
    feature_names: list[str],
) -> np.ndarray:
    """Reuse `aggregate_team_season` (the training-side aggregator) so
    parity is byte-for-byte with the model's training inputs.
    `feature_names` is the ONNX feature contract (`load_feature_contract`),
    used to enforce wire order without duplicating it in this file —
    same drift defence the Rust `validate_roster_impact_meta` provides."""
    n_features = len(feature_names)
    if len(roster) == 0:
        # All-zero except the two portal slots (the trailing pair of
        # the contract). Matches `build_roster_impact_features`'s
        # empty-roster path on the Rust side.
        vec = np.zeros(n_features, dtype=np.float32)
        vec[feature_names.index("outbound_cam_v3_sum")] = outbound_sum
        vec[feature_names.index("inbound_cam_v3_sum")] = inbound_sum
        return vec.reshape(1, -1)
    agg = aggregate_team_season(roster)
    # Sentinel-0 NaN replacement matches `build_dataset`, which drops
    # rows with `cam_wmean = NaN` — we end up here only when re-scoring
    # such a row (oracle path); fall back to 0 so the model can still
    # produce SOMETHING, but the caller should treat low-coverage
    # oracle predictions with skepticism.
    portal_values = {
        "outbound_cam_v3_sum": outbound_sum,
        "inbound_cam_v3_sum": inbound_sum,
    }
    vec: list[float] = []
    for name in feature_names:
        if name in portal_values:
            vec.append(portal_values[name])
            continue
        v = agg.get(name, 0.0)
        if v is None or (isinstance(v, float) and np.isnan(v)):
            v = 0.0
        vec.append(float(v))
    return np.array(vec, dtype=np.float32).reshape(1, -1)


def oracle_pred(
    conn,
    session: ort.InferenceSession,
    base_team_id: str,
    season: int,
    feature_names: list[str],
) -> tuple[float | None, dict]:
    """Build the actual-roster + actual-portal-sums feature vector and
    score it. Returns (prediction, diagnostic_dict). Prediction is None
    when the team didn't exist in the target season (D-I transition,
    defunct program) — the caller surfaces a NaN."""
    target_id = resolve_target_team_id(conn, base_team_id, season)
    if target_id is None:
        return None, {"resolved_target_id": None}
    roster = fetch_team_roster(conn, target_id, season)
    out_sum, in_sum = fetch_portal_sums(conn, target_id, season)
    vec = build_feature_vector(roster, out_sum, in_sum, feature_names)
    raw = session.run(None, {session.get_inputs()[0].name: vec})[0]
    pred = float(np.asarray(raw).flatten()[0])
    diag = {
        "resolved_target_id": target_id,
        "actual_roster_size": int(len(roster)),
        "actual_cam_sum": float(vec[0, feature_names.index("cam_sum")]),
        "actual_cam_top1": float(vec[0, feature_names.index("cam_top1")]),
        "outbound_cam_v3_sum": out_sum,
        "inbound_cam_v3_sum": in_sum,
    }
    return pred, diag


def summarize(df: pd.DataFrame, col_err: str) -> dict:
    return {
        "n": int(len(df)),
        "mae": float(df[col_err].abs().mean()),
        "bias": float(df[col_err].mean()),
        "rmse": float(np.sqrt((df[col_err] ** 2).mean())),
    }


def per_actual_quartile(df: pd.DataFrame) -> list[dict]:
    q = df["actual"].quantile([0.25, 0.5, 0.75]).values
    bounds = [-np.inf, q[0], q[1], q[2], np.inf]
    labels = ["Q1 bottom", "Q2 below-median", "Q3 above-median", "Q4 top"]
    out = []
    for i in range(4):
        m = (df["actual"] > bounds[i]) & (df["actual"] <= bounds[i + 1])
        sub = df[m]
        if len(sub) == 0:
            continue
        out.append({
            "bucket": labels[i],
            "n": int(len(sub)),
            "mae_pipeline": float(sub["err_pipeline"].abs().mean()),
            "bias_pipeline": float(sub["err_pipeline"].mean()),
            "mae_calibrator": float(sub["err_calibrator"].abs().mean()),
            "bias_calibrator": float(sub["err_calibrator"].mean()),
            "mae_upstream": float(sub["upstream_err"].abs().mean()),
            "bias_upstream": float(sub["upstream_err"].mean()),
        })
    return out


def case_studies(df: pd.DataFrame, names: list[str]) -> list[dict]:
    out = []
    for n in names:
        sub = df[df["team_name"].str.contains(n, case=False, na=False)]
        for _, row in sub.iterrows():
            out.append({
                "team": row["team_name"],
                "season": int(row["season"]),
                "baseline": float(row["baseline"]),
                "pipeline_pred": float(row["phase_b"]),
                "oracle_roster_pred": float(row["oracle_roster"]),
                "actual": float(row["actual"]),
                "err_pipeline": float(row["err_pipeline"]),
                "err_calibrator": float(row["err_calibrator"]),
                "upstream_err": float(row["upstream_err"]),
                "actual_roster_size": int(row["actual_roster_size"]),
                "actual_cam_sum": float(row["actual_cam_sum"]),
                "outbound_cam_v3_sum": float(row["outbound_cam_v3_sum"]),
                "inbound_cam_v3_sum": float(row["inbound_cam_v3_sum"]),
            })
    return out


def main() -> None:
    df = load_per_team()
    sessions = load_loso_models()
    feature_names = load_feature_contract()
    print(f"per-team dump: {len(df)} rows ({sorted(df.season.unique())})")
    print(f"feature contract: {len(feature_names)} features from {META_PATH.name}")

    engine = get_engine()
    oracle_preds = []
    diags = []
    with engine.connect() as conn:
        for i, row in df.iterrows():
            season = int(row["season"])
            if season not in sessions:
                oracle_preds.append(np.nan)
                diags.append({})
                continue
            pred, diag = oracle_pred(
                conn, sessions[season], row["team_id"], season, feature_names
            )
            oracle_preds.append(pred if pred is not None else np.nan)
            diags.append(diag)
            if (i + 1) % 50 == 0:
                print(f"  scored {i + 1}/{len(df)} teams")

    df["oracle_roster"] = oracle_preds
    diag_df = pd.DataFrame(diags)
    df = pd.concat([df.reset_index(drop=True), diag_df.reset_index(drop=True)], axis=1)

    # Decomposition (signs: positive = pred over-shot actual).
    df["err_pipeline"] = df["phase_b"] - df["actual"]
    df["err_calibrator"] = df["oracle_roster"] - df["actual"]
    df["upstream_err"] = df["phase_b"] - df["oracle_roster"]
    df = df.dropna(subset=["oracle_roster"]).reset_index(drop=True)
    print(f"\nscored {len(df)} teams against LOSO oracle")

    findings = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "n": int(len(df)),
        "seasons": sorted([int(s) for s in df.season.unique()]),
        "headline": {
            "pipeline": summarize(df, "err_pipeline"),
            "calibrator_only": summarize(df, "err_calibrator"),
            "upstream_only": summarize(df, "upstream_err"),
        },
        "per_actual_quartile": per_actual_quartile(df),
        "case_studies": case_studies(df, ["Maryland", "Michigan", "Purdue", "Duke", "Auburn"]),
    }

    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out_json = EVAL_DIR / f"decomposition_{date_str}_summary.json"
    out_json.write_text(json.dumps(findings, indent=2, default=float))
    print(f"\nwrote {out_json}")

    # Console preview.
    print("\n=== HEADLINE (signs: pred − actual; + = over) ===")
    for name, stats in findings["headline"].items():
        print(
            f"  {name:<18} MAE {stats['mae']:.2f}  bias {stats['bias']:+.2f}  "
            f"RMSE {stats['rmse']:.2f}  n={stats['n']}"
        )

    print("\n=== ERROR ATTRIBUTION BY ACTUAL QUARTILE ===")
    print(f"  {'bucket':<22} {'n':>3}  "
          f"{'pipeline (MAE / bias)':<22} "
          f"{'calibrator (MAE / bias)':<24} "
          f"{'upstream (MAE / bias)':<22}")
    for b in findings["per_actual_quartile"]:
        print(
            f"  {b['bucket']:<22} {b['n']:>3}  "
            f"{b['mae_pipeline']:5.2f} / {b['bias_pipeline']:+5.2f}        "
            f"{b['mae_calibrator']:5.2f} / {b['bias_calibrator']:+5.2f}          "
            f"{b['mae_upstream']:5.2f} / {b['bias_upstream']:+5.2f}"
        )

    print("\n=== CASE STUDIES ===")
    for cs in findings["case_studies"]:
        print(
            f"  {cs['team']:<25} {cs['season']}  "
            f"baseline {cs['baseline']:+6.1f}  "
            f"pipeline {cs['pipeline_pred']:+6.1f}  "
            f"oracle {cs['oracle_roster_pred']:+6.1f}  "
            f"actual {cs['actual']:+6.1f}  "
            f"| err_pipe {cs['err_pipeline']:+6.1f}  "
            f"upstream {cs['upstream_err']:+6.1f}  "
            f"calibrator {cs['err_calibrator']:+6.1f}  "
            f"| outbound {cs['outbound_cam_v3_sum']:+5.1f}  "
            f"inbound {cs['inbound_cam_v3_sum']:+5.1f}"
        )


if __name__ == "__main__":
    main()
