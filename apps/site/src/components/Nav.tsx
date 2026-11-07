import { Link } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { useTheme } from "../lib/theme";

function SunIcon() {
  return (
    <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
      <circle cx="12" cy="12" r="4.2" />
      <path d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.2 5.2l1.6 1.6M17.2 17.2l1.6 1.6M18.8 5.2l-1.6 1.6M6.8 17.2l-1.6 1.6" strokeLinecap="round" />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
      <path d="M20 14.2A8.2 8.2 0 1 1 9.8 4a6.6 6.6 0 0 0 10.2 10.2Z" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export default function Nav() {
  const { t, locale, toggleLocale } = useI18n();
  const { resolved, toggleTheme } = useTheme();

  const links = [
    { to: "/#top", label: t.nav.home },
    { to: "/funding", label: t.nav.funding },
    { to: "/#news", label: t.nav.news },
    { to: "/#strategy", label: t.nav.strategy },
    { to: "/#pricing", label: t.nav.pricing },
  ];

  return (
    <header className="fixed inset-x-0 top-0 z-50 border-b border-line bg-canvas/80 backdrop-blur-xl">
      <div className="page flex h-16 items-center justify-between gap-6">
        <Link to="/" className="flex items-center gap-2.5">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-accent/15 ring-1 ring-accent/30">
            <svg viewBox="0 0 24 24" className="h-4 w-4 text-accent" aria-hidden>
              <path
                d="M6 18 18 6M18 6h-7m7 0v7"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </span>
          <span className="text-[15px] font-semibold tracking-tight text-fg">ArbPilot</span>
        </Link>

        <nav className="hidden items-center gap-6 text-[13px] text-muted lg:flex">
          {links.map((link) => (
            <Link key={link.to} to={link.to} className="transition-colors hover:text-fg">
              {link.label}
            </Link>
          ))}
        </nav>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={toggleLocale}
            title={t.nav.language}
            aria-label={t.nav.language}
            className="flex h-8 min-w-8 items-center justify-center rounded-full border border-line px-2 font-mono text-[11px] text-muted transition-colors hover:border-line-strong hover:text-fg"
          >
            {locale === "zh" ? "EN" : "中"}
          </button>

          <button
            type="button"
            onClick={toggleTheme}
            title={resolved === "dark" ? t.nav.light : t.nav.dark}
            aria-label={t.nav.theme}
            className="flex h-8 w-8 items-center justify-center rounded-full border border-line text-muted transition-colors hover:border-line-strong hover:text-fg"
          >
            {resolved === "dark" ? <SunIcon /> : <MoonIcon />}
          </button>

          <Link
            to="/#download"
            className="rounded-full bg-accent-solid px-4 py-2 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover"
          >
            {t.nav.cta}
          </Link>
        </div>
      </div>
    </header>
  );
}
