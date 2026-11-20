"""Which Elo should the game models read once NatStat is gone (migration plan §4.4)?

Every served game model (margin / win / total and their point-in-time `pit_*`
twins) reads `diff_elo_rating` (`diff_elo` in the training frame). There that is NatStat's
pre-game Elo: `game_forecasts.home_elo_before` / `away_elo_before` where the
forecast carries it (2015-2025), NatStat's per-game log `team_elo_games`
(`elo_after - elo_change`, the value `pit_team.rs` serves) where it does not,
and `features.compute_elo_series` (in-house, reset to 1500 every season,
negligible home edge) for a team-game neither covers. Leaving NatStat removes
the first two. Candidates:

  cbbd   CollegeBasketballData's per-game Elo (`/games`: `homeTeamEloStart`,
         `awayTeamEloStart`), joined to our games through the ESPN event id
         (CBBD `sourceId` = `game_broadcasts.source_event_id`), and for games
         `broadcasts` never matched by date + both teams + final score, with
         ambiguous candidates dropped. CBBD team ids are mapped to our
         cross-season `teams.natstat_id` by majority vote over the event-id
         matches. An unmatched game keeps NaN (LightGBM routes missing).
  own    A margin-aware Elo over our own `games` table (538-style: K, a
         log-margin multiplier with autocorrelation damping, home edge,
         carry-over regression toward 1500 between seasons, non-D1 opponents
         at a fixed rating). Strictly pre-game. Its three free parameters are
         chosen on Elo-only home-win log-loss over 2016-2020, before the first
         test season, from a small grid.
  no_elo The feature removed: how much the Elo channel is worth at all.

Each arm retrains the served point-in-time fit (`train.fit_final_models`:
chronological 80/20 early stop, then a refit on every training row) on
seasons strictly before the test season, over the same frame rows, the same
folds (`walk_forward.folds`, 2021+) and the same everything else; only the
Elo columns differ. Graded on margin MAE, total MAE and win log-loss per
game, paired against the baseline.

Comparison script only: writes nothing to the database or `training/models/`.
Never calls NatStat. CBBD responses are cached under --cache-dir, so a rerun
costs no calls; `fetch` refuses to spend more than --max-calls.

Run (from `training/`, against the LOCAL database; nothing here reads NatStat):
  1. CBBD cache, 52 calls for 2015-2027 (four windows a season; a window
     that returns the 3000-game page cap is split and refetched):
       ./.venv/bin/python experiments/experiment_elo_source.py fetch \\
           --key-file /path/.cbbd_key [--max-calls 200]
  2. The served point-in-time frame (about 2 hours; the pickle is a dict of
     `df` / `feature_cols` / `sum_cols`, or a frame pickle + `.json` column
     lists as `train_heldout_game_models.py` writes):
       ./.venv/bin/python build_pit_lookup.py --seasons 2015,...,2027 \\
           --output frames/elo_experiment/pit_cam_v3_lookup.csv.gz
       GBPM_VARIANT=pit_cam_v3 PIT_LOOKUP_PATH=frames/elo_experiment/pit_cam_v3_lookup.csv.gz \\
           python -c "features.build_feature_matrix(get_engine(), list(range(2015, 2028)))" -> pickle
  3. The comparison (about 5 minutes):
       ./.venv/bin/python experiments/experiment_elo_source.py run \\
      --frame frames/elo_experiment/pit_frame_2015_2027.pkl \\
      --out eval_history/elo_source_YYYYMMDD_summary.json
"""

from __future__ import annotations

import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), ".."))
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))

import argparse
import contextlib
import io
import json
import math
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from datetime import date, timedelta
from pathlib import Path

import numpy as np
import pandas as pd

CBBD_BASE = "https://api.collegebasketballdata.com"
CBBD_PAGE_CAP = 3000  # observed: /games?season=2026 returned exactly 3000 of ~6,300
DEFAULT_CACHE = Path(__file__).resolve().parent / "cache" / "cbbd"
SEASONS = list(range(2015, 2028))
WALK_FROM = 2021

