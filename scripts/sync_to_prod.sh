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
#
# Usage:
#   ./scripts/sync_to_prod.sh              # full run
#   ./scripts/sync_to_prod.sh --dry-run    # preview without applying
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

for arg in "$@"; do
  case "$arg" in
    --dry-run|-n) DRY_RUN=1 ;;
    -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "Unknown arg: $arg"; exit 2 ;;
  esac
done

# Tables to skip on both dump and truncate sides.
EXCLUDED=("api_cache" "_sqlx_migrations")

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

# Build pg_dump -T flags from the EXCLUDED list.
EXCLUDE_FLAGS=()
for t in "${EXCLUDED[@]}"; do
  EXCLUDE_FLAGS+=("-T" "$t")
done

# Discover the live table list from local; new tables get picked up
# automatically without needing to edit this script.
EXCLUDED_QUOTED=$(printf "'%s'," "${EXCLUDED[@]}")
EXCLUDED_QUOTED="${EXCLUDED_QUOTED%,}"
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

mask_url() { sed -E 's|://[^@]+@|://***@|' <<<"$1"; }

echo "→ Local:  $(mask_url "$LOCAL_URL")"
echo "→ Prod:   $(mask_url "$PROD_URL")"
echo "→ Tables: ${TABLE_LIST//,/, }"
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

# Dump in custom format. When using docker, the dump is written *inside* the
# container, so we have to copy it out to the host afterward to feed pg_restore.
# Simpler: dump directly to stdout and capture on the host.
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
  > "$TMPFILE"

DUMP_SIZE=$(du -h "$TMPFILE" | cut -f1 | tr -d '[:space:]')
echo "  → ${DUMP_SIZE} (compressed binary)"
echo

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "→ Dry run — would TRUNCATE the tables above CASCADE on prod and restore via COPY."
  echo "  Dump table-of-contents (data sections):"
  # Read dump from stdin so this works across the docker exec boundary.
  "${PG_RESTORE[@]}" --list < "$TMPFILE" | grep "TABLE DATA" | sed 's/^/    /' || true
  exit 0
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
