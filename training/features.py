"""
Point-in-time feature extraction for game outcome prediction.

For each game, features are computed using ONLY data from games that occurred
BEFORE that game's date. This eliminates data leakage from full-season aggregates.

Feature groups:
  - Adjusted efficiency (KenPom-style iterative, recomputed per game-date snapshot)
  - ELO (NatStat pre-game ELO where available, computed incremental as fallback)
  - Cumulative team four factors and box score stats (expanding window)
  - Cumulative roster aggregates (minutes-weighted player averages)
  - Rolling form (last-5-game rolling averages from player_game_stats)
  - Point-in-time SOS (from adjusted efficiency snapshots)
  - Context: venue, conference matchup, win percentage diff
"""

import numpy as np
import pandas as pd
from db import get_engine

SEASONS = [2026]

# ELO parameters
ELO_K = 20.0
ELO_HOME_ADV = 3.5
ELO_INIT = 1500.0


# ---------------------------------------------------------------------------
# Data loaders — pull raw game-level data
# ---------------------------------------------------------------------------

def load_games(engine, seasons=None) -> pd.DataFrame:
    """Load all completed games with scores and team IDs."""
    seasons = seasons or SEASONS
    return pd.read_sql(
        """
        SELECT g.id as game_id, g.game_date, g.season,
               g.home_team_id, g.away_team_id,
               g.home_score, g.away_score,
               g.is_neutral_site, g.is_conference
        FROM games g
        WHERE g.season = ANY(%(seasons)s)
          AND g.home_score IS NOT NULL
          AND g.away_score IS NOT NULL
          AND g.home_team_id IS NOT NULL
          AND g.away_team_id IS NOT NULL
        ORDER BY g.game_date, g.id
        """,
        engine,
        params={"seasons": seasons},
    )


def load_team_game_stats(engine, seasons=None) -> pd.DataFrame:
    """Load per-team per-game box scores."""
    seasons = seasons or SEASONS
    return pd.read_sql(
        """
        SELECT tgs.team_id, tgs.game_id, tgs.game_date, tgs.opponent_id,
               tgs.is_home, tgs.win,
               tgs.points, tgs.fgm, tgs.fga, tgs.tpm, tgs.tpa,
               tgs.ftm, tgs.fta,
               tgs.off_rebounds, tgs.def_rebounds, tgs.total_rebounds,
               tgs.assists, tgs.steals, tgs.blocks, tgs.turnovers, tgs.fouls
        FROM team_game_stats tgs
        WHERE tgs.season = ANY(%(seasons)s)
          AND tgs.points IS NOT NULL
          AND tgs.fga IS NOT NULL
        ORDER BY tgs.game_date, tgs.game_id
        """,
        engine,
        params={"seasons": seasons},
    )


def load_player_game_stats(engine, seasons=None) -> pd.DataFrame:
    """Load per-player per-game box scores with rolling averages."""
    seasons = seasons or SEASONS
    return pd.read_sql(
        """
        SELECT pgs.player_id, pgs.team_id, pgs.game_id, pgs.game_date,
               pgs.minutes, pgs.points, pgs.fgm, pgs.fga, pgs.tpm, pgs.tpa,
               pgs.ftm, pgs.fta,
               pgs.off_rebounds, pgs.def_rebounds, pgs.total_rebounds,
               pgs.assists, pgs.turnovers, pgs.steals, pgs.blocks,
               pgs.game_score, pgs.usage_rate,
               pgs.team_fga, pgs.team_fgm, pgs.team_turnovers,
               pgs.rolling_ppg, pgs.rolling_rpg, pgs.rolling_apg,
               pgs.rolling_fg_pct, pgs.rolling_ts_pct, pgs.rolling_game_score
        FROM player_game_stats pgs
        WHERE pgs.season = ANY(%(seasons)s)
          AND pgs.minutes IS NOT NULL
          AND pgs.minutes > 0
        ORDER BY pgs.game_date, pgs.game_id
        """,
        engine,
        params={"seasons": seasons},
    )


def load_game_forecasts(engine, seasons=None) -> pd.DataFrame:
    """Load pre-game ELO from NatStat forecasts.

    Only loads elo_before (pre-game) to avoid data leakage.
    win_exp is loaded separately for benchmarking — NOT as a training feature.
    """
    seasons = seasons or SEASONS
    return pd.read_sql(
        """
        SELECT gf.game_id,
               gf.home_team_id, gf.away_team_id,
               gf.home_elo_before, gf.away_elo_before,
               gf.home_win_exp, gf.away_win_exp
        FROM game_forecasts gf
        WHERE gf.season = ANY(%(seasons)s)
          AND gf.home_elo_before IS NOT NULL
        ORDER BY gf.game_date
        """,
        engine,
        params={"seasons": seasons},
    )


def load_team_conferences(engine, seasons=None) -> pd.DataFrame:
    """Load team conferences."""
    seasons = seasons or SEASONS
    return pd.read_sql(
        """
        SELECT id as team_id, conference
        FROM teams
        WHERE season = ANY(%(seasons)s)
        """,
        engine,
        params={"seasons": seasons},
    )


# ---------------------------------------------------------------------------
# Adjusted efficiency — KenPom-style iterative, per date snapshot
# ---------------------------------------------------------------------------

