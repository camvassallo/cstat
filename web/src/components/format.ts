// Mixed-scale percentage formatters used across stat tables.
//
// Some pss / torvik fields arrive as fractions (0–1) and render with ×100
// scaling (AST%, TOV%, USG%, TS%, eFG%, FT Rate). Others already arrive as
// percent points (0–100) and render as-is (ORB%, DRB%, STL%, BLK%). Keeping
// both helpers in one module so the convention has a single source of truth.

export const fracPct = (v: number | null | undefined) =>
  v != null ? (v * 100).toFixed(1) : '—';

export const pointPct = (v: number | null | undefined) =>
  v != null ? v.toFixed(1) : '—';
