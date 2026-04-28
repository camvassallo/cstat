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
- [x] **Player rate stats**: AST% (from team FGM context), ORB%, DRB%, STL%, BLK% (Basketball Reference possession-based formulas), FT Rate
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
- [x] `GET /api/players/compare?ids=` — side-by-side player comparison (up to 4 players, parallel queries per player)
- [x] `GET /api/games?date=&team=` — game results

### 4b: React Frontend (Vite + AG Grid + Recharts)
- [x] Project scaffold (Vite, React, TypeScript, Tailwind CSS)
- [x] Team rankings table (sortable/filterable, AG Grid)
- [x] Team detail page (four factors, schedule, roster)
- [x] Player stats table (sortable, with search)
- [x] Player detail page (season stats, rolling form charts, percentile spider/radar)
- [x] Player comparison view (side-by-side stats + visualizations) — picker, color-coded chips, per-stat percentile bars, overlaid radar + rolling game-score lines
- [x] **Player comparison advantage indicators**: percentile-aware advantage chips on the comparison page. Each row gets a tiered chip on the leading value — `EDGE` (small percentile gap), `ADVANTAGE` (medium), `DOMINANT` (large) — so a 2-PPG gap between the 95th and 80th percentile reads differently than a 2-PPG gap mid-pack. Direction-aware (lower-is-better for TOV%, fouls, etc.). Show raw delta alongside the chip; toggle to hide chips entirely.
  - [ ] *Stretch (lands with Phase 5a):* **Duel mode** — frame the comparison as a D&D-style combat where each stat row is a "round," winner takes the round, and the header shows the round count (e.g., "*Wizard 11, Ranger 7*"). Reuses the archetype names from 5a and gives the page a shareable summary line.
- [x] Game prediction interface (pick two teams → predicted margin + win prob)
- [ ] **Game prediction explainability**: per-prediction attribution panel showing top contributing features (e.g., "Duke +5 from GBPM gap, +3 from defense, −2 road game"). Export SHAP values from training, expose via API, render as a horizontal bar breakdown beneath the margin/win prob.
- [x] **Tables UI polish across the site**: extended the home-page rankings table treatment to other tables (Players list, TeamDetail roster, PlayerDetail game logs). Shipped sticky headers, shared `SortHeader` component, Raw/Rate toggle, and percentile-tinted values via `pctileTextColor`. Reference patterns from the home page:
  - Clickable team/player names rendered as blue links (currently inconsistent)
  - Subtle percentile/rank context alongside key stats — small chip, tint, or inline rank — without overwhelming the headline number
  - Targeted color emphasis on important stats (sparing, not a full heatmap)
  - Consistent sorting + filtering UX across tables (column sort affordances, filter inputs, empty/no-results states)
  - Consistent typography, density, and sticky headers across surfaces
  - Note: per-page default-sort tweaks (e.g., players page → `cam_gbpm_v3`) live in **4f Ship**, not here
- [x] **Sortable-table follow-ups** (small polish items uncovered during the table polish work):
  - [x] **Keyboard a11y on `SortHeader`**: added `role="button"`, `tabIndex={0}`, `aria-sort`, and `Enter`/`Space` handlers so keyboard-only users can trigger column sort. Hand-rolled tables (Roster, Schedule, GameLog) inherit it via the shared `SortHeader`; AG Grid surfaces handle this themselves.
  - [x] **`pctileTextColor` input clamp**: defensive `Math.max(0, Math.min(1, p))` at the function entry in `web/src/components/pctile.ts` (extracted to a shared module so Players/TeamDetail/Rankings all use the same gradient).
- [x] **Landing-page (Rankings) polish**: trimmed the column set to a KenPom-style standard view (Rk · Team · Conf · Record · AdjEM · AdjO · AdjD · Tempo · SOS · ELO) and added a **Standard / Offense / Defense** segmented toggle so the four-factor breakdowns are opt-in. AdjEM renders as a CamPom-style tier chip (Elite / Strong / Above average / Average / Below average / Weak). The supporting ranks (`#42` subscripts on AdjO/AdjD/Tempo/SOS/ELO/4F) are tinted by per-stat percentile via the muted `pctileTextColor`. Search wired into AG Grid `quickFilterText` so one input filters every column. Columns use AG Grid `flex` so the table fills the container width on first paint without imperative `sizeColumnsToFit` races. Defense view added the missing `OppTOV%` / `OppFTR` ranks (backend was returning 6 of 8 four-factor ranks; now 8 of 8). Shared `TableToolbar` + `TableSearchInput` components keep the page chrome consistent with the Players tab.
- [ ] **Tables code-quality follow-ups** (deferred from the landing-page polish review — none load-bearing, all small):
  - **Extract shared number formatters**: `TeamDetail.tsx` (inside `RosterTable`) and `Players.tsx` both define their own `fracPct` (×100 for fractions like AST%/TOV%) and `pointPct` (no scaling for ORB%/DRB%/STL%/BLK%) helpers — the same code in two places. Pull into `web/src/components/format.ts` (or extend `pctile.ts`) so the mixed-scale convention has one source of truth and a future schema rename only touches one file.
  - **Rankings team-name as `<Link>`**: the cell currently renders a plain `<span className="text-blue-400 hover:underline">` and relies on AG Grid's row click to navigate. This works (AG Grid is keyboard-navigable via arrow keys + Enter), but a real `<Link>` would be better for screen readers, browser middle-click "open in new tab", and right-click context menus. Players.tsx already does this — match the pattern.
  - **`gradientCellStyle` closure allocation**: the helper in `Players.tsx` returns a fresh closure on every `buildColumns` call. AG Grid handles it fine at our row counts, but if the page ever re-renders frequently (e.g. when we add filter chips, archetype-aware coloring, or a season selector), memoising the column defs or hoisting the cellStyle factories would avoid stale-closure pitfalls. Defer until there's a measurable issue.