def compute_adjusted_efficiency_snapshot(tgs_prior: pd.DataFrame):
    """
    Given team_game_stats rows for all games before a date, compute
    adjusted offensive/defensive efficiency and SOS for each team.

    Returns dict: team_id -> {adj_off, adj_def, adj_margin, sos}
    """
    if tgs_prior.empty:
        return {}

    # Vectorized: filter and compute possessions
    df = tgs_prior[tgs_prior["opponent_id"].notna()].copy()
    df["pts"] = df["points"].fillna(0).astype(float)
    df["poss"] = (
        df["fga"].fillna(0) - df["off_rebounds"].fillna(0)
        + df["turnovers"].fillna(0) + 0.44 * df["fta"].fillna(0)
    )
    df = df[df["poss"] > 0]

    if df.empty:
        return {}

    # Convert to numpy arrays for fast iteration
    team_ids = df["team_id"].values
    opp_ids = df["opponent_id"].values
    pts_arr = df["pts"].values
    poss_arr = df["poss"].values
    n = len(df)

    games = [
        {"team_id": team_ids[i], "opponent_id": opp_ids[i],
         "points": pts_arr[i], "possessions": poss_arr[i]}
        for i in range(n)
    ]

    # Raw per-team totals
    raw_off = {}  # team_id -> (total_pts, total_poss)
    raw_def = {}  # team_id -> (total_pts_allowed, total_poss)

    for g in games:
        tid = g["team_id"]
        oid = g["opponent_id"]
        # Offense
        op, opos = raw_off.get(tid, (0.0, 0.0))
        raw_off[tid] = (op + g["points"], opos + g["possessions"])
        # Defense (opponent scored these points against us)
        dp, dpos = raw_def.get(oid, (0.0, 0.0))
        raw_def[oid] = (dp + g["points"], dpos + g["possessions"])

    all_teams = set(raw_off.keys()) | set(raw_def.keys())

    # League average
    total_pts = sum(v[0] for v in raw_off.values())
    total_poss = sum(v[1] for v in raw_off.values())
    league_avg = (total_pts / total_poss * 100.0) if total_poss > 0 else 100.0

    # Initialize to raw efficiencies
    adj_off = {}
    adj_def = {}
    for tid in all_teams:
        op, opos = raw_off.get(tid, (0.0, 0.0))
        adj_off[tid] = (op / opos * 100.0) if opos > 0 else league_avg
        dp, dpos = raw_def.get(tid, (0.0, 0.0))
        adj_def[tid] = (dp / dpos * 100.0) if dpos > 0 else league_avg

    # Iterative adjustment (same algorithm as Rust compute.rs)
    for _ in range(50):
        # Accumulate possession-weighted opponent ratings
        opp_def_sum = {}  # team_id -> (weighted_sum, total_poss)
        opp_off_sum = {}  # team_id -> (weighted_sum, total_poss)

        for g in games:
            tid = g["team_id"]
            oid = g["opponent_id"]
            poss = g["possessions"]

            # team's offense faced opponent's defense
            od = adj_def.get(oid, league_avg)
            s, p = opp_def_sum.get(tid, (0.0, 0.0))
            opp_def_sum[tid] = (s + od * poss, p + poss)

            # opponent's defense faced team's offense
            to = adj_off.get(tid, league_avg)
            s, p = opp_off_sum.get(oid, (0.0, 0.0))
            opp_off_sum[oid] = (s + to * poss, p + poss)

        max_change = 0.0

        for tid in all_teams:
            # Update offense
            op, opos = raw_off.get(tid, (0.0, 0.0))
            if opos > 0:
                raw_o = op / opos * 100.0
                s, p = opp_def_sum.get(tid, (0.0, 0.0))
                avg_opp_def = (s / p) if p > 0 else league_avg
                new_off = raw_o * (league_avg / avg_opp_def)
                max_change = max(max_change, abs(new_off - adj_off.get(tid, league_avg)))
                adj_off[tid] = new_off

            # Update defense
            dp, dpos = raw_def.get(tid, (0.0, 0.0))
            if dpos > 0:
                raw_d = dp / dpos * 100.0
                s, p = opp_off_sum.get(tid, (0.0, 0.0))
                avg_opp_off = (s / p) if p > 0 else league_avg
                new_def = raw_d * (league_avg / avg_opp_off)
                max_change = max(max_change, abs(new_def - adj_def.get(tid, league_avg)))
                adj_def[tid] = new_def

        if max_change < 0.01:
            break

    # SOS: average opponent adjusted margin
    opp_margin_sum = {}
    opp_margin_cnt = {}
    for g in games:
        tid = g["team_id"]
        oid = g["opponent_id"]
        opp_margin = adj_off.get(oid, league_avg) - adj_def.get(oid, league_avg)
        opp_margin_sum[tid] = opp_margin_sum.get(tid, 0.0) + opp_margin
        opp_margin_cnt[tid] = opp_margin_cnt.get(tid, 0) + 1

    result = {}
    for tid in all_teams:
        ao = adj_off.get(tid, league_avg)
        ad = adj_def.get(tid, league_avg)
        cnt = opp_margin_cnt.get(tid, 0)
        sos = (opp_margin_sum.get(tid, 0.0) / cnt) if cnt > 0 else 0.0
        result[tid] = {
            "adj_offense": ao,
            "adj_defense": ad,
            "adj_efficiency_margin": ao - ad,
            "sos": sos,
        }

    return result


