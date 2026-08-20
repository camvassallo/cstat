# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Version Control

**Leave all git operations to the user.** Do not run `git commit`, `git branch`, `git checkout`/`switch`, `git merge`, `git rebase`, `git push`, `git pull`, `git stash`, or `git reset` — the user manages version control themselves. Read-only inspection (`git status`, `git log`, `git diff`, `git show`) is fine. Perform a git mutation **only** when the user explicitly asks for it in that same message; a general "work on this PR" is not such a request. When work is ready to commit, finish the code changes and let the user handle staging, committing, and pushing.

### PR drafts (`pr.md`)

`pr.md` is a gitignored local scratch file for drafting PR descriptions. **The input is a plaintext bullet list of changes** — the user (or you, from the diff) drops a flat list of what changed into it, and you expand that into a full PR description in place. Expanded form (the PR **body** only): `## Summary`, `## Changes` (the bullets, fleshed out), and `## Verification` if applicable. Keep it Markdown; never stage or commit it.

**The title does NOT go in `pr.md`.** Do not put a `# Title` heading at the top — `pr.md` is body-only, opening with `## Summary`. The title belongs on the PR itself: when a PR exists, apply it with `gh pr edit <n> --title "…"` (and push the body with `--body-file pr.md` if the user wants it published); at PR-creation time it's `gh pr create --title "…" --body-file pr.md`. If no PR exists yet, state the proposed title in chat so the user has it. (This is the one sanctioned PR mutation flow — the "leave git to the user" rule in Version Control still governs commits/branches/pushes; only set a PR title/body when the user has asked you to populate the PR.)

## Documentation Style

**No pictographic emoji in docs or doc-like prose** (README, ROADMAP, `docs/`, `pr.md`, code comments). Use plain **bold** for banners/callouts instead of emoji like ⚠️/🚀/✅. Plain arrows (`→`) and check/cross markers (`✓`/`✗`) are fine.

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

# Frontend (web/) — React + Vite + TS
cd web && npm run dev                            # dev server (proxies /api)
cd web && npm run build                          # tsc -b && vite build
cd web && npm run lint                           # eslint (CI-relevant)
cd web && npm test                               # vitest (pure-logic unit tests, e.g. Portle)

# Local Postgres
docker compose up -d               # Postgres 17 on :5432

# Offline season-replay harness (M4) — isolated sim DB on :5433, never started
# by a plain `docker compose up -d`; simulate refuses to run against the main
# or prod DB (host/port/dbname guard)
docker compose --profile sim up -d postgres-sim
cargo run --bin cstat-ingest -- simulate --year 2026 --from 2025-11-02 --to 2025-11-09 --reset

# Archetype training (Python) — pass ALL ingested seasons (the shipped model is one
# combined-cohort fit; the CLI default of 2025,2026 is NOT a full retrain). Uses training/.venv.
cd training && ./.venv/bin/python -m archetypes --seasons 2015,2016,2017,2018,2019,2020,2021,2022,2023,2024,2025,2026 [--diagnostics]

# Retrain the model tree from a node downward, in dependency order. USE THIS
# rather than running trainers by hand — the chain is roster_impact -> roster_adjo
# -> backtest -> cae -> compute-projections, and hand-running it is what let
# `roster_adjo` serve a three-generation-stale OOF for months (#218). The two
# roster-frame models also stamp their meta with an OOF fingerprint and the API
# REFUSES TO BOOT if they disagree, so retraining one without the other is a
# hard failure, not a silent one.
# **`train_roster_adjo_model.py` does NOT ride along on a roster_impact retrain.**
# It `import`s `build_dataset` from the net trainer, which reliably reads as "the
# AdjO half updates itself." It does not — it needs its own invocation, and that
# exact misreading is what caused #218 (skipped in #130, #152, #211). Same trap
# shape as the archetype `--seasons` default above: the wrong thing succeeds
# quietly. Canonical layer map, retrain protocol, and the deploy-vs-sync split:
# `docs/model_dependency_graph.md`.
./training/retrain_downstream.sh [--dry-run]        # Layer 2 + 3 (the common case)
./training/retrain_downstream.sh --with-layer1      # also trajectory + freshman;
                                                    # these TRUNCATE the OOF tables
                                                    # and invalidate everything below
