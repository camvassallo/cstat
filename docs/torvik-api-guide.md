# Barttorvik API Guide

Unofficial documentation for the barttorvik.com data endpoints used for college basketball analytics. These are public, unauthenticated HTTP GET endpoints that return full-season data dumps.

> **Note:** There is no official API documentation from barttorvik.com. This guide is based on observed behavior and may change without notice. Replace `{year}` with the desired NCAA season year (e.g., `2026`).

---

## Endpoints

| Endpoint | URL | Format | Compression | Description |
|---|---|---|---|---|
| Player Stats | `https://barttorvik.com/getadvstats.php?year={year}&csv=1` | CSV | None | Season averages for all D1 players |
| Team Stats | `https://barttorvik.com/{year}_team_results.json` | JSON | None | Team-level rankings and efficiency metrics |
| Game Stats | `https://barttorvik.com/{year}_all_advgames.json.gz` | JSON | Gzip | Per-game, per-player box scores for all D1 games |

### Authentication

None required. All endpoints are publicly accessible via HTTP GET.

### Rate Limiting

No known rate limits, but be respectful -- these are not officially published APIs.

### Date Filtering

None available at the API level. All endpoints return the complete dataset for the requested season year. Any date-range filtering must be done client-side after ingestion.

---

## 1. Player Stats

**URL:** `GET https://barttorvik.com/getadvstats.php?year={year}&csv=1`

**Format:** CSV (headerless -- columns are positional)

**Response:** One row per player, containing season-level aggregated statistics. Typically ~5,000 rows for a full D1 season.

### Parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| `year` | integer | Yes | NCAA season year (e.g., `2026`) |
| `csv` | integer | Yes | Must be `1` to get CSV format |

### Column Schema (ordered by position)

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | `player_name` | string | Player's full name |
| 1 | `team` | string | Team name |
| 2 | `conf` | string | Conference abbreviation |
| 3 | `gp` | int | Games played |
| 4 | `min_per` | float | Minutes per game |
| 5 | `o_rtg` | float | Offensive rating |
| 6 | `usg` | float | Usage rate (%) |
| 7 | `e_fg` | float | Effective field goal percentage |
| 8 | `ts_per` | float | True shooting percentage |
| 9 | `orb_per` | float | Offensive rebound percentage |
| 10 | `drb_per` | float | Defensive rebound percentage |
| 11 | `ast_per` | float | Assist percentage |
| 12 | `to_per` | float | Turnover percentage |
| 13 | `ftm` | int | Free throws made |
| 14 | `fta` | int | Free throws attempted |
| 15 | `ft_per` | float | Free throw percentage |
| 16 | `two_pm` | int | Two-point field goals made |
| 17 | `two_pa` | int | Two-point field goals attempted |
| 18 | `two_p_per` | float | Two-point field goal percentage |
| 19 | `tpm` | int | Three-point field goals made |
| 20 | `tpa` | int | Three-point field goals attempted |
| 21 | `tp_per` | float | Three-point field goal percentage |
| 22 | `blk_per` | float | Block percentage |
| 23 | `stl_per` | float | Steal percentage |
| 24 | `ftr` | float | Free throw rate |
| 25 | `yr` | string | Class year (e.g., "Fr", "So", "Jr", "Sr") |
| 26 | `ht` | string | Height (e.g., "6-5") |
| 27 | `num` | string | Jersey number |
| 28 | `porpag` | float | Points over replacement per adjusted game (offensive) |
| 29 | `adjoe` | float | Adjusted offensive efficiency |
| 30 | `pfr` | float | Personal foul rate |
| 31 | `year` | int | Season year |
| 32 | `pid` | int | Unique player ID |
| 33 | `player_type` | string | Player archetype/type classification |
| 34 | `rec_rank` | float | Recruiting rank |
| 35 | `ast_tov` | float | Assist-to-turnover ratio |
| 36 | `rim_made` | float | Rim shots made |
| 37 | `rim_attempted` | float | Rim shots attempted |
| 38 | `mid_made` | float | Mid-range shots made |
| 39 | `mid_attempted` | float | Mid-range shots attempted |
| 40 | `rim_pct` | float | Rim shot percentage |
| 41 | `mid_pct` | float | Mid-range shot percentage |
| 42 | `dunks_made` | float | Dunks made |
| 43 | `dunks_attempted` | float | Dunks attempted |
| 44 | `dunk_pct` | float | Dunk percentage |
| 45 | `pick` | float | NBA draft projection pick number |
| 46 | `drtg` | float | Defensive rating (lower is better) |
| 47 | `adrtg` | float | Adjusted defensive rating |
| 48 | `dporpag` | float | Defensive points over replacement per adjusted game |
| 49 | `stops` | float | Defensive stops |
| 50 | `bpm` | float | Box plus/minus |
| 51 | `obpm` | float | Offensive box plus/minus |
| 52 | `dbpm` | float | Defensive box plus/minus |
| 53 | `gbpm` | float | Game box plus/minus |
| 54 | `mp` | float | Total minutes played |
| 55 | `ogbpm` | float | Offensive game box plus/minus |
| 56 | `dgbpm` | float | Defensive game box plus/minus |
| 57 | `oreb` | float | Offensive rebounds per game |
| 58 | `dreb` | float | Defensive rebounds per game |
| 59 | `treb` | float | Total rebounds per game |
| 60 | `ast` | float | Assists per game |
| 61 | `stl` | float | Steals per game |
| 62 | `blk` | float | Blocks per game |
| 63 | `pts` | float | Points per game |

