#!/usr/bin/env python3
"""Parse a saved 247Sports transfer-portal-top-prospects HTML page into the
JSON shape we ship at `data/transfers/{year}.json` (consumed by
`crates/cstat-api/src/routes/transfers.rs`).

Usage:
    python3 scripts/parse_247_transfer_html.py \\
        "docs/2025 College Basketball Transfer Portal Top Prospects.html" \\
        data/transfers/2025.json

The 2026 file was produced by a different scrape pipeline; this script
reproduces the same field shape so the route loader treats both years
uniformly. Field semantics:
  rank          — 247 ordinal (int)
  name          — "First Last"
  position      — 247 position string (PG/SG/SF/PF/C, or "")
  height        — feet-inches dash-string ("6-9"), or null
  weight        — int lbs, or null
  status        — "Enrolled" / "Committed" / "" (matches 2026.json convention)
  rating_247    — composite rating in 0–1 range (e.g. 0.99), or null
  previous_team — short team alt-text from the source-school <img>, or null
  next_team     — short team alt-text from the destination <img>, or null
  url_247       — canonical 247 player URL (used for "view on 247" link)
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

from bs4 import BeautifulSoup, Tag


def text(node: Tag | None) -> str | None:
    if node is None:
        return None
    s = node.get_text(strip=True)
    return s or None


def parse_bio(bio: str | None) -> tuple[str | None, int | None]:
    """247 renders the bio as `6-9/230`. The slash is sometimes injected via
    HTML comment so `get_text(strip=True)` collapses it cleanly. Either side
    can be missing on partially-filled records."""
    if not bio:
        return None, None
    parts = [p.strip() for p in bio.split("/")]
    height = parts[0] if parts and parts[0] else None
    weight = None
    if len(parts) > 1 and parts[1]:
        try:
            weight = int(parts[1])
        except ValueError:
            weight = None
    return height, weight


def parse(html: str) -> list[dict]:
    soup = BeautifulSoup(html, "html.parser")
    rows: list[dict] = []
    # NB: `lxml` chokes on this 247 export (it returns 0 elements); html.parser
    # is the lowest-fuss option that handles their nested <ul> structure.
    for li in soup.select("li.transfer-player.is-ranked"):
        rank_el = li.select_one(".playerRank span")
        try:
            rank = int(rank_el.get_text(strip=True)) if rank_el else None
        except ValueError:
            rank = None
        if rank is None:
            continue  # malformed row — drop

        name_a = li.select_one("h3 a")
        name = text(name_a)
        url_247 = name_a.get("href") if name_a else None

        rating_text = text(li.select_one(".rating"))
        try:
            rating = float(rating_text) if rating_text else None
        except ValueError:
            rating = None

        position = text(li.select_one(".position")) or ""
        height, weight = parse_bio(text(li.select_one(".bio")))

        # The status div is `<div class="status is-enrolled">Enrolled</div>`
        # — `find` matches on any element whose class list contains "status",
        # which is what we want (CSS selector `.status` would pick up other
        # `*-status-*` classes elsewhere on the page).
        status_el = li.find("div", class_="status")
        status = text(status_el) or ""

        pred = li.select_one(".transfer-prediction")
        previous_team = None
        next_team = None
        if pred:
            src = pred.select_one("a img.source")
            if src:
                previous_team = src.get("alt") or None
            dest = pred.select_one("li.destination img")
            if dest:
                next_team = dest.get("alt") or None

        rows.append({
            "rank": rank,
            "name": name,
            "position": position,
            "height": height,
            "weight": weight,
            "status": status,
            "rating_247": rating,
            "previous_team": previous_team,
            "next_team": next_team,
            "url_247": url_247,
        })

    rows.sort(key=lambda r: r["rank"])
    return rows


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: parse_247_transfer_html.py <input.html> <output.json>", file=sys.stderr)
        sys.exit(2)
    inp = Path(sys.argv[1])
    out = Path(sys.argv[2])
    rows = parse(inp.read_text(encoding="utf-8"))
    out.write_text(json.dumps(rows, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {len(rows)} transfers → {out}")


if __name__ == "__main__":
    main()
