# Play-by-Play: Ingestion & Utilization Design

This document is the design of record for ingesting NatStat play-by-play (PBP) data and the derived surfaces it unlocks. It supersedes the PBP sketch in `ROADMAP.md` (Refactor Backlog "Play-by-play ingestion" and Phase-6 "PBP-derived features"), which was written from the API JSON shape and is corrected here against the real CSV export.

PBP is the first data source cstat ingests that has **no box-score equivalent** — lineups, on/off splits, shot-context, foul-drawn, and assist networks exist nowhere else. It is also the largest by row count (~3.35M rows / season) and the first that is deliberately **not** shipped to prod in raw form.

## Core principle: one normalized table, two loaders, one derivation

The design is built so backfill and intra-season ingestion converge immediately:

```
   CSV (backfill)  ─┐
                    ├─► play_by_play (raw, local-only) ─► compute_pbp_aggregates ─► derived
   API (intra-day) ─┘                                       (source-agnostic)        (ship to prod)
```

- **Two loaders** (CSV bulk, API incremental) normalize into the **same** `play_by_play` rows.
- **One derivation step** (`compute_pbp_aggregates`) reads only the raw table and never knows which loader produced it.
- Lineups are reconstructed by **SUB-replay**, which works identically for both sources. We do **not** depend on the API's pre-computed `onfloorhome`/`onfloorvis` (see below) — it becomes a free validation oracle instead.

This is the load-bearing decision. Everything else follows from it.

## Ground truth: the CSV export (verified 2026-06-05)

Verified against `data/natstat_csv/2026/NatStat-MBB2026-Play-by-Play-2026-05-17-h15.csv` (692 MB, 3,349,635 rows, 6,281 games).

CSV columns:

```
GameDay, GameID, Sort, Period, Time, Visitor, VisitorID, ScoreVis,
Home, HomeID, ScoreHome, Team, TeamID, TeamAbbrev, OppID, Opp, Diff,
PlayerID, Description, ScoringPlay, Points, Distance, Tags, FieldHome, FieldVis
```

| Column | Meaning | Used as |
|--------|---------|---------|
| `GameID` | NatStat game id | join → `games.natstat_id` → `games.id` |
| `Sort` | period-sequence key e.g. `1-0060` (**not unique per row** — collides across same-instant events) | `sort_order` (reference, not a key) |
| `Period` | 1, 2, then 3+ for OT | `period` |
| `Time` | game clock e.g. `19:59.59` | `clock` |
| `Team`/`TeamID` | acting team | `team_id` |
| `PlayerID` | acting player (blank for non-player events) | `player_id` (nullable) |
| `Description` | human text e.g. `"Kody Clouet made 3-point jump shot"` | `description` |
| `ScoringPlay` | non-empty on made baskets | `scoring_play` |
| `Points` | points on the play | `points` |
| `Tags` | `\|`-delimited event tags (see below) | `tags TEXT[]` |
| `Diff` | score diff from acting team's POV | `score_diff` |
| `ScoreHome`/`ScoreVis` | running score | `score_home`/`score_vis` |

**Three corrections to the ROADMAP's API-derived assumptions:**

1. **`FieldHome`/`FieldVis` are 0% populated.** These are the would-be on-floor lineup columns. The ROADMAP claimed `onfloorhome`/`onfloorvis` are present on every row "so no SUB replay is needed" — that is true of the **API JSON only**, not the CSV. Since the CSV is the backfill path, lineups **must** be reconstructed by SUB-replay.
2. **`Distance` is dead** — always `0`, even on 3-pointers (verified across 500k FGA rows). Do **not** include a `distance` column.
3. Otherwise the data is rich and clean: `ScoringPlay`/`Points` are reliable, and the tag vocabulary carries everything else.

### Tag vocabulary (densities from first 600k rows)

`SUB` (155k) · `REB` (85k) · `FGA` (77k) · `paint` (63k) · `3FA` (47k) · `FTA` (45k) · `FGM` (40k) · `PF` (39k) · `FOULED` (38k) · `FTM` (33k) · `offto` (32k) · `AST` (30k) · `TO` (28k) · `2ch` (26k) · `ORB` (24k) · `brk` (19k) · `3FM` (17k) · `STL` (16k) · `TIMEOUT` (13k) · `MISC` (9k) · `BLK` (7k) · `block` (7k) · `TREB`/`TRB` (team rebounds) · `JUMP`.

`SUB` is the **densest** tag — substitution data is plentiful and clean (`"Name sub in"` / `"Name sub out"`, each with `PlayerID` + `TeamID`). Everything except lineups is derivable directly from tags with no replay.

## Schema (migration `029_play_by_play.sql`)

```sql
CREATE TABLE IF NOT EXISTS play_by_play (
    game_id     UUID NOT NULL REFERENCES games(id),
    season      INT  NOT NULL,
    seq         INT  NOT NULL,          -- dense 0..N ingest order within the game (row identity)
    sort_order  TEXT,                   -- NatStat "Sort" (e.g. "1-0060"); reference only, not unique
    period      INT  NOT NULL,
    clock       TEXT,                   -- "Time", e.g. "19:59.59"
    team_id     UUID REFERENCES teams(id),
    player_id   UUID REFERENCES players(id),  -- nullable: non-player events
    description TEXT,
    scoring_play BOOLEAN NOT NULL DEFAULT false,
    points      INT  NOT NULL DEFAULT 0,
    tags        TEXT[] NOT NULL DEFAULT '{}',
    score_home  INT,
    score_vis   INT,
    score_diff  INT,
    PRIMARY KEY (game_id, seq)
);
CREATE INDEX idx_pbp_game ON play_by_play (game_id, seq);
```