- [ ] **Spider/radar chart axis transparency**: surface what each prong of the player detail and compare-page radars actually represents — which underlying stat(s) + percentile feed each axis. Today the labels are opaque; a viewer can't tell whether "playmaking" is AST%, AST/TO, raw APG, or a blend. Work:
  - Audit the current axis-to-stat mapping; confirm each prong reflects its label and that no axis double-counts a stat
  - Hover/tap tooltip on each prong showing the contributing stat(s), the player's raw value, and the percentile feeding the spoke length
  - Below-chart "How this is computed" panel listing the full mapping
  - Apply consistently to PlayerDetail (single radar) and Compare (overlaid)
- [ ] Score ticker / recent results
- [ ] **Mobile-friendly responsive design** (still desktop-first; target mobile *web browsers*, not a native app). The site is table-heavy, so the main work is making wide tables usable on a phone:
  - Pick a per-table strategy: horizontal scroll with a sticky leftmost column (team/player name) for ranking-style tables, OR card/stack layout on narrow viewports for detail views
  - Hide low-signal columns at narrower breakpoints; expose them via row expand/tap
  - Touch-friendly tap targets (header chevrons, sort/filter controls, pagination) and larger hit areas for clickable names
  - Page chrome (nav, search, filters) collapses cleanly on small screens
  - Charts (radar, rolling form) reflow rather than overflow; legends move below at narrow widths
  - Verify on iPhone- and Android-sized viewports across the main pages (Rankings, TeamDetail, Players, PlayerDetail, Compare, Predict)

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
  - [x] Player matching: normalized name matching (suffix stripping, punctuation removal) + team match (98.6% match rate, 4,911/4,979)
  - [x] Backfill class_year and height_inches on player records from Torvik bio data
  - [x] Rebound backfill from Torvik game-level data (76,385 game rows updated — NatStat had 32% coverage)
  - [x] CLI subcommand: `torvik --year 2026 [--rebounds]`
  - [x] Surface Torvik advanced metrics (GBPM, shot zones, recruiting rank) in player detail API
  - [x] Polish Torvik data display on player detail page (shot zone visualization, GBPM context/percentiles)
  - [x] Use Torvik data as ML features (GBPM as roster aggregate and star-player feature)
  - [x] Replace broken cstat BPM/OBPM/DBPM with Torvik OGBPM/DGBPM passthrough in ML features; retrain (see "cstat BPM/OBPM/DBPM Are Broken" below for resolution)
  - [ ] Use recruiting rank as early-season prior (team-avg recruit rank for first ~3 weeks when model lacks game data)
- [x] **Compute pipeline audit**: cross-checked all derived metrics against Torvik (n=3,255 qualified 2026 players); fixed ORTG/DRTG (Torvik passthrough), AST% and USG% (Basketball Reference formulas), aligned the Python training pipeline, dropped dead BPM columns, and retrained the ML model. See "Compute Pipeline Audit" below.

