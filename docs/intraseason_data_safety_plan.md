# Intraseason data-input safety: prod write flows + hardening plan

**Type:** hardening / operational-safety
**Area:** `scripts/sync_to_prod.sh`, `cstat-ingest nightly`, `cstat-core::compute`, prod DB
**Motivation:** In-season, prod is written by two independent actors — the Railway nightly cron (`cstat-ingest nightly`) and the operator's laptop (`sync_to_prod.sh`). Their compatibility today rests **entirely on operator discipline**: there is no code-level guard preventing a full local sync from clobbering fresher cron-written data, no lock serializing the two, and one load-bearing safety property is an *undocumented invariant* rather than an enforced one. This issue enumerates every prod data-input flow (verified against the code), assesses the in-season risk of each, and proposes a prioritized set of guardrails.

All findings below were confirmed by reading the code, not inferred — file:line references are given.

---

## 1. Prod data-input flows (in-season)

Every row in prod is derived (no user-generated data). During the season there are five distinct write paths into the prod DB:

| # | Flow | Trigger | Tables written | Mechanism | In-season authority |
|---|------|---------|----------------|-----------|---------------------|
| **A** | **Nightly cron** — `cstat-ingest nightly` | Railway cron `30 9 * * *` UTC (`railway.cron.json`) | Serving-critical: `games`, `player_game_stats`, `team_game_stats` (games step), `game_forecasts` + ELO (elo step), `torvik_player_stats` + `torvik_player_game_stats`, then all of `compute_all`'s output — `player_season_stats`, four factors/AdjEM on `team_season_stats`, CamPom, percentiles, rolling, derived game fields. Ledger: `ingest_runs`, `ingest_run_table_counts`. | **Per-row/batched `INSERT … ON CONFLICT DO UPDATE`** (`games.rs:285,403,515`; `elo.rs:254`) — upsert, never truncate. Per-step commits. Fail-soft on best-effort feeds, hard-abort on critical step. Self-heals a missed night via the ledger gap scan (`season.rs`, `SeasonIngester::nightly`). | **Authoritative** — freshest box-score/compute data. |
| **B** | **Full local sync** — `sync_to_prod.sh` (no `--tables`) | Manual, laptop | ALL non-excluded tables | `TRUNCATE … RESTART IDENTITY CASCADE` + `pg_restore` COPY in **one atomic txn**, `session_replication_role=replica` (`sync_to_prod.sh:237-251`) | **Dangerous in-season** — replaces prod serving tables with (staler) local rows. |
| **C** | **Targeted local sync** — `sync_to_prod.sh --tables …` | Manual, laptop | Only named leaf/derived tables (`lineup_aggregates`, `player_on_off`, `player_rapm`, `player_archetypes`, `archetype_models`) | Same `TRUNCATE … CASCADE` + restore, scoped to the subset (`sync_to_prod.sh:137-162,220-224`) | **Intended in-season path** — pushes heavy local jobs the cron doesn't compute. |
| **C2** | **Column merge** — `sync_to_prod.sh --columns table.col` | Manual, laptop | Named columns only, on any non-EXCLUDED table with a UNIQUE constraint (`players.display_name` is the motivating case) | `UPDATE … FROM (VALUES …)` in batches, matched on the natural key (a unique index, never the UUID pk), skipping rows that already agree — **no TRUNCATE, no INSERT, no DELETE**, so it cannot cascade | **Safe in-season by construction** — the only route to a derived column on an FK-referenced table, which **C** would cascade-wipe (R3) and **B** is refused for. Season-scoped to PAST seasons whenever prod looks live (same signals as the R1 guard), because prod's nightly owns the current season for anything `compute_all` derives; `--force-full` overrides. Reports rows actually updated, so a key that doesn't line up with prod is loud rather than silent. |
| **D** | **Runtime API writes** | Live prod API traffic | `portle_daily_puzzle` (server-authoritative daily pin, #181), `api_cache`; ledger `ingest_runs` / `ingest_run_table_counts` | Direct writes from the running service | **Authoritative & irreplaceable** — must never be synced over. |
| **E** | **Schema migrations** | Binary boot (API + ingest) | `_sqlx_migrations` + DDL | sqlx auto-apply on startup | Flows via **binary deploy**, not sync. |

**The ownership split that lets A + C coexist:** the cron owns the serving tables and upserts them directly on prod; the laptop owns only heavy derived leaf tables and pushes those with `--tables`. Runtime/ledger tables (D) are in the script's `EXCLUDED` list. Only flow **C** is a safe in-season sync; flow **B** breaks the split.

**`EXCLUDED` verified complete** (`sync_to_prod.sh:92`) against all runtime/local-only tables: `api_cache`, `ingest_runs`, `ingest_run_table_counts`, `portle_daily_puzzle` (runtime-written on prod) + `play_by_play`, `lineup_stints`, `natstat_lineups`, `natstat_lineup_games` (local-only). No runtime table is missing from the list today.

---

## 2. Risk assessment (verified)

### R1 — Full sync (flow B) silently rolls prod backward *(highest severity)*
A reflexive `./scripts/sync_to_prod.sh` in-season truncates and replaces every serving table from the laptop, which is staler than prod. Box scores, forecasts, AdjEM, and CamPom on the live site regress to whatever the laptop last computed. **Confirmed: there is no date/in-season/`--force` guard anywhere in the script** — `grep` for `month|season|today|force` in `sync_to_prod.sh` finds only unrelated hits. The sole protection is prose in `docs/deploy_nightly_cron.md:236` and the header comment.

### R2 — Sync ↔ cron write collision (no lock) *(verified: zero advisory locks in the codebase)*
Flows A and B/C share no lock — `grep -r advisory_lock crates/ scripts/` returns **nothing**. The nightly upserts and commits per step (no single long txn), while a full sync's `TRUNCATE … CASCADE` takes an `ACCESS EXCLUSIVE` lock on each table inside one transaction. If a full sync overlaps the 09:30-UTC cron window, the two interleave non-deterministically: the sync wipes+restores to the stale local snapshot, nightly steps that committed before are lost, steps after re-apply on top → an internally inconsistent prod state that no single actor produced. (Targeted sync C touches a disjoint table set from the cron — see R4 for why that's currently true — so this is a B-mode problem in practice.)

