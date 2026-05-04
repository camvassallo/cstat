# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build
cargo build --workspace
cargo check --workspace --all-targets

# Lint (CI enforces -D warnings via RUSTFLAGS)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Test (requires running Postgres with DATABASE_URL set)
cargo test --workspace
cargo test -p cstat-core           # single crate
cargo test test_name               # single test

# Run services
cargo run -p cstat-api                          # API server (default :8080)
cargo run --bin cstat-ingest -- <subcommand>    # Ingestion CLI

# Local Postgres
docker compose up -d               # Postgres 17 on :5432

# Archetype training (Python)
cd training && python -m archetypes --seasons 2025,2026 [--diagnostics]

# Push local data to prod (no schema migrations needed if migrations/ is unchanged)
./scripts/sync_to_prod.sh [--dry-run]
```

## Environment Variables

Copy `.env.example` to `.env`. Required:
- `DATABASE_URL` — Postgres connection string
- `NATSTAT_API_KEY` — NatStat API v4 key (format: `xxxx-xxxxxx`)

Optional: `BIND_ADDR` (default `0.0.0.0:8080`), `RUST_LOG` (tracing filter)

## Architecture

Three-crate Rust workspace:

- **cstat-core** — Shared types, DB models (`models/`), query layer (`db.rs`), and compute pipeline (`compute.rs`). The `Database` struct wraps `PgPool` and handles migrations via SQLx.
- **cstat-ingest** — NatStat API client (`client.rs`), response cache (`cache.rs`), token-bucket rate limiter (`rate_limiter.rs`), and ingestion pipeline (`ingest/`). CLI binary at `src/bin/ingest.rs` with subcommands: `season`, `teams`, `players`, `team`, `games`, `perfs`, `update`, `elo`, `forecasts`, `compute`, `status`, `clean-cache`, `torvik`, `campom-parity`, `explore`. **`season` is the bootstrap command** — it runs the seven NatStat steps, then Torvik, then `compute_all`, in one call. `update` likewise runs compute at the end by default. Both accept `--no-torvik` / `--no-compute` opt-outs. **`--year` defaults to `current_natstat_season()`** (date-derived in `crates/cstat-ingest/src/lib.rs`), so the binary stays correct as the calendar rolls. Single team-id resolver lives at `cstat_ingest::team_id_by_code_and_season`; don't inline the `(natstat_id, season)` lookup. The `Team` subcommand delegates to `SeasonIngester::ingest_team(code)` — keep new per-team orchestration there, not in the bin.
- **cstat-api** — Axum HTTP server. `AppState` holds `Database` + `NatStatClient` + `Predictor`. Routes under `/api/`.

Data flow: **NatStat API → cstat-ingest → Postgres → cstat-core (compute) → cstat-api → frontend/ML**

## Compute Pipeline

`cstat-core/src/compute.rs` contains all derived metric calculations (~1,500 lines):
- `backfill_game_stats` — defensive rebounds, assist-to-turnover ratio, game score
- `compute_player_season_stats` — aggregates game stats into per-season averages, including rate stats (AST%, TOV%, ORB%, DRB%, STL%, BLK%, FT Rate) using possession-based Basketball Reference formulas
- `compute_team_season_stats` — four factors, raw efficiency
- `compute_adjusted_efficiency` — KenPom-style iterative regression for ADJO/ADJD
- `compute_player_percentiles` — PERCENT_RANK across all players (including rate stat percentiles)
- `compute_rolling_averages` — last-5-game rolling stats
- `compute_individual_ratings` — populates `pss.offensive_rating` / `defensive_rating` / `net_rating` from `torvik_player_stats.o_rtg` / `d_rtg` (passthrough; cstat's prior heuristic was broken — see ROADMAP "Compute Pipeline Audit")
- `compute_campom` — usage/minutes/sample/SOS-adjusted GBPM composites (`cam_gbpm`, `cam_gbpm_v2`, `cam_gbpm_v3` and o/d splits at every tier). Tunable constants live at the top of `compute.rs` as `CAMPOM_*` consts; methodology in `docs/campom_methodology.md`.
- `compute_derived_game_fields` — derives `is_conference`, `point_diff`, and **`wins`/`losses`** on `team_season_stats` from the authoritative `team_game_stats` rows. W-L is unconditionally overwritten so it stays self-consistent with AdjEM and four factors (the team-detail NatStat ingest also writes W-L but lags game ingest; compute always has the last word).

## Player Archetypes

12 D&D-class archetypes assigned via combined-cohort k-means clustering. Pipeline lives in `training/archetypes.py`; methodology and retraining playbook in `docs/archetypes_methodology.md`. Run with `python -m training.archetypes --seasons 2025,2026 [--diagnostics]`. Default `--seasons` covers all currently-ingested seasons — clustering runs on the union and writes per-season rows to `player_archetypes` with shared centroids in `archetype_models`. Combined-cohort training is load-bearing for cross-season class stability (45.7% returning-player primary stability vs 28% for per-season fits) — read the doc before changing it.

## Database

Postgres with SQLx. Migrations in `/migrations/` (18 files). Key tables: `teams`, `players`, `games`, `player_game_stats` (110+ columns), `player_season_stats`, `team_season_stats`, `team_game_stats`, `player_percentiles`, `game_forecasts`, `torvik_player_stats`, `player_archetypes`, `archetype_models`, `api_cache`.

All season-scoped tables carry a `season` column; the API and frontend support arbitrary multi-year browsing via a site-wide `?season=` query param plumbed through `web/src/components/season.ts::useSeason()`. **The frontend reads the season list from `GET /api/seasons`** (DISTINCT season FROM games, newest first) — no source edit needed for the dropdown when adding a year. Adding a new season is two commands: `cargo run --bin cstat-ingest -- season --year YYYY` (NatStat + Torvik + compute, end-to-end) and a `python -m training.archetypes --seasons …` retraining pass on the new combined cohort. Optional: drop a transfer-portal JSON at `data/transfers/YYYY.json` and add the entry to `EMBEDDED_TRANSFERS` in `crates/cstat-api/src/routes/transfers.rs` (the `data/` dir override exists for local dev; embed for prod).

**UUIDs are season-scoped on `teams` and `players`** — Duke 2025 and Duke 2026 are different rows with different `id`s, joined by the cross-season `natstat_id` (UNIQUE on `(natstat_id, season)`). The detail-page API endpoints (`GET /api/teams/:id`, `GET /api/players/:id`) re-resolve via `natstat_id` when the requested season doesn't match the URL's UUID, so a cross-season URL like `/teams/<2026-uuid>?season=2025` returns Duke 2025 and the frontend redirects to the canonical UUID. See `queries::resolve_{team,player}_id_for_season`.

**Roster ingest caveat**: NatStat's `/players/mbb/{TEAMCODE}` endpoint has no historical-season filter — it always returns the *current* roster. The box-score path (`games.rs`) is the sole authority for `players.team_id` per season; `players.rs::upsert_player` deliberately never overwrites `team_id` on conflict. Running `cstat-ingest players --year YYYY` against a non-current season warns once and only enriches metadata fields (height, weight, position, etc.). Box-score ingest auto-creates player rows with the correct team, so historical seasons are safe to add via `cstat-ingest season --year YYYY` alone.

## ML Inference

ONNX models are loaded at API startup via the `ort` crate (ONNX Runtime):
- `Predictor` in `cstat-core/src/inference.rs` — loads `margin_model.onnx` + `win_model.onnx`, runs inference
- `features.rs` — builds 49-feature diff vector from DB (team stats, roster aggregates, rolling form)
- `GET /api/predict?home=Duke+Blue+Devils&away=North+Carolina+Tar+Heels&neutral=false` — returns predicted margin and win probability
- Models live in `training/models/`; set `MODEL_DIR` env var to override path

## ML Training

Python pipeline in `/training/`:
- LightGBM models for margin prediction (regression) and win probability (classification)
- 49 point-in-time diff-features from team/roster/form/context (`features.py`)
- Exports to ONNX format in `training/models/` (target_opset=15); `export_onnx.py` removes ZipMap for ort compatibility

## NatStat API

Docs in `docs/natstat-api-v4.md`. Rate limit: 500 calls/hour standard tier (the binary currently passes 1500 to `NatStatClient::new` — adjust to your actual tier; see "Rate limiter unification" in the ROADMAP refactor backlog). URL pattern: `https://api4.natst.at/{apikey}/{endpoint}/{service}/{range}/{offset}`. Responses cached in `api_cache` table with TTL. **All season-wide endpoints use the same pagination shape** — `playerperfs/{season}` and `teamperfs/{season}` both fetch every team's data in one paginated call rather than per-team loops.
