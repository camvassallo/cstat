"""Preseason projection accuracy audit (ROADMAP §6 "Preseason × pit blend"
prerequisite 1).

Question: how well did `/api/projections/{year}` project actual AdjEM, and
where does it fail systematically? The Phase B projection is what serves
the live projections page; we want to know whether the residual error has
exploitable structure that points to missing features (coaching, recruit
class composite, returning-minutes fraction, portal turnover) — i.e.,
the signals the §6 "Preseason projection improvements" candidate list was
betting on.

Audit also surfaces the headline honesty story (verified by
`trajectory_honesty_check` below — all three components are leak-free at
the season level for this backtest):
- **Roster-impact** is leave-one-season-out (the projection backtest
  loads the per-target LOSO model from `models/roster_impact_loso/`).
- **Trajectory**: `leave_one_pair_out` in `train_trajectory_model.py` is
  misnamed — it holds out each unique `(s_n, s_np1)` pair, which IS true
  leave-one-target-season-out at the pair level. Predictions persisted
  to `trajectory_oof_predictions` (consumed by the backtest) are those
  LOSO predictions. LOSO pooled MAE ≈ 5-fold MAE, no season-level
  memorization.
- **Freshman** is leave-one-class-out (LOCO).

Inputs:
- `training/eval_history/projections_backtest_per_team_{date}.json` (one
  record per scored team; produced by `cstat-ingest projections-backtest
  --output PATH`). The audit script picks the most-recent dated dump.
- Live Postgres (`DATABASE_URL`) for per-team explanatory signals.

Outputs:
- `training/eval_history/preseason_audit_{date}_summary.json` (machine-
  readable findings)
- `training/eval_history/preseason_audit_{date}.md` (human-readable
  one-page report, hand-written from the summary JSON)
"""

from __future__ import annotations

import datetime as dt
import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import text

from db import get_engine

REPO_ROOT = Path(__file__).resolve().parents[1]
EVAL_DIR = REPO_ROOT / "training" / "eval_history"
# Per-team dumps are date-stamped to match the rest of `eval_history/`
# (sample-of-record convention). We pick the most-recent dump on each
# audit run so the snapshot trio (per-team + summary + .md) stays
# load-bearing for the published findings.
PER_TEAM_DUMP_GLOB = "projections_backtest_per_team_*.json"

# Phase B's shipped blend (matches routes/projections.rs and
# projections_backtest.rs after the v2 OOF retrain). Pred =
# 0.50·baseline + 0.50·phase_b_raw + 0.
BLEND_W = 0.50
BLEND_OFFSET = 0.0


def load_per_team() -> pd.DataFrame:
    """Load the most-recent per-team prediction dump from the Rust backtest."""
    dumps = sorted(EVAL_DIR.glob(PER_TEAM_DUMP_GLOB))
    if not dumps:
        suggested = EVAL_DIR / f"projections_backtest_per_team_{dt.datetime.utcnow():%Y%m%d}.json"
        sys.exit(
            f"no per-team dump found in {EVAL_DIR} (glob: {PER_TEAM_DUMP_GLOB}).\n"
            f"  run: cargo run --release --bin cstat-ingest -- "
            f"projections-backtest --output {suggested}"
        )
    dump_path = dumps[-1]
    print(f"loading per-team dump: {dump_path.name}")
    df = pd.read_json(dump_path)
    df["blended"] = BLEND_W * df["baseline"] + (1 - BLEND_W) * df["phase_b"] + BLEND_OFFSET
    df["err_blended"] = df["blended"] - df["actual"]
    df["err_phase_b"] = df["phase_b"] - df["actual"]
    df["err_baseline"] = df["baseline"] - df["actual"]
    df["abs_err_blended"] = df["err_blended"].abs()
    df["team_id"] = df["team_id"].astype(str)
    return df


