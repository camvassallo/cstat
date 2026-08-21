import { lazy } from 'react';
import { BrowserRouter, Routes, Route, Navigate, useLocation, useSearchParams } from 'react-router-dom';
import Layout from './components/Layout';

// Every route is code-split (issue #267). Before this the whole app — all 18
// routes, AG Grid and Recharts — shipped as one chunk on every first visit,
// so a landing on Rankings paid for Recharts and 14 pages it never rendered.
// Layout stays eagerly imported: it is the shell that renders on every route
// (and holds the <Suspense> boundary these chunks resolve under).
const Rankings = lazy(() => import('./pages/Rankings'));
const TeamDetail = lazy(() => import('./pages/TeamDetail'));
const Players = lazy(() => import('./pages/Players'));
const PlayerDetail = lazy(() => import('./pages/PlayerDetail'));
const PlayerProgression = lazy(() => import('./pages/PlayerProgression'));
const PlayerCompare = lazy(() => import('./pages/PlayerCompare'));
const Predict = lazy(() => import('./pages/Predict'));
const Archetypes = lazy(() => import('./pages/Archetypes'));
const Projected = lazy(() => import('./pages/Projected'));
// Named export, so map it onto the default `lazy` expects. It shares Projected's
// chunk, which is right — it only ever redirects into that page.
const ProjectedYearRedirect = lazy(() =>
  import('./pages/Projected').then((m) => ({ default: m.ProjectedYearRedirect })),
);
const Lineups = lazy(() => import('./pages/Lineups'));
const Coaches = lazy(() => import('./pages/Coaches'));
const CoachDetail = lazy(() => import('./pages/CoachDetail'));
const Portle = lazy(() => import('./pages/Portle'));
const WhichClass = lazy(() => import('./pages/WhichClass'));

// The draft board now lives as a mode tab on /players. Redirect the legacy
// /draft URL there, carrying the site-selected season through.
function DraftRedirect() {
  const [params] = useSearchParams();
  const season = params.get('season');
  const to = season ? `/players?mode=draft&season=${season}` : '/players?mode=draft';
  return <Navigate to={to} replace />;
}

// "Mystery Baller" was renamed to "Portle". Redirect the legacy /mystery-baller
// URL (including practice-share ?seed=&mode=&season= params) to /portle.
function PortleRedirect() {
  const { search } = useLocation();
  return <Navigate to={`/portle${search}`} replace />;
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<Layout />}>
          <Route path="/" element={<Rankings />} />
          <Route path="/teams/:id" element={<TeamDetail />} />
          <Route path="/players" element={<Players />} />
          <Route path="/players/compare" element={<PlayerCompare />} />
          <Route path="/players/:id" element={<PlayerDetail />} />
          <Route path="/players/:id/progression" element={<PlayerProgression />} />
          <Route path="/predict" element={<Predict />} />
          <Route path="/archetypes" element={<Archetypes />} />
          <Route path="/projected" element={<Projected />} />
          {/* Back-compat: the page used to live at /projected/:year before
              the navbar season picker took over via ?season=. */}
          <Route path="/projected/:year" element={<ProjectedYearRedirect />} />
          {/* Draft moved to a Players mode tab; keep the old URL working
              (and preserve any ?season=). */}
          <Route path="/draft" element={<DraftRedirect />} />
          <Route path="/lineups" element={<Lineups />} />
          <Route path="/coaches" element={<Coaches />} />
          <Route path="/coaches/:id" element={<CoachDetail />} />
          <Route path="/portle" element={<Portle />} />
          {/* Renamed from "Mystery Baller" — keep old (possibly shared)
              /mystery-baller links working, carrying practice query params. */}
          <Route path="/mystery-baller" element={<PortleRedirect />} />
          <Route path="/which-class" element={<WhichClass />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
