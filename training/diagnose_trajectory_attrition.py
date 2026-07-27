"""PR B mechanism check: is the trajectory model's over-projection bias
concentrated on returners from high-attrition (thin) rosters?

Hypothesis (from `decompose_projection_error.py`): when a team loses a
large share of its prior-season cam_v3, the trajectory model projects the
*surviving* returners closer to their class-year-archetype average than to
their actual (modest) current selves — so it over-projects them. That
upstream bias is what shows up as the +5.62 Q1 over-projection at the
team-projection level.

This script confirms (or refutes) the mechanism at the *player* level,
before we pick a model-side fix, by:

  1. Computing each season-N team's cam_v3 attrition ratio
     `1 - retained_pos_cam / total_pos_cam` (talent share lost to the
     portal / graduation / draft before season N+1). Positive-clamped
     cam_v3 so the ratio reads as "share of productive talent lost".
  2. Joining every persisted trajectory OOF (LOPO held-out) prediction to
     its player's season-N team attrition + prior CamPom + actual N+1
     CamPom.
  3. Bucketing OOF bias (`pred - actual`, + = over-projected) by
     (attrition quartile × prior-CamPom bucket).

If the mechanism is real we expect: within a given prior-CamPom band,
bias rises monotonically with attrition — i.e. the model over-projects
returners more the more talent their team lost. If bias is flat across
attrition quartiles, approach (a) (an attrition feature) won't help and we
should reach for (b)/(c) instead.

Outputs:
- `training/eval_history/trajectory_attrition_{date}_summary.json`
- Console: per-attrition-quartile bias, the (attrition × campom) grid, and
  the returner-only slice (the cohort the mechanism is actually about).
"""

from __future__ import annotations

import datetime as dt
import json
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import text

from compute_cae import read_dump_records
from db import get_engine

EVAL_DIR = Path(__file__).resolve().parent / "eval_history"

# Each OOF prediction joined to its season-N team attrition, prior CamPom,
# and actual N+1 CamPom. Attrition is a property of the player's *prior*
# (season-N) team — the roster they're leaving behind / staying with —
# which is what's knowable at projection time and what approach (a) would
# feed the model.
QUERY = """
WITH roster AS (
    SELECT pss.team_id, pss.season, tps.torvik_pid,
           GREATEST(tps.cam_gbpm_v3_psos, 0) AS pos_cam,
           tps.cam_gbpm_v3_psos AS raw_cam
    FROM player_season_stats pss
    JOIN torvik_player_stats tps
        ON tps.player_id = pss.player_id AND tps.season = pss.season
    WHERE pss.games_played >= 5 AND pss.minutes_per_game >= 5
      AND tps.cam_gbpm_v3_psos IS NOT NULL
      AND tps.torvik_pid IS NOT NULL
),
prog_next AS (
    -- season-N team → same-program season-N+1 team via natstat_id
    SELECT t0.id AS team_id, t0.season, t1.id AS next_team_id
    FROM teams t0
    JOIN teams t1 ON t1.natstat_id = t0.natstat_id AND t1.season = t0.season + 1
),
roster_ret AS (
    -- Set-wise retention: a roster row is retained when the same torvik_pid
    -- shows up on its program's season-N+1 team. LEFT JOIN (not correlated
    -- EXISTS) so this stays one pass over the roster. The inner JOIN to
    -- prog_next drops teams with no same-program successor — those have no
    -- well-defined attrition and fall out of the diagnostic.
    SELECT r.team_id, r.season, r.torvik_pid, r.pos_cam, r.raw_cam,
        CASE WHEN r2.torvik_pid IS NOT NULL THEN 1 ELSE 0 END AS retained
    FROM roster r
    JOIN prog_next pn ON pn.team_id = r.team_id AND pn.season = r.season
    LEFT JOIN roster r2
        ON r2.torvik_pid = r.torvik_pid
       AND r2.season = r.season + 1
       AND r2.team_id = pn.next_team_id
),
team_attr AS (
    SELECT team_id, season,
        SUM(pos_cam) AS total_pos,
        SUM(CASE WHEN retained = 1 THEN pos_cam ELSE 0 END) AS retained_pos,
        COUNT(*) AS n_players,
        SUM(retained) AS n_retained
    FROM roster_ret
    GROUP BY team_id, season
)
SELECT
    oof.torvik_pid,
    oof.target_season,
    oof.mean        AS pred,
    tps_n.cam_gbpm_v3_psos   AS prior_campom,
    tps_np1.cam_gbpm_v3_psos AS actual_campom,
    pss_n.team_id   AS prior_team_id,
    pn.next_team_id AS prior_team_next_id,
    pss_np1.team_id AS actual_team_id,
    ta.total_pos,
    ta.retained_pos,
    ta.n_players,
    ta.n_retained
FROM trajectory_oof_predictions oof
JOIN torvik_player_stats tps_n
    ON tps_n.torvik_pid = oof.torvik_pid AND tps_n.season = oof.target_season - 1
JOIN torvik_player_stats tps_np1
    ON tps_np1.torvik_pid = oof.torvik_pid AND tps_np1.season = oof.target_season
JOIN player_season_stats pss_n
    ON pss_n.player_id = tps_n.player_id AND pss_n.season = tps_n.season
JOIN player_season_stats pss_np1
    ON pss_np1.player_id = tps_np1.player_id AND pss_np1.season = tps_np1.season
LEFT JOIN team_attr ta
    ON ta.team_id = pss_n.team_id AND ta.season = tps_n.season
LEFT JOIN prog_next pn
    ON pn.team_id = pss_n.team_id AND pn.season = tps_n.season
WHERE tps_n.cam_gbpm_v3_psos IS NOT NULL
  AND tps_np1.cam_gbpm_v3_psos IS NOT NULL
"""

