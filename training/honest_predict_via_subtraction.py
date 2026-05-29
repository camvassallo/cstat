"""
Construct honest predictions by subtracting the lookahead component.

If the model fits: pred = β_pit * pit_diff + β_leak * leak_diff + ...other,
then an "honest" prediction (what the model would output if leak_diff were
unavailable) is approximately:

    honest_pred = pred - β_leak * leak_diff

The honest prediction is shifted toward the underdog when the favorite
improved over the season (the lookahead bonus is removed). We evaluate
honest_pred against actual outcomes — MAE, AUC, ATS — and compare to the
leaky baseline. The delta is the cost of removing leakage.

CAVEAT: this is a linear correction over a tree model. The true honest
model would need to be retrained, not just shifted by a linear coefficient.
But because the LightGBM model's relationship between pred_margin and
CamPom diffs is approximately linear (R² 0.86 in the earlier regression),
this is a reasonable point estimate.
"""

import argparse
from pathlib import Path

import numpy as np
import pandas as pd
from scipy.stats import norm
from sklearn.linear_model import LinearRegression
from sklearn.metrics import roc_auc_score, log_loss, brier_score_loss, mean_absolute_error
from sqlalchemy import text

from db import get_engine


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--leak", type=Path, default=Path(__file__).parent / "models" / "leakage_quantified.csv")
    args = ap.parse_args()

    df = pd.read_csv(args.leak, parse_dates=["game_date"])
    # Bring in pred_home_win_prob from oof_predictions
    oof = pd.read_csv(Path(__file__).parent / "models" / "oof_predictions.csv")
    df = df.merge(
        oof[["game_id", "pred_home_win_prob", "home_team_id", "away_team_id", "actual_home_win"]],
        on="game_id"
    )
    df["actual_home_win"] = (df["actual_margin"] > 0).astype(int)
    print(f"Loaded {len(df)} games with quantified leakage")

    # Re-fit coefficients (cleaner than reusing the script's printed ones)
    X = df[["pit_diff", "leak_diff"]].values
    y = df["pred_margin"].values
    reg = LinearRegression().fit(X, y)
    b_pit, b_leak = reg.coef_
    intercept = reg.intercept_
    print(f"Fitted model:  pred ≈ {intercept:.3f} + {b_pit:+.3f}*pit + {b_leak:+.3f}*leak")

    # Construct honest prediction
    df["honest_margin"] = df["pred_margin"] - b_leak * df["leak_diff"]

    # For win probability: map honest_margin → home_win_prob via a logistic
    # calibrated from the leaky model. Find σ such that pred_margin / σ gives
    # a probit/logistic that matches pred_home_win_prob. Use σ = 11 (CBB
    # standard), then apply same transformation to honest_margin.
    sigma = 11.0
    df["honest_home_win_prob"] = norm.cdf(df["honest_margin"] / sigma)

    # Baseline comparison: leaky vs honest
    print()
    print("=" * 72)
    print(f"  Leaky baseline (production OOF) vs. honest projection")
    print("=" * 72)

    def metrics(label, m_col, p_col):
        mae = mean_absolute_error(df["actual_margin"], df[m_col])
        auc = roc_auc_score(df["actual_home_win"], df[p_col])
        ll = log_loss(df["actual_home_win"], df[p_col].clip(0.001, 0.999))
        brier = brier_score_loss(df["actual_home_win"], df[p_col].clip(0.001, 0.999))
        return label, len(df), mae, auc, ll, brier

    rows = [
        metrics("leaky (production)", "pred_margin", "pred_home_win_prob"),
        metrics("honest (leak subtracted)", "honest_margin", "honest_home_win_prob"),
    ]
    print(f"  {'model':<32}{'n':>7}{'MAE':>8}{'AUC':>8}{'LogLoss':>10}{'Brier':>8}")
    for r in rows:
        print(f"  {r[0]:<32}{r[1]:>7}{r[2]:>8.3f}{r[3]:>8.4f}{r[4]:>10.4f}{r[5]:>8.4f}")

    # Per-season
    print()
    print("  Per-season AUC:")
    print(f"    {'season':<8}{'n':>6}{'leaky AUC':>11}{'honest AUC':>12}{'Δ':>8}")
    for s, sub in df.groupby("season"):
        leaky = roc_auc_score(sub["actual_home_win"], sub["pred_home_win_prob"])
        honest = roc_auc_score(sub["actual_home_win"], sub["honest_home_win_prob"])
        print(f"    {s:<8}{len(sub):>6}{leaky:>11.4f}{honest:>12.4f}{honest-leaky:>+8.4f}")

    # ATS comparison
    print()
    print("  ATS performance on games with Vegas data:")
    engine = get_engine()
    sql = text("""
        SELECT g.id::text AS game_id, gf.spread, gf.home_moneyline, gf.away_moneyline,
               g.home_score, g.away_score
        FROM games g JOIN game_forecasts gf ON gf.game_id = g.id
        WHERE g.id = ANY(CAST(:ids AS uuid[]))
          AND gf.spread IS NOT NULL AND gf.home_moneyline IS NOT NULL
          AND gf.away_moneyline IS NOT NULL
          AND g.home_score IS NOT NULL AND g.away_score IS NOT NULL
    """)
    with engine.connect() as conn:
        v = pd.read_sql(sql, conn, params={"ids": df["game_id"].astype(str).tolist()})
    j = df.merge(v, on="game_id")
    print(f"    {len(j)} games with Vegas data")
    fav_is_home = j["home_moneyline"] < j["away_moneyline"]
    abs_spread = j["spread"].abs()
    actual_fav_margin = np.where(fav_is_home, j["home_score"] - j["away_score"], j["away_score"] - j["home_score"])

    def ats_outcome(margin_col, bucket=None):
        fav_pred = np.where(fav_is_home, j[margin_col], -j[margin_col])
        edge = fav_pred - abs_spread
        bet_fav = edge > 0
        fav_cover = actual_fav_margin > abs_spread
        push = actual_fav_margin == abs_spread
        wins = ((bet_fav & fav_cover) | (~bet_fav & ~fav_cover)) & ~push
        loses = ((bet_fav & ~fav_cover) | (~bet_fav & fav_cover)) & ~push
        n = wins.sum() + loses.sum()
        wp = wins.sum() / n if n else 0
        roi = (wins.sum() * 90.91 - loses.sum() * 100) / (n * 100) * 100 if n else 0
        return wp, roi, edge

    leaky_wp, leaky_roi, leaky_edge = ats_outcome("pred_margin")
    honest_wp, honest_roi, honest_edge = ats_outcome("honest_margin")
    print(f"    {'model':<32}{'win%':>8}{'ROI':>9}")
    print(f"    {'leaky (production)':<32}{leaky_wp*100:>7.2f}%{leaky_roi:>+8.2f}%")
    print(f"    {'honest (leak subtracted)':<32}{honest_wp*100:>7.2f}%{honest_roi:>+8.2f}%")

    # ATS by edge bucket (the leakage signature)
    print()
    print("  ATS by |edge| bucket (leakage signature: monotonic ramp = leaky):")
    bucket_edges = [(0, 1, "|edge|<1"), (1, 3, "|edge|<3"), (3, 5, "|edge|<5"),
                    (5, 8, "|edge|<8"), (8, 1000, "|edge|>=8")]
    print(f"    {'bucket':<12}{'leaky n':>8}{'leaky win%':>12}{'honest n':>10}{'honest win%':>13}")
    for lo, hi, lbl in bucket_edges:
        # Leaky
        mask_leaky = (np.abs(leaky_edge) >= lo) & (np.abs(leaky_edge) < hi)
        sub_l = j[mask_leaky]
        if len(sub_l) > 0:
            fav_pred = np.where(fav_is_home[mask_leaky], sub_l["pred_margin"], -sub_l["pred_margin"])
            actual_fav = actual_fav_margin[mask_leaky]
            abs_sp = abs_spread[mask_leaky]
            edge_l = fav_pred - abs_sp
            bet_fav_l = edge_l > 0
            fav_cover_l = actual_fav > abs_sp
            push_l = actual_fav == abs_sp
            wins_l = ((bet_fav_l & fav_cover_l) | (~bet_fav_l & ~fav_cover_l)) & ~push_l
            loses_l = ((bet_fav_l & ~fav_cover_l) | (~bet_fav_l & fav_cover_l)) & ~push_l
            nl = wins_l.sum() + loses_l.sum()
            wl = wins_l.sum() / nl if nl else 0
        else:
            nl, wl = 0, 0

        mask_honest = (np.abs(honest_edge) >= lo) & (np.abs(honest_edge) < hi)
        sub_h = j[mask_honest]
        if len(sub_h) > 0:
            fav_pred = np.where(fav_is_home[mask_honest], sub_h["honest_margin"], -sub_h["honest_margin"])
            actual_fav = actual_fav_margin[mask_honest]
            abs_sp = abs_spread[mask_honest]
            edge_h = fav_pred - abs_sp
            bet_fav_h = edge_h > 0
            fav_cover_h = actual_fav > abs_sp
            push_h = actual_fav == abs_sp
            wins_h = ((bet_fav_h & fav_cover_h) | (~bet_fav_h & ~fav_cover_h)) & ~push_h
            loses_h = ((bet_fav_h & ~fav_cover_h) | (~bet_fav_h & fav_cover_h)) & ~push_h
            nh = wins_h.sum() + loses_h.sum()
            wh = wins_h.sum() / nh if nh else 0
        else:
            nh, wh = 0, 0

        print(f"    {lbl:<12}{nl:>8}{wl*100:>11.2f}%{nh:>10}{wh*100:>12.2f}%")

    print()
    print("  INTERPRETATION:")
    print("    - 'honest' subtracts the model's lookahead-attributable margin component.")
    print("    - If honest AUC ≈ leaky AUC: leakage doesn't help win prediction much.")
    print("    - If honest |edge|>=8 win% << 95%: yes, leakage is the source of the smoke-test inflation.")


if __name__ == "__main__":
    main()
