"""
ATS backtest harness for cstat predictions.

Loads a (game_id, pred_margin[, pred_home_win_prob]) source — either a
single OOF CSV from train.py / train_loso.py, or a directory of LOSO OOFs —
joins to `game_forecasts` for vegas spread + moneyline, computes ATS
win/loss/push + ROI at -110 vig, edge-bucketed report, and a per-season
breakdown.

Usage:
  python eval_ats.py --oof models/oof_predictions.csv
  python eval_ats.py --oof-dir models/loso         # joins all predict_{year}/oof_predictions.csv
  python eval_ats.py --oof models/loso/predict_2024/oof_predictions.csv --label "LOSO 2024"

Output: prints summary + per-season + edge-bucket tables; writes a
detailed JSONL of per-game decisions to <oof-stem>_ats.jsonl alongside the
input, plus an _ats_summary.json with the headline numbers.

Honest-vs-leaky semantics:
  - LOSO OOF predictions = honest (no cross-season leakage).
  - train.py OOF = honest within-season (5-fold random split — not
    chronological but still leak-free for the predicted game).
  - /api/predict from a running server = leaky (uses end-of-season state).

The harness doesn't care which it is; it just scores whatever you give it.
"""

import argparse
import json
import math
import os
from pathlib import Path

import pandas as pd
from sqlalchemy import text

from db import get_engine


def load_oof_single(path: Path) -> pd.DataFrame:
    df = pd.read_csv(path)
    if "pred_margin" not in df.columns:
        raise ValueError(f"{path}: missing pred_margin column")
    if "game_id" not in df.columns:
        raise ValueError(f"{path}: missing game_id column")
    return df


def load_oof_dir(root: Path) -> pd.DataFrame:
    files = sorted(root.glob("predict_*/oof_predictions.csv"))
    if not files:
        raise FileNotFoundError(f"no predict_*/oof_predictions.csv under {root}")
    frames = []
    for p in files:
        d = load_oof_single(p)
        d["_source"] = p.parent.name
        frames.append(d)
    return pd.concat(frames, ignore_index=True)


def load_vegas(engine, game_ids) -> pd.DataFrame:
    sql = text("""
        SELECT g.id AS game_id,
               g.season,
               g.game_date,
               g.home_team_id,
               g.away_team_id,
               g.home_score,
               g.away_score,
               g.is_neutral_site,
               gf.spread,
               gf.home_moneyline,
               gf.away_moneyline,
               gf.home_win_exp / 100.0 AS natstat_home_win_exp,
               ht.name AS home_name,
               at.name AS away_name
        FROM games g
        JOIN game_forecasts gf ON gf.game_id = g.id
        JOIN teams ht ON ht.id = g.home_team_id
        JOIN teams at ON at.id = g.away_team_id
        WHERE g.id = ANY(CAST(:ids AS uuid[]))
          AND g.home_score IS NOT NULL
          AND g.away_score IS NOT NULL
          AND gf.spread IS NOT NULL
          AND gf.home_moneyline IS NOT NULL
          AND gf.away_moneyline IS NOT NULL
    """)
    with engine.connect() as conn:
        return pd.read_sql(sql, conn, params={"ids": list(game_ids)})


def fav_implied_prob_from_ml(fav_ml: int, dog_ml: int) -> float:
    p_fav_raw = abs(fav_ml) / (abs(fav_ml) + 100.0)
    p_dog_raw = 100.0 / (dog_ml + 100.0)
    return p_fav_raw / (p_fav_raw + p_dog_raw)


