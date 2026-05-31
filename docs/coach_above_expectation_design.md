# Coach-Above-Expectation (CAE) — design & scope

**Status:** scoped 2026-05-31 (feasibility measured, not yet built)
**Feasibility script:** `training/cae_feasibility.py`
**Prior context:** `training/eval_history/pr_e_coaching_change_derisk_20260531.md` (the boolean
coaching-change feature was refuted; CAE is the salvageable "coach signal" path)

## 1. Goal

A **descriptive** coach grade: *how much does a team out/under-perform the talent on its
roster, attributed to the coach, aggregated across the coach's career with shrinkage.*

```
CAE_season  = actual_team_AdjEM − roster_talent_projected_AdjEM      (per team-season residual)
CAE_career  = EB_shrink( mean over the coach's seasons )             (the rating)
```

This is the coaching analog of the player CamPom residual. It is **not** a predictor
(the predictive-lift test failed in the PR E de-risk and again here — see §3); it is a
product/insight surface. Treat any future use as a projection-model feature as a separate,
later, gated question.

## 2. Feasibility verdict (measured)

**GREEN for a descriptive metric — against the roster-only denominator only.** The result
hinges entirely on the expectation denominator:

| Denominator | σ²_between (coach skill) | ICC (1-yr) | YoY persistence (same-team / moved / split-half) | verdict |
|---|---|---|---|---|
| **`phase_b`** (roster-talent-only projection) | 8.25 (σ≈**2.87**) | **0.135** | +0.047 / +0.112 / +0.114 | **viable** |
| `served` (`0.5·baseline + 0.5·phase_b`) | **0.00** | 0.000 | −0.176 / −0.054 / −0.130 | dead |

**Why `served` is self-defeating for a coach metric:** `baseline` = prior-year actual AdjEM.
A persistently-good coach raises last year's AdjEM, which raises the expectation, which
absorbs exactly the persistent coach signal we want to measure (and injects mean-reversion,
hence the negative autocorrelation). **The de-risk's null (rounds 1–2) was a denominator
artifact** — it used `served`. Against the talent-only `phase_b`, the signal is real.

