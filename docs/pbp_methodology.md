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

The loader therefore does **not** use `get_all_pages`. It paginates scope-aware (`ingest_pbp_scoped`): it computes the in-scope game-code set up front (for a date: the games on that date from our own `games` table; for a gamecode: just that code), keeps only in-scope plays, and **stops as soon as a page yields zero in-scope plays**. This bounds any query to ~1 page past its real end regardless of the filter quirk, and guarantees we never write a game we didn't ask for.

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
| `scoring_play` | `ScoringPlay` | `scoringplay` (`"Y"`/`"N"`) | |
| `points` | `Points` | **no field** — derive from tags (`FTM`=1, `FGM`&!`3FM`=2, `3FM`=3) or score delta | API↔CSV difference |
| `tags` | `Tags` | `tags` (same `\|` vocabulary) | |
| `score_home`/`score_vis` | `ScoreHome`/`ScoreVis` | `game.score-home`/`game.score-vis` | |
| `score_diff` | `Diff` | `thediff` (e.g. `"+5"`) | acting-team POV |
| on-floor lineup | (absent) | `game.onfloorhome`/`game.onfloorvis` | **present & per-play in the API** (26 distinct lineups / 500 plays) — the SUB-replay oracle |

Two loader wrinkles the spot-check pinned down: (1) the API carries **no explicit `points`** — derive it; (2) `time`/`explanation` deserialize as empty objects on non-action rows — coerce to null. Both normalize cleanly into the schema above. The raw table stays source-identical (we do **not** add an `onfloor` column) — the API lineup is consumed only as the test-time replay oracle, keeping a single derivation path.

#### Intra-season fetch is "yesterday's games only"

We never re-pull a season. The nightly job fetches **only the prior day's finished games**:

- `/playbyplay/mbb/{yesterday-date}` returns just that date's plays.
- Volume: 50–200 games/day × ~530 rows/game ≈ 27k–106k rows ≈ **55–215 API calls/day** at 500/page — a small fraction of the 500/hr standard budget.
- **Crafty-fetch headroom (TBD, not in v1):** because the unit of idempotency is the game, the fetch can be narrowed further — e.g. only games whose `games` row was updated yesterday, by-gamecode fetches for a known schedule, or skipping games already present in `play_by_play`. v1 ships the simple by-date pull; the lighter variants are an optimization once the pipeline is proven.

## Derivation: `compute_pbp_aggregates`

A new compute step (runs after `backfill_game_stats`, before `compute_player_season_stats`), source-agnostic, operating on whichever games are in `play_by_play`. For the nightly path it runs **only over the touched `game_id`s**, not the season.

### SUB-replay → lineups

For each game, walk rows in `seq` order maintaining the current 5-man on-floor set per team:
- Seed each team's starters from the first appearances before the first sub (or from `player_game_stats.starter`).
- On `SUB` rows, apply "sub in" / "sub out" to the acting team's set.
- At each period boundary, validate the set size is 5; log and self-heal anomalies (missing sub at halftime, etc.).

Outputs:
- **`lineup_stints (game_id, period, start_clock, end_clock, team_id, lineup INT[5], opp_lineup INT[5], score_delta, possessions)`** — one row per contiguous on-floor combination.
- **`lineup_aggregates (season, team_id, lineup INT[5], minutes, plus_minus, ortg, drtg, possessions)`** — season rollup; this is what the site reads.

**Validation oracle:** when the API JSON is the source (and carries `onfloorhome`/`onfloorvis`), assert the replayed lineup matches the embedded one on a sample of games. This catches replay bugs without making the derivation depend on a column the CSV lacks.

### Tag-based per-`(player_id, game_id)` columns (no replay)

Added to `player_game_stats` (additive, small, ships to prod):
- `plus_minus_pbp` — from stint deltas (overwrites the sparse box-score `plus_minus`)
- `paint_fga`/`paint_fgm`, `perimeter_fga`/`perimeter_fgm` — `paint` tag presence on FGA/FGM
- `transition_pts` (`brk`), `second_chance_pts` (`2ch`), `points_off_turnovers` (`offto`)
- `fouls_drawn` — `FOULED` event count for the player
- Optional later: `assist_edges (season, passer_id, scorer_id, count)` linking `AST` → the `FGM` it set up.

## Data-quality notes (from P1 verification)

