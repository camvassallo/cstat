import type { PlayerRow } from '../../api/client';
import {
  GUESS_COLUMNS,
  type Arrow,
  type CellState,
  type GuessCell,
} from '../../lib/portle';
import { classColor } from '../archetypeColors';

export interface GuessRow {
  player: PlayerRow;
  cells: GuessCell[];
}

const STATE_CLASS: Record<CellState, string> = {
  hit: 'bg-emerald-600/30 border-emerald-500/40 text-emerald-100',
  close: 'bg-amber-500/25 border-amber-500/40 text-amber-100',
  miss: 'bg-gray-700/40 border-gray-600/40 text-gray-300',
};

function arrowGlyph(a: Arrow): string {
  if (a === 'up') return ' ▲';
  if (a === 'down') return ' ▼';
  return '';
}

function Cell({ cell }: { cell: GuessCell }) {
  // Tint the archetype label with its class color for flavor; other columns
  // rely on the state background for their signal.
  const style =
    cell.key === 'archetype' && cell.display !== '—'
      ? { color: classColor(cell.display) }
      : undefined;
  return (
    <div
      className={`flex h-12 min-w-0 items-center justify-center rounded border px-1 text-center text-xs font-medium ${STATE_CLASS[cell.state]}`}
      title={`${cell.label}: ${cell.display}${cell.arrow ? (cell.arrow === 'up' ? ' (answer is higher)' : ' (answer is lower)') : ''}`}
    >
      <span className="truncate" style={style}>
        {cell.display}
        {arrowGlyph(cell.arrow)}
      </span>
    </div>
  );
}

/** The stack of guessed rows plus a sticky column header. Horizontally
 *  scrollable on phones; the name column stays put. Names link to the player's
 *  detail page in a new tab so an in-progress game isn't lost. */
export function GuessGrid({ rows, season }: { rows: GuessRow[]; season: number }) {
  if (rows.length === 0) return null;
  // Name column + one per attribute.
  const gridTemplate = `minmax(9rem, 1.4fr) repeat(${GUESS_COLUMNS.length}, minmax(3.5rem, 1fr))`;
  return (
    <div className="overflow-x-auto">
      <div className="min-w-[40rem] space-y-1">
        <div
          className="grid gap-1 px-1 text-[10px] uppercase tracking-wide text-gray-500"
          style={{ gridTemplateColumns: gridTemplate }}
        >
          <div className="flex items-center">Guess</div>
          {GUESS_COLUMNS.map((c) => (
            <div key={c.key} className="flex items-center justify-center text-center">
              {c.label}
            </div>
          ))}
        </div>
        {rows.map((row) => (
          <div
            key={row.player.player_id}
            className="grid gap-1"
            style={{ gridTemplateColumns: gridTemplate }}
          >
            <div className="flex h-12 items-center rounded border border-gray-700 bg-gray-800/60 px-2 text-sm font-medium text-gray-100">
              <a
                href={`/players/${row.player.player_id}?season=${season}`}
                target="_blank"
                rel="noopener noreferrer"
                className="truncate hover:text-blue-300 hover:underline"
              >
                {row.player.name}
              </a>
            </div>
            {row.cells.map((cell) => (
              <Cell key={cell.key} cell={cell} />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
