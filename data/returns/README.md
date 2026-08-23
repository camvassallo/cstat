# Curated eligibility returns

Hand-entered players whose `class_year` says they are done but who are coming
back — the stay-put half of the NCAA 5-in-5 rule (issue #220). Loaded into
`player_returns` by `cargo run --bin cstat-ingest -- returns`; the projection
reads the table, not these files.

`{year}` is the **base** season the player is returning from, matching
`data/departures/{year}_departures.json` and `draft_entrants.year`. A row in
`2026_returns.json` affects the 2027 projection.

## When a row is needed

Only for players **staying at the same school**. A senior who takes his extra
year somewhere else already resolves himself: he appears in the 247 portal
feed, and the projection's arrivals path has no class filter, so he lands on
his new team correctly with no curation.

One case is also detected automatically and needs no row: a senior who entered
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
| `reason` | no, defaults `5in5` | Display only: `5in5`, `waiver`, `injunction`, `medical`, `other`. |
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

## The class-of-2022 litigation (2026-27)

Most of the `2026_returns.json` rows are `contested` rather than `granted`, and
the reason is a single live case rather than 23 separate judgement calls.

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

**Revisit when the Tenth Circuit rules.** A win flips the cohort to `granted`;
a loss makes them departures and the rows should be deleted, not flipped.
