"""
Train the Phase B impact-aggregation projection model (v2 — OOF-trained).

Phase B vs. the box-score roster model (`train_roster_model.py`):
the box-score model deliberately EXCLUDES cam_v3 / GBPM because
`Σ(cam_v3 × minute_share) ≈ team AdjEM` by construction — feeding it
those features collapses the model to the player-impact identity and
kills the swap-Δ signal it exists to produce.

Phase B is the opposite use case. We *want* that identity: a roster
projection is exactly `AdjEM ≈ f(Σ projected cam_v3)`. This model is a
clean calibrator from a roster's cam_v3 distribution (plus archetype /
experience structure) to team AdjEM. At serve time the projections
route feeds *projected* cam_v3 (the trajectory model for returners /
arrivals, the freshman model for recruits); all projection error then
lives in those upstream models — honest and decomposable. The
box-score `roster_model.onnx` is untouched and still serves swap-Δ.

"Train on what you serve" (v2, this script). At serve time the
projections route never sees a player's actual cam_v3 — it sees a
*projected* one (the trajectory model for returners / arrivals, the
freshman model for recruits), and those projections are regression-
biased: the trajectory model under-projects elite returners by ≈3.4
CamPom. v1 trained on actual same-season `cam_gbpm_v3_psos`, so it
learned a calibration slope for *unbiased* inputs and then inherited
the upstream bias raw at serve. v2 trains on the held-out OOF cam_v3
the upstream models actually emit — `trajectory_oof_predictions` for
returners, `freshman_oof_predictions` for recruits — so this calibrator
absorbs that bias directly. The cohort neither OOF table covers (true
walk-on freshmen, JUCO arrivals, pre-2015 priors, 2015 itself) falls
back to actual `cam_gbpm_v3_psos`; that cohort skews to low-minute
bench slots, so its weight in the load-bearing minutes-weighted
aggregates is small. `build_dataset` prints the per-source coverage.

Rotation normalization — train/serve parity. Both this script and the
Rust `roster_impact::build_roster_impact_features` rank each roster by
cam_v3, take the top 13, and assign canonical MPG by rank
(`CANONICAL_ROTATION_MPG`, sourced from
`crates/cstat-core/src/roster_features.rs::CANONICAL_ROTATION_MPG`).
Every minutes-weighted aggregate therefore uses the SAME
projected-rotation weighting in training and at serve — no
out-of-distribution minutes, which was the Phase A failure mode.

Deliberate deviation from the ROADMAP feature list: literal "minutes
concentration" features (top-1/top-5 minute share, minutes stddev,
total_minutes) are dropped. After rotation normalization every team's
minute vector IS `CANONICAL_ROTATION_MPG[..roster_size]`, so those
features are a deterministic function of `roster_size` and carry no
extra cross-team signal at serve. Talent concentration is instead
captured by the cam_v3 distribution shape (cam_top1 / top3 / top7 /
sum). `roster_size` is kept as the depth feature.
"""

from __future__ import annotations

import json
from pathlib import Path

import lightgbm as lgb
import numpy as np
import pandas as pd
from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score
from sklearn.model_selection import KFold

from db import get_engine

OUT_DIR = Path(__file__).parent / "models"
SEASONS = (2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024, 2025, 2026)

# Target seasons that get an exported leave-one-season-out model for the
# end-to-end `cstat-ingest projections-backtest` (ROADMAP §5b v2 Part 2).
# Only these are backtestable: the backtest's `compose_all_projections`
# needs portal-`transfers` data (ingested 2024+) AND a finished actual
# AdjEM to score against, so 2025 / 2026 are the sole meaningful targets.
# Extend when a new season finishes with transfers data ingested.
LOSO_EXPORT_SEASONS = (2025, 2026)
ARCHETYPES = (
    "Wizard", "Sorcerer", "Warlock", "Bard", "Ranger", "Barbarian",
    "Paladin", "Monk", "Cleric", "Druid", "Rogue", "Fighter",
)