### 4d: Deployment
- [x] Containerize for Railway: multi-stage Dockerfile (Vite + Rust → slim Debian trixie runtime), `railway.json` with Dockerfile builder
- [x] Deploy to Railway (managed Postgres plugin, public domain on `*.up.railway.app`, ONNX models bundled in image)
- [x] Seed production DB via `pg_dump`/`psql` from local snapshot (full schema + computed tables + cache)
- [x] Serve React build from cstat-api (static file fallback)
- [x] Custom domain on `campom.org` (Cloudflare CNAME → Railway, TLS via Railway/Let's Encrypt)
- [ ] **Auto data consumer (in-season cron)**: Railway cron service running `cstat-ingest update --year <YYYY> && cstat-ingest compute --year <YYYY>` nightly during the season to fetch new games and refresh derived metrics. Deferred until next season tips off — offseason has no new games to consume. Same Docker image as the API service, scheduled via Railway's cron, sharing the Postgres plugin and `NATSTAT_API_KEY` env. Rate-limit budget: ~57 forecast calls + per-team perfs, well under the 500/hr NatStat ceiling.

### 4e: Bracketology & Tournament Resume
- [ ] **Quad 1-4 record tracking**: classify each game by NET-style quadrants (home/away/neutral × opponent rank tier)
- [ ] **Resume page per team**: Q1-Q4 records, signature wins, bad losses, projected seed, bid status (auto / at-large / bubble / out)
- [ ] **NET-replica ranking**: blend Team Value Index (win-based) with adjusted efficiency margin to approximate the NCAA NET; calibrate against published NET when in season
- [ ] **Bracket projector**: Monte Carlo over remaining schedule + auto-bid logic to project the field of 68
- [ ] **Bubble watch dashboard**: at-large probability per team with week-over-week movement indicators
- [ ] API endpoints for resume + bracket queries
- [ ] Frontend: Resume tab on TeamDetail, dedicated Bracketology page

### 4f: CamPom Composite Player Valuation
> Port the methodology in `docs/campom_methodology.md` into the cstat compute pipeline, iterate on the formulas using the predict model as a fitness function, and surface the results on the site. Goal: a "better BPM" that's contextualized by role on the team and produces **separate offensive, defensive, and total composites** at each tier. All required inputs (`ogbpm`, `dgbpm`, `usg`, `Min_per`, `mp`, `GP`, `conf`) already live in `torvik_player_stats` — no new ingestion needed.
>
> Note: the predict model already uses raw Torvik OGBPM/DGBPM as its top features (`diff_w_gbpm`, `diff_w_ogbpm`, `diff_w_dgbpm`, `diff_star_*`). CamPom is the natural refinement of those features, which means **the predict model is both a downstream consumer and the calibration target**.

#### Implement
- [x] **Compute layer**: ported the methodology as `compute_campom` (step 8/13) in `cstat-core/src/compute.rs`. All formulas mirror the doc.
  - `adj_gbpm` (usage-adjusted GBPM)
  - `min_factor` / `mp_factor` (sqrt-scaled volume factors)
  - `gp_weight` (Bayesian shrinkage, k=8)
  - `sos_adj` / `adj_gbpm_sos` (conference quality recomputed each run from the GP≥20 stable cohort, not hardcoded)
  - Composites: `cam_gbpm`, `cam_gbpm_v2`, `cam_gbpm_v3`
- [x] **Offensive / defensive / total as first-class outputs**: o-side and d-side components stored at every tier (`cam_o_gbpm` / `cam_d_gbpm` × original / v2 / v3). Tier-3 SOS is split between o/d proportional to each side's signed contribution to `adj_gbpm`.
- [x] **Schema**: migration 014 extends `torvik_player_stats` with all intermediates (`min_factor`, `mp_factor`, `gp_weight`, `adj_gbpm`, `conf_sos`, `sos_adj`, `adj_gbpm_sos`) plus 12 composite columns (`cam_*` and `min_adj_*` at every tier). Indexed on `(season, cam_gbpm_v3 DESC)` for the rankings query path.
- [x] **Iteration hooks**: 6 tunable constants exposed as `CAMPOM_*` consts at the top of `compute.rs` (`OFFENSE_EXPONENT=0.7`, `DEFENSE_DISCOUNT=0.1`, `USG_REF=17.87357708`, `MINUTES_EXPONENT=0.5`, `GP_K=8`, `SOS_TRANSFER_RATE=0.5`) so each grid-search experiment is a one-line change.

#### Validate
- [x] **Parity gate**: `cstat-ingest campom-parity --year 2026` joins computed composites against `docs/campom_2026_baseline.csv` on `torvik_pid` and diffs every intermediate + final. **PASS** — 4970 matched players, max abs diff 0.0005 across every column (just baseline-CSV truncation). Top of `cam_gbpm_v3` reproduces the doc's elite tier exactly (Boozer 29.17 → Dybantsa 20.76 → Lendeborg 20.59 → Ejiofor 19.68). 2025 also computed cleanly (5,046 players).
  - Caught two latent column-naming bugs in `torvik_player_stats`: `total_minutes` actually stores MP (per-game minutes) and `minutes_per_game` actually stores Min% (share). Migration 014 backfilled the new `min_per` column from `minutes_per_game`; CamPom reads each column for what it truly contains. **Follow-up**: rename these columns to match their semantics (own PR, touches ingest + any consumer that reads them by name).

#### Iterate (with a real fitness function)
- [x] **Wire CamPom into the predict model as features** (negative result — raw GBPM stays). `training/features.py` now selects the GBPM source via `GBPM_VARIANT={raw, cam_v3, cam_v3_psos}` env var; `MODEL_DIR` is overridable per-experiment. Trained 3 variants on 2025+2026, all 49 features, same hyperparameters:

  | variant | backtest MAE | win acc | AUC | 5-fold CV MAE | 5-fold CV AUC |
  |---------|------:|------:|------:|------:|------:|
  | **raw** (baseline) | **8.28** | **71.9%** | **0.790** | **8.46** | **0.803** |
  | cam_v3 (conf-SOS)  | 8.44 | 71.5% | 0.781 | 8.62 | 0.791 |
  | cam_v3_psos        | 8.46 | 71.2% | 0.783 | 8.66 | 0.793 |

  Raw wins every metric. Both CamPom variants regressed by MAE +0.16 / AUC −0.009. **Hypothesis** for the negative result: the predict model is already team-aware via the roster aggregation (`cum_minutes`-weighted) plus standalone `diff_sos` / `diff_w_player_sos` features, so CamPom's per-player USG / mp_factor / SOS adjustments are partly double-counting what the model has already accounted for. Don't ship — production model stays raw GBPM. Production artifacts unchanged; experimental artifacts at `training/models_experiments/{raw,cam_v3,cam_v3_psos}/` for future reference. **Takeaway**: CamPom remains valuable as a *player-ranking metric* (the canonical site-wide ranking per the Ship section below), but isn't a better game-prediction feature than the raw signal it refines.
- [ ] **Hyperparameter grid search against predict-model fitness**: sweep the 6 named constants (`offense_exponent ∈ [0.4, 1.0]`, `defense_discount ∈ [0.0, 0.3]`, `gp_k ∈ [4, 16]`, `minutes_exponent ∈ [0.3, 0.7]`, `sos_transfer_rate ∈ [0.0, 1.0]`, `usg_ref ∈ [16, 20]`). For each combo, recompute composites → retrain predict model → record 5-fold CV MAE. Pick the combo that minimizes error. Beats hand-picked parameters by definition. Coarse pass first (~3 levels per param), then refine around the winning region.
- [ ] **Add role context beyond usage** (this is the "contextualized by role" half): usage is one axis of role. A 30%-usage primary scorer and a 30%-usage point guard play very different roles; usage alone treats them identically. Layer in:
  - Shot diet (3PA rate, rim rate from Torvik) → spacer vs. driver context
  - Playmaking (AST%, AST/TO) → creator multiplier independent of scoring usage
  - Defensive specialty (BLK%, STL%) → role-specific weighting on dgbpm
  - Each new context dimension gets its own constant and joins the grid search. This is what turns CamPom from "weighted GBPM" into a genuinely role-aware metric.
- [x] **Player-level SOS as a parallel Tier-3** (migration 015 + new `cam_gbpm_v3_psos` columns): swaps `conf_sos × 0.5` for `player_sos × 0.15` (transfer rate scaled because cstat's `player_sos` has ~2.5× the magnitude of conf SOS in GBPM units). Kept as a parallel tier — the original conf-SOS `cam_gbpm_v3` stays parity-locked against the baseline CSV; the predict-model iteration step will A/B both. r=0.994 between the two tiers across all 4,890 (2026) / 4,793 (2025) players. Disambiguation works as designed: Penn St's Josh Reed drops 571 ranks (B10 conf bonus +1.88 → personal SOS −1.0; he played mostly bottom-of-league), Texas Tech / Michigan / UNC players jump ~100 ranks because their personal opponent slate was tougher than the conf average.
- [ ] **Other refinements** (lower priority, ordered by expected lift):
  - Empirically calibrate `sos_transfer_rate` against historical transfer outcomes once we have ≥2 seasons of portal moves matched in our data
  - Positional adjustment using class_year + height-derived position bucket
  - Multi-season blend: weighted prior from prior season once 2+ seasons are fully ingested
  - More aggressive defensive skepticism: tunable per-component weight on dgbpm beyond the current `(1 − 0.1 × usg_ratio)` haircut

#### Validate (sanity-check the winner)
- [ ] **External benchmarks**: once a tuned parameterization wins on the predict-model fitness, sanity-check the rankings against external consensus — does the top-50 by `cam_gbpm_v3` align with KenPom POY shortlist, AP all-American teams, projected NBA draft order? Names that look obviously wrong are a signal the optimizer found a degenerate local optimum.
- [ ] **Train/serve parity check**: verify Rust-side computed composites match Python training-side composites on a sampled cohort (same trap that bit BPM pre-PR #25). Lock as a regression test.

#### Ship
> Decisions taken in this batch (deviations from the original §4f Ship plan):
> - **Canonical site rank is `cam_gbpm_v3_psos`** (player-level SOS, not conf-level). The doc itself flagged conf SOS as too coarse, and PSOS disambiguates exactly the players users care about (e.g. high-major guys who scheduled cupcakes vs mid-majors who played up). Conf-SOS `cam_gbpm_v3` stays computed and parity-locked but isn't the headline.
> - **Pitch as a *descriptive* grade, not a forward-predictive feature.** The Iterate experiment showed CamPom doesn't beat raw GBPM at game prediction — but as a season-grade for "how should we rate this player," it's still our best metric. Tier labels (Elite / All-Conference / Quality starter / Rotation / Replacement / Below replacement) reinforce the grade framing.
> - **Skipped the chain breakdown panel** (per direction: "we just pitch our best stat, give it a percentile, and publish it"). Single number + percentile + tier; methodology lives in the doc for the curious.
> - **Skipped the dedicated rankings page**: the Players tab serves that role with the new default sort.

- [x] **API**: `cam_gbpm_v3_psos` + percentile (`campom`, `campom_pct`) added to `GET /api/players` (list, default-sorted by CamPom desc with the existing 5 GP / 10 MPG qualified filter), `GET /api/players/:id` (in `torvik_stats`), `GET /api/teams/:id` (roster, default-sorted by CamPom desc), and `GET /api/players/compare` (via the shared `torvik_stats` block). New `Campom` variant in `PlayerSortField` so the sort param can request CamPom explicitly. Skipped the standalone `/api/players/valuation` endpoint — `/api/players` covers it.
- [x] **Frontend — CamPom column with tier+percentile chip** on:
  - **Players tab**: dedicated `CamPom` column with sort=desc default, score+percentile chip, tier color tint. Closes the "default sort for the players page" item.
  - **TeamDetail roster**: replaced the `GBPM` column with `CamPom`, table inherits API's CamPom-first ordering.
  - **PlayerCompare header panels**: each player gets a CamPom chip (score + percentile + tier) alongside name/team.
  - **PlayerCompare Advanced Metrics table**: `CamPom` row added at the top of the section so the side-by-side comparison leads with it.
  - **PlayerDetail header**: prominent CamPom badge next to the name + archetype (score, percentile rank, tier label).
  - Tier-label helper (`web/src/components/campom.ts`) is the single source of truth for the score → tier → color mapping; reused across every surface.
- [ ] *(Deferred to a follow-up if real-world feedback warrants it)* Most Similar Players carousel: surface CamPom on each tile so similarity is contextualized by quality.
- [ ] *(Skipped per user direction)* PlayerDetail chain breakdown (raw GBPM → usage-adj → minutes-scaled → GP-shrunk → SOS-adj → final). The methodology doc covers it.
- [ ] *(Skipped per user direction)* Dedicated CamPom rankings page with v2/v3/original + offensive-only/defensive-only toggles. Players tab is the de-facto rankings page.

---

## Phase 5: Player Archetypes & Roster Composition
> Cluster players into fantasy-flavored skill archetypes, then build "what if" roster tools on top

### 5a: Player Archetype Engine (D&D Classes)
Cluster D-I players into 10-12 archetypes from skill features (shot diet, rate stats, GBPM components, usage profile). Each player gets a primary class plus secondary-class affinity scores. The naming makes the surface fun and inherently shareable, while the underlying clusters power roster fit scoring in 5b.

- [x] Feature vector per player-season: shot zone share, AST%, USG%, ORB%/DRB%, STL%, BLK%, FT Rate, 3PA rate, OGBPM/DGBPM, MP%
- [x] K-means clustering (k=12) shipped via `training/archetypes.py`; affinity scores stored in `archetype_models` per season
  - [ ] Validate cluster stability across seasons via `adjusted_rand_score` between 2025/2026 cohorts
- [x] Archetype taxonomy (12 classes — Wizard, Sorcerer, Warlock, Bard, Ranger, Barbarian, Paladin, Monk, Cleric, Druid, Rogue, Fighter):
  - **Wizard** — Pure floor general (high AST%, low TOV%, controls tempo)
  - **Sorcerer** — Star scorer / volume creator (high USG%, leads team in points, efficient)
  - **Warlock** — High-variance gunner (heavy 3PA, boom-or-bust efficiency)
  - **Bard** — Pass-first playmaker (high AST%, lower USG, elevates teammates)
  - **Ranger** — 3-and-D wing (3P% + STL%, perimeter sniper/defender)
  - **Barbarian** — Slasher / rim attacker (high FT rate, drives, physical)
  - **Paladin** — Two-way anchor (BLK% + high TS%, defensive leader)
  - **Monk** — High-efficiency role player (elite TS%, low TOV%, disciplined)
  - **Cleric** — Glue guy / connector (defensive intangibles, screens, hustle)
  - **Druid** — Positionless big (stretch + interior, plays inside/out)
  - **Rogue** — Event creator (high STL%/BLK%, off-ball opportunist)
  - **Fighter** — Balanced two-way wing (no specialty, solid all-around)
- [x] Migration: `player_archetypes` table (`player_id`, `season`, `primary_class`, `secondary_class`, `affinity_scores` JSONB, `feature_vector` REAL[]) — migration 013, plus companion `archetype_models` table for centroids/feature stats
- [x] API: `GET /api/players/:id/archetype`, `GET /api/players/:id/similar?k=10`, `GET /api/archetypes` (class glossary + exemplars). Both class ordering and exemplar ranking on the glossary use **CamPom** (the site-wide canonical player valuation) so the page matches what users see on the Players tab when they drill into a class — no more drift between "Top Wizards" on the glossary and the same scoped Players view.
- [x] Player detail UI: archetype badge with hover-tooltip surfacing **primary + secondary class** and affinity bars; "Most Similar Players" carousel with similarity scores
  - [x] Each tile in the carousel has a selection checkbox (cap at 3 selections, since compare supports 4 total); a "Compare" button beneath the carousel activates once ≥1 is selected and deep-links to `/players/compare?ids=<current>,<sel...>`.
- [x] Team detail UI: roster archetype distribution (e.g., "this team rolls 3 Rangers, 1 Druid, 1 Sorcerer") with class-tinted chips
  - [x] Roster table renders **primary + secondary class** on each row (e.g., "Wizard / Bard"); each chip has its own tooltip showing the class blurb.
  - [x] **Identity / Gaps redesign**: replaced the entropy-based "Balance" score (didn't differentiate teams — every roster reads as "diverse") with a per-class index vs the D-I-wide minute-weighted distribution. `index = team_share / d1_share`; values >1.3 with team_share ≥5% surface as **Identity**, values ≤0.5 with D-I share ≥5% surface as **Gaps** (with explicit "missing" labeling at index = 0). Each player's minutes contribute to their primary class at 1.0× and secondary class at 0.5×, capturing hybrid players (a Druid/Sorcerer like Boozer registers Sorcerer presence) without going to full affinity-vector mixing. Implemented as `get_team_archetype_index` SQL with a single CTE-based query; both team and D-I aggregates use identical weighting so the index stays apples-to-apples.
- [x] Compare page UI: each player's header panel shows **primary + secondary class** inline so the archetype framing carries through the whole comparison flow
- [x] **Archetype rankings drill-down**: shipped as `/players?archetype=Wizard[&include_secondary=true]` — clicking a class on the Archetypes page deep-links to the Players tab with the filter applied. The Players tab now infinite-scrolls, defaults to CamPom desc, and lets users re-sort by any raw or rate stat. A class chip + "Include secondary class" toggle live in the page header when the filter is active. Picked option (a) from the original plan (query-param on existing `/players`) so we got column parity, infinite scroll, and the new Raw / Rate column toggle for free.
- [ ] Easter egg: D&D alignment grid placement on player profile (Lawful Good ≈ Monk/Paladin, Chaotic Evil ≈ Warlock/Sorcerer) — half joke, half discovery surface

### 5b: Roster Composition & Transfer Portal Sandbox
- [ ] Player search and comparison across all teams
- [ ] "What if Player X transfers to Team Y?" — recompose team strength
- [ ] Roster fit scoring built on archetypes (redundancy detection: "team has 3 Sorcerers, missing a Cleric")
- [ ] Portal player rankings by projected impact at destination (Δteam rating including archetype fit bonus/penalty)
- [ ] API endpoints for all composition queries
- [ ] Transfer portal sandbox UI

---

## Phase 6: Expansion & Refinement
> Historical depth, brackets, continuous improvement

- [ ] **Full historical data support across the site** (NatStat perfs back to 2007, ~20 seasons). Today only 2025 and 2026 are ingested; expanding to the full archive unlocks career-spanning player profiles, multi-season team trends, "all-time" leaderboards, and dramatically more training data for ML. Per Phase 3 notes, this is the single highest-leverage improvement available to the predict model — current training early-stops at 49-66 iterations, data-starved on two seasons.
  - **Data availability**: `/seasons` confirms perfs for 2007-2026 (20 seasons), play-by-play from 2012+. Each season ≈ 6,200 games, ≈ 6,000 players, ≈ 110k box scores. Rate-limited at 500 API calls/hr → full backfill is a multi-day job leaning on the existing `api_cache` table.
  - **Ingest**: extend `cstat-ingest season` to accept a year range and run the full pipeline (teams → games → perfs → teamperfs → forecasts → elo) per season. Handle historical conference realignment, team renames, and defunct programs without breaking FK constraints. Layer Torvik backfill on the same range (CSV is per-year).
  - **Compute**: run the 13-step compute pipeline per historical season. CamPom, percentiles, adj efficiency, and archetypes are all already season-scoped, but worth sanity-checking early seasons where some advanced fields (e.g., shot zones from Torvik) may be missing.
  - **Schema**: confirm `(season, …)` indices are present and effective at 20× current data volume; spot-check query plans for cross-season joins. Postgres should handle the size fine — main risk is unindexed fan-out on player career queries.
  - **API**: add a `season` query param across all endpoints with a current-season default; new endpoints for career aggregates (`/api/players/:id/career`, `/api/teams/:id/history`); cross-season comparison support in `/api/players/compare`.
  - **Frontend**: site-wide season selector in the nav; player detail shows a multi-season career table + per-season CamPom/archetype trajectory; team detail shows year-by-year record + adj efficiency trend; cross-season player comparison ("2023 UConn's Hurley-era guards vs 2025"); game prediction page lets you pick historical matchups and back-test the model.
  - **ML**: retrain on all seasons (incremental: 2024 → 2023 → … to measure marginal lift per season added). Watch for distribution shift (rule changes, three-point line move in 2008, COVID-shortened 2021).
  - **Stretch**: all-time leaderboards (best CamPom seasons ever, GOAT teams by adj efficiency margin), program-history pages, cross-era archetype distribution shifts.
- [ ] Backtest models across multiple seasons
- [ ] Tournament bracket simulator (Monte Carlo, inspired by gravity project)
- [ ] Season simulation engine
- [ ] Model accuracy dashboard with calibration tracking
- [ ] Automated daily data refresh during season
- [ ] Conference/team/player trend analysis over time
- [ ] **Native cstat player impact metric** (alternative to Torvik GBPM passthrough). Frame as a *descriptive* grade — "what's this player's value, derived purely from cstat's own machinery" — not a predictor (the §4f experiment already showed CamPom-style adjustments don't beat raw GBPM as ML features; this is a different goal). Approach: non-linear regression of team-game outcomes on roster-composition features (player IDs × minutes) to attribute per-player coefficients. Acceptance: matches or beats CamPom on (a) year-over-year rank stability for returning players and (b) external benchmarks (KenPom POY, AP All-American). **Caveat — re-attempting failed work**: cstat's prior native BPM (`compute.rs` pre-PR #25) tried this with linear box-score formulas and got r=0.075 with Torvik OBPM. Don't repeat that approach; the limit at our data resolution is team-game level (no play-by-play) and box-score-derived linear coefficients are exactly what blew up. A LightGBM-on-team-game-outcomes attribution is a different methodology and worth trying — but it's a multi-PR project and would need its own design doc.

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

### cstat BPM/OBPM/DBPM Are Broken (P1 — Fixed)
Sanity-check vs Torvik (2026, 3,255 qualified players matched):
- cstat OBPM ↔ Torvik OBPM: **r = 0.075**, sd of diff = 30.0
- cstat DBPM ↔ Torvik DBPM: **r = −0.026**, sd of diff = 30.1
- cstat BPM ↔ Torvik BPM: r = 0.523 (mean +6.54 vs Torvik −0.58 — biased and floored at 0)
- cstat OBPM range: −1649 to +15.6; cstat DBPM range: 0.1 to +1655 (vs Torvik ±15)

**Root causes** (`crates/cstat-core/src/compute.rs`):
1. cstat "BPM" was `AVG(game_score)` per player — not Daniel Myers BPM. Game score skews positive, so it was biased ~+7 and floored at 0.
2. OBPM/DBPM split divided by `(off_component + def_component)`, where `off_component` includes a `ppg / fg%` term that explodes negative on low-fg% volume scorers (e.g., Rob Brown @ 35.1% → OBPM −1649, DBPM +1655).
3. These broken values flowed into `features.rs` as roster aggregates (`w_bpm`, `w_obpm`, `w_dbpm`, `star_bpm`) and into the trained ML models.

**Fix (shipped)**: Replaced cstat's compute with a Torvik passthrough.
- `compute.rs`: `compute_player_season_stats` no longer populates `bpm`. `compute_individual_ratings` no longer populates `obpm`/`dbpm` (and now NULLs out any stale values). `compute_player_percentiles` no longer computes `bpm_pct`. The `pss.bpm/obpm/dbpm` columns remain in the schema as NULL — kept for now so existing API consumers don't break.
- ML features (`features.rs` / `inference.rs` / `training/features.py`): dropped `diff_w_bpm`, `diff_w_obpm`, `diff_w_dbpm`, `diff_star_bpm`; added `diff_w_ogbpm`, `diff_w_dgbpm`, `diff_star_ogbpm`, `diff_star_dgbpm` from Torvik. Stays at 49 features.
- API roster query (`get_team_roster`) now serves Torvik `gbpm` instead of stale `pss.bpm`. Frontend TeamDetail roster column relabeled BPM → GBPM. PlayerDetail / PlayerCompare were already Torvik-only.

**Backtest comparison** (chronological 80/20, 2025+2026):
| Metric        | Before (broken BPM) | After (Torvik OGBPM/DGBPM) |
|---------------|---------------------|----------------------------|
| Margin MAE    | 8.68 pts            | **8.47 pts**               |
| Win accuracy  | 70.0%               | **71.1%**                  |
| Win AUC       | 0.764               | **0.773**                  |
| 5-fold CV MAE | 8.86                | **8.63**                   |
| 5-fold CV AUC | 0.735               | **0.791**                  |

Top features now: `diff_w_gbpm` (271), `diff_w_ogbpm` (92), `diff_w_dgbpm` (80) — Torvik impact metrics dominate the model.

### Compute Pipeline Audit (Fixed)
Cross-checked every cstat-computed metric against Torvik on 2026, qualified players (≥10 GP, ≥10 MPG), n=3,255.

| Metric  | corr  | cstat mean | Torvik mean | bias (rescaled) | verdict |
|---------|-------|-----------:|------------:|----------------:|---------|
| PPG     | 0.997 |       8.61 |        8.60 |          +0.01  | ✓ healthy |
| RPG     | 0.996 |       3.59 |        3.57 |          +0.02  | ✓ healthy |
| APG     | 0.996 |       1.60 |        1.60 |           0.00  | ✓ healthy |
| BPG     | 0.995 |       0.37 |        0.37 |           0.00  | ✓ healthy |
| SPG     | 0.993 |       0.76 |        0.76 |           0.00  | ✓ healthy |
| BLK%    | 0.990 |       2.13 |        1.98 |          +0.15  | ✓ healthy |
| ORB%    | 0.987 |       6.11 |        5.29 |          +0.81  | ✓ healthy |
| FT Rate | 0.987 |      35.71 |       35.88 |          −0.18  | ✓ healthy (after ×100) |
| DRB%    | 0.984 |      14.34 |       12.91 |          +1.43  | ✓ healthy |
| TOV%    | 0.964 |      14.42 |       16.46 |          −2.04  | ⚠ small bias (after ×100) |
| eFG%    | 0.962 |      51.85 |       51.22 |          +0.62  | ✓ healthy (after ×100) |
| FT%     | 0.961 |       0.71 |        0.71 |          +0.00  | ✓ healthy |
| TS%     | 0.960 |      55.49 |       54.38 |          +1.11  | ✓ healthy (after ×100) |
| STL%    | 0.958 |       2.03 |        1.87 |          +0.16  | ✓ healthy |
| 3P%     | 0.940 |       0.31 |        0.29 |          +0.02  | ✓ healthy |
| **USG%** | 0.924 → **0.971** | 17.65 → **19.41** | 19.11 | −1.46 → **+0.30** | ✓ box-score formula |
| **AST%** | 0.898 → **0.982** |  7.42 → **13.44** | 12.48 | −5.05 → **+0.96** | ✓ formula fixed |
| **DRTG** | 0.718 → **0.999** | 106.5 → **109.5** | 109.5 | −3.02 → **+0.01** | ✓ Torvik passthrough |
| **ORTG** | 0.702 → **0.998** |  92.0 → **107.5** | 107.5 | −15.5 → **+0.03** | ✓ Torvik passthrough |

Plus team-level checks: `adj_offense=107.3`, `adj_defense=108.6`, `adj_efficiency_margin=−1.3`, `adj_tempo=67.4` — KenPom-style values look healthy. `game_score` matches the textbook Hollinger formula. Rolling averages use a strict point-in-time `ROWS BETWEEN 5 PRECEDING AND 1 PRECEDING` window (no leakage; partial windows for early-season games are not flagged but feed downstream features as-is).

**Fixes shipped:**

- **`compute_individual_ratings`**: replaced the broken Dean-Oliver-style heuristic with a Torvik `o_rtg` / `d_rtg` passthrough — same pattern as the PR #25 BPM fix. `net_rating = o_rtg − d_rtg`. Stale values are NULLed at the start of the step so unmatched players (~1.4%) don't show garbage.
- **`compute_player_season_stats` AST%**: patched to the Basketball Reference formula `AST / ((MP / (Team_MP / 5)) × Team_FGM − Player_FGM)`, aggregated over the season as `AST / (5 × ΣMP × ΣTeam_FGM / ΣTeam_MP − ΣFGM)`.
- **`compute_player_season_stats` USG%**: replaced `AVG(per-game NatStat usgpct)` with the Bball Ref box-score formula `(Plays × Tm_MP/5) / (MP × Tm_Plays)` where `Plays = FGA + 0.44×FTA + TOV`. Closes the −1.5pp drift; gets off NatStat's black-box value.
- **Training pipeline alignment** (`training/features.py`): updated `ast_pct_g` (was `AST/Tm_FGA`) and `usage_g` (was NatStat per-game) to match cstat's Bball Ref formulas. Joined `team_game_stats` to load `team_minutes`. Eliminates train/serve formula drift on AST% and USG%.
- **Train/serve skew on `w_ortg` closed.** Inference reads `pss.offensive_rating` which now holds Torvik o_rtg (mean ~107) instead of the broken heuristic (mean ~92) — closes the ~18-point distribution shift relative to Python's `points/poss × 100` (mean ~110). Residual ~3-point gap is methodology only.
- **Retrained ML model.** With aligned features and corrected formulas, backtest improved from PR #25 baseline:

  | Metric        | PR #25  | This PR     |
  |---------------|--------:|------------:|
  | Margin MAE    | 8.47    | **8.28**    |
  | Win accuracy  | 71.1%   | **71.9%**   |
  | Win AUC       | 0.773   | **0.790**   |
  | 5-fold CV MAE | 8.63    | **8.46**    |
  | 5-fold CV AUC | 0.791   | **0.803**   |

  Top features unchanged in shape: `diff_w_gbpm` (359), `diff_w_dgbpm` (127), `diff_w_ogbpm` (118) still dominate.

- **Dropped dead `bpm` / `obpm` / `dbpm` / `bpm_pct` columns** (migration 012). Left over from PR #25 with no remaining consumers; verified across `crates/`, `web/`, and `training/`. Removed corresponding fields from `PlayerSeasonStats` / `PlayerPercentiles` model structs and the `SET … = NULL` clause in `compute_individual_ratings`.

- **Stale comment fixed** (`compute_team_four_factors`): the inline comment said team ORB% was "approximate for now (needs opponent data)" but the actual SQL has used a `team_game_stats` self-join via `reb_agg` for opponent DREB since migration 003.

**Deferred (low priority):**

- **TOV% (−2pp drift).** Formula matches Bball Ref; remaining gap is methodology (likely Torvik uses minutes-weighted team possessions in the denominator).
- **Mixed scale convention.** `pss` stores rate stats as fractions (0–1) while Torvik stores percents (0–100). Anything that joins or compares the two needs to normalize. Worth a follow-up to standardize.

### Player Rate Stats Were Per-40-Min Proxies (P2 — Fixed)
`compute_player_rates` originally computed ORB%, DRB%, STL%, BLK% as per-40-minute proxies. **Fixed**: Now uses proper possession-based Basketball Reference formulas with team/opponent game stats (e.g., `ORB% = 100 × (ORB × (Tm MP / 5)) / (MP × (Tm ORB + Opp DRB))`). Also added FT Rate (FTA/FGA) and rate stat percentiles. Player name normalization (suffix stripping, punctuation removal) improved Torvik↔NatStat match rate to 98.6%.

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
