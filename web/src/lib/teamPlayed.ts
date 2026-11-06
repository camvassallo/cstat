import type { ScheduleEntry } from '../api/client';

/// Has this game a final score? Both sides are required: a partial ingest can
/// leave one populated.
export function hasResult(g: Pick<ScheduleEntry, 'team_score' | 'opponent_score'>): boolean {
  return g.team_score != null && g.opponent_score != null;
}

/// Has this team played, going by its schedule rather than its record? W-L
/// counts box-score rows, so a game whose box has not landed yet is final on
/// the schedule and missing from the record. Texas A&M read 0-0 on a 97-66 win
/// that way, and a 0-0 record sent the page to the projection, hiding the score.
export function teamHasPlayed(
  schedule: readonly Pick<ScheduleEntry, 'team_score' | 'opponent_score'>[],
): boolean {
  return schedule.some(hasResult);
}
