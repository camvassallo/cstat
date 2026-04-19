# cstat — College Basketball Analytics Engine

## Vision
A player-centric college basketball analytics platform that derives team-level insights from the composition of individual players. Powered by NatStat data, a Rust engine, and ML models for game prediction, transfer portal evaluation, and roster optimization.

## Architecture Overview

```
NatStat API  → [cstat-ingest] → PostgreSQL → [cstat-core] → [cstat-api] → React Frontend
Barttorvik   ↗                                    ↓
                                          Python ML Training
                                                  ↓
                                            ONNX Models
                                                  ↓
                                          Rust Inference (ort)
```

### Crate Structure
- **cstat-core** — Shared types, DB models, query layer, advanced metric calculations
- **cstat-ingest** — NatStat API client, rate limiter, response caching, data pipeline
- **cstat-api** — HTTP API server (Axum), serves frontend and ML predictions

### Tech Stack
- **Backend**: Rust (Axum, SQLx, Tokio, Reqwest)
- **Database**: PostgreSQL
- **ML Training**: Python (scikit-learn, LightGBM)
- **ML Inference**: Rust via ONNX Runtime (`ort` crate)
- **Frontend**: React (Vite, AG Grid, Recharts)
- **CI/CD**: GitHub Actions
- **Deployment**: TBD domain, Nginx reverse proxy

---

## Phase 1: Foundation & Data Ingestion ✓
> Capture 2025-2026 season data with a solid foundation

- [x] Project roadmap
- [x] Cargo workspace scaffold (cstat-core, cstat-ingest, cstat-api)
- [x] PostgreSQL schema: players, teams, games, player_game_stats, schedules, api_cache
- [x] NatStat API client with rate limiting (500 calls/hr) and response caching
- [x] Data ingestion pipeline for 2025-2026 season
  - [x] Fixed NatStat v4 response parsing (endpoint-specific keys, not `results`)
  - [x] Teams: 367 teams from teamcodes + per-team TCR/ELO details
  - [x] Players: per-team roster ingestion with height, weight, hometown, nationality
  - [x] Games: 6,277 games with scores, team IDs, venue
  - [x] Player performances: box scores + advanced metrics (efficiency, usage, presence rate, perf score)
  - [x] CLI commands: `team` (single team), `explore` (raw API inspection)
  - [x] Migration 002: enriched schema (player demographics, advanced game stats, TCR fields)
- [x] Docker Compose for local Postgres 17
- [x] GitHub Actions CI (build, test, clippy, fmt)
  - [x] Revamped: concurrency groups, frontend lint/typecheck/build jobs, Postgres 17, artifact upload on main
- [x] Unit + integration test scaffolding (25 tests)

### NatStat Data Targets
- Player box scores and advanced stats
- Play-by-play event data (shot charts, possession-level)
- In-game lineup tracking (on/off splits potential)
- Team schedules and results
- Player context ratings and impact metrics

### Creative Data Ideas
- **Lineup-level net ratings**: Use play-by-play + lineup data to compute how specific player combinations perform together (offensive/defensive efficiency per lineup)
- **Pace-adjusted stats**: Normalize all counting stats to per-possession rather than per-game for fairer cross-team comparison
- **Opponent-adjusted shooting**: Weight a player's shooting splits by the defensive quality of opponents faced
- **Fatigue modeling**: Track minutes distribution and performance trends within games (play-by-play timestamps) to model fatigue effects
- **Clutch metrics**: Use play-by-play to isolate performance in close-game situations (last 5 min, score within 5)
- **Transition vs half-court splits**: If play-by-play is granular enough, separate transition and half-court offensive efficiency

---

## Phase 2: Player Metrics Engine ✓
> Compute per-player advanced metrics from raw data