# ---------------------------------------------------------------------------
# ELO — incremental game-by-game
# ---------------------------------------------------------------------------

def compute_elo_series(games_df: pd.DataFrame) -> dict:
    """
    Compute ELO ratings incrementally for all teams.
    games_df must be sorted by game_date.

    Returns dict: {(team_id, game_id) -> elo_before_game}
    We store the ELO *before* the game so it can be used as a feature.
    """
    elo = {}  # team_id -> current elo
    # Store the elo snapshot before each game for exact lookup
    elo_snapshots = {}  # (team_id, game_id) -> elo_before_game

    for _, row in games_df.iterrows():
        home = row["home_team_id"]
        away = row["away_team_id"]
        gid = row["game_id"]

        home_elo = elo.get(home, ELO_INIT)
        away_elo = elo.get(away, ELO_INIT)

        # Record pre-game ELO (keyed by game_id for exact match)
        elo_snapshots[(home, gid)] = home_elo
        elo_snapshots[(away, gid)] = away_elo

        # Expected scores
        ha = 0.0 if row["is_neutral_site"] else ELO_HOME_ADV
        exp_home = 1.0 / (1.0 + 10.0 ** ((away_elo - home_elo - ha) / 400.0))

        # Actual result
        margin = row["home_score"] - row["away_score"]
        actual_home = 1.0 if margin > 0 else (0.0 if margin < 0 else 0.5)

        # Margin-of-victory multiplier (FiveThirtyEight style)
        mov_mult = np.log(abs(margin) + 1) * (2.2 / ((home_elo - away_elo) * 0.001 + 2.2))

        # Update
        delta = ELO_K * mov_mult * (actual_home - exp_home)
        elo[home] = home_elo + delta
        elo[away] = away_elo - delta

    return elo_snapshots


# ---------------------------------------------------------------------------
# Cumulative team stats — expanding window from team_game_stats
# ---------------------------------------------------------------------------

def compute_cumulative_team_stats(tgs: pd.DataFrame, games_df: pd.DataFrame):
    """
    For each (team_id, game_date), compute cumulative averages of team stats
    using only games BEFORE that date.

    Returns DataFrame with one row per (team_id, game_date) with cumulative features.
    """
    tgs = tgs.sort_values(["team_id", "game_date"]).copy()

    # Compute per-game derived stats before aggregation
    tgs["possessions"] = tgs["fga"] - tgs["off_rebounds"] + tgs["turnovers"] + 0.44 * tgs["fta"]
    tgs["possessions"] = tgs["possessions"].clip(lower=1)
    tgs["off_eff"] = tgs["points"] / tgs["possessions"] * 100.0

    tgs["efg_pct"] = (tgs["fgm"] + 0.5 * tgs["tpm"]) / tgs["fga"].replace(0, np.nan)
    tgs["tov_pct"] = tgs["turnovers"] / tgs["possessions"]
    tgs["orb_pct"] = tgs["off_rebounds"] / tgs["total_rebounds"].replace(0, np.nan)
    tgs["ft_rate"] = tgs["fta"] / tgs["fga"].replace(0, np.nan)

    # We need opponent stats for defensive four factors — join opponent's row for same game
    tgs_opp = tgs[["team_id", "game_id", "efg_pct", "tov_pct", "orb_pct", "ft_rate", "off_eff"]].rename(
        columns={
            "efg_pct": "opp_efg_pct",
            "tov_pct": "opp_tov_pct",
            "orb_pct": "opp_orb_pct",
            "ft_rate": "opp_ft_rate",
            "off_eff": "opp_off_eff",
        }
    )
    tgs = tgs.merge(
        tgs_opp,
        left_on=["opponent_id", "game_id"],
        right_on=["team_id", "game_id"],
        how="left",
        suffixes=("", "_opp_join"),
    ).drop(columns=["team_id_opp_join"], errors="ignore")

    # Compute def_rebound_pct as 1 - opponent's orb_pct
    tgs["def_reb_pct"] = 1.0 - tgs["opp_orb_pct"]

    # Point diff via merge with opponent
    opp_pts = tgs[["team_id", "game_id", "points"]].rename(
        columns={"team_id": "opp_tid", "points": "opp_points"}
    )
    tgs = tgs.merge(
        opp_pts,
        left_on=["opponent_id", "game_id"],
        right_on=["opp_tid", "game_id"],
        how="left",
    ).drop(columns=["opp_tid"], errors="ignore")
    tgs["game_point_diff"] = tgs["points"] - tgs["opp_points"].fillna(0)

    # Road win tracking
    tgs["win_bool"] = tgs["win"].fillna(False).astype(bool)
    tgs["is_road"] = (~tgs["is_home"].fillna(True)).astype(bool)
    tgs["road_win"] = tgs["is_road"] & tgs["win_bool"]

    # Now compute expanding-window (cumulative) stats per team,
    # excluding the current game (shift by 1)
    agg_cols = [
        "off_eff", "efg_pct", "tov_pct", "orb_pct", "ft_rate",
        "opp_efg_pct", "opp_tov_pct", "def_reb_pct", "opp_ft_rate", "opp_off_eff",
        "game_point_diff", "possessions",
    ]

    results = []
    for team_id, team_df in tgs.groupby("team_id"):
        team_df = team_df.sort_values("game_date").reset_index(drop=True)

        # Expanding cumulative mean, shifted so row i uses games 0..i-1
        for col in agg_cols:
            team_df[f"cum_{col}"] = team_df[col].expanding().mean().shift(1)

        # Cumulative wins/losses
        team_df["cum_wins"] = team_df["win_bool"].astype(float).cumsum().shift(1).fillna(0)
        team_df["cum_losses"] = (~team_df["win_bool"]).astype(float).cumsum().shift(1).fillna(0)

        # Cumulative road wins / road games
        team_df["cum_road_wins"] = team_df["road_win"].astype(float).cumsum().shift(1).fillna(0)
        team_df["cum_road_games"] = team_df["is_road"].astype(float).cumsum().shift(1).fillna(0)

        # Cumulative points scored and allowed (for pythagorean)
        team_df["cum_pts_for"] = team_df["points"].astype(float).cumsum().shift(1)
        team_df["cum_pts_against"] = team_df["opp_points"].fillna(0).astype(float).cumsum().shift(1)

        results.append(team_df[[
            "team_id", "game_id", "game_date",
        ] + [f"cum_{c}" for c in agg_cols] + [
            "cum_wins", "cum_losses",
            "cum_road_wins", "cum_road_games",
            "cum_pts_for", "cum_pts_against",
        ]])

    cum_df = pd.concat(results, ignore_index=True)

    # Derived cumulative metrics
    total_games = cum_df["cum_wins"] + cum_df["cum_losses"]
    cum_df["cum_win_pct"] = cum_df["cum_wins"] / total_games.replace(0, np.nan)
    cum_df["cum_road_win_pct"] = cum_df["cum_road_wins"] / cum_df["cum_road_games"].replace(0, np.nan)

    # Pythagorean win% (exponent ~11.5 for college basketball)
    pf = cum_df["cum_pts_for"]
    pa = cum_df["cum_pts_against"]
    exp = 11.5
    cum_df["cum_pythag_win_pct"] = pf**exp / (pf**exp + pa**exp)

    # Tempo: average possessions per game
    cum_df["cum_tempo"] = cum_df["cum_possessions"]

    return cum_df


