import {
  Bot,
  LayoutDashboard,
  Radar,
  Settings as SettingsIcon,
  ShieldAlert,
  Wallet,
} from "lucide-react";
import { NavLink, Outlet } from "react-router-dom";

import { useHealth, usePauseExecutor, useResumeExecutor } from "../../hooks/useApi";
import { useExecutorStream } from "../../hooks/useExecutorStream";
import { formatUptime } from "../../lib/format";
import { cn } from "../../lib/utils";
import { useExecutorStore } from "../../state/executor";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";

const navItems = [
  { to: "/", label: "总览", icon: LayoutDashboard, end: true },
  { to: "/opportunities", label: "套利机会", icon: Radar },
  { to: "/positions", label: "仓位与订单", icon: Wallet },
  { to: "/strategies", label: "策略", icon: Bot },
  { to: "/settings", label: "设置", icon: SettingsIcon },
];

export function AppShell() {
  useExecutorStream();
  const health = useHealth();
  const pauseExecutor = usePauseExecutor();
  const resumeExecutor = useResumeExecutor();
  const streamConnected = useExecutorStore((state) => state.streamConnected);

  const online = Boolean(health.data) && !health.isError;
  const paused = health.data?.paused ?? false;

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-60 shrink-0 flex-col border-r border-ink-700/70 bg-ink-900/50">
        <div className="px-5 py-5">
          <div className="flex items-center gap-2.5">
            <div className="grid h-9 w-9 place-items-center rounded-lg bg-emerald-500/15 text-emerald-300">
              <Radar size={18} />
            </div>
            <div>
              <p className="text-sm font-semibold tracking-wide text-white">ArbPilot</p>
              <p className="text-[10px] tracking-[0.2em] text-slate-500 uppercase">
                self-hosted
              </p>
            </div>
          </div>
        </div>

        <nav className="flex-1 space-y-1 px-3">
          {navItems.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.end}
              className={({ isActive }) =>
                cn(
                  "flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors",
                  isActive
                    ? "bg-ink-800 text-white"
                    : "text-slate-400 hover:bg-ink-800/60 hover:text-slate-200",
                )
              }
            >
              <item.icon size={16} />
              {item.label}
            </NavLink>
          ))}
        </nav>

        <div className="border-t border-ink-700/60 p-4 text-[11px] leading-relaxed text-slate-500">
          <p>不托管资金 · 密钥留在执行器</p>
          <p className="mt-1">v{health.data?.version ?? "0.1.0"} · 模拟盘优先</p>
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center justify-between gap-3 border-b border-ink-700/70 bg-ink-900/40 px-6">
          <div className="flex flex-wrap items-center gap-2">
            <Badge tone={online ? "success" : "danger"}>
              {online ? "执行器在线" : "执行器离线"}
            </Badge>
            <Badge tone={streamConnected ? "info" : "neutral"}>
              {streamConnected ? "事件流已连接" : "事件流断开"}
            </Badge>
            <Badge tone="neutral">
              {health.data?.mode === "live" ? "真实交易" : "模拟盘"}
            </Badge>
            {paused && <Badge tone="warning">已暂停开仓</Badge>}
          </div>

          <div className="flex items-center gap-4 text-xs text-slate-500">
            {health.data && <span>持续 {formatUptime(health.data.uptime_secs)}</span>}
            {paused ? (
              <Button
                size="sm"
                onClick={() => resumeExecutor.mutate()}
                disabled={resumeExecutor.isPending || !online}
              >
                恢复开仓
              </Button>
            ) : (
              <Button
                size="sm"
                variant="danger"
                onClick={() => pauseExecutor.mutate("桌面端手动暂停")}
                disabled={pauseExecutor.isPending || !online}
              >
                <ShieldAlert size={13} />
                暂停开仓
              </Button>
            )}
          </div>
        </header>

        <main className="min-h-0 flex-1 overflow-y-auto px-6 py-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
