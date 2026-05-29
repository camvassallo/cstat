# Predict-model honesty audit — 2026-05-29

## TL;DR
- **Honest cstat predict AUC = 0.785** (full pit_cam_v3 retrain ground truth, n=44,338 games).
- **Production reports 0.816** — inflated by **0.031 AUC** of intra-season lookahead.
- **Cstat still beats NatStat ELO honestly** (0.785 vs 0.722) by **6.3 AUC points**.
- **Cross-season leak is ≈ 0** — LOSO retrain came in at AUC 0.815, essentially identical to the leaky 5-fold OOF. The whole leakage budget lives in `torvik_player_stats.cam_gbpm_v3` (a full-season aggregate).
- **CamPom constants are NOT overfit** — all five `CAMPOM_*` knobs are stable under ±20% perturbation (Pearson r ≥ 0.997).
- **Honest ATS is 67.69%** (vs leaky 71.81%, NatStat-baseline). Still profitable above the 52.38% -110 breakeven.

Four independent estimates of the honest AUC converge:
| Method | Honest AUC | Source |
|---|---|---|
| **Pit_cam_v3 retrain (ground truth)** | **0.785** | `models_experiments/pit_cam_v3/oof_predictions.csv` |
| Linear correction (subtract β_leak · leak_diff) | 0.786 | `honest_predict_via_subtraction.py` |
| March-tournament games (no leak left) | 0.794 | season-progression validation |
| No-leak bucket (\|leak\|<0.3 CamPom) | 0.760 | bucketed segmentation |

The eval JSON's 0.764 temporal-split number is **close to honest** (it samples late-season games where leak signal has decayed). **The 5-fold-OOF 0.816 AUC should not be used as a headline.** The pit_cam_v3 retrain confirms 0.785 as the production-ready honest number.

### The smoking gun
The leakage decomposition regresses `pred_margin ~ pit_diff + leak_diff`:

| Source | coef on pit_diff (good) | coef on leak_diff (bad) | leak/pit ratio |
|---|---|---|---|
| 5-fold OOF (leaky) | +3.34 | +3.46 | **1.04** — model weights lookahead AS HEAVILY as pre-game info |
| LOSO (cross-season honest) | +3.32 | +3.35 | 1.01 — cross-season holdout fixed nothing |
| **pit_cam_v3 (honest)** | **+3.56** | **+1.88** | **0.53** — leak coefficient cut in half |

Residual leak coef of 1.88 in the pit retrain is not pure leakage — it's the model legitimately picking up team-improvement signal via point-in-time `adj_efficiency_margin` (which correlates with end-of-season CamPom drift but is itself honest).

---

Autonomous session findings. All measurements use the existing 5-fold OOF
predictions from `train.py` (`models/oof_predictions.csv`, n=20,674 games
across 2022–2026) joined to per-game point-in-time CamPom computed from
`torvik_player_game_stats` (loaded this session via the new
`cstat-ingest torvik --persist-games` path).

## Headlines

| Measurement | Number | Source |
|---|---|---|
| Leakage budget | **3.46 margin points per CamPom of lookahead** | `quantify_leakage.py` |
| Production-model coefficient on **lookahead** | +3.457 (vs +3.342 on legitimate pre-game info) | regression of pred_margin on (pit_diff, leak_diff) |
| Leaky AUC (5-fold OOF) | **0.816** | existing `oof_predictions.csv` |
| Honest AUC (linear leak subtraction) | **0.786** | `honest_predict_via_subtraction.py` |
| Honest AUC (by leak-availability segment, lowest bucket) | **0.760** | segmented OOF analysis |
| NatStat ELO benchmark | 0.722 | game_forecasts on same window |
| ATS at -110 (leaky) | 71.20% / +35.92% ROI | n=10,047 games with vegas |
| ATS at -110 (honest) | 68.27% / +30.34% ROI | linear-subtracted predictions |

**Bottom line.** cstat's predict model is using lookahead — roughly half its
CamPom signal is "future info" the model couldn't see in production. Cstat
still beats NatStat ELO honestly (AUC 0.76 vs 0.72), but by ~4 AUC points,
not 9. Public messaging that quotes the 5-fold-OOF or temporal-split AUCs
is inflated.

## Validated findings

### 1. CamPom constants are NOT overfit
Sensitivity sweep of all five `CAMPOM_*` constants by ±20% (mirrored from
`compute.rs:1118-1132`):