# ---------------------------------------------------------------------------
# Cumulative roster aggregates — expanding player averages
# ---------------------------------------------------------------------------

def compute_cumulative_roster_stats(pgs: pd.DataFrame, games_df: pd.DataFrame):
    """
    For each (team_id, game_date), compute minutes-weighted roster averages
    using only player game stats from BEFORE that date.

    Returns DataFrame with one row per (team_id, game_date).
    """
    pgs = pgs.sort_values(["player_id", "game_date"]).copy()

    # Compute per-game advanced stats that we need
    pgs["ts_pct"] = pgs["points"] / (2.0 * (pgs["fga"] + 0.44 * pgs["fta"])).replace(0, np.nan)
    pgs["efg_pct"] = (pgs["fgm"] + 0.5 * pgs["tpm"]) / pgs["fga"].replace(0, np.nan)
    pgs["ast_to"] = pgs["assists"] / pgs["turnovers"].replace(0, np.nan)

    # BPM approximation: game_score is already computed (Hollinger)
    # OBPM/DBPM split: offensive game score vs defensive game score
    pgs["off_game_score"] = (
        pgs["points"] * 0.5 + pgs["fgm"] * 0.5 - pgs["fga"] * 0.35
        + pgs["ftm"] * 0.3 - pgs["fta"] * 0.2
        + pgs["assists"] * 0.6 + pgs["off_rebounds"] * 0.5
        - pgs["turnovers"] * 0.8
    )
    pgs["def_game_score"] = (
        pgs["def_rebounds"] * 0.5 + pgs["steals"] * 0.7
        + pgs["blocks"] * 0.7
    )

    # Rate stats (per-40-minute proxies)
    min40 = pgs["minutes"].replace(0, np.nan) / 40.0
    team_fga_safe = pgs["team_fga"].replace(0, np.nan)
    pgs["ast_pct_g"] = pgs["assists"] / team_fga_safe
    pgs["tov_pct_g"] = pgs["turnovers"] / (pgs["fga"] + 0.44 * pgs["fta"] + pgs["turnovers"]).replace(0, np.nan)
    pgs["stl_pct_g"] = pgs["steals"] / min40
    pgs["blk_pct_g"] = pgs["blocks"] / min40

    # ORTG/DRTG approximation per game: points produced / possessions used
    pgs["poss_used"] = pgs["fga"] - pgs["off_rebounds"] + pgs["turnovers"] + 0.44 * pgs["fta"]
    pgs["poss_used"] = pgs["poss_used"].clip(lower=0.1)
    pgs["ortg_g"] = pgs["points"] / pgs["poss_used"] * 100.0

    # Usage rate from NatStat (per-game, no leakage)
    pgs["usage_g"] = pgs["usage_rate"]

    # Stat columns to accumulate per player
    stat_cols = [
        "minutes", "points", "total_rebounds", "assists", "steals", "blocks", "turnovers",
        "fgm", "fga", "tpm", "tpa", "ftm", "fta",
        "off_rebounds", "def_rebounds",
        "ts_pct", "efg_pct", "game_score",
        "off_game_score", "def_game_score",
        "ast_pct_g", "tov_pct_g", "stl_pct_g", "blk_pct_g",
        "ortg_g", "usage_g",
    ]

    # For each player, compute expanding cumulative average (shifted)
    player_cum = []
    for player_id, pdf in pgs.groupby("player_id"):
        pdf = pdf.sort_values("game_date").reset_index(drop=True)
        entry = pdf[["player_id", "team_id", "game_id", "game_date", "minutes"]].copy()

        for col in stat_cols:
            entry[f"cum_{col}"] = pdf[col].expanding().mean().shift(1)

        # Count of prior games
        entry["cum_games"] = np.arange(len(pdf), dtype=float)  # 0 for first game, 1 for second, etc.

        # Rolling form: use the rolling columns directly from each game row (shifted)
        for rc in ["rolling_ppg", "rolling_rpg", "rolling_apg",
                    "rolling_fg_pct", "rolling_ts_pct", "rolling_game_score"]:
            # Shift by 1 so we use the rolling value computed AFTER the previous game
            entry[rc] = pdf[rc].shift(1)

        player_cum.append(entry)

    player_cum_df = pd.concat(player_cum, ignore_index=True)

    # Now aggregate per (team_id, game_date): minutes-weighted averages
    # across qualified players (>= 5 prior games, >= 10 cum_minutes)
    qualified = player_cum_df[
        (player_cum_df["cum_games"] >= 5) &
        (player_cum_df["cum_minutes"].notna()) &
        (player_cum_df["cum_minutes"] >= 10)
    ].copy()

    # Weight = cumulative total minutes (cum_minutes * cum_games)
    qualified["weight"] = qualified["cum_minutes"] * qualified["cum_games"]

    # Weighted average columns
    w_cols = {
        "cum_points": "w_ppg",
        "cum_total_rebounds": "w_rpg",
        "cum_assists": "w_apg",
        "cum_steals": "w_spg",
        "cum_blocks": "w_bpg",
        "cum_turnovers": "w_topg",
        "cum_ts_pct": "w_ts_pct",
        "cum_efg_pct": "w_efg_pct",
        "cum_game_score": "w_bpm",
        "cum_off_game_score": "w_obpm",
        "cum_def_game_score": "w_dbpm",
        "cum_ortg_g": "w_ortg",
        "cum_usage_g": "w_usage",
        "cum_ast_pct_g": "w_ast_pct",
        "cum_tov_pct_g": "w_tov_pct",
        "cum_stl_pct_g": "w_stl_pct",
        "cum_blk_pct_g": "w_blk_pct",
    }

    # Rolling form columns
    rolling_w_cols = {
        "rolling_game_score": "w_rolling_gs",
        "rolling_ts_pct": "w_rolling_ts",
    }

    def weighted_agg(group):
        w = group["weight"]
        total_w = w.sum()
        if total_w == 0:
            return None

        row = {"roster_size": len(group)}

        for src, dst in w_cols.items():
            vals = group[src].fillna(0) * w
            row[dst] = vals.sum() / total_w

        for src, dst in rolling_w_cols.items():
            valid = group[src].notna()
            if valid.any():
                vals = group.loc[valid, src] * w[valid]
                row[dst] = vals.sum() / w[valid].sum()
            else:
                row[dst] = np.nan

        # Trends: rolling - cumulative average
        if "rolling_ppg" in group.columns:
            valid = group["rolling_ppg"].notna()
            if valid.any():
                trend = (group.loc[valid, "rolling_ppg"] - group.loc[valid, "cum_points"]) * w[valid]
                row["w_ppg_trend"] = trend.sum() / w[valid].sum()
            else:
                row["w_ppg_trend"] = np.nan

        if "rolling_game_score" in group.columns:
            valid = group["rolling_game_score"].notna()
            if valid.any():
                trend = (group.loc[valid, "rolling_game_score"] - group.loc[valid, "cum_game_score"]) * w[valid]
                row["w_gs_trend"] = trend.sum() / w[valid].sum()
            else:
                row["w_gs_trend"] = np.nan

        # Star player stats (top by weight)
        top_idx = w.idxmax()
        row["star_ppg"] = group.loc[top_idx, "cum_points"] if pd.notna(group.loc[top_idx, "cum_points"]) else np.nan
        row["star_bpm"] = group.loc[top_idx, "cum_game_score"] if pd.notna(group.loc[top_idx, "cum_game_score"]) else np.nan
        row["star_ortg"] = group.loc[top_idx, "cum_ortg_g"] if pd.notna(group.loc[top_idx, "cum_ortg_g"]) else np.nan

        # Minutes stddev (depth indicator)
        row["minutes_stddev"] = group["cum_minutes"].std()

        return pd.Series(row)

    # Group by team_id + game_id + game_date (all players on that team for that game)
    roster_agg = qualified.groupby(["team_id", "game_id", "game_date"]).apply(
        weighted_agg, include_groups=False,
    ).reset_index()

    return roster_agg


