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
- [x] Game prediction interface v1 (pick two teams → predicted margin + win prob) — bare-bones picker shipped in PR-era; the **Predict page revamp** below is the next destination.
- [x] **Predict page revamp** — shipped in PR #43. The page is now the de-facto matchup destination: side-by-side header panels (record, AdjEM, CamPom leaders, archetype distribution, recent form), 3-way venue picker (home / away / neutral) with symmetric neutral-site predictions, big margin + win% chip, "Keys to the Game" narrative bullets, Side-by-Side stats panel, and per-team Four-Factors tug-of-war bars. Backend: `POST /api/predict` accepts `{home, away, neutral, season}` and returns margin + win% + top contributions in one round-trip; reuses `features.rs::build_game_features` so on-the-wire and trained features stay aligned. Model retrained on 2024+2025+2026 (margin model 157KB → 353KB; metrics in `training/models/model_meta.json`). Originally shipped with ablation-based attribution + a hand-coded `homeAdvantageSign` direction lookup; the TreeSHAP follow-up below retired both.
- [x] **Predict follow-up — TreeSHAP in pure Rust**: replaces the ablation-based attribution. New `crates/cstat-core/src/treeshap.rs` parses the LightGBM v4 text dump (`margin_model.lgb`, ~460KB shipped alongside the existing `.onnx`) and runs the canonical Lundberg/Erion/Lee TreeSHAP algorithm; `Predictor::predict_with_contributions` keeps the same return shape so the API and route handler are unchanged. Parity gate `treeshap_matches_lightgbm_baseline` diffs against LightGBM's own `pred_contrib` for 20 sample vectors — max abs diff = 7.11e-15 (essentially fp precision). Frontend: keys panel still uses `homeAdvantageSign` for the *data-faithful direction* (which team has the better raw stat) but switched the *importance weighting* from `|ablation|` to `|SHAP|`. The original ROADMAP carve-out claimed TreeSHAP would retire the data-direction lookup outright; the Purdue vs Michigan case (Michigan has a 0.079 better opp eFG% but TreeSHAP still attributes that feature toward Purdue due to a non-monotonic interaction) showed that the lookup serves a separate product purpose — keeping the panel a stats narrative rather than a model narrative. The split now: data direction names the leader, TreeSHAP magnitudes weight importance.
- [x] **Predict follow-up — totals / tempo model for final scores** (PR #48). Predict page headline now reads `Duke 75 — UNC 72` with margin/win% on a secondary line; TeamDetail Projected column and ScoreTicker upcoming tiles render the projected score the same way. End-to-end:
  - **Training**: `total` target in `features.py`; `train_total_model` + 5-fold CV + chronological backtest + final-on-all-data fit in `train.py`. Backtest MAE 13.58 / R² 0.179. **Lesson learned**: the 49 `diff_*` features cannot predict totals (diffs throw away absolute level — `diff_tempo=0` is ambiguous between two slow teams and two fast teams). Added 9 level-sensitive `sum_*` companion features (`sum_adj_tempo`, `sum_adj_offense`, `sum_adj_defense`, `sum_effective_fg_pct`, `sum_opp_effective_fg_pct`, `sum_w_ppg`, `sum_w_ortg`, `sum_off_rebound_pct`, `sum_def_rebound_pct`) which occupy 6 of the top 7 importance slots. Margin/win models stay on the unchanged 49-feature diff matrix; `model_meta.json` carries separate `features` (49) and `total_features` (58) lists.
  - **Inference (`crates/cstat-core/src/inference.rs`)**: `Predictor` carries a third `total_session` alongside margin/win, loads `total_model.onnx`, runs the 58-feature vector via `TOTAL_NUM_FEATURES`. `Prediction` now includes `predicted_total: f32`. The 49-feature builder was extended in place to also produce the 9 sums.
  - **API (`crates/cstat-api/src/routes/predict.rs`)**: response carries `predicted_total`, `predicted_home_score`, `predicted_away_score` (`(total ± margin) / 2`, rounded). Neutralization uses *symmetric* averaging for totals (`0.5 * (fwd + rev)`) and antisymmetric for margin (`0.5 * (fwd − rev)`); both invariants are covered by tests. The shared helper consumed by `routes/teams.rs::team_detail` and `routes/ticker.rs` returns home/away scores so the Projected column and ticker tiles read from one source.
  - **Frontend**: `Predict.tsx`, `ScoreTicker.tsx`, and `TeamDetail.tsx` all render projected scores; the API serves canonical `projected_score_team` / `projected_score_opp` strings on schedule rows so the format stays consistent across surfaces.
  - **Honest precision framing**: backtest MAE 13.58 is materially worse than KenPom (~9) and Vegas (~7-8); projected scores are framed as KenPom-style approximations, not betting-grade. MAE floor on diff+sum features at 3 seasons of data appears to be ~13.5 — the next material lever is **full historical data** (§6, NatStat archive 2007→2026), not more feature engineering.
- [ ] **Predict follow-up — point-in-time historical predictions** (retroactive `game_forecasts`) *(seed data shipped; DB + UI work deferred)*: today the Projected column always reflects "what we'd predict now" using current-state team features, which is slightly odd on completed games (we already know the result). For completed games, render "we predicted X / actual was Y" using only data available *before* tip-off.
  - **Head start landed during the totals-model PR**: `train.py` now writes `models/oof_predictions.csv` (12,821 games × 13 cols, 2.8MB) — leak-free 5-fold OOF predictions for margin / total / win prob / derived home_score / derived away_score, keyed by `game_id` + `game_date` + `season` + team UUIDs. KFold(shuffle=True, random_state=42) covers every game exactly once across folds, so each prediction comes from a model that didn't see that game. Suitable seed data for the historical backfill — though "OOF on a 5-fold random split" is a weaker honesty guarantee than "walk-forward, trained only on chronologically earlier games." For early-season games OOF is fine; late-season games would ideally be predicted from a model trained only on earlier seasons + earlier-this-season games (true point-in-time). Consider this the MVP-grade backfill; the walk-forward pass is the polish step.
  - **Deferred work to actually surface historical predictions**:
    - **Schema**: extend `game_forecasts` (already used for NatStat forecasts) with cstat columns (`cstat_pred_margin`, `cstat_pred_total`, `cstat_pred_home_win_prob`) or add a `source` discriminator column and store one row per (game_id, source). Decision favors a discriminator if we ever want to backfill predictions from older model versions side-by-side; column-extension if we never will. Migration is small either way.
    - **Ingest path**: a `cstat-ingest backfill-predictions [--year YYYY]` subcommand (or a one-shot Rust binary) that reads `models/oof_predictions.csv` and writes to `game_forecasts`. Or in Python: a small `training/backfill_predictions.py` that uses sqlalchemy to land the CSV. Python is faster to write; Rust is more consistent with the rest of the binary.
    - **Walk-forward refinement** *(optional polish)*: re-generate predictions chronologically — for each game, train a model on *all earlier games only* and predict that game. ~12k retrains is expensive (~40 min naively); cheaper version is "train per-month rolling models" (one model per month, predicts that month's games). This gives true point-in-time honesty for the schedule UI; OOF is good enough for a calibration dashboard.
    - **Frontend**: TeamDetail schedule completed rows replace the blank Projected column with a "we said X" chip + actual margin from the Score column for a side-by-side accuracy receipt. ScoreTicker recent-results tiles can show a small "predicted ✓ / predicted ✗" badge based on whether the predicted winner matched. New `/calibration` page (or section on Predict) that bins OOF predictions by predicted-win-prob and shows actual hit-rate per bin (the §6 "Model accuracy dashboard with calibration tracking" item).
    - **Totals validation**: once historical predictions land, the totals model gets a real accuracy receipt — does its 13.58 backtest MAE hold up over a calibration plot? The OOF dump already lets us answer this without DB plumbing (just open the CSV in pandas and bin), so this is doable today as a sanity check before building the dashboard.
- [x] **Predict follow-up — embedded Roster Compare panel**: under the headline margin + win% chip, side-by-side per-team roster cards (top 8 by CamPom each) with name, archetype chip, MPG/GP, and CamPom score+tier. Both rosters travel in the same `/api/predict` response (parallel `get_team_roster` calls in the route handler) so the page stays a one-round-trip surface. The radial roster plot from §5b is the next thing that drops into this same panel; the four-factor tug-of-war already lives below as its own panel.
- [x] **Predict follow-up — Previous Matchups section**: when the two teams have already played this season, the Predict page renders one card per meeting — date / venue, final score with winner highlight, top performer per side (highest game_score), and a "Show full box score" expander revealing per-player + per-team rows pulled from `player_game_stats` / `team_game_stats`. Hidden when no prior meetings. New `queries::get_prior_meetings` + box-score helpers; everything travels in the same `/api/predict` response.
- [x] **Predict follow-up — schedule & ticker click-through**: TeamDetail schedule rows (Score cell on completed games, Projected cell on upcoming) and homepage `ScoreTicker` past tiles all deep-link to `/predict?home=…&away=…[&venue=neutral]`. The Predict page reads URL params on mount and auto-submits, so deep-links are a first-class destination. Already-existing upcoming ticker tiles continue to deep-link the same way.
- [x] **Predict follow-up — Projected column on TeamDetail schedule**: upcoming games show predicted margin from the requested team's perspective + win probability inline. Backend extension: `routes::teams::team_detail` now runs `predict_margin_and_winprob` for each unplayed schedule row (rounds to one decimal, masks failures so the row still renders without a projection). Completed rows leave the column blank — the actual result already lives in the Score column.
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
- [x] **Cross-season URL stability + page metadata**: the season selector now keeps you on the same school/player when you change years (Duke 2026 ↔ Duke 2025; Ja'Kobi Gillespie at Tennessee 2026 ↔ Maryland 2025). Backend resolves cross-season UUIDs via `natstat_id` in `/api/teams/:id` and `/api/players/:id`; the frontend redirects to the canonical UUID and `?season=` so refresh / share / browser-back all work. Year dropdown shows bare years (2026 / 2025) instead of the YY-YY range. Each route sets `document.title` via a small `usePageTitle` hook so a shared link to Cooper Flagg's page reads "Cooper Flagg 2026 — CamPom" in the browser tab. Note: `document.title` is set client-side, so OG/Twitter scrapers still see the static `index.html` "CamPom" until SSR/prerender lands.
- [ ] **Tables code-quality follow-ups** (deferred from the landing-page polish review — none load-bearing, all small):
  - **Extract shared number formatters**: `TeamDetail.tsx` (inside `RosterTable`) and `Players.tsx` both define their own `fracPct` (×100 for fractions like AST%/TOV%) and `pointPct` (no scaling for ORB%/DRB%/STL%/BLK%) helpers — the same code in two places. Pull into `web/src/components/format.ts` (or extend `pctile.ts`) so the mixed-scale convention has one source of truth and a future schema rename only touches one file.
  - [x] **Rankings team-name as `<Link>`**: cell now renders a `<SeasonLink>`, matching the Players pattern. Removed the row-click handler from Rankings, Players, and TransferPortal so only the name cells navigate — keeps middle-click / right-click / screen-reader semantics intact and stops accidental nav from clicks on data cells.
  - **`gradientCellStyle` closure allocation**: the helper in `Players.tsx` returns a fresh closure on every `buildColumns` call. AG Grid handles it fine at our row counts, but if the page ever re-renders frequently (e.g. when we add filter chips, archetype-aware coloring, or a season selector), memoising the column defs or hoisting the cellStyle factories would avoid stale-closure pitfalls. Defer until there's a measurable issue.
- [x] **Spider/radar chart axis transparency** (PR #44 — 8-axis radar refresh). Tap-toggle `RadarAxisTooltip` per prong shows the contributing stat, raw value, and percentile feeding the spoke. Axis-to-stat mapping centralised in `web/src/components/radarAxes.ts` so PlayerDetail (single radar) and Compare (overlaid) draw from one source. Replaces the prior opaque labels.
- [x] **Score ticker / recent results** (PR #44). Auto-scrolling marquee at the top of the Rankings homepage pairs upcoming-game tiles (predicted margin + win%) with recent finals. Honors `prefers-reduced-motion`; hidden when both halves are empty. Past tiles now deep-link to `/predict` (matchup view + Previous Matchups), upcoming tiles already did.
- [x] **Mobile-friendly responsive design** (PRs #39, #41). Burger nav with collapsing page chrome, horizontal-scroll tables with a sticky leftmost name column, tap-toggle tooltips for touch, mobile table polish, and a `useIsMobile` hook for per-component breakpoint behavior. Verified across Rankings, TeamDetail, Players, PlayerDetail, Compare, Predict, Archetypes, and TransferPortal.

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
- [x] **Player roster ingest authority for `team_id`**: NatStat `/players/mbb/{TEAMCODE}` has no historical-season filter — it always returns the *current* roster, so running `cstat-ingest players --year 2025` was stamping 2026 rosters as 2025 and overwriting box-score-derived `team_id`s. Symptom: 769 of 5,197 (~15%) 2025 player rows had the wrong team — Maryland was missing real 2024-25 contributors (Gillespie, Rice, Gapare, Palmer, Pierce — all stamped to their 2026 destinations); ~2,800 ghost rows existed for 2026-only walk-ons / transfer-ins. Fix: `players.rs::upsert_player` no longer touches `team_id` on conflict (box-score path is the sole authority); roster ingest emits a one-time warning when run against a non-current season and only enriches metadata fields. Data correction relinked 153 mis-routed Torvik rows to the correct twin and deleted 2,747 fully-orphaned ghosts (38 Torvik-only ghosts with no NatStat box scores remain — invisible in roster pages, separate investigation). Adding more historical seasons (2024, 2023, …) is now safe: `cstat-ingest season --year YYYY` is sufficient and `players --year YYYY` can't damage `team_id`.

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
  - [x] **Combined-cohort training for cross-season stability**. Replaced per-season clustering (28% returning-player primary stability) with one k-means fit across the union of seasons (45.7% stability, 75.2% with secondary-class match). Same player → same cluster → same class regardless of which season we look at. Trade-off: doesn't track year-to-year evolution; revisit at the 5+ season horizon (Phase 6). Full methodology + retraining playbook in `docs/archetypes_methodology.md`.
- [x] Archetype taxonomy (12 classes — descriptions reflect the actual cluster centroids after the combined-cohort refit; all three sources of class identity — `archetypeColors.ts::CLASS_TAGLINES`, `Archetypes.tsx::CLASS_DEFS`, and `archetypes.py::ARCHETYPE_SIGNATURES` — are kept in sync):
  - **Wizard** — Elite lead-guard creator (heaviest minutes, high AST%, positive OGBPM)
  - **Sorcerer** — High-volume star scorer (highest USG%, strong impact, heavy minutes)
  - **Warlock** — Three-point specialist (heaviest 3PA share, lowest rim rate, boom-or-bust)
  - **Bard** — Pass-first distributor (high AST%, low USG%, modest impact)
  - **Ranger** — Perimeter spacer (high 3PA share, high STL%, low USG; not true 3-and-D)
  - **Barbarian** — Interior finisher (highest rim share, lowest 3PA, high BLK%)
  - **Paladin** — Defensive anchor (elite BLK%, highest DGBPM, rim defense)
  - **Monk** — Disciplined wing star (high OGBPM, heavy minutes, high 3PA share)
  - **Cleric** — Low-volume interior connector (rim/mid finisher, low USG%, modest impact)
  - **Druid** — Elite two-way big (highest OGBPM + DGBPM combined, owns the glass)
  - **Rogue** — Disruptive two-way wing (high STL%/BLK%, high DGBPM)
  - **Fighter** — Balanced two-way rotation (multi-axis modest positives, rotation minutes)
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
- [x] **Transfer Portal v1 — 247 list × CamPom value delta**: scraped 247Sports' 2026 portal list to `data/transfers/2026.json` (embedded into the binary at compile time via `include_str!` so Railway deploys don't need a `data/` mount). New `GET /api/transfers/{year}` endpoint (`crates/cstat-api/src/routes/transfers.rs`) joins the 247 list to cstat players by normalized name + previous team and decorates each row with `player_id`, primary/secondary archetype, CamPom score + percentile, MPG, GP, and resolved `previous_team_id` / `next_team_id` for deep-linking. `TransferPortal.tsx` renders the AG Grid view with a `rank_delta = rank_247 − rank_cstat` "value" column (positive = CamPom values the player higher than 247 does), tier-colored CamPom chip, archetype chip, and team links to both old and new schools. Surfaced as a tab on `Players.tsx`. This is the foothold for the rest of 5b — same data plumbing will feed projected-impact rankings and the what-if sandbox.
- [x] **DB-backed transfers ingest pipeline**. The portal is permanent in college basketball post-2021 NCAA rule change, so treating each year as a committed JSON file scaled poorly: PR diff churn on every refresh, full Cargo re-link on data changes, binary grows linearly with seasons, no incremental refresh, no SQL joins to cstat `players`. Replaced with a `transfers` table + paginated ingest CLI, mirroring the NatStat / Torvik pattern.

  **Bootstrap data**: a one-shot full fetch was performed 2026-05-10 → `data/transfers/2026_raw.json` (5.6 MB, 2,620 players across 105 pages, zero failures, matches the API's `expected_count` exactly). Gitignored (`/data/transfers/*_raw.json`) so it stays local; used as the seed for the first DB load via `--bootstrap-from`.

  **Enum vocab confirmed from the full fetch** (and reflected in CHECK constraints + Rust ALLOWED_* arrays): `status` ∈ `Entered` (1,517) / `Committed` (1,081) / `Withdrawn` (22). `eligibility.type` ∈ `Immediate` (2,570) / `Withdrawn` (46) / `PendingAppeal` (3) / `TBD` (1). `institutionStatus` ∈ `HS` (1,751) / `T` (869). Multi-destination edge case: 4 of 2,620 rows have 2 destinations (likely crystal-ball predictions); the rest have 0 or 1.

  **Migration** (`migrations/019_transfers.sql`):
  - One row per `(year, tfs_key)` UNIQUE constraint. `tfs_key` is 247's stable `player.key`.
  - Enums kept as TEXT + CHECK constraints, not native PG ENUM. Reason: the vocab belongs to 247; native ENUM needs `ALTER TYPE ADD VALUE` + deploy when they add a state. CHECK lets us widen with a one-line migration.
  - Source flattened to top-level columns; primary destination flattened (`transferred=true` wins, else highest `percentage`); full `destination[]` array preserved in `raw_player JSONB` for the multi-commit cases.
  - Generated `full_name` STORED column + 7 indexes (year+rank, year+status, source/destination keys, lower(full_name), last_update_date DESC, partial cstat_player_id reverse-join).
  - `cstat_player_id UUID REFERENCES players(id)`, resolved post-ingest via case-insensitive name + source-team match.

  **Client** (`crates/cstat-ingest/src/tfs.rs`): lean `TfsClient` — JWT from `TFS_247_JWT` env var, token-bucket rate limiter defaulting to 1 req/sec (configurable via `TFS_247_RATE_PER_HOUR`), 30s request timeout, exponential backoff on 5xx/429 (max 4 retries), short-circuit on 401/403 with `JwtExpired`, polite User-Agent (`cstat-ingest/0.1 (+https://campom.org)`).

  **Ingest module** (`crates/cstat-ingest/src/ingest/transfers.rs`):
  - `ingest_live(client, pool, year, incremental)` — paginate the full ~105 pages with optional cursor short-circuit on `--incremental`.
  - `bootstrap_from_snapshot(pool, year, path)` — load `data/transfers/{year}_raw.json` without hitting the network. Warns if the snapshot's `year` metadata disagrees with `--year`.
  - `resolve_cstat_joins(pool, year)` — case-insensitive `(full_name, source_institution)` → `players.id` UPDATE pass; v1 uses exact lower-case match (suffix-stripping deferred to v2).
  - **Resilience**: a single bad row from 247 cannot kill the 2,620-row ingest. Missing `key` → warn + skip. Unknown `status` value → warn + skip (would have failed the CHECK). Unknown `institution_status` / `eligibility_type` → warn + NULL (raw value stays in `raw_player` for forensics).

  **CLI**: `cstat-ingest transfers --year YYYY [--incremental] [--bootstrap-from PATH] [--no-resolve-players]`.

  **Tests**: 12 unit tests covering `parse_page`, `primary_destination` tie-breaker, `sanitize_enum`, `parse_dt`.

- [x] **Migrate `/api/transfers/{year}` route from embedded JSON to DB**. `crates/cstat-api/src/routes/transfers.rs` now reads from the `transfers` table via `sqlx::query_as` (no more `include_str!` / `EMBEDDED_TRANSFERS`). Response shape preserved except `rank_247` is now nullable — the embedded JSON era was a top-N scrape with rank always present, the DB carries the full portal (1,497 rows for 2026: 808 ranked + 689 unranked), so the long tail now appears for the 2027-projection roster aggregator to consume. The rankings page filters to ranked-only via the `rank_cstat` assignment (only assigned when both CamPom and 247 rank present, so the displayed rank matches on-screen position). Column choice: `transfer_rank` (247's within-portal rank), not the composite `rank` — gives bit-for-bit parity with the old top-N for the overlapping rows (0/247 names lost; 93 extra ranked + 1,157 unranked added). Backend type cleanup: `rating: Option<f32>` end-to-end (dropped the unnecessary `rating::float8` cast); early-return 404 after the transfers query so unknown years skip the heavy candidates join; normalized error messages across the three queries. All three currently-supported years (2024 / 2025 / 2026) backfilled locally via the live 247 API: 1,224 + 1,636 + 1,497 = 4,357 rows. Raw snapshots dumped from `raw_player JSONB` to `data/transfers/{2024,2025,2026}_raw.json` (gitignored) so re-seeding can happen without a JWT. **Prod swap pending**: cleanest ordering is (1) trigger migration 019 on prod by running any cstat binary against `PROD_DATABASE_URL` (e.g. `DATABASE_URL="$PROD_DATABASE_URL" cargo run --bin cstat-ingest -- status` — connects, calls `db.migrate()`, exits); (2) `./scripts/sync_to_prod.sh` pushes the local `transfers` rows (auto-discovers the new table); (3) deploy the new binary. Zero user-visible 404 window because data lands before code. Reverse order works too but the route 404s for the gap between deploy and sync. Same DB-backing pattern applies cleanly to the draft tables (`draft_early_entrants`, `draft_big_board`) once those have a regular refresh cadence — out of scope for this batch.
  - [x] **`resolve_cstat_joins` short-name + alias fix** (was: 0/4,357 rows resolved). Ported the route's `normalize` (accent fold + suffix strip) and `team_match_score` (short_name exact → alias table → bare prefix) into `crates/cstat-ingest/src/ingest/transfers.rs`. Matching moved from a single SQL JOIN to Rust: load portal rows + cstat candidate stints (one row per (player, team) via `player_season_stats` so mid-season transfers are disambiguable), group by normalized name, score each candidate's team against 247's `source_institution`, batch-UPDATE via `UNNEST`. Resolution rate jumped 0 → ~87% across all three years (1,064 / 1,418 / 1,298 for 2024 / 2025 / 2026). Residue is dominated by non-D1 schools (Le Moyne, Mercyhurst, Saint Anselm, Northwestern College) that have no cstat rows by construction — that's the ceiling, not a bug. Aliases verified: `UConn` → `Connecticut Huskies`, `NC State` → `North Carolina State Wolfpack`, `Miami` → `Miami (Fla.) Hurricanes` (FL alias outranks the OH prefix fallback via `min_by_key` on the score). **Fallback safety**: when team scoring finds nothing, resolution only fires if the same-name bucket has exactly *one* candidate. The dangerous multi-candidate fallback case (silently binding two distinct players who share a name) is dropped — costs 1 row/year in 2026, keeps 80 safe single-bucket fallbacks across the three years. Log surfaces `team_score_miss` (diagnostic) and `single_bucket_fallback` (resolution-bookkeeping) as separate counters. 7 new unit tests cover the helpers. Helpers are duplicated between the route and the ingest with cross-reference doc comments; promote to a shared module if a third consumer appears. `cstat_player_id` is now usable for SQL joins from the 2027 projection aggregator (the original load-bearing motivation).
  - **Follow-up (product decision)**: 22 `Withdrawn` rows in 2026 (and similar counts in 2024/2025) currently appear on the rankings page — same behavior as the embedded JSON, but probably want to filter them out for projections. Defer until projections need them.
- [ ] **Projected impact at destination (Δteam rating)**: replace the raw 247 vs CamPom delta with a destination-aware projection. Two framings, sharing one projection engine.
  - [x] **Engine v1 — roster-only model trained** (`training/train_roster_model.py`, `training/models/roster_model.onnx`). One row per (team_id, season), 1,089 team-seasons across 2024/2025/2026. Target: `team_season_stats.adj_efficiency_margin`. 36 features: minutes-weighted player-rate aggregates (PPG/RPG/APG/SPG/BPG/TOPG, TS/eFG/USG, AST%/TOV%/ORB%/DRB%/STL%/BLK%/FT_rate), roster depth (size, top-1/top-5 minute share, MPG stddev), star-player box (top by minutes), and 12 archetype-share columns. Leave-one-season-out backtest: MAE 7.43 / RMSE 9.31 / R² 0.66; 5-fold random CV: MAE 7.22 / R² 0.67. Top features by importance: STL%, SPG, total_minutes, BPG, TOV%, TOPG, TS%, BLK%, ORB%, **archetype Warlock** (!), DRB%, FT_rate — the model genuinely learns roster composition rather than recovering a tautology. **Critical design decision documented in the script**: Torvik GBPM / CamPom features are *excluded* by default because they're regression-derived per-player attributions whose minutes-weighted sum ≈ team AdjEM by construction — including them collapses MAE to 1.68 / R² 0.98, but most of the "learning" is recovering an identity, and the Δ signal degenerates into `Δ ≈ player_campom × minutes_share` (which we could already do without a model). The impact-feature variant lives behind `ROSTER_INCLUDE_IMPACT=1` for sanity-check use. **Train/serve contract**: `roster_model_meta.json` records `player_filter` (`games_played >= 5 AND minutes_per_game >= 5`) and `include_impact_features` so the upcoming Rust inference path can hard-fail if it builds features over a different qualified-player cohort than the model was trained on. Honesty framing for downstream consumers: 7.4 MAE is the absolute-level error; the **Δ from swapping one player** inherits the roster-composition signal (where the model is calibrated) and not the non-roster variance (coaching, scheme, injuries) that drives most of the absolute error. Note: `training/requirements.txt` pandas pin bumped to `>=2.2` for the `GroupBy.apply(include_groups=...)` kwarg used by the aggregator.
  - [x] **Engine v1 — Rust inference** (PR #54). `Predictor` gained a `roster_session` + `predict_adj_em`. `cstat_core::roster_features` exposes `PlayerRow`, `fetch_roster`, `build_roster_features`, `swap_player` (rank-slot by CamPom v3, preserves 200-min envelope), and `normalize_rotation` (used for symmetric baseline-vs-swap normalization so Δ reflects only the incoming player). `roster_model_meta.json` is contract-validated at boot — `validate_roster_meta` hard-fails on `player_filter` / `include_impact_features` / feature-order drift. 13 unit tests + 1 ONNX round-trip smoke test in cstat-core; Dockerfile now ships `roster_model.onnx` + `roster_model_meta.json`. The engine is warm and ready for the 2027 forecasting work below; it's currently unused on the prod surface.
  - [~] **API + UI — deferred until 2027 forecasting (deliberate pivot)**. Built end-to-end in PR #54 (route delta pipeline + `ΔAdjEM` column on `TransferPortal.tsx`) and then **reverted** after spot-checking real outputs. The fundamental problem: D-I roster retention is so low that "John Blackwell would change *2026 Duke* by Y AdjEM" is the wrong counterfactual — by the time he plays for Duke it's a *2027 Duke* roster nobody on the page has seen yet (different returning core, different other incoming transfers, different freshmen, different draft outcomes). The current-season-Δ surface ends up framing transfers against a roster they'll never actually join. Sample evidence from the abortive ship: top-end ΔAdjEM was inflated when thin destinations got rank-slot-promoted sub-replacement players (`Max Frazier CamPom −1.3 → Hampton: Δ = +7.81`), bottom-end was over-pessimistic when elite destinations rank-displaced existing rotation contributors (`Brady Dunlap CamPom 2.0 → Georgia (+22 AdjEM): Δ = −3.79`). Both pathologies disappear once the destination is the *actual projected next-season roster* rather than the *current-season roster*. Engine code stays put (`Predictor::predict_adj_em` + `roster_features::*`) — the next bullet's 2027 forecasting work is what makes this surface honest, not a tuning problem with the engine. See PR #55 for the back-out + this rationale; do not retry "current-season Δ for transfers" without solving the roster-volatility framing first.

  Two framings, sharing the engine above:
  - **Per-player Δ (current-cycle view)** — *abandoned, see deferred bullet above*. The "if added to current-season destination roster" framing is fundamentally misaligned with how college basketball rosters actually work; we won't retry this surface without first solving forward-looking roster projection.
  - **Per-team 2027 projection (forward-looking view)** — **the load-bearing surface for the engine we built**. Project ratings for the upcoming 2026-27 college season (cstat-season 2027) using the full portal data from the **DB-backed transfers pipeline above** (`SELECT … FROM transfers WHERE year = 2026`). **Engine ready**: `Predictor::predict_adj_em` + `cstat_core::roster_features::{fetch_roster, build_roster_features, swap_player, normalize_rotation}` already do the per-roster AdjEM prediction; what's missing is the *roster composition* pipeline (returning-players minus departures plus arrivals) and the *next-season stats projection* (Phase 5c's career-trajectory growth model — return each player's projected 2026-27 line rather than their actual 2025-26 line so the model sees a forward-looking roster). **Naming convention nuance**: the projection year (`2027`) uses cstat-season end-year, but the *transfers* table's `year` matches the transfer-class year (= the spring calendar year the player enters the portal). So `transfers.year = 2026` = spring-2026 portal cycle = moves into cstat-season 2027. The two naming conventions live side-by-side on purpose because they match the upstream sources (247 and srating both use class year). **Fallback source** (documented in case 247 access lapses — your sub is in cancelled-grace state per JWT `ss: "Monthly+Cancelled"`): [srating.io transfer ranking](https://srating.io/cbb/ranking?view=transfer&season=2026) at `POST https://srating.io/api` returns the full portal in one ~13 MB response, but the smoke test showed `x-kryptos-id` / `x-secret-id` alone are insufficient (got `{"error":"access denied","code":103}`) — would need a full Copy-as-cURL replay including session cookies, or Playwright. Cross-reference against On3 / Verbal Commits as needed for walk-on / mid-major gaps. Layer NBA draft early entrants from a second JSON (`data/draft/2026_early_entrants.json` — 60 entries shipped from `docs/early_paste.txt`) keyed by `(name, current_team)`, with a `status` field of `declared` / `withdrawn` / `staying` / `gone` and a `?` rendering for anything still `declared` (withdrawal deadline is ~late May / early June; everything declared today is genuinely uncertain). **Source**: NBA.com's official early-entry list ([2026 list](https://www.nba.com/news/2026-nba-draft-early-entry-candidates)) — the page is JS-heavy and timed out under WebFetch; fallback is a copy-paste into `docs/early_paste.txt` and a small TSV → JSON converter (`Player\tSchool or Team\tHeight\tStatus` columns). **Senior exclusion is automatic**: NBA's "early entry" definition excludes seniors by construction (they're automatic entries, not "early"), so the list contains only Fr / So / Jr — no need to filter. **Cross-reference rule with `data/transfers/2026.json`**: portal commitment supersedes current school. If a player appears in both the early-entrant list and the portal file with a committed `next_team`, the `?` attaches to `next_team` (e.g., John Blackwell shows on the list as Wisconsin/Jr but committed to Duke, so he's on Duke's projected roster with a `?`). If they only appear in the entrant list, the `?` attaches to their current school. If they appear in the portal file with `next_team: null` and on the entrant list, they're a homeless `?` until they pick a school. Each team's projected 2027 roster is then built from:
    1. **Returning players** ✓ shipped (v1) — cstat-season-2026 roster minus graduating seniors (`class_year = 'Sr'`), minus outbound portal, minus draft entrants flagged `gone` or unresolved `declared` (the `?` cohort).
    2. **Incoming portal** ✓ shipped (v1) — `data/transfers/2026.json` arrivals matched to their just-completed cstat-season-2026 (2025-26 college season) stats by `(name, previous_team)`.
    3. **Incoming recruits** ✓ **shipped (v2)** — class-of-`base_season` commits to each team, synthesized into a PlayerRow from a 4-tier mean freshman profile (T1 top-30 / T2 31-100 / T3 101-250 / T4 251+/unranked) calibrated against 558 paired (class-of-2024/2025 recruits × actual freshman cstat-seasons). Tier-mean is a population average, not a per-player projection — the Phase 6 freshman-impact prior is the per-player upgrade. Methodology + calibration query: `docs/projections_methodology.md`. Heuristic constants live in `crates/cstat-core/src/roster_projection.rs::{T1,T2,T3,T4}_PROFILE`.
  - **Two-scenario rendering for `?` cohort**: project each team twice — **floor** (all `?` players treated as `gone`) and **ceiling** (all `?` players treated as `staying`) — and surface both bounds plus a midpoint. Once withdrawal-deadline data lands, the `?` flag clears and the range collapses to a point estimate. Optional refinement: a single probability per `declared` player (defaults to 0.5, override per player if we want to encode draft-board signal) and a single probability-weighted projection instead of floor/ceiling.
  - **Surface**: new `Projected 2027` view on TransferPortal (or its own tab) with a sortable AdjEM table — Team / Returning CamPom / Incoming CamPom / Outgoing CamPom / Floor AdjEM / Ceiling AdjEM / Δ from 2026. Per-team page gets a "Projected 2027 roster" panel echoing the current Roster Compare panel from Predict, with `?` chips next to undecided returners.
  - [x] **UI polish on the Projected 2027 page** (shipped on top of PR #59): tier-colored AdjEM chip (`adjEmTone` 6-band scale tuned for the D-I 2025 distribution: ≥+25 elite emerald, +15–25 strong, +5–15 above-avg teal, ±5 slate, −5 to −15 amber, ≤−15 rose), `Mid AdjEM` column rendering the shrunk midpoint as the headline chip with a baseline-AdjEM tooltip, `Δ vs last` column showing the projected-minus-baseline delta (color-tiered ±1/±3 thresholds), `Floor ↔ Ceiling` band as a horizontal mini-range with explicit negative-spread surfacing (declared cohort acts as a net drag — flagged amber rather than hidden). Null-handling on sorts pins thin-roster rows to the visual bottom in both directions.
  - **Honesty framing**: predict-model MAE is 8.28 on same-season data with full game logs. A 2027 projection built from prior-season player stats + roster math has materially looser error bars — frame the output as a directional ordering ("Duke's roster looks like a top-5 floor") rather than a point estimate. The floor/ceiling range itself is part of the honesty story.
- [ ] **NBA Draft Big Board (CamPom × draft-rank value index)**: a curated ranked-prospect file (`data/draft/2026_big_board.json` — **populated from Tankathon, 116 players**) joined to cstat players + the early-entrant list, surfaced as a small Big Board page that mirrors the TransferPortal grid's "247 rank vs CamPom rank" value-delta framing — but for draft stock instead of portal value. **Schema** (flat array; one row per prospect):
  ```json
  { "rank": 1, "name": "Cameron Boozer", "current_team": "Duke",
    "position": "PF", "height": "6-9", "weight": 250,
    "class_year": "Freshman", "age": 18.9,
    "tier": "lottery",                 // lottery | 1st-round | 2nd-round | fringe | unranked
    "stats": { "pts": 24.2, "reb": 11.0, "ast": 4.4, "blk": 0.7, "stl": 1.5 },
    "source": "tankathon",
    "as_of": "2026-05-10" }
  ```
  **Data source — Tankathon** (current canonical). 247Sports doesn't publish a draft big board (only the transfer portal). Paste from Tankathon's site goes to `docs/tankathon`; `scripts/parse_tankathon.py` converts the 17-line-per-player block format → the JSON above. Layer ESPN / NBA Draft Net later for cross-source consensus if useful; the `source` field tags each row's provenance.
  **Tier mapping**: rank → `lottery` (1–14) / `1st-round` (15–30) / `2nd-round` (31–60) / `fringe` (61+) / `unranked` (NR). Matches NBA draft round structure regardless of which scout source we pull from.
  **Cross-references** (verified at populate time): `(name, current_team)` joins to `data/draft/2026_early_entrants.json` and cstat `players`. Big board is a *superset* of early entrants — Tankathon's 116 includes 32 seniors (auto-eligible, no "early entry" needed), 6 internationals, and 1 G-Leaguer who never appeared in college (these won't join to cstat data; render with `—` instead of CamPom chip). The overlap pattern is informative: 37/60 early entrants are on the board (= Tankathon thinks they're draftable); 23/60 are NOT (= implicit "you should withdraw" signal — useful prior for the `?` cohort resolution in the 2027 projection above). Name-normalization caveat: suffix handling differs across sources (early-entrants has "Christian Anderson Jr.", board has "Christian Anderson") — reuse the suffix-stripping logic already in `scripts/parse_247_transfer_html.py` when wiring joins.
  **Surface (still to build)**: `/draft` page with sortable table (Rank · Player · Team · Tier · CamPom · Δ vs draft rank · Status chip). The Δ-vs-draft-rank column is the headline: positive Δ = CamPom undervalues vs draft stock; negative = CamPom likes them more than scouts. Same "value index" story as the portal page, just applied to draft stock. Optional: a homepage card showing "CamPom's top draft sleepers" (board rank ≫ CamPom rank).
- [ ] **Roster fit scoring built on archetypes**: redundancy detection ("team has 3 Sorcerers, missing a Cleric") layered on top of the Identity/Gaps index already shipped on TeamDetail. Score each portal player at each plausible destination by how much they fill a Gap vs add to an Identity stack. Combine with the impact projection above for a final "fit-adjusted Δrating."
- [ ] **Archetype-aware team views & Team Compare** (player-filter enhancements + 12-axis roster viz + 2-team compare + enhanced Predict):
  - **Players-tab filter polish**: extend today's `?archetype=` filter (primary, or primary-OR-secondary via the toggle — see "Archetype rankings drill-down" above) with a **primary+secondary combination** mode for hybrid drill-downs (e.g., "Wizard/Bard"). UI: second class-chip slot in the page header with a mode selector (`primary only` / `primary or secondary` / `primary + secondary`).
  - **12-axis radial roster plot**: one 12-spoked radial chart (one spoke per archetype class) with every player on the roster placed as a labeled point — angular position from primary class, radial distance from minutes share (or affinity strength). Lets you eyeball role diversity vs concentration at a glance. Risk: visually noisy on deep rosters; fallback is a small-multiples grid of mini-radars (one per player) if the overlay reads as crowded. Lives on TeamDetail and the new TeamCompare page.
  - **Team Compare view** (`/teams/compare?ids=A,B`, **max 2**): mirror PlayerCompare ergonomics — picker, side-by-side header panels (record, AdjEM, CamPom leaders, archetype distribution), aligned tables for four factors and roster aggregates, the radial roster plot overlaid for both teams, and per-row advantage chips on key team stats. Capped at 2 because radial overlays + side-by-side tables stop being readable past two columns, and head-to-head outcome modeling already lives on Predict.
  - **Enhanced Predict view**: when both teams are picked, embed the Team Compare panel beneath the margin/win-prob output. Same data, two framings — Predict says "Duke −3 vs Houston," the embedded compare panel explains *why* (archetype mismatch, roster aggregate gaps). The §4b "Predict follow-up — embedded Roster Compare panel" already shipped the per-team roster cards under the headline; this item adds the radial roster plot + per-row advantage chips on top, and the Roster Compare component is the natural drop-in target.
- [ ] **Aggregate team shotchart on TeamDetail (and embed in Predict)**: court-shape heatmap or zone-rate summary aggregating the roster's shot diet (dunk / rim / mid / 2P jumper / 3P / FT splits) weighted by minutes, so a Duke vs UNC matchup can show "Duke takes 38% of shots from three vs UNC's 28%" at a glance. Per-player shot zones are already in `torvik_player_stats`; a roster-weighted aggregate is straightforward. Open design questions: (a) which visualization — court-shape heatmap or stacked-bar of zone shares? Court is more intuitive but harder to read; bars are KenPom-style. (b) how to handle defense — pair with opponent shot diet allowed on the same chart for matchup viz. Lands on TeamDetail first as its own surface, then drops into the Predict page's Roster Compare panel as a third panel pair.
- [ ] **Previous Matchup view** (Team Compare, but pinned to a specific completed game). Side-by-side team headers (record, AdjEM, CamPom leaders, archetype distribution) with stats snapshotted as of the game date, the radial roster plot overlaid for both teams, then the actual box score (player rows + team totals) and per-team **per-game** shot diets (dunks / rim / mid / 2P / 3P / FT splits) for the players who logged minutes. Click-through target from any schedule row, score ticker entry, and a "recent/upcoming matchups" panel on Predict. Reuses Team Compare's header + radial plot so the visual language is consistent. Built on existing data: `games`, `team_game_stats`, `player_game_stats` already populated; per-game shot zones are already in the Torvik gzip JSON we ingest (cols 17-28 keyed by `muid`, see `docs/torvik-api-guide.md:240-296`) — the parser handles them but we don't persist them today. Work: extend `torvik_player_stats` (or a new `torvik_player_game_stats` table) with the shot-zone columns, backfill from the existing JSON, and add a thin `/api/games/:id` endpoint plus the frontend page.
- [ ] **"What if Player X transfers to Team Y?" sandbox UI**: free-form picker — choose any current player, drop them onto any team, see the recomposed roster, archetype mix shift, and projected ΔAdjEM. Reuses the projection engine from above.
- [ ] **Player search and comparison across all teams**: extend the existing Compare page to portal-aware comparisons (current team vs hypothetical destination); add a portal-only filter to the Players tab.
- [ ] **API endpoints for all composition queries**: `GET /api/transfers/{year}/projections`, `GET /api/teams/{id}/portal-fits`, `POST /api/whatif` (player + destination → projected roster + Δrating).

---

## Phase 5c: Player Career Trajectory
> Multi-year stats view on PlayerDetail + a forecast model that turns "year N stats" into "year N+1 expected stats."

- [x] **Multi-year stats view — shipped as standalone `PlayerProgression` page**. Cross-season aggregation lives at `GET /api/players/{id}/progression` (`crates/cstat-api/src/routes/players.rs::player_progression`) and the dedicated page at `/players/:id/progression` (`web/src/pages/PlayerProgression.tsx`). Reuses `queries::get_player_available_seasons` (natstat_id ∪ torvik_pid UNION — same load-bearing 96%-coverage join the original spec called for) plus per-season `tokio::try_join!` of `get_player_by_id` / `get_player_season_stats` / `get_player_percentiles` / `get_torvik_stats` / `get_player_archetype`, so transfers join through naturally (e.g. Lendeborg's UAB → Michigan stints land as two entries). Page layout: header with archetype + trajectory chips, CamPom v3 line chart with dashed-extension projection point, full stats table (Volume / Shooting / Rates / Impact, columns oldest→newest, percentile-colored cells), and a grid of per-season cards each pairing a radar (reuses `resolveAxes`) with `ShotDietCourt` + `ShotDistributionBar`. Entry point: the PROJ chip on `PlayerDetail` is now a `SeasonLink` to `/players/:id/progression`. Two deviations from the original spec, both deliberate: (a) standalone page rather than embedded on PlayerDetail — the cross-season view is denser than a sparkline grid and would crowd the single-season detail; the PROJ chip is the discoverability hook. (b) Endpoint named `/progression` rather than `/trajectory` to avoid colliding with the existing `cstat_core::trajectory` module that powers the projection chip — the page calls *that* model too, so the URL needed disambiguation. ≥2-season gate isn't enforced server-side — the page renders for 1-season players (table + radar + shot diet card) but hides the time-series chart unless `≥1 season has Torvik CamPom data`.
- [~] **Returning-player growth model** *(in progress — task #5)*. Train a LightGBM regressor on `(season N stats) → (season N+1 CamPom v3)` pairs across every consecutive-season player in the DB. Inputs: prior-season rate stats, archetype, minutes share, class year (freshman→sophomore is the steepest growth bucket; senior→grad-transfer flattens), prior-season Torvik composite, **recruit-rank features** (see below). Output: predicted next-season CamPom v3 (and a confidence band from quantile regression). Sanity-check honest framing: ~3 seasons of paired data → ~10k returning-player rows after gating on min-games qualified in both years, enough for a baseline but the per-class-year sample is thin and the model should not be over-sold. **Acceptance**: MAE per class-year bucket better than naive "year N+1 ≈ year N" baseline; for transferring players, factor in destination-team archetype mix to avoid systematically over- or under-projecting role changes.

  **In-flight scope decision**: training pipeline + Rust inference + PlayerDetail badge + Players-page season-flip ship as a single PR. **Recruit-rank features shipped (PR 5c-iter2)** — class-of-2024/2025 ingest backfilled the `recruits` table enough that ~7% of trajectory training rows now join through `recruits.cstat_player_id`. Shared feature derivation lives in `training/recruit_features.py` ↔ `cstat-core::recruit_features` (single source of truth for the freshman-impact prior model coming next). Cross-team transferring returners are included via `torvik_pid` joins (per memory: stable cross-season key, 96% coverage); destination-team archetype mix is **not** in v1 features (transfers project against a destination-agnostic prior; documented as a known limitation).

  **Recruit-rank features — shipped** (PR 5c-iter2). 11 recruit-block features bolted onto the trajectory model via a shared extractor at `training/recruit_features.py` ↔ `crates/cstat-core/src/recruit_features.rs` (locked feature order, sentinel-encoded for the ~93% of rows with no recruit row). Feature names: `recruit_is_ranked`, `recruit_composite_rank`, `recruit_composite_rating`, `recruit_star_rating`, `recruit_position_rank`, `recruit_rank_movement`, `recruit_height_in`, `recruit_weight_lb`, `recruit_bmi_proxy`, `recruit_position_code`, `years_since_recruit`. The trajectory model's 37-feature head + 11-feature recruit tail = 48 total; `trajectory_model_meta.json::n_features = 48`, validator in `inference.rs` hard-fails on drift.

  **Coverage and lift**: 401 of 5,564 paired training rows (~7%) join through `recruits.cstat_player_id` — class-of-2024 and class-of-2025 only; class-of-2022 and earlier returners fall into the `recruit_is_ranked=0` sentinel branch (the modal cohort, LightGBM fits a separate split on it). Leave-one-pair-out pooled MAE moved 2.314 → 2.296 (−0.018); the cohort-level lift is bigger than the headline because most rows are sentinel-encoded. Per class-year bucket, Fr→So MAE moved 1.573 → 1.502 (−0.071, ≈4.5% lift) — that's the cohort with the most recruit coverage and the steepest growth, so the bucket gain is what the surface most benefits from. `recruit_composite_rating` made the top-25 importance list at the threshold (rank 25, importance 105 tied with `prior_class_year_code`); the other recruit features contribute below the top-25 cutoff but the cohort MAE delta confirms they're pulling weight.

  **Selection bias on returners is still load-bearing**. Top-ranked recruits who *return for sophomore year* are negatively selected — the Cooper Flagg / Boozer cohort leaves for the draft; the 5-stars who stay are disproportionately those whose freshman year disappointed. Mid-tier returners are average. The shipped model fits returners only, so its "rank → growth" coefficient already inherits this bias. Honest framing in the UI must acknowledge the projection is calibrated on returners-who-stayed, not on the full draft-eligible cohort.

  **Coverage upgrade is the natural follow-up**: historical recruit-class backfill (class-of-2021/2022/2023) pushes the joined-row count from ~7% toward ~25%+ and lets the recruit signal contribute outside the freshman→sophomore bucket. Once 247 historical URLs are confirmed, the ingest is the same `cstat-ingest recruits` subcommand pointed at older years; no model changes required (sentinel encoding handles both regimes).
- [~] **Surfaces**: **(a) shipped** — "Proj YYYY-YY" badge on PlayerDetail next to the current-season CamPom chip, rendering `mean (lower–upper)` with a direction arrow (↑/→/↓) vs prior year. Tier-colored dashed chip; tooltip frames the pooled MAE ~2.3 caveat. (b) [deferred] season-flip filter on the Players rankings page that swaps current-season CamPom for projected next-season CamPom — needs batch ONNX inference (3 models × ~3k qualified players ≈ 9s of naive single-row calls per request; batch tensor inference or DB-precompute is the perf fix). (c) [deferred] "biggest projected risers/fallers" homepage panel during offseason — same batch-inference perf prereq. The transfer-portal Δ engine plugs into this naturally — once we have a next-season projection per player, the Δ becomes "with this player's *projected* 2026-27 line, what does the destination roster look like" instead of "with their 2025-26 line frozen in time."
- [ ] **Train/serve contract**: same pattern as `roster_model_meta.json` — a `trajectory_model_meta.json` lists features, qualification gate, and the seasons trained on; the Rust inference path hard-fails on drift at boot.
- [ ] **Cross-year comparisons (lower priority)**: enable side-by-side comparison of *the same entity* across different seasons (e.g., "Cooper Flagg 2025 vs Cooper Flagg 2026", "Duke 2024 vs Duke 2026") and *different entities* across different seasons (e.g., "Cooper Flagg 2025 vs Jared McCain 2024"). Default behavior stays intra-season — the site-wide `?season=` selector keeps today's semantics. Cross-year mode is opt-in via a second season selector on Compare pages. Player path: `PlayerCompare` UI gains per-slot season pickers; backend resolves each `(player_id, season)` independently and the comparison shows season-aware labels (so a player who played for two different teams across seasons is unambiguous). Team path: `TeamCompare` (Phase 5b item — not yet shipped) similarly carries per-slot season. Surface honesty caveat: rule changes (3-point line, shot clock) and era effects make cross-decade comparisons noisier than cross-conference; same-era comparisons (e.g., adjacent seasons) are the load-bearing use case. Builds on top of the multi-year trajectory infrastructure above — same `torvik_pid` resolution for cross-season players, same `teams.natstat_id` resolution for cross-season teams.

---

## Phase 6: Expansion & Refinement
> Historical depth, brackets, continuous improvement

- [ ] **Full historical data support across the site** (NatStat perfs back to 2007, ~20 seasons). Today only 2025 and 2026 are ingested; expanding to the full archive unlocks career-spanning player profiles, multi-season team trends, "all-time" leaderboards, and dramatically more training data for ML. Per Phase 3 notes, this is the single highest-leverage improvement available to the predict model — current training early-stops at 49-66 iterations, data-starved on two seasons.
  - **Data availability**: `/seasons` confirms perfs for 2007-2026 (20 seasons), play-by-play from 2012+. Each season ≈ 6,200 games, ≈ 6,000 players, ≈ 110k box scores. Rate-limited at 500 API calls/hr → full backfill is a multi-day job leaning on the existing `api_cache` table.
  - **Ingest**: extend `cstat-ingest season` to accept a year range and run the full pipeline (teams → games → perfs → teamperfs → forecasts → elo) per season. Handle historical conference realignment, team renames, and defunct programs without breaking FK constraints. Layer Torvik backfill on the same range (CSV is per-year).
  - **Compute**: run the 13-step compute pipeline per historical season. CamPom, percentiles, adj efficiency, and archetypes are all already season-scoped, but worth sanity-checking early seasons where some advanced fields (e.g., shot zones from Torvik) may be missing.
  - **Schema**: confirm `(season, …)` indices are present and effective at 20× current data volume; spot-check query plans for cross-season joins. Postgres should handle the size fine — main risk is unindexed fan-out on player career queries.
  - [x] **API**: every endpoint already accepts a `season` query param with `default_season()` fallback (`crates/cstat-api/src/main.rs:24`). The `season` plumbs through `features.rs::build_game_features` so even the predict model handles arbitrary historical matchups. Career-aggregate endpoints (`/api/players/:id/career`, `/api/teams/:id/history`) and cross-season `/api/players/compare` are still future work — they need new query shapes once we have more seasons to compare across.
  - [x] **Frontend**: site-wide season selector shipped in `web/src/components/Layout.tsx`; URL is the source of truth (`?season=YYYY`) via `useSeason()` in `web/src/components/season.ts`; `<SeasonLink>` and `seasonHref()` preserve season across all in-app navigation. Predict page already supports historical back-testing via the same selector. Adding a new season is a one-line change to `AVAILABLE_SEASONS` in `season.ts` once data lands. Multi-season *career trajectories* on player/team detail pages are still future work — they need the historical seasons ingested first.
  - **ML**: retrain on all seasons (incremental: 2024 → 2023 → … to measure marginal lift per season added). Watch for distribution shift (rule changes, three-point line move in 2008, COVID-shortened 2021).
  - **Archetypes at scale**: combined-cohort training works at 2-3 seasons; degrades around 5+ when era effects (3PT volume, small-ball, rule changes) make players from different eras non-comparable. See `docs/archetypes_methodology.md` for trigger criteria and candidate strategies (sliding window, era-bucketed clustering, per-decade models).
  - **Stretch**: all-time leaderboards (best CamPom seasons ever, GOAT teams by adj efficiency margin), program-history pages, cross-era archetype distribution shifts.
- [~] **Freshman recruiting ingest + tier-mean heuristic + per-player prior model**. The /projections page now consumes a third roster source. **Ingest shipped: 1,200 recruits across class-of-2024/2025/2026 (321 + 512 + 367). Tier-mean heuristic shipped: 558 of those recruits paired to actual freshman cstat-seasons (224 from 2024 → cstat-season 2025 freshmen; 334 from 2025 → cstat-season 2026 freshmen; class-of-2026 hasn't played yet) → 4-tier population profile keyed on composite_rank. Per-player prior model deferred.**

  **✓ Shipped — recruit ingest pipeline** (migration 020 + `Recruit247Client` + ingest module + CLI):
  - `recruits` table with `(year, recruit_key)` UNIQUE on 247's stable player ID; columns for composite_rank / composite_rating / star_rating / previous_rank / position_rank / state_rank, position / height / weight, hometown city/state, high_school, committed_school + slug + `committed_team_id UUID REFERENCES teams(id)`, commit_status, profile/photo URLs, `raw_player JSONB` escape valve, `cstat_player_id UUID REFERENCES players(id)` resolved post-arrival. 5 indexes mirroring transfers.
  - `Recruit247Client` (`crates/cstat-ingest/src/tfs_recruits.rs`) mirrors `TfsClient` plumbing (`TFS_247_JWT`, shared RateLimiter, exponential backoff on 5xx/429, JwtExpired short-circuit). New piece is the `scraper`-based HTML parser `parse_recruits_html` keyed to `li.rankings-page__list-item`. Four commit-state variants handled: img-link+checkmark = "Signed", img-link alone = "Committed", bare `<img>` direct child of `.status` (schools without 247 landing pages, e.g. Cal Baptist) = "Committed" without slug, `.rankings-page__crystal-ball` = "Uncommitted".
  - `cstat-ingest recruits` CLI subcommand: `--year --groups --bootstrap-from --dump-snapshot --no-resolve-teams --no-resolve-players`. Year semantics in `--help`: "recruiting class year = spring of HS graduation; class-of-2026 first plays in cstat-season 2027" (same offset-by-one as transfers).
  - Two-pass cstat join: Pass 1 (`resolve_team_joins`) matches `committed_school` text → `teams.id` via `cstat_core::team_name_match::team_match_score`. Pass 2 (`resolve_player_joins`) matches `(full_name, committed_team_natstat_id, season=year+1)` → `players.id`; cheap idempotent no-op until cstat-season `year+1` box scores ingest.
  - 13 unit tests + 8 integration tests against a real 128KB captured page-2 fixture.
  - **Live class-of-2026 ingest**: 367 HS recruits, 100% commit_status taxonomy coverage (193 Signed / 112 Committed / 62 Uncommitted), 305/305 (100%) `committed_team_id` resolution after UMKC + Penn team aliases were added to `cstat_core::team_name_match::TEAM_ALIASES`.
  - **Historical ingest** (class-of-2024, class-of-2025): 321 + 512 = 833 recruits. 303/307 + 465/472 team resolution. 224 + 334 = 558 `cstat_player_id` resolutions against the already-ingested 2024-26 freshmen — the empirical (recruit-rank, actual-freshman-stats) corpus that powers the tier-mean heuristic. Auth: 247 changed away from a bare `JWT={value}` cookie at some point; client now accepts a full session-cookie string via `TFS_247_COOKIE` (DevTools Copy-as-cURL), falling back to legacy `TFS_247_JWT` for backward compat. See `crates/cstat-ingest/src/tfs_recruits.rs::Recruit247Client`.

  **✓ Shipped — tier-mean freshman heuristic** (Phase 5b §5b plug-in, see `docs/projections_methodology.md`):
  - 4-tier bucketing on `composite_rank`: T1 (1–30) / T2 (31–100) / T3 (101–250) / T4 (251+ or unranked). Sample sizes per tier: 52 / 114 / 201 / 185.
  - Per-tier mean profile (MPG, GP, PPG, rate stats, CamPom v3) lives as `T{1..4}_PROFILE` constants in `crates/cstat-core/src/roster_projection.rs`. Refresh by re-running the calibration query when a new class lands.
  - `synthesize_freshman_row(recruit_id, tier) -> PlayerRow` plugs each commit into `build_roster_features` like any other player.
  - Headline CamPom v3 monotonicity: T1 +8.97 / T2 +2.41 / T3 +0.70 / T4 −0.57. T3 vs T4 are nearly indistinguishable — composite rank stops being a strong signal past ~100.
  - **Honesty caveat**: tier-mean is a population average, not a per-player projection. A 5★ who busts and a 5★ All-American both project as +8.97 CamPom. The per-player upgrade is the prior model below.

  **Schema deltas from the original plan** (already reflected in `migrations/020_recruits.sql`):
  - Cross-season join uses `committed_team_id UUID REFERENCES teams(id)` and re-resolves via `natstat_id` at query time (mirrors transfers). The originally-planned separate `committed_team_natstat_id` column was unnecessary.
  - Added `previous_rank` for movement tracking (247 exposes prior-period rank via `.rank-column .other`).
  - Added `committed_school_slug` extracted from college URL; dropped `last_update_date` (not exposed in the HTML).
  - `commit_status` left without CHECK constraint until vocab was confirmed; now confirmed as `Signed` / `Committed` / `Uncommitted` after the live ingest — tightening CHECK is a one-line follow-up if desired.

  **Empirical finding worth flagging**: 247's `compositerecruitrankings` endpoint returns identical content for all `InstitutionGroup` values (`highschool` / `juco` / `prep`) when called with only the subscriber `JWT` cookie. CLI defaults to `--groups highschool`; the `Juco` / `Prep` enum values are kept so the schema vocab is ready for future endpoints once we find the right URLs for those cohorts. See `crates/cstat-ingest/src/tfs_recruits.rs::InstitutionGroup` for the parser-level note.

  **Remaining work** (separate PRs, in rough priority order):
  - [~] **Freshman-impact prior model** — the year-0 → year-1 per-player projection (upgrade from the tier-mean heuristic above). **Training pipeline shipped** (`training/train_freshman_model.py`). LightGBM mean + q10/q90 on 963 qualified freshman rows (≥5 GP / ≥5 MPG, class-of-2024 → 2025 + class-of-2025 → 2026). 13 features: 11 from the shared recruit-feature extractor + 2 freshman-specific (`committed_team_prior_adjem`, `peer_class_strength`). School-context features skip the dog-fooding trap by reading committed-team AdjEM from the season BEFORE the recruit arrives. **Result vs. tier-mean baseline**: pooled MAE 2.561 → **2.488** (−2.9%); T1 (top-30 ranked, n=110) 4.318 → **3.840** (−11.1%); T4 (unranked, n=290) 2.217 → **2.036** (−8.2%); T2/T3 nearly flat (±0.05 MAE). R² 0.306 → 0.367. The school-context features rank #1 and #2 by importance — committed-team prior AdjEM and peer-class strength are the two biggest gains beyond the recruit-direct block. Artifacts: `training/models/freshman_{mean,q10,q90}_model.onnx` + `freshman_model_meta.json`. **Selection-bias caveat is even sharper here than for the trajectory model**: elite freshmen leave for the draft, so the model is calibrated on returners-who-played-meaningful-minutes, not the full draft-eligible cohort. Future-Boozer top-30 projections inherit looser bands than the headline MAE suggests; the q10/q90 band is the honest framing.
  - [ ] **Rust inference for freshman model** (next iteration). Mirror the trajectory pattern: `crates/cstat-core/src/freshman_model.rs` with `FRESHMAN_NUM_FEATURES = 13`, `fetch_freshman_features(pool, recruit_id) -> FreshmanFeatureRow`, `build_freshman_features(row) -> [f32; 13]`. `Predictor` gains a 4th model-bundle (mean/q10/q90), `validate_freshman_meta` mirrors the trajectory validator. Ship via Dockerfile copy of the 3 ONNX files. Boot-time drift check enforces the contract.
  - [ ] **Per-recruit CamPom surface on the Recruits tab** (depends on Rust inference). Extend `GET /api/recruits/{year}` with `projected_campom_mean / lower / upper`. Frontend renders a CamPom chip per recruit row alongside the 247 rank — same value-index narrative as TransferPortal.
  - [ ] **Projection-page swap: replace tier-mean in `synthesize_freshman_row`** (separate iteration, requires either multi-output regression or a CamPom→per-stat scaling heuristic). The model predicts CamPom only; `synthesize_freshman_row` needs per-game stats (ppg/rpg/ts%/usg/etc.) to feed `build_roster_features`. Cheap path: linearly scale the tier-mean profile so its derived CamPom matches the predicted value. Principled path: train multi-output LightGBMs per stat. Cheap path first; principled path if/when the scaled output diverges noticeably from observed freshman patterns.
  - **API route** ✓ shipped (`GET /api/recruits/{year}`, PR #57).
  - **Plug into Projected next-season (5b)** ✓ shipped (tier-mean heuristic above; prior model is the next-level upgrade).
  - **Optional standalone `/recruits` page** ✓ shipped (PR #57 — Players-tab Recruits sub-page).

  **Dependency sequencing now**:
  - Phase 5c growth model can take recruit-rank as a feature *immediately* — the `recruits` table is populated and joins via `cstat_player_id` (resolves automatically once cstat-season 2027 box scores ingest).
  - The freshman-impact *prior model* is no longer soft-blocked — 558 paired classes is enough for a v1 fit. Wider bands than ideal until more classes accumulate, but the heuristic-vs-prior comparison is now an A/B we can actually run.
  - 5c is the natural neighbor for "year 1 → year 2 onward"; the prior model covers "year 0 → year 1". Together they project any player's next-season line regardless of where they are in their career arc.
- [ ] Backtest models across multiple seasons
- [ ] Tournament bracket simulator (Monte Carlo, inspired by gravity project)
- [ ] Season simulation engine
- [ ] Model accuracy dashboard with calibration tracking
- [ ] Automated daily data refresh during season
- [ ] Conference/team/player trend analysis over time
- [ ] **Native cstat player impact metric** (alternative to Torvik GBPM passthrough). Frame as a *descriptive* grade — "what's this player's value, derived purely from cstat's own machinery" — not a predictor (the §4f experiment already showed CamPom-style adjustments don't beat raw GBPM as ML features; this is a different goal). Approach: non-linear regression of team-game outcomes on roster-composition features (player IDs × minutes) to attribute per-player coefficients. Acceptance: matches or beats CamPom on (a) year-over-year rank stability for returning players and (b) external benchmarks (KenPom POY, AP All-American). **Caveat — re-attempting failed work**: cstat's prior native BPM (`compute.rs` pre-PR #25) tried this with linear box-score formulas and got r=0.075 with Torvik OBPM. Don't repeat that approach; the limit at our data resolution is team-game level (no play-by-play) and box-score-derived linear coefficients are exactly what blew up. A LightGBM-on-team-game-outcomes attribution is a different methodology and worth trying — but it's a multi-PR project and would need its own design doc.

---

## Honest Caveats & Open Questions

Things we've shipped (or scoped) that we're squinting at. Each entry is a one-line summary plus a pointer to the inline detail. The goal: a single discoverable place for "what's questionable about cstat right now" without burying the rationale away from the work it modifies.

Pointers below anchor on stable section headers / quoted phrases (not line numbers) so future edits to the doc don't break the cross-references. Grep for the quoted text to find the inline detail.

### Predict / ML

- **Totals model is materially worse than KenPom / Vegas.** Backtest MAE 13.58 vs ~9 (KenPom) / ~7–8 (Vegas). Projected scores are framed as KenPom-style approximations, not betting-grade. Next-level lever is full historical data, not more feature engineering. → see "Honest precision framing" in §4b.
- **TreeSHAP magnitudes weight the Keys panel, but data-direction names the leader.** The intuitive expectation was TreeSHAP would retire the hand-coded `homeAdvantageSign` lookup entirely. Purdue/Michigan case (Michigan has the better opp eFG% but SHAP attributes it toward Purdue via a non-monotonic interaction) showed the lookup serves a separate product purpose. Compromise: SHAP picks importance, data-direction picks the leader. Open question: is this the right split, or should we ship a separate "model narrative" view? → see "Predict follow-up — TreeSHAP in pure Rust" in §4b.
- **TreeSHAP infrastructure is shipped but has no current consumer.** ~700 LOC + 460 KB image + per-request CPU computed and serialised into every `/api/predict` response that nothing reads. Documented as removable; held as a "what if we want a calibration / debug surface later" hedge. → see "Deprecate TreeSHAP infrastructure" in Refactor Backlog.
- **OOF predictions are 5-fold random split, not walk-forward.** `models/oof_predictions.csv` (12,821 games) is the seed for the historical-prediction backfill, but a model trained on a random 80% can see late-season games for early-season predictions of the same season. Fine for calibration plots and early-season UI; weaker honesty than walk-forward for late-season games. → see "Predict follow-up — point-in-time historical predictions" in §4b.
- **CamPom doesn't beat raw GBPM as an ML feature.** The §4f experiment regressed margin MAE by +0.16 / AUC −0.009 across both variants. Production model stays on raw GBPM. CamPom remains the canonical *descriptive* player grade, not a predictive feature. → see "Wire CamPom into the predict model as features (negative result …)" in §4f.

### Transfer Δ & Roster Projection

- **Current-season transfer Δ surface is abandoned.** Built end-to-end in PR #54 and reverted in PR #55. Counterfactual mismatch: by the time a transfer plays for their destination, it's a *next-season* roster nobody on the page has seen yet. Sample evidence in the deferred bullet. Engine (`Predictor::predict_adj_em` + `roster_features::*`) stays warm for the 2027 projection surface. Do not retry this surface without first solving forward-looking roster projection. → see "API + UI — deferred until 2027 forecasting (deliberate pivot)" in §5b.
- **2027 projection MAE inherits 8.28 from the same-season base model.** Forward-looking projections built from prior-season player stats + roster math have materially looser error bars than that headline number. Framed as directional ordering ("Duke looks like a top-5 floor"), not point estimates. Floor/ceiling range itself is part of the honesty story. → see "Honesty framing" under the Projected 2027 bullet in §5b.
- **`?` cohort for declared NBA draft entrants.** Withdrawal deadline is late May / early June; everything declared today is genuinely uncertain. We render floor (all `?` players gone) and ceiling (all `?` staying) as a range, not a point estimate. Defaults to 0.5 probability; per-player overrides are out of scope until we see draft-board signal. → see "Two-scenario rendering for `?` cohort" in §5b.

### Career Trajectory / Recruits

- **Returning-player growth model has selection bias on returners.** Top-ranked recruits who *return for sophomore year* are negatively selected — the elite leave for the draft; the 5-stars who stay are disproportionately those whose freshman year disappointed. A naive "rank → growth" model trained on returners may find the opposite of conventional wisdom. Diagnostic acceptance: report MAE on full cohort vs. "stayed past year 1" cohort separately. → see "Selection bias on returners" in §5c.
- **Recruit-rank signal contributes at the margins, not the headline.** Trajectory model gained recruit-block features (PR 5c-iter2) but only ~7% of training rows have a recruit row, so pooled MAE moved a modest 2.314 → 2.296. Fr→So bucket moved 1.573 → 1.502 (~4.5% lift) — the bucket with the most recruit coverage gets most of the gain. Historical recruit-class backfill is the unlock that lifts the global number. → see "Recruit-rank features — shipped" in §5c.
- **Freshman-impact prior model: training pipeline ships at 2.9% pooled lift, 11.1% on T1.** Wins big on the top-30 cohort (the headline bucket) and T4 unranked; T2/T3 essentially flat. Limited by the 963-row training corpus from 2 paired classes — historical recruit backfill (class-of-2021/2022/2023) is the obvious next unlock. Selection bias on top recruits who returned for sophomore year is sharper here than for trajectory: elite freshmen leave, so the model is calibrated on a thinner cohort than headline MAE implies. → see "Freshman-impact prior model" in §6.

### Compute Pipeline

- **TOV% has a −2pp methodology gap vs Torvik.** Formula matches Bball Ref; remaining gap is likely Torvik using minutes-weighted team possessions in the denominator. Deferred, low priority. → see "Deferred (low priority)" in the Compute Pipeline Audit section.
- **Mixed scale convention in DB.** `player_season_stats` stores rate stats as fractions (0–1); `torvik_player_stats` stores percents (0–100). Anything that joins or compares the two needs to normalize. Worth a follow-up to standardize. → see "Mixed scale convention" in the Compute Pipeline Audit section.
- **`compute_rolling_averages` partial-window early-season rows feed downstream features unflagged.** Strict point-in-time `ROWS BETWEEN 5 PRECEDING AND 1 PRECEDING` window with no leakage — but the first 4 games of any team have shrinking windows that aren't flagged as such. → see "Rolling averages use a strict point-in-time" note in the Compute Pipeline Audit section.

### Infrastructure / Limits

- **NatStat rate limit hardcoded at 1500.** `NatStatClient::new(..., 1500)` doesn't match the 500/hr standard tier. Should read `NATSTAT_MAX_PER_HOUR` env var with 500 as the default. Testers on different tiers shouldn't have to recompile. → see "Rate limiter unification" in Refactor Backlog.
- **247Sports `compositerecruitrankings` returns identical content for all `InstitutionGroup` values.** v1 ingest is `highschool`-only; the `juco` / `prep` enum vocab is kept for when we find the separate endpoints. → see migration 020 comment and `tfs_recruits.rs::InstitutionGroup`.
- **247Sports JWT is in cancelled-grace state** (`ss: "Monthly+Cancelled"`). Captured snapshot path documented as fallback; `srating.io` smoke-tested but needs full session-cookie replay to actually work. → see "Fallback source" under the Projected 2027 bullet in §5b.
- **Native cstat impact metric — re-attempting failed work.** Prior native BPM (pre-PR #25) tried linear box-score formulas and got r=0.075 with Torvik OBPM. Don't repeat that approach. A LightGBM-on-team-game-outcomes attribution is a different methodology worth trying, but a multi-PR project that needs its own design doc. → see "Native cstat player impact metric" in §6.

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

## Refactor Backlog

Captured during the 2026-05-03 ingestion-pipeline checkup. These are
deferred-but-considered items — not blocking, but worth picking up the next
time someone is in the relevant area.

### compute.rs modularization
`crates/cstat-core/src/compute.rs` is ~1,800 lines containing every derivation
step (game backfill, season stats, four factors, AdjO/AdjD, percentiles,
rolling, individual ratings, CamPom, derived game fields, schedules). The
file is cohesive but fat. Split into `compute/` submodules — one file per
step, mirroring `ComputeReport` — once a step needs a meaningfully different
shape (e.g., parameterized weights, per-step CLI invocation, parallel
execution). Until then the single-file form keeps cross-step sharing of
helpers cheap.

### Archetype training automation
Adding a season today requires a manual `python -m training.archetypes
--seasons …` after the Rust ingest. Wiring this into `cstat-ingest season`
would close the loop (one command → site fully populated). Blockers: the
training pipeline is Python and shells out from Rust would be brittle; the
combined-cohort retraining policy in `docs/archetypes_methodology.md` is
load-bearing and shouldn't be silently re-fit. Reasonable shape: an
`--archetypes` flag that runs a subprocess and surfaces the diagnostics, or
a small Python entrypoint the bootstrap script calls.

### Per-team `Team` ingest doesn't run compute
`SeasonIngester::ingest_team(code)` does NatStat ingest only — the resulting
roster row won't have rate stats / percentiles until a season-wide compute
pass runs. Fine for power users, surprising for first-time use. Consider
adding an `--also-compute` flag; skipped for now because per-team compute
isn't supported (compute_all is season-scoped).

### Rate limiter unification
`NatStatClient::new(..., 1500)` hardcodes the rate budget at the bin
construction site. The budget is account-tier-dependent. Read it from
`NATSTAT_MAX_PER_HOUR` (with 500 as the default standard-tier budget) so
testers on different tiers don't have to recompile. Same change makes the
README's "500 calls/hour" line accurate by default.

### Sequenced ingest concurrency
Every `for team_code in teams` loop in the ingest crate is strictly
sequential. The rate limiter is the real bottleneck, not concurrency, but
the per-team `teamperfs` path (now season-wide as of 2026-05-03) was the
last big offender. If we ever add a per-team enrichment step, fan it out
behind the rate limiter (e.g. `futures::stream::buffered`) rather than
inlining another `for` loop.

### Deprecate TreeSHAP infrastructure (no current consumer)
The TreeSHAP plumbing shipped in PR #47 to drive the Keys to the Game
panel's per-feature attribution. PR #49 reframed Keys around four-factor
gaps from `team_season_stats` and removed the SHAP-driven version, then
removed the panel entirely after deciding it didn't earn its real estate
(four factors duplicate Team Stats, star/talent gaps duplicate Roster
Compare). **Result: no frontend surface currently consumes
`feature_contributions` or `contributions_by_group`** — they're computed
on every `/api/predict` call and serialised into the response, and
nothing reads them.

Currently shipped but dead:
- `crates/cstat-core/src/treeshap.rs` (~500 lines, pure-Rust
  Lundberg/Erion/Lee TreeSHAP, parses LightGBM v4 text dump). Includes
  the `treeshap_matches_lightgbm_baseline` parity gate against
  `pred_contrib` (max abs diff 7.11e-15).
- `Predictor::predict_with_contributions` in `inference.rs` — runs both
  ONNX margin + TreeSHAP per call.
- `FEATURE_META` table in `inference.rs` (per-feature labels + groups,
  only consumed by `build_contribution_payload`).
- `build_contribution_payload` + the `feature_contributions` /
  `contributions_by_group` keys in `routes/predict.rs`.
- `margin_model.lgb` (~460 KB) shipped in `training/models/` and copied
  into the Docker image.
- `web/src/components/featureExplanations.ts` (`FLAG_FEATURES`,
  `homeAdvantageSign`) — already orphaned on the frontend.
- `FeatureContribution` / `GroupContribution` types in
  `web/src/api/client.ts`.

Cost of keeping it as-is: per-request CPU for the TreeSHAP eval, ~5 KB
of unused response payload per prediction, 460 KB in the Docker image,
and ongoing maintenance load (the LightGBM text-dump parser ties the
Rust crate to a specific dump format — every retrain has to re-emit it).

Cost of removing: tearing out working code that the API contract still
exposes. Future use cases that would justify keeping it: a calibration
dashboard (§6 model accuracy item), a per-prediction debug tooltip, or
a separate "why this prediction" page.

**Gate before removal**: confirm there's no future feature that wants
per-feature SHAP attribution at request time. If we want SHAP for
*offline* analysis (calibration plots, model audits), that can run
straight from `oof_predictions.csv` in Python — doesn't require keeping
the runtime Rust path.

**If/when removed**:
1. Drop `treeshap.rs`, `predict_with_contributions`, the parity test.
2. Strip `feature_contributions` / `contributions_by_group` from the
   `/api/predict` response (breaking API change — bump the response
   shape or just delete the keys; no consumer reads them).
3. Drop `FEATURE_META`, `build_contribution_payload`,
   `featureExplanations.ts`, the unused frontend types.
4. Stop emitting `margin_model.lgb` from `train.py` / copying it in
   `Dockerfile`.

Estimated savings: ~700 lines of code, ~460 KB image size, small
per-request CPU. Defer until clearly no consumer is on the horizon.

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