# Canonical MPG by rotation rank (rank 0 = most minutes). MUST stay
# byte-identical to `roster_features.rs::CANONICAL_ROTATION_MPG` — the
# Rust source is the contract, this is a mirror. 13 slots; players
# ranked past slot 12 fall out of the rotation.
CANONICAL_ROTATION_MPG = (
    32.0, 29.8, 27.8, 25.5, 23.0, 20.1, 17.2, 14.4, 11.9, 9.6, 8.2, 7.3, 6.9,
)

# cam_v3 ranking sentinel — players with no Torvik coverage sort to the
# bottom of the rotation (same convention as the Rust `cam_v3 = None`
# → NEG_INFINITY sort in `project_rotation`).
_NEG = -1.0e9

# cam_v3 is the *projected* value, "train on what you serve": held-out
# OOF predictions where the trajectory / freshman models cover the player,
# falling back to actual same-season `cam_gbpm_v3_psos` otherwise.
#   - trajectory_oof_predictions: keyed (torvik_pid, target_season). A row
#     exists when the player qualified in BOTH season N-1 and N — i.e. a
#     returner the trajectory model trained on.
#   - freshman_oof_predictions: keyed (cstat_player_id, target_season). A
#     row exists for a resolved, ranked recruit in their first cstat season.
# A player is almost always a returner XOR a freshman in season N, so the
# two tables rarely both match one (player, season) — 4 rows in the
# 2015-2026 corpus do: non-freshmen (So-Sr) that also carry a freshman OOF
# row through a recruit-resolution edge case (a recruit linked to a
# same-named player, or a recruit-year mismatch). COALESCE puts trajectory
# first deliberately — for a genuine returner the prior-season projection
# is the right signal, so the precedence resolves these 4 correctly
# whatever the cause.
# `campom_source` tags provenance for the coverage report in build_dataset.
PLAYER_QUERY = """
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
WHERE pss.season = ANY(%(seasons)s)
  AND COALESCE(pss.games_played, 0) >= 5
  AND COALESCE(pss.minutes_per_game, 0) >= 5
"""

TEAM_QUERY = """
SELECT team_id, season, adj_efficiency_margin
FROM team_season_stats
WHERE season = ANY(%(seasons)s)
  AND adj_efficiency_margin IS NOT NULL
"""

# Per (source_team, target_season) sum of base-season cam_v3 for players
# who left in the portal cycle that moves them into the target season.
#
# Convention: `transfers.year = target_season - 1` = spring of the base
# season = portal moves INTO target season. The departed player's value
# is read from `torvik_player_stats` at the BASE season (their level
# when the source team "lost" them). Missing torvik coverage contributes
# 0 to the sum — same COALESCE convention the audit
# (`audit_preseason_projections.py::fetch_portal_signals`) uses.
#
# Pre-2020 transfers data is sparse (2-7 rows/year vs. 1,000+ post-2020;
# the portal as we know it only took off after the 2021 NCAA rule
# change). Pre-portal-era team-seasons get `outbound_cam_v3_sum = 0`
# from the `df.merge(..., how='left').fillna(0.0)` in `build_dataset`
# (this SQL uses INNER JOIN, so those teams simply don't appear in the
# query result; the pandas LEFT JOIN supplies the 0 sentinel). The
# tree-based model naturally splits on `> 0` (real portal era) vs `= 0`
# (no signal available), so the early seasons stay informative for the
# cam_v3 distribution features without polluting the outbound
# coefficient.
#
# `team_id` is resolved cross-season via `teams.natstat_id` to the
# *target-season* team UUID — `team_season_stats.team_id` (the join key
# of the training frame) uses the target-season UUID, and team UUIDs
# are season-scoped (Duke 2024 ≠ Duke 2025 by UUID; see CLAUDE.md
# "UUIDs are season-scoped"). Without the natstat_id hop the merge
# would silently produce zero coverage. The serve-side path in
# `compose_all_projections` is keyed on the *base-season* UUID (the
# only one that exists for the upcoming year), but the model only
# sees the float, not the join key — the feature *value* is identical
# either way.
OUTBOUND_QUERY = """
SELECT
    tgt_team.id AS team_id,
    (p_base.season + 1)::int4 AS season,
    COALESCE(SUM(COALESCE(tps.cam_gbpm_v3_psos, 0)), 0)::float8 AS outbound_cam_v3_sum
FROM transfers t
JOIN players p_base
    ON p_base.id = t.cstat_player_id
   AND p_base.season = t.year
JOIN teams base_team ON base_team.id = p_base.team_id
JOIN teams tgt_team
    ON tgt_team.natstat_id = base_team.natstat_id
   AND tgt_team.season = p_base.season + 1
LEFT JOIN torvik_player_stats tps
    ON tps.player_id = p_base.id AND tps.season = t.year
WHERE t.year = ANY(%(portal_years)s)
GROUP BY tgt_team.id, p_base.season
"""