def score(df: pd.DataFrame) -> pd.DataFrame:
    """Add fav-perspective columns + ATS outcome per row."""
    # Favorite is whichever team has the more-negative moneyline.
    fav_is_home = df["home_moneyline"] < df["away_moneyline"]
    df = df.copy()
    df["fav_is_home"] = fav_is_home
    df["abs_spread"] = df["spread"].abs()

    actual_home_margin = df["home_score"] - df["away_score"]
    df["actual_fav_margin"] = actual_home_margin.where(fav_is_home, -actual_home_margin)
    df["cstat_fav_margin"] = df["pred_margin"].where(fav_is_home, -df["pred_margin"])

    df["edge"] = df["cstat_fav_margin"] - df["abs_spread"]
    df["bet_side"] = df["edge"].apply(lambda e: "fav" if e > 0 else "dog")

    fav_covered = df["actual_fav_margin"] > df["abs_spread"]
    push = df["actual_fav_margin"] == df["abs_spread"]
    df["fav_covered"] = fav_covered
    df["push"] = push
    df["outcome"] = "lose"
    df.loc[push, "outcome"] = "push"
    won = ((df["bet_side"] == "fav") & fav_covered) | (
        (df["bet_side"] == "dog") & ~fav_covered
    )
    df.loc[won & ~push, "outcome"] = "win"

    # Win-prob comparison if we have one
    if "pred_home_win_prob" in df.columns:
        ml_implied = df.apply(
            lambda r: fav_implied_prob_from_ml(
                r["home_moneyline"] if r["fav_is_home"] else r["away_moneyline"],
                r["away_moneyline"] if r["fav_is_home"] else r["home_moneyline"],
            ),
            axis=1,
        )
        df["vegas_fav_prob"] = ml_implied
        df["cstat_fav_prob"] = df["pred_home_win_prob"].where(
            fav_is_home, 1.0 - df["pred_home_win_prob"]
        )
        df["prob_edge"] = df["cstat_fav_prob"] - df["vegas_fav_prob"]

    return df


def roi(wins: int, loses: int) -> float:
    n = wins + loses
    if n == 0:
        return 0.0
    return (wins * 90.91 - loses * 100.0) / (n * 100.0) * 100.0


def edge_bucket(e: float) -> str:
    a = abs(e)
    if a < 1:
        return "|edge|<1"
    if a < 3:
        return "|edge|<3"
    if a < 5:
        return "|edge|<5"
    if a < 8:
        return "|edge|<8"
    return "|edge|>=8"


BUCKET_ORDER = ["|edge|<1", "|edge|<3", "|edge|<5", "|edge|<8", "|edge|>=8"]


