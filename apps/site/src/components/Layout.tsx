import { useEffect } from "react";
import { Outlet, useLocation } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { metaForPath, usePageMeta } from "../lib/seo";
import Nav from "./Nav";
import { Footer } from "./Sections";

export default function Layout() {
  const { pathname, hash } = useLocation();
  const { locale } = useI18n();

  usePageMeta(metaForPath(pathname, locale));

  useEffect(() => {
    if (hash) {
      // 锚点 id 可能以数字或非 ASCII 字符开头，必须用 getElementById 而不是 querySelector。
      const target = document.getElementById(decodeURIComponent(hash.slice(1)));
      if (target) {
        target.scrollIntoView({ behavior: "smooth", block: "start" });
        return;
      }
    }
    window.scrollTo({ top: 0 });
  }, [pathname, hash]);

  return (
    <div className="min-h-full overflow-x-clip">
      <Nav />
      <main>
        <Outlet />
      </main>
      <Footer />
    </div>
  );
}
