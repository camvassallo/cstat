import { useEffect, useRef, useState } from 'react';
import { fetchPlayers, type PlayerRow } from '../api/client';

const fmtPpg = (v: number | null | undefined) => (v != null ? v.toFixed(1) : '—');

/** Debounced player-search autocomplete. Extracted from PlayerCompare so the
 *  Compare page and Mystery Baller share one picker. Fires `onAdd(id)` and/or
 *  `onPick(row)` when a result is chosen (Compare only needs the id; the game
 *  wants the full row). Min 2 chars, 200ms debounce, 12-result cap; stale
 *  requests are dropped via a request counter. */
export function PlayerPicker({
  onAdd,
  onPick,
  disabled = false,
  max,
  existingIds = [],
  season,
  placeholder,
  hideStats = false,
}: {
  onAdd?: (id: string) => void;
  onPick?: (player: PlayerRow) => void;
  disabled?: boolean;
  max?: number;
  existingIds?: string[];
  season: number;
  placeholder?: string;
  /** Hide the "· N PPG" trailing stat (it's a hint category in Mystery
   *  Baller). Team name still shows for disambiguation. */
  hideStats?: boolean;
}) {
  const [search, setSearch] = useState('');
  const [results, setResults] = useState<PlayerRow[]>([]);
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const reqRef = useRef(0);

  useEffect(() => {
    const term = search.trim();
    const reqId = ++reqRef.current;
    if (term.length < 2) return;
    const handle = setTimeout(() => {
      setLoading(true);
      fetchPlayers({ search: term, limit: 12, season })
        .then((r) => {
          if (reqRef.current === reqId) setResults(r.players);
        })
        .finally(() => {
          if (reqRef.current === reqId) setLoading(false);
        });
    }, 200);
    return () => clearTimeout(handle);
  }, [search, season]);

  const filtered =
    search.trim().length >= 2 ? results.filter((r) => !existingIds.includes(r.player_id)) : [];

  const activePlaceholder = disabled
    ? max != null
      ? `Up to ${max} players`
      : 'Unavailable'
    : placeholder ?? 'Add player by name…';

  return (
    <div className="relative">
      <input
        type="text"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        placeholder={activePlaceholder}
        disabled={disabled}
        className="w-full bg-gray-800 border border-gray-600 rounded px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:border-blue-500 disabled:opacity-50"
      />
      {open && search.trim().length >= 2 && (
        <div className="absolute z-10 mt-1 w-full bg-gray-900 border border-gray-700 rounded shadow-lg max-h-72 overflow-y-auto">
          {loading && <div className="px-3 py-2 text-xs text-gray-500">Searching…</div>}
          {!loading && filtered.length === 0 && (
            <div className="px-3 py-2 text-xs text-gray-500">No players found</div>
          )}
          {filtered.map((p) => (
            <button
              key={p.player_id}
              type="button"
              onMouseDown={(e) => {
                e.preventDefault();
                onAdd?.(p.player_id);
                onPick?.(p);
                setSearch('');
                setResults([]);
              }}
              className="w-full text-left px-3 py-2 hover:bg-gray-800 text-sm flex items-center justify-between gap-3"
            >
              <span className="truncate">{p.name}</span>
              <span className="text-xs text-gray-500 truncate">
                {hideStats ? (p.team_name ?? '—') : `${p.team_name ?? '—'} · ${fmtPpg(p.ppg)} PPG`}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