def summarize(df: pd.DataFrame, label: str):
    wins = int((df["outcome"] == "win").sum())
    loses = int((df["outcome"] == "lose").sum())
    pushes = int((df["outcome"] == "push").sum())
    n = wins + loses
    wp = 100 * wins / n if n else 0
    print()
    print("=" * 72)
    print(f"  {label}   (n={len(df)}, decided={n}, pushes={pushes})")
    print("=" * 72)
    print(f"  Win {wins} / Lose {loses} / Push {pushes}")
    print(f"  ATS win %: {wp:.2f}%   ROI @ -110: {roi(wins, loses):+.2f}%")

    # By edge bucket
    print()
    print(f"  by |edge| bucket")
    print(f"    {'bucket':<12}{'n':>6}{'win%':>9}{'roi%':>9}{'avg_edge':>11}")
    df = df.assign(_b=df["edge"].apply(edge_bucket))
    for b in BUCKET_ORDER:
        sub = df[(df["_b"] == b) & (df["outcome"] != "push")]
        n_b = len(sub)
        w_b = int((sub["outcome"] == "win").sum())
        l_b = n_b - w_b
        wp_b = 100 * w_b / n_b if n_b else 0
        avg_e = sub["edge"].mean() if n_b else float("nan")
        print(f"    {b:<12}{n_b:>6}{wp_b:>8.2f}%{roi(w_b, l_b):>+8.2f}%{avg_e:>+11.2f}")

    # By season
    print()
    print(f"  by season")
    print(f"    {'season':<8}{'n':>6}{'win%':>9}{'roi%':>9}{'mae':>8}{'auc?':>8}")
    for season, sub in df.groupby("season"):
        decided = sub[sub["outcome"] != "push"]
        w = int((decided["outcome"] == "win").sum())
        l = len(decided) - w
        wp = 100 * w / len(decided) if len(decided) else 0
        mae = (sub["pred_margin"] - (sub["home_score"] - sub["away_score"])).abs().mean()
        if "pred_home_win_prob" in sub.columns:
            actual = (sub["home_score"] > sub["away_score"]).astype(int)
            try:
                from sklearn.metrics import roc_auc_score
                auc = roc_auc_score(actual, sub["pred_home_win_prob"])
                auc_s = f"{auc:.3f}"
            except Exception:
                auc_s = "  --"
        else:
            auc_s = "  --"
        print(f"    {season:<8}{len(sub):>6}{wp:>8.2f}%{roi(w, l):>+8.2f}%{mae:>8.2f}{auc_s:>8}")

    return {
        "label": label,
        "n_games": int(len(df)),
        "wins": wins,
        "loses": loses,
        "pushes": pushes,
        "win_pct": wp,
        "roi_pct": roi(wins, loses),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--oof", type=Path, help="single OOF CSV")
    ap.add_argument("--oof-dir", type=Path, help="directory with predict_*/oof_predictions.csv")
    ap.add_argument("--label", type=str, default="cstat", help="run label for headers")
    ap.add_argument("--output-dir", type=Path, default=None,
                    help="where to write _ats.jsonl + _ats_summary.json (default: alongside input)")
    args = ap.parse_args()

    if not args.oof and not args.oof_dir:
        ap.error("one of --oof / --oof-dir is required")
    if args.oof and args.oof_dir:
        ap.error("pick one of --oof / --oof-dir")

    if args.oof:
        oof = load_oof_single(args.oof)
        out_root = args.output_dir or args.oof.parent
        stem = args.oof.stem
    else:
        oof = load_oof_dir(args.oof_dir)
        out_root = args.output_dir or args.oof_dir
        stem = args.oof_dir.name + "_pooled"

    print(f"Loaded {len(oof)} predictions ({args.label})")

    engine = get_engine()
    print("Joining to game_forecasts (vegas spread + ML required)...")
    oof["game_id"] = oof["game_id"].astype(str)
    vegas = load_vegas(engine, oof["game_id"].unique().tolist())
    vegas["game_id"] = vegas["game_id"].astype(str)
    print(f"  {len(vegas)} games with full vegas data")

    df = oof.merge(vegas, on="game_id", how="inner", suffixes=("", "_v"))
    print(f"  {len(df)} rows after join")

    if len(df) == 0:
        print("No overlap between predictions and vegas data — exiting.")
        return

    scored = score(df)

    summary = summarize(scored, args.label)

    # Persist
    out_root.mkdir(parents=True, exist_ok=True)
    jsonl_path = out_root / f"{stem}_ats.jsonl"
    summary_path = out_root / f"{stem}_ats_summary.json"

    keep_cols = [
        "game_id", "season", "game_date", "home_name", "away_name",
        "home_score", "away_score", "spread", "home_moneyline", "away_moneyline",
        "fav_is_home", "abs_spread",
        "pred_margin", "cstat_fav_margin", "actual_fav_margin",
        "bet_side", "outcome", "edge",
    ]
    if "pred_home_win_prob" in scored.columns:
        keep_cols += ["pred_home_win_prob", "vegas_fav_prob", "cstat_fav_prob", "prob_edge"]
    if "natstat_home_win_exp" in scored.columns:
        keep_cols += ["natstat_home_win_exp"]

    # Drop any cols not present (e.g. if a column was added/removed in oof)
    keep_cols = [c for c in keep_cols if c in scored.columns]
    scored[keep_cols].to_json(jsonl_path, orient="records", lines=True, date_format="iso")
    with open(summary_path, "w") as f:
        json.dump({"label": args.label, **summary, "n_rows_in": len(df)}, f, indent=2, default=str)
    print(f"\n  Wrote {jsonl_path}")
    print(f"  Wrote {summary_path}")


if __name__ == "__main__":
    main()