CAMPOM_BUCKETS = [
    ("<0", float("-inf"), 0.0),
    ("0..+5", 0.0, 5.0),
    ("+5..+10", 5.0, 10.0),
    (">=+10", 10.0, float("inf")),
]

# Team-level attrition for every (team, season): share of positive cam_v3
# the program did NOT retain into season+1. Joined to the projections
# backtest dump (team_id is the BASE-season UUID) to test whether the
# Q1 team-level over-projection is concentrated on high-attrition rosters
# (which would vindicate a trajectory attrition feature) or is flat across
# attrition (which would point the fix elsewhere — composition / freshman).
TEAM_ATTR_QUERY = """
WITH roster AS (
    SELECT pss.team_id, pss.season, tps.torvik_pid,
           GREATEST(tps.cam_gbpm_v3_psos, 0) AS pos_cam
    FROM player_season_stats pss
    JOIN torvik_player_stats tps
        ON tps.player_id = pss.player_id AND tps.season = pss.season
    WHERE pss.games_played >= 5 AND pss.minutes_per_game >= 5
      AND tps.cam_gbpm_v3_psos IS NOT NULL AND tps.torvik_pid IS NOT NULL
),
prog_next AS (
    SELECT t0.id AS team_id, t0.season, t1.id AS next_team_id
    FROM teams t0 JOIN teams t1
        ON t1.natstat_id = t0.natstat_id AND t1.season = t0.season + 1
),
roster_ret AS (
    SELECT r.team_id, r.season, r.pos_cam,
        CASE WHEN r2.torvik_pid IS NOT NULL THEN 1 ELSE 0 END AS retained
    FROM roster r
    JOIN prog_next pn ON pn.team_id = r.team_id AND pn.season = r.season
    LEFT JOIN roster r2
        ON r2.torvik_pid = r.torvik_pid AND r2.season = r.season + 1
       AND r2.team_id = pn.next_team_id
)
SELECT team_id::text AS base_team_id, season AS base_season,
    SUM(pos_cam) AS total_pos,
    SUM(CASE WHEN retained = 1 THEN pos_cam ELSE 0 END) AS retained_pos
FROM roster_ret GROUP BY team_id, season
"""
BACKTEST_DUMP_GLOB = "projections_backtest_per_team_*.json"


