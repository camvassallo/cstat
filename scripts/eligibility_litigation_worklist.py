#!/usr/bin/env python3
"""Worklist for `data/returns/{year}_returns.json`: named plaintiffs in the
5-in-5 eligibility suits, matched to cstat's base-season roster.

Why this exists. The stay-put half of the NCAA 5-in-5 rule is invisible to
every feed cstat ingests (docs/eligibility_5in5.md), and the official-roster
scrape (`cstat-ingest rosters`) turned out to be all over the place — partial
"(Returners)" pages, last year's roster left up all summer. The litigation is
a better signal for the population that actually matters: a player who is a
NAMED PLAINTIFF in a suit against the NCAA is, by construction, a player who
intends to play in the target season and whose eligibility to do so is
unresolved. That is precisely the `contested` bucket.

Source: The College Sports Litigation Tracker
(https://www.collegesportslitigationtracker.com/tracker), a hand-maintained
page with one prose block per case: description, named plaintiffs (often with
prior schools), court, status, latest event. No API, no feed, no filter — so
this script scrapes the "Athlete Eligibility Challenges" section, pulls every
name-shaped token out of each case's text, and matches it against the
base-season roster the projection actually scores.

It is a WORKLIST, not a loader. It never writes the capture. The prose is
irregular (some cases enumerate plaintiffs in a list, some describe them one by
one, some only count them), and a name-shaped token is not a player — judges,
attorneys and same-name players in other sports all match — so every candidate
is printed with enough context to be checked against the case text by a
human, who then writes the row. `--emit FILE` writes the unambiguous candidates
as templated rows to make that faster; review them before merging.

Known gaps in the tracker (as of 2026-09-18) that a curator has to cover from
news reports instead: the Jefferson Circuit (Kentucky) TRO of 2026-08-21 (Mark
Mitchell, Skyy Clark and eleven others), the North Carolina 50-plaintiff
Downton/Heitner suit (dismissed without prejudice; its Texas Tech plaintiffs
refiled in Lubbock County), and the Lubbock County TROs (Atwell, Williams).

Usage (the training venv has psycopg2):

    cd training && ./.venv/bin/python ../scripts/eligibility_litigation_worklist.py \
        --year 2026 [--html saved_tracker.html] [--emit candidates.json] [--all-names]

The inverse question — "who COULD be missing?" — is `--open-cohort`. The
population that can be absent from the capture is finite: the Class of 2022
with four D-I seasons (first Torvik season = year-3, four seasons on record),
which is the Wisne class. Every such senior who has no capture row, is not in
the portal and is not a draft entrant is projected as gone by the `Sr`
inference, and is one of exactly three things: signed professionally (the
default is right), suing (needs a `contested` row), or genuinely done (the
default is right). The mode prints them by CAM so the search is bounded — a
dozen names worth a look, not a crawl — and skips the tracker entirely. A
second section lists the cohort's uncaptured portal MOVERS: the portal does
not resolve those the way it resolves everyone else, because a four-season
senior who committed somewhere for a fifth season projects as a firm arrival
until a row makes him a `?` there, and the commit date says whether he holds
a waiver (old-rules commit) or is riding the stayed injunction.

    ... --year 2026 --open-cohort [--min-cam 8]

Exit status is 0 regardless; there is nothing to gate here.
"""
from __future__ import annotations

import argparse
import datetime
import html
import json
import os
import re
import sys
import unicodedata
import urllib.request
from collections import defaultdict
from pathlib import Path

TRACKER_URL = "https://www.collegesportslitigationtracker.com/tracker"
# The tracker's own case-level index for the Class-of-2022 suits: one row per
# case with TRO / injunction / status columns. No player names, but it covers
# cases whose filings the tracker cannot show (its "Tracker?" column reads
# "Unavailable" for, e.g., Wells v. NCAA — the Kentucky TRO — and the two
# Tennessee suits), so it is the list to check the prose section against.
SHEET_CSV_URL = (
    "https://docs.google.com/spreadsheets/d/1ER2t3p5Ack9dLnDIO0hUPYazb7f8DL6_QT3f0akLG7E"
    "/export?format=csv"
)
REPO = Path(__file__).resolve().parent.parent

