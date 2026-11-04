import { Route, Routes } from "react-router-dom";

import { AppShell } from "./components/layout/AppShell";
import OpportunitiesPage from "./pages/Opportunities";
import OverviewPage from "./pages/Overview";
import PositionsPage from "./pages/Positions";
import SettingsPage from "./pages/Settings";
import StrategiesPage from "./pages/Strategies";

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<OverviewPage />} />
        <Route path="opportunities" element={<OpportunitiesPage />} />
        <Route path="positions" element={<PositionsPage />} />
        <Route path="strategies" element={<StrategiesPage />} />
        <Route path="settings" element={<SettingsPage />} />
      </Route>
    </Routes>
  );
}
