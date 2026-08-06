# Tipoff self-sufficiency plan — what must be true before the first 2026-27 game

*Written 2026-08-06. Every "current state" claim below was verified that day against
the live prod DB (via `scripts/sync_to_prod.sh --prod-status` and the public API) and
the local DB. Supersedes the scattered "remaining operator steps" bullets under ROADMAP
Phase 6 *Prod self-sufficiency* (S1–S6) — that item stays the strategic entry; this is
the dated, ordered execution plan for one deadline: **the 2026-27 season tips ~Nov 3.***

## 0. The test, and what counts as passing

**Unplug the laptop for a month in-season and the site stays correct and current.**

One scope clarification that has been implicit and is worth stating, because half of
what follows depends on it: *a one-time bootstrap executed **on prod** (a Railway
one-off `cstat-ingest …` command) does not violate self-sufficiency.* The constraint is
that **no routine night** may require a machine of ours, and that **no data may
originate on the laptop** and be pushed. A `cstat-ingest teams --year 2027` run on the
Railway service fetches from NatStat, writes to prod, and needs no laptop copy of
anything — it is prod feeding itself on an operator's cue. A `sync_to_prod.sh --tables`
push of laptop-computed rows is the thing that fails the test.

Under that definition the season rollover is the crux: **the nightly is built to keep a
season current, not to start one.**

## 1. What prod already does by itself (verified 2026-08-06)

The cron ran 14h before this was written and recorded **13/13 steps `ok`**, in order:
`preflight → games → player_perfs → team_perfs → forecasts → elo → torvik →
torvik_games → playbyplay → lineups → compute → invariants → row_counts`.

That list settles several ROADMAP checkboxes that still read as pending:

| Thing | State |
|---|---|
| S2 PBP + lineups in the nightly | **Deployed**, not "prod deploy pending" — both steps are in prod's ledger |
| S2 operator step (1), push `player_rapm` | **Done** — prod holds 51,896 rows (local 51,893) |
| Archetype *assign* in Rust (S3) | **Live** — `compute_all` step 20/21, incl. carry-over + tier-3 |
| `players.display_name` | **Merged to prod** — the API returns `Obi Toppin` (2020), `Ja Morant` (2019), `Marvin Bagley III` (2018); local `players.name` still reads `Obadiah Toppin`, and prod's nightly only computes the current season, so those can only come from the `--columns` merge |
| Full-sync guard | **Working** — `--prod-status` reports `WOULD BLOCK` (prod cron-fed 14h ago) |

Prod's `play_by_play` / `lineup_stints` / `natstat_lineups` are all **0 rows**, which is
correct: the steps are date-scoped to the ingest window, and there are no games in
August. Their first real exercise is opening night — nothing before then proves them.

## 2. Blockers, in deadline order

### B1 — Nothing bootstraps season 2027 on prod. This is the hard one.

`current_natstat_season()` flips to **2027 on Nov 1** (`lib.rs:165` — `month >= 11 →
year + 1`), and from that night the cron ingests season 2027. But:

- `SeasonIngester::nightly` **deliberately omits the `teams` step** — its own doc
  comment says so (`ingest/season.rs:272-277`): *"new teams only appear at a season
  bootstrap."*
- `team_id_by_code_and_season` (`lib.rs:228`) is a **pure lookup**, no create-on-miss.
- Prod's `teams` row count (4,268) is **exactly** local's 2015–2026 total, and local has
  **zero** 2027 rows — so prod has none either.
  *Confirm with:* `psql "$PROD_DATABASE_URL" -c "select count(*) from teams where season=2027"`.

So on opening night every game resolves `home_team_id`/`away_team_id` to `NULL`, and the
whole 2027 chain — box scores, four factors, AdjEM, rankings, predictions — has nothing
to hang on. The site would show an empty 2027 with a green Slack heartbeat, because
nothing in the served-critical set is *failing*; there is simply no season.

**Fix, one of:**
- **(a) Operator, minimum viable:** run `cstat-ingest teams --year 2027` (and, if we want
  metadata, `team_details`) **on Railway** in late October. One command, no laptop data.
- **(b) Code, and the right long-term answer:** have the nightly self-bootstrap — if
  `teams` for the current season is empty, run the teams ingest before the games step.
  It fires once per year and is a no-op every other night. This is what makes the
  rollover survive an operator who is on a plane.

Recommend **(b) with (a) as the belt** — build the auto-bootstrap, and still run the
command by hand in October so the first live rollover is not also the code's first run.

