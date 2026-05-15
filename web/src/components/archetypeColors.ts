// D&D-class color palette for archetype badges, similar-player cards,
// and roster-distribution charts. Kept in its own file so the React
// refresh plugin doesn't trip on mixed component / non-component exports.

export const CLASS_COLORS: Record<string, string> = {
  Wizard: '#7c3aed',     // violet — elite lead guards
  Sorcerer: '#dc2626',   // crimson — volume scorers
  Warlock: '#c026d3',    // fuchsia — catch-and-shoot specialists (lifted from violet-900 so it reads on dark mode and stays distinct from Wizard's violet)
  Bard: '#ec4899',       // pink — mid-major primary creators
  Ranger: '#16a34a',     // green — perimeter spacers
  Barbarian: '#ea580c',  // orange — interior finishers
  Paladin: '#eab308',    // gold — rim-protecting anchors
  Monk: '#06b6d4',       // cyan — versatile rotation forwards
  Cleric: '#854d0e',     // earth brown — backup bigs / interior connectors
  Druid: '#059669',      // emerald — dominant two-way bigs (jewel-tone, distinct from Ranger's grass green)
  Rogue: '#e5e7eb',      // bone-white / blade flash — disruptive defenders
  Fighter: '#737373',    // neutral gray — low-USG rotation depth
};

export function classColor(cls: string | null | undefined): string {
  if (!cls) return '#64748b';
  return CLASS_COLORS[cls] ?? '#64748b';
}

// One-line tagline per class — used for hover tooltips on class labels
// across PlayerDetail, TeamDetail, the comparison view, and the archetype
// glossary. Keep these in sync with the longer descriptions in
// `pages/Archetypes.tsx` so the tooltip and the glossary tell the same story.
export const CLASS_TAGLINES: Record<string, string> = {
  Wizard: 'Elite floor general.',
  Sorcerer: 'High-volume star scorer.',
  Warlock: 'Three-point specialist.',
  Bard: 'Mid-major primary creator.',
  Ranger: 'Perimeter spacer.',
  Barbarian: 'Interior finisher.',
  Paladin: 'Defensive anchor.',
  Monk: 'Versatile rotation forward.',
  Cleric: 'Backup big.',
  Druid: 'Elite two-way big.',
  Rogue: 'Disruptive two-way wing.',
  Fighter: 'Low-USG rotation depth.',
};

export function classTagline(cls: string | null | undefined): string {
  if (!cls) return '';
  return CLASS_TAGLINES[cls] ?? '';
}

/// Build a "Class — Tagline" string suitable for a `title` attribute.
export function classTitle(cls: string | null | undefined): string {
  if (!cls) return '';
  const tag = classTagline(cls);
  return tag ? `${cls} — ${tag}` : cls;
}
