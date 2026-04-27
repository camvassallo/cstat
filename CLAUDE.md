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
```

## Environment Variables

Copy `.env.example` to `.env`. Required:
- `DATABASE_URL` — Postgres connection string
- `NATSTAT_API_KEY` — NatStat API v4 key (format: `xxxx-xxxxxx`)

Optional: `BIND_ADDR` (default `0.0.0.0:8080`), `RUST_LOG` (tracing filter)

## Architecture

Three-crate Rust workspace:

- **cstat-core** — Shared types, DB models (`models/`), query layer (`db.rs`), and compute pipeline (`compute.rs`). The `Database` struct wraps `PgPool` and handles migrations via SQLx.
- **cstat-ingest** — NatStat API client (`client.rs`), response cache (`cache.rs`), token-bucket rate limiter (`rate_limiter.rs`), and ingestion pipeline (`ingest/`). CLI binary at `src/bin/ingest.rs` with subcommands: `season`, `teams`, `players`, `games`, `perfs`, `update`, `compute`, `status`, `clean-cache`, `torvik`, `explore`.
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

## Database

Postgres with SQLx. Migrations in `/migrations/` (12 files). Key tables: `teams`, `players`, `games`, `player_game_stats` (110+ columns), `player_season_stats`, `team_season_stats`, `team_game_stats`, `player_percentiles`, `game_forecasts`, `torvik_player_stats`, `api_cache`.

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

Docs in `docs/natstat-api-v4.md`. Rate limit: 500 calls/hour (standard). URL pattern: `https://api4.natst.at/{apikey}/{endpoint}/{service}/{range}/{offset}`. Responses cached in `api_cache` table with TTL.
