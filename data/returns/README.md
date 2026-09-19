# Curated eligibility returns

Hand-entered players whose `class_year` says they are done but who are coming
back — the stay-put half of the NCAA 5-in-5 rule (issue #220). Loaded into
`player_returns` by `cargo run --bin cstat-ingest -- returns`; the projection
reads the table, not these files.

`{year}` is the **base** season the player is returning from, matching
`data/departures/{year}_departures.json` and `draft_entrants.year`. A row in
`2026_returns.json` affects the 2027 projection.

## When a row is needed

A row describes a **player**, keyed by the team he played for in `year`, and
its `status` follows him wherever the portal sends him:

* **Stays put** (no portal row, or a withdrawal) — `granted` projects him as
  an ordinary returner, `contested` puts him in his team's uncertain bucket.
  No feed reports a stay-put, so every one of these needs a row.
* **Committed elsewhere** in the portal — `contested` puts him in his
  **destination's** uncertain bucket instead of its firm arrivals (`?` at the
  new school, in its ceiling, out of its floor). `granted` changes nothing:
  he was already an ordinary arrival, which is the pre-existing behaviour.
  So a mover needs a row only when his eligibility is contested — and under
  the Tenth Circuit's 2026-08-21 stay that is most of the headline cases
  (Darrion Williams at Texas Tech, Chauncey Wiggins at Gonzaga, RJ Godfrey at
  Arizona), every one of whom was a firm arrival before this rule.
* **Entered the portal with no destination** — the row is read as an
  assertion that he is coming back to `current_team` (Mark Mitchell: entered,
  committed to Kentucky, un-committed, returned to Missouri under a Kentucky
  TRO). A portal entry is evidence of intent, not of a move, so the row beats
  it. A `Committed` destination is an observed move and beats the row. Do
  not write a row for an uncommitted portal entrant unless you mean that he
  is staying.

One case is detected automatically and needs no row: a senior who entered
the portal and then withdrew. `compose_all_projections` routes him to the
uncertain bucket on its own. Add a `granted` row here only to *promote* him out
of it once his eligibility is settled.

## Schema

```json
[
  {
    "name": "Player Name",
    "current_team": "Team Name",
    "status": "granted",
    "reason": "5in5",
    "source": "https://example.com/report",
    "note": "Free text."
  }
]
```

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | Matched to a roster player by normalized name + team. |
| `current_team` | yes | The school he is returning to (= the one he played for in `year`). |
| `status` | no, defaults `contested` | `granted` → projected as an ordinary returner. `contested` → uncertain bucket: present in the ceiling, absent from the floor, shown as `?`. |
| `reason` | no, defaults `5in5` | Display only: `5in5`, `waiver`, `injunction`, `medical`, `other`. `injunction` covers every row that rests on a court — order granted, stayed, or still pending. |
| `case` | no | Which suit the row rides on, as the tracker names it (`Godfrey v. NCAA`). JSON-only: not loaded into the table, not served. It exists so one court's ruling can be applied to exactly that court's plaintiffs (`--resolve-reason injunction --case "Godfrey v. NCAA"`). |
| `source` | no | URL or outlet slug for the report. |
| `note` | no | Anything the columns don't carry. |

An unrecognized `status` is rejected by the loader before anything is written,
so a typo can't half-apply a capture.

## Check that a row did something

The loader validates `status`, but it cannot validate `name` or `current_team`
— those are matched to a roster player at *projection* time, so a misspelling
produces a row that looks perfectly correct in the JSON and in the table while
doing nothing at all, leaving the player deleted by the very inference the row
was written to override. Same silent no-op as a typo'd departure.

```bash
cargo run --bin cstat-ingest -- departures-audit --year 2026
```

Section 2 of that report lists every `player_returns` row that failed to place
its player, with the likely cause (unknown name → spelling; still counted as a
departure → team string; wrong bucket → status). It exits 2 when any exist, so
a scripted curation pass can't ship a dead row.
`crates/cstat-core/tests/curated_returns.rs` asserts the same invariant against
a local DB.

## Curate conservatively

`status` is behavior-bearing and the senior class is large — 2026 carried 1,679
senior-labelled players, 43% of all positive `cam_gbpm_v3`. Marking the class
`contested` wholesale would widen every team's floor/ceiling band to
uselessness. A row belongs here when there is an actual report about an actual
player, and not before.

Prefer `contested` when in doubt: it widens the band rather than asserting an
outcome, which is the honest representation of an unsettled rule still under
litigation.

## Empty is a valid state

An empty array means "no curated returns for this season", which is the correct
starting point. It is not the same as the file being missing — the loader errors
on a missing directory on purpose, because silently writing nothing is the
failure this capture exists to prevent.

## Sourcing: the litigation IS the roster signal

The official-roster scrape (`cstat-ingest rosters`) turned out to be all over
the place — partial "(Returners)" pages, last season's roster left up all
summer. The suits are a better source for the population that matters: a
player who is a **named plaintiff** against the NCAA is, by construction, a
player who intends to play in the target season and whose eligibility to do
so is unresolved. That is the `contested` bucket, exactly.

```bash
# Named plaintiffs on the College Sports Litigation Tracker, matched to the
# base-season roster; prints per case with team / class / CAM, flags what is
# already captured, lists the index cases the tracker has no filings for.
cd training && ./.venv/bin/python ../scripts/eligibility_litigation_worklist.py \
    --year 2026 --extra ../data/returns/2026_litigation_supplement.json \
    --emit /tmp/candidates.json
```

