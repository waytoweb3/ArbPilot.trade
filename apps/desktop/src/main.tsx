import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";

import App from "./App";
import { bootstrapDesktopExecutor } from "./lib/api";
import { I18nProvider } from "./lib/i18n";
import { ThemeProvider } from "./lib/theme";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
      staleTime: 2_000,
    },
  },
});

// 桌面端先拿到内置执行器的地址与令牌，再渲染界面，保证首屏请求可用。
void bootstrapDesktopExecutor().finally(() => {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <I18nProvider>
        <ThemeProvider>
          <QueryClientProvider client={queryClient}>
            <HashRouter>
              <App />
            </HashRouter>
          </QueryClientProvider>
        </ThemeProvider>
      </I18nProvider>
    </React.StrictMode>,
  );
});