### R3 — `--tables` CASCADE footgun *(verified)*
The targeted restore still uses `TRUNCATE … CASCADE` (`sync_to_prod.sh:233`). Targeting a *referenced* table (`teams`, `games`, `players`) cascade-wipes its dependents on prod even though they aren't in the `--tables` list — potentially deleting live cron-written serving rows. Documented + prompt-warned (`sync_to_prod.sh:44-49,220-224`) but not blocked.

### R4 — Targeted-sync safety is an UNDOCUMENTED, coupled invariant *(new — the subtle one)*
The `--tables lineup_aggregates,player_on_off` path is safe **only because** `compute_all` step 10/19 `compute_pbp_lineups` — which does season-scoped `DELETE FROM lineup_aggregates / player_on_off` then rebuilds (`compute.rs:2894,2902`) — **early-returns at `compute.rs:2730` (`if games.is_empty() && covered_pairs.is_empty() { return Ok(0) }`) before reaching those DELETEs.** On prod that guard is always true, because `play_by_play` and `natstat_lineups` are `EXCLUDED` from sync, so prod holds zero PBP/lineup rows. The nightly therefore no-ops on those tables and the targeted sync is their sole prod writer.

The danger: **this is a coupling, not a guarantee.** If PBP or the lineups-object tables were ever shipped to prod (removed from `EXCLUDED`, or a well-meaning "let's serve stints" change), the nightly's `compute_pbp_lineups` would begin wiping and rebuilding `lineup_aggregates` / `player_on_off` from prod PBP **every night**, silently colliding with — and erasing — the operator's targeted sync. Nothing documents or tests this invariant.

