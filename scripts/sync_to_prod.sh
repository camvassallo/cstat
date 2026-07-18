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
#   ./scripts/sync_to_prod.sh --tables lineup_aggregates,player_rapm
#   ./scripts/sync_to_prod.sh --force-full             # override the in-season guard
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
# its heavy derived tables (PBP/RAPM/archetype/lineup outputs) without a full
# truncate clobbering what the cron just wrote. Names in EXCLUDED can never be
# selected; an unknown name aborts before any write.
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
FORCE_FULL=0          # --force-full: override the in-season full-sync guard
PROD_STATUS=0         # --prod-status: read-only prod inspection, then exit

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run|-n) DRY_RUN=1; shift ;;
    --tables) REQUESTED_TABLES="${2:?--tables needs a comma-separated list}"; shift 2 ;;
    --tables=*) REQUESTED_TABLES="${1#--tables=}"; shift ;;
    --force-full) FORCE_FULL=1; shift ;;
    --prod-status) PROD_STATUS=1; shift ;;
    -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

mask_url() { sed -E 's|://[^@]+@|://***@|' <<<"$1"; }

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
#   source tables local-only is LOAD-BEARING, not just a size optimization. It is
#   the sole reason the in-season targeted push (--tables lineup_aggregates,
#   player_on_off) can safely own those rollups on prod: with no PBP/lineup rows
#   on prod, the nightly's compute_pbp_lineups no-ops (early-returns) instead of
#   rebuilding them. Ship any of the four to prod and the nightly starts wiping
#   and rebuilding the rollups every night, clobbering the operator's push.
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
  echo "→ Recent FAILED / SKIPPED steps (last 7d):"
  FAILS=$("${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
    SELECT to_char(ended_at AT TIME ZONE 'UTC', 'MM-DD HH24:MI'), rpad(step, 14), status,
           coalesce(left(error, 60), '')
    FROM ingest_runs
    WHERE status <> 'ok' AND ended_at > now() - interval '7 days'
    ORDER BY ended_at DESC LIMIT 10
  " || true)
  if [[ -n "$FAILS" ]]; then sed 's/^/    /' <<<"$FAILS"; else echo "    (none)"; fi
  echo
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
  "${PSQL[@]}" "$PROD_URL" -t -A -F'  ' -c "
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
  " | sed 's/^/    /' || true
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
if [[ -n "$REQUESTED_TABLES" ]]; then
  echo "→ Mode:     TARGETED (--tables) — only the tables below are touched on prod"
fi
echo "→ Tables:   ${TABLE_LIST//,/, }"
echo "→ Excluded: ${EXCLUDED[*]} (stay local — never pushed to prod)"
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
