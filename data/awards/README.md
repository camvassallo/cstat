# Awards reference data

`consensus_all_americans.csv` — NCAA Division I men's basketball **consensus
All-Americans** (first and second team) plus **AP Player of the Year**, for
the 2014-15 through 2025-26 seasons.

| column | meaning |
| --- | --- |
| `season` | end year, matching cstat's `season` convention (2026 = the 2025-26 season) |
| `player` | name as published by the selectors (Torvik-style common name, not the NatStat legal name) |
| `school` | school as published |
| `consensus_team` | `1` = consensus first team, `2` = consensus second team |
| `poy` | `true` if the player won AP Player of the Year that season |

125 rows, 12 seasons. Every POY in this window was also a consensus first-team
selection, so `poy` is a flag on an existing row rather than a separate tier.

**Source**: Wikipedia's per-year `{YEAR} NCAA Men's Basketball All-Americans`
pages (consensus tables only) and `AP College Basketball Player of the Year`.
Retrieved 2026-07-31.

## Why this exists

This is **the fitness function for CamPom**. CamPom is a descriptive grade
whose job is that the best player in the country carries the highest score,
and awards are the only non-circular ground truth for that. See the
"Validation" section of `docs/campom_methodology.md` for the study this data
supports, including why NBA draft outcome and team attribution are *not*
valid targets — they pull in opposite directions and neither measures college
value.

## Matching caveat

`player` uses the common name. cstat's `players.name` comes from NatStat and
uses the legal name, so several entries do not match on an exact-name join:

| this file | `players.name` |
| --- | --- |
| Obi Toppin | Obadiah Toppin |
| Ja Morant | Temetrius Morant |
| Johnny Davis | Jonathan Davis |

A further set fails because `players.name` contains raw HTML entities
(`D&#039;Angelo Russell`). Both problems are tracked in **issue #243**; until
it is fixed, use `training/awards.py`, which carries the normalization and the
alias table. Once #243 lands the aliases should mostly become unnecessary —
re-check `load_awards(engine)`'s reported match rate before deleting them.
