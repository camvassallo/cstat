//! Integration test for [`cstat_ingest::tfs_recruits::parse_recruits_html`]
//! against a real captured 247Sports composite-rankings page fragment.
//!
//! Fixture: `tests/fixtures/recruits_2026_hs_p2.html` — page 2 (ranks 51-100)
//! of the class-of-2026 high-school composite, captured 2026-05-11. Public
//! data (player rankings on a public 247 page); no auth tokens in the body.

use cstat_ingest::tfs_recruits::{
    COMPOSITE_STAR_BANDS, RecruitRow, composite_star_rating, parse_recruits_html,
};

const FIXTURE: &str = include_str!("fixtures/recruits_2026_hs_p2.html");

fn find_by_key(rows: &[RecruitRow], key: i64) -> &RecruitRow {
    rows.iter()
        .find(|r| r.recruit_key == key)
        .unwrap_or_else(|| panic!("no row with recruit_key = {key}"))
}

#[test]
fn parses_full_page_with_expected_row_count() {
    let rows = parse_recruits_html(FIXTURE);
    // Page 2 of the class-of-2026 HS composite — 247 serves 50 rows per page.
    // Allow ±2 wiggle in case 247 changes pagination size, but flag with a
    // descriptive message if it does.
    assert!(
        (48..=52).contains(&rows.len()),
        "expected ~50 rows on a full page, got {} — 247 may have changed pagination",
        rows.len()
    );
}

#[test]
fn known_row_alex_constanza_parses() {
    let rows = parse_recruits_html(FIXTURE);
    let r = find_by_key(&rows, 46_134_907);
    assert_eq!(r.first_name.as_deref(), Some("Alex"));
    assert_eq!(r.last_name.as_deref(), Some("Constanza"));
    assert_eq!(r.composite_rank, Some(51));
    // Alex has a `.rank-column .other` of 53 → previous_rank
    assert_eq!(r.previous_rank, Some(53));
    assert_eq!(r.position.as_deref(), Some("SF"));
    assert_eq!(r.height.as_deref(), Some("6-8"));
    assert_eq!(r.weight, Some(205));
    assert_eq!(r.high_school.as_deref(), Some("SPIRE Academy"));
    assert_eq!(r.city.as_deref(), Some("Geneva"));
    assert_eq!(r.state.as_deref(), Some("OH"));
    assert_eq!(r.composite_rating, Some(0.9816));
    assert_eq!(r.star_rating, Some(4)); // 4 yellow + 1 lightgrey
    assert_eq!(r.position_rank, Some(20));
    assert_eq!(r.state_rank, Some(3));
    assert_eq!(r.commit_status.as_deref(), Some("Uncommitted"));
    assert!(r.committed_school.is_none());
}

#[test]
fn known_row_sayon_keita_parses() {
    let rows = parse_recruits_html(FIXTURE);
    let r = find_by_key(&rows, 46_160_428);
    assert_eq!(r.first_name.as_deref(), Some("Sayon"));
    assert_eq!(r.last_name.as_deref(), Some("Keita"));
    assert_eq!(r.composite_rank, Some(54));
    // Sayon has no `.rank-column .other` — no rank movement to report.
    assert_eq!(r.previous_rank, None);
    assert_eq!(r.position.as_deref(), Some("C"));
    assert_eq!(r.height.as_deref(), Some("6-11"));
    assert_eq!(r.weight, Some(215));
    assert_eq!(r.high_school.as_deref(), Some("Spain"));
    assert_eq!(r.state.as_deref(), Some("SPAI"));
    assert_eq!(r.committed_school.as_deref(), Some("North Carolina"));
    assert_eq!(r.committed_school_slug.as_deref(), Some("north-carolina"));
    // Sayon's row in this fixture has an img-link but no checkmark.
    assert_eq!(r.commit_status.as_deref(), Some("Committed"));
}

#[test]
fn every_row_has_recruit_key_and_composite_rank() {
    let rows = parse_recruits_html(FIXTURE);
    for r in &rows {
        assert!(r.recruit_key > 0, "row missing recruit_key: {r:?}");
        assert!(
            r.composite_rank.is_some(),
            "row missing composite_rank: name={:?} key={}",
            r.first_name,
            r.recruit_key
        );
    }
}

#[test]
fn star_ratings_in_expected_range() {
    let rows = parse_recruits_html(FIXTURE);
    for r in &rows {
        let stars = r.star_rating.unwrap_or(0);
        // 247's composite is normalized to 1-5 stars; page-2 (ranks 51-100)
        // should be predominantly 4-star with some 5-star outliers.
        assert!(
            (0..=5).contains(&stars),
            "star_rating out of range: {stars} for key={}",
            r.recruit_key
        );
    }
}

#[test]
fn commit_status_taxonomy_covers_all_rows() {
    let rows = parse_recruits_html(FIXTURE);
    let allowed = ["Signed", "Committed", "Uncommitted"];
    for r in &rows {
        let status = r
            .commit_status
            .as_deref()
            .unwrap_or_else(|| panic!("missing commit_status for {}", r.recruit_key));
        assert!(
            allowed.contains(&status),
            "unexpected commit_status `{status}` on row {} — widen the parser taxonomy",
            r.recruit_key
        );
    }
}

