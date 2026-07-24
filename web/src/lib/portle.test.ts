import { describe, expect, it } from 'vitest';
import type { PlayerRow } from '../api/client';
import {
  buildShare,
  compareGuess,
  filterPool,
  hash32,
  isAnswerable,
  localDateKey,
  poolHasSeedKeys,
  practiceAnswerByNonce,
  recordResult,
  EMPTY_STATS,
} from './portle';

// Minimal PlayerRow factory — every field the game reads has a default; tests
// override just what they exercise.
function player(over: Partial<PlayerRow> & { player_id: string }): PlayerRow {
  return {
    // Default the stable seed key to player_id so tests that only set player_id
    // still get distinct, deterministic answers (the daily seeds on natstat_id).
    natstat_id: over.player_id,
    name: over.player_id,
    team_id: null,
    team_name: null,
    conference: 'ACC',
    position: 'G',
    class_year: 'Fr',
    height_inches: 78,
    season: 2026,
    games_played: 20,
    minutes_per_game: 28,
    ppg: 12,
    rpg: null,
    apg: null,
    spg: null,
    bpg: null,
    topg: null,
    fg_pct: null,
    tp_pct: null,
    ft_pct: null,
    effective_fg_pct: null,
    true_shooting_pct: null,
    usage_rate: 20,
    offensive_rating: null,
    defensive_rating: null,
    net_rating: null,
    player_sos: null,
    campom: 10,
    campom_pct: null,
    campom_o: null,
    campom_d: null,
    ast_pct: null,
    tov_pct: null,
    orb_pct: null,
    drb_pct: null,
    stl_pct: null,
    blk_pct: null,
    ft_rate: null,
    ppg_pct: null,
    rpg_pct: null,
    apg_pct: null,
    spg_pct: null,
    bpg_pct: null,
    topg_pct: null,
    mpg_pct: null,
    usage_rate_pct: null,
    true_shooting_pct_pct: null,
    ast_pct_pct: null,
    tov_pct_pct: null,
    orb_pct_pct: null,
    drb_pct_pct: null,
    stl_pct_pct: null,
    blk_pct_pct: null,
    primary_class: 'Wizard',
    secondary_class: 'Bard',
    net_on_off: null,
    on_net_rtg: null,
    off_net_rtg: null,
    on_off_source: null,
    on_off_off_poss: null,
    rapm_net: null,
    rapm_paired_poss: null,
    ...over,
  };
}

describe('hash32', () => {
  it('is deterministic and spreads inputs', () => {
    expect(hash32('a')).toBe(hash32('a'));
    expect(hash32('a')).not.toBe(hash32('b'));
    expect(hash32('p5:2026:2026-07-01')).toBeGreaterThanOrEqual(0);
  });
});

describe('localDateKey', () => {
  it('formats local calendar date as YYYY-MM-DD', () => {
    // Construct with local Y/M/D to avoid TZ ambiguity.
    const d = new Date(2026, 6, 1, 15, 30); // 2026-07-01 local
    expect(localDateKey(d)).toBe('2026-07-01');
  });
});