# Own-Elo grid, tuned on 2016-2020 (2015 is burn-in), before any test fold.
OWN_K = (15.0, 20.0, 30.0, 40.0)
OWN_HFA = (50.0, 75.0, 100.0, 125.0, 150.0)
OWN_CARRY = (0.5, 0.65, 0.75, 0.85, 1.0)
OWN_INIT = 1500.0
OWN_NOND1 = 1200.0  # fixed rating for a non-D1 opponent (NULL team id); never updated
OWN_TUNE = (2016, 2020)
# The frame column (features.py `diff_pairs["elo"]`); served as `diff_elo_rating`.
ELO_COL = "diff_elo"

ARMS = ("natstat", "cbbd", "own", "no_elo")


# ------------------------------------------------------------------ CBBD fetch
def season_windows(season: int) -> list[tuple[str, str]]:
    """Four date windows that cover a season; each well under the 3000 cap.

    `endDateRange` drops the games of its own end date and some of the evening
    before (measured on 2026: every one of the 188 missing games sat on the
    last two days of a window), so consecutive windows overlap by a day and the
    loader dedupes on the CBBD game id.
    """
    y0, y1 = season - 1, season
    return [
        (f"{y0}-10-01", f"{y0}-12-02"),
        (f"{y0}-12-01", f"{y1}-01-17"),
        (f"{y1}-01-16", f"{y1}-03-02"),
        (f"{y1}-03-01", f"{y1}-05-16"),
    ]


def _cache_path(cache: Path, season: int, start: str, end: str) -> Path:
    return cache / f"games_{season}_{start}_{end}.json"


class Budget(Exception):
    pass


def _get(key: str, params: dict) -> tuple[list, int | None]:
    url = f"{CBBD_BASE}/games?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {key}", "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            remaining = r.headers.get("X-CallLimit-Remaining")
            return json.loads(r.read()), (int(remaining) if remaining is not None else None)
    except urllib.error.HTTPError as e:
        if e.code == 429:
            raise Budget("CBBD 429: monthly quota exhausted") from e
        raise


def fetch(cache: Path, key_file: Path, seasons: list[int], max_calls: int) -> None:
    cache.mkdir(parents=True, exist_ok=True)
    key = key_file.read_text().strip()
    calls = 0
    remaining = None

    def window(season: int, start: str, end: str) -> None:
        nonlocal calls, remaining
        path = _cache_path(cache, season, start, end)
        if path.exists():
            rows = json.loads(path.read_text())
        else:
            if calls >= max_calls:
                raise Budget(f"call budget {max_calls} reached")
            rows, remaining = _get(key, {"season": season, "startDateRange": start, "endDateRange": end})
            calls += 1
            path.write_text(json.dumps(rows))
            print(f"  {season} {start}..{end}: {len(rows)} games (remaining {remaining})", flush=True)
            time.sleep(0.5)
        if len(rows) >= CBBD_PAGE_CAP:
            # Truncated: split the window and drop the truncated response.
            d0, d1 = date.fromisoformat(start), date.fromisoformat(end)
            mid = d0 + (d1 - d0) / 2
            # Overlap by a day, like season_windows: the end date is not covered.
            path.rename(path.with_suffix(".truncated"))
            window(season, start, (mid + timedelta(days=1)).isoformat())
            window(season, mid.isoformat(), end)

    try:
        for s in seasons:
            for start, end in season_windows(s):
                window(s, start, end)
    finally:
        print(f"CBBD calls this run: {calls}; last X-CallLimit-Remaining: {remaining}")


def load_cbbd(cache: Path) -> pd.DataFrame:
    rows = []
    for p in sorted(cache.glob("games_*.json")):
        rows.extend(json.loads(p.read_text()))
    if not rows:
        raise SystemExit(f"no CBBD cache under {cache}; run `fetch` first")
    df = pd.DataFrame(rows).drop_duplicates("id")
    df["start_utc"] = pd.to_datetime(df["startDate"], utc=True)
    df["utc_date"] = df["start_utc"].dt.date
    keep = [
        "id", "sourceId", "season", "start_utc", "utc_date", "status", "neutralSite",
        "homeTeamId", "homeTeam", "homePoints", "homeTeamEloStart", "homeTeamEloEnd",
        "awayTeamId", "awayTeam", "awayPoints", "awayTeamEloStart", "awayTeamEloEnd",
    ]
    return df[keep].reset_index(drop=True)