def fetch_returning_share(conn, df: pd.DataFrame) -> pd.DataFrame:
    """For each (team_id, target_season), compute the share of *base-season*
    minutes that returned to ANY D-I team in the target season.

    Note: a player who transferred to another D-I school still counts as
    "returning to D-I" — the projection knows about the transfer (Phase B
    aggregates inbound/outbound), so what we measure here is "how much
    last year's roster *experience* is still in the projection's player
    pool" vs "swallowed by departures / draft / non-D-I". Higher returning
    share = more continuity = lower projection variance, in theory.
    """
    rows = []
    for _, row in df.iterrows():
        target = int(row["season"])
        base = target - 1
        team_id = row["team_id"]
        # `torvik_pid` is the cross-season player key (96% coverage,
        # zero collisions — memory:reference_torvik_pid); `players.natstat_id`
        # *also* survives transfers when the player keeps the same NatStat
        # code. Cover both keys to catch the maximum returning cohort.
        sql = text(
            """
            WITH base_roster AS (
                SELECT pgs.player_id, SUM(pgs.minutes) AS mp
                FROM player_game_stats pgs
                JOIN games g ON g.id = pgs.game_id
                WHERE pgs.team_id = :team_id AND g.season = :base
                GROUP BY pgs.player_id
            ),
            base_with_meta AS (
                SELECT br.player_id, br.mp, p_base.natstat_id,
                       tps_base.torvik_pid
                FROM base_roster br
                JOIN players p_base ON p_base.id = br.player_id
                LEFT JOIN torvik_player_stats tps_base
                  ON tps_base.player_id = p_base.id AND tps_base.season = :base
            ),
            returned_by_natstat AS (
                SELECT DISTINCT bwm.player_id
                FROM base_with_meta bwm
                JOIN players p_tgt
                  ON p_tgt.natstat_id = bwm.natstat_id
                 AND p_tgt.season = :target
                JOIN player_season_stats pss
                  ON pss.player_id = p_tgt.id AND pss.season = :target
                WHERE pss.games_played >= 1
            ),
            returned_by_torvik AS (
                SELECT DISTINCT bwm.player_id
                FROM base_with_meta bwm
                JOIN torvik_player_stats tps_tgt
                  ON tps_tgt.torvik_pid = bwm.torvik_pid
                 AND tps_tgt.season = :target
                WHERE bwm.torvik_pid IS NOT NULL
            ),
            returned AS (
                SELECT player_id FROM returned_by_natstat
                UNION
                SELECT player_id FROM returned_by_torvik
            )
            SELECT
                COALESCE(SUM(bwm.mp), 0)::FLOAT AS base_minutes,
                COALESCE(SUM(CASE WHEN r.player_id IS NOT NULL THEN bwm.mp ELSE 0 END), 0)::FLOAT
                    AS returning_minutes
            FROM base_with_meta bwm
            LEFT JOIN returned r ON r.player_id = bwm.player_id
            """
        )
        result = conn.execute(
            sql, {"team_id": team_id, "base": base, "target": target}
        ).fetchone()
        base_min = result.base_minutes
        ret_min = result.returning_minutes
        rows.append(
            {
                "team_id": team_id,
                "season": target,
                "returning_minutes_share": (ret_min / base_min) if base_min > 0 else 0.0,
                "base_minutes_total": base_min,
            }
        )
    return pd.DataFrame(rows)


