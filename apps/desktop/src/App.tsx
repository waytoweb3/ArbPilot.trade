import type { ReactNode } from "react";
import { Route, Routes } from "react-router-dom";

import { HostedLocked } from "./components/HostedLocked";
import { AppShell } from "./components/layout/AppShell";
import { getExecutorMode } from "./lib/api";
import FundingPage from "./pages/Funding";
import FundingHistoryPage from "./pages/FundingHistory";
import MarketPage from "./pages/Market";
import NewsPage from "./pages/News";
import OverviewPage from "./pages/Overview";
import PositionsPage from "./pages/Positions";
import SettingsPage from "./pages/Settings";
import StrategiesPage from "./pages/Strategies";
import TradePage from "./pages/Trade";

/** 官方托管只提供行情资讯，不保存密钥也不代下单：交易类页面统一替换为说明页。 */
const tradingEnabled = (page: ReactNode) =>
  getExecutorMode() === "hosted" ? <HostedLocked /> : page;

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        <Route index element={<OverviewPage />} />
        <Route path="funding" element={<FundingPage />} />
        <Route path="funding-history" element={<FundingHistoryPage />} />
        <Route path="trade" element={tradingEnabled(<TradePage />)} />
        <Route path="news" element={<NewsPage />} />
        <Route path="positions" element={tradingEnabled(<PositionsPage />)} />
        <Route path="strategies" element={tradingEnabled(<StrategiesPage />)} />
        <Route path="market" element={<MarketPage />} />
        <Route path="settings" element={<SettingsPage />} />
      </Route>
    </Routes>
  );
}
