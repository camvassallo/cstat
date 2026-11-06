//! The API feed stopped carrying tags, so `tags_from_description` rebuilds them.
//! Seasons loaded before that change still hold the feed's own tags beside the
//! same descriptions, which makes them the answer key. Skips where the database
//! holds no such season, as in CI.

use cstat_ingest::ingest::playbyplay::tags_from_description;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeSet;

/// Tags the feed attached that a description cannot supply (`paint`, `brk`,
/// `2ch`, `offto`, their run-together variants), or that nothing reads.
fn box_event_tags(tags: &[String]) -> BTreeSet<String> {
    const BOX: &[&str] = &[
        "SUB", "REB", "ORB", "FGA", "FGM", "3FA", "3FM", "FTA", "FTM", "PF", "FOULED", "AST", "TO",
        "STL", "BLK", "TIMEOUT",
    ];
    tags.iter()
        .filter(|t| BOX.contains(&t.as_str()))
        .cloned()
        .collect()
}

#[tokio::test]
async fn derived_tags_match_the_feeds_own_tags() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let rows: Vec<(String, Vec<String>, i64)> = sqlx::query_as(
        "SELECT description, tags, count(*) FROM play_by_play \
         WHERE season = 2026 AND description IS NOT NULL AND description <> '' \
         GROUP BY 1, 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    if rows.is_empty() {
        return;
    }

    let (mut total, mut wrong) = (0i64, 0i64);
    let mut misses: Vec<(i64, String, BTreeSet<String>, BTreeSet<String>)> = Vec::new();
    for (description, tags, count) in rows {
        let mut expected = box_event_tags(&tags);
        // The feed tagged about a third of made "3-point jump shot" plays `3FM`
        // alone. That is its bug, and the attempt belongs.
        if expected.contains("3FM") {
            expected.insert("3FA".to_string());
        }
        let derived = box_event_tags(&tags_from_description(&description));
        total += count;
        if derived != expected {
            wrong += count;
            misses.push((count, description, expected, derived));
        }
    }
    misses.sort_by_key(|m| std::cmp::Reverse(m.0));
    for (count, description, expected, derived) in misses.iter().take(15) {
        println!("{count:>7}  {description:?}  feed={expected:?} derived={derived:?}");
    }
    let share = wrong as f64 / total as f64;
    println!("{wrong} of {total} plays differ ({:.3}%)", share * 100.0);
    assert!(share < 0.002, "derived tags drifted from the feed's");
}