# The eligibility material is two consecutive sections on the page — "Athlete
# Eligibility Challenges - Class of 2022:" (the 5-in-5 suits) and then
# "Athlete Eligibility Challenges" (JUCO clocks, waivers, ex-pros) — followed
# by the NIL section. Both matter: a JUCO-route senior is just as deleted by
# the `Sr` inference as a Class-of-2022 one.
SECTION_START_PREFIX = "Athlete Eligibility Challenges"
SECTION_END = "NIL-Related Litigation"

# A case block starts with a heading like "Borovicanin v. NCAA" (sometimes
# "Robinson (Jimmori) v. NCAA", "McQuaide v. NCAA & Patriot League") followed
# within a few lines by "(last event: ...)" or "(closed: ...)".
CASE_HEAD = re.compile(r"^\s*(?:In re )?[A-Z][^\n]{0,60} v\. NCAA[^\n]{0,40}$")

# Name-shaped token: 2-4 capitalized words, allowing initials ("R.J.", "MJ"),
# apostrophes, hyphens, and a generational suffix. Deliberately loose; the
# roster match is the filter.
# One word of a name: "Javon", "R.J.", "MJ", "JaMichael", "DeJuan", "D'Angelo",
# "Kaufman-Renn", "Ma’afu".
_WORD = r"(?:[A-Z][a-z]*(?:[A-Z][a-z]+)?\.?[A-Z]?\.?|[A-Z]{2,3})(?:[’'\-][A-Za-z]+)?"
NAME_TOKEN = re.compile(
    r"\b(" + _WORD + r"(?:\s+(?:" + _WORD + r"|de|De|Van|van|La|St\.)){1,3}"
    r"(?:,?\s+(?:Jr\.?|Sr\.?|II|III|IV))?)\b"
)

# Tokens that are never a plaintiff but always name-shaped.
STOP_WORDS = {
    "NCAA", "Division", "Judge", "Court", "District", "Circuit", "County", "Superior",
    "Description", "Current", "Status", "Latest", "Event", "Key", "Upcoming", "Dates",
    "Important", "Case", "Documents", "Complaint", "Operative", "Motion", "Order",
    "Original", "Filing", "Appeal", "Preliminary", "Injunction", "Temporary",
    "Restraining", "Response", "Opposition", "Memo", "Brief", "Notice", "Plaintiffs",
    "Plaintiff", "Class", "Sherman", "Antitrust", "Act", "University", "College",
    "State", "Community", "Junior", "Transfer", "Portal", "Rule", "Restitution",
    "Football", "Basketball", "Baseball", "Softball", "Volleyball", "Soccer", "Track",
    "Swimming", "Golf", "Tennis", "Summer", "League", "NBA", "Draft", "Bylaw", "Bylaws",
    "Exhibit", "Consumer", "Sales", "Practices", "Fair", "Business", "Unfair",
    "Competition", "Law", "Deceptive", "Trade", "Free", "Enterprise", "Open", "Courts",
    "Justice", "Chief", "Commissioner", "President", "Coach", "Attorney", "Counsel",
}


def normalize_name(name: str) -> str:
    """Mirror `cstat_core::roster_projection::normalize_player_name`: lowercase,
    accent-fold, drop non-letters, strip generational suffixes."""
    s = unicodedata.normalize("NFKD", name)
    s = "".join(c for c in s if not unicodedata.combining(c)).lower()
    s = re.sub(r"[^a-z ]", "", s)
    words = [w for w in s.split() if w not in {"jr", "sr", "ii", "iii", "iv", "v", "lll"}]
    return "".join(words)