def cbbd_elo_audit(cb: pd.DataFrame) -> dict:
    """Is EloStart really pre-game? Chain check: a team's EloStart for a game
    equals its EloEnd from its previous game. Plus the season-start level."""
    long = pd.concat([
        cb[["id", "season", "start_utc", "homeTeamId", "homeTeamEloStart", "homeTeamEloEnd"]].set_axis(
            ["id", "season", "t", "team", "start", "end"], axis=1),
        cb[["id", "season", "start_utc", "awayTeamId", "awayTeamEloStart", "awayTeamEloEnd"]].set_axis(
            ["id", "season", "t", "team", "start", "end"], axis=1),
    ]).dropna(subset=["start"]).sort_values(["team", "t", "id"])
    prev_end = long.groupby(["team", "season"])["end"].shift()
    chained = (long["start"] - prev_end).abs() < 0.5
    has_prev = prev_end.notna()
    first = long.groupby(["team", "season"]).head(1)
    per_season = {}
    for s, g in first.groupby("season"):
        per_season[int(s)] = {
            "teams": int(len(g)),
            "first_game_elo_eq_1500": float((g["start"] == 1500).mean()),
            "first_game_elo_sd": float(g["start"].std()),
        }
    # Carry-over: last end of season S vs first start of S+1 for the same team.
    last = long.groupby(["team", "season"]).tail(1).set_index(["team", "season"])["end"]
    firsts = first.set_index(["team", "season"])["start"]
    pairs = [(last[(t, s - 1)], v) for (t, s), v in firsts.items() if (t, s - 1) in last.index]
    carry = None
    if len(pairs) > 50:
        a = np.array(pairs, dtype=float)
        slope = float(np.polyfit(a[:, 0] - 1500, a[:, 1] - 1500, 1)[0])
        carry = {"pairs": len(pairs), "slope_first_on_prev_last": slope}
    return {
        "team_games_with_elo": int(len(long)),
        "chain_start_equals_prev_end": float(chained[has_prev].mean()),
        "season_start": per_season,
        "carry_over": carry,
        "elo_sd": float(long["start"].std()),
    }


# ------------------------------------------------------------------ our games
def load_our_games(engine) -> pd.DataFrame:
    """Our finished games, oldest first, with each side's cross-season key.

    A non-D1 side's key is `None`. pandas 3 reads a NULL text column as NaN,
    and the first run of this script (elo_source_20261120) took that NaN for a
    team: `own_elo`'s `is None` checks missed it, so every non-D1 opponent
    shared one rating that started at 1500 and moved, instead of the fixed
    OWN_NOND1. Normalized here so every reader sees `None`.
    """
    df = pd.read_sql(
        """
        SELECT g.id::text AS game_id, g.season, g.game_date, g.is_neutral_site,
               g.home_team_id::text AS home_team_id, g.away_team_id::text AS away_team_id,
               ht.natstat_id AS home_ns, at.natstat_id AS away_ns,
               g.home_score, g.away_score, gb.source_event_id
        FROM games g
        LEFT JOIN teams ht ON ht.id = g.home_team_id
        LEFT JOIN teams at ON at.id = g.away_team_id
        LEFT JOIN game_broadcasts gb ON gb.game_id = g.id
        WHERE g.home_score IS NOT NULL AND g.away_score IS NOT NULL
        ORDER BY g.game_date, g.id
        """,
        engine,
    )
    for col in ("home_ns", "away_ns", "home_team_id", "away_team_id", "source_event_id"):
        df[col] = df[col].astype(object).where(df[col].notna(), None)
    return df


