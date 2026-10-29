# Curated season-long absences

Hand-entered players who are on a roster for the target season but will not
play in it — a torn ACL in the summer, a medical redshirt (issue #217). Loaded
into `player_absences` by `cargo run --bin cstat-ingest -- absences`; the
projection reads the table, not these files.

`{year}` is the **base** season, matching `data/departures/` and
`data/returns/`. A row in `2026_absences.json` affects the 2027 projection.

## Why this is not a departure

A departure says the player left the program. These players have not: they are
expected back the season after, and recording them in `player_departures` would
write them off in every downstream surface. An absence removes the player from
the team's **scored** roster for the one season and lists him on the team page
as "out for season", nothing more.

## What a row does

`current_team` is the team he would play **for** in the target season, so one
row shape covers all three ways onto a roster:

| He is | `current_team` | Effect |
| --- | --- | --- |
| A returner | his base-season team | out of `returning`; counts toward the team's departure sums (the turnover weight sees it) |
| A portal arrival | his destination | out of `arrivals`; not turnover at the destination, and still in `inbound_cam_v3_sum` (the portal did move him) |
| An incoming recruit | his committed school | out of `recruits` and the recruit counts |

In every case the player is out of both the floor and the ceiling.

## Partial-season absences (`availability`)

A player out for **part** of the season with a reported return (#219: JT Toppin,
Texas Tech, ACL) carries `availability`, the share of the regular season he is
expected to play, strictly between 0 and 1. He stays on the roster, in the
uncertain bucket with cause `partial_availability`. The team then scores as
the availability-weighted mix of its rosters with him (the ceiling) and without
him (the floor), the same machinery as the draft and eligibility cohorts. The
eligibility toggle does not switch him. The team page lists him under "Out part
of the season" as `out · ~59% of season`.

**Derive the share from the schedule, not from a guess:**

    availability = scheduled regular-season games on or after the return date
                   / all scheduled regular-season games

The return date is the most specific report available: a coach's stated window
or the player's own target. Where reports disagree, take the later date the
reports support, and record the range in `note`. Rust on return is not
modelled. A recruit cannot carry `availability`; he has no player row the
uncertain bucket can project.

## Schema

```json
[
  {
    "name": "Solo Ball",
    "current_team": "Connecticut",
    "injury": "wrist surgery, medical redshirt",
    "source": "https://www.rotowire.com/cbasketball/player/solo-ball-22332",
    "note": "Free text."
  }
]
```

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | Matched by normalized name against the team's projected returners, arrivals and recruits. |
| `current_team` | yes | The team he would play for in the target season. |
| `injury` | yes | Display only — shown in the chip's tooltip. |
| `availability` | no | Share of the season he is expected to play, strictly between 0 and 1. Omit for a whole-season absence. See "Partial-season absences" above. |
| `source` | no | URL for the report. A RotoWire player page is preferred: its status line ("Out for 2026-27 season") is the claim this row records. |
| `note` | no | Anything else, including a second source. |

## Check that a row did something

A misspelt name produces a row that does nothing — the injured player keeps
scoring at full value. `compute-projections` warns `season-long absence matched
no projected player` for every such row, and
`crates/cstat-core/tests/curated_absences.rs` asserts each row lands as an
`OutForSeason` entry with the player off the scored roster:

```bash
DATABASE_URL=... MODEL_DIR=$PWD/training/models \
  cargo test -p cstat-core --test curated_absences -- --ignored
```

## Where to look

RotoWire's player pages carry a status line for season-ending injuries. Its
injury report (`/cbasketball/injury-report.php`) is backed by a JSON endpoint,
`/cbasketball/tables/injury-report.php?team=ALL&pos=ALL&conf=ALL`, but logged
out it returns only a five-row sample, so the full list needs an account.
Automating this capture is tracked separately; until then, curate by hand.