describe('filterPool / isAnswerable', () => {
  const pool = [
    player({ player_id: 'acc-starter', conference: 'ACC', minutes_per_game: 30, campom: 8 }),
    player({ player_id: 'acc-bench', conference: 'ACC', minutes_per_game: 14, campom: 12 }),
    player({ player_id: 'mid-bench', conference: 'A-10', minutes_per_game: 12, campom: 2 }),
    player({ player_id: 'bigeast', conference: 'BIGEAST', minutes_per_game: 26, campom: 15 }),
    player({ player_id: 'no-campom', campom: null }),
    player({ player_id: 'no-class', primary_class: null }),
  ];

  it('gates out unanswerable rows (null campom / archetype) in every mode', () => {
    const byId = (id: string) => pool.find((p) => p.player_id === id)!;
    expect(isAnswerable(byId('no-campom'))).toBe(false);
    expect(isAnswerable(byId('no-class'))).toBe(false);
    for (const mode of ['p5', 'starters', 'campom10'] as const) {
      const ids = filterPool(pool, mode).map((p) => p.player_id);
      expect(ids).not.toContain('no-campom');
      expect(ids).not.toContain('no-class');
    }
  });

  it('p5 keeps only power conferences at >= 20 mpg', () => {
    const ids = filterPool(pool, 'p5').map((p) => p.player_id);
    expect(ids).toContain('acc-starter');
    expect(ids).toContain('bigeast');
    expect(ids).not.toContain('mid-bench'); // wrong conference
    expect(ids).not.toContain('acc-bench'); // P5 but sub-20 mpg
  });

  it('all-d1 (starters mode) keeps >= 20 mpg across all conferences — a p5 superset', () => {
    const ids = filterPool(pool, 'starters').map((p) => p.player_id);
    expect(ids).toContain('acc-starter');
    expect(ids).toContain('bigeast');
    expect(ids).not.toContain('acc-bench'); // sub-20 mpg
    expect(ids).not.toContain('mid-bench'); // sub-20 mpg
    // Same floor as p5, no conference gate → every p5 answer is in all-d1.
    const p5 = filterPool(pool, 'p5').map((p) => p.player_id);
    expect(p5.every((id) => ids.includes(id))).toBe(true);
  });

  it('campom10 keeps only campom > 10, ignoring conference and minutes', () => {
    const ids = filterPool(pool, 'campom10').map((p) => p.player_id);
    expect(ids).toContain('bigeast'); // campom 15
    expect(ids).toContain('acc-bench'); // campom 12, sub-20 mpg but high impact
    expect(ids).not.toContain('acc-starter'); // campom 8
    expect(ids).not.toContain('mid-bench'); // campom 2
  });
});

// The DAILY answer is now chosen server-side (issue #181); the client only
// resolves the pinned id. `pickByHash` remains the PRACTICE-mode picker, so its
// rendezvous properties are exercised here via `practiceAnswerByNonce`.
describe('practice rendezvous (pickByHash)', () => {
  const pool = Array.from({ length: 10 }, (_, i) =>
    player({ player_id: `p${i}`, conference: 'ACC', minutes_per_game: 30 }),
  );

  it('is stable regardless of pool order', () => {
    const a = practiceAnswerByNonce(pool, 'p5', 7);
    const b = practiceAnswerByNonce([...pool].reverse(), 'p5', 7);
    expect(a?.natstat_id).toBe(b?.natstat_id);
  });

  it('is deterministic per nonce and changes across nonces', () => {
    expect(practiceAnswerByNonce(pool, 'p5', 1)?.player_id).toBe(
      practiceAnswerByNonce(pool, 'p5', 1)?.player_id,
    );
    expect(practiceAnswerByNonce(pool, 'p5', 1)?.player_id).not.toBe(
      practiceAnswerByNonce(pool, 'p5', 2)?.player_id,
    );
  });

  // Keys on natstat_id, not player_id (#181): a data rebuild re-mints every UUID
  // but leaves natstat_id untouched, so the pick must not move.
  it('keys on natstat_id, so a player_id UUID churn does not move the pick', () => {
    const before = practiceAnswerByNonce(pool, 'p5', 3);
    const rebuilt = pool.map((p) => ({ ...p, player_id: `uuid-${p.natstat_id}-v2` }));
    const after = practiceAnswerByNonce(rebuilt, 'p5', 3);
    expect(after?.natstat_id).toBe(before?.natstat_id);
    expect(after?.player_id).not.toBe(before?.player_id);
  });

  it('returns null on an empty eligible pool', () => {
    expect(practiceAnswerByNonce([], 'p5', 1)).toBeNull();
  });
});

