import { describe, expect, it } from 'vitest';
import type { ConferenceRanking, ConferenceStandingsRow } from '../api/client';
import { nextSort, sortRankings, sortStandings } from './conferenceTable';

const team = (team_name: string, over: Partial<ConferenceStandingsRow>) =>
  ({
    team_id: team_name,
    team_name,
    conference_wins: 0,
    conference_losses: 0,
    wins: 0,
    losses: 0,
    adj_em: null,
    adj_em_rank: null,
    adj_o: null,
    adj_d: null,
    sos: null,
    sos_rank: null,
    ...over,
  }) as ConferenceStandingsRow;

const names = (rows: { team_name: string }[]) => rows.map((r) => r.team_name);

describe('sortStandings', () => {
  it('keeps each team on its league-table place under a re-sort', () => {
    const rows = sortStandings(
      [team('First', { adj_em: 1 }), team('Second', { adj_em: 9 })],
      { key: 'adj_em', dir: 'desc' },
      false,
    );
    expect(rows.map((r) => [r.team_name, r.standing])).toEqual([
      ['Second', 2],
      ['First', 1],
    ]);
  });

  it('orders records by rate, then wins, then fewer losses, with 0-0 at .500', () => {
    const record = (name: string, w: number, l: number) =>
      team(name, { conference_wins: w, conference_losses: l });
    const rows = sortStandings(
      [
        record('0-0', 0, 0),
        record('0-2', 0, 2),
        record('1-0', 1, 0),
        record('3-1', 3, 1),
        record('2-0', 2, 0),
        record('0-1', 0, 1),
      ],
      { key: 'conference_record', dir: 'desc' },
      false,
    );
    expect(names(rows)).toEqual(['2-0', '1-0', '3-1', '0-0', '0-1', '0-2']);
  });

  it('reads an expected record as a rate, so a longer listed slate does not win', () => {
    const rows = sortStandings(
      [
        team('Long slate', { expected_conference_wins: 9, expected_conference_games: 18 }),
        team('Short slate', { expected_conference_wins: 1.8, expected_conference_games: 2 }),
        team('No slate', { expected_conference_wins: 0, expected_conference_games: 0 }),
      ],
      { key: 'conference_record', dir: 'desc' },
      true,
    );
    expect(names(rows)).toEqual(['Short slate', 'Long slate', 'No slate']);
  });

  it('keeps missing numbers at the bottom in both directions', () => {
    const teams = [team('Blank', {}), team('Low', { adj_o: 90 }), team('High', { adj_o: 120 })];
    expect(names(sortStandings(teams, { key: 'adj_o', dir: 'desc' }, false))).toEqual([
      'High',
      'Low',
      'Blank',
    ]);
    expect(names(sortStandings(teams, { key: 'adj_o', dir: 'asc' }, false))).toEqual([
      'Low',
      'High',
      'Blank',
    ]);
  });
});

describe('nextSort', () => {
  it('opens each column best-first and flips on a second click', () => {
    const start = { key: 'standing', dir: 'asc' } as const;
    expect(nextSort<'standing' | 'adj_em' | 'adj_d'>(start, 'adj_em').dir).toBe('desc');
    expect(nextSort<'standing' | 'adj_em' | 'adj_d'>(start, 'adj_d').dir).toBe('asc');
    expect(nextSort(start, 'standing').dir).toBe('desc');
  });
});

describe('sortRankings', () => {
  const league = (conference: string, rank: number, w: number | null, l: number | null) =>
    ({
      conference,
      strength_rank: rank,
      mean_adj_em: 10 - rank,
      non_conference_wins: w,
      non_conference_losses: l,
    }) as ConferenceRanking;

  it('sorts the name column by display label, not the stored code', () => {
    const label = (code: string) => (code === 'A10' ? 'Atlantic 10' : 'American');
    const rows = sortRankings(
      [league('A10', 1, 0, 0), league('Amer', 2, 0, 0)],
      { key: 'conference', dir: 'asc' },
      label,
    );
    expect(rows.map((r) => r.conference)).toEqual(['Amer', 'A10']);
  });

  it('sorts the non-conference record by rate and leaves a forecast in rank order', () => {
    const label = (code: string) => code;
    const played = sortRankings(
      [league('Worse', 1, 10, 10), league('Better', 2, 15, 5)],
      { key: 'non_conference', dir: 'desc' },
      label,
    );
    expect(played.map((r) => r.conference)).toEqual(['Better', 'Worse']);

    const forecast = sortRankings(
      [league('Second', 2, null, null), league('First', 1, null, null)],
      { key: 'non_conference', dir: 'desc' },
      label,
    );
    expect(forecast.map((r) => r.conference)).toEqual(['First', 'Second']);
  });
});
