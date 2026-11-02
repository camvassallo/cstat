//! Read-only research tools for the drafter: the questions a reader would
//! ask the site before writing about a game or a player. Is this a career
//! night or a habit? Who does he play like? What does the model weigh in this
//! matchup, and how did the last meeting go?
//!
//! Every tool is a GET against the public API (`Site`), trimmed before it goes
//! back to the model: the raw responses run from a few KB to over a megabyte,
//! most of it fields no tweet needs. Everything a tool returns is also added
//! to the fact-check corpus, so a number Claude found by researching passes
//! and a number it made up still gets flagged.

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use super::site::Site;

/// Largest tool result handed back, in characters. A trimmed response is
/// well under this; it only bites if an endpoint grows.
const RESULT_CHARS: usize = 14_000;

pub struct Research<'a> {
    pub site: &'a Site,
    /// The season the item is about; tools default to it.
    pub season: i32,
}

/// The tool definitions sent with every drafting request.
pub fn definitions() -> Value {
    let season = json!({"type": "integer", "description": "Season by its end year (2026 = 2025-26). Defaults to the item's season."});
    json!([
        {
            "name": "search_players",
            "description": "Find players by name (case-insensitive substring). Returns ids, team, class, basic per-game stats, CAM and archetype. Use it to get a player_id.",
            "input_schema": {"type": "object", "properties": {
                "name": {"type": "string"}, "season": season
            }, "required": ["name"]}
        },
        {
            "name": "player_profile",
            "description": "One player's season: per-game and rate stats, percentiles among all D-I players (0-1), CAM with its offense/defense halves (CAMO/CAMD), archetype, next-season projection, season highs, and the last 10 games. Use it to tell a career night from a habit, or find a trend.",
            "input_schema": {"type": "object", "properties": {
                "player_id": {"type": "string"}, "season": season
            }, "required": ["player_id"]}
        },
        {
            "name": "player_career",
            "description": "A player's season-by-season arc across every year and school: team, class, per-game stats, CAM and archetype per season. Use it for growth stories, transfers, and 'how far he has come'.",
            "input_schema": {"type": "object", "properties": {
                "player_id": {"type": "string"}
            }, "required": ["player_id"]}
        },
        {
            "name": "similar_players",
            "description": "The players statistically most like this one. With cross_year, searches every season, which is how you find a historical comp ('plays like 2019 so-and-so').",
            "input_schema": {"type": "object", "properties": {
                "player_id": {"type": "string"}, "season": season,
                "cross_year": {"type": "boolean"}
            }, "required": ["player_id"]}
        },
        {
            "name": "team_profile",
            "description": "One team's season: ratings and ranks (adjusted efficiency margin, offense, defense, tempo, strength of schedule, four factors), every game with scores and our pregame win probability, the rotation with CAM and archetypes, and the archetype mix.",
            "input_schema": {"type": "object", "properties": {
                "team_id": {"type": "string"}, "season": season
            }, "required": ["team_id"]}
        },
        {
            "name": "team_lineups",
            "description": "A team's most-used five-man lineups with minutes and net rating per 100 possessions.",
            "input_schema": {"type": "object", "properties": {
                "team_id": {"type": "string"}, "season": season
            }, "required": ["team_id"]}
        },
        {
            "name": "team_coach",
            "description": "A team's head coach and their record against expectation: career grade, and season by season how the team did versus its preseason projection.",
            "input_schema": {"type": "object", "properties": {
                "team_id": {"type": "string"}
            }, "required": ["team_id"]}
        },
        {
            "name": "team_rankings",
            "description": "Our team ratings, best first. sort is one of adj_efficiency_margin (default), adj_offense, adj_defense, adj_tempo, sos, elo, wins, point_diff.",
            "input_schema": {"type": "object", "properties": {
                "season": season, "sort": {"type": "string"},
                "top": {"type": "integer", "description": "How many teams, default 25, max 60."}
            }}
        },
        {
            "name": "predict_matchup",
            "description": "Our model on any matchup: projected score, win probability, the groups of factors driving it and the biggest single factors, and recent prior meetings with scores. Teams by name or id. Different seasons per side make a cross-era what-if ('2026 Duke vs 2015 Kentucky').",
            "input_schema": {"type": "object", "properties": {
                "home": {"type": "string"}, "away": {"type": "string"},
                "venue": {"type": "string", "enum": ["home", "neutral"]},
                "season": season,
                "home_season": {"type": "integer"}, "away_season": {"type": "integer"}
            }, "required": ["home", "away"]}
        },
        {
            "name": "conference_standings",
            "description": "A conference's teams with records, ratings, ranks and expected wins. code is uppercase, e.g. BIG10, SEC, ACC, BIG12, BIGEAST.",
            "input_schema": {"type": "object", "properties": {
                "code": {"type": "string"}, "season": season
            }, "required": ["code"]}
        },
        {
            "name": "submit_drafts",
            "description": "Hand in your three finished drafts. Call this exactly once, when your research is done.",
            "strict": true,
            "input_schema": {"type": "object", "properties": {
                "drafts": {"type": "array", "items": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "The tweet."},
                    "angle": {"type": "string", "description": "The angle, in a few words."},
                    "source": {"type": "string", "description": "Where the key numbers came from, in one short line, so a proofreader can check them."}
                }, "required": ["text", "angle", "source"], "additionalProperties": false}}
            }, "required": ["drafts"], "additionalProperties": false}
        }
    ])
}

