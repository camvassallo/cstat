"""Parse a raw Tankathon mock-draft paste into data/draft/{year}_mock_draft.json.

Tankathon's "Copy to clipboard" output is a newline-soup of pick blocks. Each
block ends with a stats line (`X.X pts\tX.X reb\t...`) which we use as a hard
delimiter — much sturdier than counting lines, since blocks vary in length
(picks #1-9 sometimes carry a separate change-indicator line, picks #10+ pack
pick + team onto one tab-separated line).

Within each block we anchor on the "POS | School" line: the player name is the
line immediately before it, the team code is the line before the name (or
embedded on the pick-number line for compact-format picks).

Run:
    python3 scripts/draft/parse_tankathon_mock.py tankathon.txt 2026

Outputs `data/draft/{year}_mock_draft.json` — list of
`{pick, name, team, school, position}` records. Wire-formats no extras
beyond what the projection route consumes (mock pick as an informational
chip on uncertain `?` rows). Source attribution + `as_of` date are dropped
into a top-level `meta` block for the UI tooltip.
"""

import json
import re
import sys
from datetime import date
from pathlib import Path
from typing import List, Optional

POS_SCHOOL_RE = re.compile(r"^[A-Z/]+\s*\|\s*.+$")
TEAM_ON_PICK_LINE_RE = re.compile(r"^(\d+)\t([A-Z]{2,4})\t?$")
INT_LINE_RE = re.compile(r"^\d+$")
TEAM_CODE_RE = re.compile(r"^[A-Z]{2,4}\t?$")


def parse(raw: str) -> List[dict]:
    lines = raw.splitlines()
    picks: List[dict] = []
    block: List[str] = []
    for line in lines:
        block.append(line)
        # Stats line shape: "26.4 pts\t7.1 reb\t3.8 ast\t…" — the only line
        # in a block containing " pts\t". Use it as the block terminator
        # since pick blocks vary in length (some carry a change-indicator
        # line, some don't, and pick #10+ packs pick+team onto one line).
        if " pts\t" in line:
            picks.append(parse_block(block))
            block = []
    return [p for p in picks if p is not None]


def parse_block(block: List[str]) -> Optional[dict]:
    cleaned = [ln.rstrip() for ln in block if ln.strip()]
    if not cleaned:
        return None
    # Find the POS | School anchor — strongest signal in the block.
    anchor_idx = next(
        (i for i, ln in enumerate(cleaned) if POS_SCHOOL_RE.match(ln)),
        None,
    )
    if anchor_idx is None or anchor_idx == 0:
        return None
    name = cleaned[anchor_idx - 1].strip()
    pos_school = cleaned[anchor_idx]
    position, _, school = pos_school.partition(" | ")
    position = position.strip()
    school = school.strip()

    # Walk back from the name to find pick + team. Either:
    #   compact: a single line "N\tTEAM\t"  (one row before the name, or two)
    #   long:    "N" (+ optional change "M") then "TEAM\t"  (two or three rows)
    # Walk backward from the player name. The change indicator (a small
    # int between team and pick) and the pick number itself look
    # identical to the regex, so we can't break on the first integer —
    # we have to keep walking and let the topmost integer win.
    pick: Optional[int] = None
    team: Optional[str] = None
    for j in range(anchor_idx - 2, -1, -1):
        ln = cleaned[j].strip()
        m = TEAM_ON_PICK_LINE_RE.match(cleaned[j])
        if m:
            # Compact pick #10+ format: "N\tTEAM\t" on one line.
            pick = int(m.group(1))
            team = m.group(2)
            break
        if TEAM_CODE_RE.match(ln) and team is None:
            team = ln.rstrip("\t").strip()
            continue
        if INT_LINE_RE.match(ln) and team is not None:
            # Always overwrite — last integer wins, so the topmost
            # integer (the actual pick number) survives over any
            # change indicator below it.
            pick = int(ln)
            continue
        # Hit a non-integer, non-team line — the pick block above this
        # belongs to the prior pick. Stop.
        break

    if pick is None or team is None:
        return None
    return {
        "pick": pick,
        "name": name,
        "team": team,
        "school": school,
        "position": position,
    }


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: parse_tankathon_mock.py <input.txt> <year>", file=sys.stderr)
        sys.exit(2)
    src = Path(sys.argv[1])
    year = int(sys.argv[2])
    raw = src.read_text()
    picks = parse(raw)
    # Sort by pick + dedupe (any malformed double-emit collapses to first).
    picks.sort(key=lambda p: p["pick"])
    out_path = Path(__file__).resolve().parents[2] / "data" / "draft" / f"{year}_mock_draft.json"
    out = {
        "meta": {
            "source": "tankathon",
            "as_of": date.today().isoformat(),
            "year": year,
            "count": len(picks),
        },
        "picks": picks,
    }
    out_path.write_text(json.dumps(out, indent=2) + "\n")
    print(f"wrote {out_path} — {len(picks)} picks")


if __name__ == "__main__":
    main()
