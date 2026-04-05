"""
Feature extraction for game outcome prediction.

Builds a training dataset where each row is a game with features derived from
both teams' season-level stats and aggregated player metrics.

Features per team:
  - Adjusted offense/defense/margin (KenPom-style)
  - Four factors: eFG%, TOV%, ORB%, FT rate (offense + defense)
  - Tempo
  - SOS
  - Roster aggregates: minutes-weighted avg of top players' PPG, RPG, APG,
    TS%, usage, BPM, player SOS

Game-level:
  - is_home (1) / is_away (-1) / neutral (0) from the perspective of team_a
  - Difference features: team_a - team_b for each stat
"""

import pandas as pd
from db import get_engine

SEASON = 2026


def load_team_season_stats(engine) -> pd.DataFrame:
    """Load team season stats with four factors, adjusted ratings, and power metrics."""
    return pd.read_sql(
        """
        SELECT team_id, wins, losses,
               adj_offense, adj_defense, adj_efficiency_margin, adj_tempo,
               effective_fg_pct, turnover_pct, off_rebound_pct, ft_rate,
               opp_effective_fg_pct, opp_turnover_pct, def_rebound_pct, opp_ft_rate,
               sos, sos_rank,
               elo, point_diff, pythag_win_pct, road_win_pct, conference
        FROM team_season_stats
        WHERE season = %(season)s
        """,
        engine,
        params={"season": SEASON},
    )


def load_roster_aggregates(engine) -> pd.DataFrame:
    """
    For each team, compute minutes-weighted averages of key player stats
    across the roster (players with >=10 games, >=10 MPG).
    """
    return pd.read_sql(
        """
        WITH qualified AS (
            SELECT player_id, team_id,
                   minutes_per_game, games_played,
                   ppg, rpg, apg, spg, bpg, topg,
                   true_shooting_pct, effective_fg_pct,
                   usage_rate, bpm, obpm, dbpm, player_sos,
                   offensive_rating, defensive_rating, net_rating,
                   ast_pct, tov_pct, orb_pct, drb_pct, stl_pct, blk_pct,
                   -- weight = total minutes played
                   minutes_per_game * games_played as total_minutes
            FROM player_season_stats
            WHERE season = %(season)s
              AND games_played >= 10
              AND minutes_per_game >= 10
        )
        SELECT team_id,
               COUNT(*) as roster_size,
               SUM(total_minutes) as total_team_minutes,

               -- Minutes-weighted averages: box score
               SUM(ppg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_ppg,
               SUM(rpg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_rpg,
               SUM(apg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_apg,
               SUM(spg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_spg,
               SUM(bpg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_bpg,
               SUM(topg * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_topg,
               SUM(COALESCE(true_shooting_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_ts_pct,
               SUM(COALESCE(effective_fg_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_efg_pct,
               SUM(COALESCE(usage_rate, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_usage,
               SUM(bpm * total_minutes) / NULLIF(SUM(total_minutes), 0) as w_bpm,
               SUM(COALESCE(player_sos, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_player_sos,

               -- Minutes-weighted averages: advanced ratings
               SUM(COALESCE(obpm, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_obpm,
               SUM(COALESCE(dbpm, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_dbpm,
               SUM(COALESCE(offensive_rating, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_ortg,
               SUM(COALESCE(defensive_rating, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_drtg,
               SUM(COALESCE(net_rating, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_net_rating,

               -- Minutes-weighted averages: rate stats
               SUM(COALESCE(ast_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_ast_pct,
               SUM(COALESCE(tov_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_tov_pct,
               SUM(COALESCE(stl_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_stl_pct,
               SUM(COALESCE(blk_pct, 0) * total_minutes)
                   / NULLIF(SUM(total_minutes), 0) as w_blk_pct,

               -- Star player stats (top player by total minutes)
               MAX(ppg) as star_ppg,
               MAX(bpm) as star_bpm,
               MAX(COALESCE(offensive_rating, 0)) as star_ortg,

               -- Depth: std dev of minutes distribution
               STDDEV(minutes_per_game) as minutes_stddev
        FROM qualified
        GROUP BY team_id
        """,
        engine,
        params={"season": SEASON},
    )


