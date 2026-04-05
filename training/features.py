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
    """Load team season stats with four factors and adjusted ratings."""
    return pd.read_sql(
        """
        SELECT team_id, wins, losses,
               adj_offense, adj_defense, adj_efficiency_margin, adj_tempo,
               effective_fg_pct, turnover_pct, off_rebound_pct, ft_rate,
               opp_effective_fg_pct, opp_turnover_pct, def_rebound_pct, opp_ft_rate,
               sos, sos_rank
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
                   usage_rate, bpm, player_sos,
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

               -- Minutes-weighted averages
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

               -- Star player stats (top player by total minutes)
               MAX(ppg) as star_ppg,
               MAX(bpm) as star_bpm,

               -- Depth: std dev of minutes distribution
               STDDEV(minutes_per_game) as minutes_stddev
        FROM qualified
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

    # Merge team stats for home and away
    team_cols = [c for c in team_stats.columns if c != "team_id"]
    roster_cols = [c for c in roster_agg.columns if c != "team_id"]

    df = games.copy()

    # Home team stats
    df = df.merge(
        team_stats.rename(columns={c: f"home_{c}" for c in team_cols}),
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

    # Away team stats
    df = df.merge(
        team_stats.rename(columns={c: f"away_{c}" for c in team_cols}),
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

    # Venue indicator: +1 home, -1 away, 0 neutral
    df["venue"] = df["is_neutral_site"].apply(lambda x: 0 if x else 1)

    # Difference features (home - away)
    diff_pairs = {
        # Team-level
        "adj_offense": "adj_offense",
        "adj_defense": "adj_defense",
        "adj_efficiency_margin": "adj_efficiency_margin",
        "adj_tempo": "adj_tempo",
        "effective_fg_pct": "effective_fg_pct",
        "turnover_pct": "turnover_pct",
        "off_rebound_pct": "off_rebound_pct",
        "ft_rate": "ft_rate",
        "opp_effective_fg_pct": "opp_effective_fg_pct",
        "opp_turnover_pct": "opp_turnover_pct",
        "def_rebound_pct": "def_rebound_pct",
        "opp_ft_rate": "opp_ft_rate",
        "sos": "sos",
        "wins": "wins",
        "losses": "losses",
        # Roster-level
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
        "star_ppg": "star_ppg",
        "star_bpm": "star_bpm",
        "minutes_stddev": "minutes_stddev",
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

    # Targets
    df["margin"] = df["home_score"] - df["away_score"]
    df["home_win"] = (df["margin"] > 0).astype(int)

    # Select feature columns
    feature_cols = ["venue"] + [c for c in df.columns if c.startswith("diff_")]

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