- [x] **Compute layer**: derive stats from raw box score data already in DB
  - [x] `player_season_stats`: aggregate box scores → per-game avgs, shooting splits (FG%, 3P%, FT%, eFG%, TS%), usage, TOV%
  - [x] `team_season_stats`: four factors (eFG%, TOV%, ORB%, FT rate), offensive/defensive efficiency, tempo
  - [x] `schedules`: derive home/away perspectives from games table
  - [x] Backfill `def_rebounds`, `game_score` (Hollinger), `ast_to_ratio`
  - [x] `player_percentiles`: PERCENT_RANK across D-I players (≥10 GP, ≥10 MPG)
  - [x] Team game stats ingestion (`teamperfs` endpoint → `team_game_stats` table)
- [x] **Ingest full season data**: all 367 teams — rosters, games, player perfs, team perfs, team details
  - [x] 6,183 players, 110,828 box scores, 11,134 team game stats, 364 team details
  - [x] Fixed FK constraint for non-D1 exhibition opponents (skip instead of nil UUID)
  - [x] Fixed player_season_stats unique constraint for mid-season transfers (include team_id)
- [x] **Opponent-adjusted efficiency** (KenPom-style): iterative regression adjusts off/def efficiency by opponent quality until convergence, plus SOS and SOS rank
- [x] **Player strength of schedule**: minutes-weighted avg opponent adj efficiency margin, plus SOS percentile
- [x] **Rolling averages**: last 5 games PPG, RPG, APG, FG%, TS%, game score on every player_game_stats row (102K rows)
- [x] **Player rate stats**: AST% (from team FGM context), ORB%, DRB%, STL%, BLK% (per-40 proxies)
- [x] **Individual ORTG/DRTG**: box-score approximation using team adjusted efficiency as base, plus net rating
- [x] **BPM splits**: OBPM/DBPM derived from offensive/defensive game_score components
- [x] **Pipeline gap fill**: captured `team_fga`/`team_fta`/`team_turnovers` from NatStat playerperfs; `overtime`, `attendance`, `half scores`, `venue_code` from games; `is_conference` derived from team conferences; `is_postseason` from dates; `point_diff` from team_game_stats
- [x] Store all computed metrics back to Postgres (10-step compute pipeline)

### Known Limitations
- **Player position/class_year**: NatStat does not provide these fields in any endpoint
- **Plus/minus**: Not available from NatStat box scores
- **True lineup-based ORTG/DRTG**: Would require play-by-play data; current implementation is a box-score approximation

### Additional Data Sources
- **Barttorvik** (integrated): Player season stats (CSV), per-game box scores (gzip JSON). No auth required. Used for GBPM, shot zones, recruiting rank, bio data, and rebound backfill.
- **NatStat play-by-play**: Would unlock lineup-based net ratings, clutch metrics, transition vs half-court splits, shot charts, and better defensive metrics. Expensive to consume and keep updated — worth exploring once core model is solid.
- **247Sports recruiting rankings**: EvanMiya uses these as Bayesian priors for freshman/early-season projections. Separate data source, lower priority.

---

## Phase 3: ML — Player Impact & Game Prediction ✓
> Train player-level models, compose into game predictions

- [x] Python training pipeline (LightGBM, scikit-learn, ONNX export)
- [x] Feature engineering: 47 point-in-time diff features from team efficiency, roster aggregates, rolling form, power metrics
  - Team-level: adj offense/defense/margin, four factors, ELO, point diff, pythagorean win%, road win%, SOS
  - Roster-level: minutes-weighted PPG, RPG, APG, BPM, OBPM/DBPM, ORTG, rate stats (AST%, TOV%, STL%, BLK%)
  - Form: rolling game score, rolling TS%, PPG trend, game score trend
  - Context: venue, conference matchup, win percentage diff
- [x] Game outcome model: margin regression + win probability classification
- [x] Backtest against 2025-2026 results (chronological 80/20 split)
  - Pre-PIT (leaked): margin MAE 8.48 pts, win accuracy 70.5%, AUC 0.772
  - Post-PIT (honest): margin MAE 9.18 pts, win accuracy 68.3%, AUC 0.709
