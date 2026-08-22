//! Integration test for the 247Sports **JSON** row parsers
//! ([`parse_recruits_json`] / [`parse_commits_json`]) against real captured
//! rows from `ipa.247sports.com/rdb/v1/`.
//!
//! Fixtures captured 2026-08-19 from the class-of-2026 feeds, three rows each:
//!
//! * `recruits_2026_p1.json` — a signed 4-star, a committed-but-unsigned
//!   4-star, and an unranked uncommitted player (every `commit_status` branch).
//! * `commits_2026_p1.json` — a ranked commit, an unranked international
//!   commit, and a JUCO commit (the `currentInstitution.group` branch).
//!
//! Public data; the responses carry no token. The sibling
//! `recruits_parser.rs` / `commits_feed_parser.rs` cover the HTML transport,
//! which stays available behind `--html`.

use cstat_ingest::tfs_recruits::{RecruitRow, parse_commits_json, parse_recruits_json};
use serde_json::Value;

const RANKINGS: &str = include_str!("fixtures/recruits_2026_p1.json");
const COMMITS: &str = include_str!("fixtures/commits_2026_p1.json");

fn parse_all(
    fixture: &str,
    rows_key: &str,
    f: fn(&Value) -> Option<RecruitRow>,
) -> Vec<RecruitRow> {
    let body: Value = serde_json::from_str(fixture).expect("fixture is valid JSON");
    body[rows_key]
        .as_array()
        .expect("fixture has the rows array")
        .iter()
        .filter_map(f)
        .collect()
}

fn rankings() -> Vec<RecruitRow> {
    parse_all(RANKINGS, "players", parse_recruits_json)
}

fn commits() -> Vec<RecruitRow> {
    parse_all(COMMITS, "list", parse_commits_json)
}

fn by_key(rows: &[RecruitRow], key: i64) -> &RecruitRow {
    rows.iter()
        .find(|r| r.recruit_key == key)
        .unwrap_or_else(|| panic!("no row with recruit_key = {key}"))
}

#[test]
fn every_fixture_row_parses_with_a_key() {
    assert_eq!(rankings().len(), 3);
    assert_eq!(commits().len(), 3);
    for r in rankings().iter().chain(commits().iter()) {
        assert!(r.recruit_key > 0, "row missing recruit_key: {r:?}");
    }
}

/// The scale trap: 247's JSON `compositeRating` is 0–100, the column is 0–1.
/// Ralph Scott is `97.377…` in the feed and must land as `0.97377…`. A
/// regression here feeds a 100x feature into the freshman projection model
/// without any type error to catch it.
#[test]
fn composite_rating_is_rescaled_to_zero_one() {
    let rows = rankings();
    let scott = by_key(&rows, 46160429);
    let rating = scott.composite_rating.expect("Ralph Scott is rated");
    assert!(
        (rating - 0.973_772_9).abs() < 1e-6,
        "expected ~0.9737729, got {rating}"
    );
    assert!(
        rows.iter()
            .filter_map(|r| r.composite_rating)
            .all(|r| (0.0..=1.0).contains(&r)),
        "every composite_rating must be on the 0–1 scale"
    );
}

/// The star trap, and the reason `star_rating` is derived from the rating
/// rather than read off the feed.
///
/// These two captured rows are from the same class, on the same 0-1 composite,
/// and the feeds' own star fields put them in the wrong order:
///
/// | Feed | Player | Rank | Composite | Feed says |
/// | --- | --- | --- | --- | --- |
/// | `commits/`  | Caleb Ourigou | 69 | 0.97627 | 4-star |
/// | `recruits/` | Ralph Scott   | 76 | 0.97377 | **5**-star |
///
/// The lower-rated, lower-ranked player is the one the rankings feed calls a
/// 5-star. Whatever scale that field is on, it is not a band of the composite
/// rating stored beside it — and the captured composite-rankings page settles
/// which side is wrong: 247 renders every player from rank 51 (0.9816) through
/// rank 100 (0.9535) as 4-star. Both of these are 4-stars.
///
/// `tfs_recruits::composite_star_rating` therefore ignores `compositeStarRating`
/// on both feeds and bands the rating instead, which is what the HTML transport
/// has always effectively done by counting rendered glyphs.
#[test]
fn star_rating_is_derived_not_taken_from_the_feed() {
    let scott = &rankings();
    let scott = by_key(scott, 46160429);
    let ourigou = &commits();
    let ourigou = by_key(ourigou, 46154193);

    // The raw feed values this test exists to override.
    let raw_star = |row: &RecruitRow, ptr: &str| -> Option<i64> {
        serde_json::from_str::<Value>(&row.raw_source)
            .expect("raw_source round-trips")
            .pointer(ptr)
            .and_then(Value::as_i64)
    };
    assert_eq!(raw_star(scott, "/compositeStarRating"), Some(5));
    assert_eq!(raw_star(ourigou, "/ranking/compositeStarRating"), Some(4));

    // Scott is rated *below* Ourigou, so he cannot carry more stars.
    let scott_rating = scott.composite_rating.unwrap();
    let ourigou_rating = ourigou.composite_rating.unwrap();
    assert!(scott_rating < ourigou_rating);
    assert_eq!(scott.star_rating, Some(4));
    assert_eq!(ourigou.star_rating, Some(4));
}

