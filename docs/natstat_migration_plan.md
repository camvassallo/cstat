# NatStat → CollegeBasketballData migration plan

*Written 2026-11-20. Source comparison and spot checks done the same day; the evidence
is in §1. This is the ordered plan for replacing NatStat as cstat's box-score, schedule
and play-by-play source.*

**Status:** §4.1–4.3 are built (`crates/cstat-ingest/src/cbbd.rs`, `cbbd_shadow.rs`,
`source_parity.rs`, migration 075, the nightly's `cbbd_shadow` step), and so is the
ESPN-id mapping of §3 (`espn_ids.rs`, migration 076, the nightly's `espn_ids` step).
The shadow run starts once `CBBD_API_KEY` is set on the cron service. Nothing has cut
over; §4.5 is the agreed order.

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
- **Players pair exactly as `source-parity` pairs them** (`name_match::pair_box_names`:
  normalized name, then a last name unique on both sides; no jersey step, since
  `player_game_stats` has no jersey), with the game's sides oriented by the score
  rather than ESPN's flip, because CBBD's home and away do not always follow ESPN's.
  Non-D1 sides are skipped.
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
arrive with the play-by-play cutover instead. It changes nothing a page
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
3. **Gap-fill box scores.** Write CBBD's box scores into `player_game_stats` /
   `team_game_stats` only for games NatStat has none for (the four-day NatStat lag,
   and the suspension), joining on `espn_id`. Rebounds, turnovers and fouls on the
   team row come from CBBD's player sums, for the reason in step 1. NatStat stays
   primary wherever it has the game.
4. **Play-by-play and lineups from CBBD.** NatStat's November format change removed
   clock, tags, on-floor players and ids, and the lineup / on-off / RAPM chain has been
   running on reconstruction. With `onFloor` on every play, `compute_pbp_lineups`
   reads lineups directly. Plays join on `games.espn_id` and `players.espn_id`.
5. **CBBD as the primary box source**, NatStat as the gap-filler (`games`,
   `player_perfs`, `team_perfs`, `schedule`). New rows carry `espn_id`; `natstat_id`
   is null for them, so every reader that assumes `natstat_id` is set has to be found
   first (the §3 inventory). The `parse_game` exclusion check moves to `espn_id`
   (re-key `data/excluded_games.json`; it is `include_str!`-compiled, so this ships
   with a deploy).
6. **Elo**, once §4.4 decides it.
7. **Retire NatStat**: teams and player metadata (§4.6); lines (drop the NatStat
   `game_lines` fallback, CBBD `/lines` takes its place under ESPN); news (the social
   bot's `news` job reads ESPN's news feed, which also removes the second NatStat
   caller that likely contributed to the 2026-11-20 IP-block suspension). Then §5's
   retirement.

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
