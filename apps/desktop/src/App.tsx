import { Route, Routes } from "react-router-dom";

import { AppShell } from "./components/layout/AppShell";
import DocsPage from "./pages/Docs";
import FundingPage from "./pages/Funding";
import FundingHistoryPage from "./pages/FundingHistory";
import MarketPage from "./pages/Market";
import NewsPage from "./pages/News";
import OverviewPage from "./pages/Overview";
import PositionsPage from "./pages/Positions";
import SettingsPage from "./pages/Settings";
import StrategiesPage from "./pages/Strategies";
import TradePage from "./pages/Trade";

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<OverviewPage />} />
        <Route path="funding" element={<FundingPage />} />
        <Route path="funding-history" element={<FundingHistoryPage />} />
        <Route path="trade" element={<TradePage />} />
        <Route path="news" element={<NewsPage />} />
        <Route path="positions" element={<PositionsPage />} />
        <Route path="strategies" element={<StrategiesPage />} />
        <Route path="market" element={<MarketPage />} />
        <Route path="docs" element={<DocsPage />} />
        <Route path="settings" element={<SettingsPage />} />
      </Route>
    </Routes>
  );
}
