#!/usr/bin/env python3
"""Build data/draft/{year}_big_board.json for historical NBA drafts.

Unlike the live 2026 board (a *pre-draft* Tankathon prospect ranking captured in
`data/draft/2026_big_board.json`), a historical year's board is reconstructed
from the **actual draft results** — the pick order IS the rank. Source data is
the same Tankathon past-drafts paste that `build_historical_draft_entrants.py`
already holds in its `RAW` dict, so no new fetch is needed; this script just
reshapes it into the board schema the `/api/draft/{year}` route reads.

Semantics vs the 2026 board:
  - `rank` is the real draft slot (pick #), not a pre-draft big-board rank.
  - Non-college picks (Tankathon's "NON-COLLEGE" tag: international / G-League)
    are KEPT in true draft order so pick numbers don't skip; they're marked
    `class_year: "International"` so the route classifies them as `international`
    and renders them without a CamPom chip (they have no cstat college row),
    exactly like the internationals on the 2026 board.
  - There is NO undrafted tail: the source is drafted-only. Undrafted college
    players simply aren't in the past-drafts data, so they can't be listed.

College picks join to cstat by normalized (name, team) inside the route and are
stamped `gone` via the `draft_entrants` table (loaded from the sibling
early-entrants files), so this board file only needs rank/name/team/tier.

Re-run:  python3 scripts/build_historical_draft_boards.py
"""
import json
import re
from pathlib import Path

# Reuse the verbatim past-drafts paste (pick | name | team) already curated in
# the early-entrants builder for 2015-2025. scripts/ is on sys.path[0] when this
# runs.
from build_historical_draft_entrants import RAW

OUT_DIR = Path(__file__).resolve().parent.parent / "data" / "draft"

# Capture date of the underlying Tankathon past-drafts paste (see the sibling
# builder's header). Stamped on every row for provenance parity with 2026.
AS_OF = "2026-06-01"

# 2026 real draft results (Tankathon past-drafts/2026, fetched 2026-07-01). Kept
# LOCAL to this script rather than folded into the sibling builder's RAW so it
# does NOT regenerate the hand-maintained `2026_early_entrants.json` (which the
# live projection reads). The pre-draft `2026_big_board.json` this overwrites was
# a prospect ranking; post-draft the board becomes the actual pick order, in
# line with every other historical year.
RAW_2026 = """
1 | AJ Dybantsa | BYU
2 | Darryn Peterson | Kansas
3 | Cameron Boozer | Duke
4 | Caleb Wilson | North Carolina
5 | Keaton Wagler | Illinois
6 | Mikel Brown Jr. | Louisville
7 | Darius Acuff Jr. | Arkansas
8 | Kingston Flemings | Houston
9 | Morez Johnson Jr. | Michigan
10 | Brayden Burries | Arizona
11 | Yaxel Lendeborg | Michigan
12 | Aday Mara | Michigan
13 | Nate Ament | Tennessee
14 | Hannes Steinbach | Washington
15 | Dailyn Swain | Texas
16 | Bennett Stirtz | Iowa
17 | Ebuka Okorie | Stanford
18 | Christian Anderson | Texas Tech
19 | Allen Graves | Santa Clara
20 | Jayden Quaintance | Kentucky
21 | Karim López | NON-COLLEGE
22 | Labaron Philon Jr. | Alabama
23 | Zuby Ejiofor | St. John's
24 | Cameron Carr | Baylor
25 | Sergio de Larrea | NON-COLLEGE
26 | Tarris Reed Jr. | UConn
27 | Chris Cenac Jr. | Houston
28 | Joshua Jefferson | Iowa State
29 | Alex Karaban | UConn
30 | Koa Peat | Arizona
31 | Bruce Thornton | Ohio State
32 | Richie Saunders | BYU
33 | Isaiah Evans | Duke
34 | Meleek Thomas | Arkansas
35 | Trevon Brazile | Arkansas
36 | Baba Miller | Cincinnati
37 | Ryan Conwell | Louisville
38 | Braden Smith | Purdue
39 | Jack Kayil | NON-COLLEGE
40 | Dillon Mitchell | St. John's
41 | Otega Oweh | Kentucky
42 | Ja'Kobi Gillespie | Tennessee
43 | Tyler Bilodeau | UCLA
44 | Maliq Brown | Duke
45 | Emanuel Sharp | Houston
46 | Felix Okpara | Tennessee
47 | Tyler Nickel | Vanderbilt
48 | Tobi Lawal | Virginia Tech
49 | Bryce Hopkins | St. John's
50 | Jaden Bradley | Arizona
51 | Izaiyah Nelson | South Florida
52 | Henri Veesaar | North Carolina
53 | Ugonna Onyenso | Virginia
54 | Lajae Jones | Florida State
55 | Nick Martinelli | Northwestern
56 | Vsevolod Ishchenko | NON-COLLEGE
57 | Narcisse Ngoy | NON-COLLEGE
58 | Jaron Pierre Jr. | SMU
59 | Trey Kaufman-Renn | Purdue
60 | Malique Lewis | NON-COLLEGE
"""

# "1 | name | team" or "1. name | team" → (pick, "name | team")
PICK = re.compile(r"^\s*(\d+)\s*[.|]\s*(.*)$")


def tier_for(pick: int) -> str:
    """Real-draft tiering: lottery 1-14, 1st round 15-30, 2nd round 31-60.

    Mirrors the 2026 board's tier bands. There is no `fringe`/`unranked` bucket
    for a results-based board — every row is an actual pick within 1..60."""
    if pick <= 14:
        return "lottery"
    if pick <= 30:
        return "1st-round"
    return "2nd-round"


def parse(block: str) -> list[dict]:
    out = []
    for line in block.strip().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("*"):
            continue
        m = PICK.match(line)
        if not m:
            continue
        pick = int(m.group(1))
        parts = [p.strip() for p in m.group(2).split("|")]
        name, team = parts[0], parts[-1]
        row = {
            "rank": pick,
            "name": name,
            "tier": tier_for(pick),
            "source": "tankathon",
            "as_of": AS_OF,
        }
        if "NON-COLLEGE" in team.upper():
            # No cstat college row; flag international so the route skips the
            # CamPom join and renders it as a non-college pick.
            row["current_team"] = "International"
            row["class_year"] = "International"
        else:
            row["current_team"] = team
        # Stable key order for a clean diff.
        out.append(
            {
                k: row[k]
                for k in ("rank", "name", "current_team", "class_year", "tier", "source", "as_of")
                if k in row
            }
        )
    return out


def main() -> None:
    # 2015-2025 from the shared past-drafts paste + 2026 real results (local).
    blocks = {**RAW, 2026: RAW_2026}
    for year, block in sorted(blocks.items()):
        rows = parse(block)
        path = OUT_DIR / f"{year}_big_board.json"
        path.write_text(json.dumps(rows, indent=2) + "\n")
        college = sum(1 for r in rows if r["current_team"] != "International")
        intl = len(rows) - college
        print(f"{year}: wrote {len(rows)} picks ({college} college, {intl} non-college) → {path.name}")


if __name__ == "__main__":
    main()