def load() -> pd.DataFrame:
    engine = get_engine()
    df = pd.read_sql(text(QUERY), engine)
    print(f"loaded {len(df):,} OOF rows joined to prior-team attrition")
    # Attrition ratio = share of positive cam_v3 talent NOT retained by the
    # season-N team. NULL team_attr (team absent next season / no qualified
    # roster) drops out — can't define attrition for it.
    df = df.dropna(subset=["total_pos", "prior_campom", "actual_campom"])
    df = df[df["total_pos"] > 0].copy()
    df["attrition"] = 1.0 - (df["retained_pos"] / df["total_pos"])
    df["attrition"] = df["attrition"].clip(0.0, 1.0)
    df["bias"] = df["pred"] - df["actual_campom"]
    df["abs_err"] = df["bias"].abs()
    # Returner = the player's actual N+1 team is the same-program successor
    # of their season-N team. Transfers (actual_team_id != prior_team_next_id)
    # are the destination-agnostic cohort; the mechanism is about returners.
    df["is_returner"] = df["actual_team_id"] == df["prior_team_next_id"]
    print(f"  after attrition gate: {len(df):,} rows "
          f"({df['is_returner'].sum():,} returners, "
          f"{(~df['is_returner']).sum():,} transfers)")
    return df


def campom_bucket(v: float) -> str:
    for label, lo, hi in CAMPOM_BUCKETS:
        if lo <= v < hi:
            return label
    return "?"


def summarize_quartiles(df: pd.DataFrame) -> tuple[list[dict], list[float], list[str]]:
    """Bias by attrition quartile (pooled across CamPom). Quartile edges
    from the full population so the returner/transfer slices share bins.
    Returns (per-quartile records, bin bounds, bin labels) — the bounds and
    labels are reused by `grid()` so its columns line up with these rows."""
    edges = df["attrition"].quantile([0.25, 0.5, 0.75]).values
    bounds = [-np.inf, edges[0], edges[1], edges[2], np.inf]
    labels = [
        f"Q1 low (≤{edges[0]:.2f})",
        f"Q2 ({edges[0]:.2f}–{edges[1]:.2f})",
        f"Q3 ({edges[1]:.2f}–{edges[2]:.2f})",
        f"Q4 high (>{edges[2]:.2f})",
    ]
    out = []
    for i in range(4):
        m = (df["attrition"] > bounds[i]) & (df["attrition"] <= bounds[i + 1])
        sub = df[m]
        if len(sub) == 0:
            continue
        out.append({
            "bucket": labels[i],
            "n": int(len(sub)),
            "mean_attrition": float(sub["attrition"].mean()),
            "mean_prior_campom": float(sub["prior_campom"].mean()),
            "mae": float(sub["abs_err"].mean()),
            "bias": float(sub["bias"].mean()),
        })
    return out, bounds, labels


def grid(
    df: pd.DataFrame, bounds: list[float], attr_labels: list[str]
) -> tuple[pd.DataFrame, pd.DataFrame]:
    """Mean bias in each (attrition quartile × prior-CamPom bucket) cell.
    The load-bearing view: within a CamPom band, does bias climb with
    attrition?"""
    df = df.copy()
    df["attr_q"] = pd.cut(df["attrition"], bins=bounds, labels=attr_labels)
    df["campom_b"] = df["prior_campom"].map(campom_bucket)
    campom_order = [b[0] for b in CAMPOM_BUCKETS]
    bias_grid = df.pivot_table(
        index="campom_b", columns="attr_q", values="bias",
        aggfunc="mean", observed=False,
    ).reindex(campom_order)
    n_grid = df.pivot_table(
        index="campom_b", columns="attr_q", values="bias",
        aggfunc="count", observed=False,
    ).reindex(campom_order)
    return bias_grid, n_grid


