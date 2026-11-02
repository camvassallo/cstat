//! `social_drafts` (what was already sent) and `pregame_snapshots` (what we
//! expected before tipoff). Migration 061.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::site::SlateGame;

pub async fn already_handled(pool: &PgPool, kind: &str, key: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM social_drafts WHERE kind = $1 AND source_key = $2)",
    )
    .bind(kind)
    .bind(key)
    .fetch_one(pool)
    .await
}

pub struct Record<'a> {
    pub kind: &'a str,
    pub source_key: &'a str,
    pub status: &'a str,
    pub facts: Option<&'a Value>,
    pub drafts: Option<Value>,
    pub image_urls: Vec<String>,
    pub model: Option<&'a str>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

/// Insert, or overwrite on a forced rerun of the same key.
pub async fn record(pool: &PgPool, r: Record<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO social_drafts \
           (kind, source_key, status, facts, drafts, image_urls, model, input_tokens, output_tokens) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (kind, source_key) DO UPDATE SET \
           status = EXCLUDED.status, facts = EXCLUDED.facts, drafts = EXCLUDED.drafts, \
           image_urls = EXCLUDED.image_urls, model = EXCLUDED.model, \
           input_tokens = EXCLUDED.input_tokens, output_tokens = EXCLUDED.output_tokens, \
           created_at = now()",
    )
    .bind(r.kind)
    .bind(r.source_key)
    .bind(r.status)
    .bind(r.facts)
    .bind(r.drafts)
    .bind(&r.image_urls)
    .bind(r.model)
    .bind(r.input_tokens.map(|n| n as i32))
    .bind(r.output_tokens.map(|n| n as i32))
    .execute(pool)
    .await
    .map(|_| ())
}

/// Keep the slate's pregame numbers. The first capture wins: a later run the
/// same day may already see games that have tipped.
pub async fn save_snapshots(pool: &PgPool, games: &[SlateGame]) -> sqlx::Result<u64> {
    let mut written = 0;
    for g in games.iter().filter(|g| !g.is_final()) {
        let (Some(h), Some(a), Some(p), Some(m)) = (
            g.home_team_id,
            g.away_team_id,
            g.home_win_probability,
            g.predicted_margin,
        ) else {
            continue;
        };
        written += sqlx::query(
            "INSERT INTO pregame_snapshots \
               (game_id, home_team_id, away_team_id, home_win_prob, projected_margin, \
                projected_home_score, projected_away_score, basis, home_rank, away_rank, watchability) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (game_id) DO NOTHING",
        )
        .bind(g.game_id)
        .bind(h)
        .bind(a)
        .bind(p)
        .bind(m)
        .bind(g.predicted_home_score)
        .bind(g.predicted_away_score)
        .bind(g.prediction_basis.as_deref().unwrap_or("model"))
        .bind(g.home_rank.map(|r| r as i32))
        .bind(g.away_rank.map(|r| r as i32))
        .bind(g.watchability)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(written)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Snapshot {
    pub game_id: Uuid,
    pub home_win_prob: f64,
    pub projected_margin: f64,
    pub projected_home_score: Option<i32>,
    pub projected_away_score: Option<i32>,
    pub home_rank: Option<i32>,
    pub away_rank: Option<i32>,
}

pub async fn load_snapshots(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, Snapshot>> {
    let rows: Vec<Snapshot> = sqlx::query_as(
        "SELECT game_id, home_win_prob, projected_margin, projected_home_score, \
                projected_away_score, home_rank, away_rank \
         FROM pregame_snapshots WHERE game_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|s| (s.game_id, s)).collect())
}

/// Has the morning nightly finished for `date`? True once `compute` succeeded
/// and the upcoming-projection sweep after it has recorded any outcome, both
/// after 6 AM UTC on `date` (the nightly runs at 09:30 UTC). The daily posts
/// wait for this so they read refreshed numbers rather than yesterday's.
pub async fn nightly_finished(pool: &PgPool, date: NaiveDate) -> sqlx::Result<bool> {
    let since = date.and_hms_opt(6, 0, 0).expect("valid time").and_utc();
    sqlx::query_scalar(
        "SELECT \
           EXISTS (SELECT 1 FROM ingest_runs WHERE step = 'compute' AND status = 'ok' AND started_at >= $1) \
           AND EXISTS (SELECT 1 FROM ingest_runs WHERE step = 'upcoming_projections' AND started_at >= $1)",
    )
    .bind(since)
    .fetch_one(pool)
    .await
}

/// Items of `kind` posted since midnight Eastern.
pub async fn posted_today(pool: &PgPool, kind: &str) -> sqlx::Result<i64> {
    let now = chrono::Utc::now();
    let et = crate::eastern_datetime(now);
    let since = now - (et - et.date().and_hms_opt(0, 0, 0).expect("valid time"));
    sqlx::query_scalar(
        "SELECT count(*) FROM social_drafts \
         WHERE kind = $1 AND status = 'posted' AND created_at >= $2",
    )
    .bind(kind)
    .bind(since)
    .fetch_one(pool)
    .await
}