./training/retrain_downstream.sh --from cae         # resume after a failed stage
# "Which nodes are stale?" — every trainer stamps an `input_provenance`
# fingerprint of its own inputs; this recomputes them against the live DB and
# propagates staleness downward. Catches the case the boot guard structurally
# cannot: a Layer 1 retrain with no Layer 2 retrain leaves both Layer 2 halves
# agreeing with EACH OTHER, and both stale. Verdicts are CURRENT / CHURN /
# STALE / UNSTAMPED — the in-progress season is rewritten nightly, so a change
# there is expected churn, while a CLOSED season moving is real drift.
cd training && ./.venv/bin/python check_provenance.py    # exit 1 on drift
# Layer 3 (team_preseason_projection, backtest dumps, coach_season_cae) has no
# meta to stamp, so its producers record the model artifact into
# `artifact_provenance` (migration 047) and the check compares ONNX digests.
# This is what catches a backtest run against a gitignored LOSO set drawn from
# a different frame than the committed serving model. Report-only: exit 2 is
# reserved for conditions that stop the API booting, and a stale projection
# table is a freshness problem, not one of those.
# Note: `compute-projections` writes team_preseason_projection AND
# player_season_projection. The wide season range is right for the former (the
# preseason blend reads history) but materializes historical rows in the latter
# that nothing serves — narrow with --years if you only meant the team table.

# Push local data to prod (no schema migrations needed if migrations/ is unchanged).
# A FULL sync is now REFUSED (exit 3) while prod looks live — a served-critical step
# succeeded within 36h, or the calendar says in-season — because it would roll the
# live site back to this laptop's copy. Full mode is a bootstrap/backfill operation.
./scripts/sync_to_prod.sh [--dry-run]
./scripts/sync_to_prod.sh --force-full        # override the guard (know why first)
# Read-only prod inspection — writes nothing, works with the local DB down.
# Per-step ledger freshness, recent failures, exact row counts, guard verdict:
./scripts/sync_to_prod.sh --prod-status
# Targeted push — only the named tables (Railway-direct split: local heavy jobs
# push their derived tables without truncating the cron-written serving tables).
# NOT gated by the guard; this is the intended in-season path:
./scripts/sync_to_prod.sh --tables lineup_aggregates,player_rapm
# Column merge — the third mode, for a derived column on a table that is
# REFERENCED by foreign keys, where --tables would cascade-wipe the dependents
# (`players` has 10) and a full sync is refused. UPDATE-only: no TRUNCATE, no
# INSERT, no DELETE, named columns only, rows matched on a unique INDEX (the
# natural key) rather than the locally-generated UUID primary key. While prod
# looks live (same two signals as the full-sync guard), a table carrying a
# `season` column merges PAST SEASONS ONLY — prod's nightly owns the current
# season for anything `compute_all` derives, `display_name` included, so
# pushing it would be a one-column rollback. `--force-full` merges every season.
# Reports rows actually updated and warns loudly on zero (a natural key that
# doesn't line up with prod matches nothing and otherwise looks like success):
./scripts/sync_to_prod.sh --columns players.display_name
# CAVEAT — a NEW curated display-name override needs a REDEPLOY, not a sync.
# `data/player_display_names.json` is `include_str!`-compiled into the binary
# (`crates/cstat-core/src/display_names.rs:56`), so prod's nightly recomputes the
# current season from the copy baked into the DEPLOYED image. In-season the merge
# skips the current season, so the override cannot land that way at all. Off-season
# it DOES write those rows — and nothing repairs them, because the very condition
# that widens the scope (no successful nightly in 36h) means no nightly is coming.
# Either way the fix is the same: deploy the image. The merge is for HISTORICAL
# seasons, which no nightly rewrites.

# Archetype in-season stability sweep — how many games until a label matches the
# full-season label. Re-run after any retrain; the curve is a property of the fit.
cd training && ./.venv/bin/python experiment_archetype_stability.py \
  --seasons 2022,2023,2024,2025,2026 --out eval_history/archetype_stability_YYYYMMDD_summary.json