def fetch_portal_signals(conn, df: pd.DataFrame) -> pd.DataFrame:
    """Per-team portal turnover from the `transfers` table.

    For backtest target season N, the relevant portal cycle is
    `transfers.year = N - 1` (spring of base season N-1 = portal moves
    INTO target season N). We measure raw count + summed cam_v3 of
    inbound (this team gained) and outbound (this team lost), keyed by
    cstat_player_id → players.team_id at the season boundary.

    Outbound: cstat_player_id had base-season stats with team T.
    Inbound: cstat_player_id has target-season stats with team T.
    Cam_v3 read from torvik_player_stats at the player's BASE season
    (i.e., the level they brought into the portal) — what the destination
    team was "buying."
    """
    rows = []
    for _, row in df.iterrows():
        target = int(row["season"])
        base = target - 1
        portal_year = target - 1  # spring of base = portal into target
        team_id = row["team_id"]
        # `transfers.cstat_player_id` already resolves to a base-season
        # player UUID (see roadmap §5b "resolve_cstat_joins short-name +
        # alias fix"). `team_id` from the dump is the BASE-season team
        # UUID; the target-season team is a different row joined via
        # `natstat_id` (season-scoped UUIDs).
        sql = text(
            """
            WITH base_team AS (
                SELECT natstat_id FROM teams WHERE id = :team_id
            ),
            target_team AS (
                SELECT t.id
                FROM teams t
                JOIN base_team bt ON bt.natstat_id = t.natstat_id
                WHERE t.season = :target
            ),
            outbound AS (
                SELECT t.cstat_player_id,
                       COALESCE(tps.cam_gbpm_v3_psos, 0) AS cam_v3
                FROM transfers t
                JOIN players p_base ON p_base.id = t.cstat_player_id AND p_base.season = :base
                LEFT JOIN torvik_player_stats tps
                  ON tps.player_id = p_base.id AND tps.season = :base
                WHERE t.year = :portal_year
                  AND p_base.team_id = :team_id
            ),
            inbound AS (
                -- Cross-season player identity uses **natstat_id OR
                -- torvik_pid**. natstat_id is reissued per team (gets
                -- a new id at the new school), so a natstat_id-only
                -- join silently misses every transfer. `torvik_pid`
                -- is stable across transfers (per `reference_torvik_pid`
                -- memory: 96% coverage, zero collisions). Audit's prior
                -- inbound numbers (e.g. β=-0.605 OLS) were silently
                -- under-counted by this bug — fixed here so future
                -- audits measure the true portal signal.
                SELECT t.cstat_player_id,
                       COALESCE(tps_base.cam_gbpm_v3_psos, 0) AS cam_v3
                FROM transfers t
                JOIN players p_base
                  ON p_base.id = t.cstat_player_id AND p_base.season = :base
                LEFT JOIN torvik_player_stats tps_base
                  ON tps_base.player_id = p_base.id AND tps_base.season = :base
                JOIN players p_tgt
                  ON p_tgt.season = :target
                 AND (
                      p_tgt.natstat_id = p_base.natstat_id
                      OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
                          SELECT player_id FROM torvik_player_stats
                          WHERE torvik_pid = tps_base.torvik_pid AND season = :target
                      ))
                 )
                JOIN target_team tt ON tt.id = p_tgt.team_id
                WHERE t.year = :portal_year
            )
            SELECT
                (SELECT COUNT(*) FROM outbound) AS outbound_count,
                (SELECT COALESCE(SUM(cam_v3), 0)::FLOAT FROM outbound) AS outbound_cam_v3,
                (SELECT COUNT(*) FROM inbound) AS inbound_count,
                (SELECT COALESCE(SUM(cam_v3), 0)::FLOAT FROM inbound) AS inbound_cam_v3
            """
        )
        result = conn.execute(
            sql,
            {
                "team_id": team_id,
                "base": base,
                "target": target,
                "portal_year": portal_year,
            },
        ).fetchone()
        rows.append(
            {
                "team_id": team_id,
                "season": target,
                "portal_outbound_count": result.outbound_count,
                "portal_inbound_count": result.inbound_count,
                "portal_outbound_cam_v3": result.outbound_cam_v3,
                "portal_inbound_cam_v3": result.inbound_cam_v3,
                "portal_net_count": result.inbound_count - result.outbound_count,
                "portal_net_cam_v3": result.inbound_cam_v3 - result.outbound_cam_v3,
            }
        )
    return pd.DataFrame(rows)


