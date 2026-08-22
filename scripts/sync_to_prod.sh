#!/usr/bin/env bash
#
# Sync local cstat database → Railway production.
#
# The site has no user-generated data — every row in prod is derived from
# upstream APIs (NatStat, Torvik) and the local compute pipeline. Local is
# the source of truth; prod is a deterministic mirror. This script ships
# data only — schema is owned by sqlx migrations, which the API auto-applies
# on startup.
#
# Approach (atomic + fast):
#   1. pg_dump -Fc   → binary, compressed local file (~5-10× smaller than text)
#   2. pg_restore    → emits COPY statements (much faster than INSERTs)
#   3. psql --single-transaction wraps:
#        SET session_replication_role = 'replica';   -- skip FK/trigger checks
#        TRUNCATE … CASCADE;                          -- wipe in same txn
#        <COPY statements from pg_restore>;
#        SET session_replication_role = 'origin';
#   4. COMMIT — prod readers see old data until the COMMIT, then new instantly.
#
# Excluded tables (intentional):
#   - api_cache: NatStat response cache, only useful during ingestion
#   - _sqlx_migrations: managed by sqlx, never overwrite from a dump
#   - play_by_play: raw PBP is local-only (~7.5 GB across all seasons); the
#       live site reads only derived aggregates. Pushing it would blow
#       Railway's DB cap — see docs/pbp_methodology.md "Storage & prod sync".
#   - lineup_stints: per-stint PBP derivation, also local-only (P2). Listed
#       now so it's excluded the moment that table lands.
#
# Usage:
#   ./scripts/sync_to_prod.sh                          # full run (all tables)
#   ./scripts/sync_to_prod.sh --dry-run                # preview without applying
#   ./scripts/sync_to_prod.sh --prod-status            # READ-ONLY prod inspection
#   ./scripts/sync_to_prod.sh --tables a,b,c           # push only these tables
#   ./scripts/sync_to_prod.sh --tables player_rapm
#   ./scripts/sync_to_prod.sh --columns players.display_name  # merge one column
#   ./scripts/sync_to_prod.sh --force-full             # override the in-season guard
#   ./scripts/sync_to_prod.sh --force-tables           # override the staleness preflight
#
# IN-SEASON RULE (enforced, not just documented — see the P0 guard below and
# docs/intraseason_data_safety_plan.md): while prod is cron-fed, a FULL sync is
# a silent rollback of the live site and is refused. Use --tables to push the
# heavy local-only derived tables, which is the intended in-season path. A full
# replace is an off-season/bootstrap operation.
#
# --prod-status is read-only: it opens no transaction and writes nothing. Use it
# to answer "is the cron alive, and did something clobber prod?" without any
# risk of touching data.
#
# --tables restricts the dump/TRUNCATE/restore to a comma-separated subset
# (intersected with the live, non-excluded local tables). This is the targeted
# mode for the Railway-direct nightly architecture: the serving-critical tables
# are written on prod by the nightly cron job, so the local machine pushes ONLY
# its heavy derived tables (PBP/RAPM/archetype outputs) without a full truncate
# clobbering what the cron just wrote. Names in EXCLUDED can never be selected;
# an unknown name aborts before any write.
#
# --tables is TRUNCATE + restore-from-local, so it is only safe while LOCAL IS AT
# LEAST AS COMPLETE AS PROD. A staleness preflight enforces that (#249, exit 4):
# for every requested table carrying a season-like column it asks prod for the
# seasons local cannot reproduce, and refuses the push naming them. This is a
# precondition on local, not a claim about ownership — which is deliberate,
# because ownership here is seasonal. `lineup_aggregates` is legitimately
# laptop-owned until prod ingests a season's play-by-play and legitimately
# prod-owned from the first game after, so a static owner list would be wrong
# for part of every year and would train you to reach for the override. Season
# coverage is what actually changes, and it is right on both sides of the flip
# with no calendar rule. --force-tables overrides; it accepts the deletion.
#
# --columns table.col[,col…] is the third mode, for the case --tables cannot
# serve: a derived column on a table that is REFERENCED by foreign keys.
# `players.display_name` is the motivating one — `players` has 10 dependents, so
# --tables would cascade-wipe them, and a full sync (the only other carrier of
# `players`) is refused while prod is cron-fed. Column merge does the one safe
# operation available: UPDATE existing rows, named columns only. No TRUNCATE, no
# INSERT, no DELETE — so it cannot cascade, invent rows, or lose them. Rows are
# matched on a natural key taken from MERGE_ALLOWLIST (never the uuid primary
# key, which is generated locally), and rows that already agree are skipped
# rather than rewritten. Batched into `UPDATE … FROM (VALUES …)` chunks to avoid
# an N+1 over the prod link.
#
# The supported (table, key) pairs are ENUMERATED, not discovered from the
# catalog. Discovery is what made this mode hard: across four review rounds it
# accepted INCLUDE payload columns, invalid indexes, nondeterministic ties,
# surrogate-uuid composites and a nullable key — each fix opening the next case,
# for generality nothing used. Adding a table is a deliberate act with a
# reviewer; the key must be stable across databases and NOT NULL (asserted).
#
# It does NOT inherit the full-sync guard, because it is narrower — but "narrow"
# is not "harmless": prod's nightly computes some of these columns itself
# (`players.display_name` is `compute_all` step 21). So whenever prod looks live
# — same two signals the guard uses — a table with a `season` column merges PAST
# SEASONS ONLY, which is the real ownership split: prod owns the current season,
# the laptop owns history. `--force-full` merges everything.
#
# CAVEAT: the restore still uses TRUNCATE ... CASCADE, so targeting a table that
# is *referenced* by a foreign key (e.g. `teams`, which `players`/`games` point
# at) would cascade-wipe those dependents on prod even though they aren't in your
# list. --tables is intended for LEAF / derived output tables (lineup_aggregates,
# player_on_off, player_rapm, player_archetypes, archetype_models, …) that nothing
# references. The confirmation prompt prints the exact TRUNCATE — read it.
#
# PROD_DATABASE_URL is auto-loaded from ../.env (gitignored). Override with
# `PROD_DATABASE_URL=... ./scripts/sync_to_prod.sh` if needed.

set -euo pipefail

# Auto-source .env from the repo root if present.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/../.env"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

LOCAL_URL="${LOCAL_DATABASE_URL:-postgres://cstat:cstat@localhost:5432/cstat}"
PROD_URL="${PROD_DATABASE_URL:?Set PROD_DATABASE_URL in .env or your shell to the Railway prod connection string}"
DRY_RUN=0
REQUESTED_TABLES=""   # empty = all (full sync); set by --tables for targeted mode
REQUESTED_COLUMNS=""  # set by --columns for column-merge mode (see below)
FORCE_FULL=0          # --force-full: override the in-season full-sync guard
FORCE_TABLES=0        # --force-tables: override the --tables staleness preflight
PROD_STATUS=0         # --prod-status: read-only prod inspection, then exit

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run|-n) DRY_RUN=1; shift ;;
    --tables) REQUESTED_TABLES="${2:?--tables needs a comma-separated list}"; shift 2 ;;
    --tables=*) REQUESTED_TABLES="${1#--tables=}"; shift ;;
    --columns) REQUESTED_COLUMNS="${2:?--columns needs table.col[,col…]}"; shift 2 ;;
    --columns=*) REQUESTED_COLUMNS="${1#--columns=}"; shift ;;
    --force-full) FORCE_FULL=1; shift ;;
    --force-tables) FORCE_TABLES=1; shift ;;
    --prod-status) PROD_STATUS=1; shift ;;
    -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

# --columns short-circuits before the table-push path, so accepting both would
# run the merge and silently drop the requested table push. Refuse instead: the
# two are different operations with different blast radii, and "I asked for a
# push and got a merge" is exactly the kind of quiet substitution this script
# exists to prevent.
if [[ -n "$REQUESTED_COLUMNS" && -n "$REQUESTED_TABLES" ]]; then
  echo "✗ --columns and --tables are separate modes; run them one at a time." >&2
  exit 2
fi

mask_url() { sed -E 's|://[^@]+@|://***@|' <<<"$1"; }

# Strip leading/trailing whitespace from a psql -t -A value, preserving any
# internal spaces (see `coltype` — Postgres type names contain them).
trim() { sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | head -1; }

