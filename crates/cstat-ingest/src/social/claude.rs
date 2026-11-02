//! Drafting through the Claude Messages API.
//!
//! There is no official Rust SDK, so this is a plain HTTPS call. Every request
//! asks for structured JSON (`output_config.format`) so the reply parses
//! without scraping prose, and opts into the server-side refusal fallback so a
//! false-positive decline is retried on another model inside the same call.

use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// The standing brief. Kept constant (no dates, no per-run values) so it is a
/// stable prefix.
const SYSTEM: &str = "\
You write draft tweets for Camalytics, a college basketball analytics site. \
A person proofreads every draft before anything is posted, so give them real \
options rather than one safe line.

Voice: confident, dry and specific. Lead with the number that makes the point. \
Sound like a sharp analyst friend, not a press release or a hype account. No \
clickbait, no \"let's go\", no exclamation-mark pileups.

Hard rules:
- Use only the facts and numbers in the FACTS JSON, copied exactly as formatted \
there. Never add a stat, record, ranking, injury or bit of history from your own \
knowledge, and never compute a new number (no differences, averages or \
percentages the facts do not already state). If the facts do not support a \
take, write a plainer draft.
- The player value metric is CAM, with offense and defense halves CAMO and CAMD. \
Never write CamPom, campom or GBPM.
- Never name another analytics site, ranking or data provider (KenPom, Torvik, \
EvanMiya, NET, AP poll, NatStat, 247Sports and the like).
- A rank in the facts is the Camalytics ranking: write \"our No. 12\" or \"No. 12 \
in our ratings\", never a bare \"No. 12\" that reads as the AP poll.
- Win probabilities are ours and are odds, not promises: \"we give Gonzaga a 58% \
chance\", never \"Gonzaga will win\".
- Each draft is a single tweet of at most 270 characters unless the brief asks \
for a list. At most one emoji, no hashtags, no links (the poster adds the link), \
no @-mentions.
- Write three drafts with clearly different angles, and name each angle in a \
few words.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub angle: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
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
                .timeout(Duration::from_secs(180))
                .build()
                .expect("reqwest client builds"),
            key,
            model,
        })
    }

    /// Three drafts for one item.
    pub async fn draft(&self, brief: &str, facts: &Value) -> anyhow::Result<(Vec<Draft>, Usage)> {
        let schema = json!({
            "type": "object",
            "properties": {
                "drafts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string"},
                            "angle": {"type": "string"}
                        },
                        "required": ["text", "angle"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["drafts"],
            "additionalProperties": false
        });
        let user = format!(
            "BRIEF:\n{brief}\n\nFACTS:\n{}",
            serde_json::to_string_pretty(facts)?
        );
        let (out, usage) = self.call(&user, schema).await?;
        let drafts: Vec<Draft> =
            serde_json::from_value(out["drafts"].clone()).context("reply had no drafts array")?;
        Ok((drafts, usage))
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
        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", FALLBACK_BETA)
            .json(&body)
            .send()
            .await
            .context("Claude request failed")?;
        let status = resp.status();
        let reply: Value = resp.json().await.context("Claude reply was not JSON")?;
        if !status.is_success() {
            bail!("Claude returned {status}: {}", reply["error"]["message"]);
        }

        let usage = Usage {
            input_tokens: reply["usage"]["input_tokens"].as_i64().unwrap_or(0),
            output_tokens: reply["usage"]["output_tokens"].as_i64().unwrap_or(0),
        };
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
}