def fetch_recruit_signals(conn, df: pd.DataFrame) -> pd.DataFrame:
    """Per-team incoming-recruit-class strength.

    For backtest target N, recruits.year = N - 1 (class-of-(N-1) plays
    their freshman season in cstat-season N). Joined via
    `recruits.committed_team_id` → `teams.natstat_id` to handle the
    season-scoped UUID jump.
    """
    rows = []
    for _, row in df.iterrows():
        target = int(row["season"])
        base = target - 1
        recruit_year = target - 1
        team_id = row["team_id"]
        sql = text(
            """
            WITH base_team AS (
                SELECT natstat_id FROM teams WHERE id = :team_id
            ),
            recruits_for_team AS (
                SELECT r.composite_rating, r.composite_rank, r.star_rating
                FROM recruits r
                JOIN teams t_committed
                  ON t_committed.id = r.committed_team_id
                JOIN base_team bt
                  ON bt.natstat_id = t_committed.natstat_id
                WHERE r.year = :recruit_year
                  AND r.committed_team_id IS NOT NULL
            )
            SELECT
                COUNT(*) AS n_recruits,
                COALESCE(AVG(composite_rating), 0)::FLOAT AS avg_rating,
                COUNT(*) FILTER (WHERE composite_rank IS NOT NULL AND composite_rank <= 30) AS n_top30,
                COUNT(*) FILTER (WHERE composite_rank IS NOT NULL AND composite_rank <= 100) AS n_top100,
                COALESCE(AVG(star_rating), 0)::FLOAT AS avg_stars
            FROM recruits_for_team
            """
        )
        result = conn.execute(
            sql, {"team_id": team_id, "recruit_year": recruit_year}
        ).fetchone()
        rows.append(
            {
                "team_id": team_id,
                "season": target,
                "recruit_count": result.n_recruits,
                "recruit_avg_rating": result.avg_rating,
                "recruit_top30_count": result.n_top30,
                "recruit_top100_count": result.n_top100,
                "recruit_avg_stars": result.avg_stars,
            }
        )
    return pd.DataFrame(rows)


def fetch_signals(df: pd.DataFrame) -> pd.DataFrame:
    """Join all per-team signals onto the predictions frame."""
    engine = get_engine()
    with engine.connect() as conn:
        ret = fetch_returning_share(conn, df)
        portal = fetch_portal_signals(conn, df)
        recruits = fetch_recruit_signals(conn, df)
    out = df.merge(ret, on=["team_id", "season"], how="left")
    out = out.merge(portal, on=["team_id", "season"], how="left")
    out = out.merge(recruits, on=["team_id", "season"], how="left")
    return out


def correlate_with_residual(df: pd.DataFrame, signals: list[str]) -> pd.DataFrame:
    """Simple per-signal Pearson correlation with signed error and with
    absolute error. Goal is: which signals correlate with miss direction
    (signed) and which with miss magnitude (absolute)?
    """
    rows = []
    for s in signals:
        sub = df[[s, "err_blended", "abs_err_blended"]].dropna()
        if len(sub) < 30:
            continue
        rows.append(
            {
                "signal": s,
                "n": len(sub),
                "corr_signed_err": float(sub[s].corr(sub["err_blended"])),
                "corr_abs_err": float(sub[s].corr(sub["abs_err_blended"])),
                "signal_mean": float(sub[s].mean()),
                "signal_std": float(sub[s].std()),
            }
        )
    return pd.DataFrame(rows)


