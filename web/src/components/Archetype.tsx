import { useState, type ReactNode } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import type { PlayerArchetype, SimilarPlayer } from '../api/client';
import { classColor, classTagline, classTitle, provisionalMeta } from './archetypeColors';
import { slotToken } from '../lib/compareSlots';
import ModeToggle, { type ModeToggleOption } from './ModeToggle';
import { seasonHref, useSeason } from './season';
import { useDismissOnOutside } from './useDismissOnOutside';

/// Styled tooltip for any class label — mirrors the look of the affinity
/// popover on `ArchetypeBadge`. Tap (or hover, on pointer-fine devices) to
/// open; tap outside or press Escape to dismiss. Pass `extra` to append a
/// secondary line (e.g. "27.6% of minutes · 2 players").
export function ClassTooltip({
  cls,
  children,
  extra,
  asBlock = false,
}: {
  cls: string;
  children: ReactNode;
  extra?: ReactNode;
  /// Render the wrapper as a block-level element. Use this when the trigger
  /// is itself a block (e.g. flex bar segments) so layout isn't disrupted.
  asBlock?: boolean;
}) {
  const color = classColor(cls);
  const tagline = classTagline(cls);
  const [open, setOpen] = useState(false);
  const ref = useDismissOnOutside(open, () => setOpen(false));
  // Use a div when the trigger is itself a block (avoids invalid
  // span-wraps-div HTML). Callback ref accepts both element types.
  const setRef = (node: HTMLElement | null) => {
    ref.current = node;
  };
  const wrapperProps = {
    ref: setRef,
    className: `relative ${asBlock ? 'block h-full' : 'inline-block'}`,
    onMouseEnter: () => setOpen(true),
    onMouseLeave: () => setOpen(false),
  };
  const triggerProps = {
    className: 'cursor-pointer block h-full',
    onClick: (e: React.MouseEvent) => {
      e.stopPropagation();
      setOpen((v) => !v);
    },
  };
  const tooltip = (
    <span
      className={`absolute left-1/2 -translate-x-1/2 top-full mt-2 z-20 w-48 bg-gray-900 border border-gray-700 rounded-lg shadow-xl px-3 py-2 transition-opacity text-left whitespace-normal ${
        open ? 'opacity-100 visible' : 'opacity-0 invisible pointer-events-none'
      }`}
    >
      <span className="block text-xs font-bold" style={{ color }}>
        {cls}
      </span>
      {tagline && (
        <span className="block text-[11px] text-gray-300 mt-0.5 normal-case font-normal tracking-normal">
          {tagline}
        </span>
      )}
      {extra && (
        <span className="block text-[10px] text-gray-400 mt-1 normal-case font-normal tracking-normal">
          {extra}
        </span>
      )}
    </span>
  );
  if (asBlock) {
    return (
      <div {...wrapperProps}>
        <div {...triggerProps}>{children}</div>
        {tooltip}
      </div>
    );
  }
  return (
    <span {...wrapperProps}>
      <span {...triggerProps}>{children}</span>
      {tooltip}
    </span>
  );
}

