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
 *  2. **The API's `default`.** `upcoming` is legitimately null whenever
 *     nothing is projected past the newest played season — every in-season
 *     night until the next season's teams exist — and the Future tab still
 *     has to point somewhere. The default is then the season being played,
 *     whose projection does exist. This used to be `default + 1`, which named
 *     a season with no projection rows the night 2027 tipped off: every
 *     picker that prepends `upcoming` offered an empty 2028.
 *  3. **The hardcoded newest + 1.** First paint and API-unreachable only.
 *
 *  Step 2 exists rather than falling straight to step 3 because step 3 goes
 *  stale on a schedule: `AVAILABLE_SEASONS_FALLBACK` is hand-edited, so it
 *  can name a season a year out of date. Chaining through the API's default
 *  keeps the answer correct without anyone editing anything.
 *
 *  Consequence for callers: this is "the newest season with a projection",
 *  NOT "a season nobody has played". Ask `seasons.includes(s)` for that.
 */
export function resolveUpcomingSeason(
  apiUpcoming: number | null | undefined,
  apiDefault: number | null | undefined,
  fallbackNewestPlayed: number,
): number {
  if (apiUpcoming != null) return apiUpcoming;
  if (apiDefault != null) return apiDefault;
  return fallbackNewestPlayed + 1;
}