# ---------------------------------------------------------------------------
# Player SOS — point-in-time using adjusted efficiency snapshots
# ---------------------------------------------------------------------------

def compute_player_sos(pgs: pd.DataFrame, adj_eff_snapshots: dict):
    """
    For each (team_id, game_date), compute minutes-weighted player SOS
    using the adjusted efficiency margin of each player's opponents.

    adj_eff_snapshots: {game_date -> {team_id -> {adj_efficiency_margin, ...}}}
    """
    pgs = pgs.sort_values(["player_id", "game_date"]).copy()

    # For each player game, look up the opponent's adj margin from the snapshot
    # at that game's date (or the most recent snapshot before it)
    sorted_dates = sorted(adj_eff_snapshots.keys())

    # Flatten snapshots to (date, team_id) -> margin for fast lookup
    flat_snap = {}
    for d in sorted_dates:
        for tid, vals in adj_eff_snapshots[d].items():
            flat_snap[(d, tid)] = vals.get("adj_efficiency_margin", 0.0)

    # For each player-game, find opponent margin
    # First, map each game_date to its snapshot date
    pgs_dates = pgs["game_date"].unique()
    date_to_snap = {}
    for gd in pgs_dates:
        snap_date = None
        for d in sorted_dates:
            if d <= gd:
                snap_date = d
            else:
                break
        date_to_snap[gd] = snap_date

    # Now compute per-player cumulative SOS
    results = []
    for player_id, pdf in pgs.groupby("player_id"):
        pdf = pdf.sort_values("game_date").reset_index(drop=True)

        opp_margins = []
        minutes_list = []
        cum_sos_vals = []

        for _, row in pdf.iterrows():
            # SOS up to this point (excluding current game)
            if opp_margins:
                total_min = sum(minutes_list)
                if total_min > 0:
                    w_sos = sum(m * om for m, om in zip(minutes_list, opp_margins)) / total_min
                else:
                    w_sos = np.nan
            else:
                w_sos = np.nan
            cum_sos_vals.append(w_sos)

            # Record this game's opponent margin for future games
            snap_d = date_to_snap.get(row["game_date"])
            if snap_d is not None and pd.notna(row.get("opponent_id")):
                margin = flat_snap.get((snap_d, row.get("opponent_id")), 0.0)
            else:
                margin = 0.0
            opp_margins.append(margin)
            minutes_list.append(row["minutes"] if pd.notna(row["minutes"]) else 0)

        pdf_out = pdf[["player_id", "team_id", "game_date"]].copy()
        pdf_out["cum_player_sos"] = cum_sos_vals
        results.append(pdf_out)

    return pd.concat(results, ignore_index=True)