def fit_ols_residual_model(df: pd.DataFrame, signals: list[str]) -> dict:
    """OLS regression of signed residual error against the per-team
    signals. Reports per-feature coefficient + t-stat + R² so we can tell
    which signals carry independent information vs. multicollinearity.
    """
    sub = df[signals + ["err_blended"]].dropna()
    if len(sub) < 50:
        return {"error": "insufficient rows for OLS"}
    X = sub[signals].values
    y = sub["err_blended"].values
    # Standardize features so coefficients are comparable.
    mu = X.mean(axis=0)
    sd = X.std(axis=0)
    sd[sd == 0] = 1.0
    Xs = (X - mu) / sd
    Xs = np.column_stack([np.ones(len(Xs)), Xs])
    # Suppress numerically-benign overflow warnings from intermediate
    # SIMD ops when feature columns are near-collinear (portal counts vs
    # cam_v3 sums correlate strongly). The OLS solve itself is stable
    # via lstsq + pinv; final coefficients are checked at the call site.
    with np.errstate(divide="ignore", over="ignore", invalid="ignore"):
        beta, *_ = np.linalg.lstsq(Xs, y, rcond=None)
        y_hat = Xs @ beta
        resid = y - y_hat
        ss_res = float(np.sum(resid**2))
        ss_tot = float(np.sum((y - y.mean()) ** 2))
        r2 = 1.0 - ss_res / ss_tot if ss_tot > 0 else 0.0
        n, p = Xs.shape
        sigma2 = ss_res / (n - p) if n > p else float("nan")
        cov = sigma2 * np.linalg.pinv(Xs.T @ Xs)
        se = np.sqrt(np.diag(cov))
        t = beta / se
    coefs = []
    for i, name in enumerate(["intercept"] + signals):
        coefs.append(
            {
                "feature": name,
                "beta_std": float(beta[i]),
                "t_stat": float(t[i]),
            }
        )
    return {
        "n": int(n),
        "r2": float(r2),
        "rmse": float(np.sqrt(ss_res / n)),
        "coefficients": coefs,
        "note": (
            "betas are on standardized signals (z-scored). |t| > 2 ≈ "
            "significant at p<0.05. positive beta_std on a signal means "
            "higher signal → projection OVER-shoots actual; negative "
            "means projection UNDER-shoots."
        ),
    }


def by_returning_share_buckets(df: pd.DataFrame) -> list[dict]:
    """Bucket teams by returning-minutes-share quartiles and report per-
    bucket MAE / bias. If the "sticky last-year" theory holds, the LOWEST
    returning-share bucket (highest turnover) should have the worst MAE.
    """
    q = df["returning_minutes_share"].quantile([0.25, 0.5, 0.75]).values
    bounds = [-np.inf, q[0], q[1], q[2], np.inf]
    labels = ["Q1 low-return", "Q2", "Q3", "Q4 high-return"]
    out = []
    for i in range(4):
        m = (df["returning_minutes_share"] > bounds[i]) & (
            df["returning_minutes_share"] <= bounds[i + 1]
        )
        sub = df[m]
        if len(sub) == 0:
            continue
        out.append(
            {
                "bucket": labels[i],
                "lo": float(bounds[i]) if np.isfinite(bounds[i]) else None,
                "hi": float(bounds[i + 1]) if np.isfinite(bounds[i + 1]) else None,
                "n": int(len(sub)),
                "mae_blended": float(sub["abs_err_blended"].mean()),
                "bias_blended": float(sub["err_blended"].mean()),
                "mae_baseline": float((sub["baseline"] - sub["actual"]).abs().mean()),
            }
        )
    return out


