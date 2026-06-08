#!/usr/bin/env bash
# onfloor_backfill.sh — backfill API-native on-floor lineups for a season.
#
# Re-fetches play-by-play via the live API (the `playbyplay/{date}` stream carries
# game.onfloorhome / onfloorvis — verified 99% populated for 2026), so
# compute_pbp_lineups auto-switches from ~86%-accurate SUB-replay to the exact
# on-floor five. This is the real fix for the SUB-replay lineup drift; the
# box-minute clamp was only hygiene. Roadmap P-onfloor-2.
#
# Chunked DAY-BY-DAY on purpose: the date filter composes with pagination offset,
# so a single full-season range would page ~5k times and trip the ingest's
# MAX_PAGES=4000 backstop. One day per fetch keeps each run small and bounded.
#
# Idempotent & resumable: ingest does DELETE-per-game before insert, and a date
# whose games already carry onfloor (>=90%) is skipped — so a killed run just
# re-launches and continues where it left off. After all dates, runs compute so
# lineup_aggregates / plus_minus_pbp (and player_on_off, if present) pick up the
# exact lineups. Push to prod afterward with ./scripts/sync_to_prod.sh.
#
# Usage: scripts/onfloor_backfill.sh [SEASON]   (default 2026)
set -uo pipefail

SEASON="${1:-2026}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO"

# Load env (NATSTAT_API_KEY, DATABASE_URL) the same way the rest of the tooling does.
set -a
# shellcheck disable=SC1091
[[ -f .env ]] && source .env
set +a

BIN="$REPO/target/release/cstat-ingest"
DB="${LOCAL_DATABASE_URL:-postgres://cstat:cstat@localhost:5432/cstat}"

if [[ ! -x "$BIN" ]]; then
  echo "Missing release binary at $BIN — run: cargo build --release --bin cstat-ingest" >&2
  exit 1
fi

# Prefer host psql; fall back to the docker container (same pattern as sync_to_prod.sh).
if command -v psql >/dev/null 2>&1; then
  PSQL=(psql "$DB")
elif docker ps --format '{{.Names}}' | grep -q '^cstat-postgres$'; then
  PSQL=(docker exec -i cstat-postgres psql "$DB")
else
  echo "Need host psql or the cstat-postgres container running." >&2
  exit 1
fi

log() { echo "[$(date '+%F %T')] $*"; }

log "onfloor backfill starting — season $SEASON"

# Distinct game dates for the season, chronological. A read loop, NOT mapfile —
# macOS ships bash 3.2, which has no mapfile/readarray.
DATES=()
while IFS= read -r d; do
  [[ -n "$d" ]] && DATES+=("$d")
done < <("${PSQL[@]}" -t -A -c \
  "SELECT DISTINCT game_date::text FROM games WHERE season=$SEASON AND game_date IS NOT NULL ORDER BY game_date;")

if [[ ${#DATES[@]} -eq 0 ]]; then
  echo "No game dates for season $SEASON — nothing to fetch." >&2
  exit 1
fi
log "${#DATES[@]} game-dates to process"

fetched=0; skipped=0
for d in "${DATES[@]}"; do
  [[ -z "$d" ]] && continue
  # Resume: skip a date whose games already (mostly) carry onfloor.
  read -r tot oh < <("${PSQL[@]}" -t -A -F' ' -c \
    "SELECT count(*), count(onfloor_home) FROM play_by_play pbp \
     JOIN games g ON g.id=pbp.game_id \
     WHERE g.season=$SEASON AND g.game_date='$d';")
  tot=${tot:-0}; oh=${oh:-0}
  if [[ "$tot" -gt 0 && "$oh" -ge $(( tot * 9 / 10 )) ]]; then
    skipped=$((skipped+1)); log "skip $d (onfloor already $oh/$tot)"; continue
  fi
  log "fetch $d"
  "$BIN" play-by-play --year "$SEASON" --date "$d" 2>&1 | grep -E 'rows=|ERROR' | tail -2
  fetched=$((fetched+1))
done

log "fetch phase done: $fetched fetched, $skipped skipped. Running compute --year $SEASON…"
"$BIN" compute --year "$SEASON" 2>&1 | tail -5
log "onfloor backfill COMPLETE — season $SEASON. Run ./scripts/sync_to_prod.sh to ship lineup_aggregates to prod."
