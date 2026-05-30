# Preseason × pit predict blend — v1 shipped + validation

**Date:** 2026-05-30
**ROADMAP:** §6 "Preseason × pit blend" → "(3) In-season blend"

## What shipped

Output-blend + date-scheduled decay for the honest (`as_of_date`-set)
predict path:

- `migrations/023_team_preseason_projection.sql` + `cstat-ingest
  compute-projections --year` — materializes each team's served projection
  AdjEM via the shared `cstat_core::roster_projection::score_projection_adj_em`
  (parity with `/api/projections`). Populated 2024/2025/2026 = 281/263/232
  teams.
- `/api/predict`: `blended_margin = w(D)·preseason_margin + (1−w)·pit_margin`,
  `preseason_margin = proj_adjem_home − proj_adjem_away + HCA(venue)`. Win
  prob re-derived from blended margin; totals stay pit. `prediction_basis ∈
  {preseason, blended, pit, leaky}`; Predict-page chip color-codes it.
- `w(D)`: 1.0 before Nov 1 → linear → 0.0 by Jan 15 (cstat-season `S` runs
  Nov (S−1) → Apr S). HCA = 3.5.

## End-to-end validation

- **The headline fix:** Duke vs Army 2025-11-10 → **+53.3 (blended)** vs the
  old useless +1.5 (actual +55). Decays: +53.3 (Nov 10) → +47.2 (Dec 15,
  blended) → +38.4 (Jan 24, pure pit).
- **Neutral symmetry exact on the blend path:** Duke/Florida neutral
  +5.00 / −5.00, win prob 0.672 / 0.328 (sums to 1.0).
- **Graceful fallback:** teams with no projection row (too-thin roster, e.g.
  Houston 2026 after graduating its core) fall back to pit — `basis = pit`,
  no regression.
- **Coverage:** 77% / 72% / 63% of teams (2024/2025/2026) get a projection;
  the rest (thin-roster low-majors) fall back to pit early-season.

## Preseason-leg MAE + HCA (partial calibration)

Preseason-only game-margin MAE on games where both teams are projected
(8,386 games, 2024–2026), HCA = 3.5:

```
wk1 (day 0–7):   11.68 (n=267)     wk4–5:  11.72
wk2:             11.38 (n=438)     wk6–8:  11.38
wk3:             11.29 (n=506)     late:   10.58
```

Two takeaways that matter for the deferred `measure-blend-accuracy` tool:

1. **~11.3 opening-game MAE is sane, not a failure.** The §1 "5.9" is
   *team-season-AdjEM* MAE; a single *game* margin carries ~10–11 pts of
   irreducible game-to-game variance (KenPom per-game ≈ 9–10, Vegas ≈ 8). So
   preseason at ~11.3 is KenPom-ballpark — and far better than the empty-pit
   ~18+ it replaces in opening weeks. Preseason MAE is ~flat across the
   season (it's a season-long estimate), as expected.
2. **HCA can't be tuned from `games`** — no neutral-site flag, so early-season
   neutral games (Champions Classic, MTEs) are mislabeled home and pull the
   apparent optimum to HCA≈0. In the predict route the venue is caller-
   specified (clean), so the literature-standard 3.5 stays for genuine home
   games; the backtest just can't validate it. A neutral-aware game source
   would fix this.

## Remaining

The `cstat-ingest measure-blend-accuracy --year` subcommand: replay
historical games through preseason-only / pit-only / blended per week
(in-process pit inference, `backtest-ats` shape), find the crossover where
pit overtakes preseason, and retune the decay endpoints (Nov 1 / Jan 15)
+ HCA. The v1 schedule is a reasonable default until then.
