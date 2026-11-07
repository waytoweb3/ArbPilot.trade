import {
  Bot,
  Languages,
  LayoutDashboard,
  Moon,
  Newspaper,
  PanelLeftClose,
  PanelLeftOpen,
  PauseCircle,
  Percent,
  Radar,
  Settings as SettingsIcon,
  Store,
  Sun,
  Wallet,
  Wifi,
  WifiOff,
  X,
} from "lucide-react";
import { useCallback, useState } from "react";
import { NavLink, Outlet } from "react-router-dom";

import { useHealth, useNews } from "../../hooks/useApi";
import { useLicenseSync } from "../../hooks/useAccount";
import { useExecutorStream } from "../../hooks/useExecutorStream";
import { formatDateTime, formatUptime } from "../../lib/format";
import { useI18n } from "../../lib/i18n";
import { countUnread, useNewsPrefs } from "../../lib/news";
import { useTheme } from "../../lib/theme";
import { cn } from "../../lib/utils";
import { useExecutorStore } from "../../state/executor";
import { Button } from "../ui/button";
import { AccountButton } from "./AccountButton";
import { UpdateBanner } from "./UpdateBanner";

const SIDEBAR_KEY = "arbpilot.sidebar.collapsed";

export function AppShell() {
  useExecutorStream();
  // 登录后把账号授权写入执行器，退出登录时清除。
  useLicenseSync();
  const health = useHealth();
  const streamConnected = useExecutorStore((state) => state.streamConnected);
  const notices = useExecutorStore((state) => state.notices);
  const dismissNotice = useExecutorStore((state) => state.dismissNotice);
  const { t, locale, toggleLocale } = useI18n();
  const { resolved, toggleTheme } = useTheme();
  const [collapsed, setCollapsed] = useState(() => localStorage.getItem(SIDEBAR_KEY) === "1");
  const [statusHint, setStatusHint] = useState<string | null>(null);

  const toggleStatusHint = (label: string) =>
    setStatusHint((current) => (current === label ? null : label));

  const toggleSidebar = useCallback(() => {
    setCollapsed((current) => {
      const next = !current;
      localStorage.setItem(SIDEBAR_KEY, next ? "1" : "0");
      return next;
    });
  }, []);

  const online = Boolean(health.data) && !health.isError;
  const paused = health.data?.paused ?? false;

  const news = useNews(100);
  const newsPrefs = useNewsPrefs();
  const unread = countUnread(newsPrefs, news.data?.items ?? []);

  const navItems: {
    to: string;
    label: string;
    icon: typeof LayoutDashboard;
    end?: boolean;
    badge?: number;
  }[] = [
    { to: "/", label: t.nav.overview, icon: LayoutDashboard, end: true },
    { to: "/positions", label: t.nav.positions, icon: Wallet },
    { to: "/funding", label: t.nav.funding, icon: Percent },
    { to: "/news", label: t.nav.news, icon: Newspaper, badge: unread },
    { to: "/strategies", label: t.nav.strategies, icon: Bot },
    { to: "/market", label: t.nav.market, icon: Store },
    { to: "/settings", label: t.nav.settings, icon: SettingsIcon },
  ];

  return (
    <div className="flex h-full min-h-0">
      <aside
        className={cn(
          "flex shrink-0 flex-col border-r border-line bg-panel transition-[width] duration-200",
          collapsed ? "w-14" : "w-60",
        )}
      >
        <div className={cn("pt-5 pb-3", collapsed ? "px-2" : "px-5")}>
          <div className={cn("flex items-center", collapsed ? "justify-center" : "gap-2.5")}>
            <div className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-accent/15 text-accent">
              <Radar size={18} />
            </div>
            {!collapsed && (
              <>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-semibold tracking-wide text-fg">ArbPilot</p>
                  <p className="truncate text-[10px] tracking-[0.2em] text-subtle uppercase">
                    {t.app.tagline}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={toggleSidebar}
                  title={t.app.collapseSidebar}
                  aria-label={t.app.collapseSidebar}
                  className="rounded-md p-1 text-subtle transition-colors hover:bg-hover hover:text-body"
                >
                  <PanelLeftClose size={15} />
                </button>
              </>
            )}
          </div>
          {collapsed && (
            <button
              type="button"
              onClick={toggleSidebar}
              title={t.app.expandSidebar}
              aria-label={t.app.expandSidebar}
              className="mt-2 flex w-full items-center justify-center rounded-md py-1 text-subtle transition-colors hover:bg-hover hover:text-body"
            >
              <PanelLeftOpen size={15} />
            </button>
          )}
        </div>

        <nav className={cn("flex-1 space-y-1", collapsed ? "px-2" : "px-3")}>
          {navItems.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.end}
              title={collapsed ? item.label : undefined}
              className={({ isActive }) =>
                cn(
                  "flex items-center rounded-lg py-2 text-sm transition-colors",
                  collapsed ? "justify-center px-0" : "gap-2.5 px-3",
                  isActive
                    ? "bg-accent/10 font-medium text-fg"
                    : "text-muted hover:bg-hover hover:text-body",
                )
              }
            >
              <span className="relative shrink-0">
                <item.icon size={16} />
                {collapsed && Boolean(item.badge) && (
                  <span className="absolute -top-0.5 -right-0.5 h-1.5 w-1.5 rounded-full bg-accent" />
                )}
              </span>
              {!collapsed && <span className="truncate">{item.label}</span>}
              {!collapsed && Boolean(item.badge) && (
                <span className="ml-auto rounded-full bg-accent/15 px-1.5 py-0.5 text-[10px] font-medium text-accent">
                  {item.badge! > 99 ? "99+" : item.badge}
                </span>
              )}
            </NavLink>
          ))}
        </nav>

        {!collapsed && (
          <div className="relative border-t border-line px-4 py-2 text-[11px] text-subtle">
            <div className="flex items-center justify-between gap-2 font-mono">
              <span>v{health.data?.version ?? "0.1.0"}</span>
              <div className="flex items-center gap-2">
                {health.data && <span>{formatUptime(health.data.uptime_secs)}</span>}
                <button
                  type="button"
                  title={online ? t.shell.online : t.shell.offline}
                  aria-label={online ? t.shell.online : t.shell.offline}
                  onClick={() => toggleStatusHint(online ? t.shell.online : t.shell.offline)}
                  className="inline-flex items-center"
                >
                  <span
                    className={cn("h-2 w-2 rounded-full", online ? "bg-accent" : "bg-danger")}
                  />
                </button>
                <button
                  type="button"
                  title={streamConnected ? t.shell.streamConnected : t.shell.streamDisconnected}
                  aria-label={streamConnected ? t.shell.streamConnected : t.shell.streamDisconnected}
                  onClick={() =>
                    toggleStatusHint(
                      streamConnected ? t.shell.streamConnected : t.shell.streamDisconnected,
                    )
                  }
                  className="inline-flex items-center"
                >
                  {streamConnected ? (
                    <Wifi size={13} className="text-info" />
                  ) : (
                    <WifiOff size={13} />
                  )}
                </button>
                {paused && (
                  <button
                    type="button"
                    title={t.shell.paused}
                    aria-label={t.shell.paused}
                    onClick={() => toggleStatusHint(t.shell.paused)}
                    className="inline-flex items-center"
                  >
                    <PauseCircle size={13} className="text-warning" />
                  </button>
                )}
              </div>
            </div>
            {statusHint && (
              <span className="absolute bottom-full left-3 z-10 mb-1 rounded-md border border-line bg-panel px-2 py-1 text-[10px] whitespace-nowrap text-body shadow-sm">
                {statusHint}
              </span>
            )}
          </div>
        )}
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center justify-end gap-3 border-b border-line bg-panel/70 px-6 backdrop-blur">
          <div className="flex items-center gap-2 text-xs text-muted">
            <AccountButton />
            <Button
              size="sm"
              variant="ghost"
              className="w-8 px-0"
              title={locale === "zh" ? "English" : "中文"}
              aria-label={t.app.language}
              onClick={toggleLocale}
            >
              <Languages size={15} />
            </Button>

            <Button
              size="sm"
              variant="ghost"
              className="w-8 px-0"
              title={resolved === "dark" ? t.app.light : t.app.dark}
              aria-label={t.app.theme}
              onClick={toggleTheme}
            >
              {resolved === "dark" ? <Sun size={15} /> : <Moon size={15} />}
            </Button>
          </div>
        </header>

        <UpdateBanner />

        <main className="min-h-0 flex-1 overflow-y-auto px-6 py-6">
          <Outlet />
        </main>
      </div>

      {/* 应用内通知：执行器按通知设置推送新闻与资金费率套利提醒。 */}
      <div className="pointer-events-none fixed right-4 bottom-4 z-50 flex w-80 flex-col gap-2">
        {notices.map((notice) => (
          <div
            key={notice.id}
            className="pointer-events-auto rounded-lg border border-line-strong bg-panel px-3 py-2.5 shadow-lg"
          >
            <div className="flex items-start justify-between gap-2">
              <p className="text-xs font-medium text-fg">{notice.title}</p>
              <button
                type="button"
                onClick={() => dismissNotice(notice.id)}
                title={t.shell.noticeClose}
                aria-label={t.shell.noticeClose}
                className="shrink-0 rounded p-0.5 text-subtle transition-colors hover:bg-hover hover:text-body"
              >
                <X size={13} />
              </button>
            </div>
            <p className="mt-1 whitespace-pre-wrap text-[11px] leading-relaxed text-muted">
              {notice.message}
            </p>
            <p className="mt-1 font-mono text-[10px] text-subtle">
              {notice.kind === "funding" ? t.settings.notifyFunding : t.settings.notifyNews} ·{" "}
              {formatDateTime(notice.at)}
            </p>
          </div>
        ))}
      </div>
    </div>
  );
}
