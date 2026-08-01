# Awards reference data

`consensus_all_americans.csv` — NCAA Division I men's basketball All-America
selections plus **AP Player of the Year**, for the 2014-15 through 2025-26
seasons. 193 rows over 12 seasons.

| column | meaning |
| --- | --- |
| `season` | end year, matching cstat's `season` convention (2026 = the 2025-26 season) |
| `player` | name as published by the selectors (common name, not the NatStat legal name) |
| `school` | school as published |
| `consensus_team` | `1` / `2` = official consensus first / second team; `3` = **derived** third team |
| `poy` | `true` if the player won AP Player of the Year that season |
| `derived` | `true` for the third team only — see below |

Every POY in this window was also a consensus first-team selection, so `poy`
is a flag on an existing row rather than a separate tier.

## Regenerating

```bash
cd training && ./.venv/bin/python build_awards_data.py
```

That script fetches the raw wikitext of each per-year
`{YEAR} NCAA Men's Basketball All-Americans` page, parses the per-player
selector table, and derives the tiers. Do not hand-edit this CSV — fix the
builder instead. (An earlier version of this data *was* hand-transcribed from
summarized page fetches, and two independent fetches of the same page
disagreed on several seasons. The builder exists because that is not a
trustworthy way to produce ground truth.)

## The third team is derived, not official

The NCAA recognizes only **two** consensus teams. Consensus status comes from
a point system over the four major selectors (AP, USBWA, NABC, Sporting News):
three points for a first-team selection, two for second, one for third; the
top five totals plus ties are the first team, the next five plus ties the
second.

Rows with `consensus_team = 3` extend that same system one band further — the
*next* five plus ties. It uses the identical selectors and weights as the two
official tiers, so it is methodologically consistent, but it is **our
construction and carries no official standing.** `derived = true` marks it.

## Integrity gates

The builder hard-fails rather than writing bad data if either gate trips:

1. **CP check** — recomputing each player's consensus points from the selector
   columns must equal Wikipedia's own published `CP` column. All 210 parsed
   player-seasons currently pass.
2. **Consensus check** — the derived first and second teams must reproduce the
   `Consensus First Team` / `Consensus Second Team` tables that Wikipedia
   publishes on the same page. All 12 seasons currently pass, which is what
   justifies trusting the third band.

   That comparison is parsed straight from the page and **must never read this
   CSV** — checking the builder's output against the file the builder writes
   would be circular and the gate could never fail.

## Why this exists

This is **the fitness function for CamPom**. CamPom is a descriptive grade
whose job is that the best player in the country carries the highest score,
and awards are the only non-circular ground truth for that. See the
"Validation" section of `docs/campom_methodology.md`, including why NBA draft
outcome and team attribution are *not* valid targets — they pull in opposite
directions and neither measures college value.

## Matching caveat

`player` uses the common name; cstat's `players.name` comes from NatStat and
uses the legal name, so several entries do not match on an exact-name join:

| this file | `players.name` |
| --- | --- |
| Obi Toppin | Obadiah Toppin |
| Ja Morant | Temetrius Morant |
| Johnny Davis | Jonathan Davis |
| Herbert Jones | Herb Jones |
| Kay Felder | Kahlil Felder |
| Filip Petrušev | Filip Petrus**e**y (NatStat misspelling) |

Others fail because `players.name` contains raw HTML entities
(`D&#039;Angelo Russell`), and several have a NULL `player_id` on their Torvik
row so they cannot be reached through `players` at all. Both are tracked in
**issue #243**. Until it is fixed use `training/awards.py`, which carries the
normalization, school disambiguation, and an explicit `torvik_pid` override
table. Re-check `load_awards()`'s reported match rate (currently 193/193)
before deleting any override.
