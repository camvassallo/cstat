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
   (a.k.a. "Railway Config File") → set the path to **`railway.cron.json`**.
   This is the load-bearing step. **Config-as-code takes precedence over the
   dashboard**, so the API's `railway.json` (`startCommand: cstat-api`,
   `healthcheckPath: /api/health`) would otherwise force the cron service to run
   the web server with a healthcheck it can never pass — typing a start command
   into the dashboard field does *not* override `railway.json`. `railway.cron.json`
   instead supplies:
   ```jsonc
   { "deploy": { "startCommand": "cstat-ingest nightly",
                 "cronSchedule": "30 9 * * *" } }   // no healthcheckPath
   ```
   so the cron service gets the right command, the schedule (09:30 UTC ≈ **04:30
   ET**, after NatStat's ~3 AM re-tabulation), and **no healthcheck** — all from
   code, nothing to hand-set per deploy. For a one-off backfill, temporarily set
   a dashboard start command `cstat-ingest nightly --from 2026-11-08 --to 2026-11-10`
   (a *new* file isn't needed for a manual run).
4. **Shared variables** (reference the same Postgres plugin + key as the API):
   - `DATABASE_URL` — the prod Postgres connection string
   - `NATSTAT_API_KEY`
   - `NATSTAT_MAX_PER_HOUR` — `2500` on the API+ tier (matches the API service)
   - `SLACK_WEBHOOK_CRON` — Slack incoming-webhook URL for `#cron-job-alerts`
     (see Alerting; legacy name `INGEST_ALERT_WEBHOOK` still works)
   - `MODEL_DIR` is **not** needed (the nightly job runs no ONNX inference).
   - `CF_ZONE_ID` + `CF_CACHE_PURGE_TOKEN` — optional, for instant edge purge.

The cron service shares the API's database, so migrations (incl. `039`
`ingest_runs`) are applied by the API at boot; the nightly binary just writes to
the table.

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
(UptimeRobot, BetterStack, a Railway healthcheck, etc.) flips red on a *missed*
night without parsing the body. Point a monitor at this URL; that monitor is
what covers the "last success > 36h" case, since the nightly process can only
self-alert when it actually runs. The endpoint is un-guarded (never load-shed).

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

### Adding a new alert channel

Because a webhook is bound to one channel, a new bucket (e.g. `#errors-api`,
`#errors-web`) is a new webhook + a new env var, registered in one place:

1. In Slack, create the channel and add an Incoming Webhook to it; copy the URL.
2. In `crates/cstat-ingest/src/notify.rs`, add a `SlackChannel` variant and map
   it to a `SLACK_WEBHOOK_*` env var in `SlackChannel::env_var` (the file's
   doc-comment spells this out). Document the var in `.env.example`.
3. Set that env var on whichever service produces those messages (API errors →
   the API service, not the cron).
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
