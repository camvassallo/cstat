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
# Excluded tables (intentional):
#   - api_cache: NatStat response cache, only useful during ingestion
#   - _sqlx_migrations: managed by sqlx, never overwrite from a dump
#
# Usage:
#   PROD_DATABASE_URL=postgresql://... ./scripts/sync_to_prod.sh
#   PROD_DATABASE_URL=postgresql://... ./scripts/sync_to_prod.sh --dry-run
#
# Get PROD_DATABASE_URL from Railway:
#   railway variables --service cstat-postgres | grep DATABASE_URL
# or copy the public connection string from the Railway dashboard.

set -euo pipefail

LOCAL_URL="${LOCAL_DATABASE_URL:-postgres://cstat:cstat@localhost:5432/cstat}"
PROD_URL="${PROD_DATABASE_URL:?Set PROD_DATABASE_URL to the Railway prod connection string}"
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
# Postgres docker container. The container ships with matching client tools
# and can reach both local (its own server) and prod (via the host network).
DOCKER_PG="cstat-postgres"
if command -v pg_dump >/dev/null && command -v psql >/dev/null; then
  PG_DUMP=(pg_dump)
  PSQL=(psql)
elif docker ps --format '{{.Names}}' | grep -q "^${DOCKER_PG}\$"; then
  echo "→ Using docker container '${DOCKER_PG}' for psql tools"
  PG_DUMP=(docker exec -i "$DOCKER_PG" pg_dump)
  PSQL=(docker exec -i "$DOCKER_PG" psql)
else
  echo "Need either local psql/pg_dump (brew install postgresql@17) or the"
  echo "'${DOCKER_PG}' container running (docker compose up -d)."
  exit 1
fi

# Build pg_dump -T flags from the EXCLUDED list.
EXCLUDE_FLAGS=()
for t in "${EXCLUDED[@]}"; do
  EXCLUDE_FLAGS+=("-T" "$t")
done

# Discover the live table list from local; new tables get picked up
# automatically without needing to edit this script. Build a SQL IN-list
# of the excluded names so we can filter them out.
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

# Fail fast on a bad PROD_DATABASE_URL so we don't waste time dumping
# hundreds of MB only to discover the connection is wrong.
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

TMPFILE=$(mktemp -t cstat-sync.XXXXXX)
trap 'rm -f "$TMPFILE"' EXIT

echo "→ Dumping data-only snapshot..."
"${PG_DUMP[@]}" "$LOCAL_URL" \
  --data-only \
  --column-inserts \
  --no-owner \
  --no-privileges \
  "${EXCLUDE_FLAGS[@]}" \
  > "$TMPFILE"

DUMP_SIZE=$(du -h "$TMPFILE" | cut -f1 | tr -d '[:space:]')
DUMP_LINES=$(wc -l < "$TMPFILE" | tr -d '[:space:]')
echo "  → ${DUMP_LINES} lines, ${DUMP_SIZE}"
echo

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "→ Dry run — would TRUNCATE the tables above CASCADE on prod and apply the dump."
  echo "  Dump preview (first 15 non-blank lines):"
  grep -v '^$' "$TMPFILE" | head -15 | sed 's/^/    /'
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

echo "→ Applying to prod (single transaction)..."
{
  # Disable FK checks during restore — safe because TRUNCATE CASCADE wipes
  # everything first, so all references resolve by construction.
  echo "SET session_replication_role = 'replica';"
  echo "$TRUNCATE_SQL"
  cat "$TMPFILE"
  echo "SET session_replication_role = 'origin';"
} | "${PSQL[@]}" "$PROD_URL" -v ON_ERROR_STOP=1 --single-transaction --quiet

echo
echo "✓ Sync complete. Verifying prod row counts:"
for t in ${TABLE_LIST//,/ }; do
  c=$("${PSQL[@]}" "$PROD_URL" -t -A -c "SELECT count(*) FROM \"$t\"" | tr -d '[:space:]')
  printf "    %-25s %s\n" "$t" "$c"
done