impl Research<'_> {
    /// Run one tool. The result is what the model sees, already trimmed.
    pub async fn call(&self, name: &str, input: &Value) -> anyhow::Result<Value> {
        let s = |k: &str| input[k].as_str().map(str::trim).filter(|v| !v.is_empty());
        let id = |k: &str| -> anyhow::Result<&str> {
            s(k).with_context(|| format!("`{k}` is required"))
        };
        let season = input["season"]
            .as_i64()
            .map(|v| v as i32)
            .unwrap_or(self.season);
        let out = match name {
            "search_players" => {
                let q = id("name")?;
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/players?search={}&season={season}&limit=8",
                        enc(q)
                    ))
                    .await?;
                json!({ "season": season, "players": list(&v["players"], &[
                    "player_id", "name", "team_id", "team_name", "conference", "class_year",
                    "games_played", "minutes_per_game", "ppg", "rpg", "apg", "campom", "primary_class",
                ]) })
            }
            "player_profile" => {
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/players/{}?season={season}",
                        id("player_id")?
                    ))
                    .await?;
                player_profile(&v)
            }
            "player_career" => {
                let v: Value = self
                    .site
                    .get_json(&format!("/api/players/{}/progression", id("player_id")?))
                    .await?;
                player_career(&v)
            }
            "similar_players" => {
                let cross = input["cross_year"].as_bool().unwrap_or(false);
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/players/{}/similar?season={season}&k=8&cross_year={cross}",
                        id("player_id")?
                    ))
                    .await?;
                json!({ "players": list(&v["players"], &[
                    "player_id", "name", "team_name", "season", "primary_class", "secondary_class",
                    "similarity", "is_self",
                ]) })
            }
            "team_profile" => {
                let v: Value = self
                    .site
                    .get_json(&format!("/api/teams/{}?season={season}", id("team_id")?))
                    .await?;
                team_profile(&v)
            }
            "team_lineups" => {
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/teams/{}/lineups?season={season}",
                        id("team_id")?
                    ))
                    .await?;
                let lineups: Vec<Value> = v["lineups"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(6)
                    .map(|l| {
                        pick(
                            l,
                            &[
                                "player_names",
                                "minutes",
                                "ortg",
                                "drtg",
                                "net_rtg",
                                "plus_minus",
                            ],
                        )
                    })
                    .collect();
                json!({ "season": v["season"], "lineups": lineups })
            }
            "team_coach" => {
                let c: Value = self
                    .site
                    .get_json(&format!("/api/teams/{}/coach", id("team_id")?))
                    .await?;
                let coach = &c["coach"];
                let mut out = pick(
                    coach,
                    &[
                        "name",
                        "cae_shrunk",
                        "ci_low",
                        "ci_high",
                        "n_seasons",
                        "first_season",
                        "last_season",
                        "is_new_hc",
                    ],
                );
                out["cae_means"] = json!(
                    "coach above expectation: adjusted efficiency margin versus the roster's preseason projection, per season, shrunk toward zero"
                );
                if let Some(cid) = coach["coach_id"].as_str() {
                    let d: Value = self.site.get_json(&format!("/api/coaches/{cid}")).await?;
                    out["seasons"] = list(
                        &d["seasons"],
                        &[
                            "season",
                            "team_name",
                            "actual_adjem",
                            "projection",
                            "cae_raw",
                            "is_new_hc",
                        ],
                    );
                }
                out
            }
            "team_rankings" => {
                let sort = s("sort").unwrap_or("adj_efficiency_margin");
                let top = input["top"].as_u64().unwrap_or(25).min(60) as usize;
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/teams/rankings?season={season}&sort={}",
                        enc(sort)
                    ))
                    .await?;
                let teams: Vec<Value> = v["teams"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(top)
                    .map(|t| {
                        pick(
                            t,
                            &[
                                "rank",
                                "team_id",
                                "name",
                                "conference",
                                "wins",
                                "losses",
                                "adj_efficiency_margin",
                                "adj_offense",
                                "adj_offense_rank",
                                "adj_defense",
                                "adj_defense_rank",
                                "adj_tempo",
                                "sos_rank",
                            ],
                        )
                    })
                    .collect();
                if teams.is_empty() {
                    bail!(
                        "no ratings for season {season} yet; try season {}",
                        season - 1
                    );
                }
                json!({ "season": season, "sort": sort, "teams": teams })
            }
            "predict_matchup" => {
                let mut q = format!("home={}&away={}", enc(id("home")?), enc(id("away")?));
                q.push_str(&format!("&venue={}", s("venue").unwrap_or("home")));
                q.push_str(&format!("&season={season}"));
                for k in ["home_season", "away_season"] {
                    if let Some(v) = input[k].as_i64() {
                        q.push_str(&format!("&{k}={v}"));
                    }
                }
                let v: Value = self.site.get_json(&format!("/api/predict?{q}")).await?;
                predict(&v)
            }
            "conference_standings" => {
                let v: Value = self
                    .site
                    .get_json(&format!(
                        "/api/conferences/{}?season={season}",
                        enc(&id("code")?.to_uppercase())
                    ))
                    .await?;
                let mut out = pick(
                    &v,
                    &[
                        "conference",
                        "season",
                        "strength_rank",
                        "mean_adj_em",
                        "non_conference_wins",
                        "non_conference_losses",
                    ],
                );
                out["teams"] = list(
                    &v["teams"],
                    &[
                        "team_id",
                        "team_name",
                        "wins",
                        "losses",
                        "conference_wins",
                        "conference_losses",
                        "adj_em",
                        "adj_em_rank",
                        "expected_wins",
                        "expected_conference_wins",
                    ],
                );
                out
            }
            other => bail!("unknown tool `{other}`"),
        };
        Ok(out)
    }
}