```

## Environment Variables

Copy `.env.example` to `.env`. Required:
- `DATABASE_URL` — Postgres connection string
- `NATSTAT_API_KEY` — NatStat API v4 key (format: `xxxx-xxxxxx`)

Optional: `BIND_ADDR` (default `0.0.0.0:8080`), `RUST_LOG` (tracing filter). API-serving knobs (all defaulted, override only to tune): `DATABASE_MAX_CONNECTIONS` (pool size, default 25), `REQUEST_TIMEOUT_SECS` (per-request 408 timeout, default 30), `MAX_INFLIGHT_REQUESTS` (concurrency before 503 load-shed, default 256) — see `crates/cstat-api/src/guards.rs`. The API connects via `Database::connect_api` (adds an `acquire_timeout` + per-connection `statement_timeout`); the ingest/compute CLI uses the unguarded `Database::connect` so its long batch writes aren't capped.

Nightly-ingest (M2, set on the Railway cron service, not the API — all fail-soft / no-op when unset): `SLACK_WEBHOOK_CRON` (Slack incoming-webhook for `#cron-job-alerts`; `cstat-ingest nightly` posts one message per run — success heartbeat, degraded warning, or critical abort alert; legacy name `INGEST_ALERT_WEBHOOK` still honoured), `HEARTBEAT_URL` (optional dead-man's-switch — the nightly pings it on success / `…/fail` on a degraded run so an external monitor pages on a *skipped* run the in-run Slack alerts can't cover), `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` (when both set, nightly purges the Cloudflare edge after a successful compute; otherwise the 5-min `Cache-Control` TTL handles it), `TORVIK_PROXY_URL` (escape hatch for the barttorvik egress-IP block — routes only Torvik through a fixed non-Google egress; unset means a direct connection. The primary fix is Railway **static outbound IPs on the cron service**, since barttorvik refuses Google IP space and Railway's unpinned placement can land on GCP — full diagnosis and runbook in `docs/torvik_egress_block.md`). Error channels, set on the **API service** (fail-soft / no-op when unset): `SLACK_WEBHOOK_ERRORS_API` (`#errors-api` — cstat-api boot/serve failures, 5xx responses, and handler panics; 5xx-tap + panic-hook share one 60s in-process throttle, see `guards.rs`) and `SLACK_WEBHOOK_ERRORS_WEB` (`#errors-web` — uncaught browser errors from the SPA reporter `web/src/lib/errorReporter.ts` → `POST /api/client-error` → relay), and `ALERT_SELFTEST_TOKEN` (gates `GET /api/alert-selftest?channel=api|web` — token in the `X-Selftest-Token` header, constant-time compared; an on-demand synthetic post to verify the alert pipeline stays healthy, returning `posted:true` only if the Slack POST actually landed; 404 when the token is unset/wrong). Slack routing is per-channel: a webhook is bound to one channel, so each channel is its own `SLACK_WEBHOOK_*` var registered in `notify::SlackChannel`. See `crates/cstat-ingest/src/notify.rs` and `docs/deploy_nightly_cron.md`.

## Architecture

Three-crate Rust workspace:

