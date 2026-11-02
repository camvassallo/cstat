//! The social-drafts bot: `cstat-ingest social`.
//!
//! Turns the site's own data into tweet drafts and posts them to a Slack
//! channel for a person to proofread. It never posts to Twitter.
//!
//! Each run is one pass over the jobs. A job decides for itself whether it is
//! due (the Eastern hour, and for the daily posts whether the morning nightly
//! has finished), gathers facts deterministically from the site API and the
//! database, has Claude write drafts from those facts alone, flags any number
//! in a draft that is not in the facts, and posts. `social_drafts` records
//! what was sent, so the hourly cron can run as often as it likes and an item
//! is posted once. Runbook: `docs/social_drafts.md`.

pub mod claude;
pub mod factcheck;
mod games_of_day;
mod lines;
mod news;
mod recaps;
mod research;
pub mod site;
mod slack;
mod store;

use chrono::{Duration, NaiveDate, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::{info, warn};

use crate::NatStatClient;
use crate::notify::{self, SlackChannel, SlackPostOutcome};
use crate::run_ledger::{RunLedger, StepStatus};
use claude::{Draft, Drafter, Usage};
use site::Site;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Job {
    /// The morning slate: the best games tonight, and a closer look at the top one.
    GamesOfDay,
    /// Last night's best individual lines.
    Lines,
    /// Last night's biggest results and upsets against our pregame numbers.
    Recaps,
    /// Headlines from the news feed, with our data's take.
    News,
}

impl Job {
    pub const ALL: [Job; 4] = [Job::GamesOfDay, Job::Lines, Job::Recaps, Job::News];

    fn step(self) -> &'static str {
        match self {
            Job::GamesOfDay => "social_games_of_day",
            Job::Lines => "social_lines",
            Job::Recaps => "social_recaps",
            Job::News => "social_news",
        }
    }
}

pub struct Options {
    pub jobs: Vec<Job>,
    /// Replay a specific day instead of today: the slate date for games of
    /// the day, the game date for lines and recaps. Implies the job is due.
    pub date: Option<NaiveDate>,
    /// Print the Slack payloads instead of posting; write nothing.
    pub dry_run: bool,
    /// Ignore the time gates and what was already sent.
    pub force: bool,
    /// Template drafts only, even with an API key set.
    pub no_llm: bool,
}

/// A graphic attached to a post.
#[derive(Debug, Clone)]
pub struct Image {
    pub url: String,
    pub alt: String,
}

/// One idea for a post: what to write about, the facts to write it from, and
/// what to attach.
#[derive(Debug, Clone)]
pub struct Item {
    /// Dedupe namespace; also the label in `social_drafts`.
    pub kind: &'static str,
    pub source_key: String,
    pub title: String,
    /// One line of context for the proofreader, shown under the title.
    pub summary: String,
    /// What to write, in plain words, for the drafter.
    pub brief: String,
    pub facts: Value,
    pub images: Vec<Image>,
    pub links: Vec<(String, String)>,
    /// Plain drafts used when there is no drafter or it fails.
    pub fallback: Vec<String>,
    /// The season the item is about; the research tools default to it.
    pub season: i32,
}

pub(crate) struct Ctx<'a> {
    pub pool: &'a PgPool,
    pub site: Site,
    pub natstat: Option<&'a NatStatClient>,
    drafter: Option<Drafter>,
    opts: Options,
}

