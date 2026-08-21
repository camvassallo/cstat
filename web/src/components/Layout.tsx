import { Suspense, useRef, useState } from 'react';
import { NavLink, Outlet, useLocation } from 'react-router-dom';
import { useAvailableSeasons, usePageSeasons, useSeason, type Season } from './season';
import RouteErrorBoundary, { ChunkReloadReset } from './RouteErrorBoundary';

const navLinkClass = (active: boolean) =>
  `px-3 py-2 rounded text-sm font-medium transition-colors ${
    active
      ? 'bg-[var(--brand-blue-accent)] text-white'
      : 'text-[var(--brand-chrome-text)] hover:bg-white/10 hover:text-white'
  }`;

// Mobile menu links use a larger touch target (44px+) and stretch full width
// so any tap on the row registers, not just the text.
const mobileNavLinkClass = (active: boolean) =>
  `block px-4 py-3 rounded text-base font-medium transition-colors ${
    active
      ? 'bg-[var(--brand-blue-accent)] text-white'
      : 'text-[var(--brand-chrome-text-strong)] hover:bg-white/10 hover:text-white'
  }`;

// One nav destination. `to` may carry a query (e.g. `/players?mode=draft`,
// `/projected?season=2027`) — the active-matching below is query-aware.
type NavItem = { to: string; label: string };

// Grouped nav — the players pod (roster + its mode tabs + the two player-level
// analysis pages), the forecast pod (game predictor + preseason projections),
// and the just-for-fun games. Rankings / Lineups / Coaches stay top-level.
const PLAYERS_ITEMS: NavItem[] = [
  { to: '/players', label: 'Overview' },
  { to: '/players/compare', label: 'Compare' },
  { to: '/archetypes', label: 'Archetypes' },
  { to: '/players?mode=transfers', label: 'Transfer Portal' },
  { to: '/players?mode=recruits', label: 'Recruits' },
  { to: '/players?mode=draft', label: 'Draft' },
];
const FORECAST_ITEMS: NavItem[] = [
  { to: '/predict', label: 'Predict' },
  { to: '/projected?season=2027', label: 'Future' },
];
const PLAY_ITEMS: NavItem[] = [
  { to: '/portle', label: 'Portle' },
  { to: '/which-class', label: 'Archetype Quiz' },
];