### Example Request

```
curl "https://barttorvik.com/getadvstats.php?year=2026&csv=1"
```

### Example Response (truncated)

```csv
Antwann Jones,UNC Greensboro,SoCon,25,12.3,98.2,14.1,...
Elijah Elliott,New Mexico St.,CUSA,24,28.5,105.1,21.3,...
```

---

## 2. Team Stats

**URL:** `GET https://barttorvik.com/{year}_team_results.json`

**Format:** JSON array of objects

**Response:** One object per team. Typically ~365 teams for a full D1 season.

### Parameters

None (year is embedded in the URL path).

### Field Schema

| Field | Type | Description |
|---|---|---|
| `rank` | int | Overall Torvik ranking |
| `team` | string | Team name |
| `conf` | string | Conference abbreviation |
| `record` | string | Overall win-loss record (e.g., "20-5") |
| `adjoe` | float | Adjusted offensive efficiency |
| `adjoe_rank` | int | AdjOE national rank |
| `adjde` | float | Adjusted defensive efficiency |
| `adjde_rank` | int | AdjDE national rank |
| `barthag` | float | Barthag power rating (win probability vs average team) |
| `barthag_rank` | int | Barthag national rank |
| `proj_wins` | float | Projected total wins |
| `proj_losses` | float | Projected total losses |
| `proj_conf_wins` | float | Projected conference wins |
| `proj_conf_losses` | float | Projected conference losses |
| `conf_record` | string | Conference win-loss record |
| `sos` | float | Strength of schedule |
| `nconf_sos` | float | Non-conference strength of schedule |
| `conf_sos` | float | Conference strength of schedule |
| `proj_sos` | float | Projected strength of schedule |
| `proj_nconf_sos` | float | Projected non-conference SOS |
| `proj_conf_sos` | float | Projected conference SOS |
| `elite_sos` | float | Elite strength of schedule |
| `elite_ncsos` | float | Elite non-conference SOS |
| `opp_adjoe` | float | Opponents' adjusted offensive efficiency |
| `opp_adjde` | float | Opponents' adjusted defensive efficiency |
| `opp_proj_adjoe` | float | Opponents' projected AdjOE |
| `opp_proj_adjde` | float | Opponents' projected AdjDE |
| `conf_adjoe` | float | Conference opponents' AdjOE |
| `conf_adjde` | float | Conference opponents' AdjDE |
| `qual_adjoe` | float | Quality wins AdjOE |
| `qual_adjde` | float | Quality wins AdjDE |
| `qual_barthag` | float | Quality wins Barthag |
| `qual_games` | float | Number of quality games |
| `fun` | float | Fun rating |
| `fun_rank` | int | Fun rating national rank |
| `conf_pf` | float | Conference points for (per game) |
| `conf_pa` | float | Conference points against (per game) |
| `conf_poss` | float | Conference possessions per game |
| `conf_adj_o` | float | Conference adjusted offensive efficiency |
| `conf_adj_d` | float | Conference adjusted defensive efficiency |
| `conf_sos_remain` | float | Remaining conference SOS |
| `conf_win_perc` | float | Conference win percentage |
| `wab` | float | Wins above bubble |
| `wab_rank` | int | WAB national rank |
| `adj_tempo` | float | Adjusted tempo (possessions per 40 min) |

### Example Request

```
curl "https://barttorvik.com/2026_team_results.json"
```

### Example Response (truncated)

```json
[
  {
    "rank": 1,
    "team": "Duke",
    "conf": "ACC",
    "record": "24-2",
    "adjoe": 125.3,
    "adjoe_rank": 2,
    "adjde": 88.1,
    "adjde_rank": 1,
    "barthag": 0.9812,
    ...
  },
  ...
]
```

---

## 3. Game Stats

**URL:** `GET https://barttorvik.com/{year}_all_advgames.json.gz`

**Format:** Gzip-compressed JSON. The decompressed payload is an array of arrays (each inner array is a positional row).

