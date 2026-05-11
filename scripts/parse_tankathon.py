#!/usr/bin/env python3
"""Parse a Tankathon NBA Draft big-board paste into the JSON shape we ship at
`data/draft/{year}_big_board.json` (consumed by the upcoming `/draft` page).

The paste is one-element-per-line with a 17-line block per player:
    <rank|NR>\\t<school>     # line 0  ("1\\tDuke")
    <name>                  # line 1
    <pos> | <team>          # line 2  (team may differ from school for intl)
    <height>                # line 3  ("6'9\\"")
    <weight> lbs            # line 4
    <class>                 # line 5  (Freshman/.../International/G League)
    <age> yrs               # line 6
    pts / <pts>             # lines 7-8
    reb / <reb>             # lines 9-10
    ast / <ast>             # lines 11-12
    blk / <blk>             # lines 13-14
    stl / <stl>             # lines 15-16
Tier separator lines ("TIER 2", "THE REST", "Unranked Players (Alphabetical)")
are skipped — tier is derived from rank instead.

Usage:
    python3 scripts/parse_tankathon.py docs/tankathon data/draft/2026_big_board.json
"""
from __future__ import annotations

import json
import re
import sys
from datetime import date
from pathlib import Path

SEPARATORS = {"TIER 2", "THE REST", "Unranked Players (Alphabetical)"}


def height_to_dash(h: str) -> str | None:
    m = re.match(r"^\s*(\d+)'(\d+(?:\.\d+)?)\"?\s*$", h)
    return f"{m.group(1)}-{m.group(2)}" if m else (h.strip() or None)


def parse_weight(w: str) -> int | None:
    m = re.match(r"^\s*(\d+)", w)
    return int(m.group(1)) if m else None


def parse_age(a: str) -> float | None:
    m = re.match(r"^\s*(\d+(?:\.\d+)?)", a)
    return float(m.group(1)) if m else None


def tier_from_rank(rank: int | None) -> str:
    if rank is None:
        return "unranked"
    if rank <= 14:
        return "lottery"
    if rank <= 30:
        return "1st-round"
    if rank <= 60:
        return "2nd-round"
    return "fringe"


def parse(src: Path) -> list[dict]:
    lines = src.read_text().splitlines()
    rows: list[dict] = []
    i = 0
    while i < len(lines):
        line = lines[i].strip()
        if not line or line in SEPARATORS:
            i += 1
            continue
        m = re.match(r"^(\d+|NR)\s+(.+)$", line)
        if not m or i + 16 >= len(lines):
            i += 1
            continue
        rank = None if m.group(1) == "NR" else int(m.group(1))
        name = lines[i + 1].strip()
        pos_team = lines[i + 2].strip()
        if "|" in pos_team:
            position, team = (s.strip() for s in pos_team.split("|", 1))
        else:
            position, team = pos_team, m.group(2).strip()
        rows.append(
            {
                "rank": rank,
                "name": name,
                "current_team": team,
                "position": position,
                "height": height_to_dash(lines[i + 3]),
                "weight": parse_weight(lines[i + 4]),
                "class_year": lines[i + 5].strip() or None,
                "age": parse_age(lines[i + 6]),
                "tier": tier_from_rank(rank),
                "stats": {
                    "pts": float(lines[i + 8].strip()),
                    "reb": float(lines[i + 10].strip()),
                    "ast": float(lines[i + 12].strip()),
                    "blk": float(lines[i + 14].strip()),
                    "stl": float(lines[i + 16].strip()),
                },
                "source": "tankathon",
                "as_of": date.today().isoformat(),
            }
        )
        i += 17
    return rows


if __name__ == "__main__":
    src = Path(sys.argv[1] if len(sys.argv) > 1 else "docs/tankathon")
    dst = Path(sys.argv[2] if len(sys.argv) > 2 else "data/draft/2026_big_board.json")
    data = parse(src)
    dst.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {len(data)} players to {dst}")
