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

## Phase 1: Foundation & Data Ingestion ← CURRENT
> Capture 2025-2026 season data with a solid foundation

- [x] Project roadmap
- [x] Cargo workspace scaffold (cstat-core, cstat-ingest, cstat-api)
- [x] PostgreSQL schema: players, teams, games, player_game_stats, schedules, api_cache
- [x] NatStat API client with rate limiting (500 calls/hr) and response caching
- [ ] Data ingestion pipeline for 2025-2026 season
- [x] GitHub Actions CI/CD (build, test, clippy, fmt)
- [x] Unit + integration test scaffolding

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

## Phase 2: Player Metrics Engine
> Compute per-player advanced metrics from raw data

- [ ] Derived stats: offensive/defensive ratings, BPM, usage rate, true shooting, etc.
- [ ] Per-player strength of schedule based on opponents actually faced
- [ ] Rolling averages (last N games) and season aggregates
- [ ] Percentile rankings across all D-I players
- [ ] Lineup-based net ratings (if NatStat lineup data supports it)
- [ ] Pace-adjusted and opponent-adjusted metrics
- [ ] Store all computed metrics back to Postgres

---

## Phase 3: ML — Player Impact & Game Prediction
> Train player-level models, compose into game predictions

- [ ] Python training pipeline for player impact model
- [ ] Feature engineering: player stats + SOS + opponent quality → impact score
- [ ] Game outcome model: compose roster impacts + home/away/neutral → predicted score & win probability
- [ ] Spread prediction model
- [ ] Export trained models to ONNX format
- [ ] Rust inference engine via `ort` crate
- [ ] Backtest against 2025-2026 results
- [ ] Model accuracy tracking and evaluation framework

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

## Phase 4: Transfer Portal & Roster Composition Tool
> "What if" roster analysis for the offseason

- [ ] Player search and comparison across all teams
- [ ] "What if Player X transfers to Team Y?" — recompose team strength
- [ ] Roster fit scoring (complementary skills, redundancy detection)
- [ ] Portal player rankings by projected impact at destination
- [ ] API endpoints for all composition queries

---

## Phase 5: Frontend & Deployment
> React site on your domain

- [ ] Team rankings dashboard (composed from player metrics)
- [ ] Player comparison tool with percentile visualizations
- [ ] Game prediction interface (pick two teams → predicted outcome)
- [ ] Transfer portal sandbox
- [ ] Scatter plots, rolling trend charts, shot charts (if PBP supports)
- [ ] Deploy to domain with Nginx reverse proxy
- [ ] Mobile-responsive design

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
- **Phase 1**: Now — capture 2025-2026 season data
- **Phase 2-3**: Offseason — build metrics engine and train models
- **Phase 4**: Transfer portal season — evaluate portal players
- **Phase 5**: Summer — deploy frontend
- **Phase 6**: Ongoing — ready for 2026-2027 season
