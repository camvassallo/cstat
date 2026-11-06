# Season rollover: page defaulting plan

Status: P0 and P2 implemented 2026-11-01; P1 partly (one `opening` flag rather than per-surface readiness). Written the night the first 2027 game (Villanova–Notre Dame, Nov 1) landed and flipped the site.

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

**Answer to "will tomorrow's rankings be based on only the first day's games?": they were going to be.** After the Nov 3 nightly every team would have had one game, and `compute_adjusted_efficiency` rated each from that game alone, with no prior and an opponent adjustment that cannot work on 180 disconnected pairs. The preseason blend that already existed (`PRESEASON_PEAK_WEIGHT`, `projection.rs`) applies only to game predictions.

**Measured, then shipped (P2).** `training/experiments/experiment_rankings_preseason_prior.py` replays every season 2016–2026 date by date, with the Rust iteration ported to numpy, and scores each candidate's rating difference on the next game, walk-forward over 2021–2026 (summary: `training/eval_history/rankings_preseason_prior_20261101_summary.json`, 25,547 scored games):

| games played (less-experienced side) | raw | preseason only | post-hoc blend | prior inside the adjustment |
|---|---|---|---|---|
| 0 | 14.41 | 10.19 | 10.16 | **10.12** |
| 1 | 13.43 | 9.47 | 9.44 | **9.39** |
| 2–4 | 10.99 | 9.73 | 9.63 | **9.48** |
| 5–9 | 9.58 | 9.69 | 9.22 | **9.21** |
| 10–14 | 9.22 | 9.50 | 8.97 | **8.96** |
| 15–19 | 8.95 | 9.51 | 8.79 | **8.78** |
| 20+ | 9.02 | 9.54 | **8.83** | **8.83** |

Next-game MAE in points; every blended cell beats raw at z ≤ −4.9. Ranking quality reads the same way: Spearman against the season's final ratings a week in is 0.61 raw, 0.89 for the projection alone, and 0.91 for the blend.

What it settled:
- **The prior goes inside the opponent adjustment**, not on top of it. Each game is adjusted against the opponent's blended rating, so an early schedule is judged against what is known about the opponent rather than one game of it. It beats the post-hoc blend everywhere the two differ (the first nine games).
- **K = 12 games, tapered to zero at 32.** A team's prior weight is `K / (K + games)` rescaled to reach zero at a full season (`compute::tapered_prior_weight`): 0.89 after its first game, 0.60 at five, 0.38 at ten, 0.14 at twenty, none at thirty-two. K = 12 in five of six walk-forward folds.
- **It tapers, because a finished season is the record of what happened.** An untapered K = 6 predicts the next game very slightly better (9.051 vs 9.057 MAE, at most 0.02 past twenty games) but leaves a final rating 16% preseason guess, and showing raw for finished seasons instead made every rating jump when a season closed. With the taper a season with no games left carries no prior, so its blended columns equal its raw ones (`experiment_rankings_prior_taper.py`).
- **It is the site's only rating**, for finished seasons as well as the one in progress, so a team reads the same on every page.

How it is served:
- `compute_adjusted_efficiency` runs its solve twice: once raw, which is byte-identical to before (verified on all 364 rows of 2026), and once with the prior. The second solve writes `team_season_stats.blended_adj_offense` / `_defense` / `_efficiency_margin` and `preseason_weight` (migration 061). The raw columns are untouched on purpose: they feed the game model, CamPom's schedule adjustment and player SOS, all trained on the raw number.
- `queries::team_ratings_cte` is the one definition of a team's displayed rating, rank and SOS: the rankings, team page, conference pages, Schedule tab and social cards all read it, falling back to raw per team for a season computed before the columns existed. The raw columns are never shown as a rating. They appear only where a projection is graded against what happened (the Actual column for a finished season, and the graded coach rows).
- The prior's O/D split is symmetric around the league average, because `team_preseason_projection` stores only a margin. The O and D columns are therefore less informed than the margin in the first weeks. Storing the projected O/D halves would fix that, and is not done here.

Caveat: the historical `team_preseason_projection` rows are in-sample at Layer 2 (#274), so K is if anything slightly high. The sibling margin experiment found the same caveat immaterial (slope 0.600 ex-ante vs 0.595).

The game-prediction blend uses the same curve by games played, at half the strength (`projection::prediction_prior_weight`), because the model leg has already seen the season. At full strength it loses clearly; at half it is at least as good as the calendar schedule it replaced (MAE 8.762 vs 8.769, z -1.5, `training/experiments/experiment_prediction_prior_curve.py`).

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

**P2 (shipped, see the Rankings section):** the fitted blended rankings, K = 12 tapered, walk-forward, in `team_season_stats.blended_*`, with a results-only switch. Still open: unifying the game-prediction prior.

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

Still open: per-surface `ready` flags (P1 has a single `opening` today; players and Portle will lag rankings). P2 has since replaced the rankings half of `opening`: the home page reads blended ratings from the first box score on.
