import { BrowserRouter, Routes, Route } from 'react-router-dom';
import Layout from './components/Layout';
import Rankings from './pages/Rankings';
import TeamDetail from './pages/TeamDetail';
import Players from './pages/Players';
import PlayerDetail from './pages/PlayerDetail';
import PlayerCompare from './pages/PlayerCompare';
import Predict from './pages/Predict';
import Archetypes from './pages/Archetypes';

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
          <Route path="/predict" element={<Predict />} />
          <Route path="/archetypes" element={<Archetypes />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
