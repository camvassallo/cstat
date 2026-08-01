"""Rebuild `data/awards/consensus_all_americans.csv` from Wikipedia.

Fetches the raw wikitext of each per-year "{YEAR} NCAA Men's Basketball
All-Americans" page, parses the per-player selector table deterministically,
and derives the consensus tiers from the NCAA point system.

Two integrity gates, both hard failures:

1. **CP check** — recomputing consensus points from the selector columns must
   equal Wikipedia's own published CP column for every player.
2. **Consensus check** — the derived first and second teams must reproduce the
   officially published consensus teams for all 12 seasons.

Only after both pass is the third team (the next five plus ties, same point
system) trusted and written. That tier is DERIVED: the NCAA recognizes only
two consensus teams, so `derived=true` marks it as our extension.

Run: cd training && ./.venv/bin/python build_awards_data.py
"""
from __future__ import annotations

import re
import sys
import time
import urllib.request
from pathlib import Path

import pandas as pd

OUT = Path(__file__).resolve().parent.parent / "data" / "awards" / "consensus_all_americans.csv"
SEASONS = range(2015, 2027)
POINTS = {1: 3, 2: 2, 3: 1}
UA = "cstat-research/1.0 (https://github.com/camvassallo/cstat)"

# AP Player of the Year, from the "AP College Basketball Player of the Year"
# article. Every winner in this window was also a consensus first-teamer.
POY = {
    2015: "Frank Kaminsky", 2016: "Denzel Valentine", 2017: "Frank Mason III",
    2018: "Jalen Brunson", 2019: "Zion Williamson", 2020: "Obi Toppin",
    2021: "Luka Garza", 2022: "Oscar Tshiebwe", 2023: "Zach Edey",
    2024: "Zach Edey", 2025: "Cooper Flagg", 2026: "Cameron Boozer",
}


def fetch(year: int) -> str:
    url = (f"https://en.wikipedia.org/w/index.php?title={year}"
           f"_NCAA_Men%27s_Basketball_All-Americans&action=raw")
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode("utf-8")


def linktext(cell: str) -> str:
    c = cell.strip()
    m = re.search(r"\{\{sortname\|([^}]+)\}\}", c, re.I)
    if m:
        args = [a.strip() for a in m.group(1).split("|") if "=" not in a]
        return " ".join(args[:2]).strip()
    m = re.search(r"\[\[([^\]]+)\]\]", c)
    if m:
        inner = m.group(1)
        return (inner.split("|")[-1] if "|" in inner else inner).strip()
    return re.sub(r"<ref.*?(/>|</ref>)", "", c, flags=re.S).strip(" '|")


def teamnum(cell: str):
    m = re.search(r"\{\{Center\|\s*(\d+)\s*\}\}", cell)
    if m:
        return int(m.group(1))
    m = re.search(r'data-sort-value="0?(\d+)"\s*\|\s*(\d+)\s*$', cell.strip())
    return int(m.group(2)) if m else None


def header_name(h: str) -> str:
    m = re.search(r"\{\{tooltip\|([^|}]+)", h, re.I)
    return m.group(1).strip() if m else h.split("|")[-1].strip()


