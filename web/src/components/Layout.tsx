import { NavLink, Outlet } from 'react-router-dom';

const links = [
  { to: '/', label: 'Rankings', end: true },
  { to: '/players', label: 'Players', end: true },
  { to: '/players/compare', label: 'Compare' },
  { to: '/predict', label: 'Predict' },
];

export default function Layout() {
  return (
    <div className="min-h-screen flex flex-col bg-gray-900">
      <nav className="bg-gray-950 border-b border-gray-800 px-6 py-3 flex items-center gap-8">
        <NavLink to="/" className="text-xl font-bold text-blue-400 tracking-tight">
          cstat
        </NavLink>
        <div className="flex gap-1">
          {links.map((l) => (
            <NavLink
              key={l.to}
              to={l.to}
              end={l.end}
              className={({ isActive }) =>
                `px-3 py-1.5 rounded text-sm font-medium transition-colors ${
                  isActive
                    ? 'bg-blue-600 text-white'
                    : 'text-gray-400 hover:bg-gray-800 hover:text-gray-200'
                }`
              }
            >
              {l.label}
            </NavLink>
          ))}
        </div>
      </nav>
      <main className="flex-1 px-6 py-6 max-w-7xl mx-auto w-full">
        <Outlet />
      </main>
    </div>
  );
}