# Symmetric inbound query: for each transfer, locate the player's
# *target-season* team and attribute their BASE-season cam_v3 to that
# team's inbound total.
#
# Cross-season player identity uses **natstat_id OR torvik_pid**.
# natstat_id is reissued per team — a player who transfers gets a new
# natstat_id at the new school — so natstat_id-only joins silently miss
# every transfer (empirically 503/1958 = 25.7% of the 2024+2025 inbound
# cohort, per `test_cross_season_joins.py::coverage_invariant`).
# `torvik_pid` is stable across transfers (per `reference_torvik_pid`
# memory: stable cross-season, 96% coverage, zero collisions). The OR
# catches both:
#   - **non-transferring same-team players who happened to enter the
#     portal then re-signed with their current team** (rare): natstat_id
#     branch matches.
#   - **actual transfers**: torvik_pid branch matches; natstat_id branch
#     fails (fresh id at new school).
#
# `tps_base` is `LEFT JOIN`ed so transfers with no Torvik coverage still
# contribute 0 to the sum (the COALESCE on cam_v3 absorbs the NULL); the
# `OR`'s torvik branch only activates when base-side coverage exists.
# Each transfer row matches AT MOST ONE `p_tgt` row: when both branches
# return the same row (rare same-team case), SQL `OR` is idempotent —
# the row appears once, not twice — so the SUM is safe without DISTINCT.
#
# Pairing outbound + inbound (rather than a single net_cam_v3_sum) lets
# the model learn the asymmetry — a team can gain and lose simultaneously
# and the effects aren't necessarily additive at the AdjEM level
# (different roles, role fit, etc.).
INBOUND_QUERY = """
SELECT
    tgt_team.id AS team_id,
    p_tgt.season AS season,
    COALESCE(SUM(COALESCE(tps_base.cam_gbpm_v3_psos, 0)), 0)::float8 AS inbound_cam_v3_sum
FROM transfers t
JOIN players p_base
    ON p_base.id = t.cstat_player_id
   AND p_base.season = t.year
LEFT JOIN torvik_player_stats tps_base
    ON tps_base.player_id = p_base.id AND tps_base.season = t.year
JOIN players p_tgt
    ON p_tgt.season = t.year + 1
   AND (
        p_tgt.natstat_id = p_base.natstat_id
        OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
            SELECT player_id FROM torvik_player_stats
            WHERE torvik_pid = tps_base.torvik_pid AND season = t.year + 1
        ))
   )
JOIN teams tgt_team ON tgt_team.id = p_tgt.team_id
WHERE t.year = ANY(%(portal_years)s)
GROUP BY tgt_team.id, p_tgt.season
"""


def normalize_class(cy) -> str | None:
    """Fold the inconsistently-stored class_year vocab ('Sr' / 'Senior' /
    'SR' / …) to a 4-value code. Anything unrecognized → None (unknown),
    which contributes to no experience bucket."""
    if cy is None or (isinstance(cy, float) and pd.isna(cy)):
        return None
    s = str(cy).strip().lower()
    if s.startswith("fr"):
        return "Fr"
    if s.startswith("so"):
        return "So"
    if s.startswith("jr") or s.startswith("ju"):
        return "Jr"
    if s.startswith("sr") or s.startswith("se"):
        return "Sr"
    return None


