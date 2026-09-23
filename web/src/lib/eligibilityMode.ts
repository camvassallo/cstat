import type { ProjectedTeam } from '../api/client';

// How the eligibility-pending cohort enters the projected columns (#346).
// `weighted` ("50/50" on screen) is the served midpoint — every case at
// its 50/50 — and the default: it is the site's forecast, the number the team
// page headline and the game predictor's preseason anchor use, so the grid
// must agree with them until the reader asks a what-if. `in` / `out`
// swap in the server's "every case clears" / "none do" headlines; the draft
// declarants stay blended at their own probability in all three, which is why
// this cannot simply be the ceiling or the floor.
export type EligibilityMode = 'weighted' | 'in' | 'out';

// Rewrite the three projected columns for the chosen mode so every consumer
// downstream — the sort, the rank columns, the O/D percentile colors, the
// chips — reads the swapped value without knowing a toggle exists.
export function applyEligibilityMode(t: ProjectedTeam, mode: EligibilityMode): ProjectedTeam {
  if (mode === 'weighted') return t;
  // `roster_raw_adj_em` is the roster model's number for the 50/50 roster
  // only — the server derives one anchor per team from it and does not
  // swap the raw along with the headline. For a team with a pending case the
  // swapped headline is a different roster, so "recent form adds X" computed
  // against the 50/50 raw would fold the eligibility swing into history's
  // share — which is why this used to null the column instead (#381). The API
  // now serves the raw for each what-if roster, so the pairing stays honest
  // and the column follows the toggle. A team with no case has no what-if
  // raw and needs none: its headline does not move.
  const raw =
    (mode === 'in' ? t.roster_raw_eligibility_in : t.roster_raw_eligibility_out) ??
    t.roster_raw_adj_em;
  return mode === 'in'
    ? {
        ...t,
        midpoint_adj_em: t.adj_em_eligibility_in,
        projected_adj_o: t.adj_o_eligibility_in,
        projected_adj_d: t.adj_d_eligibility_in,
        roster_raw_adj_em: raw,
      }
    : {
        ...t,
        midpoint_adj_em: t.adj_em_eligibility_out,
        projected_adj_o: t.adj_o_eligibility_out,
        projected_adj_d: t.adj_d_eligibility_out,
        roster_raw_adj_em: raw,
      };
}
