"""
Post-retrain validation: compare leaky baseline vs honest models.

Runs after train_loso.py (LOSO OOF in models/loso/) and the pit_cam_v3
retrain (OOF in models_experiments/pit_cam_v3/) have finished. Produces:
  - Three-way ATS comparison (leaky / LOSO / pit)
  - quantify_leakage.py on each (coefficient on leak_diff should drop)
  - Per-season AUC + MAE table

Output: training/eval_history/post_audit_validation_<date>.json + .md
"""

import json
import subprocess
from datetime import datetime
from pathlib import Path

import pandas as pd
from sklearn.metrics import roc_auc_score, mean_absolute_error

ROOT = Path(__file__).parent
MODELS = ROOT / "models"
LOSO_DIR = MODELS / "loso"
PIT_DIR = ROOT / "models_experiments" / "pit_cam_v3"


def load_oof(path: Path) -> pd.DataFrame:
    df = pd.read_csv(path, parse_dates=["game_date"])
    # Standardize column name for win prob
    if "pred_home_win_prob" not in df.columns:
        if "pred_win_prob" in df.columns:
            df = df.rename(columns={"pred_win_prob": "pred_home_win_prob"})
    if "actual_home_win" not in df.columns and "actual_margin" in df.columns:
        df["actual_home_win"] = (df["actual_margin"] > 0).astype(int)
    return df


def summarize(name: str, df: pd.DataFrame) -> dict:
    if "pred_home_win_prob" not in df.columns:
        return {"name": name, "error": "no pred_home_win_prob"}
    sub = df.dropna(subset=["pred_margin", "pred_home_win_prob", "actual_margin", "actual_home_win"])
    return {
        "name": name,
        "n": int(len(sub)),
        "auc": float(roc_auc_score(sub["actual_home_win"], sub["pred_home_win_prob"])),
        "mae": float(mean_absolute_error(sub["actual_margin"], sub["pred_margin"])),
        "per_season_auc": {
            int(s): float(roc_auc_score(g["actual_home_win"], g["pred_home_win_prob"]))
            for s, g in sub.groupby("season") if len(g) > 50
        },
    }


def main():
    print("=" * 78)
    print("Post-retrain validation")
    print("=" * 78)

    sources = []
    if (MODELS / "oof_predictions.csv").exists():
        sources.append(("leaky-5fold", load_oof(MODELS / "oof_predictions.csv")))
    if LOSO_DIR.exists():
        loso_files = sorted(LOSO_DIR.glob("predict_*/oof_predictions.csv"))
        if loso_files:
            loso = pd.concat([load_oof(p) for p in loso_files], ignore_index=True)
            sources.append(("loso-cross-season-honest", loso))
    if (PIT_DIR / "oof_predictions.csv").exists():
        sources.append(("pit-cam-v3-honest", load_oof(PIT_DIR / "oof_predictions.csv")))

    if not sources:
        print("No OOF sources found — nothing to compare.")
        return

    print(f"Comparing {len(sources)} sources: {[s[0] for s in sources]}")
    print()

    summary = {"timestamp": datetime.now().isoformat(timespec="seconds"),
               "sources": []}
    print(f"  {'source':<30}{'n':>8}{'AUC':>8}{'MAE':>8}")
    for name, df in sources:
        s = summarize(name, df)
        summary["sources"].append(s)
        if "error" not in s:
            print(f"  {name:<30}{s['n']:>8}{s['auc']:>8.4f}{s['mae']:>8.2f}")

    print()
    print(f"  {'source':<30}", end="")
    seasons = sorted({s for src_summary in summary["sources"] if "per_season_auc" in src_summary for s in src_summary["per_season_auc"].keys()})
    for s in seasons:
        print(f"{s:>8}", end="")
    print()
    for s in summary["sources"]:
        if "per_season_auc" not in s:
            continue
        print(f"  {s['name']:<30}", end="")
        for sea in seasons:
            v = s["per_season_auc"].get(sea, float("nan"))
            print(f"{v:>8.4f}", end="")
        print()

    # Leakage decomposition on each source. The KEY validation:
    # for the pit retrain, coef on leak_diff should drop to ~0 (the
    # model can't use lookahead because it's not in features anymore).
    print()
    print("=" * 78)
    print("Leakage decomposition per source (coef on leak_diff is the smoking gun)")
    print("=" * 78)
    leak_results = {}
    for name, df in sources:
        # Need pred_margin, season, game_date, home_team_id, away_team_id
        required = {"pred_margin", "season", "game_date", "home_team_id", "away_team_id"}
        if not required.issubset(df.columns):
            print(f"  {name:<30}  missing cols, skipping")
            continue
        # Reuse quantify_leakage logic via subprocess: write tmp OOF + call it
        tmp = ROOT / f".tmp_oof_{name}.csv"
        df.to_csv(tmp, index=False)
        cmd = ["python3", str(ROOT / "quantify_leakage.py"), "--oof", str(tmp), "--seasons", "2022,2023,2024,2025,2026"]
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
            tmp.unlink(missing_ok=True)
            tail = out.stdout.splitlines()
            coefs = [l for l in tail if "coef on" in l]
            print(f"  --- {name} ---")
            for l in coefs[:4]:
                print(f"    {l.strip()}")
            leak_results[name] = "\n".join(coefs)
        except Exception as e:
            tmp.unlink(missing_ok=True)
            print(f"  {name}: error {e}")

    # ATS pass through eval_ats for each
    print()
    print("=" * 78)
    print("ATS analysis per source")
    print("=" * 78)
    for name, df in sources:
        if name == "leaky-5fold":
            oof_path = MODELS / "oof_predictions.csv"
        elif name == "loso-cross-season-honest":
            oof_path = LOSO_DIR
        else:
            oof_path = PIT_DIR / "oof_predictions.csv"
        arg = "--oof-dir" if oof_path.is_dir() else "--oof"
        cmd = ["python3", str(ROOT / "eval_ats.py"), arg, str(oof_path), "--label", name]
        print(f"\n--- {name} ---")
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
            print(out.stdout[-2000:])
            if out.returncode != 0:
                print("STDERR:", out.stderr[-500:])
        except subprocess.TimeoutExpired:
            print(f"  ATS run timed out for {name}")

    # Persist
    out_dir = ROOT / "eval_history"
    out_dir.mkdir(exist_ok=True)
    out_path = out_dir / f"post_audit_validation_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    out_path.write_text(json.dumps(summary, indent=2, default=str))
    print(f"\n  Wrote {out_path}")


if __name__ == "__main__":
    main()
