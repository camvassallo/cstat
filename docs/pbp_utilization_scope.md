# PBP Data: Utilization Scope

Scoping for how cstat uses its play-by-play (PBP) assets, written 2026-06-08. The
organizing constraint: **anything we train a served model on in the off-season
must be computable in-season from the nightly cron's API fetch, with no
train/serve skew.** This doc inventories what we have, records a decisive finding
about *what the NatStat API actually serves*, and lays out a tiered plan.

## 1. Data inventory

| Asset | Where | Span | Source / quality |
|---|---|---|---|
| Raw `play_by_play` (tags, score, clock, onfloor cols) | local only | 2015–2026 (2019 corrupt) | tags: CSV for 2015–2025, **API for 2026** (the onfloor backfill's `replace_game_pbp` swapped them — CSV and API tag streams verified equivalent, see scope §3); `onfloor_*` populated for 2026 only |
| Tag aggregates → `player_game_stats` cols (paint/perimeter, transition, 2nd-chance, off-TO pts, fouls drawn, `plus_minus_pbp`) | **prod** | 2015–2026 | direct `player_id` rollup — **source-independent** |
| `lineup_stints` (per-stint 5-man + possessions) | local only | 2015–2026 | replay ~86% (CSV seasons) / onfloor (2026) |
| `lineup_aggregates` (top lineups) | **prod** | 2015–2026 | replay / onfloor |
| `player_on_off` | **prod** | 2015–2026 | replay / onfloor |

## 2. Decisive finding: what the NatStat API actually serves (verified 2026-06-08)

There are **two distinct NatStat lineup sources**, with very different availability:

1. **Per-play `onfloorhome`/`onfloorvis`** (the `playbyplay/{date}` date-path).
   - **Current season only.** The date-path returns `NO_DATA` for every tested
     2024-25-season and earlier date (2024-12-07, 2025-02-15, 2025-03-23,
     2025-04-05 all empty); only 2025-26 dates return plays. The API retains
     date-path PBP for roughly the current season.
   - **Non-backfillable.** We cannot retrieve onfloor for any past season — the
     data is gone once the season rolls over. We have 2026 onfloor only because
     we captured it while 2025-26 is the current season.
   - **Not a version artifact (ruled out 2026-06-08).** `api3.natst.at` returns
     the *same* `NO_DATA` for the same 2025 date (identical retention; its
     per-game hydrate 302-redirects away, i.e. unsupported), and no `api3-5` /
     `api35` / `api2` / `api` host exists. Retention is NatStat-side. The only
     untested lever is the account's `apiplus` flag (currently `no`) — an API+
     tier upgrade *might* extend retention or the 500/hr cap, but that's a paid,
     unverified gamble, not a code path.
   - **Implication:** the onfloor corpus can only grow *prospectively*. To ever
     have 2027 onfloor, the nightly in-season capture must run — or it is lost
     permanently. This makes the nightly cron essential, not optional.
   - Per-play granularity (shot-context, possession-level), but per-play
     incomplete (~45% of plays list only 4 of 5 — see `pbp_methodology.md`
     "On-floor union reconstruction").

2. **Per-game `lineups` object** (the `games;playbyplay,lineups/mbb/{gamecode}`
   per-game hydrate).
   - **All seasons.** Verified present for 2015, 2018, 2020, 2022, 2025 (46–77
     5-man units per game). NatStat retains the per-game hydrate across the full
     historical range, unlike the date-path.
   - **Exact, server-computed.** Each unit carries `possessions`, `oppp`/`dppp`
     (offensive/defensive points-per-possession), `effmargin`, `plusminus`,
     `points`/`points-d`, and full offense/defense/margin box splits (fgm/fga,
     3fm/3fa, ftm/fta, reb, ast, blk, stl). No replay reconstruction, no onfloor
     gaps.
   - **Caveats:** units are keyed by abbreviated name (`"D. Swain · M. Foster ·
     …"`), so players resolve by first-initial + last-name against the game's two
     box-score rosters (the existing null-player-sub fallback pattern). It is
     *per-game aggregated*, not per-play — no shot-context, and the opponent
     lineup is not paired to each unit (it's on/off-grade, not full-matchup).
   - **Cost:** ~1 API call per game; a season ≈ 5,500 games ≈ 11 hrs at the
     500/hr cap, ~12 seasons ≈ 130 hrs — a multi-day background job.

3. **Raw plays + tags** — we already hold these for all seasons via the CSV
   backfill; the API isn't needed for them.

**The headline:** for a *cross-season-consistent, exact* lineup source, the
per-game `lineups` object is the answer — not onfloor. It is available for every
season historically (backfill) *and* in-season (nightly per-game hydrate), so
train and serve read the **same** source with **no skew**. Onfloor stays the
high-resolution *current-season* source for per-play work and a prospective
corpus for future per-possession models.

## 3. The train/serve compatibility lens

A feature is cron-compatible only if (a) it is computable incrementally from the
nightly API fetch, and (b) its training-time values match its serving-time
values. That splits PBP features by source-robustness:

- **Skew-free** — direct `player_id` tag rollups (paint%, transition rate,
  2nd-chance, points-off-TO, fouls drawn, assist rate, true A/TO). Identical
  under any lineup source; already on prod for all seasons.
- **Skew-prone** — anything depending on lineup membership (on/off,
  lineup_quality, RAPM). Today these are replay for history, onfloor for 2026 —
  a built-in skew. The `lineups`-object path removes it by making *all* seasons
  the same exact source.

## 4. Utilization plan (tiered)

**Tier 1 — skew-free tag features into the models (ship first).** Roll the
`player_game_stats` PBP columns into roster-aggregate diff features for the
existing 49-feature LightGBM (team paint%, transition rate, FT-rate,
foul-drawing, A/TO, offense and defense). Zero train/serve skew by construction,
available 2015–2026, computable nightly. No dependency on any lineup-accuracy
work. Highest value-per-risk; unblocks the "PBP-features model beats baseline"
acceptance test.

**Tier 2 — adopt the NatStat `lineups` object as the cross-season lineup
source.** Build a `lineups`-object ingest (parse, name-resolve to UUIDs, store
per-game units; aggregate to season). This:
- gives **exact** `lineup_aggregates` / on/off for *all* seasons, retiring the
  replay-reconstruction fidelity gap (and sidestepping the onfloor 4-man
  reconstruction entirely for the served aggregates);
- is **source-consistent** train↔serve (same hydrate historically and nightly);
- is the right substrate for Tier 3.
Requires the historical backfill (Section 6) and a name-resolution step.

**Tier 3 — RAPM / adjusted +/-.** The `lineups` object's per-game unit
possessions + margins are valid stints for a ridge-regressed adjusted +/-
(game-level resolution). Onfloor adds true per-possession matchups where it
exists (2026+). Multi-PR, own design doc, soft-blocked on Tier 2.

**Onfloor's role going forward.** Not the cross-season ML substrate (only 2026,
non-backfillable). It is: (a) the **2026 site-display** source (the improved
union-reconstructed lineups / on/off shipped 2026-06-08), and (b) a **prospective
per-play corpus** that only grows if the nightly capture runs — valuable later
for shot-context and possession-level models the per-game `lineups` object can't
support.

## 5. Source partitioning (keep the fidelities from mixing)

Yes — partition explicitly so training never mixes sources. Recommended:

- A single **authoritative per-game lineup-source label** (extend the existing
  `'replay'`/`'onfloor'` `source` flag with `'natstat_lineups'`), surfaced at the
  grain training reads (per game, propagated to `lineup_aggregates` /
  `player_on_off`).
- The feature-extraction layer **filters to one source** and **asserts no mix**
  (fail loudly if a training pull spans sources). Until Tier 2 lands, that source
  is either all-`replay` (consistent, ~86%) or the skew-free tag features (no
  lineup source at all — Tier 1).
- Document the rule: *served lineup-derived models train and serve on a single
  `source`.*

## 6. What to schedule (and what must be built first)

The originally-imagined "historical onfloor backfill" is **not possible** — the
API has no historical onfloor (Section 2). The schedulable background jobs are:

1. **Nightly in-season onfloor capture (build/raise priority now).** `cstat-ingest
   play-by-play --date {yesterday}` → compute → derived-only sync. This is the
   *only* way to accumulate onfloor for 2027+; skipping a season loses it forever.
   `scripts/onfloor_backfill.sh` already does the within-current-season catch-up.
2. **`lineups`-object historical backfill (after the Tier-2 ingest exists).** A
   per-game hydrate sweep over 2015–2026 (~130 hrs of API at the cap) → exact
   cross-season `lineup_aggregates`. This is a real background job, but it cannot
   be scheduled until the ingest/parse/name-resolve path is built — there is no
   existing script for it (the onfloor backfill only does the date-path).

So: we can schedule (1) immediately; (2) is a "build then schedule" item.

## 7. Recommended sequence

1. **Tier 1** tag features into the model — cron-safe by construction, no lineup
   work, measurable lift. Do first.
2. Decide **Tier 2**: commit to the `lineups`-object ingest as the cross-season
   lineup source (recommended), then run the historical backfill in the
   background.
3. Wire the **nightly capture** cron (onfloor + tags) so no future data is lost.
4. **Tier 3 (RAPM)** once Tier 2's exact lineups exist.

The on/off and top-lineup surfaces shipped this week stay as **site display**
(now much improved for 2026) and become *ML* inputs only after the Tier-2 source
decision.