### B2 — The 2027 preseason projection anchor does not exist, and cannot until B1 lands

`/predict` blends a preseason anchor over the first 42 days — the whole point being that
opening-week predictions do not ride on a 1–2 game sample. That anchor is a row lookup:
`SELECT projected_adj_em FROM team_preseason_projection WHERE season=$1 AND team_id=$2`
(`routes/predict.rs:975`), keyed by the **current season's** team UUID. No row → the
blend silently falls back to pit-only. Silently is the operative word: `prediction_basis`
would read `pit`, and nothing alerts.

`team_preseason_projection` holds **3,352 rows on prod and 3,352 locally, max season
2026 in both** — there are no 2027 rows anywhere. And they cannot be written yet:
`resolve_base_to_target` (`compute_projections.rs:59`) is an **INNER JOIN onto
`teams WHERE season = target`**, so with no 2027 teams every row is `skipped_unresolved`.

(The `/projected?season=2027` page people have been looking at is **computed live** from
2026 rosters — `routes/projections.rs` never reads this table. The board being populated
is not evidence the anchor is.)

**Fix:** after B1, run `cstat-ingest compute-projections --years 2027` on prod. Ordering
is mandatory — run it first and it writes zero rows and exits successfully.

### B3 — A missed PBP night is a permanent, silent hole

The self-heal gap scan counts a date as covered only when **all of
`games`/`player_perfs`/`team_perfs`** succeeded (`run_ledger.rs:53`, `BOX_SCORE_STEPS`).
`playbyplay` and `lineups` are deliberately outside that set, and outside
`SERVED_CRITICAL` (`routes/health.rs:32`) so they cannot false-alarm off-season.

The consequence is asymmetric with the box-score path: if `playbyplay` fails on a night
where the box scores succeeded, **no future run ever goes back for it**. `compute_pbp_lineups`
is a season-scoped `DELETE`-then-rebuild, so from then on it rebuilds the whole season
from PBP with a hole in it — `lineup_aggregates`, `player_on_off` and `lineup_stints`
quietly undercount for the rest of the year.

Nothing detects this. The existing invariant `pbp_present_but_lineups_empty`
(`invariants.rs:327`) is **all-or-nothing** — it fires only when a season has PBP and
*zero* rollups. Partial coverage reads as healthy. `row_counts` compares against the
prior run, which also lacked those rows, so the shortfall never looks like a shrink.

**Fix (cheap, do before tipoff):** add a coverage invariant — *completed games in the
season with zero `play_by_play` rows* — at `Warning` severity, so a hole shows up in the
nightly Slack summary the next morning while a backfill is one command
(`cstat-ingest playbyplay --from X --to Y`).
**Fix (complete, can follow):** extend the gap scan with a PBP-specific covered-date
notion so the heal re-pulls those dates itself.

### B4 — The R4 premise expires at tipoff; two tables change owner

`crates/cstat-core/tests/sync_prod_r4_invariant.rs` states the invariant that makes
`--tables lineup_aggregates,player_on_off` safe: prod holds no PBP **because we never
push it**, so `compute_pbp_lineups` no-ops there. S2 makes prod ingest its own PBP. From
opening night the premise is false, and the conclusion inverts:

- The **exclusion** stays right — we still never push raw PBP.
- The **ownership** flips. For the current season prod computes those rollups nightly, so
  a laptop `--tables lineup_aggregates` push (which `TRUNCATE`s the whole table and
  restores local rows) replaces prod's fresher current-season rollups with the laptop's
  until the next nightly rebuilds them.

This matters because `CLAUDE.md` currently advertises
`--tables lineup_aggregates,player_rapm` as *"the intended in-season path."* After
tipoff that line is half wrong: `player_rapm` still needs it (B6), `lineup_aggregates`
must not.

**Fix:** at tipoff, split that guidance, move both rollups to owner=prod in the ownership
table (§3), and rewrite the R4 test's rationale from *"prod has no PBP"* to *"prod
produces these itself; pushing them is the collision."* This is the concrete first
customer for the open ROADMAP P1 *"enforce table ownership on `--tables`"*.

### B5 — Archetypes: the cold start is handled, the retrain is not

Shipped and prod-native: nightly assign against frozen centroids, prior-season carry-over,
tier-3 newcomer inference, provisional presentation. `load_archetype_model` falls back to
the latest fit, so 2027 assigns against the 12-season centroids from day one with no
refit needed. **Nothing here blocks tipoff.**

