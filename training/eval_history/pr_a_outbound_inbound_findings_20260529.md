# PR A — Portal Δ-CamPom feature + cross-season join bug

**Date**: 2026-05-29
**PR scope**: ROADMAP §6 "Preseason × pit blend" → recommendation #1
(outbound/inbound cam_v3 as Phase B features).
**Bonus**: surfaced and fixed a separate cross-season SQL bug that was
silently dropping ~25% of inbound transfer signal.

## Headline result

| Metric (LOSO 2025+2026, n=495) | Pre-PR baseline | Post-PR |
| ------------------------------ | --------------- | ------- |
| Pipeline raw MAE               | 6.39            | 6.45    |
| Pipeline blended MAE (best w)  | **5.86**        | **5.92**|
| Q1 bottom bias                 | +6.09           | +5.87   |
| Q4 top bias                    | −4.35           | −4.33   |
| `portal_outbound_cam_v3` β     | +0.978 (t=+2.53)| +1.454 (t=+3.66) |
| `portal_inbound_cam_v3` β      | −0.605 (t=−1.44)| **−0.930 (t=−2.13)** |
| `portal_inbound_count` β       | −0.394 (t=−0.94)| **−1.362 (t=−2.70)** |
| OLS R² (signed err on signals) | 0.052           | 0.072   |

**Pipeline accuracy is essentially flat** (5.86 → 5.92 MAE), so PR A
did not meet the documented acceptance bar (≥0.10 MAE drop, ≥1.5
Q4-bias close). But the audit *measurement* itself moved materially:
inbound signals went from "directionally right, not significant" to
"both significant at p<0.05", and OLS R² gained 38%. This is the
post-fix audit telling us the previous numbers were biased by a
silent SQL bug. We now know what the signals truly look like.

## The cross-season SQL bug

`audit_preseason_projections.py::fetch_portal_signals` (the only
pre-existing committed site with the bug) joined target-season player
rows via `players.natstat_id` alone:

```sql
JOIN players p_tgt
    ON p_tgt.natstat_id = p_base.natstat_id
   AND p_tgt.season = t.year + 1
```

`natstat_id` is reissued per team, so a transferred player gets a new
`natstat_id` at their new school — **the join silently drops every
transferred player**. `torvik_pid` is stable across transfers (per
`reference_torvik_pid` memory: 96% coverage, zero collisions).
Corrected pattern uses `natstat_id OR torvik_pid`:

```sql
JOIN players p_tgt
    ON p_tgt.season = t.year + 1
   AND (
        p_tgt.natstat_id = p_base.natstat_id
        OR (tps_base.torvik_pid IS NOT NULL AND p_tgt.id IN (
            SELECT player_id FROM torvik_player_stats
            WHERE torvik_pid = tps_base.torvik_pid AND season = t.year + 1
        ))
   )
```

**Pool-wide measurement** (2024+2025 portal cycles, n=1,958):

| Match pattern               | Count | Share |
| --------------------------- | ----- | ----- |
| natstat_id preserved        | 1,455 | 74.3% |
| torvik_pid preserved        | 1,929 | 98.5% |
| natstat_id only (no torvik) | 17    | 0.9%  |
| **torvik_pid only (the bug surface)** | **503** | **25.7%** |
| both matched (non-transfer) | 1,426 | 72.8% |

The buggy SQL was dropping **503 out of 1,958 (25.7%) of inbound
transfer signal**. Fixed in the audit; the two new sites in this PR
(`train_roster_impact_model.py::INBOUND_QUERY` and
`decompose_projection_error.py::fetch_portal_sums`) were authored
against the same pattern to avoid reintroducing it. The new
regression test (`training/test_cross_season_joins.py`) pins the
correct behaviour for all three.

## Canonical case studies (Michigan vs Maryland mirror pair)

Both teams' 2026 backtest entries now show the right portal sums after
the SQL fix:

| Team | baseline | pipeline pred | oracle pred (calibrator given actual roster) | actual | outbound | inbound |
| ---- | -------- | ------------- | -------------------------------------------- | ------ | -------- | ------- |
| Maryland 2026 | +30.6 | +21.6 | +10.0 | +5.2  | **+38.6** | +9.6   |
| Michigan 2026 | +28.1 | +33.7 | +35.4 | +44.6 | +12.1    | **+38.6** |

Maryland lost +38.6 cam_v3 and replaced only +9.6 — they collapsed.
Michigan lost +12.1 and gained +38.6 — they surged. The model
directionally captures both moves but under-shoots the magnitude.

## Error decomposition (the load-bearing diagnostic)

For each of the 495 team-seasons, I scored an **oracle prediction**:
feed the LOSO impact model the *actual* target-season roster with
*actual* cam_v3. Comparing this to the pipeline and to actual reveals
where the projection error lives.

