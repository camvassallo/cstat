# cstat

College basketball analytics platform. Ingests data from the NatStat API and Barttorvik, computes advanced metrics (KenPom-style adjusted efficiency, player percentiles, rolling averages, CamPom composite valuation), clusters players into D&D-class archetypes, and serves everything through a REST API and React frontend. Includes ML-based game predictions (with honest point-in-time + preseason-blended in-season forecasts), preseason team projections off transfer-portal / recruit / returning-player rosters, and descriptive coach-above-expectation ratings — all from LightGBM models exported to ONNX. Multi-season browsing via a `?season=` query param; back-test the predict model against historical games by switching seasons in the nav.

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
./scripts/start.sh start
```

This starts Postgres, the API server, and the Vite dev server. On first run, Cargo will compile the Rust workspace (this takes a few minutes).

| Service  | URL                    |
|----------|------------------------|
| Frontend | http://localhost:5173  |
| API      | http://localhost:8080  |
| Postgres | localhost:5432         |

### Managing Services

```bash
./scripts/start.sh start        # Start all services
./scripts/start.sh stop         # Stop all services
./scripts/start.sh status       # Check what's running
./scripts/start.sh logs         # Tail API + web logs
./scripts/start.sh logs api     # Tail API logs only
./scripts/start.sh logs web     # Tail web logs only
./scripts/start.sh logs postgres # Tail Postgres container logs
```

The start script automatically cleans up stale processes on conflicting ports.

### Ingesting Data

Before the app has anything to display, you need to ingest data from NatStat:

```bash
# Full bootstrap: NatStat → Torvik → compute. Defaults to the current
# NCAA basketball season; pass --year YYYY to target another.
cargo run --bin cstat-ingest -- season
```

`season` runs the seven NatStat ingest steps, then Barttorvik player stats,
then the cstat-core compute pipeline. Pass `--no-torvik` or `--no-compute`
to skip parts (e.g. when batching several updates first).

`--year` defaults to the current NCAA basketball season (Nov+ rolls
forward), so you don't have to re-edit the binary at season turnover.

Other ingest subcommands:

| Command | Description |
|---------|-------------|
| `season [--year YYYY] [--no-torvik] [--no-compute]` | Full bootstrap: NatStat + Torvik + compute |
| `teams [--year YYYY]` | Teams only |
| `players [--year YYYY]` | Players only |
| `team CODE [--year YYYY]` | Single team (roster, details, box scores) |
| `games [--year YYYY] [--from DATE --to DATE]` | Games for a season or date range |
| `perfs [--year YYYY] [--from DATE --to DATE]` | Box scores for a season or date range |
| `update --from DATE --to DATE [--year YYYY] [--no-compute]` | Incremental refresh of recent games + perfs, runs compute by default |
| `compute [--year YYYY]` | Derive season stats, percentiles, rolling averages |
| `status` | Show NatStat API rate limit status |
| `clean-cache` | Remove expired API cache entries |
| `torvik [--year YYYY] [--rebounds]` | Ingest Barttorvik player stats + optional rebound backfill |
| `elo [--year YYYY]` | Ingest ELO ratings from `/elo` endpoint |
| `forecasts [--year YYYY]` | Ingest per-game forecasts (pre/post-game ELO, win exp) from `/forecasts` |
| `campom-parity [--year YYYY]` | Validate CamPom intermediates against an external reference CSV |
| `explore ENDPOINT [--range PARAMS]` | Dump raw API JSON for exploration |
| `bootstrap-csv [--year YYYY] …` | Bootstrap a historical season from NatStat dashboard CSV exports (no live API) |
| `transfers --year YYYY [--bootstrap-from PATH]` | Ingest the 247Sports transfer portal class (needs `TFS_247_JWT`) |
| `recruits --year YYYY [--bootstrap-from PATH]` | Ingest the 247Sports HS recruit class (needs `TFS_247_JWT` / `TFS_247_COOKIE`) |
| `coaches [--year YYYY]` | Ingest the Barttorvik coachdict head-coach mapping |
| `compute-projections [--year YYYY]` | Materialize each team's served preseason AdjEM band into `team_preseason_projection` |
| `projections-backtest [--years …] [--output PATH]` | Leave-one-season-out projection accuracy backtest (per-team JSON dump) |
| `measure-blend-accuracy --years … ` | Grid-search the preseason×point-in-time blend schedule against historical MAE |

### Adding a New Season

The frontend reads the season list from `GET /api/seasons` (which mirrors
`SELECT DISTINCT season FROM games`), so adding a season is just data work —
no source edits needed for the dropdown.

```bash
# 1. Bootstrap the season end-to-end (NatStat + Torvik + compute).
cargo run --bin cstat-ingest -- season --year 2022