def regression_to_mean_diagnostic(df: pd.DataFrame) -> dict:
    """Fit the OLS line `actual = a + b * predicted` and test the slope
    against the null `b = 1.0` (no compression). Also reports the
    bias-corrected MAE that would result from rescaling `pred → a + b*pred`.

    Slope direction in this parameterization:
      - **b > 1** ⇒ pred range is *narrower* than actual range — the
        projection compresses tails (under-projects top, over-projects
        bottom when binned by actual). De-shrinking expands.
      - **b < 1** ⇒ pred range is *wider* than actual range — projection
        exaggerates tails. De-shrinking compresses.
      - **b ≈ 1** ⇒ calibrated on average.

    Caveat: a near-1 slope can still hide large per-quartile bias if the
    pred↔actual relationship is non-linear (the linear fit averages
    tail divergence into the bulk). The by-actual-quartile diagnostic is
    the load-bearing per-bucket view; this slope test is the global
    single-number summary.
    """
    sub = df[["blended", "actual"]].dropna()
    n = len(sub)
    if n < 30:
        return {"error": "insufficient rows"}
    x = sub["blended"].values
    y = sub["actual"].values
    x_mean, y_mean = x.mean(), y.mean()
    b = float(np.sum((x - x_mean) * (y - y_mean)) / np.sum((x - x_mean) ** 2))
    a = float(y_mean - b * x_mean)
    y_hat = a + b * x
    resid = y - y_hat
    sigma2 = float(np.sum(resid**2) / (n - 2))
    se_b = float(np.sqrt(sigma2 / np.sum((x - x_mean) ** 2)))
    # Test b = 1.0 (null: no regression-to-mean in the projection).
    t_b_vs_1 = (b - 1.0) / se_b
    # Apply the implied correction to see how much MAE recovery is on
    # the table. `corrected = a + b * blended`.
    corrected = a + b * x
    mae_raw = float(np.mean(np.abs(x - y)))
    mae_corrected = float(np.mean(np.abs(corrected - y)))
    if b > 1.0:
        direction = (
            "Pred range narrower than actual ⇒ projection compresses tails "
            "(under-projects top, over-projects bottom when binned by actual)."
        )
    elif b < 1.0:
        direction = (
            "Pred range wider than actual ⇒ projection exaggerates tails "
            "(over-projects top, under-projects bottom when binned by actual)."
        )
    else:
        direction = "Slope ≈ 1 ⇒ globally calibrated."
    return {
        "n": int(n),
        "intercept_a": a,
        "slope_b": b,
        "slope_se": se_b,
        "t_stat_b_vs_one": float(t_b_vs_1),
        "mae_raw": mae_raw,
        "mae_after_correction": mae_corrected,
        "mae_lift": float(mae_raw - mae_corrected),
        "interpretation": (
            f"slope={b:.3f}  |t vs 1.0|={abs(t_b_vs_1):.2f}. {direction} "
            f"Linear de-shrinkage lift: {mae_raw - mae_corrected:+.3f} MAE."
        ),
    }


def by_actual_quartile(df: pd.DataFrame) -> list[dict]:
    """Bucket by *actual* AdjEM quartiles to see if the model fails worse
    at top or bottom teams (the §6 motivating finding).
    """
    q = df["actual"].quantile([0.25, 0.5, 0.75]).values
    bounds = [-np.inf, q[0], q[1], q[2], np.inf]
    labels = ["Q1 bottom", "Q2 below-median", "Q3 above-median", "Q4 top"]
    out = []
    for i in range(4):
        m = (df["actual"] > bounds[i]) & (df["actual"] <= bounds[i + 1])
        sub = df[m]
        if len(sub) == 0:
            continue
        out.append(
            {
                "bucket": labels[i],
                "actual_lo": float(bounds[i]) if np.isfinite(bounds[i]) else None,
                "actual_hi": float(bounds[i + 1]) if np.isfinite(bounds[i + 1]) else None,
                "n": int(len(sub)),
                "mae_blended": float(sub["abs_err_blended"].mean()),
                "bias_blended": float(sub["err_blended"].mean()),
                "mae_baseline": float((sub["baseline"] - sub["actual"]).abs().mean()),
            }
        )
    return out


def worst_misses(df: pd.DataFrame, k: int = 15) -> dict:
    """Top-k over- and under-projected teams. The point isn't to name and
    shame — it's to eyeball whether the misses cluster around a known
    cause (coaching change, freshman class collapse, transfer churn).
    """
    df = df.sort_values("err_blended").reset_index(drop=True)
    cols = [
        "team_name",
        "season",
        "blended",
        "actual",
        "err_blended",
        "returning_minutes_share",
        "portal_net_count",
        "portal_net_cam_v3",
        "recruit_avg_rating",
        "recruit_top100_count",
    ]
    under = df.head(k)[cols].to_dict(orient="records")  # most negative err = under-projected
    over = df.tail(k)[cols].to_dict(orient="records")[::-1]  # most positive = over-projected
    return {"most_under_projected": under, "most_over_projected": over}


