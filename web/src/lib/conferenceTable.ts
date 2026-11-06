import { compareValues, type SortDir } from '../components/tableSort';
import type { ConferenceRanking, ConferenceStandingsRow } from '../api/client';

export type Sort<K> = { key: K; dir: SortDir };

/** A team's line in the standings, carrying its place in the league table so
 *  the `#` stays with the row under any re-sort (issue #121). */
export type StandingsRow = ConferenceStandingsRow & { standing: number };

export type StandingsSortKey =
  | 'standing'
  | 'team_name'
  | 'conference_record'
  | 'expected_conference_games'
  | 'overall_record'
  | 'projected_games'
  | 'adj_em'
  | 'adj_o'
  | 'adj_d'
  | 'sos';

export type RankingSortKey =
  | 'strength_rank'
  | 'conference'
  | 'teams'
  | 'mean_adj_em'
  | 'mean_adj_o'
  | 'mean_adj_d'
  | 'non_conference'
  | 'top_team_adj_em';

/** Best-first direction for a column on its first click. Names and ranks read
 *  from the top, and a lower adjusted defense is better. */
export function openingDir(key: StandingsSortKey | RankingSortKey): SortDir {
  const ascending: (StandingsSortKey | RankingSortKey)[] = [
    'standing',
    'team_name',
    'adj_d',
    'strength_rank',
    'conference',
    'mean_adj_d',
  ];
  return ascending.includes(key) ? 'asc' : 'desc';
}

export function nextSort<K extends StandingsSortKey | RankingSortKey>(
  current: Sort<K>,
  key: K,
): Sort<K> {
  if (current.key === key) return { key, dir: current.dir === 'asc' ? 'desc' : 'asc' };
  return { key, dir: openingDir(key) };
}

/** Win rate first, so 3-1 beats 3-4; then wins, so 2-0 beats 1-0; then fewer
 *  losses, so 0-1 beats 0-2. A team with no games sits at .500 rather than the
 *  bottom: 0-0 is above 0-1 and below 1-0. */
function compareRecords(
  a: { wins: number; losses: number },
  b: { wins: number; losses: number },
  dir: SortDir,
): number {
  const rate = (r: { wins: number; losses: number }) =>
    r.wins + r.losses > 0 ? r.wins / (r.wins + r.losses) : 0.5;
  return (
    compareValues(rate(a), rate(b), dir) ||
    compareValues(a.wins, b.wins, dir) ||
    compareValues(-a.losses, -b.losses, dir)
  );
}

/** An expected record reads as a rate, never as raw wins: the schedule is
 *  published in pieces, so raw expected wins ranks teams by how many of their
 *  games are listed. Same reason the API orders the projected table this way. */
function expectedRate(wins: number | null | undefined, games: number | null | undefined) {
  return wins != null && games ? wins / games : null;
}

function compareStandings(
  a: StandingsRow,
  b: StandingsRow,
  { key, dir }: Sort<StandingsSortKey>,
  projected: boolean,
): number {
  switch (key) {
    case 'conference_record':
      return projected
        ? compareValues(
            expectedRate(a.expected_conference_wins, a.expected_conference_games),
            expectedRate(b.expected_conference_wins, b.expected_conference_games),
            dir,
          )
        : compareRecords(
            { wins: a.conference_wins ?? 0, losses: a.conference_losses ?? 0 },
            { wins: b.conference_wins ?? 0, losses: b.conference_losses ?? 0 },
            dir,
          );
    case 'overall_record':
      return projected
        ? compareValues(
            expectedRate(a.expected_wins, a.projected_games),
            expectedRate(b.expected_wins, b.projected_games),
            dir,
          )
        : compareRecords(
            { wins: a.wins ?? 0, losses: a.losses ?? 0 },
            { wins: b.wins ?? 0, losses: b.losses ?? 0 },
            dir,
          );
    default:
      return compareValues(a[key], b[key], dir);
  }
}

/** The league table in the chosen order. Ties keep league-table order. */
export function sortStandings(
  teams: ConferenceStandingsRow[],
  sort: Sort<StandingsSortKey>,
  projected: boolean,
): StandingsRow[] {
  return teams
    .map((t, i) => ({ ...t, standing: i + 1 }))
    .sort((a, b) => compareStandings(a, b, sort, projected) || a.standing - b.standing);
}

/** The conference board in the chosen order. `label` is the display name, which
 *  is what a reader expects the name column to sort by. Ties keep rank order. */
export function sortRankings(
  rows: ConferenceRanking[],
  { key, dir }: Sort<RankingSortKey>,
  label: (code: string) => string,
): ConferenceRanking[] {
  const compare = (a: ConferenceRanking, b: ConferenceRanking) => {
    if (key === 'conference') return compareValues(label(a.conference), label(b.conference), dir);
    if (key === 'non_conference') {
      if (a.non_conference_wins == null || b.non_conference_wins == null) return 0;
      return compareRecords(
        { wins: a.non_conference_wins, losses: a.non_conference_losses ?? 0 },
        { wins: b.non_conference_wins, losses: b.non_conference_losses ?? 0 },
        dir,
      );
    }
    return compareValues(a[key], b[key], dir);
  };
  return [...rows].sort((a, b) => compare(a, b) || a.strength_rank - b.strength_rank);
}