- [x] 5-fold cross-validation
  - Pre-PIT (leaked): margin MAE 8.71, win accuracy 74.1%, AUC 0.808
  - Post-PIT (honest): margin MAE 9.46, win accuracy 69.2%, AUC 0.736
- [x] Export trained models to ONNX format (31 → 49 features)
- [x] Tuned hyperparameters: lower learning rate, L1/L2 regularization, fewer leaves
- [x] **Point-in-time features**: eliminated data leakage — all features now computed using only prior-game data
  - KenPom-style adjusted efficiency recomputed per game-date snapshot (iterative regression on all prior games)
  - Incremental ELO with margin-of-victory multiplier (FiveThirtyEight style), updated game-by-game
  - Expanding-window cumulative averages for team four factors, roster aggregates, and player advanced stats
  - Point-in-time SOS derived from adjusted efficiency snapshots
  - Rolling form from per-game rolling columns (shifted to exclude current game)
  - Early-season games with insufficient data naturally excluded via NaN filtering
- [x] Retrained models with point-in-time features (honest backtest, no leakage)
  - 4,331 games with complete features (865 early-season games dropped due to insufficient prior data)
  - Backtest (chronological 80/20): margin MAE 9.18 pts, win accuracy 68.3%, AUC 0.709
  - 5-fold CV: margin MAE 9.46, win accuracy 69.2%, AUC 0.736
  - Top features: adj_efficiency_margin (dominant), ELO, minutes_stddev (depth), def_rebound_pct, adj_defense
  - Model early-stops at 49-66 iterations — data-starved with single season
- [x] Rust inference engine via `ort` crate
- [x] Model accuracy tracking and evaluation framework

### Model Improvement Ideas
- ~**Ingest historical seasons**: even 1-2 more seasons roughly doubles training data and reduces early stopping; highest-impact improvement available~ *(done — training pipeline now supports multi-season; 2025+2026 ingested)*
- ~**Use NatStat ELO as feature**: Replace computed incremental ELO with NatStat's pre-game ELO from `/forecasts` endpoint. Uses only `elo_before` (pre-game) to avoid leakage.~ *(done — `features.py` now uses NatStat pre-game ELO from `game_forecasts`, falling back to computed ELO for games without forecast data)*
- ~**Benchmark against NatStat win probability**: `/forecasts` provides ELO-based `winexp` per game. Compare our model's predictions against theirs to identify where we add value.~ *(done — cstat wins every metric: +2.1pp accuracy, +0.014 AUC, 3x better calibration)*
- **Expand historical training data**: `/seasons` confirms perfs available 2007-2026 (20 seasons), play-by-play from 2012+. Even 5-6 seasons would dramatically reduce early-stopping. ~57 `/forecasts` API calls per season for per-game ELO.
- **Lower roster qualification**: reduce from 5 to 3 prior games to recover ~200-300 training rows
- **Add `games_played` feature**: lets model know how much data it has on a team (early-season uncertainty)
- **Conference strength feature**: average adj_efficiency_margin of conference, captures tier gaps beyond SOS
- **Use recruiting rank as early-season prior**: Team-avg recruiting rank (22% of players have ranks from Torvik) could serve as a Bayesian prior for the first ~3 weeks when the model drops games due to insufficient game data. Would require imputation strategy for unranked players.

### Data Leakage Precautions for NatStat ELO
NatStat's `/forecasts` provides both `elo_before` (pre-game) and `elo_after` (post-game) for each team. Only `elo_before` may be used as an ML feature — it represents the rating at prediction time. `elo_after` and current `/elo` rankings reflect end-of-season state and must NOT be used as game-level features. The `win_exp` (NatStat's predicted win probability) must also be excluded from training features — it's a competing prediction, not an input. It should only be used as a benchmark comparison.