pub async fn run(
    pool: &PgPool,
    natstat: Option<&NatStatClient>,
    opts: Options,
) -> anyhow::Result<()> {
    let drafter = if opts.no_llm {
        None
    } else {
        Drafter::from_env()
    };
    if drafter.is_none() {
        info!("no drafter (ANTHROPIC_API_KEY unset or --no-llm); using template drafts");
    }
    let ctx = Ctx {
        pool,
        site: Site::from_env(),
        natstat,
        drafter,
        opts,
    };

    let now_et = crate::eastern_datetime(Utc::now());
    let today = crate::today_eastern();
    let ledger = RunLedger::start(pool, crate::season_for_date(today));
    let mut failures = 0;

    for &job in &ctx.opts.jobs {
        let started = Utc::now();
        if !ctx.opts.dry_run && !is_due(&ctx, job, now_et.hour(), today).await? {
            info!(?job, "not due");
            continue;
        }
        let result = match job {
            Job::GamesOfDay => games_of_day::build(&ctx, ctx.opts.date.unwrap_or(today)).await,
            Job::Lines => {
                lines::build(&ctx, ctx.opts.date.unwrap_or(today - Duration::days(1))).await
            }
            Job::Recaps => {
                recaps::build(&ctx, ctx.opts.date.unwrap_or(today - Duration::days(1))).await
            }
            Job::News => news::build(&ctx, ctx.opts.date.unwrap_or(today)).await,
        };
        let (status, posted, error) = match result {
            Ok(items) => {
                let mut posted = 0;
                for item in &items {
                    match publish(&ctx, item).await {
                        Ok(true) => posted += 1,
                        Ok(false) => {}
                        Err(e) => {
                            warn!(kind = item.kind, key = %item.source_key, error = %e, "publish failed");
                        }
                    }
                }
                (StepStatus::Ok, posted, None)
            }
            Err(e) => {
                failures += 1;
                warn!(?job, error = %e, "social job failed");
                (StepStatus::Failed, 0, Some(format!("{e:#}")))
            }
        };
        if !ctx.opts.dry_run {
            ledger
                .record(job.step(), status, Some(posted), started, error.as_deref())
                .await;
        }
    }
    anyhow::ensure!(failures == 0, "{failures} social job(s) failed");
    Ok(())
}

/// Whether `job` should run now. An explicit `--date` or `--force` skips the
/// clock; otherwise the daily posts wait for their hour and for the morning
/// nightly, so they read refreshed numbers.
async fn is_due(ctx: &Ctx<'_>, job: Job, hour_et: u32, today: NaiveDate) -> anyhow::Result<bool> {
    if ctx.opts.force || ctx.opts.date.is_some() {
        return Ok(true);
    }
    Ok(match job {
        Job::GamesOfDay => hour_et >= 10 && store::nightly_finished(ctx.pool, today).await?,
        Job::Lines | Job::Recaps => {
            hour_et >= 9 && store::nightly_finished(ctx.pool, today).await?
        }
        Job::News => hour_et >= 8,
    })
}

/// Draft, check and post one item. Returns whether it was posted.
async fn publish(ctx: &Ctx<'_>, item: &Item) -> anyhow::Result<bool> {
    if !ctx.opts.force
        && !ctx.opts.dry_run
        && store::already_handled(ctx.pool, item.kind, &item.source_key).await?
    {
        return Ok(false);
    }

    let Outcome {
        drafts,
        usage,
        mut note,
        trail,
        findings,
    } = draft(ctx, item).await;
    if !trail.is_empty() {
        let line = format!("_Researched:_ {}", trail.join(" · "));
        note = Some(match note {
            Some(n) => format!("{n}\n{line}"),
            None => line,
        });
    }
    // The fact-check corpus is everything the drafter was shown or looked up.
    let corpus = json!({ "facts": item.facts, "research": findings });

    // Slack rejects the whole message when an image URL does not serve an
    // image, which is what a card route answers before the API carrying it is
    // deployed (the SPA's HTML). Post without that card rather than not at all.
    let mut item = item.clone();
    let mut dropped = 0;
    let mut images = Vec::with_capacity(item.images.len());
    for img in std::mem::take(&mut item.images) {
        if ctx.site.serves_image(&img.url).await {
            images.push(img);
        } else {
            dropped += 1;
        }
    }
    item.images = images;
    if dropped > 0 {
        let msg = format!("_{dropped} card(s) left off: the image URL did not return an image._");
        note = Some(match note {
            Some(n) => format!("{n}\n{msg}"),
            None => msg,
        });
    }
    let item = &item;

    let checked: Vec<slack::Checked<'_>> = drafts
        .iter()
        .map(|d| slack::Checked {
            draft: d,
            unverified: factcheck::unverified_numbers(&d.text, &corpus),
        })
        .collect();
    let blocks = slack::blocks(item, &checked, note.as_deref());

    if ctx.opts.dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": item.kind,
                "source_key": item.source_key,
                "facts": item.facts,
                "research": trail,
                "blocks": blocks,
            }))?
        );
        return Ok(false);
    }

    match notify::post_slack_blocks(SlackChannel::SocialDrafts, &item.title, blocks).await {
        SlackPostOutcome::Sent => {}
        // Nothing reached a person, so leave it unrecorded and let the next
        // run try again.
        SlackPostOutcome::NotConfigured | SlackPostOutcome::Failed => return Ok(false),
    }
    let model = ctx.drafter.as_ref().map(|d| d.model.as_str());
    store::record(
        ctx.pool,
        store::Record {
            kind: item.kind,
            source_key: &item.source_key,
            status: "posted",
            // The trail, not the raw tool results: those run to tens of KB an
            // item and can be fetched again from the trail.
            facts: Some(&json!({ "facts": item.facts, "research_trail": trail })),
            drafts: Some(serde_json::to_value(&drafts)?),
            image_urls: item.images.iter().map(|i| i.url.clone()).collect(),
            model,
            input_tokens: usage.map(|u| u.input_tokens),
            output_tokens: usage.map(|u| u.output_tokens),
        },
    )
    .await?;
    Ok(true)
}

