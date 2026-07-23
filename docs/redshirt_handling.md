# Redshirt handling in the projection pipeline

**Status: documented gap, not yet fixed.** This note records how redshirt /
non-enrolling players are (mis)handled by the preseason team projection, the
data signals available to detect them, and a proposed two-PR scoping. No
behavior changes ship with this note.

## The problem

The preseason projection (`crates/cstat-core/src/roster_projection.rs`, served
by `GET /api/projections/{year}` in `crates/cstat-api/src/routes/projections.rs`)
builds a projected roster from returning players + incoming transfers + incoming
recruits, then scores team AdjEM/AdjO/AdjD. It has **no concept of redshirting** —
grep for `redshirt|is_redshirt|eligibility|years_used|enrolled` finds only
comments and 247 transfer-status strings, no columns or logic. That produces two
distinct, opposite-signed defects.

### (a) Redshirting recruits are over-credited

The recruit pull (`roster_projection.rs:724-766`) filters only on class year, a
resolved destination team, and `commit_status <> 'Uncommitted'`:

```sql
FROM recruits r
LEFT JOIN teams t ON t.id = r.committed_team_id
WHERE r.year = $1
  AND r.committed_team_id IS NOT NULL
  AND COALESCE(r.commit_status, '') <> 'Uncommitted'
```

There is **no join to `player_season_stats` / `player_game_stats`** confirming the
recruit ever played. Every ranked commit is synthesized into a freshman
`PlayerRow` (`freshman_row`, `roster_projection.rs:194-223`) carrying the
freshman model's projected `cam_v3`, and fed to the roster-impact AdjEM
calibrator via `for_scenario` (`roster_projection.rs:301-317`). A recruit who
signs but redshirts (or never enrolls) is credited to the team's projection
anyway.

