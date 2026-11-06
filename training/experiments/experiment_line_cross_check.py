"""How often does the betting-line feed name the wrong favourite?

`game_lines` is one sportsbook feed, so its spread and moneyline can be wrong
together. This compares each 2026 spread with the book line on ESPN's game
summary, the source `cstat-ingest espn-lines` now captures, and reports how far
apart the two are when they name opposite favourites. The game's result then
says whether either source was systematically the wrong one.

Games are matched to events by date and final score, which is good enough for a
rate but not exact: the one 2026 gap of 17 points, Lipscomb at West Georgia,
is a score collision with another game, not a line. The served step needs no
such match, because `broadcasts` has already paired every game with its event.

Reads the public summary endpoint, about one request per game (~15 minutes).
Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_line_cross_check.py \
          --out eval_history/line_cross_check_YYYYMMDD_summary.json
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import argparse
import collections
import concurrent.futures as cf
import datetime as dt
import json
import time
import urllib.request

import pandas as pd

from db import get_engine

ESPN = "https://site.api.espn.com/apis/site/v2/sports/basketball/mens-college-basketball/"
THRESHOLDS = [0, 3, 5, 6, 7, 8]

QUERY = """
SELECT g.game_date, g.home_score, g.away_score,
       CASE WHEN l.home_team_id = g.home_team_id THEN l.home_spread
            ELSE -l.home_spread END AS feed
FROM game_lines l
JOIN games g ON g.id = l.game_id
WHERE g.season = %(season)s AND l.home_spread IS NOT NULL AND g.home_score IS NOT NULL
"""


def fetch(url: str):
    for attempt in range(3):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "cstat"})
            return json.load(urllib.request.urlopen(request, timeout=25))
        except OSError:
            time.sleep(1 + attempt)
    return None


def events_by_score(days):
    index = collections.defaultdict(list)

    def board(day):
        url = f"{ESPN}scoreboard?dates={day:%Y%m%d}&groups=50&limit=400"
        return day, (fetch(url) or {}).get("events", [])

    with cf.ThreadPoolExecutor(4) as pool:
        for day, events in pool.map(board, sorted(days)):
            for event in events:
                sides = {c["homeAway"]: c for c in event["competitions"][0]["competitors"]}
                try:
                    home, away = int(sides["home"]["score"]), int(sides["away"]["score"])
                except (KeyError, ValueError):
                    continue
                index[(day, home, away)].append((event["id"], False))
                index[(day, away, home)].append((event["id"], True))
    return index


def second_line(target):
    event, flipped = target
    picks = (fetch(f"{ESPN}summary?event={event}") or {}).get("pickcenter") or []
    if not picks or picks[0].get("spread") is None:
        return None
    magnitude = abs(float(picks[0]["spread"]))
    spread = -magnitude if picks[0]["homeTeamOdds"].get("favorite") else magnitude
    return -spread if flipped else spread


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--season", type=int, default=2026)
    parser.add_argument("--out")
    args = parser.parse_args()

    df = pd.read_sql(QUERY, get_engine(), params={"season": args.season})
    days = set()
    for day in df["game_date"]:
        days |= {day + dt.timedelta(days=d) for d in (-1, 0, 1)}
    index = events_by_score(days)

    def target(row):
        for offset in (0, -1, 1):
            day = row.game_date + dt.timedelta(days=offset)
            found = index.get((day, row.home_score, row.away_score), [])
            if found:
                return found[0] if len(found) == 1 else None
        return None

    df["target"] = [target(r) for r in df.itertuples()]
    df = df[df["target"].notna()].copy()
    with cf.ThreadPoolExecutor(4) as pool:
        df["second"] = list(pool.map(second_line, df["target"]))
    df = df[df["second"].notna()].copy()
    df["second"] = df["second"].astype(float)
    df["gap"] = (df["feed"] - df["second"]).abs()
    df["margin"] = df["home_score"] - df["away_score"]
    opposite = df[df["feed"] * df["second"] < 0]

    summary = {
        "season": args.season,
        "games": int(len(df)),
        "feed_mae": float((df["feed"] + df["margin"]).abs().mean()),
        "second_mae": float((df["second"] + df["margin"]).abs().mean()),
        "opposite_favourites": {
            str(t): {
                "games": int((opposite["gap"] >= t).sum()),
                "feed_favourite_won": int(
                    ((opposite["feed"] < 0) == (opposite["margin"] > 0))[opposite["gap"] >= t].sum()
                ),
            }
            for t in THRESHOLDS
        },
    }
    print(json.dumps(summary, indent=2))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=2)


if __name__ == "__main__":
    main()