def team_level_cut() -> dict | None:
    """Bridge the player-level finding to the team-level Q1 over-projection.
    Joins the latest projections backtest dump to base-roster attrition and
    reports pipeline bias (phase_b − actual) by attrition tercile, plus the
    bust-team (bottom-actual-quartile) slice split by attrition. If bust
    teams are over-projected regardless of attrition, the trajectory
    attrition feature won't move Q1."""
    dumps = sorted(EVAL_DIR.glob(BACKTEST_DUMP_GLOB))
    if not dumps:
        print("\n(no projections backtest dump — skipping team-level cut)")
        return None
    # Both dump shapes (#238 envelope / historical bare array).
    bt = pd.DataFrame(read_dump_records(dumps[-1]))
    # Back-compat for the phase_b→roster_proj dump-key rename (legacy column name kept).
    if "roster_proj" in bt.columns and "phase_b" not in bt.columns:
        bt["phase_b"] = bt["roster_proj"]
    ta = pd.read_sql(text(TEAM_ATTR_QUERY), get_engine())
    ta = ta[ta["total_pos"] > 0].copy()
    ta["attrition"] = (1.0 - ta["retained_pos"] / ta["total_pos"]).clip(0, 1)
    # The backtest team_id is the BASE-season (season − 1) UUID.
    bt["base_season"] = bt["season"] - 1
    m = bt.merge(
        ta[["base_team_id", "base_season", "attrition"]],
        left_on=["team_id", "base_season"],
        right_on=["base_team_id", "base_season"], how="left",
    ).dropna(subset=["attrition"])
    m["err"] = m["phase_b"] - m["actual"]  # + = over-projected

    edges = m["attrition"].quantile([1 / 3, 2 / 3]).values
    bounds = [-1, edges[0], edges[1], 2]
    labels = [f"low(≤{edges[0]:.2f})", f"mid({edges[0]:.2f}-{edges[1]:.2f})",
              f"high(>{edges[1]:.2f})"]
    m["aq"] = pd.cut(m["attrition"], bins=bounds, labels=labels)

    by_attr = []
    print(f"\n{'='*68}\nTEAM-LEVEL pipeline err by base-roster attrition "
          f"(matched {len(m)}/{len(bt)})\n{'='*68}")
    print(f"  {'tercile':<20}{'n':>5}{'attr':>7}{'MAE':>7}{'bias':>8}{'meanAct':>9}")
    for lab in labels:
        s = m[m["aq"] == lab]
        if len(s) == 0:
            continue
        rec = {"tercile": lab, "n": int(len(s)),
               "mean_attrition": float(s["attrition"].mean()),
               "mae": float(s["err"].abs().mean()), "bias": float(s["err"].mean()),
               "mean_actual": float(s["actual"].mean())}
        by_attr.append(rec)
        print(f"  {lab:<20}{rec['n']:>5}{rec['mean_attrition']:>7.2f}"
              f"{rec['mae']:>7.2f}{rec['bias']:>+8.2f}{rec['mean_actual']:>+9.2f}")

    # Bust slice: bottom actual quartile (the Q1 cohort). A median split
    # degenerates because >50% of bust teams have attrition = 1.0 (they
    # retained ~zero positive cam_v3) — the median IS 1.0, leaving an empty
    # high bin. That clustering is itself the point: most busts gutted their
    # roster. The decisive test is the *other* tail — bust teams that KEPT
    # most of their talent (attrition < 0.5) and STILL busted. If those are
    # over-projected just as much, attrition cannot separate the
    # over-projected busts. We also report the within-bust correlation of
    # attrition vs error — near-zero means no monotone signal to learn.
    bust = m[m["actual"] <= m["actual"].quantile(0.25)]
    KEPT_THRESHOLD = 0.5  # lost < half their productive cam_v3
    bust_kept = bust[bust["attrition"] < KEPT_THRESHOLD]  # kept talent, still busted
    bust_lost = bust[bust["attrition"] >= KEPT_THRESHOLD]  # gutted roster
    attr_err_corr = (
        float(bust["attrition"].corr(bust["err"])) if len(bust) > 2 else float("nan")
    )
    bust_rec = {
        "n": int(len(bust)), "bias": float(bust["err"].mean()),
        "mean_attrition": float(bust["attrition"].mean()),
        "attrition_err_corr": attr_err_corr,
        "kept_threshold": KEPT_THRESHOLD,
        "kept_talent": {"n": int(len(bust_kept)),
                        "bias": float(bust_kept["err"].mean()) if len(bust_kept) else None},
        "lost_talent": {"n": int(len(bust_lost)),
                        "bias": float(bust_lost["err"].mean()) if len(bust_lost) else None},
    }
    print(f"\n  Bust teams (bottom actual quartile): n={bust_rec['n']} "
          f"bias={bust_rec['bias']:+.2f} mean_attr={bust_rec['mean_attrition']:.2f}  "
          f"corr(attrition, err)={attr_err_corr:+.2f}")
    kt, lt = bust_rec["kept_talent"], bust_rec["lost_talent"]
    print(f"    bust + KEPT talent (attr<{KEPT_THRESHOLD}): n={kt['n']} "
          f"bias={kt['bias']:+.2f}" if kt["bias"] is not None else
          f"    bust + KEPT talent (attr<{KEPT_THRESHOLD}): n={kt['n']} (none)")
    print(f"    bust + LOST talent (attr≥{KEPT_THRESHOLD}): n={lt['n']} "
          f"bias={lt['bias']:+.2f}" if lt["bias"] is not None else
          f"    bust + LOST talent (attr≥{KEPT_THRESHOLD}): n={lt['n']} (none)")
    return {"by_attrition_tercile": by_attr, "bust_slice": bust_rec}


