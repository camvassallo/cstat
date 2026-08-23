# Official school rosters

`cstat-ingest rosters` reads the roster each school publishes on its own
athletics site and stores it in `team_roster_fetches` / `team_roster_players`
(migration 054). It is the only forward-looking roster signal cstat has.

**It does not feed the projection's scored roster, and that is deliberate.**
See *Why this is not a projection input* below before wiring it into one.

## Why

`players` is box-score-derived — a row exists once somebody has played a game —
so cstat's entire roster picture looks backwards. Four populations are therefore
invisible to the preseason projection, and they concentrate in the teams the
projection is already weakest on:

| population | why no existing feed sees it |
| --- | --- |
| redshirts staying at the same school | no portal row, no draft row, and Torvik has no `class_year` for an unplayed season |
| D2/D3 up-transfers | in the 247 portal, but with no D-I history to resolve `cstat_player_id` against |
| JuCo arrivals | same |
| direct international signings | often in no feed at all |

The middle two are measurable: of the 1,575 portal rows for the 2026 cycle,
170 carry a destination but no `cstat_player_id`. South Alabama had four such
commits and still projected with **zero** arrivals. On the live 2027 board, 77
of 364 teams sit below `MIN_QUALIFYING_FOR_PROJECTION` and almost all of them
show `arrivals = 0`.

`docs/redshirt_handling.md` names this exact data source as the blocker for the
cases its PR 1 and PR 3 could not reach, and records that NatStat's roster
endpoint cannot substitute — it still listed Mario Saint-Supery on Gonzaga two
weeks after he signed in Valencia. Gonzaga's own site does not.

## Absence is not a fact

A player's **presence** on a fetched roster is a fact from a single row. A
player's **absence** is a claim about the completeness of the whole page, and
most pages cannot support it:

- On 2026-08-23 Gonzaga published `2026-27 Men's Basketball Roster (Returners)`
  containing **four** players. Diffing a base-season roster against that marks
  nine returners as departed.
- Campbell and Navy were still serving their **2025-26** roster in late August.
  Diffing against that marks the whole incoming class missing and silently
  reinstates players who left a year ago.

Both look exactly like a clean fetch from the outside. So every fetch records a
verdict, and only `status = 'ok'` licenses reading anything from an absence:

| status | meaning | players stored | absence usable |
| --- | --- | --- | --- |
| `ok` | full roster for the target season | yes | **yes** |
| `partial` | right season but a subset (by size or its own title), **or** a season we could not confirm | yes | no |
| `stale_season` | page serves a different season | **no** | no |
| `unsupported` | reachable, layout we cannot parse | no | no |
| `unreachable` | DNS/TLS/HTTP failure, or no roster page found | no | no |

`stale_season` is the one status that discards its players. Last season's names
are not a small truth about this season, they are the wrong roster.

Three rules produce `partial`: a headcount below `MIN_TRUSTED_ROSTER` (9); a
title containing `returner` / `incoming` / `newcomer` / `signee` / `commit` /
`recruit` (matched as whole words), which is the only place a school ever
states that the page is a subset on purpose; and a title that names **no
season at all**.

That last one matters more than it looks. `ok` means "a full roster *for the
requested season*", and four schools — Arkansas, Georgia Tech, Miami, Troy —
publish a roster titled only "Roster | Arkansas Razorbacks". Treating those as
`ok` would hand the strongest verdict, the one that licenses reading a
departure from a missing name, to the pages carrying the least evidence, and
nothing would surface it if one of them quietly started serving last season.
Only absence is withheld: their players are still stored and still true, so the
audit's eligibility section, which reads presence, is unaffected.

## Platforms

Three vendors, verified against all 364 cstat teams (2026-08-23):

| platform | teams | how |
| --- | --- | --- |
| Sidearm nextgen | 143 | unauthenticated JSON: `GET /api/v2/Sports` → the men's-basketball `sportId` → `GET /api/v2/Rosters?sportId={id}` |
| Sidearm legacy | 183 | server-rendered HTML, `sidearm-roster-player-*` vendor classes |
| WMT Digital | ~36 | server-rendered HTML, card and list layouts |
| plain roster table | ~7 | any `<table>` with a labelled header row — the WordPress-based sites (Arkansas, Kentucky, Miami, South Carolina, Oklahoma St., Troy) |

The last one is a **fallback, not a vendor**, and runs only after every
vendor-specific parser has declined. Those sites share no markup with each
other — Arkansas keys its cells `rost_field_*`, Kentucky uses
`roster-item__*` — but they all emit a real table with real headers, so
columns are mapped by **header text** rather than by position or class. One
parser covers all of them and survives a school reordering its columns, which a
positional read would silently scramble.

