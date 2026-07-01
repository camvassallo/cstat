# 247Sports JWT re-capture runbook

The transfer-portal and recruit feeds (`ipa.247sports.com`) are gated by a Bearer
JWT tied to a 247 subscription. **The token expires ~6 hours after issue with no
renewal path** — this is the single biggest connectivity risk in the pipeline. It
is deliberately kept *off* the serving-critical nightly chain (see
`docs/in_season_ingest_plan.md`); the ingest fails soft when the token is dead.

## What happens when the JWT expires

- `cstat-ingest transfers` no longer hard-fails. On a 401/403 it logs a loud
  warning and **falls back to the last committed snapshot** at
  `data/transfers/{year}_raw.json` (stale-but-present beats empty). If no snapshot
  exists for that year it aborts with an actionable error.
- `cstat-ingest preflight` reports the 247 feed as `expired` (it peeks page 1) but
  still exits 0 unless `--strict` is set — a dead 247 token is expected and must
  not gate the serving nightly.

## Re-capturing the token

1. Log in to <https://247sports.com> in a browser with an active subscription.
2. Open a transfer-portal or recruit-rankings page (e.g.
   `247sports.com/season/2026-basketball/transferportal/`).
3. Open DevTools → Network, filter to `ipa.247sports.com`, reload the page.
4. Pick a `transfers/` (or `compositerecruitrankings/`) XHR request.
   - **Transfers portal:** copy the `Authorization: Bearer <token>` request
     header value (the `<token>` part) into `TFS_247_JWT`.
   - **Recruit rankings:** these moved to a full session cookie — use
     *Copy → Copy as cURL*, take the whole `Cookie:` header, and set
     `TFS_247_COOKIE` (legacy `TFS_247_JWT` is still honored as a fallback).
5. Export the var and re-run:

   ```bash
   export TFS_247_JWT='eyJ...'          # transfers
   cargo run --bin cstat-ingest -- transfers --year 2026
   ```

## Refreshing the fallback snapshot

The committed `data/transfers/{year}_raw.json` files are the fail-soft source. To
refresh one after a successful live capture, save the raw paginated response for
the year (the curl loop documented in ROADMAP §5b "Bootstrap data") back to that
path and commit it. A newer snapshot means the fail-soft path serves fresher data
the next time the token lapses mid-window.
