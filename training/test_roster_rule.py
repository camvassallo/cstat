"""`pregame_roster` keeps a game's own box score out of its roster (#467).

One team, four games. A plays 1, 2 and 4, B plays only 1, C plays 3 and 4.
"""

import numpy as np
import pandas as pd

from features import pregame_roster


def schedule():
    rows = [
        ("A", "g1", 1, 10.0),
        ("B", "g1", 1, 20.0),
        ("A", "g2", 2, 30.0),
        ("C", "g3", 3, 40.0),
        ("A", "g4", 4, 50.0),
        ("C", "g4", 4, 60.0),
    ]
    df = pd.DataFrame(rows, columns=["player_id", "game_id", "day", "m"])
    df["team_id"] = "T"
    df["season"] = 2026
    df["game_date"] = pd.Timestamp("2026-01-01") + pd.to_timedelta(df["day"], "D")
    by_player = df.groupby("player_id")
    df["post_minutes"] = by_player["m"].expanding().mean().reset_index(level=0, drop=True)
    df["cum_minutes"] = df.groupby("player_id")["post_minutes"].shift(1)
    df["post_rolling_ppg"] = df["m"]
    df["rolling_ppg"] = by_player["m"].shift(1)
    df["cum_games"] = by_player.cumcount().astype(float)
    df["minutes"] = df["m"]
    return df.drop(columns=["day", "m"])


def members(frame):
    ordered = frame.sort_values(["game_id", "player_id"])
    return list(zip(ordered["game_id"], ordered["player_id"]))


def test_box_rule_changes_nothing_but_the_helper_columns():
    player_cum = schedule()
    out = pregame_roster(player_cum, "box")
    assert len(out) == len(player_cum)
    assert not [c for c in out.columns if c.startswith("post_")]


def test_season_rule_is_everyone_seen_before_the_game():
    player_cum = schedule()
    assert members(pregame_roster(player_cum, "season")) == [
        ("g2", "A"), ("g2", "B"),
        ("g3", "A"), ("g3", "B"),
        ("g4", "A"), ("g4", "B"), ("g4", "C"),
    ]


def test_a_player_debuting_in_a_game_is_not_on_its_roster():
    player_cum = schedule()
    assert ("g3", "C") not in members(pregame_roster(player_cum, "season"))


def test_stats_run_through_the_players_last_game():
    player_cum = schedule()
    out = pregame_roster(player_cum, "season")
    a = out[(out["game_id"] == "g4") & (out["player_id"] == "A")].iloc[0]
    assert a["cum_games"] == 2
    assert a["cum_minutes"] == 20.0
    assert a["rolling_ppg"] == 30.0
    assert np.isnan(a["minutes"])


def test_recent_rule_drops_a_player_gone_for_longer_than_the_window():
    player_cum = schedule()
    assert members(pregame_roster(player_cum, "recent1")) == [
        ("g2", "A"), ("g2", "B"), ("g3", "A"), ("g4", "C"),
    ]


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for test in tests:
        test()
    print(f"{len(tests)} roster rule tests passed")
