// D&D-class color palette for archetype badges, similar-player cards,
// and roster-distribution charts. Kept in its own file so the React
// refresh plugin doesn't trip on mixed component / non-component exports.

export const CLASS_COLORS: Record<string, string> = {
  Wizard: '#7c3aed',     // violet — controllers
  Sorcerer: '#dc2626',   // crimson — volume scorers
  Warlock: '#c026d3',    // fuchsia — eldritch / high-variance gunners (lifted from violet-900 so it reads on dark mode and stays distinct from Wizard's violet)
  Bard: '#ec4899',       // pink — pass-first
  Ranger: '#16a34a',     // green — 3&D
  Barbarian: '#ea580c',  // orange — slashers
  Paladin: '#eab308',    // gold — two-way anchors
  Monk: '#06b6d4',       // cyan — efficient
  Cleric: '#854d0e',     // earth brown — grounded glue / connector
  Druid: '#059669',      // emerald — frontcourt anchor (jewel-tone, distinct from Ranger's grass green)
  Rogue: '#e5e7eb',      // bone-white / blade flash — event creators
  Fighter: '#737373',    // neutral gray — balanced
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
  Wizard: 'Pure floor general.',
  Sorcerer: 'Star scorer.',
  Warlock: 'Chaos gunner.',
  Bard: 'Pass-first playmaker.',
  Ranger: '3-and-D wing.',
  Barbarian: 'Rim attacker.',
  Paladin: 'Two-way anchor.',
  Monk: 'Efficient role player.',
  Cleric: 'Glue connector.',
  Druid: 'Frontcourt anchor.',
  Rogue: 'Event creator.',
  Fighter: 'Balanced two-way.',
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
