/// Is a transfer-portal player still on the board, or has he landed somewhere?
///
/// Kept out of the component so the rule is unit-testable without a render
/// harness — the repo's convention is pure-logic vitest, not jsdom/RTL.
///
/// 247's own vocabulary is Entered / Committed / Withdrawn, but Withdrawn never
/// reaches the portal page (the route drops it, because a withdrawal is a
/// player staying put rather than a portal entry). What's left is a clean
/// binary, which is what the page's filter chips expose.

/// The minimal row shape the rule reads. A subset of the portal page's
/// `RankedTransfer`, so that type satisfies it.
export interface TransferAvailabilityFields {
  status: string;
  next_team: string | null;
}

export type Availability = 'committed' | 'available';

/// Committed if 247 says so **or** a destination is showing.
///
/// Deliberately an OR rather than either field alone: the two disagree on a
/// small number of rows in both directions — for the 2026 class, 5 `Entered`
/// rows carry a destination, and for 2025, 3 `Committed` rows carry none — and
/// the filter must never contradict the "Next" column rendered beside it. A row
/// displaying a school while filed under "Available" reads as a bug to the user
/// whichever field is technically the more correct one.
///
/// The OR also keeps the two buckets a true partition, so the chip counts
/// always sum to the unfiltered total.
export function availabilityOf(t: TransferAvailabilityFields): Availability {
  return t.status === 'Committed' || t.next_team != null ? 'committed' : 'available';
}
