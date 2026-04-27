import type { SortDir } from './tableSort';

export function SortHeader<K extends string>({
  label,
  sortKey,
  current,
  onSort,
  align = 'left',
  title,
  className = '',
}: {
  label: string;
  sortKey: K;
  current: { key: K; dir: SortDir };
  onSort: (key: K) => void;
  align?: 'left' | 'right' | 'center';
  title?: string;
  className?: string;
}) {
  const active = current.key === sortKey;
  const alignClass =
    align === 'right' ? 'text-right' : align === 'center' ? 'text-center' : 'text-left';
  const ariaSort: 'ascending' | 'descending' | 'none' = active
    ? current.dir === 'asc'
      ? 'ascending'
      : 'descending'
    : 'none';
  return (
    <th
      className={`sticky top-0 z-10 bg-gray-900 py-2 px-2 ${alignClass} cursor-pointer select-none hover:text-gray-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 transition-colors ${className}`}
      onClick={() => onSort(sortKey)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onSort(sortKey);
        }
      }}
      role="button"
      tabIndex={0}
      aria-sort={ariaSort}
      title={title}
    >
      <span className="inline-flex items-center gap-1">
        {label}
        <span className={`text-[10px] ${active ? 'text-gray-200' : 'text-gray-600'}`}>
          {active ? (current.dir === 'asc' ? '▲' : '▼') : '▾'}
        </span>
      </span>
    </th>
  );
}

export function StickyHeader({
  children,
  align = 'left',
  className = '',
}: {
  children: React.ReactNode;
  align?: 'left' | 'right' | 'center';
  className?: string;
}) {
  const alignClass =
    align === 'right' ? 'text-right' : align === 'center' ? 'text-center' : 'text-left';
  return <th className={`sticky top-0 z-10 bg-gray-900 py-2 px-2 ${alignClass} ${className}`}>{children}</th>;
}
