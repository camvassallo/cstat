# Redshirt handling in the projection pipeline

**Status: documented gap + implementation spec. Not yet built.** This note
records how redshirt / non-enrolling players are (mis)handled by the preseason
team projection, quantifies the impact, and specs a two-PR fix. No behavior
changes ship with this note.

## The projection, briefly

The preseason projection (`crates/cstat-core/src/roster_projection.rs`,
`compose_all_projections`, served by `GET /api/projections/{year}` in
`crates/cstat-api/src/routes/projections.rs`) builds each team's projected N+1
roster from base-season-N data: returning players + incoming transfers +
incoming recruits − departures, then scores team AdjEM/AdjO/AdjD through the
roster-impact ONNX calibrator. `base_season = N`, `target_season = N+1`.

There is **no concept of redshirting** — grep for
`redshirt|is_redshirt|eligibility|years_used|enrolled` finds only comments and
247 transfer-status strings, no columns or logic.

## What is actually broken (and what is not)

The gap is narrower than a first pass suggests. Three cases:

### (a) Redshirting recruits are over-credited — REAL, and the main issue

The recruit pull (`roster_projection.rs:724-766`) filters only on class year, a
resolved destination team, and `commit_status <> 'Uncommitted'` — **no check
that the recruit ever played**:

```sql
FROM recruits r ...
WHERE r.year = $1
  AND r.committed_team_id IS NOT NULL
  AND COALESCE(r.commit_status, '') <> 'Uncommitted'
```

Every ranked commit is synthesized into a freshman `PlayerRow`
(`freshman_row`, `roster_projection.rs:194-223`) with the freshman model's
projected `cam_v3` and fed to the AdjEM calibrator via `for_scenario`
(`roster_projection.rs:301-317`). A recruit who signs but redshirts / never
enrolls / reclassifies is credited anyway.

**Magnitude (ranked composite commits, classes 2014–2025, "never played" =
`cstat_player_id` still NULL after the play season was ingested):**

| Class → season | Ranked commits | Never played | %     |
| -------------- | -------------- | ------------ | ----- |
| 2025 → 2026    | 466            | 102          | 21.9% |
| 2024 → 2025    | 303            | 67           | 22.1% |
| 2023 → 2024    | 375            | 82           | 21.9% |
| (12-yr mean)   | ~400           | ~85          | ~21%  |

**Strongly rank-conditioned** (classes 2014–2025), which rules out a flat
haircut:

| Composite rank | Commits | Never played | %     |
| -------------- | ------- | ------------ | ----- |
| Top 30         | 342     | 12           | 3.5%  |
| 31–100         | 818     | 101          | 12.3% |
| 101–250        | 1673    | 340          | 20.3% |
| 250+           | 2030    | 531          | 26.2% |

Elite recruits almost always play, so the over-credit is concentrated in
lower-ranked depth commits (which carry small `cam_v3`) — the net AdjEM error is
largest for teams whose projected class is mostly low-ranked, and small for
blue-bloods. But it is systematic and one-directional (always over-credits).

### (b) Return-from-redshirt — the portal variant is already handled

A player who redshirts the base season played 0 games, has no qualifying
`player_season_stats` row (the returning fetch, `roster_projection.rs:654-687`,
gates on `games_played >= QUAL_MIN_GAMES_PLAYED (5)` and `minutes_per_game >=
QUAL_MIN_MPG (5.0)`, constants in `roster_features.rs:97-98`), and so is absent
from the returning set.

- **Portal variant — HANDLED.** The issue-#146 `satout_lookup`
  (`roster_projection.rs:789-853`) already looks back `TRANSFER_SEASON_LOOKBACK =
  2` seasons for any transfer whose `cstat_player_id` is missing from the
  base-season roster, and folds them in as arrivals. **Caden Pierce** (played
  Princeton through 2025, redshirted 2026, portal → Purdue for 2027) is caught
  by this path — his `transfers` row has a resolved `cstat_player_id`, so
  `satout_lookup` reattaches his 2025 line at Purdue. Verified: not a bug.
