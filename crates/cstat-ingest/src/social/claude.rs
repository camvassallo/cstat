//! Drafting through the Claude Messages API.
//!
//! There is no official Rust SDK, so this is a plain HTTPS call. A draft is a
//! short agentic loop: Claude gets the item's facts and a set of read-only
//! research tools over the site's API (`research.rs`), looks around the way a
//! curious reader would, and hands in its drafts through a `submit_drafts`
//! tool with a strict schema. Triage is a single structured-output call. Both
//! opt into the server-side refusal fallback so a false-positive decline is
//! retried on another model inside the same call.

use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use super::research::{self, Research};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// Research budget per item. Typical runs use four to eight calls; the cap
/// only stops a loop that has lost the thread.
const MAX_TOOL_CALLS: usize = 12;
/// Request rounds per item, counting the one that submits.
const MAX_ROUNDS: usize = 16;

/// The standing brief. Kept constant (no dates, no per-run values) so it is a
/// stable, cacheable prefix.
const SYSTEM: &str = "\
You write draft tweets for Camalytics, a college basketball analytics site. \
A person proofreads every draft before anything is posted, so give them real \
options, including a bold one.

Voice: a sharp, funny college hoops nerd with a spreadsheet. Playful, specific, \
a little irreverent, never mean. Lead with the detail that makes a fan stop \
scrolling. Wordplay is welcome, cliches are not. No press-release tone, no \
\"let's go\", no exclamation-mark pileups. Our player archetypes are D&D classes \
(Wizard, Sorcerer, Warlock, Bard, Ranger, Barbarian, Paladin, Monk, Cleric, \
Druid, Rogue, Fighter); lean on them for flavor when they fit, like a Paladin \
who suddenly shot like a Ranger.

Research first. Before writing, dig through the site with your tools the way a \
curious fan would: is this a career night or a habit (season highs, last ten \
games, career arc)? Who does he play like, this year or any year? What does our \
model weigh in this matchup, and how did the last meeting go? How has the team \
done against our pregame numbers lately? Look for the surprising thing: a \
streak, a contrast, a comp, a turnaround, a number that cuts against the \
obvious story. Four to eight tool calls is typical; stop when you have a story. \
Then call submit_drafts once, with three drafts on clearly different angles.

Hard rules:
- Every number must come from the FACTS or from a tool result in this \
conversation, copied as it appears there. You may count what is in front of \
you (\"his third 20-point game in five\") and say which is bigger, but do not \
compute new averages, percentages or differences. Never add a stat, record, \
ranking, injury or bit of history from your own memory; if you did not look it \
up here, it does not go in.
- Percentiles in tool results are 0-1: 0.97 means 97th percentile among all \
D-I players.
- The player value metric is CAM, with offense and defense halves CAMO and CAMD. \
Never write CamPom, campom or GBPM.
- Never name another analytics site, ranking or data provider (KenPom, Torvik, \
EvanMiya, NET, AP poll, NatStat, 247Sports and the like).
- A rank is the Camalytics ranking: write \"our No. 12\" or \"No. 12 in our \
ratings\", never a bare \"No. 12\" that reads as the AP poll.
- Win probabilities are ours and are odds, not promises: \"we give Gonzaga a 58% \
chance\", never \"Gonzaga will win\".
- Each draft is a single tweet of at most 270 characters unless the brief asks \
for a list. At most one emoji, no hashtags, no links (the poster adds the link), \
no @-mentions.
- In each draft's `source`, name where its key numbers came from in one short \
line, so the proofreader can check them.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub angle: String,
    /// Where the numbers came from, for the proofreader.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    /// Every input token the model read, cached or not.
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// The part of `input_tokens` served from the prompt cache, billed at a
    /// tenth of the input rate. A research loop resends its history every
    /// round, so this is most of it.
    pub cache_read_tokens: i64,
}

impl Usage {
    fn add(&mut self, reply: &Value) {
        let u = &reply["usage"];
        let read = u["cache_read_input_tokens"].as_i64().unwrap_or(0);
        self.input_tokens += u["input_tokens"].as_i64().unwrap_or(0)
            + read
            + u["cache_creation_input_tokens"].as_i64().unwrap_or(0);
        self.cache_read_tokens += read;
        self.output_tokens += u["output_tokens"].as_i64().unwrap_or(0);
    }
}