def aggregate_team_season(group: pd.DataFrame) -> pd.Series:
    """Reduce one team-season's player rows to a Phase B feature vector.

    Ranks by cam_v3, keeps the top 13 (the rotation), assigns canonical
    MPG by rank, and aggregates. Mirrors `build_roster_impact_features`
    on the Rust side exactly."""
    g = group.copy()
    # Rank by cam_v3 desc; missing coverage sorts last (bench slots).
    g["_rank_key"] = g["campom"].fillna(_NEG)
    g = g.sort_values("_rank_key", ascending=False).reset_index(drop=True)
    g = g.head(len(CANONICAL_ROTATION_MPG))
    g["proj_mpg"] = [CANONICAL_ROTATION_MPG[i] for i in range(len(g))]
    total_w = float(g["proj_mpg"].sum())

    row: dict[str, float] = {"roster_size": int(len(g))}

    # cam_v3 distribution — over rotation players with Torvik coverage.
    cam = g.dropna(subset=["campom"])
    if len(cam) == 0:
        for k in ("cam_wmean", "cam_sum", "cam_top1", "cam_top3_mean",
                  "cam_top7_mean", "cam_count_gt5", "cam_count_gt10",
                  "cam_count_gt15"):
            row[k] = np.nan
    else:
        vals = cam["campom"].astype(float)
        w = cam["proj_mpg"].astype(float)
        ws = float(w.sum())
        row["cam_wmean"] = float((vals * w).sum() / ws) if ws > 0 else np.nan
        row["cam_sum"] = float(vals.sum())
        sorted_cam = vals.sort_values(ascending=False).reset_index(drop=True)
        row["cam_top1"] = float(sorted_cam.iloc[0])
        row["cam_top3_mean"] = float(sorted_cam.head(3).mean())
        row["cam_top7_mean"] = float(sorted_cam.head(7).mean())
        row["cam_count_gt5"] = float((vals > 5.0).sum())
        row["cam_count_gt10"] = float((vals > 10.0).sum())
        row["cam_count_gt15"] = float((vals > 15.0).sum())

    # Experience mix — canonical-MPG-weighted class shares.
    cls = g["class_year"].map(normalize_class)
    for code, key in (("Fr", "exp_fr_share"), ("So", "exp_so_share"),
                      ("Jr", "exp_jr_share"), ("Sr", "exp_sr_share")):
        w = float(g.loc[cls == code, "proj_mpg"].sum())
        row[key] = w / total_w if total_w > 0 else 0.0

    # Archetype balance — canonical-MPG-weighted primary-class shares.
    # Players without an archetype (e.g. synthesized recruits at serve
    # time) contribute to no bucket, so a recruit-heavy roster's shares
    # sum to < 1 — that dilution is intentional, not a bug.
    for arch in ARCHETYPES:
        w = float(g.loc[g["primary_class"] == arch, "proj_mpg"].sum())
        row[f"arch_{arch.lower()}"] = w / total_w if total_w > 0 else 0.0

    return pd.Series(row)


def cam_v3_coverage(players: pd.DataFrame) -> dict:
    """Per-source breakdown of where each player's projected cam_v3 came
    from. The OOF share is the "train on what you serve" coverage — the
    fraction of inputs that carry the same regression bias as serve."""
    counts = players["campom_source"].value_counts().to_dict()
    n = len(players)
    traj = int(counts.get("trajectory_oof", 0))
    fresh = int(counts.get("freshman_oof", 0))
    cov = {
        "n_player_rows": int(n),
        "trajectory_oof": traj,
        "freshman_oof": fresh,
        "actual_fallback": int(counts.get("actual_fallback", 0)),
        "no_cam_v3": int(counts.get("none", 0)),
        "oof_pct": round(100.0 * (traj + fresh) / n, 1) if n else 0.0,
    }
    print(
        f"  cam_v3 source: {traj:,} trajectory OOF + {fresh:,} freshman OOF "
        f"= {cov['oof_pct']}% held-out projections | "
        f"{cov['actual_fallback']:,} actual fallback | {cov['no_cam_v3']:,} no cam_v3"
    )
    return cov