def join_cbbd(ours: pd.DataFrame, cb: pd.DataFrame) -> tuple[pd.DataFrame, dict, dict]:
    """Per our game: CBBD home/away pre-game Elo in OUR orientation, plus how it matched.

    Pass 1: ESPN event id. Orientation from the final score (basketball has no
    ties): same pair -> same, swapped -> flipped, else the match is rejected.
    Pass 2 (games pass 1 left): CBBD UTC date is our date or the next day (a
    late Eastern tip is tomorrow in UTC), both teams through the team map, and
    the same final score; more than one candidate -> ambiguous, dropped.
    """
    cb = cb[cb["homePoints"].notna() & cb["awayPoints"].notna()].copy()
    cb["sourceId"] = cb["sourceId"].astype(str)
    by_event = cb.drop_duplicates("sourceId", keep=False).set_index("sourceId")
    out = {}
    how = {}
    used = set()

    def orient(row, c) -> tuple[float, float] | None:
        hs, as_ = int(row.home_score), int(row.away_score)
        if (int(c["homePoints"]), int(c["awayPoints"])) == (hs, as_):
            return c["homeTeamEloStart"], c["awayTeamEloStart"]
        if (int(c["awayPoints"]), int(c["homePoints"])) == (hs, as_):
            return c["awayTeamEloStart"], c["homeTeamEloStart"]
        return None

    votes: dict[int, Counter] = defaultdict(Counter)
    for row in ours.itertuples(index=False):
        ev = row.source_event_id
        if ev is None or ev not in by_event.index:
            continue
        c = by_event.loc[ev]
        o = orient(row, c)
        if o is None:
            how[row.game_id] = "event_score_mismatch"
            continue
        out[row.game_id] = o
        how[row.game_id] = "event_id"
        used.add(int(c["id"]))
        same = (int(c["homePoints"]), int(c["awayPoints"])) == (int(row.home_score), int(row.away_score))
        h_ns, a_ns = (row.home_ns, row.away_ns) if same else (row.away_ns, row.home_ns)
        if h_ns is not None:
            votes[int(c["homeTeamId"])][h_ns] += 1
        if a_ns is not None:
            votes[int(c["awayTeamId"])][a_ns] += 1

    team_map = {}
    for cid, cnt in votes.items():
        ns, n = cnt.most_common(1)[0]
        if n >= 3 and n / sum(cnt.values()) >= 0.8:
            team_map[cid] = ns
    # one natstat id claimed by two CBBD teams -> drop both
    claimed = Counter(team_map.values())
    team_map = {k: v for k, v in team_map.items() if claimed[v] == 1}

    rest = cb[~cb["id"].isin(used)].copy()
    rest["h_ns"] = rest["homeTeamId"].map(team_map)
    rest["a_ns"] = rest["awayTeamId"].map(team_map)
    rest = rest.dropna(subset=["h_ns", "a_ns"])
    index = defaultdict(list)
    for c in rest.to_dict("records"):
        index[(c["utc_date"], frozenset((c["h_ns"], c["a_ns"])))].append(c)
    for row in ours.itertuples(index=False):
        if row.game_id in out or row.home_ns is None or row.away_ns is None:
            continue
        teams = frozenset((row.home_ns, row.away_ns))
        cands = []
        for d in (row.game_date, row.game_date + timedelta(days=1)):
            for c in index.get((d, teams), []):
                if orient(row, c) is not None and c["id"] not in used:
                    cands.append(c)
        if len(cands) == 1:
            c = cands[0]
            out[row.game_id] = orient(row, c)
            how[row.game_id] = "date_teams_score"
            used.add(c["id"])
        elif len(cands) > 1:
            how[row.game_id] = "ambiguous"

    elo = pd.DataFrame(
        [(g, h, a) for g, (h, a) in out.items()], columns=["game_id", "home_elo_cbbd", "away_elo_cbbd"]
    )
    ours = ours.assign(match=ours["game_id"].map(how).fillna("unmatched"))
    coverage = {}
    for s, g in ours.groupby("season"):
        d1 = g["home_ns"].notna() & g["away_ns"].notna()
        cnt = g.loc[d1, "match"].value_counts().to_dict()
        n = int(d1.sum())
        matched = cnt.get("event_id", 0) + cnt.get("date_teams_score", 0)
        coverage[int(s)] = {
            "d1_vs_d1_final_games": n,
            "matched": int(matched),
            "rate": matched / n if n else None,
            **{k: int(v) for k, v in cnt.items()},
        }
    return elo, coverage, {"cbbd_teams_mapped": len(team_map), "cbbd_teams_seen": len(votes)}