export function ArchetypeBadge({
  archetype,
  size = 'md',
}: {
  archetype: PlayerArchetype;
  size?: 'sm' | 'md';
}) {
  const primaryColor = classColor(archetype.primary_class);
  const primaryTagline = classTagline(archetype.primary_class);
  const ranked = Object.entries(archetype.affinity_scores)
    .sort((a, b) => b[1] - a[1]);

  // Cold-start: a prior-season seed held until the player clears this season's
  // >=10 GP gate. Rendered muted + dashed with the source year, so it never
  // reads as a settled current-season label.
  const provisional = archetype.provisional === true;
  const sourceSeason = archetype.source_season ?? null;
  const shortYear = sourceSeason ? `'${String(sourceSeason).slice(2)}` : null;

  // Compact pill + hover popover with full affinity ranking.
  const sizing =
    size === 'sm'
      ? 'text-[10px] px-2 py-0.5'
      : 'text-xs px-2.5 py-1';

  const titleStr =
    (archetype.secondary_class
      ? `${classTitle(archetype.primary_class)} / ${classTitle(archetype.secondary_class)}`
      : classTitle(archetype.primary_class)) +
    (provisional && sourceSeason ? ` · provisional, carried over from ${sourceSeason}` : '');

  const [open, setOpen] = useState(false);
  const ref = useDismissOnOutside(open, () => setOpen(false));
  const setRef = (node: HTMLElement | null) => {
    ref.current = node;
  };

  return (
    <div
      ref={setRef}
      className="relative inline-block"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        className={`inline-flex items-center gap-1.5 ${sizing} rounded-full font-bold uppercase tracking-wide cursor-pointer ${
          provisional ? 'border border-dashed' : 'ring-1'
        }`}
        style={{
          background: primaryColor + (provisional ? '14' : '22'),
          color: provisional ? primaryColor + 'cc' : primaryColor,
          // ring/border color via inline style (Tailwind uses DEFAULT).
          boxShadow: provisional ? undefined : `inset 0 0 0 1px ${primaryColor}66`,
          borderColor: provisional ? primaryColor + '99' : undefined,
        }}
        title={titleStr}
        aria-expanded={open}
      >
        <span
          className="inline-block w-1.5 h-1.5 rounded-full"
          style={{ background: primaryColor, opacity: provisional ? 0.6 : 1 }}
        />
        {archetype.primary_class}
        {archetype.secondary_class && (
          <span
            className="font-normal opacity-75"
            style={{ color: classColor(archetype.secondary_class) }}
          >
            / {archetype.secondary_class}
          </span>
        )}
        {provisional && shortYear && (
          <span className="font-normal opacity-70 lowercase tracking-normal text-[0.85em] ml-0.5">
            {shortYear}
          </span>
        )}
      </button>
      <div
        className={`absolute left-0 top-full mt-2 z-20 w-64 bg-gray-900 border border-gray-700 rounded-lg shadow-xl p-3 transition-opacity ${
          open ? 'opacity-100 visible' : 'opacity-0 invisible pointer-events-none'
        }`}
      >
        {primaryTagline && (
          <div className="text-xs font-bold mb-1" style={{ color: primaryColor }}>
            {archetype.primary_class}
            <span className="font-normal text-gray-400"> — {primaryTagline}</span>
          </div>
        )}
        {provisional && (
          <div className="flex items-start gap-1.5 mb-2 rounded bg-amber-500/10 px-2 py-1.5">
            <span className="mt-px text-amber-400 text-xs leading-none">◐</span>
            <span className="text-[10px] text-amber-200/90 normal-case font-normal tracking-normal leading-snug">
              {sourceSeason ? `Provisional — last season's archetype (${sourceSeason}).` : 'Provisional archetype.'}{' '}
              Updates to this season once they reach 10 games.
            </span>
          </div>
        )}
        <div className="text-[10px] font-bold text-gray-500 mb-2 uppercase tracking-wider">
          Class Affinity
        </div>
        <div className="space-y-1">
          {ranked.map(([cls, score]) => {
            const pct = Math.max(0, Math.min(1, score));
            const c = classColor(cls);
            return (
              <div key={cls} className="flex items-center gap-2 text-xs">
                <div className="w-16 truncate" style={{ color: c }}>
                  {cls}
                </div>
                <div className="flex-1 bg-gray-800 rounded h-1.5 overflow-hidden">
                  <div
                    className="h-1.5"
                    style={{ width: `${pct * 100}%`, background: c }}
                  />
                </div>
                <div className="w-9 text-right text-gray-400">
                  {(score * 100).toFixed(0)}%
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

// Compare flow caps at 4 players total (matches the API's MAX_COMPARE_PLAYERS).
// One slot is reserved for the current player, leaving 3 for selection here.
const MAX_SIMILAR_COMPARE_SELECTIONS = 3;

/// Which seasons the neighbour search draws from. `season` is the default and
/// leaves this panel exactly as it was before cross-year existed.
export type SimilarMode = 'season' | 'year';

/// Wording is fixed by `ModeToggle`'s cross-year contract, so this panel, the
/// Compare page and Predict all read the same. Hoisted to avoid re-allocating
/// per render.
const SIMILAR_MODE_OPTIONS: readonly ModeToggleOption<SimilarMode>[] = [
  { value: 'year', label: 'Any year', title: 'Search every season for comparable players' },
  { value: 'season', label: 'Season', title: 'Search only this season' },
];

export function SimilarPlayers({
  players,
  title = 'Most Similar Players',
  currentPlayerId,
  mode,
  onModeChange,
  loading = false,
}: {
  players: SimilarPlayer[];
  title?: string;
  /// When provided, each tile gets a selection checkbox and a "Compare" button
  /// appears below the carousel, deep-linking to /players/compare with this
  /// player as slot 1 and the selected similar players filling slots 2-4.
  currentPlayerId?: string;
  /// Pass BOTH to put the "Any year | Season" toggle in the panel header. The
  /// caller owns the mode because it owns the fetch — cross-year is a
  /// different request, not a different way of rendering the same rows.
  /// Omitting them keeps the panel read-only and header-toggle-free, which is
  /// what a caller that fetches one fixed mode wants.
  mode?: SimilarMode;
  onModeChange?: (mode: SimilarMode) => void;
  /// True while a request for a mode OTHER than the one `players` came from is
  /// in flight. Only meaningful alongside the toggle: the cross-year search is
  /// an order of magnitude slower than the single-season one (~280 ms vs
  /// ~22 ms), so a flip with no feedback looks like a dead control.
  loading?: boolean;
}) {
  const navigate = useNavigate();
  const { season } = useSeason();
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const showToggle = mode != null && onModeChange != null;
  const crossYear = mode === 'year';

  // Without the toggle there is nothing to interact with, so an empty list
  // means an absent panel — the behaviour every caller had before cross-year.
  // WITH the toggle the panel has to survive an empty result, or a mode that
  // finds nothing takes away the only control that gets you back out of it.
  if (players.length === 0 && !showToggle) return null;

  const compareEnabled = currentPlayerId != null;

  // Selection is keyed by `player_id`, and flipping the mode swaps the whole
  // set of neighbours underneath it. Resolve ticks against the CURRENT list so
  // a stale one can neither be rendered nor eat a compare slot.
  const byId = new Map(players.map((p) => [p.player_id, p]));
  const selectedRows = [...selected]
    .map((id) => byId.get(id))
    .filter((p): p is SimilarPlayer => p != null);

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set([...prev].filter((s) => byId.has(s)));
      if (next.has(id)) {
        next.delete(id);
      } else if (next.size < MAX_SIMILAR_COMPARE_SELECTIONS) {
        next.add(id);
      }
      return next;
    });
  };
  const launchCompare = () => {
    if (!currentPlayerId || selectedRows.length === 0) return;
    // Cross-year: pin every slot to its own year, including this player's. The
    // Compare page reads its mode back off the `@year` suffixes, so pinning is
    // also what makes the destination open in cross-year mode. In season mode
    // the tokens stay bare UUIDs and the link is byte-identical to before.
    const ids = crossYear
      ? [
          slotToken(currentPlayerId, season),
          ...selectedRows.map((p) => slotToken(p.player_id, p.season)),
        ]
      : [currentPlayerId, ...selectedRows.map((p) => p.player_id)];
    navigate(seasonHref(`/players/compare?ids=${ids.join(',')}`, season));
  };

  return (
    <div className="bg-gray-800 rounded-lg p-5">
      <div className="flex flex-wrap items-start justify-between gap-3 mb-1">
        <h2 className="text-lg font-bold">{title}</h2>
        {showToggle && (
          <ModeToggle
            options={SIMILAR_MODE_OPTIONS}
            value={mode}
            onChange={onModeChange}
            ariaLabel="Comparable-player search range"
            className="self-start shrink-0"
          />
        )}
      </div>
      {crossYear && (
        <p className="text-xs text-gray-500 mb-2 max-w-2xl">
          Searching every season. Expect the closest matches to still lean
          toward this player's own era — players shoot better and turn the ball
          over less than they did a decade ago, so the same style of play puts
          up different numbers in 2015 than it does now.
        </p>
      )}
      {compareEnabled && players.length > 0 && (
        <p className="text-xs text-gray-500 mb-3">
          Tick up to {MAX_SIMILAR_COMPARE_SELECTIONS} to compare side by side.
        </p>
      )}
      {players.length === 0 && (
        <p className="text-sm text-gray-500">
          {loading ? 'Searching…' : 'No comparable players found.'}
        </p>
      )}
      <div
        className={`grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-3 ${
          loading ? 'opacity-50 transition-opacity' : ''
        }`}
        aria-busy={loading || undefined}
      >
        {players.map((p) => {
          const c = classColor(p.primary_class);
          const simPct = Math.round(p.similarity * 100);
          const prov = provisionalMeta(p);
          const isSelected = selected.has(p.player_id);
          const atCap =
            !isSelected && selectedRows.length >= MAX_SIMILAR_COMPARE_SELECTIONS;

          const tileBody = (
            <>
              <div className="font-medium text-sm truncate pr-6">{p.name}</div>
              <div className="text-xs text-gray-400 truncate">
                {p.team_name ?? '—'}
                {/* The year only earns a slot when the list spans years; in
                    season mode every row is the season already in the URL. */}
                {crossYear && <span className="text-gray-500"> · {p.season}</span>}
              </div>
              {/* Cross-year keeps the target's OWN other seasons, which are
                  often the nearest neighbours there are. "This player, a year
                  later" answers a different question from "someone else who
                  plays like this", so it is labelled rather than passed off as
                  a comp. */}
              {p.is_self && (
                <div className="mt-1 inline-block rounded bg-amber-900/40 px-1.5 py-px text-[10px] font-medium text-amber-200 ring-1 ring-amber-500/40">
                  Same player, another season
                </div>
              )}
              <div className="flex items-center gap-2 mt-2">
                <ClassTooltip cls={p.primary_class} extra={prov.note ?? undefined}>
                  <span
                    className={`text-xs font-bold uppercase tracking-wide ${
                      prov.provisional ? 'opacity-70' : ''
                    }`}
                    style={{ color: c }}
                  >
                    {p.primary_class}
                  </span>
                </ClassTooltip>
                {p.secondary_class && (
                  <ClassTooltip cls={p.secondary_class}>
                    <span
                      className="text-xs opacity-70"
                      style={{ color: classColor(p.secondary_class) }}
                    >
                      / {p.secondary_class}
                    </span>
                  </ClassTooltip>
                )}
                {prov.shortYear && (
                  <span className="text-[10px] text-gray-500 lowercase">{prov.shortYear}</span>
                )}
              </div>
              <div className="mt-2 flex items-center gap-2">
                <div className="flex-1 h-1 bg-gray-700 rounded overflow-hidden">
                  <div
                    className="h-1"
                    style={{ width: `${simPct}%`, background: c }}
                  />
                </div>
                <span className="text-[10px] text-gray-500">{simPct}%</span>
              </div>
            </>
          );

          return (
            <div
              key={p.player_id}
              className={`relative bg-gray-900 rounded transition-colors border-l-4 ${
                isSelected ? 'ring-1 ring-blue-500' : ''
              }`}
              style={{ borderLeftColor: c }}
            >
              {compareEnabled && (
                <label
                  className={`absolute top-2 right-2 z-10 flex items-center justify-center w-5 h-5 rounded border ${
                    isSelected
                      ? 'bg-blue-500 border-blue-500'
                      : 'bg-gray-900/70 border-gray-600 hover:border-gray-400'
                  } ${atCap ? 'opacity-40 cursor-not-allowed' : 'cursor-pointer'}`}
                  title={
                    atCap
                      ? `Cap of ${MAX_SIMILAR_COMPARE_SELECTIONS} selections reached`
                      : isSelected
                        ? 'Remove from compare'
                        : 'Add to compare'
                  }
                  onClick={(e) => e.stopPropagation()}
                >
                  <input
                    type="checkbox"
                    checked={isSelected}
                    disabled={atCap}
                    onChange={() => toggle(p.player_id)}
                    className="sr-only"
                  />
                  {isSelected && (
                    <span className="text-[10px] font-bold text-white">✓</span>
                  )}
                </label>
              )}
              {/* Anchor on the ROW's season, not the page's. Cross-year rows
                  come from other years and player UUIDs are season-scoped, so
                  a link carrying the page season would resolve back to a
                  different season — or 404 on someone who wasn't in D-I then.
                  In season mode `p.season` IS the page season and `seasonHref`
                  still drops the param on the default year, so those links are
                  unchanged. */}
              <Link
                to={seasonHref(`/players/${p.player_id}`, p.season)}
                className="block p-3 hover:bg-gray-700/60 rounded transition-colors"
              >
                {tileBody}
              </Link>
            </div>
          );
        })}
      </div>
      {compareEnabled && players.length > 0 && (
        <div className="mt-4 flex items-center justify-between">
          <span className="text-xs text-gray-500">
            {selectedRows.length === 0
              ? `Select up to ${MAX_SIMILAR_COMPARE_SELECTIONS} players to compare`
              : `${selectedRows.length} of ${MAX_SIMILAR_COMPARE_SELECTIONS} selected`}
          </span>
          <button
            onClick={launchCompare}
            disabled={selectedRows.length === 0}
            className={`text-sm px-3 py-1.5 rounded font-medium transition-colors ${
              selectedRows.length === 0
                ? 'bg-gray-700 text-gray-500 cursor-not-allowed'
                : 'bg-blue-600 hover:bg-blue-700 text-white'
            }`}
          >
            Compare ({selectedRows.length + 1})
          </button>
        </div>
      )}
    </div>
  );
}
