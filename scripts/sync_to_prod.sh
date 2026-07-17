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
#   ./scripts/sync_to_prod.sh --tables a,b,c           # push only these tables
#   ./scripts/sync_to_prod.sh --tables lineup_aggregates,player_rapm
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

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run|-n) DRY_RUN=1; shift ;;
    --tables) REQUESTED_TABLES="${2:?--tables needs a comma-separated list}"; shift 2 ;;
    --tables=*) REQUESTED_TABLES="${1#--tables=}"; shift ;;
    -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "Unknown arg: $1"; exit 2 ;;
  esac
done

# Tables to skip on both dump and truncate sides. See the header comment for
# the rationale on each (api_cache / _sqlx_migrations: managed elsewhere;
# play_by_play / lineup_stints: local-only raw PBP, never shipped to prod;
# natstat_lineups / natstat_lineup_games: local-only lineups-object capture,
# prod serves only the derived lineup_aggregates / player_on_off;
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

mask_url() { sed -E 's|://[^@]+@|://***@|' <<<"$1"; }

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