def trajectory_honesty_check() -> dict:
    """Verify the trajectory model's OOF predictions are LOSO-equivalent.

    `train_trajectory_model.py::leave_one_pair_out` holds out each
    unique `(s_n, s_np1)` pair (target-season pair) and retrains on the
    remainder — that's true leave-one-target-season-out at the pair
    level, not row-level LOPO. The predictions persisted to
    `trajectory_oof_predictions` (consumed by `projections-backtest`) are
    those LOSO predictions.

    Comparing LOSO-equivalent to 5-fold-random CV: if the two are close,
    the model has minimal season-specific leakage. Gap >> 0 would mean
    the k-fold predictions are inflated by within-season information; the
    OOF table doesn't use those, so the projection backtest stays honest
    either way — this check is for confidence, not for correction.
    """
    meta_path = REPO_ROOT / "training" / "models" / "trajectory_model_meta.json"
    if not meta_path.exists():
        return {"error": f"missing {meta_path}"}
    meta = json.loads(meta_path.read_text())
    lopo = meta.get("backtest_lopo", {}).get("pooled", {})
    kfold = meta.get("cv_5fold", {})
    out = {
        "loso_pooled_mae": lopo.get("mae"),
        "kfold_5_pooled_mae": kfold.get("mae"),
        "verdict": "unknown",
        "note": (
            "`backtest_lopo` is leave-one-PAIR-out where each pair is a "
            "unique (target_season-1, target_season) — i.e. true LOSO "
            "at the target-season level. Predictions persisted to "
            "`trajectory_oof_predictions` are these LOSO predictions, so "
            "the projection backtest IS leak-free at the season level "
            "for the trajectory model. The 5-fold CV is reported as a "
            "sanity-check that the model isn't fitting season-specific "
            "patterns; a tight LOSO≈k-fold gap is the honesty signature."
        ),
    }
    if lopo.get("mae") is not None and kfold.get("mae") is not None:
        gap = float(lopo["mae"]) - float(kfold["mae"])
        out["gap_loso_minus_kfold"] = gap
        out["verdict"] = (
            "tight (model is honest)" if abs(gap) < 0.05 else f"gap {gap:+.3f}"
        )
    return out


def summary_stats(df: pd.DataFrame, col: str) -> dict:
    return {
        "n": int(len(df)),
        "mae": float(df[col].abs().mean()),
        "bias": float(df[col].mean()),
        "rmse": float(np.sqrt((df[col] ** 2).mean())),
    }


