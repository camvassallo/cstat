import { useEffect, useState, type ReactNode } from 'react';
import {
  fetchScheduleRecord,
  type PredictionRecord,
  type ScheduleRecordResponse,
} from '../api/client';

function shortDate(iso: string): string {
  const [y, m, d] = iso.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d)).toLocaleDateString('en-US', {
    month: 'short',
    day: 'numeric',
    timeZone: 'UTC',
  });
}

function pct(wins: number, losses: number): string {
  const n = wins + losses;
  return n ? `${Math.round((100 * wins) / n)}%` : '';
}

function units(u: number): string {
  const r = Math.round(u * 10) / 10;
  return `${r > 0 ? '+' : r < 0 ? '−' : ''}${Math.abs(r).toFixed(1)}u`;
}

function unitsClass(u: number): string {
  if (u >= 0.05) return 'text-emerald-300';
  if (u <= -0.05) return 'text-red-300';
  return 'text-gray-400';
}

/** `W-L` (or `W-L-P`) with the win rate and, for bets, the profit. */
function RecordCell({
  wins,
  losses,
  pushes,
  u,
}: {
  wins: number;
  losses: number;
  pushes?: number;
  u?: number;
}) {
  if (wins + losses + (pushes ?? 0) === 0) return <span className="text-gray-600">—</span>;
  return (
    <span className="whitespace-nowrap tabular-nums">
      {wins}-{losses}
      {pushes ? `-${pushes}` : ''}
      <span className="text-gray-400 ml-1.5">{pct(wins, losses)}</span>
      {u != null && <span className={`ml-1.5 ${unitsClass(u)}`}>{units(u)}</span>}
    </span>
  );
}

function MissCell({ r }: { r: PredictionRecord }) {
  if (r.mae == null) return <span className="text-gray-600">—</span>;
  return (
    <span className="whitespace-nowrap tabular-nums">
      {r.mae.toFixed(1)}
      {r.mae_lined != null && r.line_mae != null && (
        <span
          className="text-gray-400 ml-1.5 text-xs"
          title={`On the ${r.lined_games} games with a spread: projection ${r.mae_lined.toFixed(
            1,
          )}, spread ${r.line_mae.toFixed(1)}`}
        >
          spread {r.line_mae.toFixed(1)}
        </span>
      )}
    </span>
  );
}

type Row = {
  label: string;
  help: string;
  cell: (r: PredictionRecord) => ReactNode;
};

const ROWS: Row[] = [
  {
    label: 'Games',
    help: 'Finished games graded on the projection shown before tipoff.',
    cell: (r) => <span className="tabular-nums">{r.games || '—'}</span>,
  },
  {
    label: 'Avg. miss',
    help:
      'How far the projected margin landed from the final margin, in points, on average. ' +
      'Beside it, the same for the last spread before tipoff, on the games that had one.',
    cell: (r) => <MissCell r={r} />,
  },
  {
    label: 'Winners',
    help: 'Games where the projected favorite won.',
    cell: (r) => <RecordCell wins={r.straight_up.wins} losses={r.straight_up.losses} />,
  },
  {
    label: 'Against the spread',
    help:
      'Taking whichever side the projected margin beats the spread on, only when it beats it ' +
      'by at least a point, or two points in November and December. At the listed price, or ' +
      '-110 when none is listed.',
    cell: (r) => (
      <RecordCell wins={r.ats.wins} losses={r.ats.losses} pushes={r.ats.pushes} u={r.ats.units} />
    ),
  },
  {
    label: 'Moneyline',
    help:
      'One unit on the side whose projected win chance beats the chance its odds imply by ' +
      'more than 2 percentage points, and never at odds longer than +500.',
    cell: (r) => (
      <RecordCell wins={r.moneyline.wins} losses={r.moneyline.losses} u={r.moneyline.units} />
    ),
  },
  {
    label: 'Over/under',
    help:
      'Over when the projected score totals above the line, under when below, only when the ' +
      'chance is worth the price. At the listed price, or -110 when none is listed.',
    cell: (r) => (
      <RecordCell
        wins={r.totals.wins}
        losses={r.totals.losses}
        pushes={r.totals.pushes}
        u={r.totals.units}
      />
    ),
  },
];

const hasTotals = (r: PredictionRecord) => r.totals.wins + r.totals.losses + r.totals.pushes > 0;

/** How the Schedule tab's projections have fared: the night on screen, the
 *  week through it, and the season through it. */
export default function ScheduleRecord({ date }: { date: string }) {
  const [data, setData] = useState<ScheduleRecordResponse | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    fetchScheduleRecord(date)
      .then((r) => {
        if (cancelled) return;
        setFailed(false);
        setData(r);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [date]);

  // A record that cannot load is not worth an error box above the slate.
  if (failed) return null;
  const loaded = data && data.through === date ? data : null;
  if (!loaded) return null;

  const season = loaded.season_to_date;
  if (season.games === 0) {
    return (
      <p className="text-xs text-gray-500 mb-4">
        The projections&rsquo; record fills in as this season&rsquo;s games are played.
      </p>
    );
  }

  const columns: { label: string; r: PredictionRecord }[] = [];
  if (loaded.day.games > 0) columns.push({ label: shortDate(loaded.through), r: loaded.day });
  columns.push({
    label: `${shortDate(loaded.recent_from)} – ${shortDate(loaded.through)}`,
    r: loaded.recent,
  });
  columns.push({ label: 'Season', r: season });
  const rows = ROWS.filter(
    (row) => row.label !== 'Over/under' || columns.some((c) => hasTotals(c.r)),
  );

  return (
    <section className="mb-5 rounded border border-gray-800 bg-gray-900/40">
      <div className="px-3 pt-2.5 pb-1 flex flex-wrap items-baseline gap-x-3">
        <h2 className="text-sm font-semibold text-gray-200">How the projections have done</h2>
        {loaded.first_graded && (
          <span className="text-xs text-gray-500">since {shortDate(loaded.first_graded)}</span>
        )}
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-gray-400 border-b border-gray-800">
              <th className="py-1.5 px-3 text-left font-medium" />
              {columns.map((c) => (
                <th key={c.label} className="py-1.5 px-3 text-left font-medium whitespace-nowrap">
                  {c.label}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.label} className="border-b border-gray-800/60 last:border-0">
                <th
                  scope="row"
                  className="py-1.5 px-3 text-left font-normal text-gray-400 whitespace-nowrap cursor-help"
                  title={row.help}
                >
                  {row.label}
                </th>
                {columns.map((c) => (
                  <td key={c.label} className="py-1.5 px-3">
                    {row.cell(c.r)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {season.preseason_games > 0 && (
        <p className="px-3 pb-2 pt-1 text-xs text-gray-500">
          The season includes {season.preseason_games} games projected from preseason expectations,
          before a team had played.
        </p>
      )}
    </section>
  );
}