/// The result as text for a `tool_result`, capped.
pub fn render(v: &Value) -> String {
    let s = v.to_string();
    if s.len() <= RESULT_CHARS {
        return s;
    }
    let cut: String = s.chars().take(RESULT_CHARS).collect();
    format!("{cut}… [truncated]")
}

fn player_profile(v: &Value) -> Value {
    let mut out = json!({
        "player": pick(&v["player"], &[
            "id", "name", "team_id", "team_name", "conference", "class_year", "height_inches",
            "position",
        ]),
        "season_stats": pick(&v["season_stats"], &[
            "season", "games_played", "minutes_per_game", "ppg", "rpg", "apg", "spg", "bpg",
            "topg", "fg_pct", "tp_pct", "ft_pct", "true_shooting_pct", "effective_fg_pct",
            "usage_rate", "ast_pct", "orb_pct", "drb_pct", "stl_pct", "blk_pct", "ft_rate",
        ]),
        "percentiles_among_all_d1_players": slim(&v["percentiles"]),
        "cam": pick(&v["torvik_stats"], &["campom", "campom_pct", "campom_o", "campom_d"]),
        "archetype": pick(&v["archetype"], &[
            "primary_class", "primary_score", "secondary_class", "secondary_score", "provisional",
        ]),
        "next_season_projection": pick(&v["trajectory"], &[
            "projected_mean", "projected_lower", "projected_upper", "target_season",
        ]),
    });
    let log: Vec<&Value> = v["game_log"].as_array().into_iter().flatten().collect();
    let line_keys = [
        "game_date",
        "opponent_name",
        "is_home",
        "minutes",
        "points",
        "total_rebounds",
        "assists",
        "steals",
        "blocks",
        "turnovers",
        "fgm",
        "fga",
        "tpm",
        "tpa",
        "ftm",
        "fta",
        "game_score",
    ];
    // Season highs, so "career night or habit" is one lookup.
    let mut highs = Map::new();
    for stat in [
        "points",
        "total_rebounds",
        "assists",
        "steals",
        "blocks",
        "tpm",
        "game_score",
    ] {
        if let Some(best) = log.iter().filter(|g| g[stat].is_number()).max_by(|a, b| {
            a[stat]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b[stat].as_f64().unwrap_or(0.0))
        }) {
            highs.insert(
                stat.into(),
                json!({ "value": slim(&best[stat]), "date": best["game_date"], "opponent": best["opponent_name"] }),
            );
        }
    }
    out["season_highs"] = Value::Object(highs);
    out["games_logged"] = json!(log.len());
    // The log is oldest first; the last ten are the recent form.
    let recent: Vec<Value> = log
        .iter()
        .rev()
        .take(10)
        .map(|g| pick(g, &line_keys))
        .collect();
    out["last_10_games_newest_first"] = json!(recent);
    out
}

