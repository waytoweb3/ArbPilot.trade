import { useCallback, useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { Link } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { metaForPath, usePageMeta } from "../lib/seo";

const ACCOUNTS_URL = (
  (import.meta.env.VITE_ACCOUNTS_URL as string | undefined) ?? "http://127.0.0.1:8789"
).replace(/\/$/, "");

/** 令牌只存在当前标签页，关闭浏览器即失效。 */
const TOKEN_KEY = "arbpilot.admin.token";

interface AdminSubscription {
  id: string;
  plan: string;
  status: string;
  provider: string;
  providerOrderId: string | null;
  paymentRef: string | null;
  currentPeriodEnd: string | null;
  cancelAtPeriodEnd: boolean;
  createdAt: string | null;
  updatedAt: string | null;
}

interface AdminLicense {
  ref: string;
  plan: string | null;
  status: string;
  email: string | null;
  expiresAt: string | null;
  createdAt: string | null;
  updatedAt: string | null;
}

interface AdminDevice {
  name: string;
  platform: string;
  createdAt: string | null;
  lastSeenAt: string | null;
}

interface AdminUser {
  id: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
  createdAt: string;
  lastSeenAt: string | null;
  providers: string[];
  deviceCount: number;
  isAuthor: boolean;
  paid: boolean;
  plans: string[];
  status: string;
  currentPeriodEnd: string | null;
  subscriptions: AdminSubscription[];
  licenses: AdminLicense[];
  devices: AdminDevice[];
}

interface AdminSummary {
  totalUsers: number;
  freeUsers: number;
  paidUsers: number;
  activeSubscriptions: number;
  pastDueSubscriptions: number;
  activeDevices: number;
  newUsers7d: number;
  newUsers30d: number;
}

interface AdminOverview {
  generatedAt: string;
  summary: AdminSummary;
  users: AdminUser[];
  truncated: boolean;
}

type Tab = "all" | "free" | "paid";

class AdminRequestError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "AdminRequestError";
  }
}

