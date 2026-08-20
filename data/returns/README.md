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