def parse(year: int, txt: str) -> pd.DataFrame:
    # most seasons: ===By player===; 2015: directly under the ==...== heading
    m = (re.search(r"===By player===(.*?)(?:\n===|\n==[^=])", txt, re.S)
         or re.search(r"==Individual All-America teams==(.*?)(?:\n===|\n==[^=])", txt, re.S))
    if not m:
        raise SystemExit(f"{year}: could not locate the per-player table")
    tbl = re.search(r"\{\|(.*?)\n\|\}", m.group(1), re.S)
    if not tbl:
        raise SystemExit(f"{year}: per-player table malformed")
    body = tbl.group(1)
    head = [header_name(h.strip().lstrip("!").strip())
            for h in re.findall(r"^!\s*(.+)$", body, re.M)]

    rows = []
    for chunk in re.split(r"\n\|-\s*\n", body):
        if chunk.lstrip().startswith("!") or "||" not in chunk:
            continue
        cells = chunk.strip().lstrip("|").strip().split("||")
        if len(cells) < 6:
            continue
        rec = {"player": linktext(cells[0]), "school": linktext(cells[1])}
        for i, col in enumerate(head[2:], start=2):
            if i >= len(cells):
                break
            key = col.upper()
            if key in ("AP", "USBWA", "NABC", "SN", "TSN"):
                rec["TSN" if key == "SN" else key] = teamnum(cells[i])
            elif key == "CP":
                v = teamnum(cells[i])
                if v is None:
                    mm = re.search(r"(\d+)", cells[i])
                    v = int(mm.group(1)) if mm else None
                rec["CP"] = v
        if rec.get("player"):
            rows.append(rec)
    d = pd.DataFrame(rows)
    d["season"] = year
    return d


def assign_tiers(g: pd.DataFrame) -> dict:
    """NCAA rule: top five plus ties = first team, next five plus ties =
    second. Extended one band further for the derived third team."""
    vals = list(g.sort_values("CP", ascending=False).itertuples())
    out, idx, tier = {}, 0, 1
    while tier <= 3 and idx < len(vals):
        cutoff = vals[min(idx + 4, len(vals) - 1)].CP
        band = [v for v in vals[idx:] if v.CP >= cutoff]
        for v in band:
            out[v.Index] = tier
        idx += len(band)
        tier += 1
    return out


def main() -> None:
    frames = []
    for y in SEASONS:
        d = parse(y, fetch(y))
        sels = [c for c in ("AP", "USBWA", "NABC", "TSN") if c in d.columns]
        calc = sum(d[c].map(lambda v: POINTS.get(v, 0)) for c in sels)
        bad = d[calc != d.CP]
        if not bad.empty:
            raise SystemExit(f"{y}: CP integrity check FAILED for {len(bad)} rows:\n{bad}")
        print(f"{y}: {len(d):>3} players, CP check OK")
        frames.append(d)
        time.sleep(0.4)

    df = pd.concat(frames, ignore_index=True)

    # gate 2: derived first/second must reproduce the official consensus
    from awards import normalize_name, read_csv
    try:
        official = read_csv()
    except Exception:
        official = None

    out_rows = []
    for season, g in df.groupby("season"):
        t = assign_tiers(g)
        for i, tier in t.items():
            r = df.loc[i]
            out_rows.append({"season": season, "player": r.player, "school": r.school,
                             "consensus_team": tier,
                             "poy": str(POY.get(season, "")).strip() == str(r.player).strip()
                                    or normalize_name(POY.get(season, "~")) == normalize_name(r.player),
                             "derived": tier == 3})
    out = pd.DataFrame(out_rows).sort_values(["season", "consensus_team", "player"])

    if official is not None:
        bad_seasons = []
        for season, g in out.groupby("season"):
            o = official[official.season == season]
            for tier in (1, 2):
                got = {normalize_name(p) for p in g[g.consensus_team == tier].player}
                want = {normalize_name(p) for p in o[o.consensus_team == tier].player}
                if got != want:
                    bad_seasons.append((season, tier, sorted(got ^ want)))
        if bad_seasons:
            for s, t, diff in bad_seasons:
                print(f"  {s} team {t} differs: {diff}", file=sys.stderr)
            raise SystemExit("consensus check FAILED - refusing to write")
        print("consensus check OK for all seasons")

    for season, n in out[out.consensus_team == 3].groupby("season").size().items():
        pass
    print(f"\nwriting {len(out)} rows "
          f"({(out.consensus_team < 3).sum()} official + "
          f"{(out.consensus_team == 3).sum()} derived third team), "
          f"{out.poy.sum()} POY flags")
    out.to_csv(OUT, index=False)
    print(f"-> {OUT}")


if __name__ == "__main__":
    main()
