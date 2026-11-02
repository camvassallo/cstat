# Season rollover: page defaulting plan

Status: P0 implemented 2026-11-01 (see "What shipped in P0" below); P1 partly, P2 open. Written the night the first 2027 game (Villanova–Notre Dame, Nov 1) landed and flipped the site.

## What happened

`/api/seasons` decides "newest played" by the first game with a final score (`routes/seasons.rs::played_seasons`). One game was enough:

    before:  seasons [2026, ...]        default 2027 (upcoming)   upcoming 2027
    after:   seasons [2027, 2026, ...]  default 2027              upcoming null

Every page that reads `useSeason()` with no `?season=` now asks for 2027 *actual* data, and 2027 has almost none: one game, with its box score not yet ingested (Villanova's 2027 row has no W-L and an empty roster). The projection data for 2027 is all still there and correct; nothing routes to it any more.

The frontend's `upcoming` is now `default + 1 = 2028` (`lib/upcomingSeason.ts` step 2), a season with no projection rows, so every picker that prepends `upcoming` offers a 2028 that is empty or errors (`/api/projections/2028` is a **500**, not a 404).

Live API state at the time of writing:

| endpoint | 2027 result |
|---|---|
| `/teams/rankings` | `teams: []` (no team has an AdjEM) |
| `/players` | `total: 0` |
| `/transfers/2027` | 404 |
| `/recruits/2027` | 200, but that is the class of 2027, who play in 2028 |
| `/portle/daily` | `natstat_id: null` (pool needs GP >= 5 and a CAM) |
| `/archetypes`, `/lineups` | empty |
| `/conferences/ACC` | 404 |
| `/draft/2027` | 404 |
| `/projections/2027`, `/projected-players/2027` | full and correct |

## The missing concept: a season can be *played* without being *ready*

Today there are two states, "upcoming" (projection) and "played" (actuals). The first weeks of a season are a third state, **opening**: games exist, but actual-stat surfaces are empty or noise. Each surface becomes ready at a different time:

| surface | ready when | roughly |
|---|---|---|
| team rankings (raw AdjEM) | the schedule graph is connected and teams have several games | late Nov to Dec |
| team rankings (blended, see below) | immediately | day 1 |
| player stats / CAM | Torvik per-game file published and players clear the GP floor | 1–2 weeks |
| Portle pool | enough players with GP >= 5, a CAM and an archetype | 2+ weeks |
| archetypes, lineups | enough minutes | 2–3 weeks |

So readiness has to be per surface, and it has to come from data rather than the calendar (the repo already rejects calendar constants for this; see `prefer_upcoming`).

**Proposal.** Extend `/api/seasons` with an explicit current-season block, computed from the DB:

    {
      "seasons": [2027, 2026, ...],      // played, unchanged
      "default": 2027,
      "upcoming": null,                  // unchanged: projected AND unplayed
      "current": {
        "season": 2027,
        "phase": "opening",              // "offseason" | "opening" | "in_season"
        "ready": { "rankings": false, "players": false, "portle": false,
                   "archetypes": false, "lineups": false }
      },
      "fallback_stats_season": 2026      // newest season whose actuals are complete
    }

And stop the frontend from inventing `default + 1`. When `upcoming` is null there is no forward season, and pickers should not offer one.

## Page by page

Each row gives the default with no `?season=`, during the opening phase. Explicit `?season=` links keep working as they do today.

| page | today (broken) | opening-phase default | ready / in-season |
|---|---|---|---|
| **Rankings `/` (home)** | empty 2027 table | 2027 **blended** board once that exists. Until then, the 2027 projection board (`ProjectionView year=2027`) labelled as preseason | 2027 actual table |
| **Players `/players`** | empty 2027 list | 2027 projected players (`ProjectedPlayers year=2027`, 3,421 rows) | 2027 actual list |
| Players `?mode=transfers` | `/transfers/2027` 404 | `/transfers/{season - 1}` = the 2026 portal, i.e. who moved *into* the 2027 season | same rule |
| Players `?mode=recruits` | shows class of 2027 (plays 2028) | `/recruits/{season - 1}` = class of 2026, the freshmen playing now | same rule |
| Players `?mode=draft` | `/draft/2027` 404 | newest board that exists (2026), labelled "2026 draft" | a 2027 board once one exists |
| **Portle** | no puzzle | stays on 2027 and says it is waiting on 2027 archetypes, with a link to today's 2026 puzzle | 2027 |
| Which Class | empty 2027 pool | 2026 until `ready.archetypes` | 2027 |
| Archetypes | empty | 2026 until `ready.archetypes` | 2027 |
| Lineups | empty | 2026 until `ready.lineups` | 2027 |
| Conference | 404 | 2027 standings from the conference projection plus actual W-L as it lands | 2027 actual |
| Team detail | Actual view, empty | Projected view (it already shows the schedule), with the Actual toggle once the team has played | Actual by default |
| Player detail | follows `available_seasons` | unchanged, already safe | unchanged |
| Predict | team list comes from `fetchTeamRankings(2027)`, which is empty, so **no teams to pick** | team list from `teams` for the season, not from rankings. Predictions already blend with the preseason anchor | unchanged |
| Projected | works for 2027; picker offers a broken 2028 | 2027, with no 2028 entry | 2027 retrospective; the next season appears when its projection rows exist |
| Schedule, ticker | fine (date-driven) | unchanged | unchanged |
| Coaches | season board for 2027 | unchanged (career mode is season-agnostic; the season board publishes `available_seasons`) | unchanged |

**Navbar.** Forecast → Projected hardcodes `/projected?season=2027` (`Layout.tsx`). It should be `/projected` and let the page resolve the season. Nothing else in the navbar carries a season.

## Transfers and recruits: one convention

Both feeds are keyed by class year, and the class year is **one less than the season the players play in** (the `transfers/2026` rows link destinations to `year + 1 = 2027`, and `recruits/2026` returns `base_season: 2027`). The Players page passes the site season straight through, which has been wrong since #394 made the default follow the upcoming season. That means transfers broke at the same moment the default moved to 2027, not just tonight. The fix is a single mapping, `classYearFor(season) = season - 1`, applied in `Players.tsx` before `TransferPortal` and `RecruitClass`, with the picker labels saying "2026 portal" / "class of 2026".

## Rankings: blend the preseason projection in, weighted by games played

**Answer to "will tomorrow's rankings be based on only the first day's games?": yes, as the code stands.** After the Nov 3 nightly (09:30 UTC, after the 181-game Nov 2 slate) every team will have one game, and `compute_adjusted_efficiency` will rate each one from that game alone. There is no prior. The opponent adjustment is also nearly meaningless when the schedule graph is 180 disconnected pairs, so the table will effectively rank single-game margins: a 40-point win over a non-D1-calibre opponent ranks first. The preseason blend that exists today (`PRESEASON_PEAK_WEIGHT` 0.70, decaying over 42 days, `projection.rs:709`) applies only to *game predictions*, not to the rankings table.

What to build:
- `blended_adj_em = w(g) * preseason_adj_em + (1 - w(g)) * raw_adj_em`, and the same for AdjO/AdjD, where `g` is the team's games played and `w(g) = K / (K + g)`, i.e. the preseason projection counts as K games of evidence.
- **K is fitted, not guessed.** `team_preseason_projection` has historical rows back to 2016, and `team_game_stats` can replay each season's AdjEM after g games. Fit K walk-forward (`training/walk_forward.py`) on next-game margin prediction, or on end-of-season AdjEM. Per-team games played is the right axis rather than date, because teams open on different nights.
- Where it lives: a column (or columns) on `team_season_stats` written by `compute_all` next to the raw AdjEM, so the rankings query stays a single read and the raw value is still there for the "actual" view. The rankings table shows the blended number during opening, with a visible "includes preseason projection" note and a raw/blended toggle.
- Once `w` is small for nearly every team, the board is effectively raw and `phase` moves to `in_season`.

Open question: should the game-prediction blend switch from its date-based weight to the same games-played weight? Two inconsistent priors on one site will eventually show up as a ranking and a prediction that disagree.

## Portle specifics

Decision: Portle does **not** fall back to last season. It stays on the season being played and, while that season's pool is empty, says so ("waiting on 2027 archetypes to establish") with a link to today's puzzle for the previous season. That keeps the season switch out of the daily pin entirely: the first 2027 puzzle simply appears the first day the pool can supply one. The notes below are why a silent fallback would have been the riskier choice.

- The daily pin is keyed `(mode, season, date)` (`queries::pick_or_pin_daily_puzzle`). Changing which season Portle uses **mid-day** changes the answer for anyone who has already played today. The switch from 2026 to 2027 has to happen at a date boundary: the readiness check runs once per date, not per request.
- Nothing bad has been pinned tonight. The 2027 insert selected from an empty pool, so no row was written, and the route returned `natstat_id: null`.
- Readiness threshold: switch when the 2027 eligible pool for a mode reaches some fraction of that mode's 2026 pool (say 60%), so the first 2027 puzzles aren't drawn from a few dozen high-minute starters.
- The backend `daily_puzzle` default is `current_natstat_season()` (calendar). It should use the same readiness-resolved season as the frontend, so a client that omits `season` gets the same puzzle.

## Sequencing

**P0, before the Nov 3 nightly** (frontend only, small, all reversible):
1. Rankings: when the season is the current one and `rankings` is not ready (stopgap test: the rankings response is empty, or the season is within its first N days), render the projection board for that season with a preseason label.
2. Players: same stopgap, default to `ProjectedPlayers year=2027`.
3. Transfers and recruits: `season - 1` class-year mapping.
4. Portle: pin to 2026 until ready.
5. Pickers: drop the invented `default + 1` forward season; fix the navbar `/projected?season=2027` hardcode; make `/projections/{year}` return 404 instead of 500 for a season with no rows.
6. Predict: team list from the season's teams, not rankings.

Caveat on P0 item 1: if raw single-game AdjEM lands before P1, the stopgap must key off something other than "rankings response is empty". Otherwise it will hand the home page to the noisy table on Nov 3. A games-played floor (median team GP) is the safe test.

**P1, this week:** the `current.phase` / `ready` block in `/api/seasons`, replacing the P0 stopgap heuristics with server-computed readiness, plus tests at the boundaries (zero games, one game, threshold).

**P2:** the fitted blended rankings (K by walk-forward), its `team_season_stats` columns, and the raw/blended toggle. Then decide on unifying the game-prediction prior.

## Also noticed

- The Villanova–Notre Dame box score has not landed (both teams' 2027 rows have no W-L and empty rosters), so rankings would be empty for this game even with a working AdjEM. Check the next nightly's `games` / `player_perfs` steps for that game_id before assuming it is a code issue. It may be NatStat lag on an overseas game.
- `torvik_games` is failing because barttorvik 302s a not-yet-published per-game file instead of 404ing; covered separately.

## What shipped in P0

The `opening` signal went server-side straight away rather than as a frontend heuristic, because the heuristic the plan worried about ("rankings response is empty") would have broken on Nov 3:

- `/api/seasons` gains `opening` (median box-score games per team in the newest played season below 5, or no box score at all) and `stats_season` (the previous played season while opening). `routes/seasons.rs::OPENING_MEDIAN_GAMES`.
- `/api/projections/{year}` answers 404, not 500, when the base season has no team ratings.
- Frontend `upcoming` falls back to the API default instead of `default + 1`, so no picker offers an empty 2028.
- Rankings, Players and the team page lead with the season's projection while `opening`. Players has a "show this season's stats so far" opt-out (`?view=actual`), and the team page's Actual toggle still works.
- Transfers and recruits use class year `season - 1`. Draft falls back to the newest existing board for the current and upcoming seasons.
- Archetypes, lineups, Which Class and conference pages default to `stats_season` when no `?season=` is given.
- Portle: the archetypes-pending message described above.
- The navbar Future link is built from `upcoming` instead of a hardcoded year.
- Predict borrows the previous season's team names when the season has no rated team yet.

Still open: per-surface `ready` flags (P1 has a single `opening` today; players and Portle will lag rankings), and the blended rankings (P2). Once `opening` turns false at a median of 5 games, the home page shows raw AdjEM until P2 lands.
