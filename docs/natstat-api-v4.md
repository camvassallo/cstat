# NatStat API v4 — MBB Reference

> Extracted from natstat.com/api-v4/docs and natstat.com/api-v4/endpoints (login required).
> API Version: v4 BETA 1 (released March 2026)

## General

- **Base URL:** `https://api4.natst.at/`
- **Method:** GET (all requests)
- **Service code:** `mbb` (NCAA Men's Division I Basketball, case-insensitive)
- **Full data back to:** 2008 season

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
Metadata about service database status and known data issues. Alias: `status`.

**Range params:** `dataformat`, `season`

---

## Key Notes

1. Each API call deducts 1 credit. Hydration currently does not charge extra (may change).
2. Stats and rankings re-tabulate nightly at ~3 AM ET.
3. `/events` only holds the last 24 hours.
4. `/projline` only returns today's games.
5. Betting data (moneyline, pointspread, overunder) from Betsson — not available for every game.
6. The `credits` field in meta is experimental/illustrative only.