fn player_career(v: &Value) -> Value {
    let seasons: Vec<Value> = v["seasons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let ss = &s["season_stats"];
            json!({
                "season": s["season"],
                "team": s["team_name"],
                "class": s["class_year"],
                "games": ss["games_played"],
                "mpg": slim(&ss["minutes_per_game"]),
                "ppg": slim(&ss["ppg"]),
                "rpg": slim(&ss["rpg"]),
                "apg": slim(&ss["apg"]),
                "ts_pct": slim(&ss["true_shooting_pct"]),
                "cam": slim(&s["torvik_stats"]["campom"]),
                "archetype": s["archetype"]["primary_class"],
            })
        })
        .collect();
    let name = v["seasons"]
        .as_array()
        .and_then(|a| a.last())
        .map(|s| s["name"].clone())
        .unwrap_or(Value::Null);
    json!({
        "name": name,
        "seasons": seasons,
        "next_season_projection": pick(&v["trajectory"], &[
            "projected_mean", "projected_lower", "projected_upper", "target_season",
        ]),
    })
}

fn team_profile(v: &Value) -> Value {
    let team = pick(
        &v["team"],
        &[
            "id",
            "name",
            "season",
            "conference",
            "wins",
            "losses",
            "adj_efficiency_margin",
            "adj_efficiency_margin_rank",
            "adj_offense",
            "adj_offense_rank",
            "adj_defense",
            "adj_defense_rank",
            "adj_tempo",
            "adj_tempo_rank",
            "sos",
            "sos_rank",
            "effective_fg_pct",
            "effective_fg_pct_rank",
            "turnover_pct",
            "turnover_pct_rank",
            "off_rebound_pct",
            "off_rebound_pct_rank",
            "ft_rate",
            "ft_rate_rank",
            "opp_effective_fg_pct",
            "opp_effective_fg_pct_rank",
            "point_diff",
            "road_win_pct",
            "elo_rank",
        ],
    );
    let games: Vec<Value> = v["schedule"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            let site = if g["is_neutral"] == true {
                "N"
            } else if g["is_home"] == true {
                "H"
            } else {
                "A"
            };
            let mut row = json!({
                "date": g["game_date"], "opponent": g["opponent_name"], "site": site,
                "conference": g["is_conference"],
            });
            match (g["team_score"].as_i64(), g["opponent_score"].as_i64()) {
                (Some(t), Some(o)) => {
                    row["result"] = json!(format!("{} {t}-{o}", if t > o { "W" } else { "L" }));
                    if g["is_pre_game_projection"] == true {
                        row["our_pregame_win_prob"] = slim(&g["projected_win_prob"]);
                    }
                }
                _ => {
                    row["our_win_prob"] = slim(&g["projected_win_prob"]);
                    row["projected_margin"] = slim(&g["projected_margin"]);
                }
            }
            row
        })
        .collect();
    let mut roster: Vec<&Value> = v["roster"].as_array().into_iter().flatten().collect();
    roster.sort_by(|a, b| {
        b["minutes_per_game"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["minutes_per_game"].as_f64().unwrap_or(0.0))
    });
    let rotation: Vec<Value> = roster
        .iter()
        .take(10)
        .map(|p| {
            pick(
                p,
                &[
                    "player_id",
                    "name",
                    "class_year",
                    "games_played",
                    "minutes_per_game",
                    "ppg",
                    "rpg",
                    "apg",
                    "true_shooting_pct",
                    "usage_rate",
                    "campom",
                    "campom_o",
                    "campom_d",
                    "primary_class",
                ],
            )
        })
        .collect();
    json!({
        "team": team,
        "total_teams": v["total_teams"],
        "games": games,
        "rotation_by_minutes": rotation,
        "archetype_mix": slim(&v["archetype_distribution"]),
    })
}

