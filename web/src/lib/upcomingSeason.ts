/** Resolve the upcoming (not-yet-played) projection season — the Future tab's
 *  target — from whatever `/api/seasons` has told us so far.
 *
 *  Pure and extracted (#386) because the rule has three inputs and two call
 *  sites in `components/season.ts` — the sync accessor and the hook's state
 *  update — which were the same expression written twice and free to drift.
 *
 *  The ordering is the content:
 *
 *  1. **The API's `upcoming`.** Read from `team_preseason_projection`, so it
 *     reflects a season that can actually be served rather than one assumed
 *     to exist.
 *  2. **The API's `default` + 1.** `upcoming` is legitimately null whenever
 *     nothing is projected past the newest played season, and the Future tab
 *     still has to point somewhere.
 *  3. **The hardcoded newest + 1.** First paint and API-unreachable only.
 *
 *  Step 2 exists rather than falling straight to step 3 because step 3 goes
 *  stale on a schedule: `AVAILABLE_SEASONS_FALLBACK` is hand-edited, so from
 *  the first game of a new season until someone remembers to bump it, `newest
 *  + 1` names the season currently being played — which would point the
 *  Future tab at the present. Chaining through the API's default keeps the
 *  fallback correct without anyone editing anything.
 */
export function resolveUpcomingSeason(
  apiUpcoming: number | null | undefined,
  apiDefault: number | null | undefined,
  fallbackNewestPlayed: number,
): number {
  if (apiUpcoming != null) return apiUpcoming;
  if (apiDefault != null) return apiDefault + 1;
  return fallbackNewestPlayed + 1;
}
