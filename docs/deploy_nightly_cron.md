# Deploying the in-season nightly ingest (Railway cron)

This is the operational runbook for **M2** of `docs/in_season_ingest_plan.md`:
running `cstat-ingest nightly` unattended on Railway, with health + alerting.

The serving-critical refresh runs **directly against prod** on Railway; the
heavy/local-only jobs stay on the laptop and push only their derived tables via
`scripts/sync_to_prod.sh --tables …` (see "Local heavy jobs" below).

## What runs nightly

`cstat-ingest nightly` (`SeasonIngester::nightly`) refreshes the full
served-critical input set in dependency order and records every step to the
`ingest_runs` ledger:

```
games → player_perfs → team_perfs   (load-bearing — a failure aborts the run)
forecasts → elo → torvik → torvik_games   (best-effort — logged, run continues)
compute_all   (load-bearing)
```

Window defaults to **yesterday..today (UTC)** so NatStat's overnight stat
corrections are picked up. Torvik + `/elo` refresh **before** `compute_all`, so
`cam_gbpm_v3` / `pit_cam_v3` and the served `diff_elo_rating` feature don't
recompute from stale inputs (the M1 correctness fix).

## Railway setup

The API image already ships the `cstat-ingest` binary (see `Dockerfile`), so the
cron service reuses it — no separate build.