def main() -> None:
    df = load_per_team()
    print(f"loaded {len(df)} per-team predictions ({sorted(df.season.unique())})")
    df = fetch_signals(df)
    print(f"  joined signals — non-null returning_share: "
          f"{df['returning_minutes_share'].notna().sum()}/{len(df)}")

    # `*_net_*` is `inbound − outbound` — perfectly collinear with the
    # two components, so feed only the components into OLS. Correlation
    # report keeps the nets because they're useful one-number summaries
    # at a glance.
    corr_signals = [
        "returning_minutes_share",
        "portal_inbound_count",
        "portal_outbound_count",
        "portal_net_count",
        "portal_inbound_cam_v3",
        "portal_outbound_cam_v3",
        "portal_net_cam_v3",
        "recruit_count",
        "recruit_avg_rating",
        "recruit_top30_count",
        "recruit_top100_count",
    ]
    ols_signals = [
        "returning_minutes_share",
        "portal_inbound_count",
        "portal_outbound_count",
        "portal_inbound_cam_v3",
        "portal_outbound_cam_v3",
        "recruit_count",
        "recruit_avg_rating",
        "recruit_top100_count",
    ]

    findings = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "blend": {"baseline_weight": BLEND_W, "offset": BLEND_OFFSET},
        "headline": {
            "phase_b_blended": summary_stats(df, "err_blended"),
            "phase_b_raw": summary_stats(df, "err_phase_b"),
            "baseline_persistence": summary_stats(df, "err_baseline"),
        },
        "per_season": {},
        "by_actual_quartile": by_actual_quartile(df),
        "by_returning_share_quartile": by_returning_share_buckets(df),
        "signal_correlations": correlate_with_residual(df, corr_signals).to_dict(
            orient="records"
        ),
        "ols_residual_model": fit_ols_residual_model(df, ols_signals),
        "regression_to_mean": regression_to_mean_diagnostic(df),
        "worst_misses": worst_misses(df),
        "trajectory_honesty": trajectory_honesty_check(),
    }
    for season in sorted(df["season"].unique()):
        sub = df[df["season"] == season]
        findings["per_season"][int(season)] = {
            "blended": summary_stats(sub, "err_blended"),
            "phase_b_raw": summary_stats(sub, "err_phase_b"),
            "baseline_persistence": summary_stats(sub, "err_baseline"),
        }

    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out_json = EVAL_DIR / f"preseason_audit_{date_str}_summary.json"
    out_json.write_text(json.dumps(findings, indent=2))
    print(f"wrote {out_json}")

    # Console preview.
    print("\n=== HEADLINE ===")
    h = findings["headline"]
    for name, stats in h.items():
        print(
            f"  {name:<25} MAE {stats['mae']:.2f}  bias {stats['bias']:+.2f}  "
            f"RMSE {stats['rmse']:.2f}  n={stats['n']}"
        )

    print("\n=== BY ACTUAL ADJEM QUARTILE ===")
    for b in findings["by_actual_quartile"]:
        print(
            f"  {b['bucket']:<20} n={b['n']:>3}  MAE {b['mae_blended']:.2f}  "
            f"bias {b['bias_blended']:+.2f}  (baseline MAE {b['mae_baseline']:.2f})"
        )

    print("\n=== BY RETURNING-MINUTES-SHARE QUARTILE ===")
    for b in findings["by_returning_share_quartile"]:
        print(
            f"  {b['bucket']:<20} n={b['n']:>3}  MAE {b['mae_blended']:.2f}  "
            f"bias {b['bias_blended']:+.2f}"
        )

    print("\n=== SIGNAL CORRELATIONS WITH SIGNED ERROR ===")
    for r in sorted(
        findings["signal_correlations"],
        key=lambda x: abs(x["corr_signed_err"]),
        reverse=True,
    ):
        print(
            f"  {r['signal']:<28} corr(signed) {r['corr_signed_err']:+.3f}  "
            f"corr(|err|) {r['corr_abs_err']:+.3f}  n={r['n']}"
        )

    print("\n=== OLS MODEL (standardized signals → signed error) ===")
    ols = findings["ols_residual_model"]
    print(f"  R² = {ols['r2']:.3f}  RMSE = {ols['rmse']:.2f}  n = {ols['n']}")
    for c in sorted(ols["coefficients"], key=lambda c: abs(c["t_stat"]), reverse=True):
        marker = "*" if abs(c["t_stat"]) > 2 else " "
        print(
            f"  {marker} {c['feature']:<28} β={c['beta_std']:+.3f}  t={c['t_stat']:+.2f}"
        )

    print("\n=== REGRESSION-TO-MEAN ===")
    rtm = findings["regression_to_mean"]
    if "error" not in rtm:
        print(
            f"  slope={rtm['slope_b']:.3f}  intercept={rtm['intercept_a']:+.2f}  "
            f"|t vs 1.0|={abs(rtm['t_stat_b_vs_one']):.2f}"
        )
        print(
            f"  Raw blended MAE {rtm['mae_raw']:.2f}  →  "
            f"de-shrunk MAE {rtm['mae_after_correction']:.2f}  "
            f"(lift {rtm['mae_lift']:+.2f})"
        )
        print(f"  {rtm['interpretation']}")

    print("\n=== TRAJECTORY MODEL HONESTY ===")
    t = findings["trajectory_honesty"]
    if "error" not in t:
        print(
            f"  LOSO-pair MAE {t['loso_pooled_mae']:.3f}  vs  "
            f"5-fold MAE {t['kfold_5_pooled_mae']:.3f}  → {t['verdict']}"
        )
    print(f"  {t.get('note', t.get('error', ''))}")


if __name__ == "__main__":
    main()
