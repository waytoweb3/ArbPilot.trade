import { Route, Routes } from "react-router-dom";

import Layout from "./components/Layout";
import AdminPage from "./pages/AdminPage";
import CheckoutSuccessPage from "./pages/CheckoutSuccessPage";
import DevicePage from "./pages/DevicePage";
import HomePage from "./pages/HomePage";
import LegalPage from "./pages/LegalPage";

export default function App() {
  return (
    <Routes>
      {/* 运营后台独立布局，不展示官网导航与页脚。 */}
      <Route path="admin" element={<AdminPage />} />
      <Route element={<Layout />}>
        <Route index element={<HomePage />} />
        <Route path="pricing" element={<HomePage />} />
        <Route path="privacy" element={<LegalPage slug="privacy" />} />
        <Route path="terms" element={<LegalPage slug="terms" />} />
        <Route path="checkout/success" element={<CheckoutSuccessPage />} />
        <Route path="device" element={<DevicePage />} />
      </Route>
    </Routes>
  );
}