It is a worklist, not a loader: a name-shaped token is not a player (judges,
attorneys and same-name athletes in other sports all match), so review the
output against the case text before merging `--emit`'s rows. Two things it
cannot do for you: the tracker has no filings for some suits in its own
Class-of-2022 index (the Kentucky `Wells` TRO, the dismissed North Carolina
`Okpara` suit, the Lubbock `Atwell` TRO), so those plaintiff lists live in
`2026_litigation_supplement.json`, transcribed from press reports; and a
plaintiff the tracker says has signed professionally (two of Godfrey's) is
not coming back and gets no row.

**The inverse question — who could be missing — is bounded, and that is the
pass to run first.** The only players who can be absent from the capture are
the Class of 2022 with four D-I seasons (the Wisne class); everyone else is
either on a live feed or not eligible for the argument. `--open-cohort` lists
the ones nothing accounts for — no row, not in the portal, not a draft entrant
— by CAM, so the search is a dozen names rather than a crawl:

```bash
... --year 2026 --open-cohort --min-cam 8
```

Each open player is exactly one of three things, and the default (no row →
departed) is right for two of them: **signed professionally** (a two-way,
Exhibit 10, standard or overseas contract — no row, and a captured player who
signs comes OUT, which is how Jamarques Lawrence left the file), **suing**
(a `contested` row), or **genuinely done** (no row). Drafted is not signed:
Trey Kaufman-Renn and Roddy Gayle were drafted or went to Summer League, hold
no contract, and are suing — they stay. Kohler signed an Exhibit 10 in June
and later committed to BYU under a court order — the test is "currently under
contract", so he stays too. Run on 2026-09-18 at CAM ≥ 8: 39 open, 33 signed,
3 with no suit and no signing (Saunders, Nickel, Carroll), 3 suing (White,
Griffen, Alexis) — the three rows this pass added.

The three false-positive shapes seen so far, so the next pass knows what to
look for: a same-name player in another sport (`Tristan Smith v. NCAA` is a
Clemson football player; cstat has one at Northern Iowa), a case from a past
season (`Hickman v. NCAA` was 2025-26), and a multi-sport suit whose prose
never says which plaintiff plays what (`Hudson v. NCAA` — three names were
left out for that reason).

## The class-of-2022 litigation (2026-27)

Most of the `2026_returns.json` rows are `contested` rather than `granted`.
Twenty-three are the roster-evidence cohort described below, tagged
`case: Wisne v. NCAA`; the rest are named plaintiffs in one of two dozen
suits, each tagged with its own `case`.

On **2026-07-31** Judge Charlotte Sweeney (D. Colo.) granted a class-wide
injunction letting every Division I athlete from the 2022 freshman class who had
exhausted four years seek a fifth in 2026-27. On **2026-08-21** the Tenth
Circuit **stayed** that injunction pending appeal, which returns the NCAA to the
status quo and makes that whole cohort temporarily ineligible for weeks to
months.

So a player who entered in 2022-23, has four D-I seasons on record, and appears
on his school's published 2026-27 roster is genuinely unresolved — which is what
`contested` is for. It puts him in the ceiling and out of the floor, so the
team's band spans both outcomes instead of asserting one.

Membership is read off cstat's own season history (first D-I season 2023 plus
four seasons played), not asserted per player. Two caveats: a JuCo transfer who
used two non-D-I years first can look identical, and players holding their own
**state-court** injunctions were carved out of the Tenth Circuit stay — Donovan
Atwell is a named plaintiff in the North Carolina suit, so his individual
outcome may diverge from the cohort's.

**Revisit when a court rules — and scope the sweep to that court.** The
cohort is not one suit. It rides on the federal class action AND a dozen
state suits (Ohio, Georgia, Kentucky, Texas, California, ...) that the NCAA
appeals one court at a time, and a state appellate decision resolves that
suit's plaintiffs and nobody else. `reason` tags the mechanism; `case` tags
the suit:

```bash
# Tenth Circuit rules on the class — sweep every row resting on a court.
cargo run --bin cstat-ingest -- returns --resolve-reason injunction --as granted
cargo run --bin cstat-ingest -- returns --resolve-reason injunction --as departed

# Georgia Court of Appeals rules on Godfrey — only those thirty.
cargo run --bin cstat-ingest -- returns --resolve-reason injunction \
    --case "Godfrey v. NCAA" --as departed
```

The unscoped sweep is only right in the **athletes win** direction: a class
win makes everyone eligible, but a class loss does not end a state suit with
its own order (Kentucky's TRO was never part of the Tenth Circuit stay). For
an NCAA win on the class, resolve the rows whose `case` is `Wisne v. NCAA`
and the dismissed suits that now ride it (`Okpara`, `Dalley`, `Koonin`), and
leave the live state cases for their own courts.

`--as departed` DELETES the rows rather than writing some "denied" status. That
is the correct encoding, not a shortcut: the projection's default for an
unlisted senior is already "departing", so a third status would teach it a
state it has no use for.

Both rewrite the JSON in place and stop **without** loading, so the change
lands as a reviewable git diff first. Run `cstat-ingest returns` to apply it,
then `departures-audit --year 2026` to confirm every remaining row still places
its player.

Deletion genuinely takes effect: the loader replaces the whole year rather than
upserting into it, so a row removed from the file is removed from
`player_returns`. That was not always true — an upsert-only load honoured
additions and silently ignored removals, which would have left 23 players
restored to their teams forever while both the file and the loader's own output
said they were gone.