- **NatStat emits occasional duplicate play records.** Verified on 2026-06-06: in game 1482209, two distinct play ids (`116271602`, `116271608`) carry the same `sequence` (`2-1997F1`), same description ("Aidan Cammann made free throw 1 of 2"), both `scoringplay=Y` — i.e. one real free throw recorded twice. These are *not* an ingest artifact (paginated pages share zero play ids — confirmed) and *not* page overlap; they originate in the NatStat feed. P1 stores them faithfully (the raw table mirrors the source).
- **Consequence:** any *count-based* derivation over raw rows (tag-summed points, FT counts, and especially **possession/stint counts in P2**) will be inflated by these dupes. Across a 3-game sample, tag-summed `points` ran +1 vs the final on 2 of 3 games.
- **The authoritative within-game scoring signal is the running score** (`score_home`/`score_vis`), which NatStat does *not* double-count — it matched the final exactly on all 3 verification games. So `points` (a tag-derived convenience, and absent from the API entirely) is ±1-reliable, while running-score deltas are exact.
- **P2 guidance:** `compute_pbp_aggregates` should (a) dedup source-duplicate plays by `(game_id, sort_order, description, player_id)` before counting, and (b) prefer running-score deltas over `sum(points)` wherever a precise figure matters (plus-minus, stint scoring). This is deferred to P2 deliberately — it needs full per-game context and shouldn't bake cleaning into the raw mirror. Open question carried into "Open decisions": dedup at ingest vs. at derivation.

## Storage & prod sync

Per the ROADMAP storage split (unchanged, restated here as the rule):

- **Local Postgres** holds raw `play_by_play` (~500 MB/season; ~7.5 GB for the 2012–2026 PBP-available range) and `lineup_stints`. Used for ML training, ad-hoc analysis, and recomputing aggregates if methodology changes.
- **Railway (prod)** holds only what the live site reads: the additive `player_game_stats` columns and `lineup_aggregates`. **Never** raw `play_by_play` or `lineup_stints`.
- **`scripts/sync_to_prod.sh` must be patched before the first PBP load.** Its auto-discovery loop would otherwise try to push the multi-GB raw table to Railway and hit the DB cap. Extend `EXCLUDED` (currently `api_cache`, `_sqlx_migrations`) with `play_by_play` and `lineup_stints`, comment each with the why, and print the excluded set in `--dry-run`.

## Pipeline-maintenance surface (cron topology)

PBP adds **one** new daily pipeline, with a hard placement constraint that shapes the cron design:

```
nightly (local/ingest env, NOT Railway):
  cstat-ingest playbyplay --from {yesterday} --to {yesterday}   # API, ~55–215 calls
    → compute_pbp_aggregates over touched games
    → scripts/sync_to_prod.sh   (derived only; raw excluded)
```

The job **cannot run as a pure-prod cron** because raw PBP lives only in the local/ingest environment — Railway has no raw table to write into. This is the key fact to settle before laying out the cron jobs the ROADMAP plans: the PBP nightly belongs with the local ingest crons that sync up to prod, not with any prod-resident scheduled task.

## Build phases

| PR | Scope | Risk |
|----|-------|------|
| **P1** | Migration `029`; CSV loader (`--with-pbp`) + API loader + `playbyplay` subcommand; patch `sync_to_prod.sh` `EXCLUDED`. Step zero: `explore` spot-check of live API shape. | Low — mirrors existing ingest patterns. |
| **P2** | `compute_pbp_aggregates`: SUB-replay → `lineup_stints`/`lineup_aggregates` + tag-based `player_game_stats` columns. Validate replay vs API oracle. | **Highest** — SUB-replay edge cases (OT, halftime, missing subs) need test coverage. |
| **P3** | Wire API loader + incremental recompute + derived-only sync into the nightly flow. This is the command the cron calls. | Low–medium. |
| **P4+** | Utilization (ROADMAP Phase 6, unchanged & parallelizable): ML features (`lineup_quality`, `transition_rate`, `paint_rate`; PBP-era vs pre-PBP model variants since coverage starts 2012); API endpoints (`/players/:id/on-off`, `/teams/:id/lineups`, `/games/:id/playbyplay`); UI (player on/off, team lineups tab, game-detail page). | Per-surface. |

P1–P3 are the pipeline that must exist before the cron work; P4+ is value on top.

## Open decisions

- **Pre-PBP era handling for ML** — coverage starts 2012. Train two model variants (PBP-era vs pre-PBP) or impute missing PBP features with season averages. Decide in P4, document in `docs/features_methodology.md`.
- **Crafty intra-season fetch** — v1 is the simple by-date pull; narrowing to changed/missing games only is a post-proof optimization (above).
- **2021–2025 CSV gap** — obtain dashboard exports or accept API backfill (~13 h/season at standard rate) for those years.
- **Source-duplicate plays: dedup at ingest or at derivation?** P1 stores them faithfully; the lean is to dedup in P2's `compute_pbp_aggregates` (full context, keeps the raw table a true mirror). Revisit if a raw-row consumer other than the derivation appears.
