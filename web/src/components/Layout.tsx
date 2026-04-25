import { NavLink, Outlet, useLocation } from 'react-router-dom';

const navLinkClass = (active: boolean) =>
  `px-3 py-1.5 rounded text-sm font-medium transition-colors ${
    active
      ? 'bg-blue-600 text-white'
      : 'text-gray-400 hover:bg-gray-800 hover:text-gray-200'
  }`;

export default function Layout() {
  const { pathname } = useLocation();
  // Players highlights on /players and /players/<id>, but not /players/compare.
  const playersActive =
    pathname === '/players' ||
    (pathname.startsWith('/players/') && pathname !== '/players/compare');
  const compareActive = pathname === '/players/compare';

  return (
    <div className="min-h-screen flex flex-col bg-gray-900">
      <nav className="bg-gray-950 border-b border-gray-800 px-6 py-3 flex items-center gap-8">
        <NavLink to="/" className="text-xl font-bold text-blue-400 tracking-tight">
          cstat
        </NavLink>
        <div className="flex gap-1">
          <NavLink to="/" end className={({ isActive }) => navLinkClass(isActive)}>
            Rankings
          </NavLink>
          <NavLink to="/players" className={() => navLinkClass(playersActive)}>
            Players
          </NavLink>
          <NavLink to="/players/compare" className={() => navLinkClass(compareActive)}>
            Compare
          </NavLink>
          <NavLink to="/predict" className={({ isActive }) => navLinkClass(isActive)}>
            Predict
          </NavLink>
        </div>
      </nav>
      <main className="flex-1 px-6 py-6 max-w-7xl mx-auto w-full">
        <Outlet />
      </main>
    </div>
  );
}