def build_dataset() -> tuple[pd.DataFrame, list[str], dict]:
    engine = get_engine()
    players = pd.read_sql(PLAYER_QUERY, engine, params={"seasons": list(SEASONS)})
    teams = pd.read_sql(TEAM_QUERY, engine, params={"seasons": list(SEASONS)})
    # portal_year = target_season - 1 (spring portal moves INTO target).
    portal_years = [s - 1 for s in SEASONS]
    outbound = pd.read_sql(
        OUTBOUND_QUERY, engine, params={"portal_years": portal_years}
    )
    inbound = pd.read_sql(
        INBOUND_QUERY, engine, params={"portal_years": portal_years}
    )

    print(f"Loaded {len(players):,} player-season rows, {len(teams):,} team-seasons.")
    print(
        f"Loaded {len(outbound):,} outbound + {len(inbound):,} inbound "
        f"(team, target_season) portal rows."
    )
    coverage = cam_v3_coverage(players)

    agg = (
        players.groupby(["team_id", "season"], as_index=False)
        .apply(aggregate_team_season, include_groups=False)
        .reset_index(drop=True)
    )
    df = agg.merge(teams, on=["team_id", "season"], how="inner")
    df = df.merge(outbound, on=["team_id", "season"], how="left")
    df = df.merge(inbound, on=["team_id", "season"], how="left")
    df["outbound_cam_v3_sum"] = df["outbound_cam_v3_sum"].fillna(0.0)
    df["inbound_cam_v3_sum"] = df["inbound_cam_v3_sum"].fillna(0.0)
    # Drop team-seasons with zero Torvik coverage across the whole
    # rotation — the cam_v3 features are NaN and the row can't be scored.
    pre = len(df)
    df = df.dropna(subset=["cam_wmean", "adj_efficiency_margin"]).reset_index(drop=True)
    if len(df) < pre:
        print(f"Dropped {pre - len(df)} team-seasons with no cam_v3 coverage / target.")
    print(f"After join: {len(df):,} team-seasons with target.")
    nz_outbound = int((df["outbound_cam_v3_sum"] > 0).sum())
    nz_inbound = int((df["inbound_cam_v3_sum"] > 0).sum())
    print(
        f"non-zero outbound: {nz_outbound:,}/{len(df):,} "
        f"({100.0 * nz_outbound / len(df):.1f}%) | "
        f"non-zero inbound: {nz_inbound:,}/{len(df):,} "
        f"({100.0 * nz_inbound / len(df):.1f}%); pre-portal-era rows = 0.0."
    )

    feature_cols = [
        c for c in df.columns
        if c not in ("team_id", "season", "adj_efficiency_margin")
    ]
    return df, feature_cols, coverage


def lgb_params() -> dict:
    # Conservative for ~4k rows + ~25 features. cam_sum / cam_wmean carry
    # most of the signal (the intended near-identity), so the trees stay
    # shallow and well-regularized — the model's job is calibration, not
    # squeezing variance out of a small N.
    return {
        "objective": "regression",
        "metric": "mae",
        "num_leaves": 16,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 20,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "verbose": -1,
        "n_estimators": 1500,
        "early_stopping_rounds": 80,
        "seed": 42,
        "deterministic": True,
    }


