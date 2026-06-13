#!/usr/bin/env bash
# Durable lineups backfill chain: run each season's `cstat-ingest lineups` in
# sequence, newest-first then working backwards. Restart-safe — the
# natstat_lineup_games ledger skips already-captured games, so re-launching this
# script after a laptop sleep / kill resumes exactly where it left off.
#
# Order: 2025 (finish), then back through history. 2023/2024 are already complete
# and 2026 fills via the normal in-season `update` path, so both are omitted.
set -uo pipefail

REPO="/Users/camdenvassallo/Documents/GitHub/cstat"
BIN="$REPO/target/release/cstat-ingest"
LOG="/tmp/lineups_backfill_2025.log"   # reuse the active 2025 log; later seasons append

cd "$REPO" || exit 1
set -a; source "$REPO/.env"; set +a

# 2025/2022 complete; 2021 omitted — NatStat has NO lineups object for the entire
# 2020-21 season (0/9 sampled games across Nov 2020-Apr 2021; the games;lineups
# hydrate returns a full game object with no `lineups` key). It is a genuine
# one-year source gap, not era-thinning — 2020 and 2019<-2015 all sample 4-5/5
# with 20-70 units/game. Re-running 2021 only burns ~6h of budget for zero.
SEASONS=(2020 2019 2018 2017 2016 2015)
MAX_ATTEMPTS=5   # transient-failure retries per season (ledger makes retries cheap)

echo "=== lineups chain starting $(date '+%Y-%m-%d %H:%M:%S') :: seasons ${SEASONS[*]} ===" >> "$LOG"

for S in "${SEASONS[@]}"; do
    echo "=== chain: begin season $S $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
    attempt=1
    lineups_ok=0
    while (( attempt <= MAX_ATTEMPTS )); do
        echo "=== chain: season $S attempt $attempt/$MAX_ATTEMPTS ===" >> "$LOG"
        "$BIN" lineups --year "$S" >> "$LOG" 2>&1
        rc=$?
        if (( rc == 0 )); then
            echo "=== chain: season $S lineups DONE (exit 0) $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
            lineups_ok=1
            break
        fi
        echo "=== chain: season $S exit $rc, retrying in 120s $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
        sleep 120
        (( attempt++ ))
    done
    if (( lineups_ok == 0 )); then
        echo "=== chain: season $S gave up after $MAX_ATTEMPTS attempts; SKIPPING compute (re-run later to resume) ===" >> "$LOG"
        continue
    fi

    # Lineups for this season are complete — refresh the derived tables that
    # consume them. `compute_all` step 6/15 (compute_pbp_lineups) rebuilds
    # lineup_stints / lineup_aggregates / player_on_off from the now-complete
    # natstat_lineups; the other steps recompute idempotently from box data.
    # Local + idempotent, so a failure here is non-fatal — the chain moves on and
    # `cstat-ingest compute --year $S` can be re-run by hand. Pooled RAPM
    # (training/rapm.py) and sync_to_prod are deliberately NOT here: RAPM wants
    # all seasons present (run once at the end) and the prod push is user-owned.
    echo "=== chain: season $S compute (refresh on-off / lineup aggregates) $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
    "$BIN" compute --year "$S" >> "$LOG" 2>&1
    crc=$?
    if (( crc == 0 )); then
        echo "=== chain: season $S compute DONE $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
    else
        echo "=== chain: season $S compute FAILED (exit $crc) — re-run 'cstat-ingest compute --year $S' by hand $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
    fi
done

echo "=== lineups chain COMPLETE $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$LOG"
echo "=== MANUAL END-STEPS (user-owned): (1) cd training && python rapm.py  [pooled, all seasons]  (2) ./scripts/sync_to_prod.sh  [pushes player_on_off / lineup_aggregates / player_rapm; confirm migration 038 on prod first] ===" >> "$LOG"
