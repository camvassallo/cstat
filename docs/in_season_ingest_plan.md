# In-Season Ingest & Automation Plan

*Scoping doc — drafted 2026-06-29. Supersedes and expands ROADMAP Phase 6 "In-season pipeline hardening" (P0). Status: proposal, not yet implemented.*

The entire live prediction/projection product rides on a nightly pipeline that **does not exist as a hardened, scheduled service**. Today everything is manual CLI. This doc scopes the work to make the site keep itself current through a season — and to prove it works *before* opening night rather than discovering a regression live.

## Goals

1. **Audit complete** — every model/metric input is enumerated with its refresh cadence, and the nightly job refreshes *everything the served product depends on* (no silent staleness).
2. **Connectivity hardened** — every external API has explicit timeouts, startup/preflight health checks, expiry detection (the 247 JWT is the live risk), and fail-soft isolation so one dead feed doesn't kill the run.
3. **Automated fetch** — a scheduled nightly job keeps prod current end-to-end, idempotent and restart-safe, with per-run observability and failure alerting.
4. **Simulatable** — we can replay arbitrary days/weeks of a season offline (from committed CSV fixtures, no live API) to test the whole pipeline out of season and in CI.

## Current state (from the 2026-06-29 audit)

| Pillar | State |
|---|---|
| Incremental ingest | ✅ `update --from X --to Y` exists, idempotent (upsert), runs `compute_all` at the end. Lineups have a restart ledger. |
| Compute pipeline | ✅ 19-step `compute_all`, season-scoped, idempotent, stateless. |
| NatStat connectivity | 🟡 Robust: retry+backoff, v3 fallback, 6h cache. Gaps: no explicit HTTP timeout, v3 fallback masks v4 outages silently, no health check. |
| 247 (transfers/recruits) | 🔴 JWT/cookie expires ~6h, **no renewal, no cache, hard-fails on 401**. Single biggest connectivity risk. |
| Torvik / coachdict | 🟡 Public, no auth. No retry, no rate limit, positional CSV parse is schema-fragile. |
| Scheduling | 🔴 **None.** No cron, no GitHub Actions ingest, no Railway scheduled job, no Procfile. |
| Observability | 🔴 `status` shows only a local token count. No feed-freshness tracking, no alerting, no `ingest_runs` ledger. |
| Simulation | 🔴 Clock read directly (`current_natstat_season()`, predict future-check) — not injectable. `as_of_date` time-travel works only for pit-CamPom. `measure_blend_accuracy` already does a chronological game replay (reusable seed). CSV fixtures `data/natstat_csv/` cover 2015–2025. |

## The freshness gap — what actually goes stale in-season

This is the central finding. `update` today refreshes the box-score chain but **not** the other model inputs. The nightly job must cover the full "served-critical" set or predictions silently rot.

| Input / job | Source | Feeds | In `update` today? | Required cadence | Where it runs |
|---|---|---|---|---|---|
| games / player_game_stats / team_game_stats | NatStat games/perfs/teamperfs | AdjEM, four factors, season stats, margin/win/total models | ✅ yes | **Nightly** | Railway cron |
| `compute_all` (season stats, AdjEM, rolling, percentiles, CamPom) | derived | every model + every page | ✅ yes | **Nightly** | Railway cron |
| **torvik_player_stats + torvik_player_game_stats** | barttorvik advgames | **CamPom v3 (cam_gbpm_*), pit_cam_v3 serving model, roster aggregates** | 🔴 **NO** | **Nightly** (`torvik --persist-games`) | Railway cron |
| team_season_stats.elo_rating / elo_rank | NatStat `/elo` | **`diff_elo_rating` served feature** (sole writer) | 🔴 no | **Nightly** | Railway cron |
| game_forecasts | NatStat forecasts | per-game ELO / win exp / lines | 🔴 no | **Nightly** | Railway cron |
| transfers | 247 portal (JWT) | next-season roster projection | 🔴 no | **In portal windows** (fail-soft) | Local or Railway w/ JWT |
| recruits | 247 composite (cookie) | freshman model, roster projection | 🔴 no | **Weekly in offseason** | Local |
| coaches (coachdict) | barttorvik | CAE (display) | 🔴 no | **Weekly** | Railway cron |
| lineups capture + PBP | NatStat lineups/playbyplay | RAPM, lineup pages (display) | 🔴 no | **Weekly**, local | Local |
| archetypes retrain | Python k-means | archetype labels (display) | 🔴 no | **Monthly / on-demand** | Local |
| RAPM refit | Python ridge | adj on/off (display) | 🔴 no | **Monthly / on-demand** | Local |
| model retrains (margin/win/total/proj) | Python LightGBM | all serving | 🔴 no | **Offseason only** | Local |

