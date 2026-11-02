//! The Slack message for one item: its drafts in copyable code blocks, the
//! cards as images, and the links a proofreader needs to check the numbers.

use serde_json::{Value, json};

use super::Item;
use super::claude::Draft;

/// Slack rejects a section whose text exceeds 3000 characters.
const SECTION_LIMIT: usize = 2900;
const TWEET_LIMIT: usize = 280;

pub struct Checked<'a> {
    pub draft: &'a Draft,
    pub unverified: Vec<String>,
}

pub fn blocks(item: &Item, drafts: &[Checked<'_>], note: Option<&str>) -> Value {
    let mut blocks = vec![json!({
        "type": "header",
        "text": {"type": "plain_text", "text": clip(&item.title, 150), "emoji": true}
    })];
    if !item.summary.is_empty() {
        blocks.push(json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": clip(&escape(&item.summary), SECTION_LIMIT)}]
        }));
    }
    if let Some(note) = note {
        blocks.push(json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": clip(note, SECTION_LIMIT)}]
        }));
    }
    for (i, c) in drafts.iter().enumerate() {
        let len = c.draft.text.chars().count();
        let over = if len > TWEET_LIMIT {
            " :warning: over 280"
        } else {
            ""
        };
        let mut text = format!(
            "*Draft {}* · {} · {len} chars{over}\n```{}```",
            i + 1,
            escape(&c.draft.angle),
            escape(&c.draft.text)
        );
        if !c.unverified.is_empty() {
            text.push_str(&format!(
                "\n:warning: not in the facts, check before posting: {}",
                c.unverified.join(", ")
            ));
        }
        blocks.push(json!({
            "type": "section",
            "text": {"type": "mrkdwn", "text": clip(&text, SECTION_LIMIT)}
        }));
    }
    for img in &item.images {
        blocks.push(json!({
            "type": "image",
            "image_url": img.url,
            "alt_text": clip(&img.alt, 2000)
        }));
    }
    if !item.links.is_empty() {
        let links = item
            .links
            .iter()
            .map(|(label, url)| format!("<{url}|{}>", escape(label)))
            .collect::<Vec<_>>()
            .join("  ·  ");
        blocks.push(json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": clip(&links, SECTION_LIMIT)}]
        }));
    }
    blocks.push(json!({"type": "divider"}));
    Value::Array(blocks)
}

/// Slack mrkdwn's three control characters.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::super::Image;
    use super::*;

    fn item() -> Item {
        Item {
            kind: "game_of_day",
            source_key: "2026-11-02".into(),
            title: "Game of the day: Purdue vs Gonzaga".into(),
            summary: "7:30 PM ET · TNT".into(),
            brief: String::new(),
            facts: json!({}),
            images: vec![Image {
                url: "https://camalytics.org/og/matchup/x.png".into(),
                alt: "card".into(),
            }],
            links: vec![("Schedule".into(), "https://camalytics.org/schedule".into())],
            fallback: vec![],
        }
    }

    #[test]
    fn drafts_render_as_code_blocks_with_flags() {
        let d = Draft {
            text: "Gonzaga & Purdue <tonight>".into(),
            angle: "matchup".into(),
        };
        let checked = [Checked {
            draft: &d,
            unverified: vec!["64".into()],
        }];
        let b = blocks(&item(), &checked, None);
        let arr = b.as_array().unwrap();
        assert_eq!(arr[0]["type"], "header");
        let section = arr.iter().find(|b| b["type"] == "section").unwrap();
        let text = section["text"]["text"].as_str().unwrap();
        assert!(text.contains("```Gonzaga &amp; Purdue &lt;tonight&gt;```"));
        assert!(text.contains("check before posting: 64"));
        assert!(arr.iter().any(|b| b["type"] == "image"));
        assert_eq!(arr.last().unwrap()["type"], "divider");
    }

    #[test]
    fn long_drafts_are_marked() {
        let d = Draft {
            text: "x".repeat(300),
            angle: "a".into(),
        };
        let checked = [Checked {
            draft: &d,
            unverified: vec![],
        }];
        let b = blocks(&item(), &checked, None);
        assert!(b.to_string().contains("over 280"));
    }
}
