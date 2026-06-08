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

**Hybrid sourcing** (the accuracy/cost tradeoff resolved with the user, 2026-06-06):
- **Exact (`onfloor`)** — when a game's `play_by_play` rows carry the stored API `onfloorhome`/`onfloorvis` (nightly-forward + eventual backfill), the stint is built directly from them. Exact.
- **Replay (`replay`)** — otherwise (all CSV-loaded seasons today), SUB-replay reconstructs the fives: seed from `player_game_stats.starter`, mutate a live 5-man set on each `SUB` row (recovering the ~4% null-`player_id` subs by matching the description name to the game roster), and attribute each non-sub play to the current lineup.
- Per-game the loader picks `onfloor` if any row has it, else `replay`. `lineup_aggregates.source` records which (so the UI can flag approximate data).

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

**Box-minute clamp (SUB-replay drift mitigation).** Replay drift — a missed sub-out stretches a stint past a player's real exit — can attribute more on-floor time to a 5-man unit than physically possible, surfacing **phantom lineups** (canonical case: Duke 2025's #2 "most-used" lineup claimed 121 min, but four of its five are ~14–16 MPG bench players and per-game its minutes exceeded the least-playing member's box minutes). A lineup can't have been on the floor longer than its least-playing member played all game, so `compute_pbp_lineups` materializes a per-`(game, team, lineup)` temp rollup with a `valid` flag — invalid when summed minutes exceed `min(box minutes of its five) + 1.0` (tolerance for box-minute integer rounding; COALESCE to a huge ceiling when box minutes are unknown, so only *provably*-impossible rows drop). Both served surfaces (`lineup_aggregates`, `plus_minus_pbp`) read only valid game-lineups, so they reconcile; raw `lineup_stints` is **untouched** (a serving filter, not a delete), so the parity test is unaffected. On Duke 2025 the phantom fell from 121 min/#2 → 39 min/#4; real lineups unchanged. This is *hygiene, not a cure* — it removes provably-impossible game-lineups but not subtler membership drift (uneven by season: 1–4% of lineup-game rows impossible for 2015–2024, **11% for 2025**), so the `replay`-source caveat stays on the UI and the real fix is the onfloor ingest (ROADMAP). **Scope of contamination**: only the replay-derived lineup surfaces (`lineup_aggregates`, `plus_minus_pbp`); the P2a per-player tag aggregates are a direct `player_id` rollup with no replay and are clean.

**2019 PBP source corruption + auto-detect gate.** NatStat's **2019** Play-by-Play CSV mis-encodes made field goals as free throws (the raw "John Petty Jr. made layup" row carries `Points=1` + `Tags=FTA|FTM` instead of `Points=2` + `FGA|FGM`; the 2020 export tags the identical play correctly). This halves 2019's FGA/FGM, inflates FTA, and runs its possessions ~36% low — corrupting *all* 2019 PBP-derived surfaces (box scores, a separate CSV, are fine). Not a parser bug; the source columns hold wrong values, and no clean re-fetch exists (the API returns `NO_DATA` for 2019 PBP). Suppressed via `compute.rs::pbp_source_is_corrupt`: a season whose PBP `FGA`+`3FA` tag count covers < `PBP_MIN_FGA_COVERAGE = 0.80` of box FGA (2019 ≈ 0.55; every clean season > 0.93) has its derived surfaces **cleared and skipped** in both `compute_pbp_aggregates` and `compute_pbp_lineups`, so the UI hides them like a pre-PBP season. Chosen over a hardcoded year list because it's a measurable signal that also catches a partial in-season load or any future feed regression; the parity test asserts both good-season MAE and that corrupt seasons stay cleared.

### On/off splits — SHIPPED 2026-06-07 (migration `034`)

The headline player-value surface (KenPom / EvanMiya style): a team's offense and defense **per 100 possessions with a player on the floor vs on the bench**. Computed inside `compute_pbp_lineups` off the **same validity-clamped `_game_lineups` set** the served aggregates read, so on/off reconciles with `lineup_aggregates` and `plus_minus_pbp` (same box-minute clamp, same `'onfloor'`/`'replay'` source flag). Rolled up to a season `player_on_off` row — a prod-resident table, since the per-stint `lineup_stints` it derives from stays local-only.

- **ON** = the player's own valid on-floor stints (he is one of the five): `sum` over `unnest(lineup)` of points/possessions/seconds.
- **OFF** = his team's *remaining* valid stints in the **same games** — team totals minus his ON totals — **restricted to games he actually appeared in**. Restricting to games-played isolates rotation (bench minutes) from availability (DNPs / injuries); a game he never played contributes to neither side. A player who never sits has zero off-court possessions, so his off rates are NULL (the UI renders "—").
- **Rates**: `ortg = 100·PF/poss_for`, `drtg = 100·PA/poss_against` (NULL-guarded), on the same per-100 scale as team AdjO/AdjD and the lineup rates. `net_on_off = on_net_rtg − off_net_rtg` — the on/off swing.
- **Caveats inherited from the replay lineups**: any `replay`-sourced split (~86% accurate) carries the SUB-replay drift, so the UI flags it; and the off-court sample is thin for heavy-minute starters (the panel surfaces the off possession count and warns under ~100). Both fade for any season re-fetched with API-native onfloor lineups. Validated on Duke 2025: Flagg on net +25.3 / off +18.1 (swing +7.2), Proctor +16.5, and Caleb Foster −29.0 (Duke's lineups were elite when the struggling guard sat, 1336-poss off sample) — all reconcile with Duke's ~+25 season net. Served at `GET /api/players/:id/on-off`, panel on `PlayerDetail.tsx`.

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
| **P4+** | Utilization (ROADMAP Phase 6, unchanged & parallelizable): ML features (`lineup_quality`, `transition_rate`, `paint_rate`; PBP-era vs pre-PBP model variants since coverage starts 2012); API endpoints (`/players/:id/on-off`, `/teams/:id/lineups`, `/games/:id/playbyplay`); UI (player on/off, team lineups tab, game-detail page). | Per-surface. |

P1–P3 are the pipeline that must exist before the cron work; P4+ is value on top.

## Open decisions

- **Pre-PBP era handling for ML** — coverage starts 2012. Train two model variants (PBP-era vs pre-PBP) or impute missing PBP features with season averages. Decide in P4, document in `docs/features_methodology.md`.
- **Crafty intra-season fetch** — v1 is the simple by-date pull; narrowing to changed/missing games only is a post-proof optimization (above).
- **2021–2025 CSV gap** — RESOLVED (2026-06-07): the full historical PBP backfill landed; all 12 seasons (2015–2026) are loaded locally and their derived aggregates are on prod. 2019 is the one season *suppressed* (corrupt source — see "Possession & tempo normalization"), not missing.
- **Source-duplicate plays: dedup at ingest or at derivation?** RESOLVED (2026-06-06): **dedup at derivation.** P1 stores source rows faithfully; `compute_pbp_aggregates` (P2) dedups by `(game_id, sort_order, description, player_id)` before counting and prefers running-score deltas over `sum(points)`. Keeps the raw table a true mirror of the feed.