# ------------------------------------------------------------------ own Elo
def own_elo(ours: pd.DataFrame, k: float, hfa: float, carry: float) -> tuple[pd.DataFrame, np.ndarray]:
    """Pre-game ratings for every game, keyed by cross-season team natstat_id.

    538-style update: delta = K * mult * (actual - expected), mult =
    ln(|margin| + 1) * 2.2 / (0.001 * winner_elo_edge + 2.2). At a team's first
    game of a season its rating regresses: r <- 1500 + carry * (r - 1500).
    A non-D1 side (NULL team) is OWN_NOND1 and never updated. Returns the frame
    and the Elo-only home-win probability per game (for tuning).
    """
    rating: dict = {}
    seen_season: dict = {}
    hs = np.empty(len(ours))
    as_ = np.empty(len(ours))
    p = np.empty(len(ours))
    for i, r in enumerate(ours.itertuples(index=False)):
        sides = []
        for ns in (r.home_ns, r.away_ns):
            if ns is None:
                sides.append(OWN_NOND1)
                continue
            if seen_season.get(ns) != r.season:
                if ns in rating:
                    rating[ns] = OWN_INIT + carry * (rating[ns] - OWN_INIT)
                seen_season[ns] = r.season
            sides.append(rating.get(ns, OWN_INIT))
        h, a = sides
        hs[i], as_[i] = h, a
        edge = 0.0 if r.is_neutral_site else hfa
        exp_home = 1.0 / (1.0 + 10.0 ** ((a - h - edge) / 400.0))
        p[i] = exp_home
        margin = int(r.home_score) - int(r.away_score)
        win = 1.0 if margin > 0 else 0.0
        winner_edge = (h + edge - a) if margin > 0 else (a - h - edge)
        mult = math.log(abs(margin) + 1) * 2.2 / (winner_edge * 0.001 + 2.2)
        delta = k * mult * (win - exp_home)
        if r.home_ns is not None:
            rating[r.home_ns] = h + delta
        if r.away_ns is not None:
            rating[r.away_ns] = a - delta
    return ours.assign(home_elo_own=hs, away_elo_own=as_), p


def tune_own(ours: pd.DataFrame) -> tuple[dict, list[dict]]:
    sel = ours["season"].between(*OWN_TUNE).to_numpy()
    y = (ours["home_score"] > ours["away_score"]).to_numpy(float)[sel]
    grid = []
    for k in OWN_K:
        for hfa in OWN_HFA:
            for carry in OWN_CARRY:
                _, p = own_elo(ours, k, hfa, carry)
                q = np.clip(p[sel], 1e-6, 1 - 1e-6)
                ll = float(-np.mean(y * np.log(q) + (1 - y) * np.log(1 - q)))
                grid.append({"k": k, "hfa": hfa, "carry": carry, "logloss_2016_2020": ll})
    best = min(grid, key=lambda g: g["logloss_2016_2020"])
    return best, grid


# ------------------------------------------------------------------ frame
def load_frame(path: str) -> tuple[pd.DataFrame, list[str], list[str]]:
    obj = pd.read_pickle(path)
    if isinstance(obj, dict):
        return obj["df"], obj["feature_cols"], obj["sum_cols"]
    cols, sums = json.load(open(path + ".json"))
    return obj, cols, sums