**Face validity (top raw CAE vs `phase_b`, coaches ≥3 seasons):** Josh Schertz, Darian
DeVries, Richard Pitino, Randy Bennett (St. Mary's), Herb Sendek (Santa Clara), Kevin
Willard, Rick Pitino, Brian Wardle (Bradley), Jerrod Calhoun, Amir Abdur-Rahim. Mid-major
overachievers + elite developers — *not* dominated by blue-bloods, which is reassuring that
CAE-vs-`phase_b` measures coaching-above-talent rather than the projection's Q4 under-bias.

**Caveats that bound the design:**
- **Low reliability at 5-season depth.** Shrinkage constant k ≈ **6.4 season-equivalents**;
  reliability is 0.24 (2 seasons) → 0.44 (5 seasons). Every rating must be heavily shrunk and
  shown with a credibility band; a 1–2 season coach is mostly prior (≈0).
- **Coach/program confound.** The variance-components σ²_between (ICC 0.135) likely *overstates*
  pure coach skill because a coach who stays 3–5 years at one program also carries that
  program's persistent projection bias. The cleaner "transferable coach skill" estimate is the
  **moved-teams autocorrelation (+0.112, n=53)** — positive but thin. At 5 seasons we cannot
  cleanly separate coach from program; frame CAE as **coach×program over-expectation**, with the
  moved-team carryover surfaced separately where data allows.
- **What CAE excludes by construction.** The denominator is *roster talent*, which is itself
  partly a coaching achievement (recruiting, development, retention). So CAE captures
  scheme / rotations / motivation / in-game / system-fit **above** the roster — it does **not**
  credit roster-building. Document this explicitly; optionally add a recruiting-inclusive variant
  later (residual vs a recruit-blind or pure-returning baseline).

## 3. Predictive use — out of scope for v1 (gated separately)

The transferable signal (moved-teams +0.11, n=53) is too weak to lift the projection point
estimate out-of-sample — consistent with the PR E round-2 failure (`corr(prior CAE, new-team
resid) = −0.06`). **Ship descriptive-only.** Revisit a CAE projection feature only if (a) the
backtest extends to more seasons (raising reliability) and (b) it clears the standard lift gate
against the expanded #96 backtest. The likeliest *real* predictive use remains uncertainty
bands (new-coach teams are 1.12× noisier — the PR E salvage), not a point feature.

## 4. Methodology (the load-bearing decisions)

1. **Denominator = roster-talent-only projection (`phase_b`), never `served`.** Non-negotiable;
   it's the whole ballgame (§2). Use the persisted projection where possible — migration
   `023_team_preseason_projection.sql` already materializes the served projection, but CAE needs
   the **pre-blend roster-only** number, so either persist `phase_b` alongside it or recompute via
   `cstat_core::roster_projection`.
2. **De-bias the projection per-quartile before differencing** (safeguard). The projection has
   known Q1/Q4 tail bias (Q1 over-, Q4 under-projected); left in, it leaks into CAE as fake
   coach credit at perennially-elite/weak programs. Subtract the per-actual-quartile mean bias
   (from `decompose_projection_error.py`) first. **Acceptance check:** the top-CAE list must not
   gain blue-blood dominance after de-bias, and CAE must be ~uncorrelated with program prestige.
3. **Empirical-Bayes shrinkage.** `CAE_hat = (n/(n+k))·mean_resid`, k ≈ 6.4 (re-estimate from
   the variance components at build time). Report `n`, the shrunk value, and a credibility
   interval. Default-sort the leaderboard by shrunk CAE, not raw.
4. **Name identity & dedup (real gotcha).** coachdict uses full names; **Rick Pitino and Richard
   Pitino are different people and both appear**, as do "Phil Martelli" / "Phil Martelli Jr.".
   Build a canonical-coach entity with explicit disambiguation; never collapse on surname.
5. **Team-name join** reuses the existing Torvik→NatStat reconciliation (`team_aliases.rs`,
   `team_match_score`, `data/team_short_names.json`); coachdict→backtest matched 361/362 with one
   alias (`Texas A&M Corpus Christi`→`Texas A&M Corpus Chris`).

## 5. Data & architecture

- **Ingest**: `cstat-ingest coachdict` (or fold into the Torvik step) pulls
  `https://barttorvik.com/coachdict.json` (one file, all seasons; documented in
  `docs/torvik-api-guide.md` §4) → normalized **`coach_seasons`** table
  `(coach_id, team_natstat_id, season)` + a **`coaches`** entity table `(coach_id, canonical_name)`
  for dedup. Snapshot the raw JSON to `data/coaches/` for reproducibility/offline.
- **Compute**: CAE residual = (per-quartile-debiased) actual AdjEM − roster-only projection, then
  EB-shrink per coach → **`coach_ratings`** `(coach_id, season|career, cae_raw, cae_shrunk, n, ci)`.
  Start as an **offline Python job** (reuses the backtest + `decompose_projection_error.py`
  infra; the feasibility metrics in `cae_feasibility.py` become regression guards). Promote to a
  Rust compute step only if it needs to refresh in-season.
- **API**: `GET /api/coaches` (leaderboard), `GET /api/coaches/:id` (tenure, team history,
  per-season CAE sparkline), and a coach field on `GET /api/teams/:id`.
- **Frontend**: `/coaches` leaderboard page (shrunk CAE, tenure, teams, sparkline); coach card on
  TeamDetail; optional coach line on the projections/Future tab.

## 6. Phasing

- **PR 1 — coachdict ingest + entity model. SHIPPED 2026-05-31.** Migration `024_coaches.sql`
  (`coaches` entity + `coach_seasons` mapping), `TorkvikClient::fetch_coachdict`,
  `ingest/coaches.rs`, `cstat-ingest coaches [--year]`. Result: 12 seasons (2015–2026), 4,294
  coach-seasons, **99.4% team match** (27 NULLs all genuinely-absent team-seasons), 735 distinct
  coaches, **657 `is_new_hc` flags** (PR E new-coach signal, free). Findings worth carrying into
  PR2: (a) coachdict has redundant **inverted entries** (`coach→team` alongside `team→coach`) —
  filtered via `is_inverted_entry` (mirror + value-is-team + key-isn't, so legit unmatched teams
  survive); (b) added three coachdict-spelling aliases to the shared `team_name_match` (Texas A&M
  Corpus Christi w/ both hyphen variants, UT Martin, Arkansas Little Rock); (c) **Houston
  Baptist→Christian** rename is season-dependent and stays unmatched for pre-rename seasons —
  re-resolve via `natstat_id` continuity in PR2 if those team-seasons matter. Rick ≠ Richard
  Pitino verified distinct. The flag is `coach[Y]≠coach[Y−1]` over the same table.
- **PR 2 — CAE computation.** Persist the roster-only projection, per-quartile de-bias, EB
  shrinkage → `coach_ratings`. Offline Python first; `cae_feasibility.py` thresholds as guards
  (ICC>0, positive split-half, top-list face validity, no prestige correlation post-de-bias).
- **PR 3 — surfaces.** `/api/coaches` + `/coaches` page + TeamDetail coach card.
- **PR 4 (optional/later).** Recruiting-inclusive variant; extend the backtest to pre-2022 to
  raise reliability; the separately-gated predictive-feature test.

## 7. Biggest risk & the lever

Five seasons is thin (reliability 0.24–0.44, k≈6.4). The single highest-leverage improvement is
**more seasons of roster projections** (extend the #96 backtest to 2016+), which raises per-coach
reliability, sharpens σ²_between, and grows the moved-team sample that separates coach from
program. It's blocked on pre-2021 transfer-data quality (degrades the roster projection), so it's
a deliberate trade, not a freebie — but it's the path from "fun descriptive grade" to "trustworthy
coach rating."
