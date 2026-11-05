import { Route, Routes } from "react-router-dom";

import Layout from "./components/Layout";
import CheckoutSuccessPage from "./pages/CheckoutSuccessPage";
import HomePage from "./pages/HomePage";
import LegalPage from "./pages/LegalPage";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={<HomePage />} />
        <Route path="pricing" element={<HomePage />} />
        <Route path="privacy" element={<LegalPage slug="privacy" />} />
        <Route path="terms" element={<LegalPage slug="terms" />} />
        <Route path="checkout/success" element={<CheckoutSuccessPage />} />
      </Route>
    </Routes>
  );
}