# 2. Retrain archetypes on the combined cohort. Required to keep cross-season
#    class stability — see docs/archetypes_methodology.md before deviating.
cd training && python -m archetypes --seasons 2022,2023,2024,2025,2026
```

That's it. The next page load picks up `2022` in the season selector. If
you want transfer-portal data for the new year, ingest it via the DB-backed
pipeline: `cargo run --bin cstat-ingest -- transfers --year YYYY` (needs
`TFS_247_JWT`; pass `--bootstrap-from path/to/snapshot.json` to load from a
local capture instead of hitting the API).

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
| `GET /api/predict` | ML game predictions (margin + win prob); `?as_of_date=` for point-in-time/blended |
| `GET /api/projections/{year}` | Preseason team AdjEM projections ("Future" tab); `/teams/{team_id}` for the per-player roster breakdown |
| `GET /api/transfers/{year}` | Transfer-portal class with projected CamPom + Δ ranking |
| `GET /api/recruits/{year}` | HS recruit class with projected freshman CamPom |
| `GET /api/coaches` | Coach-above-expectation leaderboard; `/coaches/{id}` for detail |
| `GET /api/teams/{id}/coach` | Coach card for a team (decoupled from the projection loop) |
| `GET /api/seasons` | Distinct seasons (powers the season selector) |
| `GET /api/health` | Health check |
| `GET /api/status` | API status |

### Compute Pipeline

The compute pipeline in `cstat-core` derives all advanced metrics from raw box score data:

- **Game stats** — defensive rebounds, assist-to-turnover ratio, game score
- **Player season stats** — per-game averages across all stat categories
- **Team season stats** — four factors, raw efficiency, wins/losses (derived from authoritative `team_game_stats`)
- **Adjusted efficiency** — KenPom-style iterative regression (ADJO/ADJD)
- **Player percentiles** — PERCENT_RANK across all players
- **Rolling averages** — last-5-game rolling stats
- **Player rates** — AST%, ORB%, DRB%, STL%, BLK%, FT Rate (possession-based formulas)
- **CamPom** — composite player valuation; methodology in `docs/campom_methodology.md`

### Player Archetypes

12 D&D-class archetypes (Wizard, Sorcerer, Warlock, …) assigned via combined-cohort k-means in `training/archetypes.py`. Run with `cd training && python -m archetypes --seasons 2022,2023,2024,2025,2026 [--diagnostics]` (the module is loaded from inside `training/`, not as `training.archetypes`). Methodology, retraining playbook, and health-metric tripwires are documented in `docs/archetypes_methodology.md` — read it before touching signatures or adding seasons.

### ML Predictions

Four LightGBM model families, all exported to ONNX and loaded at API startup via the `ort` crate:

- **Game prediction** (margin / win / total) — 49 point-in-time diff-features for margin/win; 58 features for total (the 49 diffs plus 9 `sum_*` level-sensitive companions). Trained on 47,502 games from cstat-seasons 2015-2026 (after feature-completeness filter). A leak-free **point-in-time** twin (`pit_*`, 44,338 games) substitutes a point-in-time CamPom channel for honest in-season prediction — `/api/predict?as_of_date=…` serves it, blended with the preseason projection early in the season.
- **Trajectory** — 48 features, 24,168 N→N+1 player-pairs across the 2015-2026 cohort. Projects returning-player CamPom v3 (mean + q10/q90) for next season.
- **Freshman** — 13 features, 3,253 freshmen across recruit classes 2014-2025. Projects freshman-season CamPom v3 (mean + q10/q90) from 247 composite + school context.
- **Roster impact** — 27 features, 4,255 team-seasons (2015-2026). Projects team AdjEM from minutes-weighted roster aggregates; the preseason projection calibrator behind `/api/projections`. (Replaces the deprecated box-score `roster_model`, now dead and unloaded.)

Per-model stats: `docs/model_performance.md`. Preseason projection methodology: `docs/projections_methodology.md`.

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

## Production Sync

The site has no user-generated data — every row in prod is derived from the
local ingestion + compute pipeline, so local is the source of truth and prod
is a deterministic mirror. Schema is owned by sqlx migrations (auto-applied
at API startup); only data needs an explicit push.

```bash
# Get the connection string from Railway (dashboard or CLI)
export PROD_DATABASE_URL="postgresql://..."

# Preview what would be synced
./scripts/sync_to_prod.sh --dry-run

# Apply
./scripts/sync_to_prod.sh
```

The script dumps locally with `pg_dump -Fc` (binary, compressed — typically
~7× smaller than plain text), then in a single transaction TRUNCATEs every
public table on prod (except `api_cache` and `_sqlx_migrations`) and
restores via `COPY` statements from `pg_restore`. Atomic: prod readers see
old data until the COMMIT, then new instantly. Falls back to running the
psql tools inside the local Postgres container if the host doesn't have
them installed.

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | — | Postgres connection string |
| `NATSTAT_API_KEY` | Yes | — | NatStat API v4 key |
| `NATSTAT_MAX_PER_HOUR` | No | `500` | NatStat per-hour rate budget. Standard tier is 500; raise to match your tier rather than recompiling. |
| `BIND_ADDR` | No | `0.0.0.0:8080` | API server bind address |
| `RUST_LOG` | No | — | Tracing filter (e.g. `cstat_api=info`) |
| `MODEL_DIR` | No | `training/models/` | Path to ONNX model directory |

## License

See [LICENSE](LICENSE).
