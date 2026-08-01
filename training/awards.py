"""Loader for the consensus All-American / AP POY reference data.

This is the fitness function for CamPom: the metric's job is that the best
player in the country carries the highest score, and awards are the only
non-circular ground truth for that. See the "Validation" section of
`docs/campom_methodology.md`.

Typical use:

    from awards import load_awards, score_metric
    df = load_awards(engine)          # season, torvik_pid, tier
    print(score_metric(pool, "cam_gbpm_v3_psos", df))
"""
from __future__ import annotations

import re
import unicodedata
from pathlib import Path

import numpy as np
import pandas as pd
from sqlalchemy import text

CSV = Path(__file__).resolve().parent.parent / "data" / "awards" / "consensus_all_americans.csv"

# Relevance tiers used for NDCG scoring.
TIER_POY = 3
TIER_FIRST = 2
TIER_SECOND = 1

# Awardees whose `torvik_player_stats` row has a NULL `player_id`, so they
# cannot be reached through `players` at all and no name alias can recover
# them. Mapped straight to `torvik_pid`. Tracked in issue #243 -- once the
# linkage is fixed these should resolve by name and this table can go.
PID_OVERRIDES = {
    (2015, "D'Angelo Russell"): 38468,   # Ohio St., stored as D&#039;Angelo Russell
    (2019, "Ja Morant"): 50678,          # Murray St., NatStat "Temetrius Morant"
    (2020, "Obi Toppin"): 65484,         # Dayton, NatStat "Obadiah Toppin"
    (2022, "Johnny Davis"): 72499,       # Wisconsin, NatStat "Jonathan Davis"
}

_SUFFIX = re.compile(r"\b(jr|sr|ii|iii|iv)\b")
_ENTITY = re.compile(r"&#0?39;")


def normalize_name(name: str) -> str:
    """Fold a name to a join key.

    Mirrors the intent of `normalize_name` in `crates/cstat-ingest/src/ingest/
    torvik.rs` (diacritic folding, punctuation removal) and additionally
    repairs the raw HTML entities present in some `players.name` rows and
    folds German oe/ue/ae transliterations.
    """
    s = _ENTITY.sub("'", str(name))
    s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode()
    s = s.lower().replace(".", " ").replace("'", "").replace("-", " ")
    s = " ".join(_SUFFIX.sub(" ", s).split()).replace(" ", "")
    for a, b in (("oe", "o"), ("ue", "u"), ("ae", "a"), ("ss", "s")):
        s = s.replace(a, b)
    return s


def team_key(name: str) -> str:
    """Fold a school name for comparison across sources ("Ohio State" / "Ohio St.")."""
    s = str(name).lower().replace(".", "").replace("'", "")
    for a, b in ((" state", " st"), (" university", ""), ("&", "and"),
                 ("saint ", "st "), ("connecticut", "uconn")):
        s = s.replace(a, b)
    return " ".join(s.split())


def read_csv() -> pd.DataFrame:
    """Raw award rows: season, player, school, consensus_team, poy, tier, key."""
    df = pd.read_csv(CSV)
    df["poy"] = df.poy.astype(str).str.lower().eq("true")
    df["tier"] = np.where(df.poy, TIER_POY, np.where(df.consensus_team == 1, TIER_FIRST, TIER_SECOND))
    df["key"] = df.player.map(normalize_name)
    df["tkey"] = df.school.map(team_key)
    return df


