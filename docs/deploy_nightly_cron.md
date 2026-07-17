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
preflight   (connectivity probe — records a step, never gates control flow)
games → player_perfs → team_perfs   (load-bearing — a failure aborts the run)
forecasts → elo → torvik → torvik_games   (best-effort — logged, run continues)
compute_all   (load-bearing)
invariants → row_counts   (post-compute quality gates — degrade, never abort)
```

Window defaults to **yesterday..today (UTC)** so NatStat's overnight stat
corrections are picked up. Torvik + `/elo` refresh **before** `compute_all`, so
`cam_gbpm_v3` / `pit_cam_v3` and the served `diff_elo_rating` feature don't
recompute from stale inputs (the M1 correctness fix).

**Post-compute quality gates (M5).** After a successful compute the run adds two
non-fatal gates — the served-critical chain already finished, so a gate failure
routes to the DEGRADED Slack summary rather than aborting:

- **`invariants`** — `cstat_core::invariants::check_season` (the same set the
  offline `simulate` harness runs): teams with games but NULL AdjEM, completed
  games missing a `team_game_stats` side, W-L drift, and both swapped-game
  detectors. `Error`-severity violations degrade the run; `Warning`s (source-data
  holes the pipeline faithfully reflects) only log.
- **`row_counts`** — snapshots the season-scoped row count of each served table
  (`games`, `team_game_stats`, `player_game_stats`, `team_season_stats`,
  `player_season_stats`) and compares against the prior run's snapshot in
  `ingest_run_table_counts`. In-season these only grow, so a material shrink
  (>5% **and** >25 rows) means a truncated feed or a compute that wiped rows —
  degrades the run. A **regressed snapshot is deliberately not persisted**, so
  the baseline stays at the last known-good and the gate re-fires every night
  until the counts recover; only a clean snapshot becomes the new baseline.
  If a *deliberate* shrink (e.g. an off-season rebuild pushed by
  `sync_to_prod.sh`) leaves the gate wedged — off-season counts never grow back,
  so it would degrade every night until November — rebaseline explicitly:
  `DELETE FROM ingest_run_table_counts WHERE season = <season>;` on prod, and the
  next run records a fresh baseline.

**Backfill-gap self-heal (M5).** When the window is the default (no explicit
`--from`), the run scans the ledger for the **earliest game date it has not
fully ingested**, looking back 30 days, and widens the window start to it. Every
run stamps the window it covered onto its step rows, so this is a real coverage
scan rather than a guess — the re-covered dates are a harmless idempotent
overlap. A fully-healed run stays a SUCCESS and notes the widening in its Slack
summary. An operator-supplied `--from` is never auto-widened.

A date counts as covered only once **all three** box-score steps
(`games` → `player_perfs` → `team_perfs`) succeeded for some run. `games`
records `ok` before `player_perfs` can abort the run, so a run that died
half-way must not mark its window covered — otherwise its games would sit there
with final scores and no statlines, and no later run would fix them.

Because it scans for gaps rather than tracking a high-water mark, a manual
backfill can't hide an older hole behind it: `--from 11-07 --to 11-08` run on
11-08 still leaves 11-06 visible, and the next nightly heals it. Poking at a
broken cron is safe.

The widening is **capped at 14 days** so a long off-season silence can't trigger
a months-wide NatStat pull. When an outage runs past that cap the heal is only
*partial* — the dates between the gap start and the capped window start are
**not** re-ingested, and once they fall outside the 30-day lookback nothing will
pick them up. That case **degrades the run** with a `self-heal only PARTIAL`
line naming the exact backfill to run, e.g.:

```
cstat-ingest nightly --year 2027 --from 2026-11-11 --to 2026-11-17
```

Run it (an explicit `--from` is never auto-widened, so it does exactly that
range), then confirm the next nightly is green. Until you do, the run will keep
degrading nightly and re-pulling the capped window — that noise is deliberate:
it's a real hole in the served box scores.

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

**Verifying the pipeline stays healthy** — `GET /api/alert-selftest?channel=api|web`
posts a labelled synthetic message to the chosen error channel on demand, so you
can confirm alerting works without waiting for a real fault (important once known
bugs are fixed). Token-gated by **`ALERT_SELFTEST_TOKEN`** on the API service; the
token goes in the **`X-Selftest-Token` header** (not the URL, so it isn't logged),
compared in constant time. Returns **404** when the token is unset or wrong (so
the endpoint isn't discoverable), and always **200** on an authorized call with a
JSON body — assert on **`posted: true`** (it reflects whether the Slack POST
actually landed, not just that the webhook var is set; `webhook_configured` and
`detail` disambiguate a miss). Example:

```bash
curl -H "X-Selftest-Token: $ALERT_SELFTEST_TOKEN" \
  "https://campom.org/api/alert-selftest?channel=api"