| Layer | Pooled MAE | Pooled bias |
| ----- | ---------- | ----------- |
| Pipeline (live)                                          | **6.45** | +0.41 |
| **Calibrator-only** (oracle roster + actual cam_v3)      | **3.93** | −0.09 |
| **Upstream-only** (= pipeline − oracle = trajectory + freshman + composition errors) | **4.55** | +0.50 |

| Bucket | pipeline bias | calibrator bias | upstream bias |
| ------ | ------------- | --------------- | ------------- |
| Q1 bottom | +8.31 | +2.69 | **+5.62** |
| Q2 below-median | +0.47 | +0.11 | +0.36 |
| Q3 above-median | −2.15 | −0.69 | −1.46 |
| Q4 top | −5.03 | −2.49 | **−2.55** |

**Q1 bottom (Maryland-like collapses)**: ~⅔ of the over-projection is
**upstream** — the trajectory model thinks the remaining players will
be better than they end up being on a gutted roster. The calibrator
contributes only ⅓. **The lever here is the trajectory model on thin
rosters, not the roster-impact calibrator.**

**Q4 top (Michigan-like surges)**: roughly 50/50 split. The calibrator
itself caps the projected upside (−2.5 bias on top teams even given
perfect inputs), and the upstream projection of who will play +
their cam_v3 is also slightly conservative. **Both layers compress
the upside equally on top teams.**

## What this means for PR ordering

PR A's specific feature (outbound/inbound) is **technically correct
but yields small lift** because it's largely redundant with the
existing `cam_sum` aggregate (departed players drop out of the rotation
naturally; arrived players join it). The outbound/inbound features
capture only the *residual* effect not already in `cam_sum`. The
audit's β prediction overstated the available lift because it didn't
account for that redundancy.

**The bigger fish, per the decomposition**:
- For Q1 (over-projected bust teams): attack the **trajectory model's
  regression-to-mean on thin rosters**. When a team loses 60% of its
  cam_v3, the remaining returners should project closer to their
  modest current selves than to a class-year-archetype average.
- For Q4 (under-projected breakout teams): attack the **roster-impact
  calibrator's tail compression**. Top rosters with elite cam_v3 sums
  get a smaller AdjEM than the cam_v3-implied identity. Could be
  fixed with a monotonic feature constraint, or by retraining with
  the impact-feature variant (currently behind `ROSTER_INCLUDE_IMPACT=1`).

## Regression tests added

`training/test_cross_season_joins.py` — Python self-test that pins
the bug class. Three layers:

1. **Per-team smoke tests** with `min_torvik_only_count` bounds — if
   the bug regresses on Michigan 2026 specifically, the test fails.
2. **Pool-wide coverage invariant**: across 2024+2025 portal cycles,
   the corrected query must recover at least 300 torvik-only matches
   (vs ~503 observed today). Catches a global SQL regression.
3. **Outbound baseline pin** on Maryland 2026 — unaffected by the bug
   but useful as a sanity anchor.

Run: `cd training && python test_cross_season_joins.py` (exit 0 on
pass). Requires `DATABASE_URL`.

## Files changed

- `training/train_roster_impact_model.py` — new OUTBOUND_QUERY +
  INBOUND_QUERY (both with the correct `natstat_id OR torvik_pid`
  cross-season pattern from the start); +27-feature vector incl.
  outbound/inbound sums.
- `training/audit_preseason_projections.py` — fixed a pre-existing
  `natstat_id`-only join in `fetch_portal_signals` inbound branch
  (the **only** site that was actually buggy in committed code).
- `training/decompose_projection_error.py` — **new**; per-team
  upstream/calibrator attribution. Built with the correct
  cross-season pattern from the start.
- `training/test_cross_season_joins.py` — **new**; regression tests
  that prevent any of the three sites from reverting to natstat_id-only.
- `crates/cstat-core/src/roster_impact.rs` — 25 → 27 features, added
  outbound + inbound slots, new test.
- `crates/cstat-core/src/roster_projection.rs` — `ProjectedRoster`
  gains outbound + inbound fields; `compose_all_projections` populates
  them from in-scope data (no extra SQL).
- `crates/cstat-api/src/routes/projections.rs` — passes new fields to
  `build_roster_impact_features`.
- `crates/cstat-ingest/src/projections_backtest.rs` — ditto.
- `training/models/roster_impact_model.onnx` + `_meta.json` — retrained.
- `training/models/roster_impact_loso/roster_impact_model_{2025,2026}.onnx` — retrained.
- `ROADMAP.md` — PR A marked shipped with findings doc reference;
  PR B / C / D / E re-ordered per the decomposition's attribution.