# ---------------------------------------------------------------------------
# Main feature matrix builder
# ---------------------------------------------------------------------------

def build_feature_matrix(engine, seasons=None) -> tuple[pd.DataFrame, list[str]]:
    """
    Build the full feature matrix with point-in-time features.

    Each row = one game. Features are differences (home - away) of team/roster stats,
    plus a venue indicator. All features use only data from before the game date.
    """
    seasons = seasons or SEASONS
    print(f"Loading raw data for seasons {seasons}...")
    games = load_games(engine, seasons)
    tgs = load_team_game_stats(engine, seasons)
    pgs = load_player_game_stats(engine, seasons)
    conferences = load_team_conferences(engine, seasons)

    forecasts = load_game_forecasts(engine, seasons)
    print(f"  {len(games)} games, {len(tgs)} team-game rows, {len(pgs)} player-game rows, {len(forecasts)} forecasts")

    # 1. ELO — prefer NatStat pre-game ELO, fall back to computed incremental ELO
    print("Computing ELO ratings...")
    elo_snapshots = compute_elo_series(games)

    # Index NatStat pre-game ELO by game_id for fast lookup
    natstat_elo = {}
    for _, row in forecasts.iterrows():
        gid = row["game_id"]
        natstat_elo[(row["home_team_id"], gid)] = row["home_elo_before"]
        natstat_elo[(row["away_team_id"], gid)] = row["away_elo_before"]

    # Merge: NatStat where available, computed as fallback
    merged_elo = {}
    for key, computed_val in elo_snapshots.items():
        merged_elo[key] = natstat_elo.get(key, computed_val)
    # Include any NatStat entries not in computed (shouldn't happen, but safe)
    for key, ns_val in natstat_elo.items():
        if key not in merged_elo:
            merged_elo[key] = ns_val
    elo_snapshots = merged_elo

    # Store NatStat win_exp for benchmarking (NOT a training feature)
    natstat_win_exp = {}
    for _, row in forecasts.iterrows():
        natstat_win_exp[row["game_id"]] = row["home_win_exp"]

    # 2. Adjusted efficiency snapshots (per unique game date)
    print("Computing adjusted efficiency snapshots...")
    unique_dates = sorted(games["game_date"].unique())
    adj_eff_snapshots = {}  # game_date -> {team_id -> {adj_off, adj_def, adj_margin, sos}}

    # Sort tgs by date once; use cumulative slicing for efficiency
    tgs_sorted = tgs.sort_values("game_date").reset_index(drop=True)
    for i, gd in enumerate(unique_dates):
        # All rows with game_date < gd
        prior_tgs = tgs_sorted[tgs_sorted["game_date"] < gd]
        if prior_tgs.empty:
            adj_eff_snapshots[gd] = {}
        else:
            adj_eff_snapshots[gd] = compute_adjusted_efficiency_snapshot(prior_tgs)
        if (i + 1) % 20 == 0:
            print(f"  ... {i + 1}/{len(unique_dates)} dates")

    # 3. Cumulative team stats
    print("Computing cumulative team stats...")
    cum_team = compute_cumulative_team_stats(tgs, games)

    # 4. Cumulative roster aggregates + rolling form
    print("Computing cumulative roster aggregates...")
    cum_roster = compute_cumulative_roster_stats(pgs, games)

    # 5. Player SOS
    print("Computing player SOS...")
    # We need opponent_id on pgs for SOS — load it
    pgs_with_opp = pgs.merge(
        tgs[["team_id", "game_id", "opponent_id"]].drop_duplicates(),
        on=["team_id", "game_id"],
        how="left",
    )
    player_sos = compute_player_sos(pgs_with_opp, adj_eff_snapshots)
    # Aggregate to team level: minutes-weighted average player SOS
    player_sos_team = player_sos.merge(
        pgs[["player_id", "team_id", "game_date", "minutes"]],
        on=["player_id", "team_id", "game_date"],
        how="left",
    )
    player_sos_team["w"] = player_sos_team["minutes"].fillna(0) * player_sos_team["cum_player_sos"].fillna(0)

    team_player_sos = player_sos_team.groupby(["team_id", "game_date"]).apply(
        lambda g: g["w"].sum() / g["minutes"].sum() if g["minutes"].sum() > 0 else np.nan,
        include_groups=False,
    ).reset_index(name="w_player_sos")

    # 6. Assemble features per game
    print("Assembling feature matrix...")
    df = games.copy()

    # Add conferences
    df = df.merge(
        conferences.rename(columns={"conference": "home_conference"}),
        left_on="home_team_id", right_on="team_id", how="left",
    ).drop(columns=["team_id"])
    df = df.merge(
        conferences.rename(columns={"conference": "away_conference"}),
        left_on="away_team_id", right_on="team_id", how="left",
    ).drop(columns=["team_id"])

    # --- Home team features ---

    # Adjusted efficiency
    for prefix, tid_col in [("home", "home_team_id"), ("away", "away_team_id")]:
        adj_rows = []
        for _, row in df.iterrows():
            gd = row["game_date"]
            snap = adj_eff_snapshots.get(gd, {})
            team_snap = snap.get(row[tid_col], {})
            adj_rows.append({
                f"{prefix}_adj_offense": team_snap.get("adj_offense"),
                f"{prefix}_adj_defense": team_snap.get("adj_defense"),
                f"{prefix}_adj_efficiency_margin": team_snap.get("adj_efficiency_margin"),
                f"{prefix}_sos": team_snap.get("sos"),
            })
        adj_df = pd.DataFrame(adj_rows)
        df = pd.concat([df.reset_index(drop=True), adj_df], axis=1)

    # ELO (keyed by game_id for exact pre-game rating)
    df["home_elo"] = df.apply(
        lambda r: elo_snapshots.get((r["home_team_id"], r["game_id"]), ELO_INIT), axis=1
    )
    df["away_elo"] = df.apply(
        lambda r: elo_snapshots.get((r["away_team_id"], r["game_id"]), ELO_INIT), axis=1
    )

    # Cumulative team stats
    cum_rename = {c: c for c in cum_team.columns if c.startswith("cum_")}
    for prefix, tid_col in [("home", "home_team_id"), ("away", "away_team_id")]:
        team_cum = cum_team.rename(
            columns={c: f"{prefix}_{c}" for c in cum_rename}
        )
        # Use game_id for exact match (a team may play multiple games on same date)
        df = df.merge(
            team_cum[[
                "game_id", "team_id",
            ] + [f"{prefix}_{c}" for c in cum_rename]],
            left_on=["game_id", tid_col],
            right_on=["game_id", "team_id"],
            how="left",
        ).drop(columns=["team_id"], errors="ignore")

    # Cumulative roster aggregates
    roster_cols_all = [c for c in cum_roster.columns if c not in ("team_id", "game_id", "game_date")]
    for prefix, tid_col in [("home", "home_team_id"), ("away", "away_team_id")]:
        roster_ren = cum_roster.rename(
            columns={c: f"{prefix}_{c}" for c in roster_cols_all}
        )
        df = df.merge(
            roster_ren,
            left_on=["game_id", tid_col],
            right_on=["game_id", "team_id"],
            how="left",
        ).drop(columns=["team_id", "game_date_y"], errors="ignore")
        if "game_date_x" in df.columns:
            df = df.rename(columns={"game_date_x": "game_date"})

    # Player SOS (team-level weighted)
    for prefix, tid_col in [("home", "home_team_id"), ("away", "away_team_id")]:
        df = df.merge(
            team_player_sos.rename(columns={"w_player_sos": f"{prefix}_w_player_sos"}),
            left_on=[tid_col, "game_date"],
            right_on=["team_id", "game_date"],
            how="left",
        ).drop(columns=["team_id"], errors="ignore")

    # --- Derived features ---

    # Venue indicator
    df["venue"] = df["is_neutral_site"].apply(lambda x: 0 if x else 1)

    # Conference matchup
    df["is_conference_game"] = (
        df["home_conference"].notna()
        & (df["home_conference"] == df["away_conference"])
    ).astype(int)

    # Win percentage diff
    df["diff_win_pct"] = (
        df["home_cum_win_pct"].fillna(0.5) - df["away_cum_win_pct"].fillna(0.5)
    )

    # --- Difference features (home - away) ---
    diff_pairs = {
        # Adjusted efficiency
        "adj_offense": "adj_offense",
        "adj_defense": "adj_defense",
        "adj_efficiency_margin": "adj_efficiency_margin",
        # Four factors (offense)
        "effective_fg_pct": "cum_efg_pct",
        "turnover_pct": "cum_tov_pct",
        "off_rebound_pct": "cum_orb_pct",
        "ft_rate": "cum_ft_rate",
        # Four factors (defense)
        "opp_effective_fg_pct": "cum_opp_efg_pct",
        "opp_turnover_pct": "cum_opp_tov_pct",
        "def_rebound_pct": "cum_def_reb_pct",
        "opp_ft_rate": "cum_opp_ft_rate",
        # Tempo
        "adj_tempo": "cum_tempo",
        # Power metrics
        "sos": "sos",
        "elo": "elo",
        "point_diff": "cum_game_point_diff",
        "pythag_win_pct": "cum_pythag_win_pct",
        "road_win_pct": "cum_road_win_pct",
        # Roster box score
        "roster_size": "roster_size",
        "w_ppg": "w_ppg",
        "w_rpg": "w_rpg",
        "w_apg": "w_apg",
        "w_spg": "w_spg",
        "w_bpg": "w_bpg",
        "w_topg": "w_topg",
        "w_ts_pct": "w_ts_pct",
        "w_efg_pct": "w_efg_pct",
        "w_usage": "w_usage",
        "w_bpm": "w_bpm",
        "w_player_sos": "w_player_sos",
        # Roster advanced
        "w_obpm": "w_obpm",
        "w_dbpm": "w_dbpm",
        "w_ortg": "w_ortg",
        "w_ast_pct": "w_ast_pct",
        "w_tov_pct": "w_tov_pct",
        "w_stl_pct": "w_stl_pct",
        "w_blk_pct": "w_blk_pct",
        # Star power
        "star_ppg": "star_ppg",
        "star_bpm": "star_bpm",
        "star_ortg": "star_ortg",
        # Depth
        "minutes_stddev": "minutes_stddev",
        # Rolling form
        "w_rolling_gs": "w_rolling_gs",
        "w_rolling_ts": "w_rolling_ts",
        "w_ppg_trend": "w_ppg_trend",
        "w_gs_trend": "w_gs_trend",
    }

    for name, col in diff_pairs.items():
        home_col = f"home_{col}"
        away_col = f"away_{col}"
        if home_col in df.columns and away_col in df.columns:
            df[f"diff_{name}"] = df[home_col] - df[away_col]

    # For defense, lower is better — flip sign so positive = better for home
    for col in ["diff_adj_defense", "diff_opp_effective_fg_pct", "diff_opp_ft_rate", "diff_w_drtg"]:
        if col in df.columns:
            df[col] = -df[col]

    # Targets
    df["margin"] = df["home_score"] - df["away_score"]
    df["home_win"] = (df["margin"] > 0).astype(int)

    # NatStat win expectancy for benchmarking only (NOT a training feature)
    df["natstat_home_win_exp"] = df["game_id"].map(natstat_win_exp)

    # Select feature columns
    feature_cols = (
        ["venue", "is_conference_game", "diff_win_pct"]
        + [c for c in df.columns if c.startswith("diff_") and c != "diff_win_pct"]
    )

    print(f"  Features: {len(feature_cols)}")
    return df, feature_cols


if __name__ == "__main__":
    engine = get_engine()
    df, feature_cols = build_feature_matrix(engine)
    print(f"\nGames: {len(df)}")
    print(f"Features: {len(feature_cols)}")
    print(f"Home win rate: {df['home_win'].mean():.3f}")
    print(f"Avg margin: {df['margin'].mean():.1f}")
    print(f"\nFeature columns:\n{feature_cols}")
    print(f"\nNull counts:\n{df[feature_cols].isnull().sum().to_string()}")
    print(f"\nSample:\n{df[feature_cols + ['margin', 'home_win']].head()}")

    # NatStat win_exp benchmark
    has_exp = df["natstat_home_win_exp"].notna()
    if has_exp.any():
        bench = df[has_exp].copy()
        ns_pred = (bench["natstat_home_win_exp"] > 0.5).astype(int)
        ns_acc = (ns_pred == bench["home_win"]).mean()
        print(f"\nNatStat win_exp benchmark ({has_exp.sum()} games):")
        print(f"  Accuracy: {ns_acc:.3f}")
