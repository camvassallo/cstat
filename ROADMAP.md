# cstat — College Basketball Analytics Engine

## Vision
A player-centric college basketball analytics platform that derives team-level insights from the composition of individual players. Powered by NatStat data, a Rust engine, and ML models for game prediction, transfer portal evaluation, and roster optimization.

## Architecture Overview

```
NatStat API → [cstat-ingest] → PostgreSQL → [cstat-core] → [cstat-api] → React Frontend
                                                  ↓
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
- [x] GitHub Actions CI/CD (build, test, clippy, fmt)
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

### Future Data Sources (not yet ingested)
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
- **Lower roster qualification**: reduce from 5 to 3 prior games to recover ~200-300 training rows
- **Add `games_played` feature**: lets model know how much data it has on a team (early-season uncertainty)
- **Conference strength feature**: average adj_efficiency_margin of conference, captures tier gaps beyond SOS

### Known Model Limitations
- **No game-specific roster**: Model doesn't know who actually played — a team missing their star looks the same as full-strength.
- **Limited data**: Training on 2025+2026 seasons. More historical seasons would further improve generalization.
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

### 4c: Deployment
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

- [ ] Ingest historical seasons (NatStat data back to 2006)
- [ ] Backtest models across multiple seasons
- [ ] Tournament bracket simulator (Monte Carlo, inspired by gravity project)
- [ ] Season simulation engine
- [ ] Model accuracy dashboard with calibration tracking
- [ ] Automated daily data refresh during season
- [ ] Conference/team/player trend analysis over time

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