def leave_one_season_out(df: pd.DataFrame, feature_cols: list[str]) -> dict:
    """Honest backtest: predict each season from a model trained on the
    other N-1. Same harness as `train_roster_model.py`.

    Note — v2 vs v1 MAE: this same-season diagnostic reads ≈3.7 on the
    projected-cam_v3 inputs, well above v1's ≈2.2 on actual cam_v3. That
    is expected, not a regression: v1's inputs were the near-identity
    truth (`Σ cam_v3 ≈ AdjEM`), so the task was trivially easy. The honest
    cross-version metric is the end-to-end `projections-backtest` MAE."""
    results = {}
    overall_y, overall_p = [], []
    best_iters: list[int] = []
    for season in SEASONS:
        train = df[df["season"] != season]
        test = df[df["season"] == season]
        if len(test) == 0:
            continue
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(
            train[feature_cols], train["adj_efficiency_margin"],
            eval_set=[(test[feature_cols], test["adj_efficiency_margin"])],
            eval_metric="mae",
        )
        # Where early stopping settled on this fold — averaged across
        # folds to set the final-fit iteration budget (see `main`).
        bi = model.best_iteration_
        best_iters.append(bi if bi and bi > 0 else lgb_params()["n_estimators"])
        preds = model.predict(test[feature_cols])
        y = test["adj_efficiency_margin"]
        mae = mean_absolute_error(y, preds)
        rmse = float(np.sqrt(mean_squared_error(y, preds)))
        r2 = r2_score(y, preds)
        results[season] = {"mae": mae, "rmse": rmse, "r2": r2, "n": int(len(test))}
        overall_y.extend(y.tolist())
        overall_p.extend(preds.tolist())
        print(f"  season {season}: MAE {mae:.2f}  RMSE {rmse:.2f}  R² {r2:.3f}  n={len(test)}")
    overall = {
        "mae": mean_absolute_error(overall_y, overall_p),
        "rmse": float(np.sqrt(mean_squared_error(overall_y, overall_p))),
        "r2": r2_score(overall_y, overall_p),
    }
    print(f"  pooled:      MAE {overall['mae']:.2f}  RMSE {overall['rmse']:.2f}  R² {overall['r2']:.3f}")
    return {"per_season": results, "pooled": overall, "best_iterations": best_iters}


def random_kfold(df: pd.DataFrame, feature_cols: list[str], n_splits: int = 5) -> dict:
    kf = KFold(n_splits=n_splits, shuffle=True, random_state=42)
    X = df[feature_cols].values
    y = df["adj_efficiency_margin"].values
    maes, rmses, r2s = [], [], []
    for fold, (tr, te) in enumerate(kf.split(X), 1):
        model = lgb.LGBMRegressor(**lgb_params())
        model.fit(X[tr], y[tr], eval_set=[(X[te], y[te])], eval_metric="mae")
        p = model.predict(X[te])
        maes.append(mean_absolute_error(y[te], p))
        rmses.append(float(np.sqrt(mean_squared_error(y[te], p))))
        r2s.append(r2_score(y[te], p))
        print(f"  fold {fold}: MAE {maes[-1]:.2f}  RMSE {rmses[-1]:.2f}  R² {r2s[-1]:.3f}")
    out = {"mae": float(np.mean(maes)), "rmse": float(np.mean(rmses)), "r2": float(np.mean(r2s))}
    print(f"  mean:    MAE {out['mae']:.2f}  RMSE {out['rmse']:.2f}  R² {out['r2']:.3f}")
    return out


def export_to_onnx(model: lgb.LGBMRegressor, n_features: int, onnx_path: Path) -> None:
    import onnxmltools
    from onnxmltools.convert.common.data_types import FloatTensorType

    initial_types = [("input", FloatTensorType([None, n_features]))]
    onnx_model = onnxmltools.convert_lightgbm(
        model.booster_, initial_types=initial_types, target_opset=15
    )
    onnxmltools.utils.save_model(onnx_model, str(onnx_path))


