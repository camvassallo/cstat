# NatStat → CollegeBasketballData migration plan

*Written 2026-11-20. Source comparison and spot checks done the same day; the evidence
is in §1. This is the ordered plan for replacing NatStat as cstat's box-score, schedule
and play-by-play source.*

**Status:** §4.1–4.3 are built (`crates/cstat-ingest/src/cbbd.rs`, `cbbd_shadow.rs`,
`source_parity.rs`, migration 075, the nightly's `cbbd_shadow` step), and so is the
ESPN-id mapping of §3 (`espn_ids.rs`, migration 076, the nightly's `espn_ids` step),
the box-score gap-fill of §4.5 step 3 (`gap_fill.rs`, migration 077, the nightly's
`gap_fill` step), and play-by-play, §4.5 step 4 (`cbbd_plays.rs`, migration 078, the
nightly's `cbbd_plays` step): from 2027 on, CBBD's plays replace NatStat's. The shadow
run starts once `CBBD_API_KEY` is set on the cron service. CBBD now fills the games
NatStat has no box score for; NatStat is still primary everywhere it has one. The first
slice of §4.5 step 5 is built too: a NatStat outage no longer aborts the nightly (CBBD
supplies the night, and the served-critical `box_scores` row says which source did),
and the gap-fill creates the games and players NatStat never listed, which NatStat
adopts when it lists them later. §4.5 is the agreed order.

## 0. The decision

**CollegeBasketballData (CBBD) becomes the primary game-data source. ESPN's public
endpoints stay what they already are (live scoreboard, lines, broadcasts) and become
the fallback for CBBD. NatStat is retired once the shadow run passes (§5).**

Three facts drive it:

- **CBBD is the only candidate whose terms permit this use in writing.** Commercial
  use, ad-supported sites, model training and publishing derived ratings are all
  allowed; bulk redistribution of the raw data, a mirror, or a substitute API are not
  (<https://collegebasketballdata.com/terms>, §4–5). ESPN's endpoints are
  undocumented and carry no permission at all.
- **CBBD's ids are ESPN's.** Every game carries `sourceId` = the ESPN event id, every
  player `athleteSourceId` = the ESPN athlete id, every team `teamSourceId`. cstat
  already matches every game to ESPN through `game_broadcasts.source_event_id`, so
  the two sources join without fuzzy matching, and ESPN can stand in for CBBD on any
  day CBBD is down.
- **NatStat has nothing CBBD or ESPN lacks** (§1.3). What it has is lock-in: its ids
  are keys throughout the schema. That is a migration cost, handled in §3, not a
  reason to stay.

## 1. Evidence (2026-11-20)

### 1.1 One game, three sources — Gonzaga 80, LSU 81 (2026-11-14, ESPN 401921689)

NatStat from `api_cache` (the key was suspended; it was not called). ESPN from the
public game summary. CBBD from `/games`, `/games/players`, `/plays/game`,
`/lineups/game`.

| | NatStat | ESPN | CBBD |
|---|---|---|---|
| Score, shooting lines | match | match | match |
| Starters | **wrong for both teams** (De Sousa, 4 min, listed as a starter; Huff listed off the bench) | correct | correct |
| Minutes | truncated (Dent 35, Niang 24, Bennett 23) | 36, 25, 24 | 36, 25, 24 |
| OREB / DREB split | DREB absent | yes | yes |
| Per-player advanced | none | none | usage, ORtg/DRtg, TS%, game score |
| Plays | 519 (clock, players) | 451 (clock, ESPN player ids) | the same 451 |
| Shot location | no | 171 / 171 | 171 / 171, plus range and assister |
| On-floor players | none since Nov 2026 | none | **all 10 on every play** |
| Lineups | no | no | 28 units, exactly 2400 s per team |
| Elo | yes | no | pre- and post-game, per game |

### 1.2 Freshness and coverage

- **NatStat box scores lagged four days.** As of 2026-11-18 23:49 ET its `playerperfs`
  feed held nothing after Nov 14 — 206 finished games, Florida–Houston among them
  (listed Final with `performances: 0`). CBBD held 402 team box rows for Nov 15–19,
  Florida–Houston included.
- **One CBBD call returns a whole night of play-by-play**: `/plays/date?date=2026-11-18`
  returned 90 games, 44,242 plays, 98.2% of them with all 10 on-floor players. The
  response is ~60 MB.
- **Non-D1 opponents are included** (Belmont vs. Union (KY), Nov 18), so the
  displayed-W-L rule (non-D1 games count in W-L, never in ratings) still holds.
- **History is thinner before the tracking era.** A 2015 game has full plays but no
  on-floor players and no shot locations (ESPN did not record them). Season-level
  Elo exists for 2015 (351 teams). The year on-floor coverage starts has not been
  checked.
- **Rosters have no class year.** `/teams/roster` carries height, weight, hometown,
  ESPN ids and `startSeason`/`endSeason`, but no Fr/So/Jr/Sr. See §4.6.

### 1.3 What NatStat gives us today, and where each piece goes

| NatStat endpoint | Used by | Replacement |
|---|---|---|
| `games` (season, date range) | `games`, `schedule` steps | CBBD `/games` |
| `playerperfs`, `teamperfs` | `player_perfs`, `team_perfs` | CBBD `/games/players`, `/games/teams` |
| `playbyplay` | `playbyplay` | CBBD `/plays/date` |
| `games;lineups` | `lineups` → `natstat_lineups` | CBBD `onFloor` on each play; `/lineups/game` to cross-check |
| `elo`, `forecasts` | `elo` → `team_elo_games`, `team_season_stats.elo_rating`, `game_forecasts` | CBBD per-game Elo — **needs a game-model retrain** (§4.4) |
| `pointspread`, `moneyline`, `overunder` | `lines` → `game_lines` | ESPN is already primary (`espn_game_lines`); CBBD `/lines` replaces the NatStat fallback |
| `teamcodes`, `teams`, `players` | `teams`, `players` | CBBD `/teams`, `/teams/roster`; class year from Torvik / ESPN (§4.6) |
| `news` | social bot `news` job | ESPN news feed (CBBD has none) |
| — (live scores) | score ticker | already ESPN `scoreboard/live` — unchanged |

## 2. Cost and quota

Pricing (<https://collegebasketballdata.com/api-tiers>, 2026-11-20), billed through
Patreon, one quota shared with CollegeFootballData:

| Tier | Price | Calls / month |
|---|---|---|
| Free | $0 | 1,000 |
| Tier 1 | $1/mo | 5,000 (adds live scoreboard) |
| Tier 2 | $5/mo | 30,000 |
| Tier 3 | $10/mo | 75,000 |

A nightly needs about 5–8 calls (games, team box, player box, plays by date, lines,
schedule refresh), so ~250 a month in season. **Recommended: Tier 1 ($1) for headroom,
plus one month of Tier 2 or 3 for the backfill in §3.** Past the limit CBBD returns
429 until the 1st of the month, so the client must read `x-calllimit-remaining` and
treat a 429 as "out for the month", not a transient error (§4.1).

Not to do: don't fetch `/lineups/game` per game nightly (≈60 calls a night, 1,800 a
month). The `onFloor` field on plays carries the same information for one call.

## 3. Identity: the real work

**Built** (migration 076, `crates/cstat-ingest/src/espn_ids.rs`, `cstat-ingest
map-espn-ids --from … --to …`, the nightly's `espn_ids` step). What was built differs
from the plan below in three places, noted under "As built".

NatStat ids are the join keys today: `teams.natstat_id` / `players.natstat_id` (UNIQUE
with `season`), `games.natstat_id`, `team_elo_games`' NatStat codes,
`data/excluded_games.json`, and ~730 references across `cstat-core`, `cstat-ingest`,
`cstat-api`, `training/` and `web/src`. The plan is **additive**: keep every existing
UUID and `natstat_id`, add ESPN ids beside them, and move readers over one at a time.

**Migration (one new file):** nullable `espn_id` on `teams`, `players`, `games`, each
UNIQUE with `season` where non-null. A CBBD internal id is not stored as a key; CBBD
derives from ESPN ids, and ESPN is the fallback, so the ESPN id is the one key both
sources share.

**Mapping command, as planned (see "As built" below):**

- **Games** — already mapped for every season `broadcasts` has run over
  (`game_broadcasts.source_event_id`). For older seasons, match CBBD `/games` on
  date + both teams + final score, and drop ambiguous games rather than guessing
  (the rule the broadcasts matcher already follows, because NatStat re-issues ids
  and lists phantom and wrong-date duplicates).
- **Teams** — CBBD `/teams` gives `sourceId` per team; match on our team names and
  `data/team_short_names.json` aliases. ~365 rows a season, so the leftovers get
  hand-mapped into a small JSON capture.
- **Players** — within each mapped game, match CBBD box rows to our
  `player_game_stats` rows by normalized name (reuse `name_match.rs`), then jersey,
  then the unique-last-name fallback `repair_phantom_swapped_games` already uses. A
  player's ESPN id is the one most of his box rows agree on.

**ESPN's athlete id survives a transfer; `natstat_id` does not.** Today every
cross-season player join has to use `natstat_id OR torvik_pid`, because NatStat mints
a new id per team and a natstat-only join drops ~26% of inbound transfers. Once
`players.espn_id` is populated, it becomes the cross-season key with `torvik_pid` as
the fallback. That is a real improvement, but it touches the training joins
(`training/test_cross_season_joins.py` guards them) and moving them is its own step
(§4.7), not part of the cutover.

**As built:**

- **Window, not seasons.** `map-espn-ids --from --to` takes Eastern dates, like
  `cbbd-shadow` and `source-parity`; the nightly maps the same two days the shadow
  step just copied.
- **Players pair by name, then by spelling** (`name_match::pair_players`; no jersey
  step, since `player_game_stats` has no jersey), with the game's sides oriented by
  the score rather than ESPN's flip, because CBBD's home and away do not always follow
  ESPN's. Non-D1 sides are skipped. Passes, each pairing a row and a line only when
  each is the other's sole match: the name (`name_key`: diacritics folded, punctuation,
  hyphens and suffixes dropped, spaces run together) or Torvik's spelling of our
  player (`torvik_player_stats.player_name` on his linked row, which needs no NatStat
  call); a last name unique on both sides (`source-parity`'s fallback); a spelling
  variant that changes one half of the name only (`names_compatible`: a second
  surname, a hyphenated half, a one-letter misspelling, a short or listed nickname
  for the given name). Past the exact name the two lines must also score the same
  points. `source-parity` keeps the first two passes without aliases
  (`pair_box_names`).
- **Last season's id carries over.** A player the window's boxes give no vote takes
  the ESPN id the same person held last season, found through `natstat_id` or
  `torvik_pid`. NatStat re-keyed nearly every player between 2026 and 2027 (2,137 of
  2,139 links on a simulated roll were Torvik's), so in practice this is the Torvik
  link. It only votes; the usual conflict rules apply.
- **Teams vote from the paired sides, not by name.** Each game side with a paired
  player is a vote for its CBBD team id, which one CBBD `/teams` call (cached 30 days)
  turns into ESPN's team id. No name matching and no hand-mapped JSON.
- **Never guess, never overwrite.** An entity takes the id more than half its rows
  name. A split vote, an id two rows would take in one season, a stored id that
  differs from the one found, or an id another row already holds is a conflict: it is
  listed and not written. A wrong id is cleared by hand and re-mapped.
- **Older seasons' games** map from `game_broadcasts` for every season `broadcasts`
  has covered (all twelve, locally). Players and teams map only where the `cbbd_*`
  shadow tables have box scores, so history needs a shadow backfill first, within
  quota.

First run, 2026-11-12..18 (local): 371 of 375 games, 3,019 players (3,112 of 3,155 box
rows paired) and 276 teams mapped, no conflicts. Most of the unpaired rows are
diacritics `normalize_player_name` does not fold (Prkačin / Prkacin) or names that
differ outright (Jalen StClair / Jalen St Clair).

With Torvik's spelling and the variant pass (2026-11-03..19, a scratch copy of the
local tables): 5,296 of 5,325 rows pair instead of 5,282 (33 only through Torvik's
spelling, 11 only as a variant) and 14 more players map, among them Carlos Varela
Romero, Ahmed Nedal Abdelrahman, Gustavo Guimarães, Emil Skytta and Gantas
Krizanauskas. Every one of the 3,584 players already mapped got the same id back, and
no vote split. The 20 players still unpaired are on game sides CBBD has no lines for,
not names.

**Backfill scope:** map ids for every season (cheap: ~150 date calls a season for
games plus box). **Do not re-ingest history from CBBD.** 2015–2026 stays the data the
models were trained on; only 2027 onward comes from CBBD. Re-ingesting history is a
separate decision for later (it would change every trained model's inputs).

## 4. Build steps

Each step lands as its own PR. Nothing is served from CBBD until §5 passes.

### 4.1 CBBD client

`crates/cstat-ingest/src/cbbd.rs`: bearer auth from `CBBD_API_KEY`, responses cached
in `api_cache` under a `cbbd/` prefix (same table, distinct keys), quota read from
`x-calllimit-remaining` and logged on every run. **A 429 means the month's quota is
gone**: no retries, and the rest of the run skips CBBD (the same short-circuit the
NatStat client now has for account refusals). `/plays/date` responses are ~60 MB:
deserialize into typed structs rather than holding a `serde_json::Value` tree, and
check peak memory on the Railway cron.

**Key handling:** the key goes on the Railway cron service only, plus a laptop `.env`
for development. CBBD has no IP rule like NatStat's, but its terms forbid extra
accounts to get around the quota, so it is one key, one quota, shared deliberately.

### 4.2 Shadow ingest

A best-effort nightly step, `cbbd_shadow`, never served-critical, that writes CBBD's
last two nights into **shadow tables** (`cbbd_games`, `cbbd_team_game_stats`,
`cbbd_player_game_stats`, `cbbd_game_play_summary`), keyed on CBBD's game ids with the
ESPN event id beside them. Plays are one coverage row per game rather than raw rows:
a season of raw plays is ~2.7M rows on prod for a step nothing serves, and they
arrive with the play-by-play cutover instead (built: the `cbbd_plays` step now writes
these coverage rows, from the same calls). It changes nothing a page
reads. Recorded in `ingest_runs` like every step; excluded from `sync_to_prod.sh`
(prod-written).

### 4.3 Parity report

`cstat-ingest source-parity --from … --to …`, one row per game and a summary:

- game present in each source; final score agreement;
- team box columns, field by field;
- player rows: present in both, minutes (expect +0–1 from NatStat's truncation),
  points, rebounds split, starters;
- play counts and shot counts per game;
- days a source was missing a finished game (the four-day NatStat lag is exactly
  what this should surface).

Then one level up: replay a window through the existing `simulate` harness against
`postgres-sim` with CBBD as the input and compare `compute_all`'s outputs to prod —
team AdjO/AdjD, CAM, four factors. The game-level report says the inputs agree; this
says the site would show the same numbers.

### 4.4 Elo and the game models

`diff_elo_rating` is a feature of every served game model, and `pit_team` reads
pre-game Elo from `team_elo_games`. CBBD's Elo is a different system on a different
scale (Gonzaga 1688 and LSU 1963 before the Nov 14 game). Swapping the source without
retraining feeds the trees values from a distribution they never saw.

Recommended: **backfill CBBD per-game Elo for 2015–2027 into a new table, retrain the
game models on it walk-forward, and switch only if walk-forward is no worse** (the
`training/walk_forward.py` judge; the held-out bundles in `training/models/heldout/`
regenerate after, per #468). If CBBD Elo loses, compute Elo in-house from our own game
results, which removes the external dependency entirely. Keeping NatStat only for Elo
is not an option worth taking: it keeps the subscription, the key and the IP-block
risk for one feature.

`game_forecasts` (NatStat's forecasts) has no live reader worth keeping: its line
columns are dead from 2026 on (migration 063), and Elo moves to the table above.
Retire it.

### 4.5 Cutover, one feed at a time

The agreed order (2026-11-20). Each step lands as its own PR, after its parity numbers
are clean for the shadow window:

1. **Verify the team-box disagreements** with `source-parity`'s per-game lists. CBBD's
   team line is not our team line: its rebounds and turnovers include team rebounds
   and team turnovers (credited to no player), and its fouls include bench
   technicals, while `team_game_stats` is the sum of the players and every rate built
   on it (ORB%, DRB%, TOV%, possessions) assumes that. `source-parity` already
   compares those fields on CBBD's player sums, which matched NatStat's on the games
   checked; this step confirms what is left is NatStat being wrong, not a mapping.
2. **ESPN-id mapping** (§3). Built.
3. **Gap-fill box scores.** **Built** (`crates/cstat-ingest/src/gap_fill.rs`,
   migration 077, `cstat-ingest gap-fill --from … --to … [--dry-run]`, the nightly's
   `gap_fill` step). Write CBBD's box scores into `player_game_stats` /
   `team_game_stats` only for games NatStat has none for (the four-day NatStat lag,
   and the suspension), joining on `espn_id`. Rebounds, turnovers and fouls on the
   team row come from CBBD's player sums, for the reason in step 1. NatStat stays
   primary wherever it has the game.

   **As built:**

   - **Provenance.** Rows carry `source` (`'natstat'` by default, `'cbbd'` for a
     fill), and `games.score_source = 'cbbd'` marks a score and Final the fill set.
     The NatStat perf steps delete the `cbbd` rows of each game side they are about
     to write (`ingest::games::clear_cbbd_rows`, one statement per page set, only
     for games whose status lets NatStat's lines through), so NatStat's box
     replaces a fill instead of landing beside it. NatStat's `games` step keeps a
     filled game's Final until NatStat sends a final score of its own.
   - **All or nothing per game.** A game is written only when every CBBD line on
     each D1 side resolves to one of our players and the lines add up to the
     side's score. Players resolve by `players.espn_id`, then against that team's
     roster (players with no ESPN id yet) by name or Torvik's spelling of it, then
     by a spelling variant, each unique on both sides (`name_match::pair_players`).
     The last-name fallback of `name_match::pair_box_names` is not used: it is safe
     between two box scores of one game, not between a season roster and one
     night's box. No `players` row is created.
   - **Sides** are oriented by our score, else by which of our teams the lines'
     ESPN-resolved players belong to; the two must agree. A line resolving to a
     player of a third team means the game carries the wrong ESPN event (a NatStat
     Towson–Morgan State matched to Loyola Maryland–Morgan State's event), and the
     game, score included, is skipped.
   - **Stuck games** (no score, or an in-progress status like `H2 05:11`) get CBBD's
     final score and `status = 'Final'`, never from a CBBD 0 or NULL side and never
     over a score of ours that disagrees.
   - **Nightly placement.** `cbbd_shadow` (box scores only) → `espn_ids` →
     `gap_fill` run just before `compute`, the first two under a shared 240 s
     deadline; the play coverage moved to its own `cbbd_plays` step at the end of
     the run. A slow or down CBBD costs ledger rows, never compute.

   First run, 2026-11-12..18 (local, NatStat box data through Nov 14): 368 games
   final in CBBD; 153 already had NatStat's box; **110 filled** (202 team rows,
   2,170 player rows, 648 of them resolved by name); 8 stuck games given their
   final; 105 skipped, 103 of them for unresolved players. 185 of the 192
   unresolved lines are players absent from our `players` table altogether
   (NatStat has never listed them), so those games wait for NatStat or for step 5,
   which can create players. The other two: a CBBD 0-0 final, and the wrong-event
   game above. Every line of eight filled games checked against ESPN's box score
   matched.
4. **Play-by-play and lineups from CBBD.** NatStat's November format change removed
   clock, tags, on-floor players and ids, and the lineup / on-off / RAPM chain has been
   running on reconstruction. With `onFloor` on every play, `compute_pbp_lineups`
   reads lineups directly. Plays join on `games.espn_id` and `players.espn_id`.
   **Built** (step 4 as built, below).
5. **CBBD as the primary box source**, NatStat as the gap-filler (`games`,
   `player_perfs`, `team_perfs`, `schedule`). New rows carry `espn_id`; `natstat_id`
   is null for them, so every reader that assumes `natstat_id` is set has to be found
   first (the §3 inventory). The `parse_game` exclusion check moves to `espn_id`
   (re-key `data/excluded_games.json`; it is `include_str!`-compiled, so this ships
   with a deploy). **First slice built** (step 5 as built, below): the nightly no
   longer depends on NatStat, and CBBD creates what NatStat never listed. Per-game
   precedence is unchanged (NatStat over CBBD) until §5 passes; the rest is listed
   under "Not done".
6. **Elo**, once §4.4 decides it.
7. **Retire NatStat**: teams and player metadata (§4.6); lines (drop the NatStat
   `game_lines` fallback, CBBD `/lines` takes its place under ESPN); news (**done**:
   the social bot's `news` job reads ESPN's news feed, and `cstat-ingest social` no
   longer needs `NATSTAT_API_KEY`, so the social service runs without it. This removes
   the second NatStat caller that likely contributed to the 2026-11-20 IP-block
   suspension; details in `docs/social_drafts.md`). Then §5's retirement.

**Step 4 as built** (`crates/cstat-ingest/src/cbbd_plays.rs`, `cstat-ingest cbbd-plays
--from … --to … [--dry-run]`, the nightly's `cbbd_plays` step right after `espn_ids`,
migration 078):

- **CBBD replaces NatStat's plays, it does not just fill gaps.** On the 153 games of
  2026-11-12..14 that both sources had (285 team-games, local), CBBD's plays against
  the box score and the lineups `compute_pbp_lineups` builds from each:

  | | NatStat (reconstructed) | CBBD |
  |---|---|---|
  | Team-games whose tagged FGA / FTA equal the box | 241 / 241 | 285 / 284 |
  | Team-games whose tagged points equal the box | 197 | 282 |
  | Plays with ten on-floor players | none | 98.5% |
  | Lineup source | substitution replay | the plays' own lists |
  | Game time inside 5-man stints | 84% | 98% |
  | Team-games with ≥95% of time in 5-man stints | 2 | 267 |
  | Player-games whose stint minutes are within 2 of the box | 48% | 98% |
  | Mean \|stint minutes − box minutes\| | 2.40 | 0.61 |
  | Player events with a player id | 96.2% | 96.4% |

  NatStat's replay also loses the clock between stints (its stints sum to ~86% of the
  game), which the minutes rows partly reflect, but its lineups are reconstructed
  from substitutions on a feed with no clock, while CBBD's are listed. One game's
  plays matched ESPN's public summary exactly (Gonzaga–LSU, 401921689: the same 451
  plays in the same order with the same clocks; tagged points 80–81).
- **One source per game.** `play_by_play.source` records the feed. `cbbd-plays`
  deletes and rewrites a game whole; the NatStat API loader and the CSV loader skip
  any game holding CBBD rows. Seasons before 2027 are never read or written.
- **Tags are NatStat's box-event vocabulary** (`FGA` two-point attempt, `3FA`, `FTA`,
  `ORB`/`REB`, `TO`, `SUB`, …), mapped from CBBD's play type and shot range, so
  possessions and the FGA corruption gate read them unchanged. No contextual tags
  (`paint`, `brk`, …): NatStat's 2027 plays have none, and a season holding them on
  only some games would pass `pbp_lacks_context_tags` and show zeros for the rest.
  CBBD folds assists into the shot (no `AST` rows). Substitution text is rewritten to
  NatStat's `"<name> sub in"` so the replay fallback parses it.
- **Ids only.** A participant resolves CBBD athlete → `cbbd_player_game_stats` →
  ESPN id → `players.espn_id`, or stays NULL; on the floor an unresolved player is
  `cbbd:<athlete id>`, so a substitution involving him still splits the stint and he
  is only left out of the unit. 98.3% of player events on D1 sides with a box score
  resolved (2026-11-12..18); a game whose players have no ESPN ids yet (no box score
  yet) resolves almost nothing, and a later run rewrites it once they map.
- **Sides follow our `games` row** (CBBD's home/away do not always follow ESPN's),
  oriented by the final score as `source-parity` does; 2 of 367 games fell back to
  ESPN's flag. Plays are ordered by period, clock, then ESPN's play id (the response
  is not in game order; a running score only rises, as in the NatStat loader).
- **Exact lineups** (`pbp_replay::stints_from_exact_onfloor`): a new stint wherever
  either five changes or a period starts; a play not listing five a side stays in the
  stint around it, and a stint owns the clock up to its first play. A game listing
  all ten on fewer than 90% of its plays, or on none in some period (18 of 348 in the
  window, mostly a sixth player stuck on one side for a half), is replayed from its
  substitutions instead. The `onfloor` stint label is unchanged.
- **Quota:** one `/plays/date` per UTC day, shared with the shadow step, which no
  longer fetches plays itself: `cbbd_plays` writes the coverage rows from the same
  responses. Three calls a night.
- **Not done:** a game whose box score (and so its players' ESPN ids) arrives after
  the two-day window keeps unresolved players until `cbbd-plays` is re-run over it.

**Step 5 as built, first slice** (no migration; `crates/cstat-ingest/src/gap_fill.rs`,
`ingest/season.rs`, `ingest/games.rs`, `cstat_core::source_ids`):

- **A NatStat outage degrades the nightly instead of aborting it.** `games`,
  `player_perfs` and `team_perfs` failing (a `TOO_MANY_IPBLOCKS` suspension, the
  case that froze the site on 2026-11-20) is a failed ledger row and a DEGRADED issue
  line; the run goes on through the CBBD block and compute. A served-critical
  **`box_scores`** row, recorded after the block, holds the verdict:
  NatStat (its three steps succeeded), CBBD (one failed, and a fresh `cbbd_shadow`
  copy plus a `gap_fill` that ran supplied the night), or no source (`failed`, which
  also pings the heartbeat `/fail` and exits non-zero, as the abort did).
  `SERVED_CRITICAL` swaps NatStat's three box steps for `box_scores`, with `games`
  standing in until the first run records it. An account refusal folds every NatStat
  issue line it caused into one. `elo` stays NatStat-only and served-critical, so an
  outage past 36h still turns `/api/health/ingest` red through it until step 6.
- **Precedence, per game side: NatStat over CBBD, unchanged.** NatStat's box
  replaces a CBBD fill (`clear_cbbd_rows`), a game with any box row is never
  filled, and one game side always has one source. Flipping it (CBBD's box
  replacing NatStat's where both exist) is deliberately not in this slice: §5's
  acceptance (three weeks of clean parity) has not run yet, and the flip changes
  the minutes and rebounds every model reads.
- **New players.** A CBBD line no player of ours matches becomes a `players` row
  with `espn_id` set, when its ESPN id is no player's this season, no roster player
  might be him under another spelling (`may_be_on_roster`: squashed names agree,
  his own or Torvik's spelling of him, or, for a roster player with no ESPN id yet,
  any name part of three letters or
  more is the same, a prefix of the other, or the end of the other: Xabi / Xabier,
  Perez-Rouzan / Rouzan), and at least three, and half, of the side's lines are
  players we have (`MIN_KNOWN_LINES`): the wrong-event guard, so Loyola Maryland's
  box on our Towson game cannot create Loyola's roster as Towson players.
  **`natstat_id` is a placeholder, `espn:<ESPN id>`, not NULL**: `players.natstat_id`
  is `NOT NULL` and `(natstat_id, season)`-unique, and readers across the workspace
  decode `games.natstat_id` and `players.natstat_id` as plain strings, so NULL
  would fail them at runtime; NatStat's ids are numeric, so the prefix never
  collides. This replaces the plan's "`natstat_id` is null for them" and needs no
  migration.
- **New games.** A CBBD final on no game of ours (no `games.espn_id`, no
  `game_broadcasts` row on the event) between two teams of ours by ESPN id (CBBD
  team → ESPN team through the cached `/teams`, then `teams.espn_id`; no call)
  becomes a `games` row (`natstat_id` `espn:<event>`, `espn_id`, `score_source =
  'cbbd'`, CBBD's orientation and Eastern date and tip time), only together with
  its box, and only when nothing of ours may already be it (`creation_blocker`): no
  game between the same pair within 7 days, and no game of either team within 2
  days that no ESPN event pins down as a different game. On 2026-11-12..19 locally
  all five CBBD finals on no game of ours were NatStat games under a wrong date or
  a wrong opponent, or had a non-D1 side; none was created, which is the point.
  The case it exists for is a game NatStat lists after an outage began (a later
  MTE round, a bracket game).
- **Adoption, so nothing double-counts.** When NatStat lists a created game (the
  `games` step and `schedule_refresh`), a game of the same season between the same
  two teams, within 2 days, one-to-one, takes NatStat's id and NatStat's
  orientation (teams, scores and halves swapped, and with them the box rows'
  `is_home` and the plays' home/visitor columns) before NatStat's upsert runs, so
  the upsert updates it. When NatStat lists a created player (the perf ingest's
  auto-create and the roster ingest), the one created player on that team whose
  squashed name agrees takes NatStat's id. `compute`'s duplicate-player merge keeps
  a NatStat row over a created one and hands it the ESPN id. A NatStat game that
  shares only one team with a created game, within a day, with that team's final
  score and its opponent's the same, is NatStat's wrong-opponent listing of it (the
  UNC Wilmington case): it is not written (`SkipReason::CbbdTwin`, anomalous, so it
  reaches the warnings line), and its box lines then skip as an unknown game rather
  than counting the game twice for the shared team. A twin NatStat first listed
  without a score (scheduled, or stuck in progress) is caught when its score
  arrives, and the stored row is deleted then, unless something already hangs off
  it. An adoption that swaps orientation also flips
  `game_broadcasts.source_home_is_away`.
- **A `box_scores` "from CBBD" verdict says how much CBBD left unboxed**: its notes
  and the warnings line count the CBBD-final games of the shadow window that still
  have no player rows after the fill (skipped games, games with no ESPN id yet,
  games NatStat half-boxed with team rows only, which the fill leaves alone), so a
  green row cannot hide a fill that covered little.
- **First run** (2026-11-12..19, on a scratch copy of the local 2027 tables): 52
  more games fill than before (87 team rows, 1,021 player rows), creating 87
  players; 55 are skipped: 51 on a line that matches nobody and is not created (a
  roster player might be him under another spelling, as with Jalen St Clair /
  StClair and Roko Prkačin / Prkacin, or the line has no ESPN id), two CBBD finals
  refused as possible copies of NatStat games (Towson–UMBC, Arkansas–Indiana), the
  Towson–Morgan State wrong event, and CBBD's 0-0 final. A first cut of
  `may_be_on_roster` compared only last names and first-name-plus-initial and
  created 98 players, four of them duplicates of unmapped NatStat players (Xabi
  Lopez / Xabier Lopez Rodriguez, Carlos Varela / Carlos Varela Romero, Daniel
  Perez-Rouzan / Daniel Rouzan, Ahmed Nedal / Ahmed Nedal Abdelrahman); the rule
  now compares every part of the two names, and none of the 87 shares a name part
  with an unmapped teammate. Deleting one filled game (Tennessee–Michigan State)
  from the copy and re-running created it back from CBBD with its 21 player and 2
  team rows.
- **Spelling-robust identity** (2026-11-03..19, a scratch copy of the local tables,
  `map-espn-ids` then `gap-fill`): 91 games fill instead of 52 (1,876 player rows
  instead of 1,021) and 14 are skipped instead of 53. Of the lines resolved by
  name, 9 resolve through another name the player goes by (Torvik's spelling: Xabi
  Lopez, R.J. Newton, Krisnan-Cole Ioane, Cello Jackson Jr.; a quoted nickname: LJ
  Katz) and 30 as variants (Daniel Perez-Rouzan / Daniel Rouzan,
  Cam Hiatt / Cameron Hiatt, Mike Collins / Michael Collins); every one was checked
  by hand. 105 players are created; none shares a name part with an unmapped
  teammate, and the two that share a surname with a mapped one (a different ESPN
  id) are different people. The 12 games still unresolved are nicknames no rule
  covers (Obie / Owen Bronston, Bear / Jeremiah Cherry), initials (BJ Roy, TJ Wal,
  JR Kemm) and newcomers held back because an unmapped teammate shares a name part
  (Keeper Jackson beside Jaden Jackson, Miles White beside Miles Elliott). On every
  pair of different players of one team-season in the local history (415,619 pairs)
  58 are spelling variants of each other, nearly all NatStat's duplicate rows of one
  person (Cameron Boozer / Zameron Boozer); where two real people are variants of one
  line, the uniqueness rule pairs neither.

**Not done (later slices of step 5):**

- **The precedence flip** above, after §5.
- **Moving the exclusion check to `espn_id`**: `data/excluded_games.json` is keyed on
  NatStat ids, so a created game is never checked against it. Creation needs both
  sides D1, which keeps out the non-D1 exhibitions most entries are, but a D1 game
  on the list could still be created.
- **A schedule from CBBD**: future games still come from NatStat's
  `schedule_refresh`; an outage freezes the schedule (not the results).
- **Class year and roster metadata for created players** (§4.6): they carry name,
  team and ESPN id only, like a NatStat box-created player.
- **A created player NatStat names differently past squashing** (Josh / Joshua)
  becomes a second row when NatStat lists him; NatStat's lines replace the CBBD
  lines of every game it sends, so the created row keeps only the games NatStat
  never had, and `espn_ids` will not move the ESPN id (it never overwrites). Rare,
  since creation already refuses those names when NatStat has the player first. The
  fix is a merge pass: a NatStat-id player whose box rows vote for an ESPN id a
  placeholder row holds on the same team absorbs that row (the
  `deduplicate_players` body).
- **NatStat's own duplicate rows of one player** (C.J. Smith / CJ Smith at
  Arkansas-Pine Bluff in 2027, each with box rows): `name_key` makes them the same
  key, so a line naming him matches both and pairs neither, and `map-espn-ids`
  maps only the row whose box lines pair. Merging the two rows is a player merge,
  not a name rule.
- **Half-boxed NatStat games**: when `player_perfs` fails and `team_perfs` succeeds,
  the fill leaves those games alone (any box row counts as boxed). The `box_scores`
  notes count them; filling the missing side is a later change.
- **The self-heal still keys on NatStat's three steps**, so a NatStat outage longer
  than `MAX_HEAL_DAYS` that CBBD covered still posts "self-heal only PARTIAL": the
  NatStat re-pull it asks for is still wanted, but the site is not missing those
  games.
- **`bootstrap_csv` inserts games and players without adoption**; it only loads
  past seasons, which hold no created rows.
- **`simulate` fixtures from CBBD** (§4.8): the harness still replays NatStat-shaped
  fixtures, and the CBBD steps skip under its clock. It now reads the NatStat box
  failures off the report (they no longer abort) and counts them as hard failures.

Each cutover updates `SERVED_CRITICAL` in `crates/cstat-api/src/routes/health.rs`,
`preflight` (ping CBBD, not NatStat), and the nightly step names in
`docs/deploy_nightly_cron.md`.

### 4.6 Class year and roster metadata

CBBD rosters have no class year, and class year is load-bearing: the departure
inference deletes `class_year = 'Sr'` returners, and the projection models read it.
Sources, in order: Torvik's `class_year` (already ingested for every player with a D1
game), ESPN's team-roster `experience`, and the official-roster ingest
(`team_roster_players.class_year_raw`). Height, weight, hometown and jersey come from
CBBD `/teams/roster`. The NatStat `players` endpoint's known flaw (current roster only,
no historical filter) goes away with it.

### 4.7 Cross-season joins move to `espn_id`

After the cutover is stable: replace `natstat_id OR torvik_pid` with
`espn_id OR torvik_pid` in the training joins and `queries::resolve_*_id_for_season`,
with `training/test_cross_season_joins.py` extended to prove no join loses rows.
Retraining after this is the usual `retrain_downstream.sh` run, and
`check_provenance.py` will flag every node whose inputs moved.

### 4.8 The `simulate` harness

Its fixtures are NatStat-shaped `api_cache` rows. Once ingestion reads CBBD, the
harness needs CBBD-shaped fixtures, or it tests a path prod no longer runs. Do this
with step 5 of §4.5, not after.

## 5. Shadow-run acceptance and retiring NatStat

Run the shadow ingest and the parity report nightly for **at least three weeks of
regular-season games** (~1,000 games), then cut over feed by feed (§4.5) when:

- every finished D1 game in the window is present in CBBD by the next nightly;
- final scores agree on every game both sources have (a disagreement gets
  investigated, not averaged);
- team box columns agree except where NatStat is known wrong (truncated minutes,
  starters, DREB);
- the `simulate` replay's team ratings and CAM match prod within the noise of a
  nightly recompute.

**After the last feed cuts over:** remove the NatStat client calls, `NATSTAT_API_KEY`
and `NATSTAT_MAX_PER_HOUR`; keep every `natstat_id` column (historical rows and
trained models reference them, and dropping columns buys nothing); then cancel the
NatStat subscription after two clean weeks with nothing calling it.

## 6. Open decisions

1. **Re-ingest 2015–2026 from CBBD, or keep NatStat history?** Recommended: keep it.
   Map ids only. Revisit if the parity report shows NatStat history is wrong in ways
   that matter (the swapped-roster and phantom-id repairs suggest it sometimes is).
2. **Elo: CBBD's, or our own?** Decided by the §4.4 walk-forward test.
3. **Tier:** Tier 1 ($1) ongoing, plus a month of Tier 2/3 for the backfill.
4. **When to start:** the shadow run can begin as soon as §4.1–4.3 land; it needs no
   NatStat call, so it does not wait on the suspension lifting.
