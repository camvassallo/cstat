"""How should the preseason projection enter a player's early-season CAM?

CAM's `GP / (GP + 8)` term already blends: it is `(GP * rate + 8 * 0) / (GP + 8)`,
a blend toward zero. After one game that multiplies every player by 0.11, so
the best player in the country reads like a replacement and nothing reaches a
tier. The team ratings answered the same question with a prior worth K games
(`experiment_rankings_preseason_prior.py`); this asks it for players.

Every candidate is rebuilt as of team game T from `torvik_player_game_stats`,
using only games up to T, and scored against what the player did AFTER T. The
team-game clock is what a reader sees: everyone on a team moves together, and a
player who sat out still has a number.

    today        rate * GP/(GP+8), the served number
    observed     rate * 0.8: the full-season weight, no shrinkage at all
    prior        the projection alone
    blend_gp_K   w*prior + (1-w)*rate*0.8, w = K/(K+GP)
    blend_tg_K   the same with w = K/(K+T), T the team's games
    fade_gp      today + max(0, 1 - g(GP)/0.8) * prior, g(x) = x/(x+8)
    fade_tg      the same on the team's games: today plus a prior that reaches
                 zero at a full season, so a finished season is untouched
    taper_K      blend_gp with w rescaled to reach zero at a full season:
                 (w(GP) - w(32)) / (1 - w(32)), floored at zero

`rate` is the no-SOS CamPom composite, as in `compute_campom_at.py`; 0.8 is
g(32), the weight of a full season, which is what puts the observed number on
the scale the projection and the tiers live on. The target is the same composite
over the player's remaining games, at that same weight.

K is chosen per fold by training-season MAE. Walk-forward (#361): train on
seasons < S, score S, S = 2021..2026.

The prior is the held-out `trajectory_oof_predictions` for returners and
`freshman_oof_predictions` for freshmen. Those are leave-one-out rather than
strictly earlier-season, which #361 measured at about zero optimism. Players
with neither are excluded and counted.

Survivorship caveat: the target needs remaining games, so the cohort is players
with at least MIN_REST games and MIN_REST_MIN_PCT of minutes after T. A player
who stops playing is not scored.

Comparison script only; writes nothing to the database or the models.

Run:  cd training && ./.venv/bin/python experiments/experiment_cam_preseason_prior.py
"""

from __future__ import annotations

# Path shim: this lives in training/experiments/ and imports the trainers and
# shared libs from training/ (#364). Same convention as training/validation/.
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__))))

import argparse
import datetime as dt
import json
from pathlib import Path

import numpy as np
import pandas as pd
from scipy.stats import spearmanr
from sqlalchemy import text

from compute_cae import EVAL_DIR
from db import get_engine
from walk_forward import WALK_FROM, folds

# Mirrors crates/cstat-core/src/compute.rs CAMPOM_*.
OFFENSE_EXPONENT = 0.7
DEFENSE_DISCOUNT = 0.1
USG_REF = 17.873_577_08
MINUTES_EXPONENT = 0.5
GP_K = 8.0

FULL_SEASON_GAMES = 32
K_GRID = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 30, 40, 50, 60, 80, 100, 120, 160, 200, 300]
CHECKPOINTS = [1, 2, 3, 5, 8, 10, 15, 20, 25]
MIN_REST = 5
MIN_REST_MIN_PCT = 25.0
FIRST_SEASON = 2016
LAST_SEASON = 2026
TOP_N = 50

GAME_QUERY = text(
    """
    SELECT pid, team, game_uid, game_date, obpm, dbpm, usage, minutes_pct, possessions
      FROM torvik_player_game_stats
     WHERE season = :season
       AND obpm IS NOT NULL AND dbpm IS NOT NULL
       AND usage IS NOT NULL AND minutes_pct IS NOT NULL
    """
)
PRIOR_QUERY = text(
    """
    SELECT torvik_pid AS pid, mean AS prior, 'returner' AS kind
      FROM trajectory_oof_predictions WHERE target_season = :season
    UNION ALL
    SELECT t.torvik_pid AS pid, f.mean AS prior, 'freshman' AS kind
      FROM freshman_oof_predictions f
      JOIN torvik_player_stats t
        ON t.player_id = f.cstat_player_id AND t.season = f.target_season
     WHERE f.target_season = :season AND t.torvik_pid IS NOT NULL
    """
)


def gp_weight(games):
    return games / (games + GP_K)


FULL_WEIGHT = gp_weight(FULL_SEASON_GAMES)


