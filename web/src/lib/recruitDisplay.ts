/// Pure display helpers for incoming-recruit rows on the projection pages.
/// Kept out of the components so the formatting (and the redshirt marker that
/// PR 1 introduced) is unit-testable without a component-render harness — the
/// repo's convention is pure-logic vitest, not jsdom/RTL.

/// The minimal recruit shape these helpers read. A subset of `ProjectedRecruit`
/// / `ProjectedRecruitDetail` so both payload types satisfy it.
export interface RecruitLineFields {
  name: string;
  composite_rank: number | null;
  star_rating: number | null;
  /// Committed but never played the (completed) target season — a redshirt /
  /// non-enrollment. Only ever true for a graded past season.
  did_not_play?: boolean;
}

/// One line of the Recruits-column hover tooltip: `#<rank> <name> (<stars>★)`,
/// with a ` — redshirt (did not play)` suffix when the recruit didn't play the
/// (completed) target season. Rank is omitted when unranked; a missing star
/// rating renders as `?`. Mirrors the RecruitCard tag on TeamDetail so the two
/// surfaces read the same.
export function recruitTooltipLine(r: RecruitLineFields): string {
  const rank = r.composite_rank != null ? `#${r.composite_rank} ` : '';
  const stars = r.star_rating ?? '?';
  const redshirt = r.did_not_play ? ' — redshirt (did not play)' : '';
  return `${rank}${r.name} (${stars}★)${redshirt}`;
}
