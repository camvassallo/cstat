"""Destination-aware trajectory experiment — does telling the trajectory model
WHERE a player will play next season fix its team-context bias?

The served trajectory model is destination-agnostic by design (v1,
"documented limitation"): 60 features, none about the program the player
plays for in the target season. Its out-of-fold error is conditioned on
that program's strength, ex-ante — players at a program whose PRIOR-season
AdjEM was >= 25 are under-projected by -0.5 (2016-23) / -0.95 (2024+) each,
transfers INTO one by -0.44 / -1.13, players at weak programs over-projected
by +0.3 to +0.8. Monotone in destination strength, larger in the portal era,
and across a 13-man rotation it is the size of the elite-team gap that no
calibrator or blend change closed (#351, `docs/projections_methodology.md`
"Known limit").

Every candidate feature here is known before the target season starts, and
the serve side has all of them: a returner's destination is the team being
projected, an arrival's destination is that same team (the portal
commitment), and the source is the player's prior-season team.

  dest_prior_adj_em    destination program's AdjEM in season N
  dest_program_level   its mean AdjEM over N-3..N-1 (>= 2 seasons, else the prior)
  src_prior_adj_em     source program's AdjEM in season N (== dest for a returner)
  dest_minus_src       dest_prior_adj_em - src_prior_adj_em (0 for a returner)
  is_transfer          destination program != source program

Three feature sets, each leave-one-pair-out with the trainer's own params
and frame (`train_trajectory_model.build_dataset`, which since 2026-09
carries the block; "base" removes it), scored on the mean model:

  base        the shipped 60
  context     base + is_transfer + src_prior_adj_em — the PRIOR team's
              strength, which for a returner is the destination and for a
              transfer is deliberately not (so this is destination-blind
              exactly where it matters)
  dest        base + all five

Reported: pooled MAE, per-pair MAE, bias by destination-prior tier x era,
the team-level implication (sum of per-player bias over each destination
team-season's rotation, by destination tier), and the paired z of each set
against base.

Run:  cd training && ./.venv/bin/python experiments/experiment_trajectory_destination.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import datetime as dt
import json
import math

import lightgbm as lgb
import numpy as np
import pandas as pd

from compute_cae import EVAL_DIR
from train_trajectory_model import (
    DEST_FEATURE_COLS,
    FEATURE_COLS,
    build_dataset,
    lgb_params,
)

# The trainer's frame now carries the block itself (`DEST_FEATURE_COLS`);
# "base" is the pre-2026-09 contract, reconstructed by removing it.
DEST_COLS = list(DEST_FEATURE_COLS)
BASE_COLS = [c for c in FEATURE_COLS if c not in DEST_COLS]
SETS = {
    "base": BASE_COLS,
    "context": BASE_COLS + ["is_transfer", "src_prior_adj_em"],
    "dest": BASE_COLS + DEST_COLS,
}


def tier(v: float) -> str:
    if v >= 25:
        return "a >=25"
    if v >= 10:
        return "b 10-25"
    if v >= 0:
        return "c 0-10"
    return "d <0"


def lopo(df: pd.DataFrame, cols: list[str]) -> pd.Series:
    preds = pd.Series(index=df.index, dtype=float)
    for (s_n, s_np1), test in df.groupby(["s_n", "s_np1"]):
        train = df[(df["s_n"] != s_n) | (df["s_np1"] != s_np1)]
        m = lgb.LGBMRegressor(**lgb_params())
        m.fit(train[cols], train["target_campom"])
        preds.loc[test.index] = m.predict(test[cols])
    return preds


def paired_z(err_a: pd.Series, err_b: pd.Series) -> tuple[float, float]:
    d = err_a.abs() - err_b.abs()
    m = float(d.mean())
    sd = float(d.std(ddof=1))
    return m, (m / (sd / math.sqrt(len(d))) if sd > 0 else 0.0)


def main() -> None:
    df = build_dataset()
    print(f"rows {len(df):,}; transfers: {int(df['is_transfer'].sum()):,}")

    preds = {name: lopo(df, cols) for name, cols in SETS.items()}
    errs = {name: p - df["target_campom"] for name, p in preds.items()}

    summary = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "n": int(len(df)),
               "sets": {k: v for k, v in SETS.items()}, "pooled": {}, "per_pair": {},
               "by_dest_tier": {}, "team_level": {}, "paired_vs_base": {}}

    print("\npooled LOPO MAE:")
    for name, e in errs.items():
        summary["pooled"][name] = {"mae": float(e.abs().mean()), "bias": float(e.mean())}
        print(f"  {name:<10} MAE {e.abs().mean():.4f}  bias {e.mean():+.3f}")
    print("\npaired vs base (mean |err| delta, z; negative = better):")
    for name in ("context", "dest"):
        m, z = paired_z(errs[name], errs["base"])
        summary["paired_vs_base"][name] = {"delta": m, "z": z}
        mod = df["s_n"] >= 2023
        mm, zz = paired_z(errs[name][mod], errs["base"][mod])
        summary["paired_vs_base"][name]["targets_2024+"] = {"delta": mm, "z": zz}
        print(f"  {name:<10} {m:+.4f} z={z:+.2f}   | targets 2024+: {mm:+.4f} z={zz:+.2f}")

    print("\nper-pair MAE (base / context / dest):")
    for (s_n, s_np1), g in df.groupby(["s_n", "s_np1"]):
        key = f"{s_n}->{s_np1}"
        summary["per_pair"][key] = {n: float(errs[n][g.index].abs().mean()) for n in SETS}
        print(f"  {key}: " + "  ".join(f"{errs[n][g.index].abs().mean():.3f}" for n in SETS) + f"   n={len(g)}")

    df["dest_tier"] = df["dest_prior_adj_em"].map(tier)
    df["era"] = np.where(df["s_np1"] >= 2024, "2024+", "2016-23")
    df["kind"] = np.where(df["is_transfer"] == 1.0, "transfer", "returner")
    print("\nbias by destination-prior tier (pred - actual): base / context / dest")
    print(f"{'kind':<9}{'tier':<9}{'era':<9}{'n':>6}" + "".join(f"{n:>22}" for n in SETS))
    for (kind, t, era), g in df.groupby(["kind", "dest_tier", "era"]):
        row = {n: {"bias": float(errs[n][g.index].mean()), "mae": float(errs[n][g.index].abs().mean())}
               for n in SETS}
        summary["by_dest_tier"][f"{kind}|{t}|{era}"] = {"n": int(len(g)), **row}
        print(f"{kind:<9}{t:<9}{era:<9}{len(g):>6}" +
              "".join(f"{row[n]['bias']:>+10.2f}{row[n]['mae']:>12.3f}" for n in SETS))

    # Team-level implication: sum the per-player error over each destination
    # team-season (every projected returner/arrival the trainer's frame has
    # for that team), then average by destination tier. This is the
    # direction and rough size of what the roster projection inherits.
    print("\nteam-level: mean Σ(pred - actual) over a destination team-season's players, by dest tier x era")
    for n in SETS:
        df[f"err_{n}"] = errs[n]
    team = df.groupby(["dest_ns", "s_np1", "dest_tier", "era"]).agg(
        n_players=("target_campom", "size"),
        **{f"sum_{n}": (f"err_{n}", "sum") for n in SETS},
    ).reset_index()
    print(f"{'tier':<9}{'era':<9}{'teams':>6}{'players/team':>13}" + "".join(f"{n:>12}" for n in SETS))
    for (t, era), g in team.groupby(["dest_tier", "era"]):
        summary["team_level"][f"{t}|{era}"] = {
            "teams": int(len(g)), "players_per_team": float(g["n_players"].mean()),
            **{n: float(g[f"sum_{n}"].mean()) for n in SETS}}
        print(f"{t:<9}{era:<9}{len(g):>6}{g['n_players'].mean():>13.1f}" +
              "".join(f"{g[f'sum_{n}'].mean():>+12.2f}" for n in SETS))

    # Where the dest model's lift comes from — gain by feature.
    m = lgb.LGBMRegressor(**lgb_params()).fit(df[SETS["dest"]], df["target_campom"])
    imp = sorted(zip(SETS["dest"], m.booster_.feature_importance("gain")), key=lambda x: -x[1])
    total = sum(v for _, v in imp)
    summary["dest_gain_share"] = {k: float(v / total) for k, v in imp if k in DEST_COLS}
    print("\ngain share of the destination features (all-data fit):",
          {k: f"{v / total:.1%}" for k, v in imp if k in DEST_COLS})

    out = EVAL_DIR / f"trajectory_destination_{dt.date.today():%Y%m%d}_summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
