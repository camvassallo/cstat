import { colorSchemeDarkBlue, themeQuartz } from 'ag-grid-community';

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
