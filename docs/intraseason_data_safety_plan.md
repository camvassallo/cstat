# Intraseason data-input safety: prod write flows + hardening plan

**Type:** hardening / operational-safety
**Area:** `scripts/sync_to_prod.sh`, `cstat-ingest nightly`, prod DB
**Motivation:** In-season, prod is written by two independent actors (the Railway nightly cron and the operator's laptop sync). Their compatibility today rests entirely on **operator discipline** — there is no code-level guard preventing a full local sync from clobbering fresher cron-written data. This issue enumerates every prod data-input flow, assesses the in-season risk of each, and proposes a prioritized set of guardrails.

---

## 1. Prod data-input flows (in-season)

Every row in prod is derived (no user-generated data). During the season there are five distinct write paths into the prod DB:

| # | Flow | Trigger | Tables written | Mechanism | Authority in-season |
|---|------|---------|----------------|-----------|---------------------|
| **A** | **Nightly cron** — `cstat-ingest nightly` | Railway cron `30 9 * * *` UTC | Serving-critical: `games`, `team_game_stats`, `player_game_stats`, `game_forecasts`, ELO on `team_season_stats`, `torvik_player_stats` + `torvik_player_game_stats`, then all of `compute_all`'s output (`player_season_stats`, four factors / AdjEM on `team_season_stats`, CamPom, percentiles, rolling, derived game fields). Ledger: `ingest_runs`, `ingest_run_table_counts`. | Per-step upserts against prod, fail-soft on best-effort feeds, hard-abort on critical step. Records each step. | **Authoritative** — this is the freshest box-score/compute data. |
| **B** | **Full local sync** — `sync_to_prod.sh` (no `--tables`) | Manual, laptop | ALL non-excluded tables | `TRUNCATE … RESTART IDENTITY CASCADE` + `pg_restore` COPY, single atomic txn, `session_replication_role=replica` | **Dangerous** — replaces prod serving tables with (staler) local rows. |
| **C** | **Targeted local sync** — `sync_to_prod.sh --tables …` | Manual, laptop | Only named leaf/derived tables (`lineup_aggregates`, `player_on_off`, `player_rapm`, `player_archetypes`, `archetype_models`) | Same `TRUNCATE … CASCADE` + restore, scoped to the subset | **Intended in-season path** — pushes heavy local jobs the cron doesn't compute, without touching cron-owned tables. |
| **D** | **Runtime API writes** | Live prod API traffic | `portle_daily_puzzle` (server-authoritative daily pin, #181), `api_cache`; ledger writes to `ingest_runs`/`ingest_run_table_counts` | Direct writes from the running service | **Authoritative & irreplaceable** — must never be synced over. |
| **E** | **Schema migrations** | Binary boot (API + ingest) | `_sqlx_migrations` + DDL | sqlx auto-apply on startup | Flows via **binary deploy**, not sync. |

**The ownership split that makes A + C coexist:** the cron owns the serving tables and writes them directly on prod; the laptop owns only heavy derived leaf tables and pushes those with `--tables`. Runtime/ledger tables (D) are in the script's `EXCLUDED` list so no sync can truncate them. This is correct — but only flow **C** is a safe in-season sync; flow **B** breaks the split.

---

## 2. Risk assessment

### R1 — Full sync (flow B) silently rolls prod backward *(highest severity)*
A habitual or reflexive `./scripts/sync_to_prod.sh` in-season truncates and replaces every serving table from the laptop, which is staler than prod. Result: box scores, forecasts, AdjEM, and CamPom on the live site regress to whatever the laptop last computed. **Nothing in the script prevents this** — the only guard is a sentence in `docs/deploy_nightly_cron.md` and the header comment. `grep` confirms there is no date/in-season check.

### R2 — Sync ↔ cron write collision
Flows B/C and A are not mutually excluded and share no lock. A full sync (B) overlapping the 09:30 UTC cron window would have the two contend on the serving tables; the sync's `TRUNCATE` inside its txn and the cron's per-step upserts can block or clobber each other non-deterministically. (Targeted sync C touches a disjoint table set, so this is really a B-mode problem.)

### R3 — `--tables` CASCADE footgun
The targeted restore still uses `TRUNCATE … CASCADE`. Targeting a *referenced* table (`teams`, `games`, `players`) cascade-wipes its dependents on prod even though they aren't in the `--tables` list — potentially deleting live cron-written serving rows. Documented + prompt-warned (`sync_to_prod.sh:44-49`, `220-224`), but not blocked.

### R4 — Silent clobber has no observability
A sync leaves no trace in `ingest_runs` and posts no Slack notice. If a full sync does roll prod back, the first signal is the nightly's row-count gate (M5a) firing a regression the *next* night — or a user noticing stale data. There's no "prod was overwritten by a laptop sync at HH:MM" record.

### Correctly handled (no action, noted for completeness)
- **Ledger exclusion is load-bearing.** `ingest_run_table_counts` / `ingest_runs` in `EXCLUDED` isn't just hygiene — the M5a row-count gate compares each nightly against the **prod-written** prior snapshot; syncing local counts in would poison the baseline. Keep excluded.
- **Portle pin exclusion** (`portle_daily_puzzle`, #181) — a sync must never wipe a pin prod already served. Keep excluded.
- **Atomicity** — the sync wraps TRUNCATE+restore in one transaction, so readers never see a torn/empty state; no partial-write concern.
- **Migration ordering** — schema flows via binary deploy (E), so a new table (e.g. `ingest_run_table_counts`) exists before the nightly writes it, provided the deployed image is current.

---

## 3. Proposed plan

### P0 — Refuse full sync in-season without explicit override *(closes R1)*
Add a date-aware guard to `sync_to_prod.sh`. When `REQUESTED_TABLES` is empty (full mode) **and** the current date is inside the NCAA season window (derive from the same logic as `current_natstat_season()` — roughly Nov 1 → Apr 15), abort with a message unless an explicit `--force-full` (or `--i-understand-full-replace`) flag is passed. Off-season, full mode stays frictionless.
- Acceptance: `./scripts/sync_to_prod.sh` on a Nov–Apr date exits non-zero with guidance to use `--tables` or `--force-full`; `--dry-run` still works; off-season behavior unchanged.

### P1 — Advisory lock to serialize prod writes *(closes R2)*
Have both the sync and the nightly take the same Postgres `pg_advisory_lock` (a fixed 64-bit key) around their prod-write section. The sync acquires it (non-blocking `pg_try_advisory_lock`) before TRUNCATE and aborts with "nightly ingest in progress" if held; the nightly holds it for its serving-table steps.
- Acceptance: a sync launched while the nightly holds the lock refuses cleanly instead of contending.

### P1 — CASCADE-reference guard in `--tables` *(closes R3)*
Before restoring in targeted mode, query prod's FK graph (`pg_constraint`) for any table that references a selected table but is *not* itself selected. If found, abort unless `--allow-cascade` is passed, listing exactly which dependents would be cascade-wiped.
- Acceptance: `--tables teams` aborts naming `players`/`games` as cascade victims; `--tables lineup_aggregates` (a leaf) proceeds.

### P2 — Backward-motion / freshness check *(defense-in-depth for R1)*
Before applying (full or targeted), compare prod vs local for the target tables — row counts and, where a table has a timestamp, `max(updated_at)`. If prod is newer or materially larger, print a prominent warning (or require confirmation) that the sync would move prod backward. Extends the existing pre-apply "local row counts" block to also fetch prod counts and show a delta.
- Acceptance: dry-run and real run print a `local → prod` delta per table; a prod-is-newer condition is surfaced before the confirm prompt.

### P2 — Sync observability *(closes R4)*
On a successful apply, record a synthetic `ingest_runs` step (e.g. `manual_sync`, with the table list) and optionally post to the existing Slack cron/errors webhook so a laptop sync is visible in the same timeline as nightly runs.
- Acceptance: after a sync, `ingest_runs` shows a `manual_sync` row; `/api/health/ingest` and Slack reflect it.

### P3 — Runbook + deploy-ordering note
Fold the above into `docs/deploy_nightly_cron.md`: the in-season rule ("`--tables` only, never full"), the new flags, and an explicit "apply migrations via binary deploy *before* the first dependent nightly" line.

---

## 4. Suggested sequencing
1. **P0** (biggest risk, smallest change — a date guard + one flag).
2. **P1 CASCADE guard** (pure `sync_to_prod.sh` change, no cross-service coordination).
3. **P1 advisory lock** (touches both the script and the nightly orchestrator).
4. **P2 freshness check + P2 observability**.
5. **P3 docs** alongside each of the above.

Each P0/P1 item is independently shippable; P0 alone eliminates the catastrophic case.