def natstat_source(engine, frame: pd.DataFrame) -> dict:
    """Where the baseline frame's Elo came from, per season (the three-tier merge in features.py)."""
    fc = pd.read_sql(
        "SELECT game_id::text, home_team_id::text, away_team_id::text FROM game_forecasts "
        "WHERE home_elo_before IS NOT NULL", engine,
    )
    log = pd.read_sql(
        """
        SELECT g.id::text AS game_id, t.id::text AS team_id
        FROM team_elo_games e
        JOIN games g ON g.natstat_id = e.game_code
        JOIN teams t ON t.natstat_id = e.team_code AND t.season = g.season
        WHERE t.id IN (g.home_team_id, g.away_team_id)
        """, engine,
    )
    fc_keys = set(zip(fc["game_id"], fc["home_team_id"])) | set(zip(fc["game_id"], fc["away_team_id"]))
    log_keys = set(zip(log["game_id"], log["team_id"]))
    out = {}
    for s, g in frame.groupby("season"):
        keys = list(zip(g["game_id"].astype(str), g["home_team_id"].astype(str))) + list(
            zip(g["game_id"].astype(str), g["away_team_id"].astype(str)))
        n = len(keys)
        f = sum(k in fc_keys for k in keys)
        lg = sum((k not in fc_keys) and (k in log_keys) for k in keys)
        out[int(s)] = {"team_games": n, "game_forecasts": f / n, "team_elo_games": lg / n,
                       "in_house_fallback": (n - f - lg) / n}
    return out


# ------------------------------------------------------------------ walk-forward
def run_arms(df: pd.DataFrame, feature_cols: list[str], total_cols: list[str], last: int) -> dict:
    from train import fit_final_models
    from walk_forward import folds

    arms = {}
    for arm in ARMS:
        a = df.copy()
        if arm == "cbbd":
            a[ELO_COL] = a["home_elo_cbbd"] - a["away_elo_cbbd"]
        elif arm == "own":
            a[ELO_COL] = a["home_elo_own"] - a["away_elo_own"]
        fc = [c for c in feature_cols if not (arm == "no_elo" and c == ELO_COL)]
        tc = [c for c in total_cols if not (arm == "no_elo" and c == ELO_COL)]
        arms[arm] = (a, fc, tc)

    seasons = df["season"]
    preds = {arm: {"margin": pd.Series(np.nan, index=df.index), "win": pd.Series(np.nan, index=df.index),
                   "total": pd.Series(np.nan, index=df.index)} for arm in ARMS}
    for s, train, test in folds(seasons[seasons <= last], WALK_FROM):
        train = train.reindex(df.index, fill_value=False)
        test = test.reindex(df.index, fill_value=False)
        for arm, (a, fc, tc) in arms.items():
            t0 = time.time()
            with contextlib.redirect_stdout(io.StringIO()):
                fit = fit_final_models(a[train].reset_index(drop=True), fc, tc)
            te = a[test]
            preds[arm]["margin"][test] = fit["margin"].predict(te[fc])
            preds[arm]["win"][test] = fit["win"].predict_proba(te[fc])[:, 1]
            preds[arm]["total"][test] = fit["total"].predict(te[tc])
            m = np.abs(preds[arm]["margin"][test] - te["margin"]).mean()
            print(f"  {s} {arm:7s} margin MAE {m:.3f}  ({time.time() - t0:.0f}s)", flush=True)
    return preds


def losses(df: pd.DataFrame, p: dict) -> dict[str, pd.Series]:
    q = p["win"].clip(1e-6, 1 - 1e-6)
    y = df["home_win"].astype(float)
    return {
        "margin_mae": (p["margin"] - df["margin"]).abs(),
        "total_mae": (p["total"] - df["total"]).abs(),
        "win_logloss": -(y * np.log(q) + (1 - y) * np.log(1 - q)),
    }


