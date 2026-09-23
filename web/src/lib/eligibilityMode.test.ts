import { describe, expect, it } from 'vitest';
import { applyEligibilityMode } from '../lib/eligibilityMode';
import type { ProjectedTeam } from '../api/client';

// The Roster AdjEM column has to follow the eligibility toggle (#381).
//
// `roster_raw_adj_em` is the roster model's number for the 50/50 roster. In a
// what-if view the headline is a DIFFERENT roster, so pairing it with the
// 50/50 raw would fold the eligibility swing into history's share and make
// "recent form added X" wrong. This used to be handled by blanking the column
// — for 53% of the board and 31 of the top 50, since a pending 5-in-5 case is
// the norm among good teams. The API now serves the raw for each what-if
// roster; these pin the swap.
const team = (over: Partial<ProjectedTeam>): ProjectedTeam =>
  ({
    team_name: 'T',
    midpoint_adj_em: 10,
    projected_adj_o: 110,
    projected_adj_d: 100,
    adj_em_eligibility_in: 12,
    adj_em_eligibility_out: 8,
    adj_o_eligibility_in: 112,
    adj_o_eligibility_out: 108,
    adj_d_eligibility_in: 100,
    adj_d_eligibility_out: 100,
    roster_raw_adj_em: 9,
    roster_raw_eligibility_in: 11,
    roster_raw_eligibility_out: 7,
    eligibility_pending_count: 1,
    ...over,
  }) as ProjectedTeam;

describe('applyEligibilityMode', () => {
  it('leaves the weighted view untouched', () => {
    const t = team({});
    expect(applyEligibilityMode(t, 'weighted')).toBe(t);
  });

  it('swaps the raw along with the headline, so the history gap describes the roster shown', () => {
    for (const [mode, headline, raw] of [
      ['in', 12, 11],
      ['out', 8, 7],
    ] as const) {
      const got = applyEligibilityMode(team({}), mode);
      expect(got.midpoint_adj_em).toBe(headline);
      expect(got.roster_raw_adj_em).toBe(raw);
      // The number the column actually communicates: what the anchor added.
      expect(got.midpoint_adj_em! - got.roster_raw_adj_em!).toBe(headline - raw);
    }
  });

  it('never blanks the raw — the defect this fixes', () => {
    for (const mode of ['in', 'out'] as const) {
      expect(applyEligibilityMode(team({}), mode).roster_raw_adj_em).not.toBeNull();
    }
  });

  it('falls back to the weighted raw for a team with no pending case', () => {
    // No case means no what-if roster: the server sends nulls and the headline
    // does not move, so the 50/50 raw is the right answer in every mode.
    const t = team({
      eligibility_pending_count: 0,
      roster_raw_eligibility_in: null,
      roster_raw_eligibility_out: null,
      adj_em_eligibility_in: 10,
      adj_em_eligibility_out: 10,
    });
    for (const mode of ['in', 'out'] as const) {
      const got = applyEligibilityMode(t, mode);
      expect(got.roster_raw_adj_em).toBe(9);
      expect(got.midpoint_adj_em! - got.roster_raw_adj_em!).toBe(1);
    }
  });
});