#[test]
fn committed_rows_have_school_and_slug() {
    let rows = parse_recruits_html(FIXTURE);
    for r in &rows {
        match r.commit_status.as_deref() {
            Some("Committed") | Some("Signed") => {
                assert!(
                    r.committed_school.is_some(),
                    "committed row missing committed_school: key={}",
                    r.recruit_key
                );
                assert!(
                    r.committed_school_slug.is_some(),
                    "committed row missing committed_school_slug: key={}",
                    r.recruit_key
                );
            }
            Some("Uncommitted") => {
                assert!(r.committed_school.is_none());
                assert!(r.committed_school_slug.is_none());
            }
            other => panic!("unexpected status {other:?} on row {}", r.recruit_key),
        }
    }
}

#[test]
fn ranks_are_monotonic_on_page_2() {
    // Page 2 of the class-of-2026 HS composite should cover ranks 51-100
    // (give or take a row near the boundaries). The raw HTML preserves source
    // order, so iterating rows in document order should give monotonically
    // non-decreasing composite_rank — a guard against the parser silently
    // re-ordering or duplicating rows.
    let rows = parse_recruits_html(FIXTURE);
    let ranks: Vec<i32> = rows.iter().filter_map(|r| r.composite_rank).collect();
    let min = *ranks.iter().min().unwrap();
    let max = *ranks.iter().max().unwrap();
    assert!(
        (45..=55).contains(&min),
        "page-2 min rank looks wrong: {min}"
    );
    assert!(
        (95..=105).contains(&max),
        "page-2 max rank looks wrong: {max}"
    );
    for w in ranks.windows(2) {
        assert!(
            w[0] <= w[1],
            "ranks should be non-decreasing in source order"
        );
    }
}

/// Calibration guard for [`composite_star_rating`], which the JSON transport
/// uses in place of the feeds' own `compositeStarRating`.
///
/// This fixture is the ground truth that pins the bands: 50 real rows of the
/// 2026 composite, each carrying both the rendered star glyphs (which the HTML
/// parser counts) and the 0-1 composite rating they were assigned from. If a
/// band constant drifts away from what 247 actually renders, this fails.
///
/// It cannot pin the 4/5 boundary — page 2 starts at rank 51 and holds no
/// 5-star row — so it asserts the half it can: that every rating here is
/// *below* the 5-star floor, which is what makes the JSON feed's 5-star claim
/// at rank 76 (rating 0.9738) impossible. See `scripts/audit_recruit_stars.sql`
/// query 3 for the DB-side check that does pin it.
#[test]
fn star_bands_match_the_captured_rankings_page() {
    let rows = parse_recruits_html(FIXTURE);
    assert!(!rows.is_empty(), "fixture parsed to no rows");

    let five_star_floor = COMPOSITE_STAR_BANDS[0].0;
    for r in &rows {
        let rendered = r.star_rating.expect("every fixture row renders stars");
        let rating = r
            .composite_rating
            .expect("every fixture row carries a composite rating");
        let derived = composite_star_rating(Some(rating)).unwrap();
        assert_eq!(
            derived,
            rendered,
            "247 renders {} {} (rating {rating}) as {rendered}-star, bands say {derived}",
            r.first_name.as_deref().unwrap_or("?"),
            r.last_name.as_deref().unwrap_or("?"),
        );
        assert!(
            rating < five_star_floor,
            "ranks 51-100 are all 4-star, so none may reach the 5-star floor"
        );
    }
}

/// The bands must be a non-increasing step function of the rating and must
/// cover the whole 0-1 range: a rating that falls through every floor is a
/// 2-star, never `None`. `None` is reserved for an unrated recruit.
#[test]
fn star_bands_are_total_and_monotone() {
    assert_eq!(composite_star_rating(None), None);
    assert_eq!(composite_star_rating(Some(1.0)), Some(5));
    assert_eq!(composite_star_rating(Some(0.0)), Some(2));

    let mut last = 5;
    for step in 0..=1000 {
        let rating = 1.0 - (step as f32) / 1000.0;
        let stars = composite_star_rating(Some(rating)).unwrap();
        assert!(
            stars <= last,
            "stars must not increase as rating falls: {rating} gave {stars} after {last}"
        );
        last = stars;
    }
    assert_eq!(last, 2, "the bottom of the range must be a 2-star");
}

/// Pins the fitted floors themselves.
///
/// Every other band test is deliberately constant-agnostic — it asks whether
/// the bands reproduce the fixture, and the fixture is 4-stars only. That is
/// exactly how `0.8900` / `0.7900` shipped green while mis-banding 17% of the
/// scraped history one star too high. The floors are a *fit*, not a convention,
/// so they need a test that fails when someone re-guesses them.
///
/// The fit lives in `scripts/audit_recruit_stars.sql` query 3, against the
/// 9,266 HTML-scraped rows whose star came from counting 247's rendered glyphs.
/// If 247 rescales, re-run that query and move these numbers to match it — do
/// not round them toward a published scale, which is what these two guard
/// against.
#[test]
fn star_band_floors_are_the_fitted_values() {
    assert_eq!(
        COMPOSITE_STAR_BANDS,
        [(0.9900, 5), (0.9350, 4), (0.8100, 3)],
        "band floors are fitted to the scraped history (audit_recruit_stars.sql \
         query 3), not to 247's published scale — only the 5-star floor matches \
         a published figure. Re-fit before changing these."
    );
}
