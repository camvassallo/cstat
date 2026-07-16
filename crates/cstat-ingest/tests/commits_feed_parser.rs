//! Integration test for [`cstat_ingest::tfs_recruits::parse_commits_html`]
//! against real captured rows from the 247Sports national commits feed
//! (`/season/2026-basketball/commits/`).
//!
//! Fixture: `tests/fixtures/commits_2026_p1.html` — three representative rows
//! captured 2026-07 from the class-of-2026 feed: an unranked international
//! commit (the case issue #175 is about), a ranked 5-star commit, and a signed
//! (checkmark) international commit. Public data; no auth tokens in the body.
//!
//! The commits feed is the gap-filler for the composite-rankings ingest: it
//! carries unranked/international/G-League commits the ranked composite omits,
//! each with its committed school. This parser shares row extraction with the
//! composite parser but keys off the `ri-page__` class prefix.

use cstat_ingest::tfs_recruits::{RecruitRow, parse_commits_html};

const FIXTURE: &str = include_str!("fixtures/commits_2026_p1.html");

fn find_by_key(rows: &[RecruitRow], key: i64) -> &RecruitRow {
    rows.iter()
        .find(|r| r.recruit_key == key)
        .unwrap_or_else(|| panic!("no row with recruit_key = {key}"))
}

#[test]
fn parses_all_fixture_rows() {
    let rows = parse_commits_html(FIXTURE);
    assert_eq!(rows.len(), 3, "expected 3 rows from the fixture");
    // The protocol-relative, capital-`P` `//247sports.com/Player/…-{id}/` href
    // must resolve — the composite feed's lowercase `/player/…` regex would
    // have missed this without the case-insensitive fix.
    for r in &rows {
        assert!(r.recruit_key > 0, "row missing recruit_key: {r:?}");
    }
}

#[test]
fn unranked_international_commit_parses() {
    // Babacar Sane — Senegal, unranked, committed to St. John's. Exactly the
    // kind of commit the composite rankings omit (issue #175).
    let rows = parse_commits_html(FIXTURE);
    let r = find_by_key(&rows, 46_167_194);
    assert_eq!(r.first_name.as_deref(), Some("Babacar"));
    assert_eq!(r.last_name.as_deref(), Some("Sane"));
    assert_eq!(r.height.as_deref(), Some("6-8"));
    // Committed school comes from the bare `.status > img` alt (HTML-entity
    // decoded), and matches to a team by name downstream — no slug on this feed.
    assert_eq!(r.committed_school.as_deref(), Some("St. John's"));
    assert!(r.committed_school_slug.is_none());
    assert_eq!(r.commit_status.as_deref(), Some("Committed"));
    // Origin lands in the HS/city/state columns; intl uses a 3-letter code.
    assert_eq!(r.high_school.as_deref(), Some("Senegal"));
    assert_eq!(r.state.as_deref(), Some("SEN"));
    // Unranked: 0 solid stars, "NA" ranks/score → no composite metrics.
    assert_eq!(r.star_rating, Some(0));
    assert_eq!(r.composite_rank, None);
    assert_eq!(r.composite_rating, None);
}

#[test]
fn ranked_five_star_commit_parses() {
    // Marcus Spears Jr. — 5 solid stars, committed (no checkmark) to Texas.
    // The `.meta` carries a nested video `<a>` that must not pollute the origin.
    let rows = parse_commits_html(FIXTURE);
    let r = find_by_key(&rows, 46_149_740);
    assert_eq!(r.first_name.as_deref(), Some("Marcus"));
    assert_eq!(r.last_name.as_deref(), Some("Spears Jr."));
    assert_eq!(r.committed_school.as_deref(), Some("Texas"));
    assert_eq!(r.commit_status.as_deref(), Some("Committed"));
    assert_eq!(r.star_rating, Some(5));
    assert_eq!(r.high_school.as_deref(), Some("Dynamic Prep"));
    assert_eq!(r.city.as_deref(), Some("Dallas"));
    assert_eq!(r.state.as_deref(), Some("TX"));
    // The feed's `.score` is 247's 0–100 rating, a different metric from the
    // 0–1 composite — the ingest deliberately drops it, but the parser still
    // exposes the raw parse. `composite_rating` here is the "98" parsed as a
    // float; downstream `upsert_commit` does not persist it.
    assert_eq!(r.composite_rating, Some(98.0));
}

#[test]
fn signed_commit_detected_by_checkmark() {
    // Pedro Sancho Moraga — Spain, signed (LOI) to Washington State: bare
    // `.status` img + `<b class="checkmark">`.
    let rows = parse_commits_html(FIXTURE);
    let r = find_by_key(&rows, 46_168_887);
    assert_eq!(r.committed_school.as_deref(), Some("Washington State"));
    assert_eq!(r.commit_status.as_deref(), Some("Signed"));
}

#[test]
fn every_committed_row_carries_a_school() {
    // The whole value of this feed: unlike the ranked composite, every row is a
    // commitment and must name its destination so it can join to a team.
    let rows = parse_commits_html(FIXTURE);
    for r in &rows {
        assert!(
            r.committed_school.is_some(),
            "commits-feed row missing committed_school: key={}",
            r.recruit_key
        );
        assert!(
            matches!(r.commit_status.as_deref(), Some("Committed" | "Signed")),
            "commits-feed row has unexpected status {:?}: key={}",
            r.commit_status,
            r.recruit_key
        );
    }
}

#[test]
fn empty_and_sentinel_fragments_yield_no_rows() {
    // 247 serves a nav-only sentinel (no `a.ri-page__name-link`) past the last
    // data page — the paginator's stop signal.
    assert!(parse_commits_html("").is_empty());
    assert!(
        parse_commits_html(
            r#"<ul class="ri-page__list">
                 <li class="ri-page__list-item showmore_blk"><div>Show more</div></li>
                 <li class="ri-page__list-item"><div class="nav">no recruit here</div></li>
               </ul>"#
        )
        .is_empty()
    );
}
