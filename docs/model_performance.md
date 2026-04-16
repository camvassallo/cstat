# Model Performance Report

Last updated: 2026-04-15
Training data: 2025 + 2026 seasons (4,107 games with complete features after NaN filtering)

## Overview

cstat uses two LightGBM models for game prediction:

- **Margin model** (regression): predicts the home team's scoring margin
- **Win model** (classification): predicts home team win probability

Both models use 49 point-in-time diff-features (home minus away) covering team efficiency, roster composition, Barttorvik GBPM, rolling form, and game context. All features are computed using only data available before each game — no data leakage.

---

## Backtest Results

Chronological 80/20 split: train on first 3,285 games (2025-11-18 to 2026-02-21), test on last 822 games (2026-02-21 to 2026-04-06).

### Margin Model

| Metric | Value |
|--------|-------|
| MAE | 8.68 pts |
| RMSE | 11.16 pts |
| R² | 0.300 |
| Win accuracy (from sign) | 70.0% |

### Win Probability Model

| Metric | Value |
|--------|-------|
| Accuracy | 70.0% |
| AUC | 0.764 |
| Log loss | 0.558 |

### 5-Fold Cross-Validation

| Metric | Value |
|--------|-------|
| Margin MAE | 8.81 +/- 0.34 |
| Margin Acc | 73.2% +/- 1.2% |
| Win Acc | 71.6% +/- 1.3% |
| Win AUC | 0.785 +/- 0.013 |

### Training Details

| Parameter | Margin | Win |
|-----------|--------|-----|
| Best iteration | 52 | 127 |
| Num leaves | 24 | 24 |
| Learning rate | 0.03 | 0.03 |
| Feature fraction | 0.7 | 0.7 |
| Early stopping | 80 rounds | 80 rounds |

---

## Benchmark vs NatStat ELO

Compared on the same 1,590 test games where both cstat and NatStat forecasts are available.

### Head-to-Head

| Metric | cstat | NatStat | Delta | Winner |
|--------|-------|---------|-------|--------|
| Win Accuracy | 69.4% | 67.3% | +2.1pp | cstat |
| AUC | 0.738 | 0.724 | +0.014 | cstat |
| Log Loss | 0.578 | 0.595 | -0.017 | cstat |
| Calibration ECE | 0.022 | 0.061 | 3x better | cstat |

cstat wins every metric. NatStat uses a pure ELO model; cstat combines adjusted efficiency, roster composition, four factors, rolling form, and ELO.

### Calibration Comparison

| Predicted Prob | N | cstat Pred | NatStat Pred | Actual Win% |
|---------------|---|-----------|-------------|-------------|
| 0-10% | 32 | 0.112 | 0.074 | 0.125 |
| 10-20% | 64 | 0.203 | 0.165 | 0.203 |
| 20-30% | 94 | 0.286 | 0.264 | 0.298 |
| 30-40% | 94 | 0.383 | 0.365 | 0.362 |
| 40-50% | 154 | 0.479 | 0.463 | 0.461 |
| 50-60% | 248 | 0.569 | 0.554 | 0.560 |
| 60-70% | 309 | 0.657 | 0.656 | 0.641 |
| 70-80% | 227 | 0.744 | 0.752 | 0.696 |
| 80-90% | 218 | 0.838 | 0.849 | 0.798 |
| 90-100% | 150 | 0.934 | 0.942 | 0.947 |

Both models are well-calibrated in the 0-60% range. NatStat overestimates favorites in the 70-90% range (predicts 0.75/0.85, actual 0.70/0.80). cstat is slightly closer to actual across those bins.

### Segment Breakdown

| Segment | N | cstat Acc | NatStat Acc | Winner |
|---------|---|-----------|-------------|--------|
| Confident (>65%) | 974 | 75.8% | 75.2% | cstat |
| Lean (55-65%) | 411 | 60.8% | 57.4% | cstat |
| Toss-up (<55%) | 205 | 56.6% | 49.8% | cstat |
| Blowout margin (>15) | 379 | 87.3% | 83.6% | cstat |
| Moderate margin (6-15) | 708 | 70.8% | 68.1% | cstat |
| Close margin (<=5) | 503 | 54.1% | 53.9% | cstat |

cstat wins every segment. The largest edge is in toss-up games (+6.8pp) and lean picks (+3.4pp) — exactly where a richer feature set helps most.

### Disagreement Analysis

- Models disagree on the winner: 174 games (10.9% of test set)
- When they disagree, cstat correct: 104 (59.8%)
- When they disagree, NatStat correct: 70 (40.2%)

---

## Segment Performance

### By Venue

| Segment | N | MAE | Win Acc | AUC |
|---------|---|-----|---------|-----|
| Home games | 1,231 | 9.06 | 69.0% | 0.745 |
| Neutral site | 343 | 8.72 | 67.1% | 0.700 |

### By Matchup Type