- **Stay-at-school variant — BROKEN but data-blocked.** A player who redshirts
  and returns to the *same* school has no portal row and no official-roster feed
  (cstat's `players` table is box-score-derived — it only contains players who
  played). We therefore have **no forward signal that they exist or will
  return**. This cannot be fixed by a filter tweak; it needs a new roster /
  eligibility data source (see PR 3). Population is smaller than (a).

## Detection signals available today

- **Redshirt recruit:** `recruits.cstat_player_id` is resolved in ingest Pass 2
  (`crates/cstat-ingest/src/ingest/recruits.rs:620-684`) by matching a recruit to
  a `players` row in season `year+1`; `players` rows exist only for players who
  appeared in a box score. So **committed recruit + `cstat_player_id IS NULL`
  after season `year+1` is ingested = never played**. Already selected in the
  projection SQL (`r.cstat_player_id`, `roster_projection.rs:732`).
- **Season completeness gate:** the proxy is only valid once `target_season`
  actually happened. Clean signal — `games` rows exist for played seasons
  (2025: 6294, 2026: 6280) and none for the unplayed upcoming season (2027: 0).
  A per-class NULL rate of 100% (2026 class → 2027) is the tell that the season
  is unplayed, not that everyone redshirted.
- **Transfer eligibility:** `transfers.eligibility_type` (`Immediate` / `TBD` /
  `PendingAppeal` / `Withdrawn`) and `eligibility_years`
  (`migrations/019_transfers.sql:66-70`) are ingested but never read by the
  projection — a `Withdrawn` / `PendingAppeal` arrival is not immediately
  eligible.

## Recruiting-pipeline roadmap

The guiding decision (owner call): **we do not forecast who will redshirt.** The
live upcoming projection includes every committed freshman. We only correct
*retrospectively*, once a season is complete and we know who actually played.
That rules out a forward redshirt-probability model (see "Parked" below) and
keeps every PR here well-tested and free of speculative modeling.

### PR 1 — retroactive redshirt-recruit exclusion — SHIPPED (this PR)

**Goal:** stop crediting recruits who never played, for projections of
*completed* seasons (backtests, grading, the "what we projected for 2026" view).
The live upcoming-season projection is untouched.

**As built:**
1. `compose_all_projections` computes a self-contained `target_season_complete`
   flag from **relative game volume** — `target_games >= 0.90 * base_games`
   (`SEASON_COMPLETE_GAME_FRACTION`), one small aggregate query. The base season
   is always fully ingested, so this is era-robust and needs no clock and no
   caller signature change. An unplayed upcoming season has 0 target games →
   `false` → nothing excluded. (The earlier plan to thread a
   `target_season < current_natstat_season()` bool through every caller was
   dropped in favour of this — same semantics, less churn, deterministic in
   tests.)
2. Each recruit gets `RecruitMeta.did_not_play = target_season_complete &&
   cstat_player_id IS NULL`. When true the recruit is dropped from the scored
   roster (`for_scenario`, `projecting_recruits_count`) — mirroring the existing
   `feeds_projection` commits-cohort exclusion — and from the API's
   `recruits_cam_v3_sum` display, so the graded report card's recruit
   contribution matches its AdjEM.
3. Recruits stay in the `recruits` payload/list for display (who committed);
   only their *scored* and *summed* contribution is zeroed.

**Tests:** `roster_projection::tests::
redshirt_recruit_excluded_from_scored_roster_but_still_displayed` (unit), plus
the existing `commits_feed_*` and `for_scenario_*` tests still green.

**Verified on real data:** gate reads complete for base 2025 → target 2026
(6280 ≥ 0.9·6294) and incomplete for base 2026 → target 2027 (0 games); Sebastian
Wilkins (Duke 2025 class, `cstat_player_id` NULL) is flagged for the 2026 graded
projection and untouched for the live 2027 forecast.

**Known limitation — ~5.3% false-exclude.** Of 882 NULL-id recruits across
completed classes, 47 (5.3%) actually played (exact-name match) but the Pass-2
resolver never linked them (name mismatch, or they played for a school other
than the one they committed to). Those get wrongly zeroed — ~4/year across all of
D-I, immaterial to any single team's projection, but it is the motivation for
PR 3.