struct Outcome {
    drafts: Vec<Draft>,
    usage: Option<Usage>,
    note: Option<String>,
    trail: Vec<String>,
    findings: Vec<Value>,
}

/// Claude's researched drafts, or the item's templates with a note saying why.
async fn draft(ctx: &Ctx<'_>, item: &Item) -> Outcome {
    let templates = |note: String, usage: Option<Usage>| Outcome {
        drafts: item
            .fallback
            .iter()
            .map(|t| Draft {
                text: t.clone(),
                angle: "template".into(),
                source: String::new(),
            })
            .collect(),
        usage,
        note: Some(note),
        trail: vec![],
        findings: vec![],
    };
    let Some(drafter) = &ctx.drafter else {
        return templates("_Template draft: no drafter configured._".into(), None);
    };
    let tools = research::Research {
        site: &ctx.site,
        season: item.season,
    };
    match drafter.draft(&item.brief, &item.facts, &tools).await {
        Ok(d) if !d.drafts.is_empty() => Outcome {
            drafts: d.drafts,
            usage: Some(d.usage),
            note: None,
            trail: d.trail,
            findings: d.findings,
        },
        Ok(d) => templates(
            ":warning: The drafter returned nothing; template draft below.".into(),
            Some(d.usage),
        ),
        Err(e) => {
            warn!(kind = item.kind, error = %e, "drafting failed; using templates");
            templates(
                format!(":warning: Drafting failed ({e}); template draft below."),
                None,
            )
        }
    }
}

/// Recording an item the bot decided not to post, so it is not reconsidered.
pub(crate) async fn record_skip(ctx: &Ctx<'_>, kind: &str, key: &str, facts: &Value) {
    if ctx.opts.dry_run {
        return;
    }
    let r = store::Record {
        kind,
        source_key: key,
        status: "skipped",
        facts: Some(facts),
        drafts: None,
        image_urls: vec![],
        model: None,
        input_tokens: None,
        output_tokens: None,
    };
    if let Err(e) = store::record(ctx.pool, r).await {
        warn!(kind, key, error = %e, "failed to record a skipped item");
    }
}

/// `"58%"` from a 0-1 probability.
pub(crate) fn pct(p: f64) -> String {
    format!("{:.0}%", p * 100.0)
}

/// `1.234` → `1.2`, as JSON, so the facts carry the precision a tweet would.
pub(crate) fn round1(x: f64) -> Value {
    json!((x * 10.0).round() / 10.0)
}
