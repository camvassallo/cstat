// D&D-class color palette for archetype badges, similar-player cards,
// and roster-distribution charts. Kept in its own file so the React
// refresh plugin doesn't trip on mixed component / non-component exports.

export const CLASS_COLORS: Record<string, string> = {
  Wizard: '#7c3aed',     // violet — controllers
  Sorcerer: '#dc2626',   // crimson — volume scorers
  Warlock: '#4c1d95',    // deep purple — eldritch / high-variance gunners
  Bard: '#ec4899',       // pink — pass-first
  Ranger: '#16a34a',     // green — 3&D
  Barbarian: '#ea580c',  // orange — slashers
  Paladin: '#eab308',    // gold — two-way anchors
  Monk: '#06b6d4',       // cyan — efficient
  Cleric: '#0d9488',     // teal — healer / glue
  Druid: '#854d0e',      // earth brown — shapeshifter / frontcourt anchor
  Rogue: '#e5e7eb',      // bone-white / blade flash — event creators
  Fighter: '#737373',    // neutral gray — balanced
};

export function classColor(cls: string | null | undefined): string {
  if (!cls) return '#64748b';
  return CLASS_COLORS[cls] ?? '#64748b';
}
