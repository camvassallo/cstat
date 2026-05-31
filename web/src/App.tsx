import { BrowserRouter, Routes, Route } from 'react-router-dom';
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
import Draft from './pages/Draft';

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
          <Route path="/draft" element={<Draft />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
