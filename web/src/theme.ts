import {
  CellStyleModule,
  ClientSideRowModelModule,
  ColumnAutoSizeModule,
  ModuleRegistry,
  PaginationModule,
  QuickFilterModule,
  TooltipModule,
  ValidationModule,
  colorSchemeDarkBlue,
  themeQuartz,
} from 'ag-grid-community';

// Selective AG Grid module registration.
//
// This lives here — rather than in each grid's own file — because every grid in
// the app has to import `gridTheme` to look right, so the registration rides
// along and a new grid cannot forget it. That matters more since the routes are
// lazy-loaded: a route whose chunk registered nothing would render a grid that
// silently drops features.
//
// Register only what the grids actually use. `AllCommunityModule` pulls in the
// entire community feature set — editing, every filter type, CSV export, the
// infinite row model — and was 310 KB gzip of the bundle (issue #267). The
// core grid module is always present, so sorting, column resize, column
// pinning, cell renderers and the loading overlay need no entry below.
ModuleRegistry.registerModules([
  ClientSideRowModelModule, // rowData-backed grids (every grid here)
  CellStyleModule, // colDef.cellStyle — the percentile gradients, column dividers
  TooltipModule, // colDef.headerTooltip
  QuickFilterModule, // the `quickFilterText` search boxes
  PaginationModule, // Players / ProjectedPlayers page-size controls
  ColumnAutoSizeModule, // RecruitClass's autoSizeStrategy: fitCellContents
  // Dev-only: turns "feature X needs module Y" into a readable console error
  // instead of a silently missing feature. Dropped from production builds.
  ...(import.meta.env.DEV ? [ValidationModule] : []),
]);

export const gridTheme = themeQuartz.withPart(colorSchemeDarkBlue).withParams({
  backgroundColor: '#1f2937',
  headerBackgroundColor: '#111827',
  oddRowBackgroundColor: '#1a2233',
  rowHoverColor: '#374151',
  borderColor: '#374151',
  fontSize: 13,
  foregroundColor: '#e5e7eb',
  headerFontSize: 11,
  headerFontWeight: 600,
  headerTextColor: '#9ca3af',
  rowBorder: { color: '#273244', width: 1, style: 'solid' },
  columnBorder: false,
  wrapperBorder: false,
});