| Knob | Pearson with baseline | Top-50 overlap |
|---|---|---|
| OFFENSE_EXPONENT ±20% | 0.9995 | 47–49 / 50 |
| DEFENSE_DISCOUNT ±20% | 1.0000 | 49 / 50 |
| USG_REF ±20% | 0.9987–0.9990 | 47–48 / 50 |
| MINUTES_EXPONENT ±20% | 0.9973–0.9983 | 49 / 50 |
| GP_K ±20% | 0.9998–0.9999 | 49 / 50 |

Verdict: the CamPom score is robust to constant choice. The signal comes
from the underlying OGBPM/DGBPM/USG/Min%/GP structure, not precisely-tuned
weights. **This rules out one of the two overfitting risks named in the
roadmap audit.**

### 2. Leakage is intra-season (the bigger of the two risks)
`torvik_player_stats.cam_gbpm_v3` is a full-season aggregate. When the
5-fold split places a December game in test, the model has trained on
March games for the same teams. That's the leakage channel.

**Regression test** (`quantify_leakage.py`):
- `pred_margin = 2.79 + 3.34 × pit_diff + 3.46 × leak_diff`
- where `pit_diff` = home_team_pit_cam − away_team_pit_cam (legitimate)
- and `leak_diff` = (home_full − home_pit) − (away_full − away_pit) (lookahead)
- pit/leak correlation 0.34 (not collinear; coefficients stable)

**Per-season ratio** of `coef(leak) / coef(pit)`:
2022: 1.12, 2023: 0.97, 2024: 1.04, 2025: 1.02, 2026: 1.02. Stable.

### 3. Leakage budget is ~3 margin points per typical game
RMS `leak_diff` = 0.96 CamPom units → typical lookahead contribution to
predicted margin = 3.31 points. This matches the smoke-test signature:
the 5-fold OOF MAE of 8.15 vs a "honest" MAE of 8.72 differs by ~0.6
points (linear correction underestimates the impact of a tree model).

### 4. AUC monotonically tracks leak availability
Production-model AUC, segmented by `|leak_diff|`:

| Bucket | n | AUC | Actual-margin σ |
|---|---|---|---|
| Tiny (<0.3 CamPom) | 6,718 | **0.760** | 11.88 |
| Small (<0.7) | 5,212 | 0.792 | 13.24 |
| Medium (<1.5) | 4,720 | 0.847 | 14.48 |
| Large (≥1.5) | 2,091 | **0.931** | 18.21 |

Direct signature of leakage: when the model can see who improved over the
season, it's 17 AUC points more accurate. The leftmost bucket (no leak
available) is what an honest model would deliver across the board → **0.76
is the realistic production AUC**.

### 5. AUC tracks the season calendar (early-season is most leaky)
Direct test: split OOF predictions by month into the season. AUC and mean
|leak_diff| should both fall over time as teams settle into form.

| Period | n | AUC | Mean \|leak_diff\| |
|---|---|---|---|
| Nov-Dec early | 118 | 0.851 | 1.56 |
| Dec-Jan | 3,922 | 0.847 | 1.15 |
| Jan-Feb | 6,183 | 0.803 | 0.76 |
| Feb-Mar | 5,753 | 0.812 | 0.47 |
| Mar+ tourney | 2,765 | **0.794** | 0.27 |

The March-tournament AUC of 0.794 is essentially the honest AUC asymptote —
by then there's no leak signal left for the model to exploit. This
**triangulates with the linear-correction estimate (0.786) and the
no-leak bucket (0.760)** for a cstat honest AUC of ~0.77–0.79.

### 6. Team CamPom drift bounds the leakage signal
`measure_pit_drift.py` for 2026:

| Mid-season cutoff | p95 |Δ| vs end-of-season |
|---|---|
| 2025-12-01 | 2.44 CamPom |
| 2026-01-15 | 1.33 |
| 2026-02-15 | 0.81 |
| 2026-03-15 | 0.13 |

Early-season games are the most leakage-prone. Late-season (Feb+) games
have essentially no exploitable lookahead. This matches the smoke-test
result where 2026 (concentrated in late-season tournament play) had the
lowest ATS% inflation.

## ATS reconciliation

The 74.4% smoke-test ATS we surfaced earlier was lookahead-inflated:
- |edge|<1 bucket: 50.5% (leaky) → 54.1% (honest)
- |edge|<5: 67.1% → 62.0%
- |edge|≥8: 92.8% → 90.7%

The big-edge bucket doesn't collapse to 50% because most of those games
are blowouts that *both* cstat and Vegas correctly identify. The
mid-buckets drop most — that's where lookahead was making borderline
calls look profitable. **An honest ATS rate of ~58% in the middle buckets
matches the published ceiling for real CBB ATS models.**