**Response:** One row per player per game. Typically ~80,000+ rows for a full D1 season.

### Parameters

None (year is embedded in the URL path).

### Decompression

The response body is gzip-compressed. Decompress before parsing as JSON.

```
curl -s "https://barttorvik.com/2026_all_advgames.json.gz" | gunzip | head -c 500
```

### Column Schema (ordered by array index)

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | `numdate` | string | Numeric date identifier (sortable) |
| 1 | `datetext` | string | Human-readable date (e.g., "11-4") |
| 2 | `opstyle` | int? | Opponent style rating |
| 3 | `quality` | int? | Game quality rating |
| 4 | `win1` | int? | Win indicator (1 = win, 0 = loss) |
| 5 | `opponent` | string | Opponent team name |
| 6 | `muid` | string | Unique game/matchup ID |
| 7 | `win2` | int? | Secondary win indicator |
| 8 | `min_per` | float? | Minutes percentage |
| 9 | `o_rtg` | float? | Offensive rating |
| 10 | `usage` | float? | Usage rate |
| 11 | `e_fg` | float? | Effective field goal percentage |
| 12 | `ts_per` | float? | True shooting percentage |
| 13 | `orb_per` | float? | Offensive rebound percentage |
| 14 | `drb_per` | float? | Defensive rebound percentage |
| 15 | `ast_per` | float? | Assist percentage |
| 16 | `to_per` | float? | Turnover percentage |
| 17 | `dunks_made` | int? | Dunks made |
| 18 | `dunks_att` | int? | Dunks attempted |
| 19 | `rim_made` | int? | Rim shots made |
| 20 | `rim_att` | int? | Rim shots attempted |
| 21 | `mid_made` | int? | Mid-range shots made |
| 22 | `mid_att` | int? | Mid-range shots attempted |
| 23 | `two_pm` | int? | Two-point field goals made |
| 24 | `two_pa` | int? | Two-point field goals attempted |
| 25 | `tpm` | int? | Three-point field goals made |
| 26 | `tpa` | int? | Three-point field goals attempted |
| 27 | `ftm` | int? | Free throws made |
| 28 | `fta` | int? | Free throws attempted |
| 29 | `bpm_rd` | float? | BPM round |
| 30 | `obpm` | float? | Offensive box plus/minus |
| 31 | `dbpm` | float? | Defensive box plus/minus |
| 32 | `bpm_net` | float? | Net box plus/minus |
| 33 | `pts` | float? | Points scored |
| 34 | `orb` | float? | Offensive rebounds |
| 35 | `drb` | float? | Defensive rebounds |
| 36 | `ast` | float? | Assists |
| 37 | `tov` | float? | Turnovers |
| 38 | `stl` | float? | Steals |
| 39 | `blk` | float? | Blocks |
| 40 | `stl_per` | float? | Steal percentage |
| 41 | `blk_per` | float? | Block percentage |
| 42 | `pf` | float? | Personal fouls |
| 43 | `possessions` | float? | Possessions |
| 44 | `bpm` | float? | Box plus/minus |
| 45 | `sbpm` | float? | Simple box plus/minus |
| 46 | `loc` | string | Game location (H/A/N) |
| 47 | `tt` | string | Player's team name |
| 48 | `pp` | string | Player's full name |
| 49 | `inches` | int? | Player height in inches |
| 50 | `cls` | string | Class year (Fr/So/Jr/Sr) |
| 51 | `pid` | int? | Unique player ID |
| 52 | `year` | int? | Season year |

> **Note:** Fields marked with `?` may be null, empty strings, or absent. Handle gracefully.

### Example Response Structure (decompressed, truncated)

```json
[
  ["20251104", "11-4", 45, 3, 1, "Kansas St.", "abc123", 1, 55.2, 98.1, 14.3, ...],
  ["20251104", "11-4", 45, 3, 1, "Kansas St.", "abc123", 0, 32.1, 105.3, 21.0, ...],
  ...
]
```

---

## Inverse Stats

Some statistics are "better" when lower. When applying color coding or percentile rankings, invert the scale for:

| Stat | Field(s) | Reason |
|---|---|---|
| Defensive Rating | `drtg`, `adrtg` | Lower = better defense |
| Turnover Percentage | `to_per` | Lower = fewer turnovers |
| Personal Fouls | `pf`, `pfr` | Lower = fewer fouls |

---

## Common Patterns

### Linking Players Across Endpoints

Use `pid` (player ID) to join player stats, game stats, and any derived data. The `pid` field is consistent across all endpoints for the same season.

### Linking Games

Use `muid` (matchup unique ID) to group all player rows belonging to the same game.

### Season Year Convention

The `year` parameter refers to the spring calendar year of the NCAA season. For example, the 2025-26 season uses `year=2026`.