def grade(df: pd.DataFrame, preds: dict, mask: pd.Series) -> dict:
    scored = mask & preds["natstat"]["margin"].notna()
    d = df[scored]
    L = {arm: {k: v[scored] for k, v in losses(d, {kk: vv[scored] for kk, vv in preds[arm].items()}).items()}
         for arm in ARMS}
    out = {"n_games": int(scored.sum()), "pooled": {}, "per_season": {}, "vs_natstat": {}}
    for arm in ARMS:
        out["pooled"][arm] = {k: float(v.mean()) for k, v in L[arm].items()}
    for s in sorted(d["season"].unique()):
        sm = d["season"] == s
        out["per_season"][int(s)] = {
            "n": int(sm.sum()),
            **{arm: {k: float(v[sm].mean()) for k, v in L[arm].items()} for arm in ARMS},
        }
    for arm in ARMS:
        if arm == "natstat":
            continue
        out["vs_natstat"][arm] = {}
        for metric in L[arm]:
            diff = L[arm][metric] - L["natstat"][metric]
            z = float(diff.mean() / (diff.std() / math.sqrt(len(diff))))
            per = [float(diff[d["season"] == s].mean()) for s in sorted(d["season"].unique())]
            out["vs_natstat"][arm][metric] = {
                "diff": float(diff.mean()), "z": z,
                "folds_better": int(sum(x < 0 for x in per)), "folds": len(per),
                "per_season": dict(zip(map(int, sorted(d["season"].unique())), per)),
            }
    # Early season (Nov-Dec) is where an Elo's season-start rule shows.
    month = pd.to_datetime(d["game_date"]).dt.month
    phases = {"nov_dec": month.isin([11, 12]), "jan_on": ~month.isin([11, 12])}
    out["by_phase"] = {}
    for ph, pm in phases.items():
        out["by_phase"][ph] = {"n": int(pm.sum())}
        for arm in ARMS:
            if arm == "natstat":
                continue
            row = {}
            for metric in L[arm]:
                diff = (L[arm][metric] - L["natstat"][metric])[pm]
                row[metric] = {"diff": float(diff.mean()), "z": float(diff.mean() / (diff.std() / math.sqrt(len(diff))))}
            out["by_phase"][ph][arm] = row
    return out


def elo_only_logistic(df: pd.DataFrame, last: int) -> dict:
    """Each source on its own: logistic(home_win ~ Elo diff + venue), fitted on
    2016-2020, home-win log-loss per test season on games every source covers.
    A strength check of the raw signal, independent of the tree."""
    from sklearn.linear_model import LogisticRegression

    diffs = {
        "natstat": df[ELO_COL],
        "cbbd": df["home_elo_cbbd"] - df["away_elo_cbbd"],
        "own": df["home_elo_own"] - df["away_elo_own"],
    }
    common = diffs["cbbd"].notna()
    y = df["home_win"].astype(int)
    out = {}
    for name, d in diffs.items():
        X = np.column_stack([d.fillna(0) / 100.0, df["venue"]])
        tr = common & df["season"].between(2016, 2020)
        m = LogisticRegression().fit(X[tr], y[tr])
        p = np.clip(m.predict_proba(X)[:, 1], 1e-6, 1 - 1e-6)
        ll = -(y * np.log(p) + (1 - y) * np.log(1 - p))
        out[name] = {int(s): float(ll[common & (df["season"] == s)].mean()) for s in range(WALK_FROM, last + 1)}
        te = common & df["season"].between(WALK_FROM, last)
        out[name]["pooled"] = float(ll[te].mean())
    return out