> **Update 2026-08-22 (#249) — the premise expired, and it did not need anyone to ship PBP.** The paragraph above is preserved as the original analysis; read it as history, not as current guidance. Prod ingests its own play-by-play now (the nightly's `playbyplay` / `lineups` steps), so from the first game of a season the early-return stops firing and prod rebuilds both rollups nightly — the exact collision described, arriving through the front door rather than through a change to `EXCLUDED`. The **conclusion inverts**: `--tables lineup_aggregates,player_on_off` is no longer the safe in-season path, it is the collision. The exclusion itself stays, for two different reasons — scope coherence (prod's PBP is only what prod ingested, so its rebuild is confined to the current season and the laptop keeps the historical rollups) and prod's disk budget (#252). Live ownership table: `docs/tipoff_self_sufficiency_plan.md` §3.

### R5 — Silent clobber has no observability *(verified)*
A sync leaves no trace in `ingest_runs` and posts no Slack notice. If a full sync rolls prod back, the first signal is the M5a row-count gate firing a regression the *next* night, or a user noticing stale data. There is no "prod was overwritten by a laptop sync at HH:MM" record.

### Correctly handled (no action; verified, noted so a future change doesn't regress them)
- **Ledger exclusion is load-bearing.** `ingest_runs` / `ingest_run_table_counts` excluded (`039_ingest_runs.sql:7-10`) — the M5a gate compares each nightly against the **prod-written** prior snapshot; syncing local counts in would poison the baseline.
- **Portle pin exclusion** (`portle_daily_puzzle`, #181) — a sync must never wipe a pin prod already served.
- **Atomicity** — the sync wraps TRUNCATE+restore in one transaction (`sync_to_prod.sh:251` `--single-transaction`), so readers never see a torn/empty state.
- **Migration ordering** — schema flows via binary deploy (E); `_sqlx_migrations` excluded. A new table exists before the nightly writes it, provided the deployed image is current.

---

## 3. Proposed plan

Design goal: make the safe in-season path (targeted sync only, serialized with the cron) the **default that requires no thought**, and make every dangerous path require an explicit, informed override.

### P0 — Refuse full sync when prod is live, without explicit override *(closes R1)* — **SHIPPED 2026-07-17**
**Implemented as specified below**, plus a read-only `--prod-status` inspector (per-step ledger freshness, recent failures, exact row counts, guard verdict; writes nothing and works with the local DB down). Both signals landed; `--force-full` overrides; `--dry-run` reports without blocking; `--tables` is not gated. The guard runs *before* the dump, so it costs nothing to hit. Verified across 10 scenarios including the one that matters: **in-season + dead cron still blocks** — that is signal 1 going quiet for a bad reason, and exactly when an operator is most tempted to "fix" prod with a full sync. Note the threshold reuses `STALE_AFTER_HOURS = 36` from `health.rs` rather than inventing a second staleness rule; keep the two in sync.

Original specification, retained as the record of intent:

Add a guard to `sync_to_prod.sh` that fires when `REQUESTED_TABLES` is empty (full mode). Two complementary signals, both cheap:
1. **Data-driven (primary, robust):** query prod `ingest_runs` for the most recent successful serving step — `SELECT max(ended_at) FROM ingest_runs WHERE status='ok' AND step IN ('games','compute')`. If that is within ~36h, prod is actively cron-fed; abort a full sync. This is preferable to a pure calendar check because it self-adjusts to tournament runs, early/late tip, and the `simulate` harness.
2. **Calendar (secondary, zero-dependency):** in-season window from the same rule as `season_for_date` (`lib.rs:116`) — treat month ∈ {11,12,1,2,3} and early April as in-season.

Either signal ⇒ abort full mode unless `--force-full` (a.k.a. `--i-understand-full-replace`) is passed. `--dry-run` and off-season full syncs stay frictionless.
- **Acceptance:** `./scripts/sync_to_prod.sh` aborts non-zero with guidance ("use `--tables`, or `--force-full`") when prod has a recent successful nightly OR the date is in-season; `--force-full` overrides; `--dry-run` unaffected; off-season + idle-prod behavior unchanged.

### P1 — Advisory lock to serialize prod writes *(closes R2)*
Both the sync and the nightly take the same Postgres advisory lock (a fixed 64-bit key) around their prod-write section. The sync uses `pg_try_advisory_lock` (non-blocking) before TRUNCATE and aborts with "nightly ingest in progress, retry later" if held; the nightly holds it across its serving-table steps and releases at the end. Greenfield on both sides (no existing lock to reconcile).
- **Acceptance:** a sync launched while the nightly holds the lock refuses cleanly instead of racing; a nightly launched while a sync holds it waits or defers per the chosen policy.

### P1 — CASCADE-reference guard in `--tables` *(closes R3)*
Before restoring in targeted mode, query prod's FK graph (`pg_constraint`) for any table that references a selected table but is *not* itself selected. If found, abort unless `--allow-cascade` is passed, listing exactly which dependents would be cascade-wiped.
- **Acceptance:** `--tables teams` aborts naming `players`/`games` as cascade victims; `--tables lineup_aggregates` (a leaf) proceeds untouched.

### P1 — Lock down the R4 invariant *(closes R4)*
The targeted-sync path's safety depends on prod never holding PBP/lineup source rows. Make that explicit and enforced:
- Add an assertion/comment at `compute.rs:2708` and in the `sync_to_prod.sh` header documenting that `lineup_aggregates` / `player_on_off` are prod-write-owned by the targeted sync **because** `compute_pbp_lineups` no-ops on a PBP-less prod, and that shipping PBP/lineups to prod would break this.
- Add a guard test (mirrors the existing `swapped_games.rs` invariant style) asserting `play_by_play`, `lineup_stints`, `natstat_lineups`, `natstat_lineup_games` remain in `EXCLUDED`, so removing one trips CI.
- **Acceptance:** a test fails if any of the four local-only tables is dropped from `EXCLUDED`; the coupling is documented at both ends.

### P2 — Backward-motion / freshness surfacing *(defense-in-depth for R1)*
Serving tables have **no `updated_at`** (verified — `grep` finds none on `games`/`team_season_stats`/etc.), so freshness must come from the prod `ingest_runs.ended_at` signal introduced in P0, plus a row-count delta. Extend the existing pre-apply "local row counts" block (`sync_to_prod.sh:184-188`) to also fetch prod counts and print a `local → prod` delta per table before the confirm prompt, and echo prod's last successful nightly timestamp.
- **Acceptance:** dry-run and real run print a per-table `local → prod` delta and prod's last-nightly time; a prod-is-newer/larger condition is visible before confirming.

### P2 — Sync observability *(closes R5)*
On a successful apply, record a synthetic `ingest_runs` row (`step='manual_sync'`, `notes` = the table list, `status='ok'`) and optionally post to the existing Slack cron/errors webhook, so a laptop sync appears in the same timeline as nightly runs and on `/api/health/ingest`.
- **Acceptance:** after a sync, `ingest_runs` shows a `manual_sync` row and Slack/health reflect it.

### P3 — Runbook + deploy-ordering note
Fold the above into `docs/deploy_nightly_cron.md`: the in-season rule ("`--tables` only, never full"), the new flags (`--force-full`, `--allow-cascade`), the R4 invariant, and an explicit "apply migrations via binary deploy *before* the first dependent nightly" line.

---

## 4. Suggested sequencing
1. **P0** — biggest risk, smallest change (a prod-freshness check + one flag). Eliminates the catastrophic case alone.
2. **P1 CASCADE guard** and **P1 R4 invariant test** — pure `sync_to_prod.sh` + test changes, no cross-service coordination.
3. **P1 advisory lock** — touches both the script and the nightly orchestrator (`season.rs`).
4. **P2 freshness surfacing + P2 observability**.
5. **P3 docs** alongside each of the above.

Each P0/P1 item is independently shippable. **P0 alone closes the one path that can take the live site backward; P1-R4 closes the one latent trap that a future "serve lineups on prod" change would spring.**