| Segment | N | MAE | Win Acc | AUC |
|---------|---|-----|---------|-----|
| Conference | 1,436 | 8.98 | 68.2% | 0.727 |
| Non-conference | 138 | 9.08 | 71.7% | 0.838 |

Non-conference games have higher AUC despite similar MAE, likely because talent gaps between conferences are easier to capture.

### By Predicted Spread

| Segment | N | MAE | Win Acc | AUC |
|---------|---|-----|---------|-----|
| Big favorite (10+ pts) | 273 | 9.42 | 89.0% | 0.904 |
| Moderate (5-10 pts) | 489 | 8.59 | 73.8% | 0.745 |
| Lean (<5 pts) | 812 | 9.08 | 58.5% | 0.599 |

### By Actual Closeness

| Segment | N | MAE | Win Acc | AUC |
|---------|---|-----|---------|-----|
| Blowout (15+ pts) | 409 | 15.88 | 82.9% | 0.889 |
| Moderate (6-14 pts) | 664 | 6.68 | 70.9% | — |
| Close (<=5 pts) | 501 | 5.33 | 55.7% | 0.590 |

Close games approach coin-flip accuracy — expected, as outcomes in tight games are driven by in-game variance (free throws, turnovers, last-second shots) that no pre-game model can predict.

### By Season Timing

| Segment | N | MAE | Win Acc | AUC |
|---------|---|-----|---------|-----|
| First half of test set | 820 | 8.76 | 69.0% | 0.739 |
| Second half of test set | 754 | 9.23 | 68.0% | 0.734 |

Performance is stable across the test window with a slight MAE increase late, likely from tournament volatility.

---

## Feature Importance

Top 15 features by LightGBM split importance (combined margin + win models):

| Rank | Feature | Splits | Description |
|------|---------|--------|-------------|
| 1 | diff_w_gbpm | 227 | Minutes-weighted Barttorvik GBPM |
| 2 | diff_adj_efficiency_margin | 60 | KenPom-style adjusted net efficiency |
| 3 | diff_elo | 40 | NatStat pre-game ELO rating |
| 4 | diff_win_pct | 39 | Overall win percentage |
| 5 | diff_star_gbpm | 38 | Star player's Barttorvik GBPM |
| 6 | diff_ft_rate | 36 | Free throw rate |
| 7 | diff_def_rebound_pct | 36 | Defensive rebounding rate |
| 8 | diff_w_tov_pct | 35 | Minutes-weighted turnover rate |
| 9 | diff_w_dbpm | 34 | Minutes-weighted defensive BPM |
| 10 | diff_minutes_stddev | 34 | Minutes distribution (depth proxy) |
| 11 | diff_star_ppg | 33 | Star player's PPG |
| 12 | diff_opp_effective_fg_pct | 32 | Opponent effective FG% allowed |
| 13 | diff_w_rpg | 28 | Minutes-weighted rebounds per game |
| 14 | diff_adj_defense | 27 | Adjusted defensive efficiency |
| 15 | diff_w_usage | 24 | Minutes-weighted usage rate |

Barttorvik GBPM dominates (nearly 4x the next feature). GBPM is a possession-adjusted plus/minus metric that captures player impact beyond what box-score BPM can measure. The model also draws signal from adjusted efficiency, ELO, four factors, and roster composition.

---

## Known Limitations

- **Two seasons of training data**: Early stopping at 52/127 iterations suggests the model is still data-starved. NatStat has data back to 2007; ingesting even 3-4 more seasons should meaningfully improve generalization.
- **No roster availability**: The model doesn't know who actually played in each game. A team missing its best player looks identical to a full-strength team.
- **No lineup data**: Can't model specific 5-man combinations or substitution patterns.
- **Close game ceiling**: ~55% accuracy on games decided by 5 or fewer points. This is near the theoretical ceiling for pre-game models — in-game variance dominates.
- **Player rate stats are approximations**: AST%, ORB%, etc. use per-40-minute proxies rather than true possession-based formulas.

---

## Context: How Does This Compare?

For reference, public college basketball models typically achieve:

| Model | Win Accuracy | Notes |
|-------|-------------|-------|
| Home team always wins | ~58% | Naive baseline |
| AP/Coaches poll ranking | ~65% | Higher-ranked team wins |
| Basic ELO | ~67% | Where NatStat sits |
| KenPom/Barttorvik | ~70-72% | Full-season adjusted efficiency |
| **cstat** | **70.0%** | 2 seasons, 49 features (incl. Torvik GBPM), PIT |
| Vegas closing lines | ~73-74% | Incorporates injury reports, betting market info |

cstat is competitive with established systems despite training on only 2 seasons and lacking injury/lineup data. Adding Barttorvik GBPM brought cstat from 68.6% to 70.0% — closing the gap with KenPom/Barttorvik-tier models. The main paths to further improvement are more training data and recruiting rank as an early-season prior.
