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

1. **Create a second service** in the same Railway project as the API, from the
   same repo/image. Railway calls this a *Cron* (scheduled) service.
2. **Schedule:** `30 9 * * *` (cron is UTC on Railway → 09:30 UTC ≈ **04:30
   ET**, after NatStat's ~3 AM re-tabulation). Adjust for EST/EDT as desired;
   exactness doesn't matter, "a few hours after the games settle" does.
3. **Start command:**
   ```
   cstat-ingest nightly
   ```
   `--year` defaults to the current season (date-derived), and the window
   defaults to yesterday..today — no args needed. Override for a backfill:
   `cstat-ingest nightly --from 2026-11-08 --to 2026-11-10`.
4. **Shared variables** (reference the same Postgres plugin + key as the API):
   - `DATABASE_URL` — the prod Postgres connection string
   - `NATSTAT_API_KEY`
   - `NATSTAT_MAX_PER_HOUR` — `2500` on the API+ tier (matches the API service)
   - `INGEST_ALERT_WEBHOOK` — Slack incoming-webhook URL (see Alerting)
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

`INGEST_ALERT_WEBHOOK` is a Slack incoming-webhook URL. The nightly job posts:

- **Critical** (`:rotating_light:`) — a load-bearing step (games / player_perfs
  / team_perfs / compute) failed and the run aborted.
- **Degraded** (`:warning:`) — the run completed but a best-effort feed
  (forecasts / ELO / Torvik) failed, or it consumed ≥80% of the hourly rate
  budget. Lists each issue.

Unset webhook → no posts (the message is still logged). Alerts are fail-soft: a
Slack outage never affects the ingest. Create the webhook at
`api.slack.com/apps → Incoming Webhooks`.

## Rate-budget headroom

Each run logs net token drawdown vs `NATSTAT_MAX_PER_HOUR` and warns (and adds a
degraded-alert line) at ≥80%. A peak March Saturday is ~40–60% of the 2500/hr
ceiling, so this is a safety tripwire, not an expected condition.

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
can never be selected — they're in the script's `EXCLUDED` list.

## First-night checklist (opening week)

1. Confirm the cron service ran (Railway logs show "nightly ingestion complete").
2. `curl https://<host>/api/health/ingest` → `healthy: true`, fresh timestamps.
3. Spot-check a fresh box score and `GET /api/predict` on an opening-night game
   (expect the thin-sample `preseason`/`blended` regime for the first ~2 weeks
   before AdjEM converges — see `prediction_basis`).
4. Force a failure once (e.g. temporarily bad `NATSTAT_API_KEY` on a throwaway
   run) to confirm the Slack alert fires, then restore.