The only recruit exclusion that exists is unrelated — `feeds_projection`
(`roster_projection.rs:160-166, 977`) drops the unranked `institution_group =
'commits'` cohort (issue #175), keeping the ranked composite cohort the
calibrator was trained on.

### (b) Returning-from-redshirt players are silently dropped

The returning-player fetch (`roster_projection.rs:654-687`) requires a
base-season `player_season_stats` row clearing playing-time gates:

```sql
WHERE pss.season = $1
  AND COALESCE(pss.games_played, 0)   >= $2   -- QUAL_MIN_GAMES_PLAYED = 5
  AND COALESCE(pss.minutes_per_game, 0) >= $3 -- QUAL_MIN_MPG = 5.0
```

(gate constants in `crates/cstat-core/src/roster_features.rs:97-98`). A player who
redshirted the base season played 0 games, so he has **no qualifying PSS row**
and never enters the returning set — even though he returns to play next season.

The one partial rescue is portal-gated: the issue-#146 `satout_lookup`
(`roster_projection.rs:789-853`) looks back up to `TRANSFER_SEASON_LOOKBACK = 2`
seasons for a sat-out player, but **only for `cstat_player_id`s that appear in the
`transfers` table**. A stay-at-school redshirt returner (no portal row) gets no
rescue.

## Detection signals (already in the data, currently unused)

- **Redshirt recruit** — `recruits.cstat_player_id` is resolved in ingest Pass 2
  (`crates/cstat-ingest/src/ingest/recruits.rs:620-684`) by matching a recruit to
  a `players` row in season `year+1`, and `players` rows exist only for players
  who appear in a box score. So **a committed recruit whose `cstat_player_id` is
  still NULL after season `year+1` is ingested = committed-but-never-played =
  redshirt / non-enrollment**. Equivalently: recruit for year Y, zero
  `player_game_stats` for season Y+1.
- **Redshirt returner** — the *absence* of a base-season PSS row for a
  `natstat_id` / `torvik_pid` that exists in a prior season and reappears the
  next. The `satout_lookup` block already demonstrates this exact query shape;
  it is just gated on portal membership rather than run for all returners.
- **Transfer eligibility** — `transfers.eligibility_type`
  (`Immediate` / `TBD` / `PendingAppeal` / `Withdrawn`) and `eligibility_years`
  (`migrations/019_transfers.sql:66-70`) are ingested but never read by the
  projection; a `Withdrawn` / `PendingAppeal` arrival won't be immediately
  eligible.

## Spot-check evidence

Recruit-year convention: `recruits.year = N` plays season `N+1` (verified:
Cooper Flagg is a 2024 recruit, played season 2025). So Duke's 2025 recruiting
class maps to the 2026 projection.

**Sebastian Wilkins — confirmed over-credit (case a).** Duke's 2025 signed class,
checked against who actually played 2026:

| Recruit          | Composite rank | Played 2026? |
| ---------------- | -------------- | ------------ |
| Cameron Boozer   | 3              | ✓            |
| Nikolas Khamenia | 19             | ✓            |
| Cayden Boozer    | 20             | ✓            |
| Dame Sarr        | 32             | ✓            |
| Sebastian Wilkins| 35             | ✗            |

Wilkins is "Signed" with **zero `players` rows in any season** — the projection
fed him through the freshman model and credited Duke for a player who never took
the floor. `commit_status = "Signed"` did not catch it (all five are "Signed").

**Caden Pierce — return-from-redshirt pattern (case b).** Played Princeton
seasons 2023 / 2024 / 2025, **no 2026 PSS row** (redshirt), then `transfers`:
Princeton → Purdue for 2026, `eligibility_type = "Immediate"`. He is caught for
Purdue's 2027 projection only because he entered the portal; a non-transfer
redshirt returner in the same situation would be dropped.

## Forward vs. ex-post: what is fixable when

The `cstat_player_id IS NULL` proxy only resolves once season `year+1` is
ingested, so the two cases have very different forward value:

- **(b) return-from-redshirt is forward-fixable and valuable.** Projecting season
  Y, the base season Y-1 is complete, so we already know who sat out. We currently
  drop them wrongly. Clean win, knowable at projection time.
- **(a) redshirt recruit is only ex-post / in-season fixable.** For a futures
  projection *before* the season, every incoming recruit's `cstat_player_id` is
  NULL (the season is unplayed), so the proxy cannot discriminate. It corrects
  retroactively and progressively as box scores land in-season; a small
  historical attrition haircut is the only pre-season lever.

## Proposed scoping

- **PR 1 (small, high-value, filter-level).**
  - (b) Retain returning-from-redshirt players: generalize the `satout_lookup`
    lookback (`roster_projection.rs:789-853`) to non-portal returners, or relax
    the returning fetch to re-include a prior-season qualified player who has no
    base-season row.
  - (a) Exclude committed recruits with zero games once season `year+1` is
    ingested (the `cstat_player_id IS NULL` proxy), analogous to the existing
    `feeds_projection` commits-cohort exclusion. Improves historical-backtest
    accuracy immediately.
- **PR 2 (research).** A forward-looking attrition / `is_redshirt` prior for
  futures, plus wiring `transfers.eligibility_type` into arrival handling. This
  is the harder modeling piece and needs its own LOSO validation.

## Key files

- `crates/cstat-core/src/roster_projection.rs` — roster composition (recruit
  pull 724-766, returning fetch 654-687, `satout_lookup` 789-853,
  `feeds_projection` 160-166).
- `crates/cstat-core/src/roster_features.rs:97-98` — `QUAL_MIN_GAMES_PLAYED`,
  `QUAL_MIN_MPG` gates.
- `crates/cstat-ingest/src/ingest/recruits.rs:620-684` — Pass-2
  `cstat_player_id` resolution (the redshirt-recruit signal).
- `migrations/019_transfers.sql:66-70` — `eligibility_type` / `eligibility_years`.
- `migrations/020_recruits.sql`, `migrations/001_initial_schema.sql` — recruit /
  player schema (no eligibility column today).
