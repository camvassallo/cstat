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
| Tag aggregates → `player_game_stats` cols (paint/perimeter, transition, 2nd-chance, off-TO pts, fouls drawn, `plus_minus_pbp`) | **prod** | **2020–2026 (2019 corrupt-gated)** — the 2015–2018 feeds carry only box-event tags, zero contextual vocabulary (paint share of tagged FGA = 0.000 vs ≥0.41 every 2020+ season; found 2026-06-09); `compute.rs::pbp_lacks_context_tags` gates them to NULL. `plus_minus_pbp` is stint-derived, unaffected (2015–2026). | direct `player_id` rollup — **source-independent** |
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
   - **All seasons — but NOT complete in all seasons (coverage correction,
     2026-06-10, off the first ~2,700 backfilled games).** The hydrate exists
     for every season, but the unit set only *covers the full game* in the
     middle era: unit points sum to ~91% of the box score on 2020 games (78%
     of team-games within ±5%) vs **~38% on 2025/2026** (median 16 units/game
     vs 53–57 — NatStat appears to compute modern units from its sparse
     onfloor stream), and **2015 over-counts (~109%, overlapping windows)**.
     The de-risk's "46–77 units per game" sample was not representative.
     Coverage is a **gradient**, not a cliff: 2024 averages ~65% (uniform
     across months) and passes the ±5% gate at only ~7.5% of team-games, so
     the high-yield exact era is likely ≤2023 — boundary refines as the
     backfill descends. Gate selection also carries a **mild winner lean**
     in gradient seasons (2024 gated sides: avg margin +4.9, 60% winners) —
     within-team rates stay exact, but gated games are not a random sample;
     model features built on them must account for this.
   - **Exact where coherent, server-computed.** Each unit carries
     `possessions`, `oppp`/`dppp` (offensive/defensive points-per-possession),
     `effmargin`, `plusminus`, `points`/`points-d`, and full
     offense/defense/margin box splits (fgm/fga, 3fm/3fa, ftm/fta, reb, ast,
     blk, stl). No replay reconstruction, no onfloor gaps. **Scale caveat:**
     the `possessions` field is not cstat's possession unit — it sums to only
     ~55–66% of the box-score estimate even on coherent games — so consumers
     must rescale each team-game's unit possessions to the box estimate
     (compute does).
   - **Caveats:** units are keyed by abbreviated name (`"D. Swain · M. Foster ·
     …"`), so players resolve by first-initial + last-name against the game's two
     box-score rosters (the existing null-player-sub fallback pattern). It is
     *per-game aggregated*, not per-play — no shot-context, and the opponent
     lineup is not paired to each unit (it's on/off-grade, not full-matchup).
   - **Cost:** ~1 API call per game; a season ≈ 5,500 games ≈ 11 hrs at the
     500/hr cap, ~12 seasons ≈ 130 hrs — a multi-day background job.

3. **Raw plays + tags** — we already hold these for all seasons via the CSV
   backfill; the API isn't needed for them.

**The headline (revised 2026-06-10):** the per-game `lineups` object is the
exact lineup source *where it is coherent* — which the backfill shows is the
middle era, not all seasons. The original "same source train↔serve, no skew,
all seasons" framing is dead as stated: 2025/2026 units are too sparse to
serve, so the modern era stays onfloor/replay and the object's value
concentrates in ~2019–2024 (boundary to be confirmed as the backfill lands).
Compute arbitrates per team-game with a coherence gate (unit points ≈ box
score) rather than per source. Onfloor stays the high-resolution
*current-season* source for per-play work and a prospective corpus for future
per-possession models.

## 3. The train/serve compatibility lens

A feature is cron-compatible only if (a) it is computable incrementally from the
nightly API fetch, and (b) its training-time values match its serving-time
values. That splits PBP features by source-robustness:

- **Skew-free** — direct `player_id` tag rollups (paint%, transition rate,
  2nd-chance, points-off-TO, fouls drawn, assist rate, true A/TO). Identical
  under any lineup source; on prod for every season whose feed carries the
  contextual vocabulary (2020+, see §1).
- **Skew-prone** — anything depending on lineup membership (on/off,
  lineup_quality, RAPM). Today these are replay for history, onfloor for 2026 —
  a built-in skew. The `lineups`-object path removes it by making *all* seasons
  the same exact source.

## 4. Utilization plan (tiered)

**Tier 1 — skew-free tag features into the models (ship first). [CLOSED
2026-06-10: backtested and REJECTED in all three model families — serving-only.]**
The plan was to roll the `player_game_stats` PBP columns into diff features for
the existing 49-feature LightGBM (team paint%, transition rate, foul-drawing,
offense and defense). Zero train/serve skew by construction, available
2020–2026 (contextual tags don't exist before 2020 — see §1), computable
nightly. Outcome: margin MAE got 0.075 *worse* on a shared 2026 holdout;
trajectory and archetype integrations also rejected (numbers in
`training/eval_history/tier1_pbp_models_20260610_summary.json`). CamPom/GBPM
already absorb the value signal the style rates carry. The gated plumbing
(`PBP_FEATURES`, default off) and the `training/experiment_*_pbp.py` harnesses
remain as the re-test path if the data changes.

**Tier 2 — adopt the NatStat `lineups` object as the cross-season lineup
source. [INGEST SHIPPED 2026-06-10; SOURCE SWAP SHIPPED 2026-06-11
(coherence-gated); backfill running.]**
The de-risk passed with three live findings now encoded in the loader
(`crates/cstat-ingest/src/ingest/lineups.rs` module docs): unit player *codes*
are cross-era unreliable (resolution is two-tier and game-scoped — code vs the
game's box roster, then the abbreviated `lineupplayers` name; measured ≥99.2%
resolved on 2015/2020/2025/2026 samples), the hydrate must be lineups-only
(`games;playbyplay,lineups` 500s on 2026) and v4-pinned, and the feed swaps the
two team codes on some games (each unit resolves against both rosters; a clear
majority overturns). Units + raw JSONB persist to the durable local-only
`natstat_lineups` / `natstat_lineup_games` tables (migration 037) — the fetch is
spent once; compute re-derives from the table.

**[SOURCE SWAP SHIPPED 2026-06-11 — coherence-gated, not blanket.]** The
era-coverage finding (§2) forced a redesign: `compute_pbp_lineups` adopts a
team-game's natstat units only when Σ unit points ≈ box score (±5%) and slot
resolution ≥0.9 (`natstat_covered_team_games` in `compute.rs`); covered sides
skip replay entirely, everything else stays replay/onfloor. Unit possessions
are rescaled to the box-score estimate (defensive via `points-d / dppp`,
rescaled to the opponent's), stint seconds are possession-share estimates (the
object has no clock), and the box-minute validity clamps are skipped for this
source (membership is exact). The corrupt-2019 gate now blocks only replay —
gated natstat team-games still emit, so 2019 gets lineups/on-off once its
backfill lands. Source label `'natstat_lineups'` flows through
`lineup_stints` / `lineup_aggregates` / `player_on_off` (priority
natstat > onfloor > replay, best-source-seen). The original intent below
survives only where the gate passes. This:
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
*(Design doc landed 2026-06-12 — `docs/rapm_methodology.md` — with one
correction to this paragraph: the lineups-object units are NOT RAPM
observations (no opponent pairing, no clock), and the soft-block on Tier 2 is
gone — replay carries both-side 5-man pairing in every season, so the paired
replay/onfloor stints are the corpus. The Tier-2 source swap actually
*removes* paired stints for covered team-games; the doc's `replay_shadow`
decision protects the corpus as the backfill lands. **Resolved same day:
spike REJECTED RAPM as a value metric / ML feature, and it SHIPPED narrowed
as the "Adj on/off (RAPM)" display companion; the trajectory-slot test kept
the raw on/off block (swap decisively worse, add noise). Tier 3 is closed —
doc §8.**)*

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
2. **`lineups`-object historical backfill — RUNNING since 2026-06-10.** The
   ingest/parse/resolve path shipped as `cstat-ingest lineups --year YYYY`
   (restart-safe: the `natstat_lineup_games` ledger is the done-set, per-game
   errors are recorded and skipped, rate-limit errors abort the sweep cleanly).
   Sweep order **reordered 2026-06-10 to 2024→2015 first, then 2025/2026** —
   the coverage finding (§2) makes the middle era the payoff, so finding the
   coherent-era boundary beats finishing sparse 2025; **2019 is included** —
   the lineups object is server-computed and unaffected by 2019's corrupt PBP
   tag CSV. Note on pace: only v3 calls deplete the metered budget, but
   sustained 1500/hr on v4 drew `throttle-level=1` (2026-06-10), so the sweep
   runs at the nominal 500/hr with `throttle-level` warnings and a
   429/OUT_OF_CALLS abort as the guardrails.

So: (1) is still the to-schedule item; (2) is built and in flight.

## 7. Recommended sequence

1. **Tier 1** tag features into the model — cron-safe by construction, no lineup
   work, measurable lift. Do first.
2. Decide **Tier 2**: commit to the `lineups`-object ingest as the cross-season
   lineup source (recommended), then run the historical backfill in the
   background. *(Done 2026-06-10/11 — ingest shipped, backfill in flight, and
   the compute-side source swap shipped coherence-gated per team-game.)*
3. Wire the **nightly capture** cron (onfloor + tags) so no future data is lost.
4. **Tier 3 (RAPM)** once Tier 2's exact lineups exist.

The on/off and top-lineup surfaces shipped this week stay as **site display**
(now much improved for 2026) and become *ML* inputs only after the Tier-2 source
decision.

**[MEMBERSHIP-FEATURES BACKTEST 2026-06-11 — split verdict.]** The first
membership-derived ML pass ran on the current (mostly replay/onfloor) stints,
after a `player_on_off` data-quality fix (≥10-possession floor on OFF rates —
`nullif(x, 0)` passed float-residual possession sums and minted ~1e16 ratings;
plus an 11-season recompute flushing pre-gate stale rows). **Team-level
`lineup_quality` → game models: REJECTED** (`experiment_game_lineups.py`; +3
expanding diffs — possession HHI / top-lineup share / top-lineup net — degraded
all 7 holdout metrics, margin MAE +0.030, while ranking 10–15/52 by importance:
the Tier-1 importance-is-not-value trap again). **Player-level prior-season
on/off → trajectory: POSITIVE** (`experiment_trajectory_onoff.py`; +3 features
— `prior_on_net_rtg` / `prior_net_on_off` / `prior_on_poss_share` — covered
LOPO MAE −0.011, 9/11 pairs, all class buckets improve — the first positive
PBP-feature verdict; shipping it is a 48→51 trajectory-contract change, its own
PR if accepted). Full numbers:
`training/eval_history/tier2_membership_models_20260611_summary.json`. The
split sharpens Tier 3's prior: player-level membership signal is real where
team-level continuity summaries are absorbed — RAPM (player-level by
construction) stays the principled play; re-run the game-side harness only if
natstat exact coverage materially upgrades stint quality.

**[TRAJECTORY ON/OFF ACCEPTED + SHIPPED 2026-06-11.]** The positive verdict
shipped end-to-end as the 48→51 trajectory contract change: the three on/off
features are native to `train_trajectory_model.py` (the experiment harness now
ablates them from production instead of re-adding), the Rust contract
(`TRAJECTORY_FEATURE_NAMES`, `TrajectoryPlayerRow`, both fetch queries,
sentinel fill) carries the `player_on_off` join, and the full downstream chain
regenerated: OOF → roster-impact retrain → 11-season backtest dump (pooled
roster_proj MAE 6.04 → 5.96) → CAE (guards pass) → `compute-projections`.
Production retrain reproduced the accept exactly (pooled LOPO 2.133 → 2.127;
`prior_on_net_rtg` 3rd of 51 by importance). Details in the ROADMAP ship
bullet under P-onfloor-4. Tier-3 RAPM is now the next membership item.
