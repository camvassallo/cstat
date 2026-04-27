import type { ReactNode } from 'react';

/// Shared header bar for the table-style pages (Rankings, Players, etc.).
/// Layout: title — optional count chip — search slot — left-side extras —
/// right-aligned controls. Keeps the page chrome visually consistent so
/// switching between pages doesn't shift the user's eye around.
export function TableToolbar({
  title,
  count,
  countLabel = 'rows',
  search,
  children,
  controls,
}: {
  title: string;
  count?: number | null;
  countLabel?: string;
  /// The search input. Pass `<TableSearchInput />` or a custom node.
  search?: ReactNode;
  /// Extra header content (filter chips, badges) inline with the search.
  children?: ReactNode;
  /// Right-aligned controls (view toggles, etc.).
  controls?: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-3 mb-4">
      <h1 className="text-2xl font-bold">{title}</h1>
      {count != null && (
        <span className="text-xs text-gray-500">
          {count.toLocaleString()} {countLabel}
        </span>
      )}
      {search}
      {children}
      {controls && <div className="ml-auto flex items-center gap-2">{controls}</div>}
    </div>
  );
}

export function TableSearchInput({
  value,
  onChange,
  placeholder = 'Search…',
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <input
      type="search"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      className="bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-white placeholder-gray-500 focus:outline-none focus:border-blue-500 w-56"
    />
  );
}