async function fetchOverview(token: string): Promise<AdminOverview> {
  const response = await fetch(`${ACCOUNTS_URL}/api/admin/users`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  const payload = (await response.json().catch(() => null)) as
    | (Partial<AdminOverview> & { message?: string })
    | null;
  if (!response.ok) {
    throw new AdminRequestError(
      payload?.message ?? `请求失败（${response.status}）`,
      response.status,
    );
  }
  return payload as AdminOverview;
}

const STATUS_LABEL: Record<string, string> = {
  active: "生效中",
  canceling: "期末取消",
  past_due: "逾期",
  pending: "待支付",
  expired: "已过期",
  canceled: "已取消",
  none: "免费",
};

const PLAN_LABEL: Record<string, string> = {
  arbitrage: "自托管部署",
  strategy: "策略创建附加包",
};

function statusClass(status: string): string {
  if (status === "active") return "border-accent/40 bg-accent/10 text-accent";
  if (status === "canceling" || status === "pending")
    return "border-warning/40 bg-warning/10 text-warning";
  if (status === "past_due") return "border-danger/40 bg-danger/10 text-danger";
  return "border-line bg-panel-soft text-muted";
}

function formatTime(value: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDay(value: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

function daysLeft(value: string | null): string {
  if (!value) return "";
  const diff = Date.parse(value) - Date.now();
  if (Number.isNaN(diff)) return "";
  if (diff <= 0) return "已到期";
  return `剩余 ${Math.ceil(diff / 86_400_000)} 天`;
}

function planName(plan: string): string {
  return PLAN_LABEL[plan] ?? plan;
}

/** 运营后台：账号库里的注册用户、付费订阅与设备明细。 */
export default function AdminPage() {
  const { locale } = useI18n();
  usePageMeta(metaForPath("/admin", locale));

  const [token, setToken] = useState(() => sessionStorage.getItem(TOKEN_KEY) ?? "");
  const [draft, setDraft] = useState("");
  const [overview, setOverview] = useState<AdminOverview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("all");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const load = useCallback(async (value: string) => {
    setLoading(true);
    setError(null);
    try {
      setOverview(await fetchOverview(value));
    } catch (cause) {
      setOverview(null);
      setError(cause instanceof Error ? cause.message : "加载失败");
      // 令牌失效时回到登录页，避免一直停留在空表格。
      if (cause instanceof AdminRequestError && cause.status === 401) {
        sessionStorage.removeItem(TOKEN_KEY);
        setToken("");
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (token) {
      void load(token);
    }
  }, [token, load]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const value = draft.trim();
    if (!value) return;
    sessionStorage.setItem(TOKEN_KEY, value);
    setDraft("");
    setToken(value);
  };

  const logout = () => {
    sessionStorage.removeItem(TOKEN_KEY);
    setOverview(null);
    setError(null);
    setToken("");
  };

  const users = useMemo(() => {
    const items = overview?.users ?? [];
    const needle = query.trim().toLowerCase();
    return items.filter((user) => {
      if (tab === "free" && user.paid) return false;
      if (tab === "paid" && !user.paid) return false;
      if (!needle) return true;
      return [user.email ?? "", user.name, user.id].some((value) =>
        value.toLowerCase().includes(needle),
      );
    });
  }, [overview, tab, query]);

  if (!token) {
    return (
      <AdminShell>
        <div className="mx-auto max-w-md pt-10">
          <p className="eyebrow">运营后台</p>
          <h1 className="mt-3 text-[clamp(1.6rem,3vw,2.1rem)] font-semibold tracking-[-0.02em] text-fg">
            查看注册与付费用户
          </h1>
          <p className="mt-3 text-[13px] leading-[1.8] text-body">
            请输入账号服务中配置的 <code className="font-mono text-fg">ADMIN_TOKEN</code>
            。令牌只保存在当前标签页，关闭浏览器后需要重新输入。
          </p>

          <form onSubmit={submit} className="mt-6 space-y-3">
            <input
              type="password"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="ADMIN_TOKEN"
              autoFocus
              autoComplete="current-password"
              className="w-full rounded-xl border border-line bg-panel px-4 py-3 font-mono text-[13px] text-fg outline-none transition-colors placeholder:text-subtle focus:border-accent/60"
            />
            <button
              type="submit"
              disabled={loading || !draft.trim()}
              className="w-full rounded-full bg-accent-solid px-5 py-2.5 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover disabled:opacity-60"
            >
              {loading ? "验证中…" : "进入后台"}
            </button>
          </form>

          {error && <p className="mt-4 text-[12.5px] text-danger">{error}</p>}

          <Link
            to="/"
            className="mt-8 inline-block text-[12.5px] text-muted underline-offset-4 hover:text-fg hover:underline"
          >
            返回官网
          </Link>
        </div>
      </AdminShell>
    );
  }

  const summary = overview?.summary;
  const tabs: { id: Tab; label: string; count: number }[] = [
    { id: "all", label: "全部用户", count: summary?.totalUsers ?? 0 },
    { id: "free", label: "免费用户", count: summary?.freeUsers ?? 0 },
    { id: "paid", label: "付费用户", count: summary?.paidUsers ?? 0 },
  ];

  return (
    <AdminShell>
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <p className="eyebrow">运营后台</p>
          <h1 className="mt-2 text-[clamp(1.5rem,2.6vw,2rem)] font-semibold tracking-[-0.02em] text-fg">
            用户与订阅
          </h1>
          <p className="mt-2 text-[12px] text-muted">
            数据来自账号服务
            {overview ? ` · 更新于 ${formatTime(overview.generatedAt)}` : ""}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void load(token)}
            disabled={loading}
            className="rounded-full border border-line-strong px-4 py-2 text-[12.5px] text-body transition-colors hover:border-accent/50 hover:text-fg disabled:opacity-60"
          >
            {loading ? "刷新中…" : "刷新数据"}
          </button>
          <button
            type="button"
            onClick={logout}
            className="rounded-full border border-line px-4 py-2 text-[12.5px] text-muted transition-colors hover:border-line-strong hover:text-fg"
          >
            退出
          </button>
        </div>
      </div>

      {error && (
        <p className="mt-5 rounded-2xl border border-danger/30 bg-danger/5 px-5 py-3 text-[12.5px] text-danger">
          {error}
        </p>
      )}

      {overview?.truncated && (
        <p className="mt-5 rounded-2xl border border-warning/30 bg-warning/5 px-5 py-3 text-[12.5px] text-warning">
          用户数超过 5000，当前只展示最近注册的部分用户。
        </p>
      )}

      <div className="mt-6 grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
        <StatCard label="注册用户" value={summary?.totalUsers} />
        <StatCard label="免费用户" value={summary?.freeUsers} />
        <StatCard label="付费用户" value={summary?.paidUsers} accent />
        <StatCard
          label="生效订阅"
          value={summary?.activeSubscriptions}
          hint={summary?.pastDueSubscriptions ? `逾期 ${summary.pastDueSubscriptions}` : undefined}
        />
        <StatCard
          label="活跃设备"
          value={summary?.activeDevices}
          hint={summary ? `7 日新增 ${summary.newUsers7d} 人` : undefined}
        />
      </div>

      <div className="mt-8 flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap gap-1.5">
          {tabs.map((item) => (
            <button
              key={item.id}
              type="button"
              onClick={() => setTab(item.id)}
              className={`rounded-full border px-3.5 py-1.5 text-[12.5px] transition-colors ${
                tab === item.id
                  ? "border-accent/50 bg-accent/10 text-accent"
                  : "border-line text-muted hover:border-line-strong hover:text-fg"
              }`}
            >
              {item.label}
              <span className="ml-1.5 font-mono text-[11px] opacity-70">{item.count}</span>
            </button>
          ))}
        </div>
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="搜索邮箱 / 昵称 / 用户 ID"
          className="w-full rounded-full border border-line bg-panel px-4 py-2 text-[12.5px] text-fg outline-none transition-colors placeholder:text-subtle focus:border-accent/60 sm:w-72"
        />
      </div>

      <div className="mt-4 overflow-x-auto rounded-2xl border border-line bg-panel">
        <table className="w-full min-w-[1040px] border-collapse text-left">
          <thead>
            <tr className="border-b border-line text-[10.5px] uppercase tracking-[0.1em] text-subtle">
              <th className="px-4 py-3 font-medium">用户</th>
              <th className="px-3 py-3 font-medium">类型</th>
              <th className="px-3 py-3 font-medium">套餐</th>
              <th className="px-3 py-3 font-medium">状态</th>
              <th className="px-3 py-3 font-medium">到期时间</th>
              <th className="px-3 py-3 font-medium">设备</th>
              <th className="px-3 py-3 font-medium">注册时间</th>
              <th className="px-3 py-3 font-medium">最近活跃</th>
              <th className="px-3 py-3 font-medium" />
            </tr>
          </thead>
          <tbody>
            {users.map((user) => {
              const open = Boolean(expanded[user.id]);
              return (
                <UserRow
                  key={user.id}
                  user={user}
                  open={open}
                  onToggle={() =>
                    setExpanded((current) => ({ ...current, [user.id]: !current[user.id] }))
                  }
                />
              );
            })}
            {!loading && users.length === 0 && (
              <tr>
                <td colSpan={9} className="px-4 py-12 text-center text-[12.5px] text-muted">
                  {overview ? "没有符合条件的用户" : "暂无数据"}
                </td>
              </tr>
            )}
            {loading && !overview && (
              <tr>
                <td colSpan={9} className="px-4 py-12 text-center text-[12.5px] text-muted">
                  正在加载…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </AdminShell>
  );
}

function AdminShell({ children }: { children: ReactNode }) {
  return (
    <div className="min-h-screen bg-canvas">
      <header className="border-b border-line bg-panel/80 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-[1280px] items-center justify-between px-5">
          <span className="flex items-center gap-2 text-[13px] font-semibold text-fg">
            <span className="inline-flex h-6 w-6 items-center justify-center rounded-md bg-accent-solid text-[11px] font-bold text-on-accent">
              A
            </span>
            ArbPilot Admin
          </span>
          <Link
            to="/"
            className="text-[12px] text-muted underline-offset-4 hover:text-fg hover:underline"
          >
            arbpilot.trade
          </Link>
        </div>
      </header>
      <main className="mx-auto max-w-[1280px] px-5 pb-20 pt-8">{children}</main>
    </div>
  );
}

function StatCard({
  label,
  value,
  hint,
  accent = false,
}: {
  label: string;
  value: number | undefined;
  hint?: string;
  accent?: boolean;
}) {
  return (
    <div className="rounded-2xl border border-line bg-panel px-4 py-3.5">
      <p className="text-[10.5px] uppercase tracking-[0.1em] text-subtle">{label}</p>
      <p
        className={`mt-1.5 font-mono text-2xl font-semibold ${
          accent ? "text-accent" : "text-fg"
        }`}
      >
        {value ?? "—"}
      </p>
      {hint && <p className="mt-1 text-[11px] text-muted">{hint}</p>}
    </div>
  );
}

function UserRow({
  user,
  open,
  onToggle,
}: {
  user: AdminUser;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <>
      <tr className="border-b border-line/70 align-top transition-colors hover:bg-hover/60">
        <td className="px-4 py-3">
          <div className="flex items-start gap-2.5">
            {user.avatarUrl ? (
              <img
                src={user.avatarUrl}
                alt=""
                referrerPolicy="no-referrer"
                className="mt-0.5 h-7 w-7 rounded-full border border-line object-cover"
              />
            ) : (
              <span className="mt-0.5 inline-flex h-7 w-7 items-center justify-center rounded-full bg-panel-soft text-[11px] text-muted">
                {(user.name || user.email || "?").slice(0, 1).toUpperCase()}
              </span>
            )}
            <div className="min-w-0">
              <p className="truncate text-[12.5px] font-medium text-fg">
                {user.name || "未命名"}
                {user.isAuthor && (
                  <span className="ml-1.5 rounded border border-info/30 bg-info/10 px-1 py-0.5 text-[9.5px] font-normal text-info">
                    作者
                  </span>
                )}
              </p>
              <p className="truncate text-[11.5px] text-muted">{user.email ?? "未绑定邮箱"}</p>
              {user.providers.length > 0 && (
                <p className="mt-0.5 text-[10.5px] uppercase tracking-wide text-subtle">
                  {user.providers.join(" · ")}
                </p>
              )}
            </div>
          </div>
        </td>
        <td className="px-3 py-3">
          <span
            className={`inline-flex rounded-full border px-2 py-0.5 text-[10.5px] ${
              user.paid
                ? "border-accent/40 bg-accent/10 text-accent"
                : "border-line bg-panel-soft text-muted"
            }`}
          >
            {user.paid ? "付费" : "免费"}
          </span>
        </td>
        <td className="px-3 py-3 text-[12px] text-body">
          {user.plans.length > 0 ? user.plans.map(planName).join(" · ") : "—"}
        </td>
        <td className="px-3 py-3">
          <span
            className={`inline-flex rounded-full border px-2 py-0.5 text-[10.5px] ${statusClass(
              user.status,
            )}`}
          >
            {STATUS_LABEL[user.status] ?? user.status}
          </span>
        </td>
        <td className="px-3 py-3 text-[12px] text-body">
          {user.currentPeriodEnd ? (
            <>
              <span>{formatDay(user.currentPeriodEnd)}</span>
              <span className="ml-1 text-[10.5px] text-subtle">
                {daysLeft(user.currentPeriodEnd)}
              </span>
            </>
          ) : (
            "—"
          )}
        </td>
        <td className="px-3 py-3 font-mono text-[12px] text-body">{user.deviceCount}</td>
        <td className="px-3 py-3 text-[12px] text-muted">{formatDay(user.createdAt)}</td>
        <td className="px-3 py-3 text-[12px] text-muted">{formatTime(user.lastSeenAt)}</td>
        <td className="px-3 py-3 text-right">
          {user.paid && (
            <button
              type="button"
              onClick={onToggle}
              className="rounded-full border border-line px-2.5 py-1 text-[11px] text-muted transition-colors hover:border-accent/50 hover:text-fg"
            >
              {open ? "收起" : "明细"}
            </button>
          )}
        </td>
      </tr>
      {user.paid && open && (
        <tr className="border-b border-line/70 bg-panel-soft/60">
          <td colSpan={9} className="px-4 py-4">
            <div className="grid gap-4 lg:grid-cols-2">
              <DetailBlock title={`订阅记录（${user.subscriptions.length}）`}>
                {user.subscriptions.length === 0 ? (
                  <p className="text-[11.5px] text-muted">暂无订阅记录</p>
                ) : (
                  <div className="space-y-2">
                    {user.subscriptions.map((item) => (
                      <div
                        key={item.id}
                        className="rounded-xl border border-line bg-panel px-3 py-2.5"
                      >
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-[12px] font-medium text-fg">
                            {planName(item.plan)}
                          </span>
                          <span
                            className={`rounded-full border px-1.5 py-0.5 text-[10px] ${statusClass(
                              item.status,
                            )}`}
                          >
                            {STATUS_LABEL[item.status] ?? item.status}
                          </span>
                          <span className="text-[10.5px] text-subtle">{item.provider}</span>
                          {item.cancelAtPeriodEnd && (
                            <span className="text-[10.5px] text-warning">期末取消</span>
                          )}
                        </div>
                        <dl className="mt-1.5 grid gap-x-4 gap-y-1 text-[11px] sm:grid-cols-2">
                          <DetailItem label="当前周期结束" value={formatTime(item.currentPeriodEnd)} />
                          <DetailItem label="最近更新" value={formatTime(item.updatedAt)} />
                          <DetailItem label="支付订单号" value={item.providerOrderId} mono />
                          <DetailItem label="支付引用" value={item.paymentRef} mono />
                        </dl>
                      </div>
                    ))}
                  </div>
                )}
              </DetailBlock>

              <div className="space-y-4">
                <DetailBlock title={`授权记录（${user.licenses.length}）`}>
                  {user.licenses.length === 0 ? (
                    <p className="text-[11.5px] text-muted">暂无授权记录</p>
                  ) : (
                    <div className="space-y-1.5">
                      {user.licenses.slice(0, 6).map((item) => (
                        <div
                          key={item.ref}
                          className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-line bg-panel px-2.5 py-1.5 text-[11px]"
                        >
                          <span className="font-mono text-[10.5px] text-muted">{item.ref}</span>
                          <span className="flex items-center gap-2">
                            <span className="text-body">
                              {item.plan ? planName(item.plan) : "—"}
                            </span>
                            <span
                              className={`rounded-full border px-1.5 py-0.5 text-[10px] ${statusClass(
                                item.status,
                              )}`}
                            >
                              {STATUS_LABEL[item.status] ?? item.status}
                            </span>
                            <span className="text-subtle">{formatDay(item.expiresAt)}</span>
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </DetailBlock>

                <DetailBlock title={`设备（${user.devices.length}）`}>
                  {user.devices.length === 0 ? (
                    <p className="text-[11.5px] text-muted">暂无激活设备</p>
                  ) : (
                    <div className="space-y-1.5">
                      {user.devices.map((device, index) => (
                        <div
                          key={`${device.name}-${index}`}
                          className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-line bg-panel px-2.5 py-1.5 text-[11px]"
                        >
                          <span className="text-body">
                            {device.name}
                            {device.platform && (
                              <span className="ml-1.5 text-subtle">{device.platform}</span>
                            )}
                          </span>
                          <span className="text-subtle">最近 {formatTime(device.lastSeenAt)}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </DetailBlock>

                <p className="font-mono text-[10px] text-subtle">用户 ID：{user.id}</p>
              </div>
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

function DetailBlock({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div>
      <p className="mb-2 text-[10.5px] uppercase tracking-[0.1em] text-subtle">{title}</p>
      {children}
    </div>
  );
}

function DetailItem({
  label,
  value,
  mono = false,
}: {
  label: string;
  value: string | null;
  mono?: boolean;
}) {
  return (
    <div className="flex min-w-0 gap-1.5">
      <dt className="shrink-0 text-subtle">{label}</dt>
      <dd className={`truncate text-body ${mono ? "font-mono text-[10px]" : ""}`}>
        {value ?? "—"}
      </dd>
    </div>
  );
}
