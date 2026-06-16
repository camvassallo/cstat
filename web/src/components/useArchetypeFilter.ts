import { useCallback, useMemo } from 'react';
import { useSearchParams } from 'react-router-dom';
import { CLASS_ORDER } from './archetypeColors';
import type { MatchMode } from './ArchetypeFilter';

/** The minimal shape the archetype filter needs from a row. */
export interface ArchetypeRow {
  primary_class: string | null;
  secondary_class: string | null;
}

export interface ArchetypeFilterState {
  /** Selected classes (union/OR or intersection/AND per `matchMode`). */
  selected: Set<string>;
  matchMode: MatchMode;
  includeSecondary: boolean;
  toggleClass: (cls: string) => void;
  setMatchMode: (m: MatchMode) => void;
  toggleIncludeSecondary: () => void;
  /** Clear the selection and the include-secondary / match flags. */
  clear: () => void;
  /** Filter archetype-bearing rows by the active selection (no-op when empty). */
  filterRows: <T extends ArchetypeRow>(rows: T[]) => T[];
}

// URL-backed archetype filter shared by the players ranking page and the
// transfer portal. State lives entirely in the query string so it deep-links
// and survives reloads:
//   ?archetypes=Paladin,Monk   selected classes (CLASS_ORDER, comma-joined)
//   ?match=all                 AND (intersection); absent = OR (union, default)
//   ?include_secondary=true    OR-mode widening to the secondary class
//
// DEPRECATED: the legacy single `?archetype=Wizard`. Nothing emits it anymore
// (the /archetypes cards now link to `?archetypes=`), but we still read it and
// fold it into the selection so old bookmarks / shared links keep resolving;
// the first `toggleClass` rewrites such a URL to the canonical `?archetypes=`.
// Safe to drop the back-compat read once those links have aged out.
export function useArchetypeFilter(): ArchetypeFilterState {
  const [searchParams, setSearchParams] = useSearchParams();

  const selected = useMemo(() => {
    const set = new Set<string>();
    const multi = searchParams.get('archetypes');
    if (multi) multi.split(',').map((s) => s.trim()).filter(Boolean).forEach((c) => set.add(c));
    const legacy = searchParams.get('archetype'); // deprecated; back-compat only
    if (legacy) set.add(legacy);
    return set;
  }, [searchParams]);

  const includeSecondary = searchParams.get('include_secondary') === 'true';
  const matchMode: MatchMode = searchParams.get('match') === 'all' ? 'all' : 'any';

  // Toggle one class. Migrates any deprecated `?archetype=` into the canonical
  // `?archetypes=` list and keeps it in CLASS_ORDER for stable, readable URLs.
  const toggleClass = useCallback(
    (cls: string) => {
      setSearchParams((prev) => {
        const next = new URLSearchParams(prev);
        const set = new Set<string>();
        const multi = next.get('archetypes');
        if (multi) multi.split(',').map((s) => s.trim()).filter(Boolean).forEach((c) => set.add(c));
        const legacy = next.get('archetype'); // deprecated; migrate then drop
        if (legacy) {
          set.add(legacy);
          next.delete('archetype');
        }
        if (set.has(cls)) set.delete(cls);
        else set.add(cls);
        if (set.size === 0) {
          next.delete('archetypes');
          next.delete('include_secondary');
          next.delete('match');
        } else {
          next.set('archetypes', CLASS_ORDER.filter((c) => set.has(c)).join(','));
        }
        return next;
      });
    },
    [setSearchParams],
  );

  const setMatchMode = useCallback(
    (m: MatchMode) => {
      setSearchParams((prev) => {
        const next = new URLSearchParams(prev);
        if (m === 'all') next.set('match', 'all');
        else next.delete('match');
        return next;
      });
    },
    [setSearchParams],
  );

  const toggleIncludeSecondary = useCallback(() => {
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev);
      if (next.get('include_secondary') === 'true') next.delete('include_secondary');
      else next.set('include_secondary', 'true');
      return next;
    });
  }, [setSearchParams]);

  const clear = useCallback(() => {
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev);
      next.delete('archetypes');
      next.delete('archetype');
      next.delete('include_secondary');
      next.delete('match');
      return next;
    });
  }, [setSearchParams]);

  // Client-side filter over an already-loaded pool. Empty selection = no-op.
  //   'any' (union): primary is selected, or — with include-secondary on —
  //          secondary is.
  //   'all' (intersection): the row must hold EVERY selected class across
  //          {primary, secondary}. Players have at most two classes, so this
  //          always considers the secondary and returns nothing for 3+ selected.
  const filterRows = useCallback(
    <T extends ArchetypeRow>(rows: T[]): T[] => {
      if (selected.size === 0) return rows;
      if (matchMode === 'all') {
        return rows.filter((p) => {
          for (const c of selected) {
            if (p.primary_class !== c && p.secondary_class !== c) return false;
          }
          return true;
        });
      }
      return rows.filter(
        (p) =>
          (p.primary_class != null && selected.has(p.primary_class)) ||
          (includeSecondary && p.secondary_class != null && selected.has(p.secondary_class)),
      );
    },
    [selected, matchMode, includeSecondary],
  );

  return {
    selected,
    matchMode,
    includeSecondary,
    toggleClass,
    setMatchMode,
    toggleIncludeSecondary,
    clear,
    filterRows,
  };
}