What is still open is the retrain footgun (ROADMAP S3-follow-up): `write_results`
(`training/archetypes.py:475`) runs `DELETE FROM player_archetypes WHERE season IN
:seasons` then re-inserts **real-only** rows — so any `python -m archetypes` retrain wipes
carry-over and inference across all 12 seasons, and the nightly restores only the current
one. The interim playbook (`docs/archetypes_methodology.md:56`) works but is a full
20-step `compute --year` per season.

**Fix:** (a) drop the `player_archetypes` INSERT from `write_results` so the Python fit
writes only `archetype_models`; (b) add an archetype-only all-season sweep (a
`cstat-ingest archetypes --all-seasons` looping `compute_archetypes(season, false)`).
**Deadline is "before the next refit", not tipoff** — but note the #244 CamPom recovery
moved the archetype input features, so a refit is plausibly wanted this offseason, which
would pull this forward. Decide that explicitly rather than by accident.

### B6 — RAPM cannot become prod-native without a decision (see §5)

`training/rapm.py` fits a **decayed 3-season pooled window** with career chaining across
seasons (`rapm.py:11`), reading `lineup_stints`. Prod will only ever hold the **current
season's** stints — it ingests current-season PBP, and historical PBP is local-only by
the storage split.

So "port RAPM to Rust and run it nightly" does not produce the metric we ship. It
produces a **single-season fit**, which the doc measures at 30–50% worse split-half
reliability. That is a different, worse number under the same column header. This is a
product decision, not a porting task — options and a recommendation in §5.

Until it is decided, RAPM is laptop-computed and **frozen at whatever was last pushed**.
Given the 250-paired-possession display floor, a stale RAPM in November mostly means
*absent*, not *wrong* — which is survivable, but should be disclosed rather than implied.

### B7 — Torvik will fail every night for the first days of November. Expect it.

The requested-year guard (`torvik.rs:660`, `validate_requested_year`) **returns an error**
when barttorvik answers a `year=2027` request with 2026 rows — which is exactly what it
does until 2027 data is published. The per-game path (`{year}_all_advgames.json.gz`)
404s on its own. Both `torvik` and `torvik_games` are **served-critical**
(`routes/health.rs:32`), so from Nov 1 until barttorvik publishes:

- the nightly posts a DEGRADED Slack summary every night,
- `GET /api/health/ingest` returns **503**,
- and the 36h staleness alarm carries no information during exactly the window when we
  most want it to.

The guard is correct — ingesting 2026 rows labelled 2027 is far worse. But we should
decide now whether to (a) accept and pre-announce the noise, or (b) teach the guard to
report a distinct "source has not published this season yet" status that degrades the run
without marking a served-critical step failed. **(b) is a small change and worth it**,
because a red health endpoint that everyone has been told to ignore for a week is how a
real outage gets missed.

### B8 — Prod PBP storage: monitor, don't pre-solve

