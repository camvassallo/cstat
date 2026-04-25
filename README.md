# cstat

College basketball analytics platform. Ingests data from the NatStat API and Barttorvik, computes advanced metrics (KenPom-style adjusted efficiency, player percentiles, rolling averages), and serves them through a REST API and React frontend. Includes ML-based game predictions using LightGBM models exported to ONNX.

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Docker](https://docs.docker.com/get-docker/) (for Postgres)
- [Node.js](https://nodejs.org/) >= 18 (for the frontend)
- A [NatStat](https://natst.at/) API v4 key

### Setup

1. Clone the repo and copy the environment file:

```bash
cp .env.example .env
# Edit .env with your DATABASE_URL and NATSTAT_API_KEY
```

2. Install frontend dependencies:

```bash
cd web && npm install && cd ..
```

3. Start everything:

```bash
./start.sh start
```

This starts Postgres, the API server, and the Vite dev server. On first run, Cargo will compile the Rust workspace (this takes a few minutes).

| Service  | URL                    |
|----------|------------------------|
| Frontend | http://localhost:5173  |
| API      | http://localhost:8080  |
| Postgres | localhost:5432         |

### Managing Services

```bash
./start.sh start        # Start all services
./start.sh stop         # Stop all services
./start.sh status       # Check what's running
./start.sh logs         # Tail API + web logs
./start.sh logs api     # Tail API logs only
./start.sh logs web     # Tail web logs only
./start.sh logs postgres # Tail Postgres container logs
```

The start script automatically cleans up stale processes on conflicting ports.

### Ingesting Data

Before the app has anything to display, you need to ingest data from NatStat:

```bash
# Full season ingest (teams, players, games, box scores)
cargo run --bin cstat-ingest -- season --year 2026

# Run the compute pipeline to derive advanced stats
cargo run --bin cstat-ingest -- compute --year 2026
```

Other ingest subcommands:

| Command | Description |
|---------|-------------|
| `season --year YYYY` | Full season ingest |
| `teams --year YYYY` | Teams only |
| `players --year YYYY` | Players only |
| `team CODE --year YYYY` | Single team (roster, details, box scores) |
| `games --year YYYY [--from DATE --to DATE]` | Games for a season or date range |
| `perfs --year YYYY [--from DATE --to DATE]` | Box scores for a season or date range |
| `update --year YYYY --from DATE --to DATE` | Incremental update for a date range |
| `compute --year YYYY` | Derive season stats, percentiles, rolling averages |
| `status` | Show NatStat API rate limit status |
| `clean-cache` | Remove expired API cache entries |
| `torvik --year YYYY [--rebounds]` | Ingest Barttorvik player stats + optional rebound backfill |
| `explore ENDPOINT [--range PARAMS]` | Dump raw API JSON for exploration |

## Architecture

Three-crate Rust workspace:

```
crates/
  cstat-core/     Shared types, DB models, query layer, compute pipeline
  cstat-ingest/   NatStat + Barttorvik clients, caching, rate limiting, ingestion CLI
  cstat-api/      Axum HTTP server, REST routes, ONNX inference
web/              React + Vite + Tailwind frontend
training/         Python ML pipeline (LightGBM, ONNX export)
migrations/       SQLx Postgres migrations
```

**Data flow:** NatStat API + Barttorvik → cstat-ingest → Postgres → cstat-core (compute) → cstat-api → frontend

### API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /api/teams/rankings` | Team rankings with advanced stats |
| `GET /api/teams/{id}` | Team profile, schedule, and roster |
| `GET /api/players` | Player index with search, sort, pagination |
| `GET /api/players/{id}` | Player profile, season stats, percentiles, game log |
| `GET /api/games` | Game results with filtering |
| `GET /api/predict` | ML game predictions (margin + win probability) |
| `GET /api/health` | Health check |
| `GET /api/status` | API status |

### Compute Pipeline

The compute pipeline in `cstat-core` derives all advanced metrics from raw box score data:

- **Game stats** — defensive rebounds, assist-to-turnover ratio, game score
- **Player season stats** — per-game averages across all stat categories
- **Team season stats** — four factors, raw efficiency
- **Adjusted efficiency** — KenPom-style iterative regression (ADJO/ADJD)
- **Player percentiles** — PERCENT_RANK across all players
- **Rolling averages** — last-5-game rolling stats
- **Player rates** — AST%, ORB%, DRB%, STL%, BLK%, FT Rate (possession-based formulas)

### ML Predictions

LightGBM models trained on 47 point-in-time diff-features (team stats, roster aggregates, rolling form). Exported to ONNX and loaded at API startup via the `ort` crate.

```
GET /api/predict?home=Duke+Blue+Devils&away=North+Carolina+Tar+Heels&neutral=false
```

Training pipeline lives in `training/`. Set `MODEL_DIR` env var to override the model path (defaults to `training/models/`).

## Development

```bash
cargo build --workspace              # Build all crates
cargo check --workspace --all-targets # Type check
cargo fmt --all -- --check           # Format check
cargo clippy --workspace --all-targets -- -D warnings  # Lint
cargo test --workspace               # Run tests (requires Postgres)
```

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | — | Postgres connection string |
| `NATSTAT_API_KEY` | Yes | — | NatStat API v4 key |
| `BIND_ADDR` | No | `0.0.0.0:8080` | API server bind address |
| `RUST_LOG` | No | — | Tracing filter (e.g. `cstat_api=info`) |
| `MODEL_DIR` | No | `training/models/` | Path to ONNX model directory |

## License

See [LICENSE](LICENSE).