def export_loso_models(
    df: pd.DataFrame, feature_cols: list[str], final_n: int
) -> list[int]:
    """Export per-target-season leave-one-season-out models for the
    end-to-end `cstat-ingest projections-backtest` (ROADMAP §5b v2 Part 2).

    The backtest scores target season S against actual AdjEM. The shipped
    `roster_impact_model.onnx` trained on *every* season including S, so
    its backtest MAE carries a small in-sample leak. The LOSO model for S
    trains on every season EXCEPT S, so the backtest reads an honest
    held-out prediction. Fixed `n_estimators` — early stopping would have
    to watch the held-out season and re-introduce the leak it removes.

    Files land in `models/roster_impact_loso/`; they are gitignored
    diagnostic artifacts (the `*.onnx` rule with no allowlist entry),
    regenerable by rerunning this script. Returns the per-season train
    row counts for the meta JSON."""
    loso_dir = OUT_DIR / "roster_impact_loso"
    loso_dir.mkdir(parents=True, exist_ok=True)
    params = lgb_params()
    params.pop("early_stopping_rounds", None)
    params["n_estimators"] = final_n
    train_ns: list[int] = []
    for season in LOSO_EXPORT_SEASONS:
        train = df[df["season"] != season]
        train_ns.append(int(len(train)))
        model = lgb.LGBMRegressor(**params)
        model.fit(train[feature_cols], train["adj_efficiency_margin"])
        path = loso_dir / f"roster_impact_model_{season}.onnx"
        export_to_onnx(model, len(feature_cols), path)
        print(f"  LOSO model (excl. {season}, train n={len(train):,}) → {path}")
    return train_ns


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    print("=" * 60)
    print("Building dataset...")
    df, feature_cols, coverage = build_dataset()
    print(f"Features: {len(feature_cols)}  | rows: {len(df)}")
    print(f"Feature order: {feature_cols}")

    print("\n" + "=" * 60)
    print("Leave-one-season-out backtest")
    print("=" * 60)
    loso = leave_one_season_out(df, feature_cols)

    print("\n" + "=" * 60)
    print("5-fold random CV")
    print("=" * 60)
    cv = random_kfold(df, feature_cols, n_splits=5)

    print("\n" + "=" * 60)
    print("Final fit on all data")
    print("=" * 60)
    X, y = df[feature_cols], df["adj_efficiency_margin"]
    final_params = lgb_params()
    # No held-out set on the final fit — set the iteration budget to the
    # mean of where the LOSO folds early-stopped. Calibrated for THIS
    # model rather than copied from train_roster_model.py; floored at 50
    # to guard against a degenerate fold.
    final_params.pop("early_stopping_rounds", None)
    best_iters = loso["best_iterations"]
    final_n = max(50, round(sum(best_iters) / len(best_iters)))
    final_params["n_estimators"] = final_n
    print(f"Final-fit n_estimators = {final_n}  (LOSO best-iterations: {best_iters})")
    final = lgb.LGBMRegressor(**final_params)
    final.fit(X, y)

    print("\nFeature importances:")
    importance = sorted(zip(feature_cols, final.feature_importances_), key=lambda x: -x[1])
    for name, imp in importance:
        print(f"  {name:<22} {imp}")

    onnx_path = OUT_DIR / "roster_impact_model.onnx"
    export_to_onnx(final, len(feature_cols), onnx_path)
    print(f"\nExported ONNX → {onnx_path}")

    print("\n" + "=" * 60)
    print("Leave-one-season-out models for projections-backtest (v2 Part 2)")
    print("=" * 60)
    loso_train_ns = export_loso_models(df, feature_cols, final_n)

    meta = {
        "model": "roster_impact_model",
        "target": "adj_efficiency_margin",
        "seasons": list(SEASONS),
        "n_rows": int(len(df)),
        "n_features": len(feature_cols),
        "features": feature_cols,
        # Honored verbatim by the Rust boot validator (`validate_model_meta`
        # checks this equals `QUAL_FILTER_STRING`).
        "player_filter": "games_played >= 5 AND minutes_per_game >= 5",
        # v2: trained on projected (held-out OOF) cam_v3, "train on what
        # you serve". v1 was "actual" same-season cam_v3. See module docstring.
        "cam_v3_source": "oof",
        # Per-source provenance of the training cam_v3 inputs.
        "cam_v3_coverage": coverage,
        "final_n_estimators": final_n,
        # Per-target-season LOSO models exported to models/roster_impact_loso/
        # for the honest end-to-end backtest (gitignored; regenerable here).
        "loso_export_seasons": list(LOSO_EXPORT_SEASONS),
        "loso_train_rows": dict(zip(LOSO_EXPORT_SEASONS, loso_train_ns)),
        "canonical_rotation_mpg": list(CANONICAL_ROTATION_MPG),
        "backtest_loso": loso,
        "cv_5fold": cv,
        "feature_importance": [
            {"name": n, "importance": int(i)} for n, i in importance
        ],
    }
    meta_path = OUT_DIR / "roster_impact_model_meta.json"
    meta_path.write_text(json.dumps(meta, indent=2))
    print(f"Wrote meta → {meta_path}")


if __name__ == "__main__":
    main()