Prod's volume is 10 GB, currently ~2.5 GB. A lived-through season of PBP is ~1 GB
(2026 measured 978 MB); `lineup_stints` adds to that. One season is comfortable, three
is not, and nothing prunes. **Not a tipoff blocker** — the action is a calendar reminder
to check the volume in January and a decision on retention (prune raw PBP older than N
seasons on prod, since the rollups are what's served) before the 2028 bootstrap.

## 3. Ownership after tipoff

The point of the table is that each row has exactly **one** legitimate writer. Where the
two columns disagree with today's habit, that disagreement is the work.

| Table | Writer after tipoff | How it gets there | If the wrong side writes |
|---|---|---|---|
| `games`, `player_game_stats`, `team_game_stats`, `team_season_stats`, `player_season_stats`, `player_percentiles`, `schedules`, `game_forecasts` | **prod** | nightly ingest + `compute_all` | a full sync rolls the live site back (guard already refuses) |
| `torvik_player_stats`, `torvik_player_game_stats` | **prod** | nightly torvik steps | same |
| `play_by_play`, `lineup_stints`, `natstat_lineups`, `natstat_lineup_games` | **prod** (current season only) | nightly `playbyplay`/`lineups` | stays EXCLUDED from every push — unchanged |
| `lineup_aggregates`, `player_on_off` | **prod** — *changed at tipoff* | `compute_all` step 10 | a laptop `--tables` push clobbers the current season (B4) |
| `player_archetypes` | **prod** for the current season; laptop for historical carry-over after a refit | nightly step 20 / post-retrain sweep | a routine in-season push is overwritten next nightly — which is correct |
| `players.display_name` | **prod** for the current season; laptop `--columns` for historical | `compute_all` step 21 | — |
| `archetype_models` | **laptop**, annual | `--tables` | — |
| `player_rapm` | **laptop**, periodic (until §5 is decided) | `--tables` | — |
| `transfers`, `recruits`, `coaches`, `draft_entrants` | **laptop**, offseason/on-demand | `--tables` | no in-season churn, so no staleness (ROADMAP S5) |
| `team_preseason_projection`, `player_season_projection` | **prod** for 2027 (B2) | `compute-projections` run on prod | **local currently has no 2027 rows — a `--tables` push from the laptop after B2 would delete prod's 2027 anchors.** Do not push this table once prod owns 2027 |
| `ingest_runs`, `ingest_run_table_counts`, `api_cache`, `portle_daily_puzzle` | **prod** | runtime | already EXCLUDED |

That `team_preseason_projection` row is not hypothetical: local and prod are byte-equal at
3,352 rows today, and the moment prod computes 2027 the laptop becomes the stale copy.

## 4. Ordered runbook

**Now → mid-October (code).**
1. B1(b) nightly season self-bootstrap.
2. B3 PBP coverage invariant (`Warning`) + confirm the `playbyplay --from/--to` backfill path.
3. B7 "source has not published this season" status for the Torvik year guard.
4. B4 ownership split — `CLAUDE.md` line, R4 test rationale, ownership table enforcement.
5. B5 (a)+(b) if a refit is planned this offseason; otherwise schedule before the refit.
6. Keep the weekly `cstat-ingest simulate --reset` habit — it is the only thing exercising
   the nightly end-to-end while there are no games.

**Late October (operator, on prod).**
7. `cstat-ingest teams --year 2027` on Railway → verify `select count(*) from teams where season=2027` ≈ 364.
8. `cstat-ingest compute-projections --years 2027` on Railway → verify `team_preseason_projection` has 2027 rows.
9. Final offseason `--tables` pushes while the laptop is still the owner: `transfers`,
   `recruits`, `coaches`, `draft_entrants`, `archetype_models`, `player_rapm`,
   `coach_ratings`, `coach_season_cae`.
10. Confirm no `CSTAT_SIMULATED_DATE` on either Railway service.

**Nov 1 (rollover day).** Season flips to 2027. Expect `torvik`/`torvik_games` failures
until barttorvik publishes (B7). Confirm the nightly still completes and the *2026* site
is untouched.

**Opening night + the morning after.** Run the existing checklist in
`docs/deploy_nightly_cron.md` §*First-day-of-season checklist*, plus three additions this
plan adds:
- `prediction_basis` on an opening-night `/api/predict` is `preseason` or `blended` — **if
  it says `pit`, B2 did not land** and the anchor lookup is missing.
- `play_by_play` for season 2027 is non-zero, and `lineup_aggregates` / `player_on_off`
  have 2027 rows produced by prod (this is the S2 acceptance).
- No `--tables lineup_aggregates` push from the laptop, ever again, in-season.

**Two weeks in.** Re-check PBP coverage (games with zero plays), volume growth, and that
archetypes have started appearing as players clear the ≥10 GP gate (with provisional
labels before that).

## 5. Open decisions — these need a call, not more analysis

1. **RAPM.** Three honest options: **(i)** accept it as an explicitly periodic laptop
   metric and *label the staleness on the page* (ROADMAP's option (b) — the honest
   failure mode for any push-based table); **(ii)** ship the two prior seasons'
   `lineup_stints` to prod (~1 GB) so a Rust port can fit the real 3-season pooled
   window — this deliberately breaks the R4 exclusion list and must come **after** B4's
   ownership work; **(iii)** port with a single-season fit and accept a measurably worse
   number. Recommend **(i) for tipoff, (ii) as the real fix afterward** — (iii) trades a
   quality regression for a self-sufficiency checkbox on a display-only metric.
2. **Season bootstrap:** auto-step in the nightly (B1b) or permanently an operator
   command? Recommend the auto-step; the rollover is annual, which is precisely when
   nobody remembers the runbook.
3. **Archetype refit this offseason?** #244 moved the inputs. If yes, B5 (a)+(b) must land
   first, or the refit wipes carry-over across 12 seasons and the site degrades until a
   manual sweep.
4. **PBP retention on prod** — decide before the 2028 bootstrap, not now.