// Desktop hover-dropdown for a nav group. Opens on mouse-enter (and on
// keyboard focus entering the group), closes on mouse-leave or when focus
// leaves the group. `active` highlights the trigger whenever any child route
// is current; `isItemActive` highlights the open row.
function NavDropdown({
  label,
  active,
  items,
  isItemActive,
}: {
  label: string;
  active: boolean;
  items: NavItem[];
  isItemActive: (to: string) => boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close when keyboard focus leaves the group entirely (Tab out of the menu).
  const handleBlur = (e: React.FocusEvent<HTMLDivElement>) => {
    const next = e.relatedTarget as Node | null;
    if (!ref.current || !next || !ref.current.contains(next)) setOpen(false);
  };

  return (
    <div
      ref={ref}
      className="relative"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={handleBlur}
    >
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        // Click also toggles, for touch / explicit taps on hybrid displays.
        onClick={() => setOpen((v) => !v)}
        className={`${navLinkClass(active)} inline-flex items-center gap-1`}
      >
        {label}
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`transition-transform ${open ? 'rotate-180' : ''}`}
        >
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>
      {open && (
        // `top-full pt-1` keeps the hover area continuous from the trigger into
        // the menu — the transparent padding bridges the visual gap so moving
        // the pointer onto the menu doesn't fire mouse-leave and close it.
        <div className="absolute left-0 top-full pt-1 z-50">
          <div
            role="menu"
            className="brand-surface min-w-[11rem] rounded-md border py-1 shadow-xl shadow-black/40"
          >
            {items.map((it) => (
              <NavLink
                key={it.to}
                to={it.to}
                role="menuitem"
                onClick={() => setOpen(false)}
                className={`block px-3 py-2 text-sm transition-colors ${
                  isItemActive(it.to)
                    ? 'bg-[var(--brand-blue-accent)] text-white'
                    : 'text-[var(--brand-chrome-text-strong)] hover:bg-white/10 hover:text-white'
                }`}
              >
                {it.label}
              </NavLink>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function SeasonSelector() {
  const { season, setSeason } = useSeason();
  const { seasons: globalSeasons } = useAvailableSeasons();
  const pageSeasons = usePageSeasons();
  // A page can hide the selector entirely by publishing an EMPTY list — used by
  // the season-agnostic career coaches board, where a year picker is
  // meaningless. (Detail pages publish a non-empty list to constrain the
  // dropdown; null releases the override back to the global list.)
  if (pageSeasons != null && pageSeasons.length === 0) return null;
  // Detail pages publish their entity's eligible seasons via `setPageSeasons`;
  // when present, constrain the dropdown so the user can't pick a year the
  // entity has no data in. Global list otherwise.
  const seasons = pageSeasons ?? globalSeasons;
  return (
    <label className="flex items-center gap-2 text-xs text-[var(--brand-chrome-text)]">
      <span className="uppercase tracking-wide hidden sm:inline">Season</span>
      <select
        value={season}
        onChange={(e) => setSeason(Number(e.target.value) as Season)}
        aria-label="Season"
        className="bg-[var(--brand-navy)] border border-[var(--brand-navy-line)] text-gray-100 text-sm rounded px-3 py-2 hover:border-[var(--brand-blue)] transition-colors"
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

// Slim global footer. Its main job is carrying the data-acknowledgments link
// so the provider credits have one honest home, instead of being scattered
// across every page as outbound links that send readers elsewhere.
function SiteFooter() {
  return (
    <footer className="brand-chrome border-t mt-8">
      <div className="max-w-7xl mx-auto w-full px-4 sm:px-6 py-5 flex flex-col sm:flex-row items-center justify-between gap-3">
        <div className="flex items-center gap-2.5">
          <img
            src="/logo.svg"
            alt=""
            width={22}
            height={22}
            className="w-[22px] h-[22px] rounded ring-1 ring-[var(--brand-navy-line)]"
          />
          <span className="text-sm text-[var(--brand-chrome-text)]">
            <span className="font-semibold text-gray-200">Camalytics</span>
            <span className="mx-1.5 text-[var(--brand-chrome-divider)]">·</span>
            College basketball analytics
          </span>
        </div>
        <nav className="flex items-center gap-5 text-sm" aria-label="Footer">
          <NavLink
            to="/acknowledgments"
            className="text-[var(--brand-chrome-text)] hover:text-[var(--brand-blue-bright)] transition-colors"
          >
            Data &amp; Acknowledgments
          </NavLink>
          <a
            href="https://github.com/camvassallo/cstat"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[var(--brand-chrome-text)] hover:text-[var(--brand-blue-bright)] transition-colors"
          >
            GitHub
          </a>
        </nav>
      </div>
    </footer>
  );
}

export default function Layout() {
  const { pathname, search } = useLocation();
  const [menuOpen, setMenuOpen] = useState(false);
  const currentMode = new URLSearchParams(search).get('mode');

  // Per-item active state (query-aware). The Players group shares the
  // `/players` pathname across its mode tabs, so Overview vs Transfers/…/Draft
  // is disambiguated by the `?mode=` param; Compare / Archetypes are their own
  // pathnames; Future matches any /projected route.
  const itemActive = (to: string): boolean => {
    const [path, query] = to.split('?');
    const mode = query ? new URLSearchParams(query).get('mode') : null;
    if (path === '/players') {
      if (mode) return pathname === '/players' && currentMode === mode;
      // Overview: bare /players (no mode) plus the player detail pages.
      return (
        (pathname === '/players' && !currentMode) ||
        (pathname.startsWith('/players/') && pathname !== '/players/compare')
      );
    }
    if (path === '/projected') return pathname.startsWith('/projected');
    return pathname === path;
  };

  // A group's trigger is active whenever any of its routes is current.
  const playersActive = pathname.startsWith('/players') || pathname === '/archetypes';
  const forecastActive = pathname === '/predict' || pathname.startsWith('/projected');
  const playActive = pathname === '/portle' || pathname === '/which-class';

  // Auto-close the mobile menu when a link is tapped. We attach onClick to
  // each mobile NavLink rather than reacting to pathname in an effect (the
  // codebase forbids setState in effects).
  const closeMenu = () => setMenuOpen(false);

  const links = (
    <>
      <NavLink to="/" end className={({ isActive }) => navLinkClass(isActive)}>
        Rankings
      </NavLink>
      <NavDropdown
        label="Players"
        active={playersActive}
        items={PLAYERS_ITEMS}
        isItemActive={itemActive}
      />
      <NavLink to="/lineups" className={({ isActive }) => navLinkClass(isActive)}>
        Lineups
      </NavLink>
      <NavDropdown
        label="Forecast"
        active={forecastActive}
        items={FORECAST_ITEMS}
        isItemActive={itemActive}
      />
      <NavLink to="/coaches" className={({ isActive }) => navLinkClass(isActive)}>
        Coaches
      </NavLink>
      <NavDropdown
        label="Play"
        active={playActive}
        items={PLAY_ITEMS}
        isItemActive={itemActive}
      />
    </>
  );

  // Mobile drawer: the same groups, flattened into labeled sections (no nested
  // collapse state — the drawer is already a vertical list).
  const mobileGroup = (label: string, items: NavItem[]) => (
    <div className="mt-1">
      <div className="px-4 pt-2 pb-1 text-xs font-semibold uppercase tracking-wide text-[var(--brand-chrome-divider)]">
        {label}
      </div>
      {items.map((it) => (
        <NavLink
          key={it.to}
          to={it.to}
          onClick={closeMenu}
          className={() => `${mobileNavLinkClass(itemActive(it.to))} pl-6`}
        >
          {it.label}
        </NavLink>
      ))}
    </div>
  );

  return (
    <div className="min-h-screen flex flex-col bg-gray-900">
      <nav className="brand-chrome border-b px-4 sm:px-6 py-3 flex items-center gap-4 sm:gap-8">
        <NavLink
          to="/"
          className="flex items-center gap-2.5 shrink-0"
          aria-label="Camalytics home"
        >
          {/* The mark ships on its own navy field (it's a JPEG, no alpha), so
              it reads as a rounded tile against the navbar rather than trying
              to blend — see the note in the acknowledgments page source. */}
          <img
            src="/logo.svg"
            alt=""
            width={32}
            height={32}
            className="w-8 h-8 rounded-md ring-1 ring-[var(--brand-navy-line)]"
          />
          {/* Shown at every width — on mobile the row is just logo + season +
              burger, so there is room for the name and hiding it wasted it. */}
          <span className="text-lg sm:text-xl font-bold text-white tracking-tight">
            Cam<span className="text-[var(--brand-blue)]">alytics</span>
          </span>
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
            className="md:hidden inline-flex items-center justify-center w-11 h-11 rounded text-[var(--brand-chrome-text-strong)] hover:bg-white/10 hover:text-white"
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
        <div className="brand-chrome md:hidden border-b px-3 py-2 flex flex-col gap-1">
          <NavLink to="/" end onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Rankings
          </NavLink>
          {mobileGroup('Players', PLAYERS_ITEMS)}
          <NavLink to="/lineups" onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Lineups
          </NavLink>
          {mobileGroup('Forecast', FORECAST_ITEMS)}
          <NavLink to="/coaches" onClick={closeMenu} className={({ isActive }) => mobileNavLinkClass(isActive)}>
            Coaches
          </NavLink>
          {mobileGroup('Play', PLAY_ITEMS)}
        </div>
      )}
      {/* `overflow-x-clip` is a safety net for charts and sticky cells that
          occasionally render 1–2px wider than their container on mobile.
          We use `clip` rather than `hidden` because per the CSS spec, setting
          one axis to `hidden` forces the other axis's computed `overflow` to
          `auto`, silently turning `<main>` into a nested vertical scroll
          container as soon as page content exceeds the viewport. `clip`
          performs the same horizontal clipping without creating a scroll
          context. Internal scroll regions (game log, roster, AG Grid) still
          create their own scroll context and remain swipeable. */}
      <main className="flex-1 px-3 sm:px-6 py-4 sm:py-6 max-w-7xl mx-auto w-full overflow-x-clip">
        {/* Route components are lazy-loaded (issue #267), so the Suspense
            boundary sits here rather than around the whole app — the nav and
            season picker stay mounted and interactive while the next route's
            chunk downloads.

            `key={pathname}` is what makes the fallback actually appear.
            BrowserRouter wraps its location update in `React.startTransition`,
            and React deliberately keeps an ALREADY-REVEALED boundary's content
            on screen through a transition instead of showing its fallback — so
            without the key, clicking a nav link sat on the previous page with
            no spinner and no moving active-link highlight until the chunk
            arrived. A key change mounts a NEW boundary, which has nothing to
            preserve and renders its fallback immediately.

            Keyed on `pathname` and NOT `search` on purpose: the URL-backed
            filters on /players (archetype chips, match mode, mode tabs) only
            move the query string, so they keep the same boundary and stay
            inside the transition, where React can interrupt them. Opting out
            of transitions globally would have fixed the spinner and made those
            filters ~5x more expensive — measured at 413ms of blocked main
            thread per chip vs 83ms. */}
        <RouteErrorBoundary resetKey={pathname + search}>
          <Suspense key={pathname} fallback={<div className="text-gray-400">Loading…</div>}>
            <ChunkReloadReset />
            <Outlet />
          </Suspense>
        </RouteErrorBoundary>
      </main>
      <SiteFooter />
    </div>
  );
}