- **cstat-core** — Shared types, DB models (`models/`), query layer (`db.rs`), and compute pipeline (`compute.rs`). The `Database` struct wraps `PgPool` and handles migrations via SQLx.
- **cstat-ingest** — NatStat API client (`client.rs`), response cache (`cache.rs`), token-bucket rate limiter (`rate_limiter.rs`), and ingestion pipeline (`ingest/`). CLI binary at `src/bin/ingest.rs` with subcommands: `season`, `teams`, `players`, `team`, `games`, `perfs`, `update`, `elo`, `forecasts`, `compute`, `status`, `preflight` (connectivity health check — pings DB/NatStat/Torvik/247, exits non-zero on a serving-critical feed down), `clean-cache`, `torvik`, `campom-parity`, `explore`, `bootstrap-csv` (historical CSV bootstrap), `playbyplay` (intra-season PBP loader), `simulate` (M4 offline season-replay harness — replays a CSV-bootstrapped season through the real `nightly` orchestrator against the isolated `postgres-sim` DB via synthesized `api_cache` fixtures, with invariant checks per window + an idempotency re-run; the simulated clock advances through `cstat_ingest::today_utc()`, which also honors `CSTAT_SIMULATED_DATE`), `lineups` (NatStat `games;lineups`-object capture into the durable local-only `natstat_lineups` tables — restart-safe via its ledger, v4-pinned), `transfers` / `recruits` / `coaches` (247-portal / 247-recruit / coachdict ingest), `departures` / `departures-audit` (hand-curated non-portal, non-draft exits into `player_departures` from `data/departures/{year}_departures.json`, plus the offseason worklist that ranks the returners the projection still assumes are coming back — see `docs/projections_methodology.md`), `returns` (the inverse capture — players the `class_year == 'Sr'` inference deletes who are actually coming back under the NCAA 5-in-5 rule, loaded from `data/returns/{year}_returns.json` into `player_returns`; `granted` projects as an ordinary returner, `contested` goes to the uncertain `?` bucket and widens the team's floor/ceiling band — see `docs/eligibility_5in5.md`), and the projection/eval tooling `projections-backtest`, `compute-projections`, `measure-blend-accuracy`. **`season` is the bootstrap command** — it runs the seven NatStat steps, then Torvik, then `compute_all`, in one call. `update` likewise runs compute at the end by default. Both accept `--no-torvik` / `--no-compute` opt-outs. **`nightly` is the in-season production refresh** (`SeasonIngester::nightly`) — the served-critical subset (games/perfs/teamperfs by date range → forecasts → `/elo` ratings → Torvik with per-game persistence) **before** `compute_all`, recording each step to the `ingest_runs` ledger; window defaults to yesterday..today (UTC). Unlike `update` it refreshes Torvik + `/elo` first, so `cam_gbpm_v3`/`pit_cam_v3` and `team_season_stats.elo_rating` (the served `diff_elo_rating` feature) don't go stale on a recompute. It also refreshes the 247 transfer portal best-effort (two class years, incremental) using a **guest** JWT minted per run from the public portal page — no `TFS_247_JWT` needed. That reverses ROADMAP S5/P3, whose premises (no in-season portal churn; 247 can't be autonomous) both stopped holding under the 5-in-5 rule: 51 players entered the 2026 class in the first 19 days of August 2026. Best-effort and placed last, so a 247 outage can't degrade a game-night box-score refresh. Full plan: `docs/in_season_ingest_plan.md`; 247 endpoint map, auth tiers and payload quirks: `docs/247_api.md`. **`--year` defaults to `current_natstat_season()`** (date-derived in `crates/cstat-ingest/src/lib.rs`), so the binary stays correct as the calendar rolls. Single team-id resolver lives at `cstat_ingest::team_id_by_code_and_season`; don't inline the `(natstat_id, season)` lookup. The `Team` subcommand delegates to `SeasonIngester::ingest_team(code)` — keep new per-team orchestration there, not in the bin.
- **cstat-api** — Axum HTTP server. `AppState` holds `Database` + `NatStatClient` + `Predictor`. Routes under `/api/`. Health/observability routes (`/api/health`, `/api/status`, `/api/health/ingest`) are mounted **un-guarded** in `main.rs` so a saturated server or stale pipeline still answers — they bypass the cache/timeout/load-shed layer that wraps the data routes. `GET /api/health/ingest` (`routes/health.rs`) reports per-step freshness from `ingest_runs` and returns **503 when any served-critical step is >36h stale**, so an external uptime monitor catches a missed nightly.

Data flow: **NatStat API → cstat-ingest → Postgres → cstat-core (compute) → cstat-api → frontend/ML**

## Compute Pipeline

`cstat-core/src/compute.rs` contains all derived metric calculations (~1,500 lines):
- `reconcile_player_teams` — sets `players.team_id` to each player's *most-frequent* `player_game_stats` team (the box-score majority), undoing the ingest's first-write-wins `team_id` poisoning from NatStat source roster swaps (issue #119; e.g. a season opener that tagged Zion to Kentucky). Runs early so every team-joined step downstream sees the corrected roster.
- `correct_swapped_games` — finds fully-swapped games (NatStat labels a 2-team game's rosters/scores/box rows onto each other's team — the 2018 Duke/Kentucky Champions Classic stored Duke as *losing* 118-84) via a conservative bidirectional-cross-tag detector, then relabels them: swap `home`/`away` on `games`, swap the box columns between the two `team_game_stats` rows, point each `player_game_stats` row at its reconciled real team. Runs after `reconcile_player_teams` and before four factors / W-L / AdjEM so they recompute from corrected box rows. Idempotent.
- `repair_phantom_swapped_games` — the harder swap variant `correct_swapped_games` can't see (issue #140): NatStat crossed the two rosters AND minted a fresh per-game "phantom" natstat id for every player, so each phantom's only game reconciles it to its own (wrong) label and the cross-tag detector finds no displacement. Four 2024-11-15/16 games hit this (Virginia/Villanova, Virginia Tech/Penn State, Holy Cross/Sacred Heart, UT Rio Grande Valley/Tennessee Tech) — e.g. Virginia's 2025 roster was full of Villanova players and the box said Villanova won 70-60 when Virginia won 70-60. Each phantom is a duplicate of a real human on the OPPONENT team; re-identified by exact normalized name then a unique/first-name-disambiguated last-name fallback (catches "DK Thorn"→"Dekedran Thorn", "Tobi Lawal"→"Toibu Lawal", "Ace Baldwin Jr."→"Ace Baldwin"). A phantom with no counterpart is a genuine 1-game player and is re-teamed, not merged. For each gated game (both sides ≥80% of box rows resolving to the opponent) it reattaches the phantom's box / play-by-play / Torvik rows to the real player, relabels the game and PBP team-side (and PBP running score / onfloor columns), re-teams **every** box row to the opponent (not only phantoms — a full swap also crosses the odd non-phantom real player who happened to play, whose stranded line would otherwise split his season stats), then deletes the orphaned phantom players (`player_archetypes` / `player_rapm` cascade). Runs after `correct_swapped_games`, before `compute_player_season_stats` / four factors / W-L / AdjEM. Idempotent — invariant guarded by `tests/swapped_games.rs::no_phantom_swapped_games_remain`. (Archetypes for the corrected rosters repopulate on the next `python -m archetypes` retrain.)
- `reattach_misidentified_players` — moves `player_game_stats` rows that NatStat stamped with the WRONG *same-name* player's natstat id onto the real human (issue #138; e.g. two "Jake Davis" in 2026 — Illinois and Cal Poly — where Cal Poly's box lines arrived stamped with Illinois Davis's id, producing a spurious 2-GP per-team season row on his progression page). Fingerprint: a box row on a team that is NOT its owner's reconciled majority team, while a single DISTINCT same-name player genuinely rosters to that team. Conservative — only fires on an unambiguous sibling with no clashing `(player_id, game_id)` row; genuine mid-season transfers (one natstat_id, no same-name sibling on the new team) are exempt. Runs after `reconcile_player_teams` and before `compute_player_season_stats`. Idempotent.
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

12 D&D-class archetypes assigned via combined-cohort k-means clustering. The **fit** (clustering + Hungarian class matching, authoritative for `archetype_models`) lives in `training/archetypes.py` and runs annually; the **assign** half (standardize → nearest frozen centroid → class → softmax affinities → `player_archetypes`) was ported to Rust (`cstat_core::compute::compute_archetypes`) and runs every nightly as `compute_all`'s last step, byte-exact with the Python writer (guarded by `crates/cstat-core/tests/archetype_assign_parity.rs`) — so prod produces archetypes with no laptop, and `player_archetypes` is prod-owned in-season. Methodology and retraining playbook in `docs/archetypes_methodology.md`. Run with `cd training && ./.venv/bin/python -m archetypes --seasons 2015,…,2026 [--diagnostics]` — `training/` has no `__init__.py`, so `python -m training.archetypes` from the repo root fails; you must `cd` in first. **A full retrain must pass EVERY ingested season explicitly** — the shipped model is a single combined-cohort fit over all 12 seasons (one shared centroid set in `archetype_models`, verified identical across season rows). The CLI's `--seasons` *default is only `2025,2026`*, which is a 2-season fit that does NOT match the shipped model and clusters differently (it tripped extra signature-alignment violations) — don't use the default for a refresh. Clustering runs on the union and writes per-season rows to `player_archetypes` with shared centroids. The signature-alignment guardrail hard-fails the write on label/cluster mismatch; bypass with `--no-verify` only after reviewing the diagnostics (e.g. the benign Rogue `blk_pct` flag — that weight is deliberately softened). Combined-cohort training is load-bearing for cross-season class stability (45.7% returning-player primary stability vs 28% for per-season fits) — read the doc before changing it.

## Database

Postgres with SQLx. Migrations in `/migrations/` (52 files). Key tables: `teams`, `players`, `games`, `player_game_stats` (110+ columns), `player_season_stats`, `team_season_stats`, `team_game_stats`, `player_percentiles`, `game_forecasts`, `torvik_player_stats`, `torvik_player_game_stats` (point-in-time CamPom source), `player_archetypes`, `archetype_models`, `api_cache`, `transfers`, `recruits`, `draft_entrants`, `player_departures` (hand-curated non-portal/non-draft exits — pro signings abroad, retirements, dismissals; the projection's fourth departure channel, see `docs/projections_methodology.md`), `player_returns` (hand-curated eligibility returns — the 5-in-5 stay-put channel, see `docs/eligibility_5in5.md`), `trajectory_oof_predictions` / `freshman_oof_predictions` (held-out projection inputs), `team_preseason_projection` (materialized served AdjEM band), `coaches` / `coach_seasons` (coachdict entity model), `coach_season_cae` / `coach_ratings` (coach-above-expectation grades), `ingest_runs` (per-step nightly-ingest ledger — runtime-written on prod, excluded from `sync_to_prod.sh`), `artifact_provenance` (which model artifact produced each Layer 3 derived product — see `docs/model_dependency_graph.md` §6).

All season-scoped tables carry a `season` column; the API and frontend support arbitrary multi-year browsing via a site-wide `?season=` query param plumbed through `web/src/components/season.ts::useSeason()`. **The frontend reads the season list from `GET /api/seasons`** (DISTINCT season FROM games, newest first) — no source edit needed for the dropdown when adding a year. Adding a new season is two commands: `cargo run --bin cstat-ingest -- season --year YYYY` (NatStat + Torvik + compute, end-to-end) and a `cd training && python -m archetypes --seasons …` retraining pass on the new combined cohort. Optional: ingest the 247Sports transfer portal for that class year via `cargo run --bin cstat-ingest -- transfers --year YYYY` (live API, needs `TFS_247_JWT`) or `--bootstrap-from data/transfers/YYYY_raw.json` to load a captured snapshot. Rows land in the `transfers` table; the `/api/transfers/{year}` route reads from there.

**UUIDs are season-scoped on `teams` and `players`** — Duke 2025 and Duke 2026 are different rows with different `id`s, joined by the cross-season `natstat_id` (UNIQUE on `(natstat_id, season)`). The detail-page API endpoints (`GET /api/teams/:id`, `GET /api/players/:id`) re-resolve via `natstat_id` when the requested season doesn't match the URL's UUID, so a cross-season URL like `/teams/<2026-uuid>?season=2025` returns Duke 2025 and the frontend redirects to the canonical UUID. See `queries::resolve_{team,player}_id_for_season`.

**Roster ingest caveat**: NatStat's `/players/mbb/{TEAMCODE}` endpoint has no historical-season filter — it always returns the *current* roster. The box-score path (`games.rs`) is the sole authority for `players.team_id` per season; `players.rs::upsert_player` deliberately never overwrites `team_id` on conflict. Running `cstat-ingest players --year YYYY` against a non-current season warns once and only enriches metadata fields (height, weight, position, etc.). Box-score ingest auto-creates player rows with the correct team, so historical seasons are safe to add via `cstat-ingest season --year YYYY` alone.

**Never edit a migration prod has applied** — not even comments. SQLx checksums every file in `/migrations/` and refuses to boot if the on-disk hash differs from `_sqlx_migrations.checksum` in prod. To correct one, add a new migration.

**A migration created in an unmerged PR is NOT in that category.** Prod applies migrations on deploy, so a file added on a branch that hasn't merged has never entered prod's ledger and is free to edit — fold the correction into it rather than shipping a `CREATE` + `DROP` pair that has to live in the history forever. The checksum only binds once it has been deployed.

The catch is *local*: your dev DB has already applied it, so editing the file makes your own boot fail. Re-baseline before rebuilding — undo what the migration did, drop its ledger row, and let the migrator re-apply the edited version:

```sql
-- e.g. reverting migration 050 after editing it
ALTER TABLE players DROP COLUMN IF EXISTS display_name;
DELETE FROM _sqlx_migrations WHERE version = 50;
```

Then `touch crates/cstat-core/src/db.rs && cargo build` — the migration files are embedded by the `sqlx::migrate!` macro at compile time, so a `.sql`-only change does **not** trigger a rebuild on its own and the binary will silently keep running the old set. Re-derive anything the dropped column held (a compute step, usually).

For data-driven migrations (e.g. `017_team_short_names.sql` is sourced from `data/team_short_names.json`), edit the JSON and re-run the relevant `cstat-ingest` command — no SQL needed.

## ML Inference

ONNX models are loaded at API startup via the `ort` crate (ONNX Runtime):
- `Predictor` in `cstat-core/src/inference.rs` loads the full suite and hard-fails on meta drift at boot: game models `margin_model` / `win_model` / `total_model`; their point-in-time twins `pit_margin` / `pit_win` / `pit_total` (fed point-in-time CamPom for honest in-season prediction); the per-player projection models `trajectory_{mean,q10,q90}` and `freshman_{mean,q10,q90}`; and the team `roster_impact_model` (preseason projection calibrator). The legacy box-score `roster_model.onnx` is **dead — deliberately not loaded** (its only consumer, the freshman statline, was deprecated; full removal tracked in ROADMAP Refactor Backlog).
- `features.rs` — builds the 49-feature diff vector from DB (team stats, roster aggregates, rolling form); `build_all_features_pit` swaps the leaky season-aggregate CamPom channel for a point-in-time map when `as_of_date` is set.
- `GET /api/predict?home=Duke+Blue+Devils&away=North+Carolina+Tar+Heels&neutral=false` — returns predicted margin + win probability; `&as_of_date=YYYY-MM-DD` serves point-in-time + preseason×pit-blended predictions, and `prediction_basis` (`preseason`|`blended`|`pit`|`leaky`) labels the active regime. The early-season preseason blend (peak 0.70 at the Nov 1 open, linear decay to 0 over 42 days) also engages on the **live** path — no `as_of_date` computes the weight from `today_utc()`, so opening-week live predictions (Predict page, TeamDetail Projected column, ScoreTicker via `predict_projection`) anchor on the preseason projection instead of a 1–2 game sample; past the decay window it's a no-op.
- Models live in `training/models/`; set `MODEL_DIR` env var to override path

## ML Training

Python pipeline in `/training/`:
- LightGBM models for margin (regression), win probability (classification), and total points (regression), each with a leak-free point-in-time variant (`pit_*`)
- 49 point-in-time diff-features from team/roster/form/context (`features.py`); `GBPM_VARIANT=pit_cam_v3` asof-merges a point-in-time CamPom grid for the honest in-season backtest
- Projection models, in dependency order: `train_trajectory_model.py` (returner year-over-year CamPom, mean + q10/q90) and `train_freshman_model.py` (recruit first-season CamPom) are **Layer 1** — they persist held-out predictions to the `*_oof_predictions` tables; `train_roster_impact_model.py` (roster aggregate → team AdjEM, the served preseason calibrator) and `train_roster_adjo_model.py` (the display-only AdjO half, same frame) are **Layer 2**, trained on those OOF tables rather than on actuals. All leave-one-season/class-out backtested. Because Layer 2 trains on Layer 1's *predictions*, it absorbs upstream bias rather than compounding it — which makes the failure mode **desynchronization**, not bad data. Retrain from the highest stale node downward via `retrain_downstream.sh`; graph and protocol in `docs/model_dependency_graph.md`.
- `compute_cae.py` computes coach-above-expectation grades from the roster-projection residual (descriptive, display-only)
- Exports to ONNX format in `training/models/` (target_opset=15); `export_onnx.py` removes ZipMap for ort compatibility and honors a `MODEL_DIR` override

## NatStat API

Docs in `docs/natstat-api-v4.md`. Rate limit: 2500 calls/hour on the API+ tier (configurable via `NATSTAT_MAX_PER_HOUR`; default 2500; standard tier is 500). Both the API server and `cstat-ingest` binary read this through `cstat_ingest::rate_budget_from_env()`. URL pattern: `https://api4.natst.at/{apikey}/{endpoint}/{service}/{range}/{offset}`. Responses cached in `api_cache` table with TTL. **All season-wide endpoints use the same pagination shape** — `playerperfs/{season}` and `teamperfs/{season}` both fetch every team's data in one paginated call rather than per-team loops.
