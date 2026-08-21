import { BrowserRouter, Routes, Route, Navigate, useLocation, useSearchParams } from 'react-router-dom';
import Layout from './components/Layout';
import Rankings from './pages/Rankings';
import TeamDetail from './pages/TeamDetail';
import Players from './pages/Players';
import PlayerDetail from './pages/PlayerDetail';
import PlayerProgression from './pages/PlayerProgression';
import PlayerCompare from './pages/PlayerCompare';
import Predict from './pages/Predict';
import Archetypes from './pages/Archetypes';
import Projected, { ProjectedYearRedirect } from './pages/Projected';
import Lineups from './pages/Lineups';
import Coaches from './pages/Coaches';
import CoachDetail from './pages/CoachDetail';
import Portle from './pages/Portle';
import WhichClass from './pages/WhichClass';
import Acknowledgments from './pages/Acknowledgments';

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
          <Route path="/acknowledgments" element={<Acknowledgments />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