**The critical bug today:** `update` runs `compute_campom` which recomputes `cam_gbpm_v3` *from `torvik_player_stats`* — but `update` never refreshes Torvik. So a nightly `update` recomputes CamPom from **stale Torvik**, and the `pit_cam_v3` serving model (fed by `torvik_player_game_stats`) goes stale even though NatStat games ingest fine. **Folding `torvik --persist-games` into the nightly run is the single highest-value fix.**

## Recommended runtime architecture

There's a real fork here (see Open Decisions). Recommendation:

- **Railway scheduled job** (same Docker image as the API, shares the Postgres plugin + `NATSTAT_API_KEY`) runs the **serving-critical nightly set** *directly against prod*: games/perfs/teamperfs → torvik → forecasts → `compute_all`. ~04:30 ET, after NatStat's ~3 AM re-tabulation. Budget: peak March Saturday ≈ 200 game-detail calls + pagination ≈ 40–60% of the 2500/hr ceiling — comfortable.
- **Local machine** keeps the **heavy/non-served-critical jobs** on a looser cadence (PBP + lineups capture, RAPM refit, archetype retrain, model retrains, recruits) and pushes *only their derived tables* to prod via a **targeted, table-scoped** `sync_to_prod.sh --tables ...` (today's script TRUNCATEs everything, which would clobber the Railway-written tables — this is a required change for the split to be coherent).
- **transfers**: in portal windows, run wherever the JWT can be kept fresh — likely local (manual JWT capture). Fail-soft; never blocks the serving nightly.

This keeps the serving path robust and always-on (Railway), while the laptop-bound heavy compute stays off the critical path. The alternative (everything local + full nightly sync) is simpler but needs an always-on local host and runs a nightly full-truncate write-lock on prod.

---

## Phase 0 — Close the data-input freshness gaps

*Make the nightly job refresh everything the served product reads. Prerequisite for everything else — automating a job that fetches an incomplete set just automates staleness.*

- **0.1 — Fold Torvik into the nightly path.** Add `torvik --persist-games` to the `update` orchestration (or a new `nightly` command, see 2.1). One HTTP call (`barttorvik.com/{year}_all_advgames.json.gz`, ~30s, no NatStat budget). This unblocks fresh `cam_gbpm_v3` + `pit_cam_v3`. *(ROADMAP Phase 6 already specs this — implement it.)*
- **0.2 — Fold ELO/forecasts into the nightly path.** Add the `forecasts` step. Must **not** fail the whole run on an empty payload (off-season / pre-tabulation) — fail-soft.
- **0.3 — Decide transfers/recruits/coaches cadence** and wire them as **separate, fail-soft jobs** (not in the serving-critical chain). coaches = weekly (Railway, public). transfers = portal-window, JWT-gated (Phase 1.3). recruits = weekly offseason, local.
- **0.4 — Document the "what's recomputed nightly" contract** in `docs/` so it's auditable: a Tuesday stat correction silently propagates into Wednesday's rankings because CamPom/AdjEM/percentiles recompute every run.
- **Acceptance:** a single nightly invocation leaves *zero* served-critical input older than the run. Verified by a freshness assertion (Phase 2.3) and a diff test: run nightly twice on the same window → byte-identical derived tables (idempotency).

## Phase 1 — API connectivity hardening

*Make every external dependency degrade gracefully and surface its own health.*

- **1.1 — Explicit HTTP timeouts** on every client (`client.rs`, `tfs.rs`, `tfs_recruits.rs`, `torvik.rs`). Today NatStat + Torvik inherit reqwest's effectively-infinite default — a stalled socket hangs the run.
- **1.2 — Preflight health check command** (`cstat-ingest preflight` / `healthz`): pings each feed (NatStat known-season fetch, Torvik coachdict, 247 page-1 if JWT present, DB) and reports reachable/auth-valid/stale. The orchestrator runs this first and **skips + warns** on a dead feed rather than hard-failing the run.
- **1.3 — 247 JWT expiry handling (the live risk).** Decode the JWT `exp` claim (or peek page 1) at startup; if expired/absent, **skip the 247 step with a loud warning and keep the last snapshot** instead of crashing. Persist a `data/transfers/{year}_raw.json` snapshot on every successful run so stale-but-present beats empty. Add a runbook for manual re-capture. *(Longer-term: investigate a more durable 247 auth path.)*
- **1.4 — Surface the v3 fallback.** When NatStat silently downgrades v4→v3, emit a structured warning + a per-run flag in the `ingest_runs` ledger (Phase 2.2) so a prolonged v4 outage is visible, not masked.
- **1.5 — Torvik CSV schema guard.** The positional CSV parse misaligns silently if Bart inserts a column. Add a header/column-count assertion that fails loudly instead of writing garbage.
- **Acceptance:** killing any single feed (revoke JWT, block Torvik, 500 from NatStat v4) produces a clear preflight failure / fail-soft skip + alert, and the serving-critical chain still completes on the feeds that are up.

## Phase 2 — Automated fetch (scheduler + orchestrator + observability)

*The production wrapping. This is the heart of ROADMAP Phase 6.*

- **2.1 — A single canonical orchestrator command.** Wrap the serving-critical set behind one entry point (`cstat-ingest nightly [--date]`) that runs: preflight → games/perfs/teamperfs (`--from yesterday --to today`, to catch late corrections) → torvik --persist-games → forecasts → compute. Each step **error-isolated** (a 247/forecasts failure logs + continues; a games/compute failure fails the run). Re-runnable on the same date with no dup rows / no derived-stat corruption (audit + regression test the upsert paths end-to-end).
- **2.2 — `ingest_runs` ledger table** (migration): `run_id, started_at, ended_at, status, source, rows_touched, api_calls, v3_fallback, notes`. One row per step per run. This is both the audit trail and the freshness source.
- **2.3 — `GET /api/health/ingest` route** returning last-successful timestamp per data source (reads `ingest_runs`). Drives a status badge on the site and an external uptime monitor.
- **2.4 — Alerting hook.** On run failure or last-success >36h during season: Slack incoming-webhook (`INGEST_ALERT_WEBHOOK`). Cheap and load-bearing — today a failed run leaves stale data silently.
- **2.5 — Rate-budget headroom logging.** Log per-run tokens consumed; warn if >80% of the daily budget (March-Saturday safety).
- **2.6 — Schedule it.** Railway scheduled job on the API image, ~04:30 ET. Plus the looser-cadence jobs (coaches weekly; local heavy jobs on their own crons + targeted sync).
- **2.7 — Edge cache coherence.** After the nightly compute (+ targeted sync), the 5-min `Cache-Control` TTL we just shipped means freshness lands within minutes automatically; optionally add a Cloudflare cache-purge call at the end of the run for instant propagation. (Ties into the edge-caching work already shipped.)
- **2.8 — Targeted `sync_to_prod.sh --tables`.** Required for the Railway-direct architecture: let the local heavy jobs push only their derived tables without truncating the Railway-written serving tables.
- **Acceptance:** the job runs unattended overnight, `/api/health/ingest` shows fresh timestamps, a forced failure fires an alert, and a re-run on the same date is a no-op on row counts.

## Phase 3 — Season simulation & out-of-season test harness

*Prove the whole pipeline before opening night, and keep it regression-tested in CI — using committed fixtures, no live API, no rate budget.*

- **3.1 — Make the clock injectable.** Introduce a single `now()`/season indirection honoring an env override (e.g. `CSTAT_SIMULATED_DATE`) in the two places that read the wall clock: `current_natstat_season()` (`lib.rs:28`) and the predict future-check (`predict.rs:85`). Low-risk, unlocks "run the pipeline as if today is 2025-12-15."
- **3.2 — A replay/simulation driver** (`cstat-ingest simulate --from --to [--step daily|weekly]`): against a CSV-bootstrapped historical season (`data/natstat_csv/`, offline), iterate dates and run the nightly orchestrator scoped to each window, asserting it completes and the derived tables stay invariant-clean. Reuses the chronological loop already in `measure_blend_accuracy.rs:164`. This is the out-of-season confidence check — run weekly between now and October against several window shapes (single-game day, 100-game Saturday, postponement day, conference-tournament day, zero-game day).
- **3.3 — Synthetic API fixtures for CI** (ROADMAP Approach C): fixed NatStat-endpoint payload fixtures replayed through the ingest path in `crates/cstat-ingest/tests/`, run on every push. Highest-value ongoing protection — catches API-shape regressions without burning rate budget. The `api_cache` table + `data/natstat_csv/` give a starting corpus.
- **3.4 — Edge-case smoke tests** (from Phase 6's list): postponed→final status flip overwrites scores; cancelled game writes no phantom row; mid-season player add creates no FK violation; empty `/elo` or `/forecasts` payload doesn't fail the run; conference re-class recomputes `is_conference`.
- **Acceptance:** `simulate` replays a full historical month offline with zero crashes and clean invariants; the synthetic-fixture test runs green in CI; the edge cases are covered.

## Phase 4 — Data-quality gates, runbook (the "anything else I'd recommend")

- **4.1 — Post-run invariant gates.** After each nightly compute, assert data-quality invariants (no team with games but NULL AdjEM, no orphan games, row-count sanity vs prior run, the existing swapped-game/phantom invariants already in `tests/swapped_games.rs`). Fail-soft → alert, don't silently serve corrupt derived stats.
- **4.2 — Backfill-gap self-heal.** Detect a skipped night (last-success > expected) and auto-widen the date window on the next run so a missed day heals without manual intervention.
- **4.3 — Secret/JWT rotation runbook + first-day-of-season checklist** (the manual opening-night verification from Phase 6: confirm overnight run, check `/api/health/ingest`, inspect a fresh box score, spot-check predict on opening-night games — with the thin-sample `season` fallback during the first ~2 weeks before AdjEM converges).
- **4.4 — Calibration drift monitor (stretch).** Periodically score served predictions vs realized outcomes (extend `measure_blend_accuracy`) and alert on MAE/AUC drift — early warning that a model needs an offseason retrain.

---

## Sequencing & milestones

1. **M1 — Complete + correct nightly (Phase 0 + 1.1/1.5 + 2.1/2.2).** The job exists, fetches the full served-critical set, is idempotent, and records runs. *Highest value; unblocks everything.* **— core SHIPPED 2026-06-29** (see *M1 status* below).
2. **M2 — Scheduled + observable (Phase 2.3–2.8).** It runs unattended on Railway with health endpoint + alerting + edge coherence. **— code SHIPPED 2026-06-29** (see *M2 status* below); only the Railway dashboard wiring is operator-side.
3. **M3 — Connectivity fail-soft (Phase 1.2–1.4).** Preflight + JWT expiry handling + v3-fallback visibility.
4. **M4 — Simulatable + CI-protected (Phase 3).** Clock injection + replay driver + synthetic fixtures. *Run M4's replay weekly from now until tipoff.*
5. **M5 — Quality gates + runbook (Phase 4).** Invariant gates, self-heal, opening-night checklist.

M1–M4 should all land **before October 2026** — the pipeline can't be first-run on opening night.

## M1 status — core shipped 2026-06-29

The correctness fix + ledger + CLI landed and were verified live against the local DB:

- **`cstat-ingest nightly [--year] [--from] [--to] [--no-compute]`** — new orchestrator (`SeasonIngester::nightly`, `crates/cstat-ingest/src/ingest/season.rs`). Runs games → player_perfs → team_perfs (load-bearing, hard-fail) → forecasts → **ELO ratings** → torvik → torvik per-game persist + rebounds (best-effort, fail-soft) → `compute_all` (hard-fail). Defaults the window to yesterday..today (UTC). **Fixes the stale-CamPom bug**: Torvik refreshes *before* compute, so `cam_gbpm_v3` and the `pit_cam_v3` serving input (`torvik_player_game_stats`) stay fresh. The `/elo` step is the sole writer of `team_season_stats.elo_rating` (the served `diff_elo_rating` feature) — `compute` never touches it, so it too would go stale without nightly refresh. `teams`/`team_details` are deliberately omitted (reference data / W-L overwritten by compute / conference static in-season).
- **`ingest_runs` ledger** (migration `039_ingest_runs.sql`, module `crates/cstat-ingest/src/run_ledger.rs`) — one row per step per run, with status / rows_touched / timing / error. Excluded from `sync_to_prod.sh` (runtime-written on prod). This is the data source for the future `/api/health/ingest` route + staleness alerting (M2).
- **Verified**: off-season smoke run refreshed 5,695 forecasts + 4,978 Torvik players + 113,882 per-game rows; all 6 ledger steps recorded `ok` with per-step timings. fmt/clippy clean, ledger unit test green.

**Still open in M1's phase group (rolled into M2/M3):** explicit HTTP timeouts (1.1), Torvik CSV schema guard (1.5), and the full e2e replay idempotency test (lands with the Phase 3 synthetic-fixture harness — idempotency today rests on the `ON CONFLICT` upsert paths `update` already uses).

## M2 status — code shipped 2026-06-29

The scheduling/observability/alerting layer landed; everything is code except the
Railway dashboard wiring (documented in `docs/deploy_nightly_cron.md`).

- **`GET /api/health/ingest`** (2.3) — `crates/cstat-api/src/routes/health.rs`, mounted un-guarded next to `/api/health`. Per-step last-OK timestamp + last status from `ingest_runs`, an overall `healthy`/`stale` verdict (any served-critical step >36h or never-run → stale), and **HTTP 503 when stale** so an external uptime monitor flips red without parsing the body. `forecasts` is excluded from the staleness gate (legitimately empty off-season).
- **Slack notifications** (2.4) — `crates/cstat-ingest/src/notify.rs` (`post_slack`, fail-soft) wired into `SeasonIngester::nightly`, one post per run: a `:white_check_mark:` **success heartbeat** (with games/perfs/ELO/forecasts/Torvik/compute counts + remaining rate budget — also confirms the cron fired) on a clean run, a `:warning:` **degraded** post when the run completes but a best-effort feed failed (or budget low), and a `:rotating_light:` **critical** alert when a load-bearing step aborts the run. Routing is **per-channel** via a `SlackChannel` registry (a Slack webhook is bound to one channel): nightly posts to `#cron-job-alerts` via `SLACK_WEBHOOK_CRON` (legacy `INGEST_ALERT_WEBHOOK` honoured); future buckets (`#errors-api`, `#errors-web`) add a variant + `SLACK_WEBHOOK_*` var. Unset = log-only.
- **Rate-budget headroom** (2.5) — nightly snapshots `rate_limit_remaining()` before/after, logs both net drawdown and remaining headroom vs `NATSTAT_MAX_PER_HOUR`, and warns + adds a degraded-alert line when drawdown ≥80% *or* remaining ≤20% (the bucket refills mid-run, so remaining is the load-bearing signal; an exact per-call count would need the ledger's unused `api_calls` column — deferred).
- **Edge cache purge** (2.7) — `notify::purge_edge_cache` (fail-soft) fires after a successful compute when `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` are set; otherwise the 5-min `Cache-Control` TTL alone keeps the site fresh within minutes.
- **Targeted sync** (2.8) — `scripts/sync_to_prod.sh --tables a,b,c` restricts the dump/TRUNCATE/restore to a validated subset, the required primitive for the Railway-direct split (local heavy jobs push only their derived tables without truncating the cron-written serving tables).
- **Schedule** (2.6) — operator step, not code: runbook in `docs/deploy_nightly_cron.md` (Railway cron service on the API image, `cstat-ingest nightly`, ~09:30 UTC ≈ 04:30 ET, shared env).

**Still open (rolled into M3+):** the in-process >36h staleness *alert* is delegated to an external monitor polling `/api/health/ingest` (the nightly can't alert about a night it didn't run); a Railway healthcheck/native cron-failure hook is the operator's to wire. Connectivity fail-soft (preflight, JWT expiry, v3-fallback visibility) remains M3.

### Production lesson — DB connection latency vs. per-row loops (2026-06-30)

First live cron runs stalled: the ledger recorded games/perfs/team_perfs and then nothing for tens of minutes. Not a hang — the nightly has several **per-row DB loops** that are fine on a localhost DB (sub-ms/round-trip) but pathological over a high-latency connection. The cron was on the **public proxy URL, cross-region** (~85 ms/round-trip), and a step's thousands-to-100k sequential round-trips ballooned to tens of minutes / hours. Offenders by volume: `torvik_games` upsert (~113k rows, still per-row), `forecasts` (~5.6k, ~5 queries each = ~28k), `elo` (~728).

- **Operational fix (required, the real unblock):** cron `DATABASE_URL` = the **private** in-region Postgres URL, cron co-located with Postgres. Documented in `docs/deploy_nightly_cron.md`.
- **Code fixes shipped — all three big ingest N+1s batched:** (1) `ingest_game_forecasts` prefetches teams + games into maps, dedups by `game_id`, batches the upsert — ~28k queries → ~10 (`crates/cstat-ingest/src/ingest/elo.rs`, #158); (2) `apply_persist_torvik_game_stats` stages + dedups by `(pid, game_uid)` and batches the 36-column upsert — ~113k queries → ~115; (3) `apply_rebound_backfill` stages + dedups by `(player_id, game_date)` and batches the UPDATE as `UPDATE … FROM (VALUES …)` — ~113k UPDATEs → ~10 (both in `crates/cstat-ingest/src/ingest/torvik.rs`). The torvik per-game persist *and* rebound backfill were EACH a ~5-10 min step over the prod connection (not the ~95s first estimated — the loops iterate over all ~113k torvik rows). All verified identical output + idempotent against the local DB. First full prod run after the private-URL fix + (2)/(3): all 8 ledger steps `ok`, `/api/health/ingest` healthy.
- **Follow-up (M3 hardening):** the `elo` step is still N+1 (~728) but small (~4s in-region). Audit any remaining ingest loops; add explicit HTTP/statement timeouts so a genuine stall self-aborts.

## M3 status — connectivity fail-soft shipped 2026-07-01

Every external feed now degrades gracefully; a single dead dependency can't hang or silently corrupt the run.

- **Explicit HTTP timeouts (1.1)** — NatStat (`client.rs`: 10s connect / 60s request) and Torvik (`torvik.rs`: 10s connect / 120s request, looser for the multi-MB per-game gzip) now have hard timeouts instead of reqwest's effectively-infinite default. A NatStat timeout still fails over to v3 in `get_inner`. (`tfs.rs` / `tfs_recruits.rs` already carried a 30s timeout.)
- **Preflight command (1.2)** — `cstat-ingest preflight [--year] [--strict]` (`crates/cstat-ingest/src/preflight.rs`) probes the feeds and prints a per-feed reachable/skipped/down table: Postgres (`SELECT 1`), NatStat (cheap cached `teamcodes` fetch = reachable + auth-valid), Torvik (lightweight `coachdict.json` status probe), and — for the standalone command only — 247 (page-1 auth peek, if a JWT is configured). Exits non-zero when a **serving-critical** feed (DB/NatStat/Torvik) is down, or with `--strict` when *any* feed (incl. 247) is down. The `run(…, include_tfs)` flag gates the 247 probe. The nightly orchestrator runs it as step 0 with **`include_tfs = false`** — 247 is offseason roster-construction, never in the nightly chain, so probing it nightly would just burn a 247 call on an already-expired in-season token. It logs the per-feed summary and records a `preflight` ledger row, but does **not** gate control flow — the existing per-step isolation already fail-softs best-effort feeds and hard-fails the serving-critical chain; a down critical feed just lands in the degraded Slack summary.
- **247 JWT expiry handling (1.3)** — the #1 connectivity risk. `TfsClient::probe_auth` classifies a page-1 fetch as `Valid` / `Expired` (401/403) / `Unreachable` (the dependency-free "peek page 1" option, chosen over decoding the `exp` claim to avoid a base64/JWT dep and because the peek is ground truth). The `transfers` command no longer hard-fails on an expired token: it logs a loud warning and **falls back to the last committed `data/transfers/{year}_raw.json` snapshot** (stale-but-present beats empty), aborting with an actionable message only if no snapshot exists. Re-capture runbook: `docs/247_jwt_recapture.md`.
- **v4→v3 fallback visibility (1.4)** — `NatStatClient::used_v3_fallback()` exposes the previously-silent downgrade. The nightly reads it after the run and, when v4 fell back, records a `natstat_v4` ledger row (`failed`) + a degraded Slack line, so a prolonged v4 outage is visible instead of masked by the transparent v3 serve. (`natstat_v4` is deliberately **not** in the health route's `SERVED_CRITICAL` set — it's informational and won't flip `/api/health/ingest` to 503.)
- **Torvik CSV schema guard (1.5)** — `validate_player_csv_schema` runs once on the first data row of the headerless positional CSV, asserting the *type shape* of load-bearing columns with two signal tiers tuned to avoid false-firing on a stray value: **strong** — a numeric value in a text slot (name/team/conf), where even one is drift (a real name is never a bare number); **weak** — non-numeric text in a numeric slot (gp/usage/pid/gbpm), where a lone one could be a sentinel so only **2+** count. If Bart inserts/reorders a column the row misaligns and the parse **fails loudly** instead of writing garbage; a stray `"N/A"` is tolerated exactly as the per-field parse already does. Covered by `parse_csv_rejects_shifted_schema` + `parse_csv_tolerates_lone_nonnumeric_sentinel`.

**Acceptance met:** revoke the JWT → `transfers` falls back to snapshot + `preflight` reports `247 down`; block Torvik / 500 from NatStat v4 → preflight `down` / v3-fallback ledger row + degraded alert, and the serving-critical chain still completes on the feeds that are up. Remaining before opening night: M4 (offline replay + CI fixtures) and M5 (quality gates + runbook).

## Decisions (resolved 2026-06-29)

1. **Runtime split** — ✅ **Railway-direct serving nightly + targeted local sync.** The serving-critical nightly runs on Railway against prod; heavy local jobs push only their tables via a new `sync_to_prod.sh --tables` mode (Phase 2.8 is now a required dependency of the split, not optional).
2. **Alerting channel** — ✅ **Slack webhooks, per-channel.** Each channel = its own incoming-webhook URL in its own `SLACK_WEBHOOK_*` env var (a webhook is bound to one channel), registered in `notify::SlackChannel`. Nightly → `#cron-job-alerts` (`SLACK_WEBHOOK_CRON`). No infra.
3. **Build order** — ✅ **M1 first** (complete + correct nightly: fold Torvik/forecasts into one idempotent orchestrator + `ingest_runs` ledger; fixes the stale-CamPom bug).

## Still open (defaulted, revisit if it bites)

- **transfers/recruits in-season cadence** — how live next-season projections need to be *during* the current season. Default: JWT-gated 247 runs only in portal windows, fail-soft.
- **247 auth durability** — default: accept manual ~6h JWT recapture + fail-soft for v1; revisit if it bites.