def run(args) -> None:
    from db import get_engine
    from features import completeness_subset

    engine = get_engine()
    df, feature_cols, sum_cols = load_frame(args.frame)
    total_cols = feature_cols + sum_cols
    before = len(df)
    df = df.dropna(subset=completeness_subset(total_cols)).reset_index(drop=True)
    df["game_id"] = df["game_id"].astype(str)
    print(f"frame: {before} games, {len(df)} feature-complete")
    if not np.allclose(df[ELO_COL], df["home_elo"] - df["away_elo"]):
        raise SystemExit("frame diff_elo does not rebuild from home_elo - away_elo")

    ours = load_our_games(engine)
    cb = load_cbbd(Path(args.cache_dir))
    audit = cbbd_elo_audit(cb)
    print("CBBD audit:", json.dumps(audit)[:600])
    cb_elo, coverage, mapping = join_cbbd(ours, cb)
    for s, c in coverage.items():
        print(f"  join {s}: {c}")

    best, grid = tune_own(ours)
    print("own Elo params:", best)
    own, _ = own_elo(ours, best["k"], best["hfa"], best["carry"])

    df = df.merge(cb_elo, on="game_id", how="left").merge(
        own[["game_id", "home_elo_own", "away_elo_own"]], on="game_id", how="left")
    if df["home_elo_own"].isna().any():
        raise SystemExit("own Elo missing for a frame game")
    frame_cov = {int(s): float(g["home_elo_cbbd"].notna().mean()) for s, g in df.groupby("season")}
    print("CBBD coverage of frame rows:", frame_cov)
    corr = {int(s): {"natstat_vs_cbbd": float(g[ELO_COL].corr(g["home_elo_cbbd"] - g["away_elo_cbbd"])),
                     "natstat_vs_own": float(g[ELO_COL].corr(g["home_elo_own"] - g["away_elo_own"]))}
            for s, g in df.groupby("season")}
    elo_only = elo_only_logistic(df, args.last)
    print("Elo-only logistic log-loss:", json.dumps(elo_only))

    preds = run_arms(df, feature_cols, total_cols, args.last)
    tested = df["season"].between(WALK_FROM, args.last)
    full = grade(df, preds, tested)
    matched = grade(df, preds, tested & df["home_elo_cbbd"].notna())
    for name, g in (("all frame games", full), ("CBBD-matched games", matched)):
        print(f"\n== {name} (n={g['n_games']})")
        for arm in ARMS:
            print(f"  {arm:7s} " + "  ".join(f"{k} {v:.4f}" for k, v in g["pooled"][arm].items()))
        for arm, mets in g["vs_natstat"].items():
            print(f"  {arm:7s} - natstat: " + "  ".join(
                f"{k} {v['diff']:+.4f} (z {v['z']:+.2f}, {v['folds_better']}/{v['folds']})" for k, v in mets.items()))
        for ph, block in g["by_phase"].items():
            for arm in ARMS[1:]:
                print(f"  [{ph} n={block['n']}] {arm:7s} - natstat: " + "  ".join(
                    f"{k} {v['diff']:+.4f} (z {v['z']:+.2f})" for k, v in block[arm].items()))

    summary = {
        "question": "Replace NatStat Elo in the pit game models with CBBD Elo or an in-house Elo? (migration plan 4.4)",
        "judge": f"walk-forward via walk_forward.folds, test seasons {WALK_FROM}-{args.last}, train strictly earlier; "
                 "served pit fit (train.fit_final_models) per arm; frame GBPM_VARIANT=pit_cam_v3, ROSTER_RULE=season",
        "frame": {"path": args.frame, "games": before, "feature_complete": len(df)},
        "baseline_elo_source_share": natstat_source(engine, df),
        "cbbd": {"cache_dir": str(args.cache_dir), "audit": audit, "join_coverage": coverage,
                 "team_mapping": mapping, "frame_row_coverage": frame_cov},
        "own_elo": {"params": best, "grid": grid, "init": OWN_INIT, "non_d1_rating": OWN_NOND1,
                    "tuned_on": list(OWN_TUNE)},
        "diff_correlation_with_natstat": corr,
        "elo_only_logistic": elo_only,
        "all_frame_games": full,
        "cbbd_matched_games": matched,
    }
    if args.out:
        Path(args.out).write_text(json.dumps(summary, indent=1, default=str))
        print("wrote", args.out)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch", help="cache CBBD /games for every season (costs calls once)")
    f.add_argument("--key-file", type=Path, required=True)
    f.add_argument("--cache-dir", type=Path, default=DEFAULT_CACHE)
    f.add_argument("--seasons", default=",".join(map(str, SEASONS)))
    f.add_argument("--max-calls", type=int, default=200)
    r = sub.add_parser("run", help="join, build arms, walk forward (no network)")
    r.add_argument("--frame", required=True, help="pit frame pickle from features.build_feature_matrix")
    r.add_argument("--cache-dir", type=Path, default=DEFAULT_CACHE)
    r.add_argument("--last", type=int, default=2026, help="last test season (2027 is partial)")
    r.add_argument("--out")
    args = ap.parse_args()
    if args.cmd == "fetch":
        fetch(args.cache_dir, args.key_file, [int(s) for s in args.seasons.split(",")], args.max_calls)
    else:
        run(args)


if __name__ == "__main__":
    main()
