import { useState } from 'react';
import { NavLink, Outlet, useLocation } from 'react-router-dom';
import { useAvailableSeasons, useSeason, type Season } from './season';

const navLinkClass = (active: boolean) =>
  `px-3 py-2 rounded text-sm font-medium transition-colors ${
    active
      ? 'bg-blue-600 text-white'
      : 'text-gray-400 hover:bg-gray-800 hover:text-gray-200'
  }`;

// Mobile menu links use a larger touch target (44px+) and stretch full width
// so any tap on the row registers, not just the text.
const mobileNavLinkClass = (active: boolean) =>
  `block px-4 py-3 rounded text-base font-medium transition-colors ${
    active
      ? 'bg-blue-600 text-white'
      : 'text-gray-300 hover:bg-gray-800 hover:text-gray-100'
  }`;

function SeasonSelector() {
  const { season, setSeason } = useSeason();
  const { seasons } = useAvailableSeasons();
  return (
    <label className="flex items-center gap-2 text-xs text-gray-400">
      <span className="uppercase tracking-wide hidden sm:inline">Season</span>
      <select
        value={season}
        onChange={(e) => setSeason(Number(e.target.value) as Season)}
        className="bg-gray-900 border border-gray-700 text-gray-200 text-sm rounded px-3 py-2 focus:outline-none focus:border-blue-500"
      >
        {seasons.map((s) => (
          <option key={s} value={s}>
            {s}
          </option>
        ))}
      </select>
    </label>
  );
}

export default function Layout() {
  const { pathname } = useLocation();
  const [menuOpen, setMenuOpen] = useState(false);

  // Players highlights on /players and /players/<id>, but not /players/compare.
  const playersActive =
    pathname === '/players' ||
    (pathname.startsWith('/players/') && pathname !== '/players/compare');
  const compareActive = pathname === '/players/compare';

  // Auto-close the mobile menu when a link is tapped. We attach onClick to
  // each mobile NavLink rather than reacting to pathname in an effect (the
  // codebase forbids setState in effects).
  const closeMenu = () => setMenuOpen(false);

  const links = (
    <>
      <NavLink to="/" end className={({ isActive }) => navLinkClass(isActive)}>
        Rankings
      </NavLink>
      <NavLink to="/players" className={() => navLinkClass(playersActive)}>
        Players
      </NavLink>
      <NavLink to="/players/compare" className={() => navLinkClass(compareActive)}>
        Compare
      </NavLink>
      <NavLink to="/archetypes" className={({ isActive }) => navLinkClass(isActive)}>
        Archetypes
      </NavLink>
      <NavLink to="/predict" className={({ isActive }) => navLinkClass(isActive)}>
        Predict
      </NavLink>
    </>
  );

  return (
    <div className="min-h-screen flex flex-col bg-gray-900">
      <nav className="bg-gray-950 border-b border-gray-800 px-4 sm:px-6 py-3 flex items-center gap-4 sm:gap-8">
        <NavLink to="/" className="text-xl font-bold text-blue-400 tracking-tight">
          CamPom
        </NavLink>
        {/* Desktop nav — visible from md up */}
        <div className="hidden md:flex gap-1">{links}</div>
        <div className="ml-auto flex items-center gap-2">
          <SeasonSelector />
          {/* Mobile burger — visible below md */}
          <button
            type="button"
            aria-label={menuOpen ? 'Close menu' : 'Open menu'}
            aria-expanded={menuOpen}
            onClick={() => setMenuOpen((v) => !v)}
            className="md:hidden inline-flex items-center justify-center w-11 h-11 rounded text-gray-300 hover:bg-gray-800 hover:text-gray-100"
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="22"
              height="22"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              {menuOpen ? (
                <>
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </>
              ) : (
                <>
                  <line x1="3" y1="6" x2="21" y2="6" />
                  <line x1="3" y1="12" x2="21" y2="12" />
                  <line x1="3" y1="18" x2="21" y2="18" />
                </>
              )}
            </svg>
          </button>
        </div>
      </nav>
      {/* Mobile menu drawer */}
      {menuOpen && (
        <div className="md:hidden bg-gray-950 border-b border-gray-800 px-3 py-2 flex flex-col gap-1">
          <NavLink to="/" end onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Rankings
          </NavLink>
          <NavLink to="/players" onClick={closeMenu} className={() => mobileNavLinkClass(playersActive)}>
            Players
          </NavLink>
          <NavLink to="/players/compare" onClick={closeMenu} className={() => mobileNavLinkClass(compareActive)}>
            Compare
          </NavLink>
          <NavLink to="/archetypes" onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Archetypes
          </NavLink>
          <NavLink to="/predict" onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Predict
          </NavLink>
        </div>
      )}
      {/* `overflow-x-hidden` is a safety net for charts and sticky cells that
          occasionally render 1–2px wider than their container on mobile.
          Internal scroll regions (game log, roster, AG Grid) create their own
          scroll context and remain swipeable. */}
      <main className="flex-1 px-3 sm:px-6 py-4 sm:py-6 max-w-7xl mx-auto w-full overflow-x-hidden">
        <Outlet />
      </main>
    </div>
  );
}