def composite(rows: pd.DataFrame, mean_min_pct: float) -> pd.DataFrame:
    """The no-SOS CamPom rate per player over `rows`, before the GP weight."""
    r = rows.assign(
        o_w=rows.obpm * rows.possessions,
        d_w=rows.dbpm * rows.possessions,
        u_w=rows.usage * rows.minutes_pct,
    )
    g = r.groupby("pid", sort=True).agg(
        gp=("game_uid", "count"),
        poss=("possessions", "sum"),
        o_w=("o_w", "sum"),
        d_w=("d_w", "sum"),
        u_w=("u_w", "sum"),
        min_sum=("minutes_pct", "sum"),
        min_pct=("minutes_pct", "mean"),
    )
    g = g[(g.poss > 0) & (g.min_sum > 0)]
    usg = (g.u_w / g.min_sum) / USG_REF
    adj = (g.o_w / g.poss) * usg.clip(lower=0) ** OFFENSE_EXPONENT + (g.d_w / g.poss) * (
        1.0 - DEFENSE_DISCOUNT * usg
    )
    g["rate"] = adj * (g.min_pct.clip(lower=0) / mean_min_pct) ** MINUTES_EXPONENT
    return g[["gp", "min_pct", "rate"]]


def season_frame(engine, season: int) -> tuple[pd.DataFrame, dict]:
    with engine.connect() as conn:
        games = pd.read_sql(GAME_QUERY, conn, params={"season": season})
        priors = pd.read_sql(PRIOR_QUERY, conn, params={"season": season})
    for col in ("obpm", "dbpm", "usage", "minutes_pct", "possessions"):
        games[col] = games[col].astype(float).fillna(0.0)
    priors["prior"] = priors["prior"].astype(float)
    priors = priors.drop_duplicates("pid", keep="first").set_index("pid")

    # A team's game number, from the distinct games it appears in.
    order = games[["team", "game_uid", "game_date"]].drop_duplicates(["team", "game_uid"])
    order = order.sort_values(["team", "game_date", "game_uid"])
    order["team_game"] = order.groupby("team").cumcount() + 1
    games = games.merge(order[["team", "game_uid", "team_game"]], on=["team", "game_uid"])
    mean_min_pct = games.groupby("pid").minutes_pct.mean().mean()

    frames, unmatched, bare = [], 0, []
    for t in CHECKPOINTS:
        early = composite(games[games.team_game <= t], mean_min_pct)
        rest = composite(games[games.team_game > t], mean_min_pct)
        rest = rest[(rest.gp >= MIN_REST) & (rest.min_pct >= MIN_REST_MIN_PCT)]
        both = early.join(rest, rsuffix="_rest", how="inner")
        with_prior = both.join(priors, how="inner")
        unmatched += len(both) - len(with_prior)
        bare.append(both[~both.index.isin(priors.index)].reset_index().assign(season=season, t=t))
        frames.append(with_prior.reset_index().assign(season=season, t=t))
    return pd.concat(frames, ignore_index=True), pd.concat(bare, ignore_index=True)


def taper_weight(k, games):
    w, end = k / (k + games), k / (k + FULL_SEASON_GAMES)
    return np.maximum(0.0, (w - end) / (1.0 - end))


def candidates(df: pd.DataFrame, k_gp: float, k_tg: float, k_tp: float) -> dict[str, np.ndarray]:
    obs = df.rate.values * FULL_WEIGHT
    today = df.rate.values * gp_weight(df.gp.values)
    prior = df.prior.values
    gp, tg = df.gp.values, df.t.values
    w_gp, w_tg = k_gp / (k_gp + gp), k_tg / (k_tg + tg)
    w_tp = taper_weight(k_tp, gp)
    return {
        "today": today,
        "observed": obs,
        "prior": prior,
        "blend_gp": w_gp * prior + (1 - w_gp) * obs,
        "blend_tg": w_tg * prior + (1 - w_tg) * obs,
        "taper": w_tp * prior + (1 - w_tp) * obs,
        "fade_gp": today + np.maximum(0, 1 - gp_weight(gp) / FULL_WEIGHT) * prior,
        "fade_tg": today + np.maximum(0, 1 - gp_weight(tg) / FULL_WEIGHT) * prior,
    }


def fit_k(train: pd.DataFrame, which: str, weight=lambda k, g: k / (k + g)) -> float:
    y = train.rate_rest.values * FULL_WEIGHT
    obs = train.rate.values * FULL_WEIGHT
    games = train[which].values
    prior = train.prior.values

    def mae(k):
        w = weight(k, games)
        return np.abs(w * prior + (1 - w) * obs - y).mean()

    return float(min(K_GRID, key=mae))