#[test]
fn ranked_signed_recruit_parses() {
    let rows = rankings();
    let r = by_key(&rows, 46160429);
    assert_eq!(r.first_name.as_deref(), Some("Ralph"));
    assert_eq!(r.last_name.as_deref(), Some("Scott"));
    assert_eq!(r.composite_rank, Some(76));
    // 4, not the 5 this row's `compositeStarRating` claims — see
    // `star_rating_is_derived_not_taken_from_the_feed` above.
    assert_eq!(r.star_rating, Some(4));
    assert_eq!(r.position_rank, Some(28));
    assert_eq!(r.state_rank, Some(12));
    assert_eq!(r.position.as_deref(), Some("SF"));
    assert_eq!(r.city.as_deref(), Some("Bradenton"));
    assert_eq!(r.state.as_deref(), Some("FL"));
    assert_eq!(r.committed_school.as_deref(), Some("Tennessee"));
    // `signedInstitution` present → Signed, the firmest of the three states.
    assert_eq!(r.commit_status.as_deref(), Some("Signed"));
    assert_eq!(
        r.profile_url.as_deref(),
        Some("/player/Ralph-Scott-46160429")
    );
    assert!(r.photo_url.is_some());
}

#[test]
fn committed_but_unsigned_recruit_is_committed_not_signed() {
    let r = &rankings();
    let harrison = by_key(r, 46138185);
    assert_eq!(harrison.committed_school.as_deref(), Some("Oregon"));
    assert_eq!(harrison.commit_status.as_deref(), Some("Committed"));
}

#[test]
fn unranked_uncommitted_recruit_keeps_its_row() {
    let rows = rankings();
    let green = by_key(&rows, 46138195);
    assert_eq!(green.last_name.as_deref(), Some("Green"));
    assert!(green.committed_school.is_none());
    assert_eq!(green.commit_status.as_deref(), Some("Uncommitted"));
    assert!(green.composite_rank.is_none());
    assert!(green.composite_rating.is_none());
    assert!(green.star_rating.is_none());
    // Still a usable row — position and hometown survive.
    assert_eq!(green.position.as_deref(), Some("PG"));
    assert_eq!(green.state.as_deref(), Some("TX"));
}

/// The rankings feed carries none of these. They must parse as `None` rather
/// than as an empty string or a zero, because the upsert COALESCEs on NULL to
/// avoid blanking what the commits feed or the HTML scrape supplied.
#[test]
fn rankings_feed_leaves_uncarried_columns_null() {
    for r in rankings() {
        assert!(r.height.is_none(), "rankings feed has no height");
        assert!(r.weight.is_none(), "rankings feed has no weight");
        assert!(r.high_school.is_none(), "rankings feed has no high_school");
        assert!(
            r.previous_rank.is_none(),
            "rankings feed has no previous_rank"
        );
        assert!(r.committed_school_slug.is_none());
    }
}

/// The commits feed is keyed on `playerKey`, not `key` — on the same row `key`
/// is the recruit-interest id. Keying on the wrong one would silently create a
/// parallel universe of rows that join to nothing.
#[test]
fn commits_feed_keys_on_player_key() {
    let rows = commits();
    // Caleb Ourigou: playerKey 46154193, key 1064268.
    assert!(rows.iter().any(|r| r.recruit_key == 46154193));
    assert!(
        !rows.iter().any(|r| r.recruit_key == 1064268),
        "keyed on `key` instead of `playerKey`"
    );
}

/// The physical columns are the whole reason the commits feed is still fetched
/// under the JSON transport — the rankings feed has none of them, and `height`
/// is an input to the served freshman projection model.
#[test]
fn commits_feed_carries_physicals() {
    let rows = commits();
    let ourigou = by_key(&rows, 46154193);
    // `formattedHeight` ("6-10"), not the raw `height` float (82.0).
    assert_eq!(ourigou.height.as_deref(), Some("6-10"));
    assert_eq!(ourigou.weight, Some(240));
    assert_eq!(ourigou.position.as_deref(), Some("C"));
    assert_eq!(ourigou.city.as_deref(), Some("Atlanta"));
    assert_eq!(ourigou.state.as_deref(), Some("GA"));
    assert_eq!(ourigou.committed_school.as_deref(), Some("Arkansas"));
    assert_eq!(ourigou.star_rating, Some(4));
    assert_eq!(ourigou.composite_rank, Some(69));
}

/// `currentInstitution` is where the player is *now* — a high school, a JUCO,
/// or a foreign club. It is read as `high_school` for all of those, and only
/// suppressed when `group` says "College" (an early enrollee, where the field
/// holds the destination program instead).
#[test]
fn high_school_comes_from_current_institution() {
    let rows = commits();
    assert_eq!(
        by_key(&rows, 46154193).high_school.as_deref(),
        Some("Overtime Elite")
    );
    // International: the "school" is the country. Kept — it is what the HTML
    // scrape stored too, and it is the only origin signal on the row.
    assert_eq!(
        by_key(&rows, 46169328).high_school.as_deref(),
        Some("Congo")
    );
    // JUCO — group is `JuniorCollege`, not `College`, so it is not suppressed.
    assert_eq!(
        by_key(&rows, 46166536).high_school.as_deref(),
        Some("Jones College")
    );
}

#[test]
fn unranked_commit_has_no_rating_but_keeps_its_destination() {
    let rows = commits();
    let miteo = by_key(&rows, 46169328);
    assert_eq!(miteo.committed_school.as_deref(), Some("Rutgers"));
    assert_eq!(miteo.commit_status.as_deref(), Some("Committed"));
    assert!(miteo.star_rating.is_none());
    assert!(miteo.composite_rating.is_none());
    assert!(miteo.composite_rank.is_none());
}

/// `raw_source` holds the unparsed row so a parser bug is diagnosable without
/// a re-fetch — the JSON transport's analogue of the scrape's raw `<li>`.
#[test]
fn raw_source_round_trips_as_json() {
    for r in rankings().iter().chain(commits().iter()) {
        let v: Value = serde_json::from_str(&r.raw_source).expect("raw_source is valid JSON");
        assert!(v.is_object(), "raw_source should be the row object");
    }
}