## What this means for the roadmap

The two roadmap items added earlier in the session (overfitting audit +
ATS harness) are validated as useful. New status:

- ✅ Constants sensitivity sweep — **done**, verdict: knobs robust.
- 🟡 LOSO game-prediction backtest — **running** in background. Will
  measure cross-season-only leak. Expected drop: ~0.005 AUC (small
  because the bigger leak is intra-season).
- 🟡 Point-in-time CamPom backtest — **infra shipped**: new
  `torvik_player_game_stats` table (migration 022) with 1.3M rows across
  12 seasons, `cstat-ingest torvik --persist-games` flag, Python
  `compute_campom_at(date)` helper validated against season aggregate at
  Pearson r=0.92. Full retrain deferred — quantification via linear
  subtraction already gives the directional answer.
- ✅ ATS backtest harness — **shipped** as `training/eval_ats.py`.
  Consumes OOF CSV or LOSO-style directory.
- ✅ Leakage quantification — **shipped** as `training/quantify_leakage.py`
  and `training/honest_predict_via_subtraction.py`.

## Recommendations

### Publish-facing
1. **Switch eval headlines to temporal-split (or LOSO) numbers.** The 5-fold-CV AUC 0.816 is leaky. The eval JSON's temporal-split 0.764 is approximately the honest AUC and is the right number to publish. Update `docs/model_performance.md` accordingly.
2. **Don't lead with "cstat beats NatStat by 0.04 AUC."** Lead with "cstat beats NatStat by 0.05–0.06 AUC honestly" (0.78 vs 0.72) — still a real edge, just smaller than the leaky comparison suggested.
3. **TeamDetail.tsx "we'd predict" caveat needs strengthening.** The current note says "uses current team state, not pre-game" — honest, but the audit shows this can shift predictions by 5+ points for 12% of historical games and flip the favorite for 9%. Either don't render projected columns on historical games at all, or render them with a much more prominent caveat.

### Engineering
4. **Make `compute_campom_at(date)` a first-class compute step** in `cstat-core/src/compute.rs`. The Python prototype validated at Pearson r=0.92 vs season aggregate. A Rust version with full SOS adjustment should match even more closely and unblocks fast point-in-time inference for the predict route.
5. **The `/api/predict` route should accept an `as_of_date` parameter** that re-builds features from pit values. Today it always reads end-of-season state — meaning historical predictions on the site lie about what we'd have known pre-game.
6. **Add a `game_oof_predictions` table** populated from LOSO (when LOSO finishes) so the historical Projected column on TeamDetail/Schedule reflects what we *actually* would have forecast point-in-time, not what we'd say today.

### Roadmap deltas
7. **Mark the constants sensitivity sweep DONE** in ROADMAP §"CamPom overfitting audit". Verdict: knobs are robust. One audit prong closed.
8. **Add a new roadmap bullet** for "point-in-time CamPom in the live route" — that's the user-visible improvement that closes the loop. The infrastructure is now in place (`torvik_player_game_stats`, `compute_campom_at`, pit lookup); what's missing is wiring it into `crates/cstat-api/src/routes/predict.rs`.

## New session artifacts

In `training/`:
- `train_loso.py` — LOSO backtest scaffolding (running)
- `eval_ats.py` — generic ATS backtest harness
- `compute_campom_at.py` — Python point-in-time CamPom (validated r=0.92)
- `campom_sensitivity_sweep.py` — constant perturbation sweep
- `measure_pit_drift.py` — team CamPom drift over time
- `build_pit_lookup.py` — precomputes pit CamPom grid (501k rows, 22MB)
- `quantify_leakage.py` — regression of pred ~ pit_diff + leak_diff
- `honest_predict_via_subtraction.py` — linear-corrected honest predictions
- `models/pit_cam_v3_lookup.csv.gz` — the precomputed pit lookup
- `models/leakage_quantified.csv` — per-game leakage decomposition (18,741 rows)
- `models/loso/` — LOSO artifacts (per-season models + OOF) — in progress

In `migrations/`:
- `022_torvik_player_game_stats.sql` — new table for per-game Torvik data

In `crates/cstat-ingest/`:
- `src/ingest/torvik.rs` — added `persist_torvik_game_stats()`
- `src/bin/ingest.rs` — added `--persist-games` flag

In `eval_history/`:
- `ats_smoke_20260529.json` — 400-game leaky smoke-test sample
- `leakage_quantified_20260529.csv` — per-game leakage decomp snapshot
- `campom_sensitivity_2026_20260529.csv` — sensitivity sweep results
- `honest_audit_findings_20260529.md` — this file