def load_awards(engine, verbose: bool = True) -> pd.DataFrame:
    """Award rows joined to `torvik_pid`. Returns season, torvik_pid, tier.

    Reports the match rate; anything below ~98% means the linkage regressed
    or a new season needs aliases.
    """
    aw = read_csv()
    with engine.connect() as conn:
        players = pd.read_sql(text("""
            SELECT tps.season, tps.torvik_pid, tps.team_name, p.name
            FROM torvik_player_stats tps
            JOIN players p ON p.id = tps.player_id
            WHERE tps.torvik_pid IS NOT NULL
        """), conn)
    players["key"] = players.name.map(normalize_name)
    players["tkey"] = players.team_name.map(team_key)

    cand = aw.merge(players[["season", "key", "tkey", "torvik_pid"]],
                    on=["season", "key"], how="left", suffixes=("", "_db"))
    # Same normalized name can appear twice in a season (2017 Justin Jackson at
    # both UNC and Maryland; "Braeden Smith" folding onto "Braden Smith"). Keep
    # the candidate whose school agrees; only fall back to a name-only match
    # when the name is unique that season.
    cand["school_ok"] = cand.tkey == cand.tkey_db
    n_cand = cand.groupby(["season", "key"]).torvik_pid.transform("size")
    keep = cand.school_ok | ((n_cand == 1) & cand.torvik_pid.notna())
    resolved = cand[keep].drop_duplicates(["season", "key"])

    merged = aw.merge(resolved[["season", "key", "torvik_pid"]], on=["season", "key"], how="left")
    for (season, player), pid in PID_OVERRIDES.items():          # issue #243
        sel = (merged.season == season) & (merged.player == player)
        merged.loc[sel, "torvik_pid"] = pid

    hit = merged.torvik_pid.notna()
    if verbose:
        print(f"awards matched: {int(hit.sum())}/{len(merged)} ({hit.mean():.1%})")
        for _, r in merged[~hit].iterrows():
            print(f"  UNMATCHED {int(r.season)} {r.player} ({r.school})")
    out = merged.loc[hit, ["season", "torvik_pid", "tier"]].copy()
    out["torvik_pid"] = out.torvik_pid.astype(int)
    # a player can appear once per season; keep the highest honour
    return out.sort_values("tier", ascending=False).drop_duplicates(["season", "torvik_pid"])


def score_metric(pool: pd.DataFrame, column: str, awards: pd.DataFrame, k: int = 25) -> dict:
    """Score a player-rating column against the awards.

    `pool` must carry `season`, `torvik_pid`, and `column`, already filtered to
    the qualified population (the study used GP >= 5 and MPG >= 10). Returns
    NDCG@k plus All-American recall and POY placement.
    """
    df = pool.merge(awards, on=["season", "torvik_pid"], how="left")
    df["tier"] = df.tier.fillna(0.0)
    ndcg, rec10, rec_k, poy1, poy5 = [], [], [], [], []
    for _, g in df.groupby("season"):
        v, t = g[column].to_numpy(), g.tier.to_numpy()
        if np.isnan(v).all() or t.max() == 0:
            continue
        order = np.argsort(-np.nan_to_num(v, nan=-np.inf))
        rank = np.empty(len(v), dtype=float)
        rank[order] = np.arange(1, len(v) + 1)
        top = order[:k]
        gain = ((2 ** t[top] - 1) / np.log2(np.arange(1, len(top) + 1) + 1)).sum()
        ideal_t = np.sort(t)[::-1][:k]
        ideal = ((2 ** ideal_t - 1) / np.log2(np.arange(1, len(ideal_t) + 1) + 1)).sum()
        ndcg.append(gain / ideal if ideal else np.nan)
        aa = rank[t > 0]
        rec10.append((aa <= 10).mean())
        rec_k.append((aa <= k).mean())
        p = rank[t == TIER_POY]
        if len(p):
            poy1.append(p[0] <= 1)
            poy5.append(p[0] <= 5)
    return {"ndcg@%d" % k: float(np.nanmean(ndcg)), "aa_recall@10": float(np.mean(rec10)),
            "aa_recall@%d" % k: float(np.mean(rec_k)), "poy@1": float(np.mean(poy1)),
            "poy@5": float(np.mean(poy5))}


if __name__ == "__main__":
    from db import get_engine

    eng = get_engine()
    aw = load_awards(eng)
    with eng.connect() as c:
        pool = pd.read_sql(text("""
            SELECT season, torvik_pid, gbpm, cam_gbpm_v3_psos
            FROM torvik_player_stats
            WHERE games_played >= 5 AND minutes_per_game >= 10
              AND cam_gbpm_v3_psos IS NOT NULL AND torvik_pid IS NOT NULL
        """), c)
    for col in ("gbpm", "cam_gbpm_v3_psos"):
        print(f"\n{col}:")
        for k, v in score_metric(pool, col, aw).items():
            print(f"  {k:<14} {v:.4f}")