def main() -> None:
    df = load()

    findings = {"generated_at": dt.datetime.utcnow().isoformat() + "Z",
                "n": int(len(df))}

    for slice_name, sdf in (
        ("all", df),
        ("returners_only", df[df["is_returner"]]),
    ):
        quart, bounds, labels = summarize_quartiles(sdf)
        bias_grid, n_grid = grid(sdf, bounds, labels)
        findings[slice_name] = {
            "n": int(len(sdf)),
            "quartiles": quart,
            "bias_grid": json.loads(bias_grid.to_json()),
            "n_grid": json.loads(n_grid.to_json()),
        }

        print(f"\n{'='*68}\n{slice_name.upper()}  (n={len(sdf):,})\n{'='*68}")
        print("  Bias (pred − actual; + = OVER-projected) by attrition quartile:")
        print(f"  {'bucket':<22} {'n':>5} {'attr':>6} {'priorCP':>8} {'MAE':>6} {'bias':>7}")
        for b in quart:
            print(f"  {b['bucket']:<22} {b['n']:>5} {b['mean_attrition']:>6.2f} "
                  f"{b['mean_prior_campom']:>+8.2f} {b['mae']:>6.2f} {b['bias']:>+7.2f}")
        print("\n  Mean bias grid (rows = prior CamPom, cols = attrition quartile):")
        with pd.option_context("display.float_format", lambda x: f"{x:+.2f}"):
            print(bias_grid.to_string())
        print("\n  Cell counts:")
        print(n_grid.fillna(0).astype(int).to_string())

    findings["team_level"] = team_level_cut()

    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out_json = EVAL_DIR / f"trajectory_attrition_{date_str}_summary.json"
    out_json.write_text(json.dumps(findings, indent=2, default=float))
    print(f"\nwrote {out_json}")


if __name__ == "__main__":
    main()