**Validation run:** `projections-backtest --years 2024,2025,2026` (base seasons
2023–2025, all target-complete so the exclusion is active) grades clean — pooled
roster-impact MAE 6.13, bias +0.22, R² 0.765, consistent with documented
performance: **no regression**. The grading delta from the exclusion is small by
construction (redshirts carry near-zero cam_v3, so removing them barely moves a
team's AdjEM) — the win is correctness, not MAE. An exact before/after delta
needs the pre-PR-1 binary (a `main` comparison); the live upcoming year is
untouched by design (gate false).

### PR 2 — surface `did_not_play` in the report card (frontend) — SHIPPED (this PR)

**Goal:** make the retroactive exclusion visible so a completed-season report
card explains itself. Today `did_not_play` is serving-internal (`#[serde(skip)]`).

**As built:** `did_not_play` is exposed on the recruits payload (list
`top_recruits` and single-team `recruits_json`) and on the frontend types
(`ProjectedRecruit`, `ProjectedRecruitDetail`). The Projected page's Recruits
hover appends ` — redshirt (did not play)` to a flagged commit; TeamDetail's
`RecruitCard` greys the row (`opacity-55`), adds a `redshirt` tag, and
strikes-through the projected cam_v3 with a "not counted" tooltip. Display-only.

**Tests:** the tooltip-line formatting (rank / stars / redshirt marker) was
extracted to a pure helper `web/src/lib/recruitDisplay.ts::recruitTooltipLine`
and unit-tested (`recruitDisplay.test.ts`, 4 cases) — matching the repo's
pure-logic vitest convention (`portle`, `whichClass`). The card greying itself is
presentational; the repo has no jsdom/RTL component-test harness, and adding one
is a separate infra decision (see below), not bundled here.

**Size:** small, isolated, no model.

**Testing-infra note (raised during this PR):** `web/` has vitest but only
pure-logic tests — no component rendering. The high-ROI pattern for this
analytics UI is to keep extracting display/derivation logic into `lib/` helpers
and unit-testing those (as PR 2 did), rather than adding jsdom + React Testing
Library for mostly-presentational pages. A component-test harness is worth its
own PR if/when genuinely interactive, stateful components need regression cover.

### PR 3 — recruit → player linkage hardening (de-risks PR 1)

**Goal:** cut the 5.3% false-null so PR 1 rarely mis-flags a player who did
suit up. Also improves recruit-tracking data quality generally.

**Change:** extend the Pass-2 `cstat_player_id` resolver
(`crates/cstat-ingest/src/ingest/recruits.rs:620-684`) beyond exact
name+committed-team: add a `torvik_pid`/normalized-name fallback and a
"played for a *different* D-I team in `year+1`" match (decommits/flips), so a
recruit who enrolled anywhere is linked. Emit a resolver-coverage stat.

**Tests:** name-normalization unit tests (mirroring the existing
`normalize_player_name` cases) + a coverage-regression assertion that the
completed-class false-null rate stays below a floor. Re-measure the 5.3%.

**Size:** small–medium, ingest-only, no model.

### Parked — recruiting pipeline (deliberately not scoped now)

- **Forward redshirt-probability model** — a rank-conditioned `P(play)`
  classifier (the rate runs 3.5% for top-30 recruits to 26.2% for 250+). Clean
  and learnable, but it would put us in the business of forecasting redshirts,
  which the owner has declined. The rank-conditioned data and the LOCO/OOF plan
  are captured here so the decision can be revisited without re-deriving it.
- **Transfer eligibility wiring** (`transfers.eligibility_type`) — low current
  yield: 2026 committed arrivals are ~all `Immediate`; the `PendingAppeal` / `TBD`
  rows are all still `Entered` (uncommitted, not arrivals). Revisit if a future
  class shows material committed-but-ineligible arrivals.
- **Redshirt-freshman debut projection** — a recruit who redshirts then debuts
  ~2.4 seasons later at ~0.149 cam_v3 is invisible to the projection in the debut
  season (no prior stats, recruit link never resolved). Small population and low
  value; needs the PR 3 linkage first.
- **Stay-at-school redshirt *returner*** — blocked on a data source we don't
  ingest (official roster / eligibility feed) to know a 0-game player is still
  rostered. The *portal* redshirt returner is already handled (`satout_lookup`);
  only the stay-put variant is missing. Scope only if the population proves
  material.
- **Redshirt flag on the actual-season roster — considered and declined.** The
  `did_not_play` signal is a projection/recruiting concept; surfacing it on the
  box-score-derived season roster would be confusing and incomplete: (1) a
  redshirt has no box-score row, so they aren't on that roster at all — flagging
  them means injecting phantom entries; (2) we only detect *recruit* redshirts,
  not returner-redshirts or walk-ons, so the label would mislead by omission;
  (3) "committed but never appeared" conflates redshirt with non-enroll /
  reclass / silent transfer — honest as "did not play" on a recruit card, an
  overclaim as a "redshirt" badge on a roster. The flag stays on the projection's
  Incoming-recruits card, where it's complete and contextually correct. A real
  actual-roster redshirt view is the blocked official-roster/eligibility feed
  above, not this signal.

## Cross-cutting backlog (surfaced during this work, not recruiting-specific)

These came up alongside the redshirt work but belong to other subsystems; parked
here for continuity and easy to promote into `ROADMAP.md`.

- **Frontend component-test harness (jsdom + React Testing Library).** `web/` has
  vitest but only pure-logic tests (`portle`, `whichClass`, and now
  `recruitDisplay`) — no component rendering. The high-ROI pattern for this
  analytics UI is to keep extracting display/derivation logic into `lib/` helpers
  and unit-testing those, which this PR did. A jsdom/RTL harness is worth its own
  infra PR only when genuinely interactive, stateful components need regression
  cover; not justified for the mostly-presentational pages today.
- **Coach-data freshness: schedule the weekly `coaches` cron + auto-sync.** The
  `coaches` ingest (barttorvik coachdict → `coaches` / `coach_seasons`) is
  **not** in the nightly and has **no cron**, despite `docs/deploy_nightly_cron`
  / the in-season plan calling for a weekly job. It's a manual
  `cargo run -- coaches` + `sync_to_prod.sh --tables coaches,coach_seasons`, so
  new-HC moves (e.g. Michigan → Boynton for 2027) silently go stale until someone
  reruns it. Wire a weekly Railway cron + prod push.
- **Coach "arrived from" misses gap-year hires.** `coach_prev_team` is derived
  from a *base-season* different-program row (`fetch_coach_cae` LATERAL in
  `projections.rs`), so a coach who sat out a year (e.g. Boynton: Oklahoma St.
  through 2024 → Michigan 2027, no 2026 row) resolves to NULL and the badge reads
  "first season" instead of "from Oklahoma St." Widen the lookback a season or
  two to the coach's most recent prior program.

## Key files

- `crates/cstat-core/src/roster_projection.rs` — `did_not_play` on `RecruitMeta`,
  `target_season_complete` gate + `SEASON_COMPLETE_GAME_FRACTION`, `for_scenario`
  / `projecting_recruits_count` exclusion, recruit pull, returning fetch,
  `satout_lookup`, `freshman_row`.
- `crates/cstat-api/src/routes/projections.rs` — `recruits_cam_v3_sum` exclusion
  (list route); recruits display payload (PR 2 target).
- `crates/cstat-core/src/roster_features.rs:97-98` — `QUAL_MIN_GAMES_PLAYED`,
  `QUAL_MIN_MPG`.
- `crates/cstat-ingest/src/ingest/recruits.rs:620-684` — Pass-2 `cstat_player_id`
  resolution (the redshirt signal; PR 3 target).
- `migrations/019_transfers.sql:66-70` — `eligibility_type` / `eligibility_years`.
- `migrations/020_recruits.sql`, `migrations/001_initial_schema.sql` — no
  eligibility column today.