### Known Model Limitations
- **No game-specific roster**: Model doesn't know who actually played — a team missing their star looks the same as full-strength.
- **Limited data**: Training on 2025+2026 seasons (9,147 games). More historical seasons would further improve generalization. NatStat has data back to 2007.
- **No lineup data**: Can't model specific 5-man combinations on court.

### Player-Centric Composition Approach
Each player gets:
- Individual offensive/defensive impact scores
- Strength of schedule adjustment based on their actual games played
- Usage-weighted contribution metrics
- Complementary skill indicators (spacing, rim protection, playmaking, etc.)

Team prediction = f(roster_composition, minutes_distribution, home/away/neutral, opponent_roster)

This naturally enables:
- Transfer portal "what-if" analysis (swap players between rosters)
- Injury impact estimation
- Optimal lineup recommendations

---

## Phase 4: Frontend — Rankings, Stats & Predictions ← CURRENT
> React web UI on top of the existing data/compute/ML layers (KenPom/Barttorvik-style)

### 4a: API Endpoints (cstat-api)
- [x] `GET /api/teams/rankings` — team rankings sortable by adj efficiency, ELO, SOS, etc.
- [x] `GET /api/teams/:id` — team profile: season stats, four factors, schedule/results
- [x] `GET /api/players?search=&team=&season=` — player search/filter
- [x] `GET /api/players/:id` — player profile: season stats, percentiles, rolling form
- [ ] `GET /api/players/compare?ids=` — side-by-side player comparison
- [x] `GET /api/games?date=&team=` — game results

### 4b: React Frontend (Vite + AG Grid + Recharts)
- [x] Project scaffold (Vite, React, TypeScript, Tailwind CSS)
- [x] Team rankings table (sortable/filterable, AG Grid)
- [x] Team detail page (four factors, schedule, roster)
- [x] Player stats table (sortable, with search)
- [x] Player detail page (season stats, rolling form charts, percentile spider/radar)
- [ ] Player comparison view (side-by-side stats + visualizations)
- [x] Game prediction interface (pick two teams → predicted margin + win prob)
- [ ] Score ticker / recent results
- [ ] Mobile-responsive design