Two column shapes exist and the order of the checks matters: a combined
`High School/Previous School` cell must be tested before either single-field
spelling, or the transfer origin gets filed as a high school and lost. A
combined cell with no separator (`Little Rock Christian Academy`) is a high
school and nothing else — inventing a previous school from it would manufacture
exactly the signal this ingest exists to measure.

`sportId` is **per-site** (Duke 7, BU 3, LA Tech 5), so it is resolved fresh
every run and deliberately not cached in `data/team_sites.json`.

WMT ships four layouts; three are parsed. Its **table** layout (Purdue,
Nebraska, Notre Dame, Penn St., Northwestern, UCF) publishes only jersey, name,
position, height and weight — no hometown, no previous school — so it is
detected and reported `unsupported` rather than parsed into a roster that is
missing the fields worth having.

### Traps the parsers exist to handle

- **Doubled markup.** Sidearm legacy renders a mobile and a desktop copy of
  every field inside one player container. Taking the first match per container
  de-duplicates *and* picks the abbreviated academic year (`R-Jr.`) over the
  long form (`Redshirt Junior`) — the form that keeps the redshirt marker.
- **Coaching staff on the roster page.** WMT's card layouts mark staff with an
  extra class (`roster-staff-members-card-item`); its **list** layout does not
  mark them at all. San Diego State arrived as 13 players plus 13 staff, head
  coach included, and Virginia carried Ryan Odom and Malcolm Brogdon. Two
  guards: a staff-class check, and a check that a row carries at least one
  actual roster field. That second one matters beyond tidiness — staff inflate
  the headcount past `MIN_TRUSTED_ROSTER`, so LSU's genuinely partial four-man
  page was scoring `ok` on the strength of 16 staff.
- **Numeric-or-string JSON.** Sidearm's `weight` is a JSON string at Duke and a
  JSON integer at ~90 other schools. Strict typing failed the entire roster
  parse for every one of them.
- **Nicknames.** Georgia lists `Marcus "Smurf" Millender`. Quoted and
  parenthesised nicknames are stripped before normalization; single quotes are
  left alone, because they are load-bearing in Sei'Mir and O'Neal.

## Name matching

Roster names are stored verbatim and also normalized via
`cstat_core::roster_projection::normalize_player_name`, the same function the
curated captures use. The audit additionally matches on a **first initial +
surname** key, with the German digraphs folded (`ue`→`u`, `oe`→`o`, `ae`→`a`,
`ß`→`s`).

That fallback exists because the two sources spell the same person differently
often enough to matter: Virginia publishes `Johann Grünloh` where cstat carries
the transliterated `Gruenloh`, and San Diego State publishes middle names cstat
omits. Both directions of error are possible, and they are not symmetric — a
missed match invents a departure and sends someone to check a player who never
left, while a loose-key collision hides a real one. The second is the cheaper
error: it leaves the status quo, in which nothing detected that departure at
all.

## What consumes it

`cstat-ingest departures-audit --year N` gained two sections, both reading
season `N+1` rosters:

1. **Returners absent from the official roster** — the projection's returning
   cohort minus what the school lists, ranked by CamPom. Reads `trusted`
   (`status = 'ok'` only). This is what turns the audit from a worklist into
   something closer to a detector; it is still not proof, because a walk-on
   omitted from a published roster looks identical to an exit.
2. **Seniors still on the official roster** — players the `class_year = 'Sr'`
   inference deletes who the school still lists, with the school's own label
   (`5th`, `R-Sr.`, `Gr.`). Reads *every* fetch regardless of status, because
   presence is a fact. This is the only automatic signal cstat has for the
   population `docs/eligibility_5in5.md` calls invisible: a senior taking the
   extra year at the same school.

Both are worklists feeding the existing curated captures
(`data/departures/{N}_departures.json`, `data/returns/{N}_returns.json`).
Neither changes the audit's exit code, which stays reserved for curated rows
that resolve to nobody.

## Why this is not a projection input

Adding roster-confirmed players to the projection's scored roster is not a
small change, and the reason is on the record.

`train_roster_impact_model.py` builds every training roster from
`player_season_stats ... games_played >= 5` — players who actually played — so
the calibrator has never seen a roster carrying bodies with no `cam_v3`.
Serving it one is the same train/serve mismatch that got the returner-redshirt
exclusion built, measured and reverted: raw MAE 6.13 → 6.20, bias +0.22 →
+0.54, and 91 team-seasons of coverage lost
(`docs/redshirt_handling.md`). A D2 or JuCo arrival also has no `cam_v3` at all,
so scoring one needs a new projection sub-model with no obvious training target.

