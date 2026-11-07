import { renderToString } from "react-dom/server";
import { StaticRouter } from "react-router-dom";

import App from "./App";
import { I18nProvider, type Locale } from "./lib/i18n";
import { metaForPath, type PageMeta } from "./lib/seo";
import { ThemeProvider } from "./lib/theme";

export { SITE_URL } from "./lib/seo";

/** 构建期要落成静态 HTML 的公开页面；工具页与 /pricing 只保留客户端渲染。 */
export const PRERENDER_ROUTES = ["/", "/privacy", "/terms"] as const;

export type PrerenderPage = { meta: PageMeta; html: string };

/** 由 scripts/prerender.mjs 调用，把整站渲染成静态 HTML 后写进 dist。 */
export function renderPages(locale: Locale = "zh"): PrerenderPage[] {
  return PRERENDER_ROUTES.map((path) => ({
    meta: metaForPath(path, locale),
    html: renderToString(
      <I18nProvider initialLocale={locale}>
        <ThemeProvider>
          <StaticRouter location={path}>
            <App />
          </StaticRouter>
        </ThemeProvider>
      </I18nProvider>,
    ),
  }));
}