fn predict(v: &Value) -> Value {
    let mut out = pick(
        v,
        &[
            "home_team",
            "away_team",
            "season",
            "venue",
            "predicted_winner",
            "predicted_margin",
            "home_win_probability",
            "predicted_home_score",
            "predicted_away_score",
            "predicted_total",
            "prediction_basis",
        ],
    );
    out["factor_groups"] = list(&v["contributions_by_group"], &["group", "contribution"]);
    let mut feats: Vec<&Value> = v["feature_contributions"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    feats.sort_by(|a, b| {
        b["contribution"]
            .as_f64()
            .unwrap_or(0.0)
            .abs()
            .total_cmp(&a["contribution"].as_f64().unwrap_or(0.0).abs())
    });
    out["biggest_factors"] = json!(
        feats
            .iter()
            .take(8)
            .map(|f| pick(f, &["label", "value", "contribution"]))
            .collect::<Vec<_>>()
    );
    out["factor_sign"] = json!("contributions are points of margin toward the home team");
    out["prior_meetings"] = json!(
        v["prior_meetings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let h = &m["headline"];
                pick(
                    h,
                    &[
                        "game_date",
                        "home_team_name",
                        "home_score",
                        "away_team_name",
                        "away_score",
                        "is_neutral_site",
                        "is_postseason",
                    ],
                )
            })
            .collect::<Vec<_>>()
    );
    out
}

/// Only `keys` of `v`, nulls dropped, floats rounded.
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for k in keys {
        let x = &v[*k];
        if !x.is_null() {
            m.insert((*k).to_string(), slim(x));
        }
    }
    Value::Object(m)
}

fn list(v: &Value, keys: &[&str]) -> Value {
    json!(
        v.as_array()
            .into_iter()
            .flatten()
            .map(|x| pick(x, keys))
            .collect::<Vec<_>>()
    )
}

/// Round floats to three decimals and drop nulls, recursively: the model
/// quotes what it sees, so it should see what a tweet would print.
pub fn slim(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => {
            let x = n.as_f64().unwrap_or(0.0);
            json!((x * 1000.0).round() / 1000.0)
        }
        Value::Array(a) => Value::Array(a.iter().map(slim).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(_, x)| !x.is_null())
                .map(|(k, x)| (k.clone(), slim(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Percent-encode a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_rounds_and_drops_nulls() {
        let v = json!({"a": 0.123456, "b": null, "c": [1.98765, 3], "d": "x"});
        assert_eq!(slim(&v), json!({"a": 0.123, "c": [1.988, 3], "d": "x"}));
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(enc("Texas A&M"), "Texas%20A%26M");
        assert_eq!(enc("Duke"), "Duke");
    }

    #[test]
    fn profiles_find_season_highs_and_recent_form() {
        let v = json!({
            "player": {"id": "p", "name": "A"},
            "game_log": [
                {"game_date": "2026-01-01", "opponent_name": "X", "points": 12, "total_rebounds": 4},
                {"game_date": "2026-01-05", "opponent_name": "Y", "points": 31, "total_rebounds": 9},
                {"game_date": "2026-01-09", "opponent_name": "Z", "points": 18, "total_rebounds": 11}
            ]
        });
        let p = player_profile(&v);
        assert_eq!(p["season_highs"]["points"]["value"], 31);
        assert_eq!(p["season_highs"]["total_rebounds"]["opponent"], "Z");
        assert_eq!(p["last_10_games_newest_first"][0]["opponent_name"], "Z");
    }

    #[test]
    fn every_tool_has_a_schema() {
        for t in definitions().as_array().unwrap() {
            assert!(t["name"].is_string());
            assert_eq!(t["input_schema"]["type"], "object", "{}", t["name"]);
        }
    }
}