> **STOP — do NOT add the cron schedule to the API service.** Railway runs a
> service's *start command* on the schedule, and the API's start command
> (`cstat-api`, from `railway.json`) is a never-exiting web server — so the cron
> would just relaunch the API and the ingest would never run (`ingest_runs`
> stays empty, you'll see only API request logs). Worse, Railway **strips the
> restart policy and serverless from any service that has a cron schedule** (it
> says so in the service settings), so scheduling the API turns the live site
> into a cron job with no auto-restart — a latent outage. The cron must be its
> own **separate service**. A cron command must *exit*; `cstat-ingest nightly`
> runs ~5 min and exits, `cstat-api` never does.

1. **Create a second service** in the same Railway project as the API, from the
   same repo/image. Railway calls this a *Cron* (scheduled) service. Keep the
   API service schedule-free so it stays always-on with its restart policy.
2. **Point it at its own config file** — Service → Settings → *Config-as-code*
   → set the config file path to **`/railway.cron.json`** (absolute path from the
   repo root, per Railway's docs: *"provide the absolute path to the file in your
   repository, for example `/backend/railway.toml`"*). This is the load-bearing
   step. Railway: *"Configuration defined in code will always override values from
   the dashboard"*, so the API's `railway.json` (`startCommand: cstat-api`,
   `healthcheckPath: /api/health`) would otherwise force the cron service to run
   the web server with a healthcheck it can never pass — typing a start command
   into the dashboard field does *not* override it. `/railway.cron.json` instead
   supplies:
   ```jsonc
   { "deploy": { "startCommand": "cstat-ingest nightly",
                 "cronSchedule": "30 9 * * *" } }   // no healthcheckPath
   ```
   so the cron service gets the right command, the schedule (09:30 UTC ≈ **04:30
   ET**, after NatStat's ~3 AM re-tabulation), and **no healthcheck** — all from
   code, nothing to hand-set per deploy.
3. **Shared variables** (reference the same Postgres plugin + key as the API):
   - `DATABASE_URL` — **use the PRIVATE Postgres URL** (`${{Postgres.DATABASE_PRIVATE_URL}}`
     / the `…railway.internal` host), **not** the public `…proxy.rlwy.net` one,
     and put the cron service in the **same region as Postgres** (private
     networking is per-region). This is load-bearing, not a nicety: the nightly
     runs several large per-row DB loops (e.g. `torvik_games` upserts ~113k rows
     one statement at a time, forecasts ~5.6k). At sub-millisecond in-region
     latency that's ~2 min; over the public proxy / cross-region (~85 ms/round-trip)
     the same loops take **hours**, and the run never finishes. (Symptom seen in
     practice: games/perfs/team_perfs record in the ledger, then nothing for
     tens of minutes — it's not hung, it's crawling through round-trips.)
   - `NATSTAT_API_KEY`
   - `NATSTAT_MAX_PER_HOUR` — `2500` on the API+ tier (matches the API service)
   - `SLACK_WEBHOOK_CRON` — Slack incoming-webhook URL for `#cron-job-alerts`
     (see Alerting; legacy name `INGEST_ALERT_WEBHOOK` still works)
   - `HEARTBEAT_URL` — optional dead-man's-switch ping (see below).
   - `MODEL_DIR` is **not** needed (the nightly job runs no ONNX inference).
   - `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` — optional, for instant edge purge.

The cron service shares the API's database, so migrations (incl. `039`
`ingest_runs`) are applied by the API at boot; the nightly binary just writes to
the table.

**Manual runs / backfill.** The nightly writes straight to the prod DB, so a
one-off catch-up is just the same command pointed at prod — no service
reconfiguration (a dashboard start command wouldn't override `/railway.cron.json`
anyway). Run it from your laptop against the prod `DATABASE_URL`, or via
`railway run` to borrow the service's env:

```bash
DATABASE_URL="$PROD_DATABASE_URL" NATSTAT_API_KEY=… \
  cargo run --bin cstat-ingest -- nightly --from 2026-11-08 --to 2026-11-10
# or, with the cron service's env injected by the Railway CLI:
railway run --service <cron-service> cstat-ingest nightly --from 2026-11-08 --to 2026-11-10
```

## Observability — `GET /api/health/ingest`

Reads `ingest_runs` and returns last-successful timestamp + last status per step,
plus an overall verdict:

```jsonc
{
  "status": "ok",            // "stale" if any served-critical step > 36h old
  "healthy": true,
  "stale_after_hours": 36,
  "last_run_at": "2026-11-09T09:34:12Z",
  "missing_critical_steps": [],
  "steps": [ { "step": "compute", "last_status": "ok",
              "last_ok_at": "…", "hours_since_ok": 6.1, "stale": false }, … ]
}
```

Returns **200** when healthy, **503** when stale — so an external uptime monitor
(UptimeRobot, BetterStack, etc.) flips red on a *missed* night without parsing
the body. Point a monitor at this URL; that monitor is what covers the "last
success > 36h" case, since the nightly process can only self-alert when it
actually runs. The endpoint is un-guarded (never load-shed). Do **not** wire it
as the API service's Railway healthcheck — a stale cron would then mark the API
unhealthy and restart it, coupling the live site's uptime to the ingest cadence.

## Dead-man's-switch heartbeat — `HEARTBEAT_URL`

`/api/health/ingest` catches a *stale* pipeline, but it only helps if something
polls it, and it can't distinguish "cron ran and did nothing" from "cron never
ran." The complementary signal is a **heartbeat**: the nightly pings
`HEARTBEAT_URL` (`notify::ping_heartbeat`) with these semantics —

- **success ping** (base URL) when the run **completes** its served-critical
  chain, *including a degraded run* — best-effort feed failures show up in
  `#cron-job-alerts` and shouldn't page the dead-man's-switch;
- **`…/fail`** (the healthchecks.io convention) only on a **hard abort**
  (`games`/`perfs`/`compute` failed), so the monitor pages immediately instead of
  waiting out its grace period.

Point it at a dead-man's-switch monitor (healthchecks.io, Cronitor, Better Stack
Heartbeats) set to expect a ping each morning; it pages on exactly two things: a
**missing** ping (the cron never ran — the one failure mode the in-run Slack
alerts structurally can't cover) or a **`/fail`** ping (the run aborted). No-op
when unset; fail-soft.

Pick one or both: `/api/health/ingest` needs an HTTP poller but reports per-step
detail; `HEARTBEAT_URL` needs no poller and directly catches a skipped run.

## Alerting (Slack)

`SLACK_WEBHOOK_CRON` is a Slack incoming-webhook URL for the `#cron-job-alerts`
channel (the legacy `INGEST_ALERT_WEBHOOK` name is still accepted as a fallback).
A Slack webhook is locked to one channel, so other subsystems route to their own
channels via their own `SLACK_WEBHOOK_*` vars — see the channel registry in
`crates/cstat-ingest/src/notify.rs` (`SlackChannel`). The nightly job posts
exactly one message per run:

- **Success** (`:white_check_mark:`) — a clean run, with a one-line summary
  (games / player perfs / team perfs / ELO / forecasts / Torvik / compute +
  remaining rate budget). This doubles as a heartbeat: seeing it confirms the
  cron fired and finished.
- **Degraded** (`:warning:`) — the run completed but a best-effort feed
  (forecasts / ELO / Torvik) failed, or rate-budget headroom got low. Lists each
  issue.
- **Critical** (`:rotating_light:`) — a load-bearing step (games / player_perfs
  / team_perfs / compute) failed and the run aborted.

Unset webhook → no posts (the message is still logged). Posts are fail-soft: a
Slack outage never affects the ingest. Create the webhook at
`api.slack.com/apps → Incoming Webhooks`. If the nightly success ping becomes
noise, mute the channel rather than unsetting the var — you still want the
degraded/critical posts.

### Error channels — `#errors-api` and `#errors-web`

Two error buckets are wired and set on the **API service** (not the cron):

- **`SLACK_WEBHOOK_ERRORS_API`** → `#errors-api`. Fires on a cstat-api
  **boot/serve failure** (bad `DATABASE_URL`, migration mismatch, missing
  `NATSTAT_API_KEY`, or an ONNX export whose meta drifted — otherwise a silent
  Railway crash-loop), on any **5xx** response, and on a **panic** in a handler.
  The 5xx tap and panic hook share one in-process throttle (one alert / 60s) so a
  crash loop can't flood the channel. Load-shed 503s and 408 timeouts are
  deliberate backpressure and are excluded.
- **`SLACK_WEBHOOK_ERRORS_WEB`** → `#errors-web`. Fires when the SPA's global
  `error` / `unhandledrejection` reporter (`web/src/lib/errorReporter.ts`) posts
  an uncaught browser error to `POST /api/client-error`, which relays it. Both
  the client and the server throttle/cap so a bad deploy hitting every visitor
  can't spam Slack. Untrusted fields are length-capped server-side.

Unset webhook → no posts (message still logged). Both are fail-soft.

**Verifying the pipeline stays healthy** — `GET /api/alert-selftest?token=…&channel=api|web`
posts a labelled synthetic message to the chosen error channel on demand, so you
can confirm alerting works without waiting for a real fault (important once known
bugs are fixed). Token-gated by **`ALERT_SELFTEST_TOKEN`** on the API service;
returns **404** when the token is unset or wrong (so the endpoint isn't
discoverable), and a JSON body with `webhook_configured` so a monitor can assert
the env var is actually set. Point a scheduled check at it (e.g. weekly) if you
want continuous assurance.

### Adding a further alert channel

Because a webhook is bound to one channel, a new bucket is a new webhook + a new
env var, registered in one place:

1. In Slack, create the channel and add an Incoming Webhook to it; copy the URL.
2. In `crates/cstat-ingest/src/notify.rs`, add a `SlackChannel` variant and map
   it to a `SLACK_WEBHOOK_*` env var in `SlackChannel::env_var` (the file's
   doc-comment spells this out).
3. Set that env var on whichever service produces those messages.
4. Call `notify::post_slack(SlackChannel::TheNewOne, &msg)` from the producer.

No central wiring to touch — the registry is the single source of truth.

## Rate-budget headroom

Each run logs both the net token drawdown and the **remaining** headroom vs
`NATSTAT_MAX_PER_HOUR`, and warns (adding a degraded-alert line) when drawdown is
≥80% *or* remaining is ≤20%. The bucket refills mid-run, so remaining is the
number to watch — it trends toward 0 only if calls are outpacing the refill. A
peak March Saturday is ~40–60% of the 2500/hr ceiling, so this is a safety
tripwire, not an expected condition.

## Local heavy jobs → targeted sync

The serving nightly writes prod directly, so the laptop must **not** run a full
`sync_to_prod.sh` (its `TRUNCATE` would clobber what the cron just wrote). Push
only the derived tables the heavy local jobs produce:

```bash
# after a local RAPM refit / lineup rebuild / archetype retrain:
./scripts/sync_to_prod.sh --tables lineup_aggregates,player_on_off,player_rapm
./scripts/sync_to_prod.sh --tables player_archetypes,archetype_models
```

`--tables` validates each name against the live non-excluded set and aborts on a
typo before any write. `ingest_runs` (and the other runtime/local-only tables)
can never be selected — they're in the script's `EXCLUDED` list. **Use it only
for leaf/derived tables**: the restore is `TRUNCATE … CASCADE`, so targeting a
*referenced* table (e.g. `teams`) would cascade-wipe its dependents on prod even
though they aren't in your list. The confirmation prompt flags this in targeted
mode.

## First-night checklist (opening week)

1. Confirm the cron service ran (Railway logs show "nightly ingestion complete").
2. `curl https://<host>/api/health/ingest` → `healthy: true`, fresh timestamps.
3. Spot-check a fresh box score and `GET /api/predict` on an opening-night game
   (expect the thin-sample `preseason`/`blended` regime for the first ~2 weeks
   before AdjEM converges — see `prediction_basis`).
4. Force a failure once (e.g. temporarily bad `NATSTAT_API_KEY` on a throwaway
   run) to confirm the Slack alert fires, then restore.