def load_rolling_form(engine) -> pd.DataFrame:
    """
    For each team, compute minutes-weighted rolling form from the most recent
    player_game_stats rolling averages. This captures whether a team is trending
    up or down heading into each game.
    """
    return pd.read_sql(
        """
        WITH latest_rolling AS (
            -- For each player, get their most recent game's rolling averages
            SELECT DISTINCT ON (pgs.player_id)
                   pgs.player_id,
                   pss.team_id,
                   pss.minutes_per_game,
                   pss.games_played,
                   pgs.rolling_ppg,
                   pgs.rolling_rpg,
                   pgs.rolling_apg,
                   pgs.rolling_fg_pct,
                   pgs.rolling_ts_pct,
                   pgs.rolling_game_score,
                   -- Compare rolling to season avg as "form" indicator
                   pgs.rolling_ppg - pss.ppg as ppg_trend,
                   pgs.rolling_game_score - pss.bpm as gs_trend
            FROM player_game_stats pgs
            JOIN player_season_stats pss
              ON pss.player_id = pgs.player_id AND pss.season = %(season)s
            JOIN games g ON g.id = pgs.game_id
            WHERE g.season = %(season)s
              AND pgs.rolling_ppg IS NOT NULL
              AND pss.games_played >= 10
              AND pss.minutes_per_game >= 10
            ORDER BY pgs.player_id, g.game_date DESC
        )
        SELECT team_id,
               SUM(rolling_game_score * minutes_per_game * games_played)
                   / NULLIF(SUM(minutes_per_game * games_played), 0) as w_rolling_gs,
               SUM(rolling_ts_pct * minutes_per_game * games_played)
                   / NULLIF(SUM(minutes_per_game * games_played), 0) as w_rolling_ts,
               SUM(ppg_trend * minutes_per_game * games_played)
                   / NULLIF(SUM(minutes_per_game * games_played), 0) as w_ppg_trend,
               SUM(gs_trend * minutes_per_game * games_played)
                   / NULLIF(SUM(minutes_per_game * games_played), 0) as w_gs_trend
        FROM latest_rolling
        GROUP BY team_id
        """,
        engine,
        params={"season": SEASON},
    )


def load_games(engine) -> pd.DataFrame:
    """Load all completed games with scores and team IDs."""
    return pd.read_sql(
        """
        SELECT g.id as game_id, g.game_date,
               g.home_team_id, g.away_team_id,
               g.home_score, g.away_score,
               g.is_neutral_site
        FROM games g
        WHERE g.season = %(season)s
          AND g.home_score IS NOT NULL
          AND g.away_score IS NOT NULL
          AND g.home_team_id IS NOT NULL
          AND g.away_team_id IS NOT NULL
        """,
        engine,
        params={"season": SEASON},
    )


