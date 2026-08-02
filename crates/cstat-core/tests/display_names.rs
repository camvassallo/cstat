//! End-to-end checks for `compute::compute_display_names` (issue #243 follow-up).
//!
//! `players.display_name` is the name the site shows; `players.name` stays the
//! legal name the joins key on. The step is allowed to set a display name from
//! exactly two sources — a generational suffix Torvik kept and NatStat dropped,
//! and a curated override — and the point of these checks is that it never does
//! anything else. In particular it must never import a Torvik misspelling,
//! which is the failure mode that made the wholesale "just use Torvik's name"
//! approach unusable (see `cstat_core::display_names`).
//!
//! Deliberately ONE test rather than several: every check needs the column
//! populated, cargo runs test fns concurrently, and two of these racing to
//! `UPDATE players` deadlock in Postgres. The assertions are split into helper
//! functions instead.
//!
//! Gated `#[ignore]` — needs a local DB with Torvik ingested for all seasons.
//! Running it recomputes the column, which is what `compute_all` does anyway.
//!   DATABASE_URL=... cargo test -p cstat-core --test display_names -- --ignored --nocapture

use cstat_core::compute::compute_display_names;
use cstat_core::display_names;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

#[tokio::test]
#[ignore = "needs local DB with Torvik ingested for all seasons"]
async fn display_names_are_derived_safe_and_cover_the_known_cases() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let seasons: Vec<i32> =
        sqlx::query_scalar("SELECT DISTINCT season FROM players ORDER BY season")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(!seasons.is_empty(), "no players rows");

    let mut total = 0u64;
    for season in &seasons {
        total += compute_display_names(&pool, *season).await.unwrap();
    }
    eprintln!("  display names written: {total}");
    assert!(total > 0, "expected at least the suffix restorations");

    no_redundant_or_blank_values(&pool).await;
    every_non_override_is_a_pure_suffix_restoration(&pool).await;
    every_override_lands(&pool).await;
    known_players_render_under_the_name_people_use(&pool).await;
}

/// A display name equal to `name` is noise on the wire and hides whether the
/// step had an opinion; a blank one is a bug that would render as an empty
/// player.
async fn no_redundant_or_blank_values(pool: &PgPool) {
    let redundant: i64 =
        sqlx::query_scalar("SELECT count(*) FROM players WHERE display_name = name")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        redundant, 0,
        "display_name should be NULL when it equals name"
    );

    let blank: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM players WHERE display_name IS NOT NULL AND btrim(display_name) = ''",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(blank, 0, "display_name must never be empty");
}

/// THE safety property. Every non-override display name must be the legal name
/// plus a generational suffix — identical once the suffix is stripped. A row
/// whose letters differ means a source misspelling got in, which is exactly
/// what the narrow rule exists to prevent.
async fn every_non_override_is_a_pure_suffix_restoration(pool: &PgPool) {
    let override_pids: Vec<i32> = display_names::overrides()
        .iter()
        .map(|o| o.torvik_pid)
        .collect();
    let rows = sqlx::query(
        r#"SELECT p.name, p.display_name, p.season
             FROM players p
            WHERE p.display_name IS NOT NULL
              AND NOT EXISTS (
                  SELECT 1 FROM torvik_player_stats t
                   WHERE t.player_id = p.id AND t.torvik_pid = ANY($1))"#,
    )
    .bind(&override_pids)
    .fetch_all(pool)
    .await
    .unwrap();

    let mut violations = Vec::new();
    for r in &rows {
        let name: String = r.get("name");
        let display: String = r.get("display_name");
        let season: i32 = r.get("season");
        // Reconstructing through the same helper is the check: given the legal
        // name and the display name as the "Torvik" side, the rule must
        // reproduce the display name exactly.
        if display_names::suffix_restoration(&name, &display).as_deref() != Some(display.as_str()) {
            violations.push(format!("{season} '{name}' -> '{display}'"));
        }
    }
    assert!(
        violations.is_empty(),
        "{} non-override display name(s) are not pure suffix restorations — a source \
misspelling may have leaked in: {:?}",
        violations.len(),
        violations.iter().take(10).collect::<Vec<_>>()
    );
}

/// An override entry that matches no row is a typo'd pid quietly doing nothing.
async fn every_override_lands(pool: &PgPool) {
    for o in display_names::overrides() {
        let hits: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM players p
                 JOIN torvik_player_stats t ON t.player_id = p.id
                WHERE t.torvik_pid = $1 AND p.display_name = $2"#,
        )
        .bind(o.torvik_pid)
        .bind(&o.display_name)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(
            hits > 0,
            "override pid {} ('{}') matched no player row — wrong pid, or the \
legal name already equals it",
            o.torvik_pid,
            o.display_name
        );
    }
}

/// The headline cases, asserted by name so a regression is legible.
async fn known_players_render_under_the_name_people_use(pool: &PgPool) {
    // (legal name, expected display name)
    let expected = [
        ("Obadiah Toppin", "Obi Toppin"),
        ("Temetrius Morant", "Ja Morant"),
        ("Filip Petrusey", "Filip Petrusev"),
    ];
    for (legal, display) in expected {
        let got: Option<String> = sqlx::query_scalar(
            "SELECT display_name FROM players WHERE name = $1 AND display_name IS NOT NULL LIMIT 1",
        )
        .bind(legal)
        .fetch_optional(pool)
        .await
        .unwrap()
        .flatten();
        assert_eq!(got.as_deref(), Some(display), "legal name '{legal}'");
    }

    // Suffixes NatStat dropped, restored mechanically rather than by hand.
    let suffixed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM players WHERE display_name ~ ' (Jr\\.|Sr\\.|II|III|IV)$'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        suffixed > 1000,
        "expected ~2,000 suffix restorations, got {suffixed}"
    );
}
