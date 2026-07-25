# Torvik 403 (egress-IP block) runbook

barttorvik has no auth and no documented rate limit, but it **refuses requests
from Google IP space**. When the nightly's Railway container happens to egress
from a Google-owned address, every Torvik fetch 403s for that container's whole
lifetime — so the failures arrive in multi-day streaks and then vanish on a
redeploy, which is what made this look intermittent and unexplainable twice.

## Symptom

A DEGRADED nightly alert in `#cron-job-alerts` naming all three Torvik touch
points:

```
preflight: serving-critical feed(s) down: torvik
torvik: HTTP status client error (403 Forbidden) for url (https://barttorvik.com/getadvstats.php?year=2026&csv=1)
torvik_games: HTTP status client error (403 Forbidden) for url (https://barttorvik.com/2026_all_advgames.json.gz)
```

The run still completes: `compute_all` runs on the previous day's Torvik rows, so
`cam_gbpm_v3` / `pit_cam_v3` go one day stale rather than empty. `/api/health/ingest`
stays green until the 36h staleness threshold trips, i.e. two consecutive misses.

## Root cause

Bart blocked Google Apps Script after one was pulling a single file hundreds of
times per minute ([his data page](https://adamcwisports.blogspot.com/p/data.html),
2023-10-17: *"I blocked google apps scripts … Using a script to pull the data in
that file occasionally — even like once an hour — is certainly fine"*). Google's
published range list (`goog.json`) is a superset that includes GCP **customer**
ranges, so a Railway container placed on GCP is caught by a rule aimed at
somebody else. We are collateral damage, not the target: the nightly makes
**three** Torvik requests per day.

It is **not** a generic datacenter block and **not** rate limiting. Evidence from
the `nightly public egress IP` log line cross-referenced with each run's Torvik
verdict:

| run | egress IP | owner (rDAP) | in `goog.json` | torvik |
|---|---|---|---|---|
| 2026-07-19 | 152.55.180.28 | Railway (`RC-1550`) | no | ok |
| 2026-07-20…22 | 162.220.234.37 | Railway (`RLWY-METALGEN1-01`) | no | ok |
| 2026-07-23…24 | 34.233.31.38 | AWS EC2 | no | ok |
| 2026-07-25 | 35.245.179.150 | Google Cloud | **yes** (`35.240.0.0/13`) | **403** |

Two details rule out the alternative "AWS-managed reputation list" theory: a
hosting provider's own range (Railway Metal) passed, which such a list would
likely include; and our User-Agent is `cstat/0.1`, not an Apps Script UA, so if
his rule reaches us it can only reach us by IP.

barttorvik is fronted by **AWS CloudFront**, not Cloudflare. A refusal is
generated at the edge (`server: CloudFront`, no origin headers) and never reaches
Bart's Apache; a served response carries `server: Apache` plus `via: … CloudFront`.
Diagnostics in `torvik.rs::edge_block_error` log `x-amz-cf-id` (the request id
Bart or AWS can look up), `x-amz-cf-pop`, `x-cache`, `via`, `server`,
`retry-after`, and a body snippet — the body is what separates a WAF/IP rule from
a geo restriction. An earlier version logged `cf-ray`/`cf-mitigated`, which are
structurally always absent here; that is why the first investigation reached the
wrong conclusion.

## The fix: Railway static outbound IPs

Enabled on the **cron service only** (Pro plan, per-service; the API never calls
Torvik). Railway's static egress sits in **Railway-owned** address space, so it
sidesteps the underlying-provider lottery entirely rather than just picking a
better provider. As of 2026-07-25 the assigned set is:

| IP | range | owner |
|---|---|---|
| 162.220.234.241 | 162.220.232.0/22 (`RLWY-METALGEN1-01`) | Railway |
| 162.220.234.242 | 162.220.232.0/22 (`RLWY-METALGEN1-01`) | Railway |
| 152.55.180.240 | 152.55.176.0/20 (`RC-1550`) | Railway |

None are in `goog.json` or `cloud.json`, and both /24s appear in the evidence
table above as **observed serving 200s**. This is not inference from ownership.

Caveats:

- All three are marked **Shared** — Railway does not guarantee dedication. A
  co-tenant's abuse could taint one, and luck-based rotation is gone. That is what
  `TORVIK_PROXY_URL` remains for (see below).
- **The set is not stable across toggling.** The addresses shown when the feature
  was previewed differed from the ones actually assigned. Per Railway's docs they
  also change if the service moves region. Re-read them after any change, and
  refresh this table.
- `deploy.region` **does** exist in `railway.schema.json` (despite the
  config-as-code docs page omitting it) but is deliberately left unset: pinning a
  region would only choose a provider, which static IPs make moot.

## Verifying

The next nightly proves it for free — no request to barttorvik needed:

1. Read the `egress_ip` field off the `nightly public egress IP` log line, or off
   the `_egress IP …_` footer of any DEGRADED alert, or from the `preflight` row
   in `ingest_runs` (`error` column, on a critical-down run).
2. Confirm it is one of the three addresses above.
3. Confirm the `torvik` / `torvik_games` steps report `ok`.

To check the egress of any host without touching barttorvik:

```bash
cargo test -p cstat-ingest egress_ip_probe -- --ignored --nocapture
```

To check an address against Google's published ranges:

```bash
curl -s https://www.gstatic.com/ipranges/goog.json \
  | python3 -c "import json,sys,ipaddress; ip=ipaddress.ip_address(sys.argv[1]); \
print(any(ip in ipaddress.ip_network(p['ipv4Prefix']) for p in json.load(sys.stdin)['prefixes'] if 'ipv4Prefix' in p))" 35.245.179.150
```

## If it recurs

In order:

1. **Read the egress IP off the alert.** In Google space → static IPs stopped
   applying (a region move re-rolls them; the toggle needs a redeploy). Not in
   Google space → a different cause; read the logged `x-amz-cf-id` and body
   snippet before assuming this runbook applies.
2. **Contact Bart** — he explicitly invites it: *"If that happens to you and your
   aims were not malicious, let me know"* (DM `@totally_t_bomb`). Lead with the
   three static IPs, the 3-requests-per-day cadence, and the 09:30 UTC schedule.
   This is also the point at which the contactable-UA `TODO` in
   `torvik.rs::TorkvikClient::new` should be actioned — being identifiable is an
   asset in that conversation.
3. **Set `TORVIK_PROXY_URL`** on the cron service to route Torvik through a fixed
   non-Google egress. Fail-soft: unset means a direct connection.

**Do not add retries.** A 4xx is terminal by design
(`torvik.rs::is_client_error`): the block is IP-scoped, so a retry from the same
container hits the same refused address, and hammering a host that already said
no is exactly the behaviour that got Google IP space blocked in the first place.
The guard is covered by `with_retry_does_not_retry_client_errors`, which runs
offline in CI precisely so that verifying it costs Bart nothing.

## Deferred follow-up

An **expected-egress-IP assertion** — a preflight warning when `egress_ip` is not
in an `EXPECTED_EGRESS_IPS` allowlist. Now that egress is a known constant, the
live failure mode shifts from "which provider did we land on" to "did the static
IPs silently stop applying", which this would catch in one line with no external
dependency. Deferred until the static IPs have been observed holding across a few
nightlies; it needs the env var set on the cron service to do anything.