def build_feature_matrix(engine) -> pd.DataFrame:
    """
    Build the full feature matrix for game outcome prediction.

    Each row = one game. Features are differences (home - away) of team/roster stats,
    plus a venue indicator.

    Targets:
      - home_win: 1 if home team won, 0 otherwise
      - margin: home_score - away_score
    """
    games = load_games(engine)
    team_stats = load_team_season_stats(engine)
    roster_agg = load_roster_aggregates(engine)
    rolling_form = load_rolling_form(engine)

    # Merge team stats for home and away
    team_cols = [c for c in team_stats.columns if c not in ("team_id", "conference")]
    roster_cols = [c for c in roster_agg.columns if c != "team_id"]
    rolling_cols = [c for c in rolling_form.columns if c != "team_id"]

    df = games.copy()

    # Home team stats
    df = df.merge(
        team_stats.rename(columns={c: f"home_{c}" for c in team_cols}),
        left_on="home_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])
    # Keep conference for matchup feature
    df = df.merge(
        team_stats[["team_id", "conference"]].rename(columns={"conference": "home_conference"}),
        left_on="home_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    df = df.merge(
        roster_agg.rename(columns={c: f"home_{c}" for c in roster_cols}),
        left_on="home_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    df = df.merge(
        rolling_form.rename(columns={c: f"home_{c}" for c in rolling_cols}),
        left_on="home_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    # Away team stats
    df = df.merge(
        team_stats.rename(columns={c: f"away_{c}" for c in team_cols}),
        left_on="away_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])
    df = df.merge(
        team_stats[["team_id", "conference"]].rename(columns={"conference": "away_conference"}),
        left_on="away_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    df = df.merge(
        roster_agg.rename(columns={c: f"away_{c}" for c in roster_cols}),
        left_on="away_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    df = df.merge(
        rolling_form.rename(columns={c: f"away_{c}" for c in rolling_cols}),
        left_on="away_team_id",
        right_on="team_id",
        how="left",
    ).drop(columns=["team_id"])

    # Venue indicator: +1 home, -1 away, 0 neutral
    df["venue"] = df["is_neutral_site"].apply(lambda x: 0 if x else 1)

    # Conference matchup: 1 if same conference (conference game), 0 otherwise
    df["is_conference_game"] = (
        df["home_conference"].notna()
        & (df["home_conference"] == df["away_conference"])
    ).astype(int)

    # Win percentage
    df["home_win_pct"] = df["home_wins"] / (df["home_wins"] + df["home_losses"]).replace(0, 1)
    df["away_win_pct"] = df["away_wins"] / (df["away_wins"] + df["away_losses"]).replace(0, 1)
    df["diff_win_pct"] = df["home_win_pct"] - df["away_win_pct"]

    # Difference features (home - away)
    diff_pairs = {
        # Team-level efficiency
        "adj_offense": "adj_offense",
        "adj_defense": "adj_defense",
        "adj_efficiency_margin": "adj_efficiency_margin",
        "adj_tempo": "adj_tempo",
        # Four factors
        "effective_fg_pct": "effective_fg_pct",
        "turnover_pct": "turnover_pct",
        "off_rebound_pct": "off_rebound_pct",
        "ft_rate": "ft_rate",
        "opp_effective_fg_pct": "opp_effective_fg_pct",
        "opp_turnover_pct": "opp_turnover_pct",
        "def_rebound_pct": "def_rebound_pct",
        "opp_ft_rate": "opp_ft_rate",
        # Power metrics
        "sos": "sos",
        "elo": "elo",
        "point_diff": "point_diff",
        "pythag_win_pct": "pythag_win_pct",
        "road_win_pct": "road_win_pct",
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
        # Roster advanced ratings
        "w_obpm": "w_obpm",
        "w_dbpm": "w_dbpm",
        "w_ortg": "w_ortg",
        "w_drtg": "w_drtg",
        "w_net_rating": "w_net_rating",
        # Roster rate stats
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
        # Rolling form (recent trend)
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

    # For defense, lower is better — flip the sign so positive = better for home
    if "diff_adj_defense" in df.columns:
        df["diff_adj_defense"] = -df["diff_adj_defense"]
    if "diff_opp_effective_fg_pct" in df.columns:
        df["diff_opp_effective_fg_pct"] = -df["diff_opp_effective_fg_pct"]
    if "diff_opp_ft_rate" in df.columns:
        df["diff_opp_ft_rate"] = -df["diff_opp_ft_rate"]
    if "diff_w_drtg" in df.columns:
        df["diff_w_drtg"] = -df["diff_w_drtg"]  # lower DRTG = better defense

    # Targets
    df["margin"] = df["home_score"] - df["away_score"]
    df["home_win"] = (df["margin"] > 0).astype(int)

    # Select feature columns
    feature_cols = (
        ["venue", "is_conference_game", "diff_win_pct"]
        + [c for c in df.columns if c.startswith("diff_") and c != "diff_win_pct"]
    )

    return df, feature_cols


if __name__ == "__main__":
    engine = get_engine()
    df, feature_cols = build_feature_matrix(engine)
    print(f"Games: {len(df)}")
    print(f"Features: {len(feature_cols)}")
    print(f"Home win rate: {df['home_win'].mean():.3f}")
    print(f"Avg margin: {df['margin'].mean():.1f}")
    print(f"\nFeature columns:\n{feature_cols}")
    print(f"\nNull counts:\n{df[feature_cols].isnull().sum().to_string()}")
    print(f"\nSample:\n{df[feature_cols + ['margin', 'home_win']].head()}")