### 4c: Data Quality & Ingestion Hardening
- [x] Fix USG% ingestion (divide NatStat `usgpct` by 100)
- [x] Fix rebound mapping (`reb` = defensive rebounds, not total)
- [x] Fix ORB%/DRB% computation (game-level self-join with NULL guards)
- [x] Force-overwrite rebounds/usage on re-ingestion (no COALESCE)
- [x] ~Label ELO as "ELO Rk" (rank, not rating)~ → replaced with real ELO rating from `/elo` endpoint
- [x] Make team names clickable on Rankings page
- [x] Player deduplication merge pass (989 duplicate pairs)
- [x] Ingest real ELO ratings from `/elo` endpoint (4 API calls/season)
- [x] Fix ELO rank: NatStat `elorank` resets per-page — now recomputed globally via `DENSE_RANK()`
- [x] Ingest per-game forecasts from `/forecasts` endpoint (pre/post ELO, win exp, spread, moneyline — 57 calls/season)
- [x] Fix cache poisoning: error responses (string + object shapes, `success: "0"`) no longer cached; 740 poisoned entries purged
- [x] Fix pagination runaway: abandoned unreliable `pages-total`/`page-next` metadata, uses payload-empty detection + `MAX_PAGES=2000` safety cap
- [x] Fix v3 string-encoded meta: `value_as_u64` helper handles both `"1214"` and `1214` for all numeric meta fields
- [x] Fix body decode crashes: chunked-encoding EOF / malformed JSON now retried instead of aborting pagination
- [x] Auto-create player records from box scores: `upsert_player_game_stats` inserts minimal player row on first perf encounter — removes dependency on broken `/players` roster endpoint
- [x] Remove dead `players` step from `SeasonIngester` (was ~365 wasted API calls per season)
- [x] Scrub fake-rebound-zeros: game-level NULL propagation when any player in a game has contradictory `reb=0 + oreb>0`
- [x] Update ML to use NatStat pre-game ELO (elo_before only — no leakage)
- [x] 2026 season re-ingestion + recompute after all fixes
- [x] 2025 season full re-ingestion (113k player perfs, 100% rebound coverage)
- [x] Retrain ML models on 2026 (MAE 8.98, win acc 67.7%, AUC 0.725)
- [x] Retrain on 2025+2026 combined (9,147 games; backtest MAE 8.86, win acc 68.6%, AUC 0.735; model trains 2x deeper)
  - [x] Added Torvik GBPM features (w_gbpm, star_gbpm — 47→49 features; backtest MAE 8.68, win acc 70.0%, AUC 0.764; GBPM is #1 feature by importance)
- [x] Benchmark model against NatStat win probability (cstat wins every metric: +2.1pp accuracy, +0.014 AUC, 3x better calibration; wins 59.8% of disagreements)
- [x] Fix player rate stats to use possession-based formulas (ORB%, DRB%, STL%, BLK% now use Basketball Reference formulas with team/opponent game stats)
- [x] Barttorvik integration as secondary data source (player-centric focus)
  - [x] Migration 008: `torvik_player_stats` table (GBPM, shot zones, bio, recruiting rank, 64 columns)
  - [x] `TorkvikClient` — fetches CSV player season stats and gzip JSON per-game box scores
  - [x] CSV parser (headerless, 64 positional columns) and gzip JSON parser (array-of-arrays, 53 columns)
  - [x] Player matching: fuzzy team name match + name-only fallback (93.7% match rate, 4,664/4,979)
  - [x] Backfill class_year and height_inches on player records from Torvik bio data
  - [x] Rebound backfill from Torvik game-level data (76,385 game rows updated — NatStat had 32% coverage)
  - [x] CLI subcommand: `torvik --year 2026 [--rebounds]`
  - [x] Surface Torvik advanced metrics (GBPM, shot zones, recruiting rank) in player detail API
  - [x] Polish Torvik data display on player detail page (shot zone visualization, GBPM context/percentiles)
  - [x] Use Torvik data as ML features (GBPM as roster aggregate and star-player feature)
  - [ ] Use recruiting rank as early-season prior (team-avg recruit rank for first ~3 weeks when model lacks game data)

### 4d: Deployment
- [ ] Deploy to domain with Nginx reverse proxy
- [x] Serve React build from cstat-api (static file fallback)

---

## Phase 5: Transfer Portal & Roster Composition Tool
> "What if" roster analysis for the offseason

- [ ] Player search and comparison across all teams
- [ ] "What if Player X transfers to Team Y?" — recompose team strength
- [ ] Roster fit scoring (complementary skills, redundancy detection)
- [ ] Portal player rankings by projected impact at destination
- [ ] API endpoints for all composition queries
- [ ] Transfer portal sandbox UI

---

## Phase 6: Expansion & Refinement
> Historical depth, brackets, continuous improvement

- [ ] Ingest historical seasons (NatStat perfs back to 2007, PBP from 2012+, per `/seasons` endpoint)
- [ ] Backtest models across multiple seasons
- [ ] Tournament bracket simulator (Monte Carlo, inspired by gravity project)
- [ ] Season simulation engine
- [ ] Model accuracy dashboard with calibration tracking
- [ ] Automated daily data refresh during season
- [ ] Conference/team/player trend analysis over time

---

## Known Bugs / Data Quality Issues

### Duplicate Player Records (P1 — Fixed)
NatStat's `/playercodes` endpoint returns different codes for the same physical player across seasons (e.g., `57987927` and `87832246` both map to Caleb Foster on Duke). This creates two `players` rows per affected player — one with most games, one with 1-2 games. **~989 duplicate pairs** exist in the 2026 season data. 241 have overlapping games with identical stats (concentrated on opening night Nov 3).

**Impact**: Player season stats are split across two records, deflating per-game averages for the primary record and showing misleading 1-game entries on rosters.

**Fix**: Implemented `deduplicate_players()` as step 1/12 in the compute pipeline. For each `(name, team_id, season)` duplicate group: picks the primary (highest game count), deletes overlapping identical game stats, reassigns non-overlapping game stats to primary, removes duplicate player + season stats + percentiles records.

### NatStat `reb` Field is Total Rebounds, Not Defensive (P1 — Fixed)
NatStat's `reb` field in both `playerperfs` and `teamperfs` represents **total rebounds**, not defensive rebounds. This was initially misidentified as defensive rebounds, causing inflated totals (e.g., Tobe Awaka showed 26 total vs actual 18). Additionally, ~69% of records return `reb=0` even when `oreb > 0`, which is missing data.

**Verification**: Cross-referenced Tobe Awaka vs Utah Tech (NatStat `reb=18, oreb=8` → 18 total, 10 defensive, matching ESPN). Confirmed team-level `reb` sums match player-level `reb` sums, and both are total (not defensive).

**Verified via live API curl**: NatStat genuinely doesn't have total/defensive rebounds for ~68% of games — it's missing at the source, not an ingestion bug. The missing data is all-or-nothing per game (no mixed games). When `reb` is populated, `playerperfs` also includes a `dreb` field; `teamperfs` never has `dreb`.

**Fix**: Ingestion now correctly maps `reb` → `total_rebounds`, uses `dreb` directly when present (playerperfs only), otherwise derives `def_rebounds = total - oreb`. Guards `reb=0 + oreb>0` as NULL. Force-overwrites on upsert. The compute pipeline estimates missing team DREB from box score (`DREB ≈ opponent_missed_FGA - opponent_OREB`, r=0.840) for the ~68% of games where `reb=0`.

### ELO Shows Rank, Not Rating (P2 — Fixed)
NatStat's `/teams` endpoint only provides `elo.rank` (ordinal 1-364), not the actual ELO rating. **Fixed**: Real ELO ratings now ingested from dedicated `/elo` endpoint (364 teams for 2025, 365 for 2026). Ranks recomputed globally via `DENSE_RANK()` to avoid NatStat's per-page rank collision bug.

### Player Rate Stats Are Per-40-Min Proxies (P2)
`compute_player_rates` computes AST%, ORB%, DRB%, STL%, BLK% as per-40-minute rates, not true possession-based percentages. Reasonable proxies but differ from standard definitions (e.g., Basketball Reference).

**Fix**: Implement proper possession-based formulas using team pace and possession estimates.

### USG% Was Ingested as Whole Numbers (Fixed)
NatStat returns `usgpct` as whole numbers (e.g., 19.5 for 19.5%). Frontend `pct()` multiplied by 100 again → 1950%. **Fixed**: divide by 100 at ingestion time.

### COALESCE on Upsert Preserving Stale Data (Partially Fixed)
Upsert `ON CONFLICT` used `COALESCE(EXCLUDED.x, old.x)`, so NULL new values wouldn't overwrite old corrupt data. **Fixed** for rebounds and usage_rate. Other columns still use COALESCE — acceptable for fields where NULL means "not provided this time" but could mask issues elsewhere.

---

## Data Caching Strategy
Given the 500 API calls/hour NatStat limit:
1. **Response cache table** in Postgres: store raw API responses with TTL
2. **Incremental ingestion**: only fetch games/stats since last sync
3. **Bulk operations**: batch multiple data needs into fewer API calls where possible
4. **Off-peak scheduling**: run large ingestion jobs during low-usage periods
5. **Local development**: seed a dev database from cached data to avoid API calls during development

---

## Timeline
- **Phase 1**: ✓ Capture 2025-2026 season data
- **Phase 2-3**: ✓ Metrics engine, ML training, inference
- **Phase 4**: Now — frontend + API to surface data (KenPom/Barttorvik-style)
- **Phase 5**: Transfer portal season — roster composition tool
- **Phase 6**: Ongoing — historical depth, brackets, ready for 2026-2027