// Fail-closed guard for the DAILY resolve path (issue #181 tail): the client
// resolves the server-pinned id by natstat_id, so the pool must carry it. A
// frontend deployed ahead of the API that serves natstat_id would see it
// undefined; `poolHasSeedKeys` catches that so the page shows a "refreshing"
// state instead of rendering off an inconsistent pool.
describe('poolHasSeedKeys', () => {
  const pool = Array.from({ length: 10 }, (_, i) =>
    player({ player_id: `p${i}`, conference: 'ACC', minutes_per_game: 30 }),
  );

  it('is false when natstat_id is missing, true when present', () => {
    expect(poolHasSeedKeys(pool)).toBe(true);
    const skew = pool.map((p) => ({ ...p, natstat_id: undefined as unknown as string }));
    expect(poolHasSeedKeys(skew)).toBe(false);
    // A partial gap (one row missing the key) must also fail closed.
    const partial = [{ ...pool[0], natstat_id: undefined as unknown as string }, ...pool.slice(1)];
    expect(poolHasSeedKeys(partial)).toBe(false);
    expect(poolHasSeedKeys([])).toBe(false);
  });
});

describe('compareGuess', () => {
  const answer = player({
    player_id: 'answer',
    team_name: 'Duke',
    conference: 'ACC',
    class_year: 'Jr',
    position: 'F',
    primary_class: 'Wizard',
    secondary_class: 'Bard',
    ppg: 18,
    usage_rate: 26,
    campom: 15,
  });

  const cell = (guess: PlayerRow, key: string) =>
    compareGuess(guess, answer).find((c) => c.key === key)!;

  it('categorical exact match = hit, else miss', () => {
    const g = player({ player_id: 'g', team_name: 'Duke', conference: 'SEC' });
    expect(cell(g, 'team').state).toBe('hit');
    expect(cell(g, 'conference').state).toBe('miss');
  });

  it('class year gives a direction arrow toward the answer', () => {
    const younger = player({ player_id: 'y', class_year: 'Fr' });
    const c = cell(younger, 'class');
    expect(c.state).toBe('miss');
    expect(c.arrow).toBe('up'); // answer (Jr) is older than guess (Fr)
    const same = player({ player_id: 's', class_year: 'Jr' });
    expect(cell(same, 'class').state).toBe('hit');
  });

  it('numeric close band + arrow', () => {
    const near = player({ player_id: 'n', ppg: 19 }); // |18-19|=1 <= band 2
    const c = cell(near, 'ppg');
    expect(c.state).toBe('close');
    expect(c.arrow).toBe('down'); // answer (18) < guess (19)
    const spot = player({ player_id: 'x', ppg: 18 });
    expect(cell(spot, 'ppg').state).toBe('hit');
    const far = player({ player_id: 'f', ppg: 5 });
    expect(cell(far, 'ppg').state).toBe('miss');
    expect(cell(far, 'ppg').arrow).toBe('up');
  });

  it('height gives a direction arrow and ft-in display', () => {
    const answerTall = player({ player_id: 'tall', height_inches: 81 });
    const shorter = player({ player_id: 'sh', height_inches: 74 });
    const c = compareGuess(shorter, answerTall).find((x) => x.key === 'height')!;
    expect(c.state).toBe('miss');
    expect(c.arrow).toBe('up'); // answer (81) taller than guess (74)
    expect(c.display).toBe(`6'2"`); // 74 inches
    const close = compareGuess(player({ player_id: 'c', height_inches: 79 }), answerTall).find(
      (x) => x.key === 'height',
    )!;
    expect(close.state).toBe('close'); // within 3 inches
  });

  it('archetype: primary match hit, guessed primary == answer secondary close, else miss', () => {
    expect(cell(player({ player_id: 'a', primary_class: 'Wizard' }), 'archetype').state).toBe('hit');
    // guessed (displayed) primary equals the answer's secondary -> close
    expect(
      cell(player({ player_id: 'b', primary_class: 'Bard', secondary_class: 'Rogue' }), 'archetype')
        .state,
    ).toBe('close');
    // guessed SECONDARY matching the answer's primary no longer counts (only the
    // displayed primary is colored) -> miss
    expect(
      cell(player({ player_id: 'd', primary_class: 'Rogue', secondary_class: 'Wizard' }), 'archetype')
        .state,
    ).toBe('miss');
    expect(
      cell(player({ player_id: 'c', primary_class: 'Paladin', secondary_class: 'Druid' }), 'archetype')
        .state,
    ).toBe('miss');
  });

  it('campom O/D cells hit/close/miss on their own bands, null = miss', () => {
    const ans = player({ player_id: 'od', campom_o: 8, campom_d: 4 });
    const spotO = compareGuess(player({ player_id: 'o1', campom_o: 8 }), ans).find(
      (c) => c.key === 'campom_o',
    )!;
    expect(spotO.state).toBe('hit');
    const closeD = compareGuess(player({ player_id: 'd1', campom_d: 5.5 }), ans).find(
      (c) => c.key === 'campom_d',
    )!;
    expect(closeD.state).toBe('close'); // |4 - 5.5| = 1.5, within band 2
    // Guess missing the split (null) reads as a miss, not a hit against a null answer.
    const nullO = compareGuess(player({ player_id: 'o2', campom_o: null }), ans).find(
      (c) => c.key === 'campom_o',
    )!;
    expect(nullO.state).toBe('miss');
  });

  it('handles null attributes as non-green misses', () => {
    const g = player({
      player_id: 'z',
      ppg: null,
      conference: null,
      primary_class: null,
      secondary_class: null,
    });
    expect(cell(g, 'ppg').state).toBe('miss');
    expect(cell(g, 'conference').state).toBe('miss');
    expect(cell(g, 'archetype').state).toBe('miss');
  });
});