def paired_z(err_a: np.ndarray, err_b: np.ndarray) -> float:
    d = err_a - err_b
    return float(d.mean() / (d.std(ddof=1) / np.sqrt(len(d))))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    engine = get_engine()
    parts, bares = [], []
    for season in range(FIRST_SEASON, LAST_SEASON + 1):
        f, bare = season_frame(engine, season)
        parts.append(f)
        bares.append(bare)
    df = pd.concat(parts, ignore_index=True)
    no_prior = pd.concat(bares, ignore_index=True)
    unmatched = len(no_prior)
    print(f"{len(df):,} player-checkpoints, {unmatched:,} excluded for having no prior")

    scored, chosen = [], []
    for season, train, test in folds(df.season, WALK_FROM):
        k_gp, k_tg = fit_k(df[train], "gp"), fit_k(df[train], "t")
        k_tp = fit_k(df[train], "gp", taper_weight)
        chosen.append({"season": season, "k_gp": k_gp, "k_tg": k_tg, "k_taper": k_tp})
        block = df[test].copy()
        for name, pred in candidates(block, k_gp, k_tg, k_tp).items():
            block[name] = pred
        block["y"] = block.rate_rest * FULL_WEIGHT
        scored.append(block)
    res = pd.concat(scored, ignore_index=True)
    print("chosen K:", chosen)

    names = ["today", "observed", "prior", "blend_gp", "blend_tg", "taper", "fade_gp", "fade_tg"]
    summary = {"chosen": chosen, "unmatched": unmatched, "rows": len(df), "by_checkpoint": {}}

    def report(mask: pd.Series, label: str) -> dict:
        sub = res[mask]
        out = {}
        for name in names:
            err = np.abs(sub[name] - sub.y).values
            rhos, hits = [], []
            for _, s in sub.groupby("season"):
                rhos.append(spearmanr(s[name], s.y)[0])
                top = set(s.nlargest(TOP_N, "y").pid)
                hits.append(len(top & set(s.nlargest(TOP_N, name).pid)))
            base = np.abs(sub["today"] - sub.y).values
            out[name] = {
                "mae": float(err.mean()),
                "spearman": float(np.mean(rhos)),
                "top50": float(np.mean(hits)),
                "z_vs_today": 0.0 if name == "today" else paired_z(err, base),
                "sd": float(sub[name].std()),
            }
        print(f"\n{label}: n={len(sub):,}  target sd {sub.y.std():.2f}")
        print(f"  {'':10} {'MAE':>6} {'rho':>6} {'top50':>6} {'z':>7} {'sd':>5}")
        for name in names:
            o = out[name]
            print(
                f"  {name:10} {o['mae']:6.3f} {o['spearman']:6.3f} {o['top50']:6.1f} "
                f"{o['z_vs_today']:7.1f} {o['sd']:5.2f}"
            )
        return out

    for t in CHECKPOINTS:
        summary["by_checkpoint"][str(t)] = report(res.t == t, f"after team game {t}")

    # Who a games-played clock misjudges: players who have missed games.
    missed = res.gp <= 0.7 * res.t
    summary["missed_games"] = report(missed & (res.t >= 5), "missed 30%+ of team games (T >= 5)")

    # Players with no projection: what should they shrink toward?
    summary["no_prior"] = {}
    y_all = no_prior.rate_rest * FULL_WEIGHT
    obs_all = no_prior.rate * FULL_WEIGHT
    print(f"\nno projection: n={len(no_prior):,}  target mean {y_all.mean():.2f} sd {y_all.std():.2f}")
    best = min(
        ((c, k) for c in np.arange(-4, 4.5, 0.5) for k in K_GRID),
        key=lambda ck: np.abs(
            ck[1] / (ck[1] + no_prior.gp) * ck[0] + no_prior.gp / (ck[1] + no_prior.gp) * obs_all - y_all
        ).mean(),
    )
    for t in (1, 3, 5, 10, 20):
        m = no_prior.t == t
        g, o, y = no_prior.gp[m], obs_all[m], y_all[m]
        rate = no_prior.rate[m]
        row = {
            "today": float(np.abs(rate * gp_weight(g) - y).mean()),
            "zero_k": float(np.abs(g / (g + best[1]) * o - y).mean()),
            "best": float(np.abs(best[1] / (best[1] + g) * best[0] + g / (g + best[1]) * o - y).mean()),
        }
        summary["no_prior"][str(t)] = row
        print(f"  T={t:2d} n={m.sum():5d} today {row['today']:.3f}  to zero {row['zero_k']:.3f}  to {best[0]:+.1f} (K={best[1]}) {row['best']:.3f}")

    # The O and D halves: split the projection evenly, or around the player's
    # last-season profile, as the team ratings split theirs (#451)?
    halves = pd.read_sql(
        text(
            """
            SELECT c.season, (c.cam_o_gbpm_v3_psos - c.cam_d_gbpm_v3_psos) AS gap,
                   (b.cam_o_gbpm_v3_psos - b.cam_d_gbpm_v3_psos) AS last_gap
              FROM torvik_player_stats c
              JOIN torvik_player_stats b
                ON b.torvik_pid = c.torvik_pid AND b.season = c.season - 1
             WHERE c.season >= :first AND c.total_minutes >= 10
               AND abs(c.cam_o_gbpm_v3_psos) <= 30 AND abs(c.cam_d_gbpm_v3_psos) <= 30
               AND abs(b.cam_o_gbpm_v3_psos) <= 30 AND abs(b.cam_d_gbpm_v3_psos) <= 30
            """
        ),
        engine,
        params={"first": WALK_FROM},
    ).astype(float)
    even = float(halves.gap.abs().mean())
    tilted = float((halves.gap - halves.last_gap).abs().mean())
    summary["halves"] = {"n": len(halves), "even_mae": even, "last_season_mae": tilted}
    print(f"\nO-D gap: n={len(halves):,} even split MAE {even:.3f}, last-season profile {tilted:.3f}")

    out = args.out or (
        EVAL_DIR / f"cam_preseason_prior_{dt.date.today():%Y%m%d}_summary.json"
    )
    out.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