# → {"posted":true,"channel":"errors-api","webhook_configured":true,"detail":"sent"}
```

Point a scheduled check at it (e.g. weekly, asserting `posted:true`) for
continuous assurance.

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

## Secret & token rotation

Every secret the nightly and API depend on, where it lives, and how to rotate it.
All are set as Railway service variables (cron service unless noted); rotating one
is: update the value → redeploy that service → verify.

| Secret | Service | Rotation cadence | Runbook |
| --- | --- | --- | --- |
| `NATSTAT_API_KEY` | cron + API | On provider reissue only | Paste the new key, redeploy. A bad key hard-aborts the nightly (step `games`) with a Slack alert. |
| `TFS_247_JWT` | (transfers job only) | ~6–12h expiry — offseason roster work | **Auto-fetched guest token by default**; only needed for subscriber-only fields. See [`247_jwt_recapture.md`](247_jwt_recapture.md). Fail-soft: an expired token skips 247 and keeps the last snapshot. |
| `SLACK_WEBHOOK_CRON` | cron | On channel/webhook reissue | Recreate the incoming webhook for `#cron-job-alerts`, paste, redeploy. Unset = silent (fail-soft). |
| `SLACK_WEBHOOK_ERRORS_API` / `_WEB` | API | On channel/webhook reissue | Same, for `#errors-api` / `#errors-web`. |
| `ALERT_SELFTEST_TOKEN` | API | On demand | Rotate the value, redeploy, re-verify with `GET /api/alert-selftest`. |
| `HEARTBEAT_URL` | cron | On monitor reissue | Update the dead-man's-switch URL, redeploy. |
| `CF_ZONE_ID` / `CF_CACHE_PURGE_TOKEN` | cron | On Cloudflare token reissue | Update both, redeploy. Unset = the 5-min edge TTL handles freshness. |

**Operator hazard — `CSTAT_SIMULATED_DATE` must be UNSET on the cron and API
services.** A lingering value pins the serving clock: the nightly window freezes
on one past date while every monitor stays green. The nightly marks itself
degraded when it sees the var, but the safe posture is to never set it on a real
service (it's for local `simulate`/testing only).

## First-day-of-season checklist (opening week)

Run through this the morning after the season's first slate of games.

1. **Cron fired and finished.** Railway logs show `nightly ingestion complete`;
   `#cron-job-alerts` has a green `Nightly ingest OK` post (or a DEGRADED post you
   understand — a self-heal note or an empty-off-season feed is benign).
2. **Health route is green.** `curl https://<host>/api/health/ingest` →
   `healthy: true` with fresh (`< 36h`) `last_ok_at` on every served-critical
   step. A `503` means a step is stale — check which and why.
3. **Fresh data landed.** Spot-check an opening-night box score in the DB/UI and
   confirm `games`/`player_perfs` counts on the run are non-zero for a night that
   had games (the empty-box heuristic would have degraded the run otherwise).
4. **Predict works on live games.** `GET /api/predict?home=…&away=…` on an
   opening-night matchup returns a margin + win prob. Expect the thin-sample
   `preseason`/`blended` regime for the first ~2 weeks before AdjEM converges —
   check `prediction_basis` is one of those, not `leaky`.
5. **Quality gates are clean.** In `ingest_runs`, the run's `invariants` and
   `row_counts` steps are `ok`. A `row_counts` failure on the *first* in-season
   run is expected-absent (no prior snapshot to compare) — it just records the
   baseline; the gate engages from the second run on.
6. **Alerting is live.** Confirm the Slack pipeline end-to-end with
   `GET /api/alert-selftest?channel=api` (`X-Selftest-Token` header) →
   `{"posted":true}`. Optionally force one real failure (temporarily bad
   `NATSTAT_API_KEY` on a throwaway run) to see the abort alert, then restore.
7. **No leftover sim clock.** Grep the cron + API env for `CSTAT_SIMULATED_DATE`
   — it must be unset (see the hazard above).

**Weekly until tipoff:** keep running `cstat-ingest simulate --reset` against the
sim DB (`docs/in_season_ingest_plan.md`) so an ingest/compute regression surfaces
now, not on opening night.