/// A finished draft run: the drafts, what it cost, and what the model looked
/// up along the way (each call for the Slack trail, each result for the
/// fact-check).
pub struct Drafted {
    pub drafts: Vec<Draft>,
    pub usage: Usage,
    pub trail: Vec<String>,
    pub findings: Vec<Value>,
}

pub struct Drafter {
    http: reqwest::Client,
    key: String,
    pub model: String,
}

impl Drafter {
    /// `None` when `ANTHROPIC_API_KEY` is unset; callers fall back to
    /// template drafts so the pipeline still runs end to end.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())?;
        let model = std::env::var("SOCIAL_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Some(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client builds"),
            key,
            model,
        })
    }

    /// Research the item with the site's tools, then three drafts.
    pub async fn draft(
        &self,
        brief: &str,
        facts: &Value,
        tools: &Research<'_>,
    ) -> anyhow::Result<Drafted> {
        let definitions = research::definitions();
        let mut messages = vec![json!({
            "role": "user",
            "content": format!(
                "BRIEF:\n{brief}\n\nFACTS (ids in here work with the tools):\n{}",
                serde_json::to_string_pretty(facts)?
            )
        })];
        let mut usage = Usage::default();
        let mut trail = Vec::new();
        let mut findings = Vec::new();
        let mut calls = 0;
        let mut nudged = false;

        for _ in 0..MAX_ROUNDS {
            let body = json!({
                "model": self.model,
                "max_tokens": 16000,
                "system": SYSTEM,
                "fallbacks": "default",
                "cache_control": {"type": "ephemeral"},
                "output_config": {"effort": "medium"},
                "tools": definitions,
                "tool_choice": {"type": "auto"},
                "messages": messages,
            });
            let reply = self.post(&body).await?;
            usage.add(&reply);
            let content = reply["content"].clone();
            // Append the assistant turn exactly as returned, thinking blocks
            // included: editing it would invalidate them on the next round.
            messages.push(json!({"role": "assistant", "content": content}));

            let uses: Vec<&Value> = content
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "tool_use")
                .collect();
            if let Some(submit) = uses.iter().find(|b| b["name"] == "submit_drafts") {
                let drafts: Vec<Draft> = serde_json::from_value(submit["input"]["drafts"].clone())
                    .context("submit_drafts input did not match its schema")?;
                info!(
                    tool_calls = calls,
                    input_tokens = usage.input_tokens,
                    cache_read_tokens = usage.cache_read_tokens,
                    output_tokens = usage.output_tokens,
                    "drafted after research"
                );
                return Ok(Drafted {
                    drafts,
                    usage,
                    trail,
                    findings,
                });
            }

            match reply["stop_reason"].as_str() {
                Some("tool_use") => {
                    let mut results = Vec::with_capacity(uses.len());
                    for u in uses {
                        let name = u["name"].as_str().unwrap_or("");
                        let id = u["id"].clone();
                        calls += 1;
                        if calls > MAX_TOOL_CALLS {
                            results.push(json!({
                                "type": "tool_result", "tool_use_id": id, "is_error": true,
                                "content": "Research budget used up. Call submit_drafts now with what you have."
                            }));
                            continue;
                        }
                        match tools.call(name, &u["input"]).await {
                            Ok(v) => {
                                trail.push(describe(name, &u["input"], Some(&v)));
                                let text = research::render(&v);
                                findings.push(v);
                                results.push(json!({
                                    "type": "tool_result", "tool_use_id": id, "content": text
                                }));
                            }
                            Err(e) => {
                                trail.push(format!(
                                    "{} (failed)",
                                    describe(name, &u["input"], None)
                                ));
                                results.push(json!({
                                    "type": "tool_result", "tool_use_id": id, "is_error": true,
                                    "content": format!("{e:#}")
                                }));
                            }
                        }
                    }
                    messages.push(json!({"role": "user", "content": results}));
                }
                // A server-side pause: resend as is and it resumes.
                Some("pause_turn") => {}
                Some("end_turn") if !nudged => {
                    nudged = true;
                    messages.push(json!({
                        "role": "user",
                        "content": "Call submit_drafts now with your three drafts."
                    }));
                }
                Some("refusal") => bail!("Claude declined: {}", reply["stop_details"]),
                Some("max_tokens") => bail!("Claude reply was cut off at max_tokens"),
                other => bail!("Claude stopped without drafts ({other:?})"),
            }
        }
        bail!("no drafts after {MAX_ROUNDS} rounds")
    }

    /// Pick which of `items` (each carrying an `id`) are worth a post. Returns
    /// the chosen ids, at most `max`.
    pub async fn triage(
        &self,
        brief: &str,
        items: &Value,
        max: usize,
    ) -> anyhow::Result<(Vec<String>, Usage)> {
        let schema = json!({
            "type": "object",
            "properties": {
                "picks": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["picks"],
            "additionalProperties": false
        });
        let user = format!(
            "BRIEF:\n{brief}\nPick at most {max}. Return their ids in `picks`, best first; \
             an empty list is a fine answer.\n\nFACTS:\n{}",
            serde_json::to_string_pretty(items)?
        );
        let (out, usage) = self.call(&user, schema).await?;
        let mut picks: Vec<String> =
            serde_json::from_value(out["picks"].clone()).context("reply had no picks array")?;
        picks.truncate(max);
        Ok((picks, usage))
    }

    async fn call(&self, user: &str, schema: Value) -> anyhow::Result<(Value, Usage)> {
        let body = json!({
            "model": self.model,
            "max_tokens": 16000,
            "system": SYSTEM,
            "fallbacks": "default",
            "output_config": {
                "effort": "medium",
                "format": {"type": "json_schema", "schema": schema}
            },
            "messages": [{"role": "user", "content": user}]
        });
        let reply = self.post(&body).await?;
        let mut usage = Usage::default();
        usage.add(&reply);
        match reply["stop_reason"].as_str() {
            Some("refusal") => bail!("Claude declined: {}", reply["stop_details"]),
            Some("max_tokens") => bail!("Claude reply was cut off at max_tokens"),
            _ => {}
        }
        let text: String = reply["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let out: Value = serde_json::from_str(&text).context("Claude reply was not the schema")?;
        Ok((out, usage))
    }

    async fn post(&self, body: &Value) -> anyhow::Result<Value> {
        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", FALLBACK_BETA)
            .json(body)
            .send()
            .await
            .context("Claude request failed")?;
        let status = resp.status();
        let reply: Value = resp.json().await.context("Claude reply was not JSON")?;
        if !status.is_success() {
            bail!("Claude returned {status}: {}", reply["error"]["message"]);
        }
        Ok(reply)
    }
}

/// "player_profile(Otega Oweh, 2026)"-style trail entry for Slack: the tool
/// and who it was about, named from the result where the input is an id.
fn describe(name: &str, input: &Value, result: Option<&Value>) -> String {
    let named = result.and_then(|r| {
        [
            &r["player"]["name"],
            &r["team"]["name"],
            &r["name"],
            &r["conference"],
        ]
        .into_iter()
        .find_map(|v| v.as_str())
        .map(str::to_string)
    });
    let side = |k: &str| {
        result
            .and_then(|r| r[format!("{k}_team")].as_str())
            .or_else(|| input[k].as_str())
            .map(str::to_string)
    };
    let arg = match (name, side("home"), side("away")) {
        ("predict_matchup", Some(h), Some(a)) => format!("{a} at {h}"),
        _ => named
            .or_else(|| {
                ["name", "code", "sort"]
                    .iter()
                    .find_map(|k| input[*k].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default(),
    };
    let mut parts: Vec<String> = Vec::new();
    if !arg.is_empty() {
        parts.push(arg);
    }
    if let Some(s) = input["season"].as_i64() {
        parts.push(s.to_string());
    }
    if input["cross_year"].as_bool() == Some(true) {
        parts.push("all years".into());
    }
    format!("{name}({})", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_names_who_a_call_was_about() {
        let input = json!({"player_id": "b504", "season": 2026});
        let result = json!({"player": {"name": "Yaxel Lendeborg"}});
        assert_eq!(
            describe("player_profile", &input, Some(&result)),
            "player_profile(Yaxel Lendeborg, 2026)"
        );
        let input = json!({"home": "uuid-a", "away": "uuid-b"});
        let result = json!({"home_team": "Duke", "away_team": "Kentucky"});
        assert_eq!(
            describe("predict_matchup", &input, Some(&result)),
            "predict_matchup(Kentucky at Duke)"
        );
        let input = json!({"player_id": "x", "cross_year": true});
        assert_eq!(
            describe("similar_players", &input, None),
            "similar_players(all years)"
        );
    }
}
