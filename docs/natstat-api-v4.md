# NatStat API v4 — MBB Reference

> Extracted from natstat.com/api-v4/docs, natstat.com/api-v4/endpoints (login required), and api_doc.txt.
> API Version: v4 BETA 1 (2025.12.10). Scheduled for full release March 2026.

## Differences From v3

- **Hierarchical endpoints**: Three primary (`teams`, `games`, `players`), 22 secondary, 6 reference
- **Stackable endpoints**: Hydrate primary endpoints with secondary data via semicolons (e.g., `games;playbyplay,lineups`)
- **Reference endpoints**: `datastatus` (metadata + known data issues), `seasons`, `teamcodes`, `gamecodes`, `playercodes`, `glossary`
- **New endpoint**: `text` — generated game previews/summaries from Interstat Pressroom
- **Backward-compatible** from v3, but may return less data. Adjust queries for v4's more flexible methods

## General

- **Base URL:** `https://api4.natst.at/`
- **Method:** GET (all requests)
- **Service code:** `mbb` (NCAA Men's Division I Basketball, case-insensitive)
- **Full data back to:** 2007 season (159K games, 2.2M performances, 3.4M play-by-play records per `/datastatus`)

## Authentication

API key is embedded in the URL path (not a header):

```
https://api4.natst.at/{apikey}/{endpoint}/{service}/{range}/{offset}
```

- Key format: 11 characters — four alphanumeric, dash, six alphanumeric (e.g., `57c1-0f9765`)
- Found on your NatStat account page or under **Analysis Tools > NatStat API > Query Builder**

## URL Structure

| Segment | Required | Description |
|---------|----------|-------------|
| `{apikey}` | Yes | Your API key |
| `{endpoint}` | Yes | e.g., `games`, `players`, `teams` |
| `{service}` | Yes | `mbb` for college basketball |
| `{range}` | No | Comma-separated: season, date(s), codes, format, search term |
| `{offset}` | No | Pagination offset. Use `_` as range placeholder when no range needed |

## Rate Limits

| Account | Limit | Reset | Concurrency |
|---------|-------|-------|-------------|
| Standard | 500 calls/hour | Top of each hour | Max 4 queries/sec |
| API+ | 100,000 calls/day | 12:01 AM ET daily | Unlimited |

- Standard accounts are limited to a certain number of different IP blocks within 24 hours
- Rate limit status is returned in the `user` node of every response:
  - `ratelimit` — total limit
  - `ratelimit-remaining` — calls remaining
  - `ratelimit-timeframe` — "hour" or "day"
  - `ratelimit-reset` — datetime of next reset
  - `throttle-level` — seconds of automatic throttle (0 = none). System may throttle to protect service.

### Multiple IP Policy

Each API key is allowed access from up to **4 IP blocks** (`X.X.-.-`) within 24 hours (`ip-last24` in meta node). Accessing from more than 6 IP blocks triggers an automatic 24-hour suspension.

### API Abuse Policy

Three-strike system:
1. Written warning + manual key reset
2. Key revoked, access suspended (can cancel with refund)
3. Permanent termination, no refund

Triggers: excessive concurrent requests, >4 queries/sec, improperly written code affecting other users, replicating NatStat's services.

### NatStat API+

Annual or non-expiring upgrade add-on (requires existing NatStat subscription):
- 100,000 API calls/day (resets 12:01 AM ET)
- No IP or concurrency restrictions (enables cloud/rotating IP use)
- Early access to new features and closed beta endpoints
- If underlying subscription lapses, API+ is suspended; expires 365 days after purchase
- Still subject to abuse policy regarding excessive concurrency affecting service availability

## Response Format

Default: JSON. Also supports XML, PHP SimpleXML (specify in range: e.g., `/xml`).

All timestamps are in **US Eastern (New York)** timezone.

Response nodes:
- `success` — 1 or 0
- `error` — error code string (empty if success)
- `warnings` — non-fatal warnings
- `user` — rate limit info
- `meta` — pagination, processing time, reference URIs
- `results` — the actual data (array or object)

## Pagination

- Max results per page: usually 100 (some endpoints 500)
- `meta` node includes: `results-max`, `results-total`, `page`, `pages-total`, `page-next`
- To paginate: add offset as the 5th path parameter
- When no range is used, place `_` in the 4th position: `/.../mbb/_/100`

## Error Codes

| Code | Description |
|------|-------------|
| `AUTHORIZE_FAILED` | Key is incorrect, invalid, or expired |
| `UNAUTHORIZED` | Invalid key |
| `NO_ENDPOINT` | No valid endpoint specified |
| `PARAMETER_MISSING` | No valid sport/level path parameter |
| `CONCURRENCY` | More than 4 queries/sec (API+ removes this) |
| `OUT_OF_RANGE` | API key not valid for the sport requested |
| `OUT_OF_CALLS` | Rate limit exceeded |
| `TOO_MANY_IPBLOCKS` | Too many different IP blocks in 24 hours |
| `APIPLUS_ONLY` | Premium feature requiring API+ |
| `BLOCKED` | API key suspended or revoked |
| `UNDER_MAINTENANCE` | API undergoing maintenance |

## Endpoint Hydration

Primary endpoints can be hydrated with secondary endpoints using a semicolon:

```
https://api4.natst.at/{key}/games;playbyplay,lineups/mbb/2026-03-15
```

| Primary | Valid Secondary Endpoints |
|---------|--------------------------|
| `teams` | `games, players, stats, teamperfs, transactions, news, videos, text` |
| `players` | `seasons, playerperfs, playbyplay, projline, transactions, news, videos` |
| `games` | `playbyplay, lineups, text, boxscores, forecasts[elo, moneyline, overunder, simulations, pointspread, winprob], projline` |

Auto-populated nodes (always included):
- **games** → `players, playerperfs, venue, officials`
- **players** → `current_season`
- **teams** → `elo, tcr`

## Endpoint Aliases

| Endpoint | Alias(es) |
|----------|-----------|
| `datastatus` | `status` |
| `forecasts` | `forecast` |
| `games` | `game` |
| `teams` | `team` |
| `pointspread` | `spread` |
| `playbyplay` | `pbp` |
| `seasons` | `season` |
| `transactions` | `trans` |

---

## Endpoints (All 30 MBB-Compatible)

### Primary Endpoints

#### /teams
List of all teams (without code) or full metadata for a specific team (with code). Returns ELO rating, seasons played.

**Max results:** 100

**Range params:** `dataformat`, `season`, `teamcode`, `leaguecode`, `search`

```
/teams/mbb/              # All teams
/teams/mbb/DUKE          # Duke metadata
/teams/mbb/2026          # All teams for 2026 season
```

#### /players
List of all players (without code) or full metadata + game performances for a specific player (with code + season).

**Max results:** 100

**Range params:** `dataformat`, `season`, `teamcode`, `leaguecode`, `playercode` (numeric), `search`

```
/players/mbb/            # All players
/players/mbb/12345678    # Specific player
```

#### /games
List of games in reverse chronological order. With game code: full metadata, scoring plays, in-game score datapoints, play-by-play, forecasts summary.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date` (YYYY-MM-DD), `daterange` (two dates), `teamcode`, `leaguecode`, `gamecode` (numeric), `search`

```
/games/mbb/                            # Recent games
/games/mbb/2026-03-15                  # Games on date
/games/mbb/2026-03-15,2026-03-22       # Date range
/games/mbb/DUKE                        # Duke's games
/games;playbyplay,lineups/mbb/12345    # Hydrated single game
```

### Secondary Endpoints

#### /playerperfs
All player performances (box scores) in reverse chronological order. Filter by season, player, game, team, date range.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `playercode`, `gamecode`, `search`

#### /teamperfs
All team performances in reverse chronological order.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /playbyplay
Play-by-play entries. Alias: `pbp`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `playercode`, `gamecode`, `search`

#### /lineups
Recent lineups. Filter by team or season.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /boxscores
Flat-text boxscores for games.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `playercode`, `search`

#### /stats
Statistical rankings tables. Use `/glossary/mbb` for stat code dictionary.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

Add `player` or `team` and a stat code in the range. Default is `team`.

```
/stats/mbb/player,ppg    # Player PPG rankings
/stats/mbb/team,fgm      # Team FGM rankings
```

**Note:** Stats re-tabulated nightly at ~3 AM ET during season.

#### /elo
Current ELO rankings (without range), historical snapshot (with date), or detailed ELO changes (with team code).

**Max results:** 100

**Range params:** `dataformat`, `season`, `teamcode`, `leaguecode`

#### /forecasts
ELO, computer simulations, money line and over/under forecasts. Alias: `forecast`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /winprob
ELO win probability forecasts. Alias: `winprobs`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /simulations
Monte Carlo and stat-based game simulations. Not every game can be simulated.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /moneyline
Betsson money line data (American format). Alias: `moneylines`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `playercode`, `search`

#### /pointspread
Betsson point spread data. Alias: `spread`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `playercode`, `search`

#### /overunder
Betsson over/under projections. Alias: `overunders`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /projline
Projected player statlines for today's games. Alias: `projectedstatlines`.

**Max results:** 100

**Range params:** `dataformat`, `playercode`

**Note:** Only returns today's games. Use Interstat API for other days.

#### /events
Real-time in-game events (runs, late lead changes, 30/40/50 point games). **Only last 24 hours.**

**Max results:** 500

**Range params:** `dataformat`, `teamcode`, `leaguecode`, `search`

#### /transactions
All transactions in reverse chronological order (transfer portal, etc.). Alias: `trans`.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /officials
Official/umpire list or metadata + game record for a specific official.

**Max results:** 100

**Range params:** `dataformat`, `season`, `date`, `daterange`, `leaguecode`, `search`

#### /venues
Venue list or metadata + games played at a specific venue.

**Max results:** 100

**Range params:** `dataformat`, `season`, `venuecode` (numeric), `search`

#### /news
News items in reverse chronological order.

**Max results:** 100

**Range params:** `dataformat`, `date`, `daterange`, `teamcode`, `leaguecode`, `search`

#### /videos
YouTube and Twitter videos in reverse chronological order. Alias: `video`.

**Max results:** 100

**Range params:** `dataformat`, `date`, `daterange`, `search`

#### /text
Combined node with all associated game text/stories.

**Max results:** 100

**Range params:** `dataformat`

### Reference Endpoints

#### /glossary
Statistical abbreviation explanations.

```
/glossary/mbb/
```

#### /seasons
List of all seasons with stat granularity availability. Alias: `season`.

```
/seasons/mbb
```

#### /teamcodes
List of team codes for use in other queries.

**Max results:** 500

**Range params:** `dataformat`, `season`, `search`

#### /playercodes
List of player codes for use in other queries.

**Max results:** 100

#### /leaguecodes
List of league codes for use in other queries.

**Max results:** 500

**Range params:** `dataformat`, `season`, `leaguecode`, `search`

#### /datastatus
Metadata about service database status and known data issues. Alias: `status`. Works on both v3 and v4.

**Range params:** `dataformat`, `season`

Returns: `status` (current season, season day, qualifier criteria), `totals` (games, performances, play-by-play, news, transactions, tweets, videos, data-back-to year), `user` (account tier, rate limits).

```
/datastatus/mbb/    # MBB data status
```

As of 2026-04-09 for MBB: 159,537 games, 2.2M performances, 3.4M play-by-play, data back to 2007.

---

## Field Mapping Quirks

These are non-obvious behaviors discovered through data validation:

### `reb` = Total Rebounds (NOT Defensive)
Per the official `/glossary/mbb` endpoint: `reb` (code `reb`, abbrev `REB`) = **"Total Rebounds"**. This applies to both `playerperfs` and `teamperfs`. Defensive rebounds must be derived: `dreb = reb - oreb`. Verified by cross-referencing against ESPN box scores (e.g., Tobe Awaka vs Utah Tech: NatStat `reb=18, oreb=8` → 18 total, 10 def).

Related glossary entries:
| Code | Abbrev | Name | Type |
|------|--------|------|------|
| `reb` | REB | Total Rebounds | player |
| `oreb` | OREB | Offensive Rebounds | player |
| `dreb` | DREB | Defensive Rebounds | player |
| `reb-d` | REB-D | Total Rebounds (Defense) | team |
| `oreb-d` | OREB-D | Offensive Rebounds (Defense) | team |
| `dreb-d` | DREB-D | Defensive Rebounds (Defense) | team |
| `orbpct` | ORB% | Offensive Rebound % | team |
| `drbpct` | DRB% | Defensive Rebound % | team |
| `rebpct` | REB% | Rebound % | player |

### `dreb` Field (Player Only, Conditional)
When `reb` is populated, `playerperfs` also includes a `dreb` (defensive rebounds) field. When `reb=0` (missing), the `dreb` key is entirely absent. `teamperfs` never includes a `dreb` field — defensive rebounds must be derived from `reb - oreb`.

### ~68% of Games Have No Rebound Data
Both `playerperfs` and `teamperfs` return `reb=0` for ~68% of games even when `oreb > 0`. This is **missing data at the source**, not zero total rebounds — confirmed via live API curl (2026-04-09). The missing data is per-game (all-or-nothing): within a game, either all players have `reb` or none do. 4,248 games missing vs 2,025 present in 2026 season. No team/conference/date pattern — scattered across the entire season.

### `usgpct` = Whole Number Percentage
NatStat returns `usgpct` as whole numbers (e.g., `19.5` for 19.5%). Divide by 100 before storing as a decimal.

### `/playercodes` Returns Different Codes Across Seasons
The same physical player can have different `player-code` values in different seasons (e.g., Caleb Foster on Duke: `57987927` in 2025, `87832246` in 2026). This creates duplicate player records. ~989 duplicate pairs observed in 2026 data, mostly from opening-week games. Deduplication by `(name, team_id, season)` is required post-ingestion.

### `teamperfs` Stats Are Nested Under `stats`
Unlike `playerperfs` where stats are flat on the performance object, `teamperfs` nests all stat fields under a `stats` key.

### ELO: Only Rank Available
The `/teams` endpoint's `elo` object only provides `.rank` (ordinal 1-364), not the actual ELO rating value.

---

## API Version History

| Version | Date | Base URL | Notes |
|---------|------|----------|-------|
| v4 BETA 1 | Dec 2025 | `api4.natst.at` | Hierarchical endpoints, stackable hydration, `text` endpoint |
| v3.5 | May 2025 | `api3.natst.at` | Added `events`, `projline` endpoints; search improvements |
| v3.0 | May 2024 | `api3.natst.at` | Simplified query structure; deprecated `rpi`, `social`, `tweets`; `id` → `code` terminology |
| v2 | May 2022 | — | Removed CSV/TSV/Excel export; added `max_results` param |
| v1 | Mar 2018 | — | Original API. Taken offline Dec 31, 2024 |

Key v2→v3 change: `code` now means "use with another endpoint" (e.g., team code `DUKE`), while `id` means unique record identifier. All returned parameters lowercase in v3+.

---

## Key Notes

1. Each API call deducts 1 credit. Hydration currently does not charge extra (may change). Future: may transition to credit-based transactions charging extra for stacked secondary endpoints.
2. Stats and rankings re-tabulate nightly at ~3 AM ET during season.
3. `/events` only holds the last 24 hours.
4. `/projline` only returns today's games. Use Interstat API for other days.
5. Betting data (moneyline, pointspread, overunder) from Betsson — not available for every game.
6. The `credits` field in meta is experimental/illustrative only.
7. Auto-populated nodes may change during v4 beta — check changelog.
8. Hydrating with contextually wrong secondary endpoints (e.g., `players;teamperfs`) returns a warning, not an error.
9. API key can be reset at **Analysis Tools > NatStat API > Query Builder** on any subscribed subsite.
10. NatStat has been operating since 2007; covers 30 competition levels across 5 sports.