describe('buildShare', () => {
  const answer = player({ player_id: 'answer', ppg: 18, campom: 15, conference: 'ACC' });
  it('renders a header + emoji grid + url, no answer name', () => {
    const rows = [
      compareGuess(player({ player_id: 'g1', conference: 'SEC', ppg: 3, campom: 2 }), answer),
      compareGuess(answer, answer),
    ];
    const out = buildShare(rows, {
      mode: 'p5',
      won: true,
      daily: true,
      dateKey: '2026-07-01',
      url: 'https://x/portle',
    });
    expect(out).toContain('CamPom · Portle · 2026-07-01 · Power 5 · 2/10');
    expect(out).toContain('https://x/portle');
    expect(out).not.toContain('answer');
    expect(out.split('\n')).toHaveLength(4); // header + 2 grid rows + url
    expect(out).toMatch(/[🟩🟨⬛]/u);
  });
  it('labels a practice run and marks a loss with X', () => {
    const out = buildShare([compareGuess(player({ player_id: 'g' }), answer)], {
      mode: 'starters',
      won: false,
      daily: false,
    });
    expect(out).toContain('CamPom · Portle · Practice · All D1 · X/10');
  });
  it('annotates hinted solves', () => {
    const out = buildShare([compareGuess(answer, answer)], {
      mode: 'p5',
      won: true,
      hintsUsed: 2,
      dateKey: '2026-07-01',
    });
    expect(out).toContain('1/10 (2 hints)');
  });
});

describe('recordResult', () => {
  it('increments played/wins, bumps streak + distribution on a win', () => {
    const s1 = recordResult(EMPTY_STATS, { won: true, guesses: 3 });
    expect(s1.played).toBe(1);
    expect(s1.wins).toBe(1);
    expect(s1.streak).toBe(1);
    expect(s1.maxStreak).toBe(1);
    expect(s1.dist[2]).toBe(1); // solved in 3 -> index 2
  });

  it('counts a clean win (0 hints) but not a hinted one', () => {
    let s = recordResult(EMPTY_STATS, { won: true, guesses: 3, hintsUsed: 0 });
    expect(s.cleanWins).toBe(1);
    s = recordResult(s, { won: true, guesses: 2, hintsUsed: 1 });
    expect(s.wins).toBe(2);
    expect(s.cleanWins).toBe(1); // hinted win doesn't count as clean
  });

  it('resets streak but keeps maxStreak on a loss', () => {
    let s = recordResult(EMPTY_STATS, { won: true, guesses: 2 });
    s = recordResult(s, { won: true, guesses: 4 });
    expect(s.streak).toBe(2);
    s = recordResult(s, { won: false, guesses: 8 });
    expect(s.streak).toBe(0);
    expect(s.maxStreak).toBe(2);
    expect(s.played).toBe(3);
    expect(s.wins).toBe(2);
  });
});