def fetch_tracker(saved: Path | None) -> str:
    if saved:
        return saved.read_text(encoding="utf-8", errors="ignore")
    req = urllib.request.Request(TRACKER_URL, headers={"User-Agent": "Mozilla/5.0 (cstat worklist)"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8", errors="ignore")


def to_text(page: str) -> list[str]:
    t = re.sub(r"<script.*?</script>|<style.*?</style>", "", page, flags=re.S)
    t = re.sub(r"<[^>]+>", "\n", t)
    t = html.unescape(t)
    t = re.sub(r"\n\s*\n+", "\n", t)
    return [ln.rstrip() for ln in t.split("\n")]


def eligibility_cases(lines: list[str], index_keys: set[str]) -> list[dict]:
    """Split the page into case blocks and keep the eligibility ones.

    The page has a table-of-contents up top that repeats case names as short
    "Name v. NCAA:" links; real blocks are identified by a heading line
    followed (within three lines) by a "(last event: ...)" or "(closed: ...)"
    line. A block is kept when it sits in one of the two eligibility sections
    OR its case name is in the tracker's Class-of-2022 index — the second rule
    picks up suits that were dismissed and moved to "Archived Cases" (Dalley,
    Koonin), whose plaintiffs are still class members of the live federal
    action and still belong in the capture.
    """
    starts = [i for i, ln in enumerate(lines) if ln.strip().startswith(SECTION_START_PREFIX)]
    if not starts:
        sys.exit(f"could not find the '{SECTION_START_PREFIX}' section heading on the tracker page")
    start = starts[0]
    end = next((i for i in range(start + 1, len(lines)) if lines[i].strip() == SECTION_END), len(lines))
    heads = [
        i
        for i in range(len(lines))
        if CASE_HEAD.match(lines[i])
        and any(
            "(last event:" in lines[j] or "(closed:" in lines[j]
            for j in range(i + 1, min(i + 4, len(lines)))
        )
    ]
    cases = []
    for a, b in zip(heads, heads[1:] + [len(lines)]):
        block = [ln for ln in lines[a:b] if ln.strip()]
        title = block[0].strip()
        in_section = start <= a < end
        if not in_section and case_key(title) not in index_keys:
            continue
        # A second co-heading ("& Elliott v. NCAA") folds into the title.
        if len(block) > 2 and block[2].strip().startswith("&"):
            title += " " + block[2].strip()
        text = "\n".join(block)
        latest = ""
        m = re.search(r"Latest Event:\s*\n?\s*(.+?)\s*\n\s*\((\d{1,2}/\d{1,2}/\d{4})\)", text)
        if m:
            latest = f"{m.group(1).strip()} ({m.group(2)})"
        court = ""
        m = re.search(
            r"\((?:Original[^:]*|No\.)[^)]*?, ([^,)]+(?:\([^)]*\))?[^,)]*), (?:Judge|filed)[^)]*\)",
            text,
        )
        if m:
            court = m.group(1).strip()
        cases.append({"title": title, "text": text, "latest": latest, "court": court, "archived": not in_section})
    return cases


def case_key(title: str) -> str:
    """"Godfrey v. NCAA" / "Tice (Monroe) v. NCAA" / "Robinson (Jimmori) v. NCAA" -> first word."""
    return title.split()[0].split("(")[0].lower()


def load_sheet() -> dict[str, dict]:
    """The Class-of-2022 case index, keyed by the case's first word ("Godfrey")."""
    import csv
    import io

    try:
        req = urllib.request.Request(SHEET_CSV_URL, headers={"User-Agent": "Mozilla/5.0 (cstat worklist)"})
        with urllib.request.urlopen(req, timeout=60) as r:
            body = r.read().decode("utf-8", errors="ignore")
    except Exception as e:  # noqa: BLE001 — best-effort enrichment
        print(f"(case-index spreadsheet unavailable: {e})\n", file=sys.stderr)
        return {}
    out = {}
    for row in csv.DictReader(io.StringIO(body)):
        name = (row.get("Case Name") or "").strip()
        if not name:
            continue
        out[case_key(name)] = row
    return out


def sheet_line(row: dict) -> str:
    return (
        f"TRO {row.get('TRO Granted?') or '-'}"
        + (f" ({row['TRO Date']})" if row.get("TRO Date") else "")
        + f"; injunction {row.get('Injunction Granted?') or '-'}"
        + (f" ({row['Injunction Date']})" if row.get("Injunction Date") else "")
        + (f"; {row['Current Status']}" if row.get("Current Status") else "")
    )


def candidate_names(text: str) -> list[str]:
    # `Anthony "Pig" Johnson` / `Hamed "Larry" Olayinka`: the roster carries the
    # legal name, so drop the quoted nickname before tokenizing.
    text = re.sub(r"\s+[\"“][^\"”]+[\"”]\s+", " ", text)
    # The tracker separates sentences with two spaces. Without a hard break
    # there, "...signed with Tulsa.  Jaquan Sanders, who..." tokenizes as one
    # name and Sanders is never tried on his own.
    text = re.sub(r"\.\s{2,}", ".\n", text)
    seen: dict[str, str] = {}
    for m in NAME_TOKEN.finditer(text):
        raw = m.group(1).strip().rstrip(",")
        words = raw.replace(",", "").split()
        if len(words) < 2 or any(w.strip(".") in STOP_WORDS for w in words):
            continue
        key = normalize_name(raw)
        if len(key) < 5:
            continue
        seen.setdefault(key, raw)
    return list(seen.values())


def spans(raw: str) -> list[str]:
    """The token itself, then the 2- and 3-word prefixes and suffixes, so a
    name glued to a neighbouring word ("Mohand Ammad, Maryland" is not the
    problem; "Hungary. Lamar Washington" was) still finds its player."""
    words = raw.replace(",", "").split()
    out = [raw]
    for n in (3, 2):
        if len(words) > n:
            out.append(" ".join(words[:n]))
            out.append(" ".join(words[-n:]))
    return out


def load_roster(year: int) -> dict[str, list[dict]]:
    """Base-season roster with the projection's own class label and value
    metric, keyed by normalized name. Uses the same qualification gate the
    projection uses (roster_features::QUAL_MIN_*: 5 GP, 8 MPG) only as a
    reported flag, not a filter — a sub-gate plaintiff is still worth seeing."""
    try:
        import psycopg2  # type: ignore
    except ImportError:
        sys.exit("psycopg2 not importable — run with training/.venv/bin/python")
    url = os.environ.get("DATABASE_URL")
    if not url:
        env = REPO / ".env"
        if env.exists():
            for ln in env.read_text().splitlines():
                if ln.startswith("DATABASE_URL="):
                    url = ln.split("=", 1)[1].strip().strip('"')
    if not url:
        sys.exit("DATABASE_URL not set")
    conn = psycopg2.connect(url)
    cur = conn.cursor()
    cur.execute(
        """
        SELECT p.name, COALESCE(t.short_name, t.name), p.class_year,
               tps.class_year, tps.cam_gbpm_v3_psos,
               COALESCE(pss.games_played, 0) >= 5 AND COALESCE(pss.minutes_per_game, 0) >= 8
        FROM players p
        JOIN teams t ON t.id = p.team_id
        LEFT JOIN player_season_stats pss ON pss.player_id = p.id AND pss.season = p.season
        LEFT JOIN (
            SELECT DISTINCT ON (player_id, season) player_id, season, class_year, cam_gbpm_v3_psos
            FROM torvik_player_stats WHERE player_id IS NOT NULL
            ORDER BY player_id, season, torvik_pid
        ) tps ON tps.player_id = p.id AND tps.season = p.season
        WHERE p.season = %s
        """,
        (year,),
    )
    roster: dict[str, list[dict]] = defaultdict(list)
    for name, team, cls, tcls, cam, qualified in cur.fetchall():
        roster[normalize_name(name)].append(
            {
                "name": name,
                "team": team,
                "class": tcls or cls,
                "cam": None if cam is None else round(float(cam), 1),
                "qualified": bool(qualified),
            }
        )
    conn.close()
    return roster


def open_cohort(year: int, min_cam: float) -> list[dict]:
    """Class-of-(year-4) seniors with four D-I seasons whom nothing accounts
    for: no capture row, no live portal row, no draft entry. Keyed on
    `torvik_pid` for the season count because `natstat_id` breaks on a
    transfer (a new id per team), which would read a transfer's history as a
    one-season career and drop him from the cohort."""
    try:
        import psycopg2  # type: ignore
    except ImportError:
        sys.exit("psycopg2 not importable — run with training/.venv/bin/python")
    url = os.environ.get("DATABASE_URL")
    if not url:
        env = REPO / ".env"
        if env.exists():
            for ln in env.read_text().splitlines():
                if ln.startswith("DATABASE_URL="):
                    url = ln.split("=", 1)[1].strip().strip('"')
    if not url:
        sys.exit("DATABASE_URL not set")
    conn = psycopg2.connect(url)
    cur = conn.cursor()
    cur.execute(
        """
        WITH seasons AS (
            SELECT torvik_pid, min(season) AS first_season, count(DISTINCT season) AS n_seasons
            FROM torvik_player_stats WHERE torvik_pid IS NOT NULL GROUP BY 1
        ),
        seniors AS (
            SELECT p.id, p.name, COALESCE(t.short_name, t.name) AS team, tps.torvik_pid,
                   tps.cam_gbpm_v3_psos AS cam, COALESCE(pss.minutes_per_game, 0) AS mpg
            FROM players p
            JOIN teams t ON t.id = p.team_id
            JOIN torvik_player_stats tps ON tps.player_id = p.id AND tps.season = p.season
            LEFT JOIN player_season_stats pss
                   ON pss.player_id = p.id AND pss.season = p.season AND pss.team_id = p.team_id
            WHERE p.season = %(year)s AND tps.class_year = 'Sr'
        ),
        cohort AS (
            SELECT s.* FROM seniors s JOIN seasons x ON x.torvik_pid = s.torvik_pid
            WHERE x.first_season = %(year)s - 3 AND x.n_seasons = 4
        ),
        norm AS (SELECT c.*, lower(regexp_replace(c.name, '[^A-Za-z]', '', 'g')) AS k FROM cohort c),
        captured AS (
            SELECT lower(regexp_replace(player_name, '[^A-Za-z]', '', 'g')) AS k
            FROM player_returns WHERE year = %(year)s
        ),
        portal AS (
            SELECT cstat_player_id FROM transfers
            WHERE year = %(year)s AND status <> 'Withdrawn' AND cstat_player_id IS NOT NULL
        ),
        draft AS (
            SELECT lower(regexp_replace(player_name, '[^A-Za-z]', '', 'g')) AS k
            FROM draft_entrants WHERE year = %(year)s
        )
        SELECT n.name, n.team, n.cam, n.mpg,
               n.k IN (SELECT k FROM captured) AS captured,
               n.id IN (SELECT cstat_player_id FROM portal) AS in_portal,
               n.k IN (SELECT k FROM draft) AS in_draft,
               tr.destination_institution, tr.status, tr.status_date::date
        FROM norm n
        LEFT JOIN LATERAL (
            SELECT destination_institution, status, status_date FROM transfers
            WHERE year = %(year)s AND status <> 'Withdrawn' AND cstat_player_id = n.id
            ORDER BY status_date DESC NULLS LAST LIMIT 1
        ) tr ON TRUE
        ORDER BY n.cam DESC NULLS LAST, n.name
        """,
        {"year": year},
    )
    rows = [
        {
            "name": name, "team": team, "cam": None if cam is None else round(float(cam), 1),
            "mpg": round(float(mpg), 1), "captured": captured, "in_portal": in_portal, "in_draft": in_draft,
            "dest": dest, "portal_status": status, "portal_date": date,
        }
        for name, team, cam, mpg, captured, in_portal, in_draft, dest, status, date in cur.fetchall()
    ]
    conn.close()
    total = len(rows)
    counts = {
        "captured": sum(r["captured"] for r in rows),
        "in_portal": sum(r["in_portal"] and not r["captured"] for r in rows),
        "in_draft": sum(r["in_draft"] and not r["captured"] and not r["in_portal"] for r in rows),
    }
    open_rows = [r for r in rows if not (r["captured"] or r["in_portal"] or r["in_draft"])]
    print(
        f"Class of {year - 4}, four D-I seasons, labelled Sr in {year}: {total} players — "
        f"{counts['captured']} captured, {counts['in_portal']} in the portal, "
        f"{counts['in_draft']} on the draft list, {len(open_rows)} open "
        f"({sum(1 for r in open_rows if (r['cam'] or -99) >= min_cam)} at CAM >= {min_cam:g}).\n"
    )
    print("OPEN — nothing accounts for these; each is signed pro (no row), suing (`contested` row), or done (no row):")
    print(f"    {'PLAYER':<26} {'TEAM':<22} {'CAM':>5} {'MPG':>5}")
    for r in open_rows:
        if (r["cam"] or -99) < min_cam:
            continue
        print(f"    {r['name']:<26} {r['team']:<22} {r['cam'] if r['cam'] is not None else '—':>5} {r['mpg']:>5}")

    # The portal does NOT resolve a Class-of-2022 mover the way it resolves
    # everyone else. A four-season senior who committed somewhere for a
    # fifth season is, by construction, playing on contested eligibility, and
    # without a row he projects as a FIRM arrival at his new school (Seth
    # Trimble at Louisville, 2026). The commit date is the tell: a commit
    # under the old rules (before the 2026-06-23 rule change) means a waiver
    # he already holds; a commit inside the window the class injunction was
    # in force (2026-07-31 to the 2026-08-21 stay) means the injunction.
    movers = [r for r in rows if r["in_portal"] and not r["captured"] and r["portal_status"] == "Committed"]
    if movers:
        print()
        print(
            "MOVERS — committed to a new school for a fifth season, no row, so each is a FIRM arrival "
            "at the destination today. A commit dated inside the injunction window is a `contested` "
            "row (case Wisne unless named elsewhere); an old-rules commit is a waiver holder and needs "
            "no row:"
        )
        print(f"    {'PLAYER':<26} {'FROM':<22} {'TO':<22} {'DATE':<11} {'CAM':>5}")
        for r in sorted(movers, key=lambda r: (r["portal_date"] or datetime.date.min), reverse=True):
            if (r["cam"] or -99) < min_cam:
                continue
            print(
                f"    {r['name']:<26} {r['team']:<22} {(r['dest'] or '?'):<22} "
                f"{str(r['portal_date'] or '?'):<11} {r['cam'] if r['cam'] is not None else '—':>5}"
            )
    return open_rows


def load_capture(year: int) -> dict[str, dict]:
    path = REPO / "data" / "returns" / f"{year}_returns.json"
    if not path.exists():
        return {}
    rows = json.loads(path.read_text())
    return {normalize_name(r["name"]): r for r in rows}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--year", type=int, required=True, help="BASE season (2026 = rows affecting the 2027 projection)")
    ap.add_argument("--html", type=Path, help="read a saved copy of the tracker page instead of fetching")
    ap.add_argument("--emit", type=Path, help="write unambiguous, uncaptured candidates as templated capture rows")
    ap.add_argument("--all-names", action="store_true", help="also print name tokens that matched nobody")
    ap.add_argument(
        "--open-cohort",
        action="store_true",
        help="skip the tracker; list the four-season Class-of-(year-4) seniors nothing accounts for",
    )
    ap.add_argument("--min-cam", type=float, default=5.0, help="--open-cohort: CAM floor for the printed list")
    ap.add_argument(
        "--extra",
        type=Path,
        help="JSON of cases the tracker has no prose for, {\"Case v. NCAA\": {\"court\": ..., "
        "\"latest\": ..., \"names\": [...]}} — matched exactly like a tracker block",
    )
    args = ap.parse_args()

    if args.open_cohort:
        open_cohort(args.year, args.min_cam)
        return

    lines = to_text(fetch_tracker(args.html))
    sheet = load_sheet()
    cases = eligibility_cases(lines, set(sheet))
    if args.extra:
        for title, spec in json.loads(args.extra.read_text()).items():
            if title.startswith("_"):
                continue
            cases.append(
                {
                    "title": title,
                    "text": ", ".join(spec.get("names", [])),
                    "latest": spec.get("latest", ""),
                    "court": spec.get("court", ""),
                    "source": spec.get("source", ""),
                }
            )
    roster = load_roster(args.year)
    captured = load_capture(args.year)
    print(f"{len(cases)} eligibility case(s) on the tracker; {sum(len(v) for v in roster.values())} "
          f"roster rows for {args.year}; {len(captured)} row(s) already captured\n")

    emit: list[dict] = []
    totals = {"matched": 0, "captured": 0, "ambiguous": 0, "unmatched": 0}
    for case in cases:
        names = candidate_names(case["text"])
        hits, ambiguous, misses = [], [], []
        for raw in names:
            for span in spans(raw):
                key = normalize_name(span)
                players = roster.get(key, [])
                if players:
                    break
            if not players:
                misses.append(raw)
            elif len(players) == 1:
                hits.append((raw, players[0], key))
            else:
                ambiguous.append((raw, players))
        if not hits and not ambiguous:
            continue
        print(f"=== {case['title']}" + ("  [archived on the tracker]" if case.get("archived") else ""))
        if case["court"]:
            print(f"    court:  {case['court']}")
        if case["latest"]:
            print(f"    latest: {case['latest']}")
        srow = sheet.get(case_key(case["title"]))
        if srow:
            print(f"    index:  {sheet_line(srow)}")
        for raw, p, key in sorted(hits, key=lambda h: -(h[1]["cam"] or -99)):
            tag = "CAPTURED" if key in captured else "        "
            gate = "" if p["qualified"] else "  (below projection gate)"
            print(f"    {tag}  {p['name']:<26} {p['team']:<22} {str(p['class']):<4} cam {p['cam']}{gate}")
            totals["matched"] += 1
            totals["captured"] += key in captured
            if key not in captured and args.emit:
                emit.append(
                    {
                        "name": p["name"],
                        "current_team": p["team"],
                        "status": "contested",
                        "reason": "injunction",
                        "case": case["title"],
                        "source": case.get("source") or TRACKER_URL,
                        "note": (
                            f"Named plaintiff in {case['title']}"
                            + (f" ({case['court']})" if case["court"] else "")
                            + ". "
                            + (f"Latest: {case['latest']}. " if case["latest"] else "")
                            + "REVIEW: confirm this is the same human (same-name players exist across "
                            "D-I and across sports) before merging."
                        ),
                    }
                )
        for raw, players in ambiguous:
            totals["ambiguous"] += 1
            opts = "; ".join(f"{p['team']} {p['class']} cam {p['cam']}" for p in players)
            print(f"    AMBIGUOUS  {raw:<26} -> {opts}")
        if args.all_names and misses:
            print(f"    unmatched: {', '.join(misses)}")
        totals["unmatched"] += len(misses)
        print()

    if sheet:
        seen = {case_key(c["title"]) for c in cases}
        missing = [r for k, r in sheet.items() if k not in seen]
        if missing:
            print(
                f"{len(missing)} case(s) in the tracker's Class-of-2022 index have NO prose block "
                f"(no plaintiff names to match — cover these from news reports):"
            )
            for r in sorted(missing, key=lambda r: r.get("Case Name", "")):
                print(f"    {r['Case Name']:<28} {r.get('State',''):<15} {sheet_line(r)}")
            print()
    print(
        f"matched {totals['matched']} plaintiff(s) to a {args.year} roster player "
        f"({totals['captured']} already captured, {totals['matched'] - totals['captured']} not); "
        f"{totals['ambiguous']} ambiguous name(s); {totals['unmatched']} name token(s) matched nobody "
        f"(judges, attorneys, other sports, non-D-I — use --all-names to list them)."
    )
    if args.emit:
        args.emit.write_text(json.dumps(emit, indent=2) + "\n")
        print(f"wrote {len(emit)} candidate row(s) to {args.emit} — review before merging into "
              f"data/returns/{args.year}_returns.json")


if __name__ == "__main__":
    main()