The principled version is a roster-impact retrain on a frame that includes these
players, with its own accept/reject gates — `docs/roster_impact_retrain_plan.md`
— not a serving-side join.

## Running it

```bash
# Whole sweep. ~2 minutes at the default concurrency.
cargo run --bin cstat-ingest -- rosters --year 2027

# One team, no writes.
cargo run --bin cstat-ingest -- rosters --year 2027 --teams Gonzaga --dry-run
```

`--year` is the season the roster is **for** (2027 = the 2026-27 rosters) — the
opposite convention to `departures` / `returns`, which take the base season.

Its default is `cstat_ingest::roster_season_for_date`, deliberately **not**
`current_natstat_season() + 1`. That `+ 1` is right only in the offseason:
`current_natstat_season` rolls forward on 1 November, so from December a bare
`+ 1` asks every site for 2027-28 while all of them still serve 2026-27, and the
season gate correctly rejects all 364 as stale — a sweep that refreshes nothing
and reads like a mass outage. Re-running in December to pick up the schools that
had not posted in August is the main reason to run it at all, so that default
had to be right.

The team → athletics-host map is `data/team_sites.json`, keyed by
`teams.short_name`. It was generated from the NCAA's public member directory
(`web3.ncaa.org/directory/api/directory/memberList?type=12&division=I`, whose
`athleticWebUrl` field covers all 367 D-I members) and then verified by fetching
every one; the ~40 name-match misses and 8 mis-assignments were corrected by
hand. It is a plain committed file — fix an entry and re-run, no regeneration step.

Each entry also carries the `platform` (and, for the HTML platforms, the
`path`) discovered on the last successful fetch. That is a hint, not a
contract: a stale one costs an extra request and is re-probed, never a wrong
answer. Keeping it current is worth doing after a sweep that recovers new
schools — without it those teams probe the Sidearm JSON API first every run,
take a 404, and store it as a `note` on a fetch that then succeeds, which reads
as a failure on an `ok` row.

Both tables are laptop-written and prod has no writer for them, so a full
`sync_to_prod.sh` carries them. A targeted push must name **both**, since
`team_roster_players` has an FK to `team_roster_fetches`:

```bash
./scripts/sync_to_prod.sh --tables team_roster_fetches,team_roster_players
```

## Coverage, 2026-08-23

364 teams: **298 `ok`**, 9 `partial`, 36 `stale_season`, 6 `unsupported`, 15
`unreachable`. 4,473 players, **2,257 with a previous school** — the D2 / JuCo /
international signal, present on just over half of all rows.

**The remaining 21 are a rendering problem, not a URL problem.** Of the 15
`unreachable`, all but a couple serve a JavaScript shell whose roster is
fetched client-side (Bradley, George Mason, Utah St., Wyoming, UTSA, Washington
St. and friends); the page in a browser is complete, the page over HTTP has no
players in it. Closing that gap means running a headless browser in the ingest,
which is a large addition for ~5% of D-I and is not obviously worth it. Hunting
for better URLs will not help — verified per-host, not assumed. The 6
`unsupported` are WMT's table layout, which is server-rendered but publishes
neither hometown nor previous school.

A stale host in the NCAA directory *is* worth fixing by hand and does happen:
Colgate's listed `gocolgateraiders.com` no longer serves a roster, and
`colgateathletics.com` does.

The 36 `stale_season` teams are the ingest working, not failing: those schools
had not published a 2026-27 roster yet in late August. Re-running in October
converts most of them.

Eligibility labels that cstat's four-value `class_year` cannot express:
143 `R-Jr.`, 127 `R-Fr.`, 120 `R-So.`, 93 `R-Sr.`, 129 `Gr.`, 65 `5th`, plus
long-form spellings — roughly 700 players.

### Rejected alternative: ESPN

ESPN's core API (`sports.core.api.espn.com`; the `site.api` host is edge-blocked
from our egress) does publish 2027 rosters, and coverage is good — 38 of a
40-team sample carried 10+ athletes. It was rejected as the primary source
because it carries **no previous-school field**, which is the single field that
makes a D2/JuCo/international arrival identifiable as one, and because its
`experience` value is derived (`Senior`) where the school states the eligibility
that actually matters (`5th`). It also costs one request per athlete against
two per school.

It remains a reasonable future *corroborator* — a cheap second opinion on
whether a school's page is complete, which is the one thing the current design
has to infer from headcount.
