# 5-in-5 eligibility handling

How cstat represents the NCAA's age-based eligibility rule. Tracking issue:
**#220**. Read `data/returns/README.md` for the curation workflow and
`docs/247_api.md` for the feed this leans on.

## The rule, and why cstat is exposed

On 2026-06-23 the D-I Cabinet adopted "5-in-5": five years of eligibility
beginning the academic year after an athlete turns 19 or graduates high school.
It replaces "four seasons of competition within a five-year window from
enrollment" and takes effect for **2026-27 — cstat season 2027**, the season the
Future page projects today. Athletes who completed eligibility by spring 2026
stay under the old rules. The rule is under active legal challenge.

cstat's entire eligibility mechanism was one string comparison: a roster row
whose `class_year` reads `Sr` is assumed gone next season. There is no
eligibility table, no years-remaining field, no age.

That inference is now wrong for an unknown but large population, and it is the
**largest** departure channel:

```
2026 senior-labelled players (Torvik):      1,679
  ... at >= 15 mpg:                         1,317
Sigma positive cam_gbpm_v3, seniors:        2,748
Sigma positive cam_gbpm_v3, all players:    6,374   -> seniors are 43%
```

## Two populations, only one self-healing

**Movers — already correct, no work needed.** A senior who takes his extra year
at another school appears in the 247 portal feed, and the projection's arrivals
path has no class filter (`age_up_class_year` has an explicit `Sr -> Sr`
branch). He lands on his new team. Measured: of the 56 players who entered the
2026 portal after June 1 and resolve to a cstat player, **53 were `Sr`**.

**Stay-puts — invisible.** A senior who takes the extra year at the same school
appears in no feed. Not the portal, not the draft list, and not Torvik's
`class_year`, which does not exist for a season that has not been played. He is
simply deleted from his team's projection.

Everything below exists for the second group.

## Representation: reuse `uncertain`, do not build an eligibility model

A senior whose eligibility is unsettled is structurally identical to a player
who has declared for the NBA draft and not withdrawn: a known player, an
unresolved binary, a resolution date we do not control. That case already has
machinery — the `uncertain` bucket, materialized in the **ceiling** scenario and
dropped from the **floor**, which widens the team's projected band instead of
asserting an outcome, and which the UI already marks with `?`.

Routing eligibility through it means **no new model, no new feature, and no
change to the served 27-feature roster-impact vector**.

## Classifier order

`compose_all_projections`, per prior-season roster player, first match wins:

| # | Channel | Kind | Outcome |
| --- | --- | --- | --- |
| 1 | `player_departures` | curated | `LeftProgram` departure |
| 2 | 247 portal, non-withdrawn | observed | `Transferred` departure |
| 3 | `player_returns` = `granted` | curated | **returning** |
| 4 | `player_returns` = `contested` | curated | **uncertain** |
| 5 | senior + portal withdrawal | derived | **uncertain** |
| 6 | `class_year == 'Sr'` | inferred | `GraduatedSenior` departure |
| 7 | draft list `gone` | observed | `DraftGone` departure |
| 8 | draft list `declared` | observed | uncertain |
| 9 | — | | returning |

The principle: **observations beat inferences, and curation beats both.** The
senior check moved from position 2 to position 6 as part of this work; it is the
only inferred channel and now sits below everything it can be checked against.

Two ordering choices are load-bearing and should not be "tidied":

* **Portal above returns (2 before 3/4).** A player who actually moved has
  moved, whatever a stale curated row says.
* **Senior above the draft branches (6 before 7/8).** Moving it below
  `declared_draft` would route a senior who merely declared into `uncertain` —
  i.e. treat him as possibly returning — which changes roster math rather than
  labels.

## The one automatic signal

A senior who **entered the portal and then withdrew** demonstrates, with no
curation on our part, both that he believes he has eligibility left and that he
intends to use it where he is. That is exactly the stay-put population, and it
is free.

Classified `contested`, never `granted`: entering the portal is evidence of
intent, not proof the NCAA agreed. A curated `granted` row overrides it.

Scale check — 2026 non-withdrawn portal entrants by class: `Jr 13 / So 7 /
Fr 6 / Sr 1`. One player today (Asim Jones, Quinnipiac, withdrew 2026-08-06).
Small now; the mechanism is what matters.

This also resolved a real invariant collision. `withdrawn_transfers_return.rs`
asserts a withdrawal stays on his team; the senior branch asserts every `Sr`
departs. A withdrawn senior satisfies both preconditions and they contradict —
which is precisely what the fresh portal data surfaced. Routing him to
`uncertain` satisfies both.

## Where the data lives

| Component | Path |
| --- | --- |
| Table | `player_returns` (migration `051`) |
| Capture | `data/returns/{year}_returns.json` |
| Loader | `cargo run --bin cstat-ingest -- returns` |
| Read by | `fetch_player_returns` -> `compose_all_projections` |
| Gate | `crates/cstat-core/tests/curated_returns.rs` |

`year` is the **base** season, matching `player_departures.year` and
`draft_entrants.year`: rows in `2026_returns.json` affect the 2027 projection.

`fetch_player_returns` is called **inside** `compose_all_projections` rather
than threaded through as a parameter, unlike `player_departures`. That is
deliberate — there are nine call sites, and a parameter is something a call site
can pass `&[]` for. That failure would be silent and would look exactly like
"this team lost its seniors", which is the bug rather than a symptom of it.

## Downstream: the projected player rankings

`player_season_projection` (the `/players?season=N+1` view) originally
materialized only `returning + arrivals + recruits`, so anything in `uncertain`
was absent from the rankings entirely. Tolerable when the bucket held
declared-draft players who mostly leave; not tolerable when it holds seniors who
mostly play — the team band would widen to acknowledge the uncertainty while the
player vanished from his own page.

Fixed in two places, both needed:

* migration `052` widens the `source` CHECK to admit `'uncertain'`;
* `compute_projections.rs` adds `uncertain` to **both** the write loop and the
  `real_ids` identity prefetch. Missing the second is a silent drop — a missing
  identity is a `continue` that does not increment the row counter, so the run
  summary reports success.

The UI renders the cohort as a `?` chip (`ProjectedPlayers.tsx`), alongside
Ret / Tfr / Fr.

## Curate conservatively

`status` is behavior-bearing. Marking the senior class `contested` wholesale
would widen every team's floor/ceiling band to uselessness. A row belongs in the
capture when there is an actual report about an actual player. Prefer
`contested` when unsure — an unrecognized status is parsed as `contested` for
the same reason.

## Still open

* **`team_preseason_projection` has no 2027 rows** and cannot get them until
  season 2027 is bootstrapped (no 2027 `teams` rows exist). Pre-existing;
  issues #245 / #246. The Future tab is unaffected — `/api/projections`
  composes live — but `/predict`'s preseason anchor is.
* **The empirical check from #220.** Once 2027 games are ingested, count players
  labelled `Sr` in 2026 who appear in 2027 box scores. Near zero under the old
  rule; the climb is a direct measurement of how badly the graduation channel is
  mispricing, and needs no new data source.
* **Torvik's `class_year` vocabulary** under an age-based model. If the feed's
  values or their meaning shift, cstat's behavior changes with no code change on
  our side. This is the concrete trigger to watch.
* **Litigation.** The rule could be modified or enjoined. The `contested` status
  exists so that outcome does not require a schema change.