# The season the pipeline considers current, mirroring
# `cstat_ingest::season_for_date` (lib.rs:165): November rolls to next year.
current_season() {
  local y m
  y=$(date -u +%Y); m=$(date -u +%m)
  if [[ "${m#0}" -ge 11 ]]; then echo $((y + 1)); else echo "$y"; fi
}

# Staleness threshold for "is prod still being fed by the cron?", mirroring
# STALE_AFTER_HOURS in crates/cstat-api/src/routes/health.rs. One missed nightly
# is still fresh; two in a row is not. Keep the two in sync.
STALE_AFTER_HOURS=36

# True when the calendar says college basketball is being played, using the same
# Nov-rollover boundary as `season_for_date` (crates/cstat-ingest/src/lib.rs).
# Deliberately crude: it is the *secondary* signal, and only has to hold when
# the ledger can't be read (see the P0 guard).
in_season_now() {
  local m d
  m=$((10#$(date -u +%m)))   # 10# forces base-10: "08"/"09" are invalid octal
  d=$((10#$(date -u +%d)))
  case "$m" in
    11|12|1|2|3) return 0 ;;
    4) [[ "$d" -le 15 ]] && return 0 ;;   # through the Final Four
  esac
  return 1
}

# Hours since prod last recorded a SUCCESSFUL served-critical step, or "" if the
# ledger is empty/unreadable. Read-only. `|| true` so an unreachable prod or a
# missing table degrades to "unknown" rather than killing the script under
# `set -e` — the caller decides what unknown means.
prod_nightly_age_hours() {
  "${PSQL[@]}" "$PROD_URL" -t -A -c "
    SELECT coalesce(round(extract(epoch FROM (now() - max(ended_at))) / 3600)::text, '')
    FROM ingest_runs
    WHERE status = 'ok' AND step IN ('games', 'compute')
  " 2>/dev/null | tr -d '[:space:]' || true
}

# Tables to skip on both dump and truncate sides. See the header comment for
# the rationale on each (api_cache / _sqlx_migrations: managed elsewhere;
# play_by_play / lineup_stints: local-only raw PBP, never shipped to prod;
# natstat_lineups / natstat_lineup_games: local-only lineups-object capture,
# prod serves only the derived lineup_aggregates / player_on_off.
#   R4 INVARIANT (docs/intraseason_data_safety_plan.md §R4): keeping these four
#   source tables local-only is LOAD-BEARING, not just a size optimization —
#   though what it bears CHANGED at tipoff (#249), and the old reason is now
#   backwards. It used to be what let the laptop safely own lineup_aggregates /
#   player_on_off on prod: with no PBP there, compute_pbp_lineups early-returned
#   instead of rebuilding them. Prod now ingests its own PBP, so it rebuilds both
#   rollups nightly for the season it is ingesting, and pushing them from here is
#   the collision — NOT the intended path. What the exclusion protects now is (a)
#   scope: prod's PBP is exactly what prod ingested, so its rebuild is confined to
#   the current season and the laptop keeps the historical rollups; ship history
#   up and both sides write the same rows; and (b) prod's disk, at ~1 GB of PBP
#   per lived-through season against a 10 GB volume.
#   Enforced by crates/cstat-core/tests/sync_prod_r4_invariant.rs — do not remove
#   any of the four without reading that coupling first.
# ingest_runs / ingest_run_table_counts: runtime ledger + row-count snapshots
# written directly by the prod nightly job — a local full-sync must not truncate
# them out from under the live pipeline (the row-count gate compares against the
# prod-written prior snapshot);
# portle_daily_puzzle: runtime-frozen daily answers pinned by the prod API — a
# sync must never wipe a pin prod already served to live players (issue #181)).
EXCLUDED=("api_cache" "_sqlx_migrations" "play_by_play" "lineup_stints" "natstat_lineups" "natstat_lineup_games" "ingest_runs" "ingest_run_table_counts" "portle_daily_puzzle")

# Prefer host-installed psql tools; fall back to running them inside the local
# Postgres docker container. The container ships matching client tools and
# can reach both local (its own server) and prod (via the host network).
DOCKER_PG="cstat-postgres"
if command -v pg_dump >/dev/null && command -v pg_restore >/dev/null && command -v psql >/dev/null; then
  PG_DUMP=(pg_dump)
  PG_RESTORE=(pg_restore)
  PSQL=(psql)
elif docker ps --format '{{.Names}}' | grep -q "^${DOCKER_PG}\$"; then
  echo "→ Using docker container '${DOCKER_PG}' for psql tools"
  PG_DUMP=(docker exec -i "$DOCKER_PG" pg_dump)
  PG_RESTORE=(docker exec -i "$DOCKER_PG" pg_restore)
  PSQL=(docker exec -i "$DOCKER_PG" psql)
else
  echo "Need either local psql/pg_dump/pg_restore (brew install postgresql@17)"
  echo "or the '${DOCKER_PG}' container running (docker compose up -d)."
  exit 1
fi

# --prod-status: read-only prod inspection, then exit. Answers "is the cron
# alive, and has anything clobbered prod?" — the two questions worth asking
# before reaching for a sync at all. Deliberately placed before every local
# query so it still works when the local DB is down, and deliberately writes
# nothing: no transaction, no TRUNCATE, no restore. It is safe to run at any
# time, in-season included.
if [[ "$PROD_STATUS" -eq 1 ]]; then
  echo "→ Prod: $(mask_url "$PROD_URL")  (read-only — this mode writes nothing)"
  echo
  if ! "${PSQL[@]}" "$PROD_URL" -t -A -c "SELECT 1" >/dev/null 2>&1; then
    echo "  ✗ Cannot connect to prod. Check PROD_DATABASE_URL."
    exit 1
  fi
  echo "→ Last SUCCESSFUL nightly step (from the ingest_runs ledger):"
  "${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    SELECT rpad(step, 14),
           to_char(max(ended_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') || ' UTC',
           '(' || round(extract(epoch FROM (now() - max(ended_at))) / 3600) || 'h ago)'
    FROM ingest_runs
    WHERE status = 'ok'
    GROUP BY step
    ORDER BY max(ended_at) DESC
  " | sed 's/^/    /' || true
  echo
  # `source_not_published` is excluded here and summarised on its own line below
  # (#248). It is not a fault, and it is HIGH VOLUME in the one window where this
  # panel matters most: through the November gap the nightly writes one for
  # `torvik` and one for `torvik_games` every night — 14 rows over this 7-day
  # window, all newer than anything else. Left in the same list they would fill
  # the LIMIT and push a real `forecasts` or `playbyplay` failure out of view,
  # making the panel least informative exactly when it is being read most.
  echo "→ Recent FAILED / SKIPPED steps (last 7d):"
  FAILS=$("${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    SELECT to_char(ended_at AT TIME ZONE 'UTC', 'MM-DD HH24:MI'), rpad(step, 14), status,
           coalesce(left(error, 60), '')
    FROM ingest_runs
    WHERE status NOT IN ('ok', 'source_not_published')
      AND ended_at > now() - interval '7 days'
    ORDER BY ended_at DESC LIMIT 10
  " || true)
  if [[ -n "$FAILS" ]]; then sed 's/^/    /' <<<"$FAILS"; else echo "    (none)"; fi
  echo
  # Summarised, not listed: one line per step says everything an operator needs
  # (which feed, how many nights, how recently) without spending the panel above.
  # Silent when there are none, so this costs nothing outside the gap.
  UNPUB=$("${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    SELECT rpad(step, 14), count(*) || ' night(s)',
           'latest ' || to_char(max(ended_at) AT TIME ZONE 'UTC', 'MM-DD HH24:MI') || ' UTC'
    FROM ingest_runs
    WHERE status = 'source_not_published' AND ended_at > now() - interval '7 days'
    GROUP BY step ORDER BY max(ended_at) DESC
  " || true)
  if [[ -n "$UNPUB" ]]; then
    echo "→ Upstream has not published this season (last 7d) — expected at the Nov flip:"
    sed 's/^/    /' <<<"$UNPUB"
    echo
  fi
  # Sequence skew — a sequence sitting at or below its table's max(id) makes the
  # NEXT insert a duplicate-key violation, and keeps doing so until nextval
  # climbs past max(id). That is issue #186's actual damage, and it is invisible
  # in every other panel here: the ledger writer is fail-soft, so a dead sequence
  # looks exactly like a cron that stopped running (every step reads "76h ago",
  # "Recent FAILED: (none)") while the nightly is in fact running fine.
  #
  # Checked for EVERY sequence, not just the excluded-table ones the dump guard
  # covers. That guard enforces "excluded table's sequence must not leak"; the
  # real invariant is "prod-written table must be excluded", whose other half
  # nothing enforces. A future prod-written SERIAL table that someone forgets to
  # add to EXCLUDED would be dumped legitimately, rewind prod, and never trip
  # SEQ_LEAKS. This check is the detective control for that gap — it reports the
  # damage regardless of which path caused it. (Today the schema has exactly one
  # sequence, ingest_runs_id_seq; everything else is UUID- or natural-keyed.)
  # Fully catalog-driven: pg_depend resolves each sequence to its owning
  # table+column, and query_to_xml runs the per-column max() that a static join
  # can't express (same trick as the row-count panel below — one round trip, no
  # N+1 against a high-latency prod). A sequence added by a future migration is
  # covered automatically; nothing here needs updating by hand.
  echo "→ Sequence health (last_value vs max(id) — skew breaks the NEXT insert):"
  # Captured rather than streamed so an empty result can be reported as such.
  # This check exists BECAUSE a silent no-op looked like health for three nights;
  # printing a bare header on error would reproduce exactly that failure mode
  # (reads as "no sequences, nothing to worry about"). Needs Postgres 10+ for
  # pg_sequence_last_value.
  SEQ_HEALTH=$("${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    WITH owned AS (
      SELECT c.oid AS seqoid, c.relname AS seqname, t.relname AS tblname, a.attname AS colname
      FROM pg_class c
      JOIN pg_depend d  ON d.objid = c.oid AND d.classid = 'pg_class'::regclass
                       AND d.deptype IN ('a','i')
      JOIN pg_class t   ON t.oid = d.refobjid
      JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = d.refobjsubid
      WHERE c.relkind = 'S' AND c.relnamespace = 'public'::regnamespace
    ), probed AS (
      SELECT o.seqname, o.tblname, o.colname,
             pg_sequence_last_value(o.seqoid) AS last_value,
             (xpath('/row/m/text()', query_to_xml(
                format('SELECT max(%I) AS m FROM public.%I', o.colname, o.tblname),
                false, true, '')))[1]::text::bigint AS max_id
      FROM owned o
    )
    SELECT rpad(seqname, 24),
           'seq=' || rpad(coalesce(last_value::text, 'unused'), 10),
           'max(' || colname || ')=' || rpad(coalesce(max_id::text, '-'), 10),
           CASE
             WHEN max_id IS NULL THEN 'ok (table empty)'
             WHEN last_value IS NULL THEN 'BROKEN — sequence never called but table has rows'
             WHEN last_value < max_id THEN
               '*** BROKEN — next ' || (max_id - last_value) || ' insert(s) fail on duplicate key.'
               || '  Fix: SELECT setval(''' || seqname || ''', (SELECT max(' || colname
               || ') FROM ' || tblname || ')); ***'
             ELSE 'ok'
           END
    FROM probed
    ORDER BY (last_value IS NOT NULL AND max_id IS NOT NULL AND last_value < max_id) DESC, seqname
  " 2>&1) || true
  if [[ -z "$SEQ_HEALTH" ]]; then
    echo "    ✗ check returned nothing — could not read sequence state (treat as UNKNOWN, not ok)"
  else
    sed 's/^/    /' <<<"$SEQ_HEALTH"
  fi
  echo
  # Exact counts, deliberately — NOT n_live_tup or reltuples. Those are only
  # populated by ANALYZE/autovacuum, so a never-analyzed or freshly-restored
  # table reports 0 while holding millions of rows (verified locally:
  # play_by_play reports n_live_tup = 0 at 32.8M actual rows, last_analyze
  # NULL). This tool exists to answer "did something wipe prod?", and a table
  # that just got restored is precisely the case where autovacuum hasn't caught
  # up — so the estimate would false-alarm exactly when it matters most.
  #
  # One round trip via query_to_xml rather than a count per table: prod is
  # high-latency and N+1 against it is a known stall (docs/in_season_ingest_plan.md).
  # Measured at ~4.5s across the whole non-excluded set, which is fine for a
  # diagnostic you run on purpose.
  echo "→ Prod row counts (exact — takes a few seconds):"
  "${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    SELECT rpad(relname, 28), to_char(cnt, 'FM999,999,999')
    FROM (
      SELECT relname,
             (xpath('/row/c/text()',
                    query_to_xml(format('SELECT count(*) AS c FROM public.%I', relname),
                                 false, true, '')))[1]::text::bigint AS cnt
      FROM pg_stat_user_tables
    ) x
    ORDER BY cnt DESC
  " | sed 's/^/    /' || true
  echo
  AGE_H=$(prod_nightly_age_hours)
  if [[ "$AGE_H" =~ ^[0-9]+$ ]] && [[ "$AGE_H" -lt "$STALE_AFTER_HOURS" ]]; then
    echo "→ Full-sync guard: WOULD BLOCK — prod is cron-fed (last success ${AGE_H}h ago)."
  elif in_season_now; then
    echo "→ Full-sync guard: WOULD BLOCK — the calendar says in-season."
  else
    echo "→ Full-sync guard: would allow (prod looks idle and it is off-season)."
  fi
  exit 0
fi

EXCLUDED_QUOTED=$(printf "'%s'," "${EXCLUDED[@]}")
EXCLUDED_QUOTED="${EXCLUDED_QUOTED%,}"

# An excluded table's OWNED sequence has to be held back alongside its rows.
# `-T <table>` matches the TABLE only: a serial/identity sequence is a separate
# relation with its own name (`<table>_<column>_seq`), so it sails past the
# table's exclusion and its state still reaches the dump as a `SEQUENCE SET`
# entry. On restore that setval() rewinds prod's sequence to THIS machine's
# value, and every insert on prod then fails with a duplicate key until the
# sequence climbs back past max(id) — silently, because the nightly ledger
# writer is fail-soft. That is what hit `ingest_runs` on 2026-07-16 (issue
# #186): the rows were correctly held back, the sequence was not.
#
# Derive the names from the catalog rather than pattern-matching them. A
# `<table>_*_seq` pattern would cover every serial/identity sequence (Postgres
# names them `<table>_<column>_seq`) but silently miss a hand-named one, which
# is the same class of near-miss that caused #186. Asking the catalog which
# sequences an excluded table OWNS is exact, needs no naming convention to hold,
# and follows how TABLE_LIST is already discovered rather than hardcoded.
#
# `deptype = 'a'` is deliberate, and narrower than it looks — do not "fix" it to
# IN ('a','i') without re-testing. Only a SERIAL sequence (auto dependency, 'a')
# survives its table's -T and leaks; an IDENTITY sequence (internal dependency,
# 'i') is dropped along with its table automatically, so naming it would be dead
# code implying a leak that cannot happen. Verified against pg_dump 17.
#
# Note this can only ever withhold a sequence belonging to an already-excluded
# table: pg_dump re-attaches owned sequences of dumped tables, so a sequence
# whose owning table is INCLUDED is dumped regardless of any -T naming it.
EXCLUDED_SEQS=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
  SELECT string_agg(s.relname, ',' ORDER BY s.relname)
  FROM pg_class s
  JOIN pg_depend d ON d.objid = s.oid AND d.deptype = 'a'
  JOIN pg_class t ON t.oid = d.refobjid
  WHERE s.relkind = 'S'
    AND t.relname IN ($EXCLUDED_QUOTED)
" | tr -d '[:space:]')

# Build pg_dump -T flags: every excluded table, plus every sequence they own.
EXCLUDE_FLAGS=()
for t in "${EXCLUDED[@]}"; do
  EXCLUDE_FLAGS+=("-T" "$t")
done
for s in ${EXCLUDED_SEQS//,/ }; do
  EXCLUDE_FLAGS+=("-T" "$s")
done

# Discover the live table list from local; new tables get picked up
# automatically without needing to edit this script.
TABLE_LIST=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
  SELECT string_agg(tablename, ',' ORDER BY tablename)
  FROM pg_tables
  WHERE schemaname = 'public'
    AND tablename NOT IN ($EXCLUDED_QUOTED)
" | tr -d '[:space:]')
if [[ -z "$TABLE_LIST" ]]; then
  echo "No tables found in local DB. Aborting."
  exit 1
fi

# Targeted mode: keep only the requested tables, validating each against the
# live non-excluded set so a typo or an excluded/local-only name fails loudly
# instead of silently syncing nothing (or everything).
if [[ -n "$REQUESTED_TABLES" ]]; then
  SELECTED=""
  for want in ${REQUESTED_TABLES//,/ }; do
    found=0
    for have in ${TABLE_LIST//,/ }; do
      [[ "$want" == "$have" ]] && { found=1; break; }
    done
    if [[ "$found" -eq 0 ]]; then
      echo "✗ --tables: '$want' is not a syncable table (unknown, excluded, or local-only)."
      echo "  Syncable: ${TABLE_LIST//,/, }"
      exit 2
    fi
    SELECTED="${SELECTED:+$SELECTED,}$want"
  done
  TABLE_LIST="$SELECTED"
fi

# ── --tables staleness preflight (#249) ─────────────────
# `--tables` is TRUNCATE + restore-from-local. So the question that decides
# whether a push is safe is not "who owns this table" but "is local at least as
# complete as prod?" — anything prod holds that local cannot reproduce is
# DELETED, and the deletion is silent: the restore succeeds, the row counts look
# plausible, and the missing rows are simply gone until something rebuilds them.
#
# Asking the databases beats declaring ownership in a list. Ownership here is
# SEASONAL — `lineup_aggregates` is legitimately laptop-owned right up until
# prod ingests a season's play-by-play, and legitimately prod-owned from the
# first game after — so a static list would be wrong for part of every year and
# would train the operator to reach for the override. Season coverage is the
# thing that actually changes, it is cheap to read, and it is right on both
# sides of the flip with no calendar rule.
#
# Scope: tables carrying a season-like column, which is every table where this
# has bitten. `class_year` is deliberately NOT a candidate — it is a class label
# ('Fr'/'So'), not a season, and `player_season_projection` carries both.
#
# Fails OPEN (warn, continue to the confirm prompt) when prod cannot be read for
# a table, because a table prod does not have yet is a legitimate first push and
# refusing it would break bootstrap. The interactive confirm below still prints
# the exact TRUNCATE.
if [[ -n "$REQUESTED_TABLES" ]]; then
  LOSSES=""
  UNVERIFIED=""
  for t in ${TABLE_LIST//,/ }; do
    SEASON_COL=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
      SELECT column_name FROM information_schema.columns
      WHERE table_schema = 'public' AND table_name = '$t'
        AND column_name IN ('season', 'target_season', 'year')
      ORDER BY CASE column_name
                 WHEN 'season' THEN 1 WHEN 'target_season' THEN 2 ELSE 3 END
      LIMIT 1" 2>/dev/null | tr -d '[:space:]' || true)
    [[ -n "$SEASON_COL" ]] || continue

    # Local's coverage, as a quoted SQL list. Empty when local holds no rows.
    LOCAL_SEASONS=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
      SELECT string_agg(DISTINCT quote_literal($SEASON_COL::text), ',')
      FROM \"$t\"" 2>/dev/null | tr -d '[:space:]' || true)

    # Seasons prod holds that local does not. `NOT IN ()` is a syntax error, so
    # an empty local list becomes TRUE — every prod season is unreplaceable.
    PRED="TRUE"
    [[ -n "$LOCAL_SEASONS" ]] && PRED="$SEASON_COL::text NOT IN ($LOCAL_SEASONS)"
    if ! MISSING=$("${PSQL[@]}" "$PROD_URL" -t -A -F' ' -c "
      SELECT $SEASON_COL::text, count(*)
      FROM \"$t\" WHERE $PRED
      GROUP BY 1 ORDER BY 1" 2>/dev/null); then
      UNVERIFIED="${UNVERIFIED:+$UNVERIFIED }$t"
      continue
    fi
    while read -r season rows; do
      [[ -n "$season" ]] || continue
      LOSSES="${LOSSES}    $(printf '%-28s' "$t") prod has $SEASON_COL $season ($rows rows), local has none"$'\n'
    done <<< "$MISSING"
  done

  if [[ -n "$UNVERIFIED" ]]; then
    echo "  ! Could not read prod coverage for: $UNVERIFIED"
    echo "    (absent on prod is normal for a first push) — staleness UNVERIFIED for those."
    echo
  fi

  if [[ -n "$LOSSES" ]]; then
    echo "✗ Local is BEHIND prod — this --tables push would DELETE rows prod holds"
    echo "  and local cannot replace:"
    echo
    printf '%s' "$LOSSES"
    echo
    echo "  --tables is TRUNCATE + restore-from-local, so those rows go away and"
    echo "  nothing reports it. Two ways forward, and the second is usually right:"
    echo
    echo "    1. Bring local up to date for those seasons, then push."
    echo "    2. Drop that table from --tables. Prod produces some of these itself"
    echo "       (the nightly computes lineup_aggregates / player_on_off from the"
    echo "       PBP it ingests, and team_preseason_projection for the forecast"
    echo "       season), which means local is not merely behind — it is the wrong"
    echo "       writer. Ownership table: docs/tipoff_self_sufficiency_plan.md §3."
    echo
    if [[ "$FORCE_TABLES" -eq 1 ]]; then
      echo "  ! --force-tables given — proceeding anyway, and accepting the deletion."
      echo
    else
      echo "  Override with --force-tables only if you know why prod's rows are wrong."
      exit 4
    fi
  fi
fi

# pg_dump must restrict to the selected tables too, else the dump still carries
# every table's data even when we only TRUNCATE/restore a subset. Build -t flags
# in targeted mode (full mode keeps the simple -T exclude flags above).
TABLE_FLAGS=()
if [[ -n "$REQUESTED_TABLES" ]]; then
  for t in ${TABLE_LIST//,/ }; do
    TABLE_FLAGS+=("-t" "$t")
  done
fi

echo "→ Local:    $(mask_url "$LOCAL_URL")"
echo "→ Prod:     $(mask_url "$PROD_URL")"
# The table list belongs to the dump/restore modes. Printing it under --columns
# put all 30 syncable tables on screen directly above a one-column merge's
# production confirm prompt — in a script whose whole safety model is "the
# prompt prints the exact operation, read it". Worse in the other direction: it
# reassured the reader that a table was included when the merge never touches it.
if [[ -z "$REQUESTED_COLUMNS" ]]; then
  if [[ -n "$REQUESTED_TABLES" ]]; then
    echo "→ Mode:     TARGETED (--tables) — only the tables below are touched on prod"
  fi
  echo "→ Tables:   ${TABLE_LIST//,/, }"
  echo "→ Excluded: ${EXCLUDED[*]} (stay local — never pushed to prod)"
fi
echo

# Fail fast on a bad PROD_DATABASE_URL so we don't waste time dumping.
echo "→ Verifying prod connection..."
if ! "${PSQL[@]}" "$PROD_URL" -t -A -c "SELECT 1" >/dev/null 2>&1; then
  echo "  ✗ Cannot connect to prod. Check PROD_DATABASE_URL."
  exit 1
fi
echo "  ✓ reachable"
echo

# ---------------------------------------------------------------------------
# COLUMN-MERGE mode (--columns table.col[,col…]).
#
# WHY IT EXISTS. `--tables` replaces a table by TRUNCATE ... CASCADE + restore,
# which is only safe for leaf tables. A table that is *referenced* by foreign
# keys cannot go that route: `players` is referenced by 10 tables, so
# `--tables players` would wipe `player_game_stats` and nine others as
# collateral. But derived columns do land on such tables — `players.display_name`
# is computed locally by `compute_all` step 21 and has no other way to reach
# prod, because a full sync (the only mode that carries `players`) is refused
# while prod is cron-fed.
#
# Rather than a bespoke side script, this is the same "compute locally, push to
# prod" path the rest of the file implements, narrowed to the one safe
# operation available on an FK-referenced table: UPDATE existing rows, named
# columns only.
#
# SAFETY, by construction rather than by care:
#   * no TRUNCATE  → nothing can cascade
#   * no INSERT    → cannot invent rows prod's pipeline didn't create
#   * no DELETE    → cannot lose rows
#   * only the named columns are written; every other column is untouched
#   * rows present locally but not on prod are silently skipped (0-row UPDATEs)
# The worst case is that the named column ends up matching local, which is the
# entire intent. That is why this mode is NOT gated by the full-sync guard —
# it is strictly narrower than `--tables`, which also isn't gated.
#
# Rows are matched on the table's natural key, read from its UNIQUE constraint
# rather than hardcoded, so this works for any table that has one. `players`
# keys on (natstat_id, season). Deliberately NOT the primary key: `id` is a
# locally-generated UUID, and matching on the natural key means a row whose
# UUID ever diverged between the two databases still lands on the right player.
if [[ -n "$REQUESTED_COLUMNS" ]]; then
  MERGE_TABLE="${REQUESTED_COLUMNS%%.*}"
  MERGE_COLS="${REQUESTED_COLUMNS#*.}"
  if [[ "$MERGE_TABLE" == "$REQUESTED_COLUMNS" || -z "$MERGE_COLS" ]]; then
    echo "✗ --columns wants table.col[,col…] (e.g. players.display_name)" >&2
    exit 2
  fi
  for e in "${EXCLUDED[@]}"; do
    if [[ "$e" == "$MERGE_TABLE" ]]; then
      echo "✗ '$MERGE_TABLE' is EXCLUDED — it is prod-written and never pushed." >&2
      exit 2
    fi
  done

  # The match key comes from an ALLOWLIST, not from the catalog.
  #
  # Discovering it from `pg_index` was the source of most of this mode's history:
  # over four review rounds it accepted INCLUDE payload columns, invalid indexes,
  # nondeterministic ties, single- then composite- surrogate UUID keys, and a
  # nullable unique column — each fix opening the next case. None of that
  # generality was ever used: this mode exists to carry `players.display_name`
  # onto prod's historical seasons.
  #
  # So the supported merges are enumerated here, `table:key,cols`, and a table
  # not on the list is refused. Adding one is a deliberate act with a reviewer:
  # the key must be a REAL natural key — stable across databases, NOT NULL, and
  # not a locally-generated uuid (`players.id` is `gen_random_uuid()`, season-
  # scoped, and re-minted by prod's own box-score ingest, so matching on it would
  # assume exactly the agreement this mode exists not to assume).
  MERGE_ALLOWLIST=(
    "players:natstat_id,season"
  )
  KEY_COLS=""
  for entry in "${MERGE_ALLOWLIST[@]}"; do
    if [[ "${entry%%:*}" == "$MERGE_TABLE" ]]; then KEY_COLS="${entry#*:}"; fi
  done
  if [[ -z "$KEY_COLS" ]]; then
    echo "✗ column merge is not supported for '$MERGE_TABLE'." >&2
    echo "  Supported: ${MERGE_ALLOWLIST[*]%%:*}" >&2
    echo "  A table qualifies only if it has a real natural key (stable across" >&2
    echo "  databases, NOT NULL, no locally-generated uuid). Add it to" >&2
    echo "  MERGE_ALLOWLIST with that key once someone has checked it holds." >&2
    echo >&2
    echo "  Do NOT reach for --tables as a substitute unless the table is a" >&2
    echo "  local-only derived leaf (player_rapm, player_on_off). For anything" >&2
    echo "  prod's nightly writes — team_season_stats, game_forecasts — --tables" >&2
    echo "  is a TRUNCATE + restore from this laptop's staler copy, i.e. exactly" >&2
    echo "  the in-season rollback the full-sync guard refuses." >&2
    exit 2
  fi

  # Assert the allowlist's own precondition rather than trusting the comment
  # above it. A NULLABLE key column silently matches nothing for its NULL rows
  # (SQL NULL never equals NULL) and does not enforce uniqueness either — and
  # this schema has exactly that trap sitting in plain sight: `games.natstat_id`
  # is UNIQUE but nullable. Checking here means a future allowlist entry cannot
  # reintroduce it by inspection alone.
  NULLABLE_KEY=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
    SELECT string_agg(a.attname, ', ')
      FROM unnest(string_to_array('$KEY_COLS', ',')) AS w(col)
      JOIN pg_attribute a ON a.attrelid = '\"$MERGE_TABLE\"'::regclass
                         AND a.attname = w.col AND a.attnum > 0
     WHERE NOT a.attnotnull" | trim)
  if [[ -n "$NULLABLE_KEY" ]]; then
    echo "✗ key column(s) nullable on $MERGE_TABLE: $NULLABLE_KEY" >&2
    echo "  A NULL key matches nothing and enforces nothing — fix the" >&2
    echo "  MERGE_ALLOWLIST entry (or the schema) before merging." >&2
    exit 2
  fi

  # Schema checks, before generating anything and before the confirm prompt —
  # local first (below), then prod. Prod has to be asked at all because every
  # other introspection here reads local, which is the wrong database to ask
  # whether prod can accept the write: the motivating column
  # (`players.display_name`) arrived in a recent migration, so the likeliest
  # failure for this mode is "prod hasn't deployed that migration yet". Without
  # this the operator confirms a production write and *then* watches psql abort
  # on a missing column — and `--dry-run` could not warn either, since it also
  # only looked at local.
  # Local first, so the diagnosis is right. Asking prod first made a plain typo
  # ("--columns coaches.name", where the column is `canonical_name` and exists in
  # neither database) report as "prod is behind on migrations", sending the
  # operator to check a deploy that is fine.
  MISSING_COLS_SQL="
    SELECT string_agg(w.col, ', ')
      FROM unnest(string_to_array('${KEY_COLS},${MERGE_COLS}', ',')) AS w(col)
     WHERE NOT EXISTS (
       SELECT 1 FROM information_schema.columns c
        WHERE c.table_schema = 'public' AND c.table_name = '$MERGE_TABLE'
          AND c.column_name = w.col)"
  MISSING_LOCALLY=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "$MISSING_COLS_SQL" | trim)
  if [[ -n "$MISSING_LOCALLY" ]]; then
    echo "✗ '$MERGE_TABLE' has no column(s): $MISSING_LOCALLY" >&2
    echo "  (checked locally — check the spelling against \\d $MERGE_TABLE)" >&2
    exit 2
  fi
  # A merge column that is part of the match key can never differ from itself, so
  # the run is guaranteed to update 0 rows — an outcome the operator would then
  # have to interpret against the counts, when the request was simply impossible.
  for c in ${MERGE_COLS//,/ }; do
    for k in ${KEY_COLS//,/ }; do
      if [[ "$c" == "$k" ]]; then
        echo "✗ '$c' is part of the match key (${KEY_COLS//,/, }) — merging it onto" >&2
        echo "  itself can only ever update 0 rows. Pick a non-key column." >&2
        exit 2
      fi
    done
  done

  MISSING_ON_PROD=$("${PSQL[@]}" "$PROD_URL" -t -A -c "$MISSING_COLS_SQL" | trim)
  if [[ -n "$MISSING_ON_PROD" ]]; then
    echo "✗ prod's '$MERGE_TABLE' is missing: $MISSING_ON_PROD" >&2
    echo "  Prod is behind on migrations, or the column is local-only." >&2
    echo "  Deploy first — a merge would abort mid-transaction." >&2
    exit 2
  fi

  # Existence is not enough — the generated SQL also assumes prod's TYPES match
  # local's, because every `::type` cast is read from the local catalog. A prod
  # that is behind a widening migration (`varchar(50)` where local has `text`)
  # passes the existence check and then aborts the confirmed transaction on the
  # first oversized value, which is the precise failure the precheck exists to
  # move in front of the prompt.
  TYPES_SQL="
    SELECT string_agg(format('%s %s', w.col, format_type(a.atttypid, a.atttypmod)), '; '
                      ORDER BY w.col)
      FROM unnest(string_to_array('${KEY_COLS},${MERGE_COLS}', ',')) AS w(col)
      JOIN pg_attribute a ON a.attrelid = '\"$MERGE_TABLE\"'::regclass
                         AND a.attname = w.col AND a.attnum > 0"
  LOCAL_TYPES=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "$TYPES_SQL" | trim)
  PROD_TYPES=$("${PSQL[@]}" "$PROD_URL" -t -A -c "$TYPES_SQL" | trim)
  if [[ "$LOCAL_TYPES" != "$PROD_TYPES" ]]; then
    echo "✗ column types differ between local and prod:" >&2
    echo "    local: $LOCAL_TYPES" >&2
    echo "    prod:  $PROD_TYPES" >&2
    echo "  The generated casts come from local, so the merge would abort" >&2
    echo "  mid-transaction. Deploy the migration first." >&2
    exit 2
  fi

  # And the match key must actually BE a key on prod. The allowlist asserts it is
  # one *here*; that says nothing about a prod running behind the migration which
  # added the unique index, where the same key can match SEVERAL rows per tuple —
  # one local row written over all of them, with the inflated count reported as
  # success.
  PROD_KEY_UNIQUE=$("${PSQL[@]}" "$PROD_URL" -t -A -c "
    SELECT 1
      FROM pg_index i
     WHERE i.indrelid = '\"$MERGE_TABLE\"'::regclass
       AND i.indisunique AND i.indisvalid
       AND i.indpred IS NULL AND i.indexprs IS NULL
       -- Compare as a SET, not as an ordered list. Uniqueness does not depend on
       -- column order, so a prod index spelled (season, natstat_id) enforces
       -- exactly the same constraint as local's (natstat_id, season) — and
       -- string-comparing the joined names would refuse a perfectly safe merge
       -- while blaming prod for a missing index it actually has.
       AND (SELECT array_agg(a.attname::text ORDER BY a.attname::text)
              FROM unnest(i.indkey::int[]) WITH ORDINALITY AS k(attnum, ord)
              JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
             WHERE k.ord <= i.indnkeyatts)
           = (SELECT array_agg(c::text ORDER BY c::text)
                FROM unnest(string_to_array('$KEY_COLS', ',')) AS c)
     LIMIT 1" | trim)
  if [[ -z "$PROD_KEY_UNIQUE" ]]; then
    echo "✗ (${KEY_COLS//,/, }) is not a unique index on PROD, only locally." >&2
    echo "  The merge would match more than one prod row per key tuple." >&2
    echo "  Deploy the migration that adds it first." >&2
    exit 2
  fi

  # Season scoping — the fix for this mode's sharpest edge.
  #
  # The merge is a whole-table operation, but `players.display_name` (its
  # motivating column) is computed ON PROD every night by `compute_all` step 21
  # for the CURRENT season. Pushing every local row therefore does exactly what
  # the P0 full-sync guard exists to prevent, just one column wide: the laptop's
  # staler current-season values (often NULL, if local's Torvik copy is a day
  # behind) overwrite prod's fresh ones, and every live player page shows bare
  # legal names until the next nightly recomputes ~24h later.
  #
  # So when prod looks live — the same two signals the P0 guard uses — a table
  # carrying a `season` column is merged for PAST seasons only. That matches the
  # real ownership split: prod owns the current season, the laptop owns history.
  # Off-season (cron quiet and calendar out-of-season) the whole table merges,
  # which is the bootstrap case. `--force-full` overrides, since an operator who
  # has said "I mean it" for a full sync means it here too.
  SEASON_FILTER=""
  HAS_SEASON=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
    SELECT 1 FROM information_schema.columns
     WHERE table_schema='public' AND table_name='$MERGE_TABLE'
       AND column_name='season'" | trim)
  PROD_LIVE=0
  AGE_H=$(prod_nightly_age_hours)
  if [[ "$AGE_H" =~ ^[0-9]+$ ]] && [[ "$AGE_H" -lt "$STALE_AFTER_HOURS" ]]; then PROD_LIVE=1; fi
  if in_season_now; then PROD_LIVE=1; fi
  # Say which of the three reasons actually applies. A blanket "no season column,
  # or prod is not cron-fed" would have been a lie in the --force-full case, and
  # the whole point of printing the scope is that the operator can check it.
  if [[ -z "$HAS_SEASON" ]]; then
    SCOPE_NOTE="every row — $MERGE_TABLE has no season column"
  elif [[ "$PROD_LIVE" -eq 0 ]]; then
    SCOPE_NOTE="every season — prod is not currently cron-fed (off-season/bootstrap)"
  elif [[ "$FORCE_FULL" -eq 1 ]]; then
    CUR_SEASON=$(current_season)
    SCOPE_NOTE="every season INCLUDING $CUR_SEASON — --force-full given, so this can
            overwrite what prod's nightly computed today"
  else
    CUR_SEASON=$(current_season)
    SEASON_FILTER="WHERE season <> $CUR_SEASON"
    SCOPE_NOTE="seasons other than $CUR_SEASON — prod owns the current season while
            its cron is live (--force-full to merge every season)"
  fi

  # Batched, not row-at-a-time. 59k single-row UPDATEs is 59k round trips to a
  # database across the internet — the same N+1 that made the Torvik per-game
  # persist take ~10 min against prod before it was batched (ingest/torvik.rs).
  # One `UPDATE … FROM (VALUES …)` per MERGE_CHUNK rows makes it ~120
  # statements. Literals are rendered by quote_nullable in SQL, so quoting stays
  # Postgres' problem rather than bash's.
  MERGE_CHUNK=500
  # One catalog round trip for every type we need, not one per column — the
  # type-divergence precheck above already had to fetch exactly this set, and a
  # second source for the same values is both slower and a place for the two to
  # drift apart. Types are read whole: NOT stripped of internal spaces, since
  # Postgres names them `double precision` / `timestamp without time zone` /
  # `character varying(50)`, and collapsing those produced casts like
  # `::doubleprecision` that parse in bash and then abort the transaction on
  # prod — after the operator confirmed the write.
  # A newline-delimited `col|type` table, not a bash associative array: this
  # script runs on macOS, whose /bin/bash is 3.2 and has no `declare -A`. The
  # lookup is an awk pass over a handful of lines, which costs nothing next to
  # the psql round trip it replaces.
  TYPE_MAP=$("${PSQL[@]}" "$LOCAL_URL" -t -A -F'|' -c "
    SELECT w.col, format_type(a.atttypid, a.atttypmod)
      FROM unnest(string_to_array('${KEY_COLS},${MERGE_COLS}', ',')) AS w(col)
      JOIN pg_attribute a ON a.attrelid = '\"$MERGE_TABLE\"'::regclass
                         AND a.attname = w.col AND a.attnum > 0")
  coltype() { awk -F'|' -v c="$1" '$1 == c { print $2; exit }' <<<"$TYPE_MAP"; }

  TUPLE_EXPR=""; ALIAS_COLS=""; SET_EXPR=""; WHERE_EXPR=""; DIST_T=""; DIST_V=""
  for k in ${KEY_COLS//,/ }; do
    ktype=$(coltype "$k")
    TUPLE_EXPR="${TUPLE_EXPR:+$TUPLE_EXPR || ',' || }quote_nullable(\"$k\")"
    ALIAS_COLS="${ALIAS_COLS:+$ALIAS_COLS, }\"$k\""
    # Cast on the VALUES side: literals arrive as `unknown`, and the real column
    # may be int/uuid/etc. Reading the type keeps this generic across tables.
    WHERE_EXPR="${WHERE_EXPR:+$WHERE_EXPR AND }t.\"$k\" = v.\"$k\"::$ktype"
  done
  for c in ${MERGE_COLS//,/ }; do
    ctype=$(coltype "$c")
    if [[ -z "$ctype" ]]; then
      echo "✗ column '$c' does not exist on $MERGE_TABLE" >&2
      exit 2
    fi
    TUPLE_EXPR="$TUPLE_EXPR || ',' || quote_nullable(\"$c\")"
    ALIAS_COLS="$ALIAS_COLS, \"$c\""
    SET_EXPR="${SET_EXPR:+$SET_EXPR, }\"$c\" = v.\"$c\"::$ctype"
    DIST_T="${DIST_T:+$DIST_T, }t.\"$c\""
    DIST_V="${DIST_V:+$DIST_V, }v.\"$c\"::$ctype"
  done

  # Enforce the season scope on PROD's side too, not just on which local rows
  # were selected. Gated on the TABLE having a season column, not on the KEY
  # having one: where the key carries `season` this predicate is already implied
  # by the key join, and where it does not — `games` keyed on `natstat_id` alone
  # — gating on the key omitted it from exactly the case that needs it. A local
  # past-season row whose `natstat_id` collides with a prod current-season row
  # (this repo has hit cross-season natstat_id collisions from typo'd NatStat
  # dates) would overwrite live data while the banner claimed the current season
  # was out of scope.
  if [[ -n "$SEASON_FILTER" ]]; then
    WHERE_EXPR="$WHERE_EXPR AND t.season <> $CUR_SEASON"
  fi

  # Only touch rows that actually change. Without this every row of the table is
  # rewritten on every merge — 59k row versions on `players` to move 2k real
  # values — which is pure bloat and WAL for prod's autovacuum to clean up.
  # Row-constructor form so one column and many read the same.
  WHERE_EXPR="$WHERE_EXPR AND ($DIST_T) IS DISTINCT FROM ($DIST_V)"

  echo "→ Mode:     COLUMN MERGE — UPDATE only, no TRUNCATE/INSERT/DELETE"
  echo "→ Table:    $MERGE_TABLE"
  echo "→ Columns:  ${MERGE_COLS//,/, }"
  echo "→ Match on: ${KEY_COLS//,/, }  (natural key, from MERGE_ALLOWLIST)"
  echo "→ Scope:    $SCOPE_NOTE"
  echo

  STATEMENTS=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "
    WITH r AS (
      SELECT row_number() OVER (ORDER BY $KEY_COLS) AS rn,
             '(' || ($TUPLE_EXPR) || ')' AS tup
        FROM \"$MERGE_TABLE\" $SEASON_FILTER
    )
    SELECT 'UPDATE \"$MERGE_TABLE\" AS t SET $SET_EXPR FROM (VALUES '
        || string_agg(tup, ',' ORDER BY rn)
        || ') AS v($ALIAS_COLS) WHERE $WHERE_EXPR;'
      FROM r GROUP BY (rn - 1) / $MERGE_CHUNK ORDER BY min(rn)")
  N=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c \
    "SELECT count(*) FROM \"$MERGE_TABLE\" $SEASON_FILTER" | trim)
  B=$(grep -c '^UPDATE' <<<"$STATEMENTS" || true)
  echo "→ $N row(s) offered from local, in $B batched statement(s)"
  echo "  (rows whose named columns already match prod are skipped, not rewritten)"

  # Nothing in scope is a legitimate outcome (a season-scoped merge on a table
  # local only has current-season rows for), and it must stop here: `STATEMENTS`
  # is empty, so continuing would send psql a lone SET and then prompt for a
  # production write that could not do anything.
  if [[ "${N:-0}" -eq 0 || -z "$STATEMENTS" ]]; then
    echo "→ Nothing to merge in this scope. No prod write attempted."
    exit 0
  fi

  # NULLs are values here, not absences: `compute_display_names` writes NULL
  # deliberately when the display name would only repeat `name`. So a local NULL
  # is indistinguishable from "local never computed this", and merging it CLEARS
  # whatever prod holds. Season scoping covers the current season, but prod owns
  # every season it was ever current for, so this stays a live risk one rollover
  # later.
  #
  # Two attempts at turning this into a NUMBER both failed, so it is stated as a
  # property instead. Counting local NULLs alarms on 52k of 54k rows for
  # `display_name` — its by-design shape, i.e. noise every run. Comparing
  # scope-wide non-NULL counts between the two databases is worse than useless:
  # it fires when prod merely holds more ROWS in scope (aborting a correct
  # merge), and stays silent when local and prod hold equal counts on DISJOINT
  # rows, which is real clearing. An honest number needs per-row matching, i.e.
  # the payload — the cost this mode is built to avoid. So say the semantic once,
  # plainly, and let the operator judge it against what they just recomputed.
  echo "  NOTE: a NULL in local is a value here, not an absence — it CLEARS whatever"
  echo "        prod holds for that row. Merge a column you have just recomputed."

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "→ Dry run — would apply these on prod in ONE transaction."
    echo "  First batch (head, then its WHERE — the part that carries the semantics):"
    head -1 <<<"$STATEMENTS" | cut -c1-200 | sed 's/^/    /'
    echo "    … "
    # The old preview cut at 260 characters, which is all VALUES tuples and never
    # reached the WHERE — so the one clause an operator needs to check (season
    # predicate, key join, IS DISTINCT FROM guard) was the one part never shown.
    #
    # `|| true` because `set -o pipefail` is on: a value containing a newline
    # makes the statement span lines, `head -1` then yields a fragment with no
    # WHERE, and an unguarded grep would exit 1 and kill the dry run mid-preview
    # with no message. The sibling `grep -c … || true` above is the same guard.
    head -1 <<<"$STATEMENTS" | grep -o 'WHERE .*' | sed 's/^/    /' || true
    exit 0
  fi

  read -r -p "→ Apply to PROD? UPDATEs the differing rows of $MERGE_TABLE.${MERGE_COLS} (≤$N) [y/N] " confirm
  [[ "$confirm" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 1; }

  START=$(date +%s)
  # `lock_timeout` bounds how long THIS transaction waits for a row lock before
  # giving up — nothing more. It does NOT decide who loses a deadlock: that is
  # `deadlock_timeout` and Postgres' own victim selection, which can just as
  # easily abort the nightly's `compute` step (a failed served-critical step, a
  # degraded Slack post, a 503 from /api/health/ingest). Nor does it bound how
  # long this transaction BLOCKS the nightly once it holds the locks.
  #
  # So the timeout is a partial mitigation, not the guarantee an earlier version
  # of this comment claimed. The actual protection is not overlapping: the cron
  # runs 09:30 UTC, and `--prod-status` shows when it last ran. Season scoping
  # narrows the exposure further by keeping the merge off the rows the nightly
  # rewrites — for every table that has a `season` column, since the predicate
  # is applied to prod's side of the UPDATE regardless of what the key contains.
  #
  # NOT --quiet: psql's `UPDATE n` command tags are the only evidence that the
  # natural key actually matched prod rows. Without them a merge that matched
  # NOTHING (diverged keys, wrong table) printed the same cheerful "✓ Merged" as
  # a successful one, and the follow-up non-NULL count is equally unchanged in
  # both cases. Summing the tags turns silent no-ops into a loud warning.
  APPLY_OUT=$(printf 'SET lock_timeout = %s;\n%s\n' "'30s'" "$STATEMENTS" \
    | "${PSQL[@]}" "$PROD_URL" -v ON_ERROR_STOP=1 --single-transaction -f -)
  ROWS=$(awk '/^UPDATE [0-9]+$/ { s += $2 } END { print s + 0 }' <<<"$APPLY_OUT")
  echo "✓ Merged in $(($(date +%s) - START))s — $ROWS row(s) actually updated on prod."
  if [[ "$ROWS" -eq 0 ]]; then
    # Zero updated is genuinely ambiguous — with the IS DISTINCT FROM guard it is
    # the normal result of any second run — and two attempts at RESOLVING it
    # automatically both misfired: a sampled key probe called one surviving tuple
    # "agreement", and then strict equality against the sample called one missing
    # row a permanent failure. Worse, swallowing the probe's own errors turned a
    # dropped connection into a confident accusation that the key was broken.
    #
    # So it is stated as ambiguous, with the counts below (local vs prod, same
    # scope) as the thing to read. Those numbers cannot lie about a wrong
    # database or an empty local table, and they need no payload to compute.
    echo "  Zero rows changed. That is the normal result of a re-run; it is also"
    echo "  what a wrong PROD_DATABASE_URL or an empty local table looks like."
    echo "  Compare the two counts below before treating it as a no-op."
  fi
  # Scoped the same way the merge was, and labelled with that scope. Printing an
  # unscoped whole-table count here while the merge covered past seasons only
  # showed the operator two numbers for what reads as the same quantity — one
  # including the current season, one not.
  # Both sides, same scope, side by side. One number invites a story; two let the
  # operator see the answer — prod far below local means the merge did not land,
  # local far below prod means this laptop was the stale one and just cleared
  # values. Neither line makes a claim, which is why neither can be wrong.
  if [[ -n "$SEASON_FILTER" ]]; then
    NN_WHERE="$SEASON_FILTER AND"
    echo "  Non-NULL counts, seasons other than $CUR_SEASON (the merged scope):"
  else
    NN_WHERE="WHERE"
    echo "  Non-NULL counts, whole table (the merged scope):"
  fi
  printf "    %-25s %10s %10s\n" "" "local" "prod"
  for c in ${MERGE_COLS//,/ }; do
    NN_SQL="SELECT count(*) FROM \"$MERGE_TABLE\" $NN_WHERE \"$c\" IS NOT NULL"
    lv=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "$NN_SQL" | trim)
    pv=$("${PSQL[@]}" "$PROD_URL" -t -A -c "$NN_SQL" | trim)
    printf "    %-25s %10s %10s\n" "$c" "$lv" "$pv"
  done
  exit 0
fi

# ---------------------------------------------------------------------------
# P0 guard — refuse a full replace while prod is live.
# (docs/intraseason_data_safety_plan.md R1; issue #187.)
#
# A full sync TRUNCATEs and replaces every serving table with this laptop's
# copy. In-season that is a silent rollback: the Railway cron (`cstat-ingest
# nightly`, 09:30 UTC) owns those tables on prod and upserts them nightly, so it
# is fresher than local *by construction*. A reflexive full sync regresses box
# scores, forecasts, AdjEM and CamPom on the live site, and leaves no trace
# anywhere (R5) — the first signal would be the M5a row-count gate firing the
# NEXT night, or a user noticing. Before this guard, the only thing standing
# between that and a live site was prose in a doc header.
#
# Two independent signals, either of which blocks:
#   1. Prod is actively cron-fed — a served-critical step succeeded within
#      STALE_AFTER_HOURS. PRIMARY, because it is data-driven: it self-adjusts
#      to tournament runs, early/late tip, and the simulate harness.
#   2. The calendar says in-season. SECONDARY and zero-dependency: it still
#      fires when the ledger is unreadable or the cron has been failing — i.e.
#      exactly when signal 1 goes quiet for a bad reason rather than a good one.
#
# Targeted mode is NOT gated: --tables is the intended in-season path, and is
# how the heavy local-only derived tables (PBP/RAPM/archetype outputs, which
# prod cannot compute — it holds no play_by_play) legitimately reach prod.
if [[ -z "$REQUESTED_TABLES" ]]; then
  BLOCK_REASONS=()
  AGE_H=$(prod_nightly_age_hours)
  if [[ "$AGE_H" =~ ^[0-9]+$ ]] && [[ "$AGE_H" -lt "$STALE_AFTER_HOURS" ]]; then
    BLOCK_REASONS+=("prod recorded a successful nightly step ${AGE_H}h ago — the cron owns the serving tables and is fresher than this laptop")
  fi
  if in_season_now; then
    BLOCK_REASONS+=("today ($(date -u +%Y-%m-%d)) is in-season by the calendar")
  fi

  if [[ ${#BLOCK_REASONS[@]} -gt 0 ]]; then
    echo "  ! FULL SYNC BLOCKED — prod looks live:"
    for r in "${BLOCK_REASONS[@]}"; do echo "      - $r"; done
    echo
    echo "    A full sync replaces EVERY serving table with this machine's copy,"
    echo "    rolling the live site back to whatever local last computed."
    echo
    echo "    Instead:"
    echo "      - push only what you regenerated:  --tables <leaf,tables>"
    echo "      - inspect prod without writing:    --prod-status"
    echo "      - if you truly mean a full replace: --force-full"
    echo
    if [[ "$FORCE_FULL" -eq 1 ]]; then
      echo "  ! --force-full given — proceeding with a FULL REPLACE of prod anyway."
      echo
    elif [[ "$DRY_RUN" -eq 1 ]]; then
      # Report but don't block: a dry run writes nothing, and its job is to show
      # what a real run would do — including that a real run would refuse.
      echo "  → --dry-run: continuing to preview (a real run would abort here)."
      echo
    else
      exit 3
    fi
  fi
fi

echo "→ Local row counts:"
for t in ${TABLE_LIST//,/ }; do
  c=$("${PSQL[@]}" "$LOCAL_URL" -t -A -c "SELECT count(*) FROM \"$t\"" | tr -d '[:space:]')
  printf "    %-25s %s\n" "$t" "$c"
done
echo

# Dump in custom format to a host-side temp file. When pg_dump runs inside
# the docker container, its stdout still streams out through `docker exec`,
# so the redirect captures everything host-side regardless of mode.
TMPFILE=$(mktemp -t cstat-sync.XXXXXX)
trap 'rm -f "$TMPFILE"' EXIT

echo "→ Dumping local data (custom binary format)..."
"${PG_DUMP[@]}" "$LOCAL_URL" \
  --format=custom \
  --data-only \
  --no-owner \
  --no-privileges \
  --compress=6 \
  "${EXCLUDE_FLAGS[@]}" \
  ${TABLE_FLAGS[@]+"${TABLE_FLAGS[@]}"} \
  > "$TMPFILE"

DUMP_SIZE=$(du -h "$TMPFILE" | cut -f1 | tr -d '[:space:]')
echo "  → ${DUMP_SIZE} (compressed binary)"
echo

# Guard (issue #186): assert none of the excluded tables' sequences actually
# rode along in the dump. Belt-and-braces over the -T flags above — it verifies
# pg_dump honoured them, so a version or quoting change can't quietly reopen
# #186. An escaped `SEQUENCE SET` would setval() prod's sequence down to this
# machine's value and silently break inserts on the live pipeline, which is
# worth failing the sync over rather than finding in the prod logs a day later.
# Reads only the dump's table-of-contents, so it stays cheap on a multi-GB
# dump. Runs before the dry-run exit — a dry run should surface what a real
# sync would.
TOC=$("${PG_RESTORE[@]}" --list < "$TMPFILE")
SEQ_LEAKS=""
for s in ${EXCLUDED_SEQS//,/ }; do
  # Explicit `if` rather than `[[ … ]] && …`: under `set -e` an AND-list whose
  # test fails yields a non-zero status for the whole list, which is a footgun
  # a future edit could easily trip. Not worth the terseness in a prod guard.
  if grep -qE "SEQUENCE SET public ${s}( |\$)" <<<"$TOC"; then
    SEQ_LEAKS="${SEQ_LEAKS:+$SEQ_LEAKS }$s"
  fi
done
if [[ -n "$SEQ_LEAKS" ]]; then
  echo "  ✗ Dump carries sequence state for EXCLUDED tables: ${SEQ_LEAKS}"
  echo "    Restoring this would rewind prod's sequence to this machine's value,"
  echo "    breaking prod inserts until it catches up (issue #186)."
  echo "    Fix: ensure EXCLUDE_FLAGS covers the sequence above."
  exit 1
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "→ Dry run — would TRUNCATE the tables above CASCADE on prod and restore via COPY."
  echo "  ✓ No excluded-table sequence state in dump"
  echo "  Dump table-of-contents (data sections):"
  # Reuses the TOC already read for the guard above.
  grep -E "TABLE DATA|SEQUENCE SET" <<<"$TOC" | sed 's/^/    /' || true
  exit 0
fi

if [[ -n "$REQUESTED_TABLES" ]]; then
  echo "→ TARGETED restore uses TRUNCATE ... CASCADE on: ${TABLE_LIST//,/, }"
  echo "  CASCADE follows foreign keys — if any of these is REFERENCED by another"
  echo "  table, that dependent is wiped too. Only proceed for leaf/derived tables."
fi
read -r -p "→ Apply to PROD? This TRUNCATEs every table above and restores from the dump. [y/N] " confirm
[[ "$confirm" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 1; }

# Build TRUNCATE statement with quoted identifiers.
TRUNCATE_SQL="TRUNCATE TABLE"
for t in ${TABLE_LIST//,/ }; do
  TRUNCATE_SQL="$TRUNCATE_SQL \"$t\","
done
TRUNCATE_SQL="${TRUNCATE_SQL%,} RESTART IDENTITY CASCADE;"

echo "→ Applying to prod (single transaction, COPY-based restore)..."
START=$(date +%s)
{
  # session_replication_role = 'replica' silences FK / trigger checks for
  # this session — safe because TRUNCATE CASCADE wiped everything first, so
  # all references in the dump resolve by construction (and the dump is
  # ordered by FK dependencies anyway).
  echo "SET session_replication_role = 'replica';"
  echo "$TRUNCATE_SQL"
  # pg_restore -f - emits the restore as SQL (with COPY statements) on
  # stdout, which we splice into the same transaction. -f - is required on
  # pg_restore 17+; older versions default to stdout. Read the dump via
  # stdin so this works across the docker exec boundary (host TMPFILE path
  # is invisible inside the container).
  "${PG_RESTORE[@]}" --data-only --no-owner --no-privileges -f - < "$TMPFILE"
  echo "SET session_replication_role = 'origin';"
} | "${PSQL[@]}" "$PROD_URL" -v ON_ERROR_STOP=1 --single-transaction --quiet

ELAPSED=$(($(date +%s) - START))
echo
echo "✓ Sync complete in ${ELAPSED}s. Verifying prod row counts:"
for t in ${TABLE_LIST//,/ }; do
  c=$("${PSQL[@]}" "$PROD_URL" -t -A -c "SELECT count(*) FROM \"$t\"" | tr -d '[:space:]')
  printf "    %-25s %s\n" "$t" "$c"
done