Notes:
- **`Sort` is not row-unique** (multiple events share `1-0060`), so we assign a synthetic dense `seq` in source order. `seq` is the row identity; `sort_order` is kept for debugging/ordering parity with NatStat.
- **No `distance`** column (dead in source).
- **Idempotency unit is the game.** PBP for a finished game is immutable, so re-ingest is `DELETE FROM play_by_play WHERE game_id = $1` then bulk insert. This sidesteps needing a natural per-row unique key and makes both loaders trivially re-runnable. Intra-season only fetches finished (yesterday's) games, so there is no in-progress-game mutation case to handle.
- Raw `play_by_play` is **local-only** (see Storage). Minimal indexing — only the `(game_id, seq)` PK/index, since every consumer is a full-game scan during derivation.

## Loaders

Both resolve identifiers exactly as the box-score path does: `GameID → games.natstat_id`, `PlayerID`/`TeamID` → season-scoped UUIDs via the existing resolvers. A PBP row whose game isn't ingested yet is skipped (same as `upsert_player_game_stats`).

### CSV loader (backfill)

- Lives behind a `--with-pbp` opt-in flag on `bootstrap-csv` (off by default — PBP is heavy and most backfills don't want it).
- Reads `data/natstat_csv/YYYY/NatStat-MBB{YYYY}-Play-by-Play-*.csv`, groups rows by `GameID`, and for each game does delete-then-bulk-insert. Use `COPY`/binary bulk insert; a season loads in well under a minute.
- **CSVs on disk today:** 2015–2020 and 2026. **2021–2025 are missing** and need a dashboard export (or the API path) before they can be backfilled.

### API loader (intra-season)

- `cstat-ingest play-by-play { --year, --date, --from/--to, --gamecode }` subcommand, plus an `ingest/playbyplay.rs` module. There is intentionally **no full-season default** (that's a ~6,700-call sweep — use `bootstrap-csv --with-pbp` for backfill).
- Endpoint `/playbyplay/mbb/{range}` (alias `/pbp`), **500 results/page**, range params include `date`, `daterange`, `gamecode`, `season`.

#### The pagination must be scope-aware (verified 2026-06-06)

**NatStat only honors the `range` filter on page 1.** Past offset 0, a `gamecode` query silently returns the **global season play-by-play stream** — verified: offset 23000 of gamecode 1511104 came back as a *different* game (1510339). A naive "paginate until the response is empty" loop (`get_all_pages`) therefore never terminates on a gamecode query and runs away through all ~6,700 pages of the season. (One test run reached offset 24,000 / 49 requests before being killed. Budget note: NatStat did *not* decrement `ratelimit-remaining` for these — it stayed pinned at 500 — so the runaway cost work, not quota, but it must not ship.)

The loader therefore does **not** use `get_all_pages`. It paginates scope-aware (`ingest_pbp_scoped`): it computes the in-scope game-code set up front (for a date: the games on that date from our own `games` table; for a gamecode: just that code) and keeps only in-scope plays. Termination is the subtle part — a page with zero in-scope plays is *not* on its own a stop signal (a date legitimately includes non-D1 games we don't ingest, which can fill a whole page *between* two of our games). The rules:
- **empty / `NO_DATA` page** → real end (the `date`/`daterange` filter composes, so this is the normal terminator);
- **every in-scope game seen AND current page has none** → we have everything, the rest is out-of-scope tail (this is what trips a multi-page `gamecode` once its game ends, bounding the runaway);
- **single-target query (`gamecode` / one-game date) whose target never appeared by its first in-scope-empty page** → it has no PBP (postponed / feed gap / bad code); stop rather than walk the season;
- **`MAX_PAGES` backstop** as a last resort.

This bounds any query to ~1 page past its real end regardless of the filter quirk, never drops an interleaved game, and never writes a game we didn't ask for.

- **The `date`/`daterange` filters *do* compose with offset** (like `playerperfs`), so for the nightly path the scope check is mostly a safety belt; for `gamecode` it's load-bearing. Verified live: `--date 2025-12-27` (3 games) collected all 3, 1,690 rows, in **4 API calls**, 0 skipped — i.e. ~1 call / 500 plays, which extrapolates to ~250 calls for a 200-game day. The nightly slate is comfortably inside the 500/hr budget.
- **Single games >500 plays cannot be fetched by `gamecode` alone** (page 2 falls into the global stream); the scope filter still recovers the right game (it spans pages 1–2 before the filter breaks), but `--gamecode` is really a debugging affordance — the nightly job uses `--date`.

#### Verified API JSON shape (spot-check 2026-06-05, gamecode 1511104)

The API response is **deeply nested** and shaped differently from the flat CSV — same data, different layout. Data lives under the top-level `playbyplay` key; each play is an object. Field mapping into the normalized row:

| Normalized column | CSV column | API JSON path | Notes |
|---|---|---|---|
| game natstat id | `GameID` | `game.code` | |
| `sort_order` | `Sort` | `game.sequence` | collides (295 distinct / 500 plays) → synthetic `seq` still required |
| `period` | `Period` | `game.period` | |
| `clock` | `Time` | `game.time` | **empty `{}`** on some rows → null |
| `team_id` (acting) | `TeamID` | `team.code` | |
| `player_id` (acting) | `PlayerID` | `players.primary.code` | object absent on team events → null |
| `description` | `Description` | `explanation` | **empty `{}`** on some rows → null |
| `points` | `Points` | **no field** — derive from tags (`FTM`=1, `FGM`&!`3FM`=2, `3FM`=3) | API↔CSV difference |
| `scoring_play` | (`ScoringPlay`) | (`scoringplay`) | **derived as `points > 0` in both loaders**, not read from the source flag — the CSV `ScoringPlay` column omits made free throws, so the source flags disagree |
| `tags` | `Tags` | `tags` (same `\|` vocabulary) | |
| `score_home`/`score_vis` | `ScoreHome`/`ScoreVis` | `game.score-home`/`game.score-vis` | |
| `score_diff` | `Diff` | `thediff` (e.g. `"+5"`) | acting-team POV |
| (`seq` sort key) | (file order) | `id` (top-level, numeric) | API plays arrive in arbitrary map order; sorted by `id` ascending to assign chronological `seq`. Verified present + numeric on 100% of plays. |
| on-floor lineup | (absent) | `game.onfloorhome`/`game.onfloorvis` | **present & per-play in the API** (26 distinct lineups / 500 plays) — the SUB-replay oracle |

Two loader wrinkles the spot-check pinned down: (1) the API carries **no explicit `points`** — derive it; (2) `time`/`explanation` deserialize as empty objects on non-action rows — coerce to null. Both normalize cleanly into the schema above. The raw table stays source-identical (we do **not** add an `onfloor` column) — the API lineup is consumed only as the test-time replay oracle, keeping a single derivation path.

#### Intra-season fetch is "yesterday's games only"

We never re-pull a season. The nightly job fetches **only the prior day's finished games**:

- `/playbyplay/mbb/{yesterday-date}` returns just that date's plays.
- Volume: 50–200 games/day × ~530 rows/game ≈ 27k–106k rows ≈ **55–215 API calls/day** at 500/page — a small fraction of the 500/hr standard budget.
- **Crafty-fetch headroom (TBD, not in v1):** because the unit of idempotency is the game, the fetch can be narrowed further — e.g. only games whose `games` row was updated yesterday, by-gamecode fetches for a known schedule, or skipping games already present in `play_by_play`. v1 ships the simple by-date pull; the lighter variants are an optimization once the pipeline is proven.

## Derivation: `compute_pbp_aggregates` (P2a) + `compute_pbp_lineups` (P2b)

Two compute steps run after `compute_player_season_stats` and before the team/CamPom steps (steps 5 and 6 of 15), source-agnostic, operating on whichever games are in `play_by_play`. Both no-op for seasons with no PBP loaded.

### Lineups & stints — P2b, SHIPPED 2026-06-06 (hybrid sourcing)

`compute.rs::compute_pbp_lineups` (compute step 6/15) produces, per game, a set of **stints** (contiguous windows where both teams' on-floor fives were constant) and from them `lineup_stints`, `lineup_aggregates`, and per-player `plus_minus_pbp`. The engine lives in `crates/cstat-core/src/pbp_replay.rs` (pure + unit-tested).

**Hybrid sourcing** (the accuracy/cost tradeoff resolved with the user, 2026-06-06; third source added 2026-06-11):
- **Captured NatStat units (`natstat_lineups`)** — when a team-game's captured `lineups`-object units pass the **coherence gate** (`compute.rs::natstat_covered_team_games`: Σ unit points within ±5% of the box score AND slot resolution ≥0.9), that side's stints are emitted directly from the durable `natstat_lineups` table — exact membership, server-computed, one synthetic per-game stint row per unit (`period 0`, no clock; `opp_lineup` empty). The feed's `possessions` is not our possession unit (~55–66% of the box estimate even on coherent games), so each team-game's unit possessions are **rescaled to the box-score estimate** (defensive via `points-d / dppp`, rescaled to the opponent's estimate) and `seconds` is a possession-share estimate of team box minutes. The gate exists because the object is **era-incomplete** (2020 ~91% points coverage / 78% pass; 2024 ~65% / ~7.5% pass; 2025–26 ~38%; 2015 over-counts ~109%) — see `docs/pbp_utilization_scope.md` §2. The box-minute validity clamps are skipped for this source (membership exact, seconds estimated).
- **Exact (`onfloor`)** — when a game's `play_by_play` rows carry the stored API `onfloorhome`/`onfloorvis` (nightly-forward; non-backfillable), the stint is built from them by **union reconstruction** (see below) — NOT a literal per-play read, because the source field is incomplete on ~half of plays.
- **Replay (`replay`)** — otherwise (all CSV-loaded seasons today), SUB-replay reconstructs the fives: seed from `player_game_stats.starter`, mutate a live 5-man set on each `SUB` row (recovering the ~4% null-`player_id` subs by matching the description name to the game roster), and attribute each non-sub play to the current lineup.
- Per team-game the loader picks `natstat_lineups` if gated, else `onfloor` if any row has it, else `replay` — a side is exactly one source (covered sides skip replay emission entirely). `lineup_aggregates.source` records the best source seen (so the UI can flag approximate data). The corrupt-PBP gate (2019) blocks only the replay path; gated natstat team-games still emit.

**On-floor union reconstruction (`pbp_replay.rs::stints_from_onfloor_rows`, 2026-06-08).** NatStat's `onfloorhome`/`onfloorvis` are populated on ~99% of plays but **per-play incomplete**: across 2026, the home field lists only **four** of the five players on ~45% of plays (1.42M) and **three** on ~8% (0.30M), with exactly five on the other ~45% (raw counts verified on `play_by_play`). Keying a stint on the exact per-play set — the original behavior — therefore shattered every real stint into 4-man micro-fragments, and the downstream **5-man-only** rollup (`lineup_aggregates`, `plus_minus_pbp`, `player_on_off`) silently discarded them: 2026 5-man lineup-time coverage was only **~45%**, halving floor minutes and gutting the bench/OFF sample on/off needs. The fix unions the per-play sets within a run of plays — over a single true stint the union converges to exactly the five on the floor — and treats *the first play whose running union would exceed five* as the substitution boundary (the only point a sixth distinct player can appear; either team subbing ends the joint stint). Impact on 2026: 5-man lineup-time coverage **45% → 85%**, stint rows **1.66M → 615k** (fragments coalesced into real stints), and per-player on-floor minutes corrected (e.g. Arkansas's Darius Acuff 17.9 → 31.2 min/game). **Limitation:** a stint whose fifth player is never reported (a deep-bench player who never touches the ball during a short garbage-time run) stays a 4-man stint and is still excluded by the 5-man rollup — this disproportionately thins the OFF sample of the highest-minute *starters* (their rare bench minutes fall in exactly those sub-5-man stints). Unit-tested in `pbp_replay.rs` (flicker → one five; sixth player → split with chained deltas).

**Measured replay accuracy: ~85.6%** exact 5-on-5 lineup match vs the API `onfloorhome` oracle (the gated integration test `tests/pbp_replay_oracle.rs`). The ceiling is feed sub-pairing quality, not seeding/resolution — naive self-heal (add any acting player) *hurts* (balloons the set past 5), so it's deliberately omitted. Off-5 drift stints are kept in the local `lineup_stints` but **excluded from `lineup_aggregates`** (5-man only); the served aggregates capture ~97% of team points and reconcile exactly to actual scoring on the contiguous stint sum.

**Score deltas** chain off the authoritative running score (`score_home`/`score_vis`), carried forward with `max` — some event rows ("media timeout", "End of period") report score `0`, which must not reset the running total (that bug produced 137%-over-counted points before the fix).

Outputs:
- **`lineup_stints`** (local-only) — one row per team per stint: `(game_id, season, period, start_seq, end_seq, team_id, lineup UUID[], opp_lineup UUID[], points_for, points_against, source)`.
- **`lineup_aggregates`** (ships to prod) — season rollup per `(season, team_id, 5-man lineup)`: `stint_count, points_for, points_against, plus_minus, source`.
- **`player_game_stats.plus_minus_pbp`** (ships to prod) — each player's summed **5-man** on-floor stint differential. Gated to 5-man stints (like the aggregates) so it reconciles with `lineup_aggregates` and isn't poisoned by off-5 replay-drift windows, where the on-floor membership is wrong.

**Serving note:** `lineup_aggregates.lineup` is a `UUID[]`; Postgres can't FK array elements, so a player UUID in a lineup has no referential guarantee. Serving queries must `LEFT JOIN unnest(lineup)` to `players` (not `INNER JOIN`), or a lineup with one unresolved member would silently vanish.

**Performance:** the derivation bulk-loads all per-game metadata (teams, starters, name maps, code→UUID) in 4 queries up front, leaving only a per-game PK-indexed plays query, then replays in memory — ~40 s for a full 6,108-game season.

**Validation oracle:** when the source is `onfloor` (API JSON carries `onfloorhome`/`onfloorvis`), the replayed lineup can be asserted against the embedded one — this is exactly the 85.6% measurement above, and it never makes the derivation *depend* on a column the CSV lacks.

### Tag-based per-`(player_id, game_id)` columns (no replay) — P2a, SHIPPED 2026-06-06

Added to `player_game_stats` (migration `030`, additive, ships to prod) by `compute.rs::compute_pbp_aggregates` (compute step 5/15). All counts are over **source-deduplicated** plays (`DISTINCT ON (game_id, sort_order, description, player_id)`):
- `paint_fga`/`paint_fgm`, `perimeter_fga`/`perimeter_fgm` — `paint` tag presence on FGA/3FA (attempts) and FGM/3FM (makes); perimeter = total − paint.
- `transition_pts` (`brk`), `second_chance_pts` (`2ch`), `points_off_turnovers` (`offto`) — summed `points` on the player's tagged scoring plays.
- `fouls_drawn` — `FOULED` event count for the player (who DREW the foul, distinct from who shot the FTs).
- Semantics: NULL = no PBP for that player-game; 0 = had PBP, none of this event. Verified on 2026: 111,196 player-games populated; spot-checks textbook (a 7-ft center 97% paint / 242 fouls drawn; a perimeter wing 5% paint).

`plus_minus_pbp` ships in P2b (above, from stint deltas). Optional later: `assist_edges (season, passer_id, scorer_id, count)` linking `AST` → the `FGM` it set up.

### Possession & tempo normalization — SHIPPED 2026-06-07 (migration `033`)

P2b's stints/aggregates shipped **raw-count only** (`points_for/against`, `plus_minus`), with no denominator — a 200-possession lineup and a 5-possession lineup summed into the same +/-, conflating quality with floor-time. P3 adds the tempo-free unit so the served lineups carry **per-100 offensive/defensive ratings**, the on-floor **minutes**, and **possessions** (and unblocks the on/off + `lineup_quality` features downstream).

**Possession formula = `(FGA + 3FA) − ORB + TOV + 0.44·FTA`** — the exact convention `compute_adjusted_efficiency` uses for tempo / AdjO / AdjD (the **0.44** FTA coefficient, *not* 0.475), so lineup ortg/drtg land on the same scale as team AdjO/AdjD. Counted per side from `play_by_play.tags`, attributed to the acting `team_id`, over each stint's `[start_seq, end_seq]` window in one merge-walk (`pbp_replay.rs::stint_metrics`). `possessions_for` = the stint team's events; `possessions_against` = the opponent's. Stored `DOUBLE PRECISION` to keep the fractional 0.44·FTA term across rollup. The season rollup emits `ortg = 100·PF/poss_for`, `drtg = 100·PA/poss_against`, `net_rtg`, and `minutes` (NULL-guarded division). **Validated** by `tests/pbp_possession_parity.rs` (`#[ignore]`, DB-gated): per-team-game stint possession sum vs the box-score estimate, **1.9–7.8% MAE across 2015–2026** (slight undercount from off-five drift + NULL-team marker rows, expected), gate at 8%.

**Three feed-vintage gotchas** the counting must absorb (each silently wrong before the parity/minutes checks caught it; all now unit-tested):
1. **`FGA` is 2-point attempts only** — `3FA` is mutually exclusive (0 co-occurrences across all seasons), so total FGA = `FGA + 3FA`. Counting only `FGA` drops every three-point attempt.
2. **Turnover tag changed vintage** — `TOV` in the 2015–2018 feeds, `TO` from ~2019 on (2019 mixes both, non-overlapping). Count `TO || TOV`; omitting legacy `TOV` undercounted 2015–2018 by ~13 turnovers/team-game (26% → 4–5% MAE).
3. **Clock delimiter drifts** — `MM:SS` (2015–2018), `MM:SS:hh` colon-hundredths (2019–2024), `MM:SS.hh` dot-hundredths (2025–2026). `parse_clock` splits on both `:` and `.` and takes the first two fields as minutes/seconds (no college clock exceeds 20:00, so field 1 is always minutes). Handling only two forms silently zeroed `minutes` for 2020–2024. Stint duration = sum of positive clock decrements within the window (a period reset inside one stint self-cancels by dropping that one break interval); slightly under-counts wall-clock (~32–43 min/team-game vs the ~40 ideal), which is a harmless sanity-signal, not a load-bearing number.

**Box-minute clamp (SUB-replay drift mitigation).** Replay drift — a missed sub-out stretches a stint past a player's real exit — can attribute more on-floor time to a 5-man unit than physically possible, surfacing **phantom lineups** (canonical case: Duke 2025's #2 "most-used" lineup claimed 121 min, but four of its five are ~14–16 MPG bench players and per-game its minutes exceeded the least-playing member's box minutes). A lineup can't have been on the floor longer than its least-playing member played all game, so `compute_pbp_lineups` materializes a per-`(game, team, lineup)` temp rollup with a `valid` flag — invalid when summed minutes exceed `min(box minutes of its five) + 1.0` (tolerance for box-minute integer rounding; COALESCE to a huge ceiling when box minutes are unknown, so only *provably*-impossible rows drop). **0-minute box rows are treated as unknown, not as a 0-minute ceiling** (`pgs.minutes > 0`, 2026-06-08): NatStat occasionally records a player with 0 box minutes who still appears in the on-floor data (~1.1k player-games in 2026, sometimes even flagged `starter=t`) — a box-side artifact; without this guard one such member collapses the ceiling to 1.0 and wrongly nukes an otherwise-valid bench lineup (it was the single biggest false-positive source on `onfloor` data — ~20% of clamp-killed minutes). Excluding the 0/NULL member still constrains the lineup by its positive-minute members, so a genuine over-merge is unaffected. Both served surfaces (`lineup_aggregates`, `plus_minus_pbp`) read only valid game-lineups, so they reconcile; raw `lineup_stints` is **untouched** (a serving filter, not a delete), so the parity test is unaffected. (Note: the on/off rollup no longer uses this clamp — it reads all stints with a per-player box-minute cap instead; see "On/off splits".) On Duke 2025 the phantom fell from 121 min/#2 → 39 min/#4; real lineups unchanged. This is *hygiene, not a cure* — it removes provably-impossible game-lineups but not subtler membership drift (uneven by season: 1–4% of lineup-game rows impossible for 2015–2024, **11% for 2025**), so the `replay`-source caveat stays on the UI and the real fix is the onfloor ingest (ROADMAP). **Scope of contamination**: only the replay-derived lineup surfaces (`lineup_aggregates`, `plus_minus_pbp`); the P2a per-player tag aggregates are a direct `player_id` rollup with no replay and are clean.

**2019 PBP source corruption + auto-detect gate.** NatStat's **2019** Play-by-Play CSV mis-encodes made field goals as free throws (the raw "John Petty Jr. made layup" row carries `Points=1` + `Tags=FTA|FTM` instead of `Points=2` + `FGA|FGM`; the 2020 export tags the identical play correctly). This halves 2019's FGA/FGM, inflates FTA, and runs its possessions ~36% low — corrupting *all* 2019 PBP-derived surfaces (box scores, a separate CSV, are fine). Not a parser bug; the source columns hold wrong values, and no clean re-fetch exists (the API returns `NO_DATA` for 2019 PBP). Suppressed via `compute.rs::pbp_source_is_corrupt`: a season whose PBP `FGA`+`3FA` tag count covers < `PBP_MIN_FGA_COVERAGE = 0.80` of box FGA (2019 ≈ 0.55; every clean season > 0.93) has its derived surfaces **cleared and skipped** in both `compute_pbp_aggregates` and `compute_pbp_lineups`, so the UI hides them like a pre-PBP season. Chosen over a hardcoded year list because it's a measurable signal that also catches a partial in-season load or any future feed regression; the parity test asserts both good-season MAE and that corrupt seasons stay cleared.

**Pre-2020 contextual-tag absence + second gate (found 2026-06-09).** The 2015–2018 feeds carry **only box-event tags** (`FGA`/`FGM`/`3FA`/`REB`/`TOV`/`SUB`/…) — zero `paint`/`brk`/`2ch`/`offto`/`FOULED` vocabulary (paint share of tagged FGA: 0.000 for 2015–2018 vs ≥ 0.41 for every 2020+ season; the signal is binary). The FGA-coverage corrupt gate passes those seasons (their box-event tags are fine), so deriving the tag aggregates from them produced misleading zeros — every shot counted "perimeter", 0 transition points, 0 fouls drawn — which had been served on the 2015–2018 player PBP panels. Suppressed via a second gate, `compute.rs::pbp_lacks_context_tags` (paint share of tagged FGA < `PBP_MIN_PAINT_TAG_COVERAGE = 0.05` → clear + skip the tag-derived aggregates only). The lineup/possession path is **not** gated by this — it reads box-event tags + SUBs, which every vintage carries (possession parity holds 1.9–7.8% MAE for 2015–2018). Effective tag-aggregate coverage: **2020–2026** (2019 separately corrupt-gated). The parity test asserts both directions: pre-context-tag seasons cleared, contextual-era seasons populated.

### On/off splits — SHIPPED 2026-06-07 (migration `034`)

The headline player-value surface (KenPom / EvanMiya style): a team's offense and defense **per 100 possessions with a player on the floor vs on the bench**. Computed inside `compute_pbp_lineups`, rolled up to a season `player_on_off` row — a prod-resident table, since the per-stint `lineup_stints` it derives from stays local-only.

**Source set (2026-06-08): ALL stints, not the 5-man-clamped lineups.** Unlike the top-lineup aggregates, on/off attributes by individual *presence*, so it reads **every reconstructed stint regardless of lineup size** — a 3/4-man stint is valid evidence for whether a *known* player was on the floor. This is deliberately *not* the validity-clamped `_game_lineups` set: requiring a clean 5-man unit erased the bench (OFF) windows of high-minute starters, because ~19% of 2026 onfloor player codes don't resolve to a roster row (garbage-time deep-bench / walk-ons absent from NatStat's DB), and when one is the unresolved 5th the stint stays sub-5-man and the 5-man rollup drops it. The `'onfloor'`/`'replay'` source flag is still recorded per player.

- **ON** = the player's own valid on-floor stints (he is one of the five): `sum` over `unnest(lineup)` of points/possessions/seconds.
- **OFF** = his team's *remaining* stints in the **same games** — team totals minus his (clamped, see below) ON totals — **restricted to games he actually appeared in**. Restricting to games-played isolates rotation (bench minutes) from availability (DNPs / injuries); a game he never played contributes to neither side. A player who never sits has zero off-court possessions, so his off rates are NULL (the UI renders "—").
- **Per-player box-minute clamp** (replaces the lineup clamp for this surface): the onfloor reconstruction *over-credits* a high-minute player's ON time — his brief rests fall in sparse-onfloor windows where the data never registers him leaving (an iron-man reads ~94% on-floor by stints vs ~86% by box). So each player's per-game ON accumulators are capped at his box minutes (`sc = LEAST(1.0, box/on)`, scale **down** only), and the excess flows to OFF. This recovers an honest off-court sample for exactly the starters raw on/off failed — **Arkansas's Darius Acuff (2026, 34.7 min/game): OFF 11 → 271 possessions, swing −201 → −13**. We never scale up (when the reconstruction under-credits we can't tell which possessions were his — leave them, the conservative direction); a player with no positive box-minute row that game is left unscaled. Season-wide impact: rows with a healthy (≥100-poss) OFF sample rose 92% → 96%, and only ~2% of players' ON minutes still exceed box by >2/game.
- **Rates**: `ortg = 100·PF/poss_for`, `drtg = 100·PA/poss_against`, on the same per-100 scale as team AdjO/AdjD and the lineup rates. `net_on_off = on_net_rtg − off_net_rtg` — the on/off swing. **OFF rates carry a ≥10-possession floor, not a `nullif(x, 0)` guard** (2026-06-11): per-game possession estimates have ~±1–2 noise, so an iron-man's OFF possessions can sum to a float residual (~1e-13) while his integer OFF points stay nonzero — `nullif` passes the residual and the division mints a ~1e16 rating. Below 10 possessions the season sum is indistinguishable from zero, so the OFF rates and `net_on_off` go NULL (the honest no-off-court-sample outcome; the ON side needs no floor — the ≥100-possession ON gate covers it).
- **Own-team credit**: a player is credited only for stints whose lineup team matches his canonical `players.team_id`. The replay/onfloor resolution occasionally maps two same-named players on different teams to one UUID (so a UUID leaks into another team's lineup arrays — see ROADMAP "Cross-team player attribution in PBP lineups"); keying on the box-score-authoritative team drops those phantoms, makes `(season, player_id)` unique (enforced by migration `035`), and guarantees a player's on/off is never another team's. A **minimum-ON-sample gate (`on_posf >= 100 AND on_posa >= 100`)** also applies: on/off is only meaningful for a real rotation player, and since the all-stints source gives every player who logged a few minutes a tiny ON slice, a lower gate let garbage-time benchwarmers through with ±600-per-100 noise ratings that topped the swing rankings. The 100-possession floor (matching the panel's OFF small-sample threshold) drops only sub-~3-min/game players — a 9-min/game role player clears it easily — and cut 2026 extreme-swing rows (|swing|>60) from 211 to 1. The UI hides the panel/column for a dropped player.
- **Math identity (verified)**: on and off are a possession-weighted split of the team's net rating over the player's games — reconstructing `100·(on+off PF)/(on+off poss) − …` gives the **same** team net for every full-season player (e.g. St. John's 2026 → +16.0 across the rotation). on/off does **not** sum to zero across a team (no partition identity; St. John's possession-weighted mean swing ≈ −2.4), and it is a *contextual team-result* stat — a high-CamPom star on a deep team can read negative because the bench pads its rating in garbage time (Zuby Ejiofor 2026: CamPom +20.8, on/off −8.4). The principled fix is adjusted +/- (RAPM) — ROADMAP.
- **Caveats inherited from the lineups**: any `replay`-sourced split (~86% accurate) carries the SUB-replay drift, so the UI flags it; and the off-court sample is thin for heavy-minute starters (the panel surfaces the off possession count and warns under ~100). Validated on Duke 2025: Flagg on net +25.3 / off +18.1 (swing +7.2), Proctor +16.5, and Caleb Foster −29.0 (Duke's lineups were elite when the struggling guard sat, 1336-poss off sample) — all reconcile with Duke's ~+25 season net. Served at `GET /api/players/:id/on-off` (panel on `PlayerDetail.tsx`) and as a sortable column on four ranking surfaces — the `TeamDetail.tsx` roster, the Players list, and the Transfers grid (the latter shows the transfer's *source-season* on/off at their old school) — each a one-`LEFT JOIN` add keyed on the player's canonical `team_id`, with shared color/tooltip helpers in `web/src/components/onoff.ts`.
- **Residual `onfloor` OFF-rate limitation (2026-06-08).** The all-stints source + per-player box clamp (above) fixed the OFF *coverage* problem — high-minute starters now get a real off-court sample (Acuff 271 OFF poss; 96% of rows ≥100). What remains is an OFF-*rate* fidelity limit for the very highest-minute iron-men: the possessions that played during their rests are mostly in sparse-onfloor / sub-5-man windows, so the clamp recovers the *amount* of their off-court time but the points/possessions moved to OFF are scaled at the player's average on-court rate rather than measured from the actual bench possessions (a uniform-in-time approximation). So an iron-man's OFF *rating* is an estimate, not a direct read — fine for the swing's sign and magnitude, not for a precise off-court ORtg. The principled end-state that needs none of this is adjusted +/- / **RAPM** (controls for teammates/opponents/garbage-time directly) — ROADMAP. Separately, the related `player_game_stats` artifact (a player flagged `starter=t` with 0 recorded minutes, ~1.1k player-games in 2026) is now handled by the 0-minute guard in the **lineup** box-minute clamp (which still gates `lineup_aggregates`).

## Data-quality notes (from P1 verification)

- **NatStat emits occasional duplicate play records.** Verified on 2026-06-06: in game 1482209, two distinct play ids (`116271602`, `116271608`) carry the same `sequence` (`2-1997F1`), same description ("Aidan Cammann made free throw 1 of 2"), both `scoringplay=Y` — i.e. one real free throw recorded twice. These are *not* an ingest artifact (paginated pages share zero play ids — confirmed) and *not* page overlap; they originate in the NatStat feed. P1 stores them faithfully (the raw table mirrors the source).
- **Consequence:** any *count-based* derivation over raw rows (tag-summed points, FT counts, and especially **possession/stint counts in P2**) will be inflated by these dupes. Across a 3-game sample, tag-summed `points` ran +1 vs the final on 2 of 3 games.
- **The authoritative within-game scoring signal is the running score** (`score_home`/`score_vis`), which NatStat does *not* double-count — it matched the final exactly on all 3 verification games. So `points` (a tag-derived convenience, and absent from the API entirely) is ±1-reliable, while running-score deltas are exact.
- **P2 guidance:** `compute_pbp_aggregates` should (a) dedup source-duplicate plays by `(game_id, sort_order, description, player_id)` before counting, and (b) prefer running-score deltas over `sum(points)` wherever a precise figure matters (plus-minus, stint scoring). This is deferred to P2 deliberately — it needs full per-game context and shouldn't bake cleaning into the raw mirror. Open question carried into "Open decisions": dedup at ingest vs. at derivation.

## Storage & prod sync

Per the ROADMAP storage split (unchanged, restated here as the rule):

- **Local Postgres** holds raw `play_by_play` (~500 MB/season; ~7.5 GB for the 2012–2026 PBP-available range) and `lineup_stints`. Used for ML training, ad-hoc analysis, and recomputing aggregates if methodology changes.
- **Railway (prod)** holds only what the live site reads: the additive `player_game_stats` columns, `lineup_aggregates`, and the `player_on_off` rollup. **Never** raw `play_by_play` or `lineup_stints`.
- **`scripts/sync_to_prod.sh` must be patched before the first PBP load.** Its auto-discovery loop would otherwise try to push the multi-GB raw table to Railway and hit the DB cap. Extend `EXCLUDED` (currently `api_cache`, `_sqlx_migrations`) with `play_by_play` and `lineup_stints`, comment each with the why, and print the excluded set in `--dry-run`.

## Pipeline-maintenance surface (cron topology)

PBP adds **one** new daily pipeline, with a hard placement constraint that shapes the cron design:

```
nightly (local/ingest env, NOT Railway):
  cstat-ingest play-by-play --date {yesterday}   # API, scope-aware, ~1 call/500 plays
    → compute_pbp_aggregates over touched games
    → scripts/sync_to_prod.sh   (derived only; raw excluded)
```

The job **cannot run as a pure-prod cron** because raw PBP lives only in the local/ingest environment — Railway has no raw table to write into. This is the key fact to settle before laying out the cron jobs the ROADMAP plans: the PBP nightly belongs with the local ingest crons that sync up to prod, not with any prod-resident scheduled task.

## Build phases

| PR | Scope | Risk |
|----|-------|------|
| **P1** ✅ SHIPPED & VERIFIED (2026-06-06) | Migration `029`; CSV loader (`--with-pbp` / `--pbp-only`) + scope-aware API loader + `play-by-play` subcommand; `sync_to_prod.sh` `EXCLUDED` patched. Verified: full 2026 load = 3,258,166 rows / 100% game coverage / 99.8% running-score-exact / dense seq / box scores untouched. Surfaced & fixed the gamecode-pagination runaway; documented the source-duplicate-play quirk for P2. | Low — mirrored existing ingest patterns. |
| **P2a** ✅ SHIPPED (2026-06-06) | `compute_pbp_aggregates` (compute step 5/14) + migration `030`: tag-based `player_game_stats` columns (paint/perimeter FGA·FGM, transition/2nd-chance/off-TO pts, fouls drawn), over source-deduplicated plays. Verified on 2026 (111,196 player-games). | Low — pure SQL, no replay. |
| **P2b** ✅ SHIPPED (2026-06-06) | Migration `031` + `compute_pbp_lineups` (step 6/15) + `pbp_replay.rs`: hybrid `lineup_stints` (local) / `lineup_aggregates` (prod) / `plus_minus_pbp` from exact API on-floor or ~85.6%-accurate SUB-replay. Oracle-validated; integrity reconciles exactly. Bulk-loaded (~40 s/season). | Highest — landed, with the edge cases (OT, score-0 rows, null-player subs, self-heal) handled & tested. |
| **Possession & tempo norm.** ✅ SHIPPED (2026-06-07) | Migration `033` + `stint_metrics`: per-stint possessions `(FGA+3FA)−ORB+TOV+0.44·FTA` + on-floor seconds → `lineup_aggregates` per-100 `ortg`/`drtg`/`net_rtg` + `minutes`; box-minute clamp drops phantom drift lineups; auto-detect coverage gate suppresses corrupt-source 2019. Parity-validated 1.9–7.8% MAE (ex-2019). See "Possession & tempo normalization" above. | Medium — three feed-vintage gotchas + the clamp, all unit/parity-tested. |
| **Nightly wiring** | Wire API loader + incremental recompute + derived-only sync into the nightly flow. This is the command the cron calls. | Low–medium. |
| **P4+** | Utilization (ROADMAP Phase 6): API endpoints (`/players/:id/on-off` ✅, `/teams/:id/lineups` ✅, `/games/:id/playbyplay`); UI (player on/off ✅, team lineups ✅, game-detail page). **Tag-derived ML features (`transition_rate`, `paint_rate`, …) were backtested 2026-06-10 and REJECTED in all three model families** (see ROADMAP Tier-1 "Models — DONE" + `training/eval_history/tier1_pbp_models_20260610_summary.json`); `lineup_quality` and other membership-derived features remain candidates, gated on the Tier-2 lineup-source lock. | Per-surface. |

P1–P3 are the pipeline that must exist before the cron work; P4+ is value on top.

## Open decisions

- **Pre-PBP era handling for ML** — coverage starts 2012. Train two model variants (PBP-era vs pre-PBP) or impute missing PBP features with season averages. Decide in P4, document in `docs/features_methodology.md`.
- **Crafty intra-season fetch** — v1 is the simple by-date pull; narrowing to changed/missing games only is a post-proof optimization (above).
- **2021–2025 CSV gap** — RESOLVED (2026-06-07): the full historical PBP backfill landed; all 12 seasons (2015–2026) are loaded locally. Two suppression gates apply to the derived surfaces (see the gate paragraphs above): 2019 (corrupt source — all PBP-derived surfaces cleared) and 2015–2018 (contextual tags absent — *tag-derived* aggregates cleared; lineups/possessions/plus-minus unaffected). Raw plays are present for all 12 seasons.
- **Source-duplicate plays: dedup at ingest or at derivation?** RESOLVED (2026-06-06): **dedup at derivation.** P1 stores source rows faithfully; `compute_pbp_aggregates` (P2) dedups by `(game_id, sort_order, description, player_id)` before counting and prefers running-score deltas over `sum(points)`. Keeps the raw table a true mirror of the feed.
