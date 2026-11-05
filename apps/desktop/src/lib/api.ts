import type {
  AccountsResponse,
  BacktestReport,
  Candle,
  CompileStrategyCodeResponse,
  ControlResponse,
  CreateOrderRequest,
  CreateOrderResponse,
  CreatePerformanceRequest,
  CreatePublicationRequest,
  Decimal,
  DeployStrategyCodeResponse,
  EquityHistoryResponse,
  ExecutorLicense,
  LeaderboardEntry,
  MarketConfig,
  SandboxDryRunRequest,
  SandboxRunReport,
  SettlementStatus,
  StrategyAuthorCard,
  StrategyPerformanceRecord,
  StrategyPublication,
  StrategySettlement,
  StrategySubscription,
  UpdatePublicationRequest,
  FundingRate,
  FundingStatus,
  FundingSummary,
  HealthResponse,
  ImportAccountRequest,
  NewsResponse,
  NotificationSettings,
  Opportunity,
  Order,
  Position,
  PublicationBundle,
  RiskLimits,
  StrategyDefinition,
  StrategyRecord,
  StrategyRunsResponse,
  StrategySource,
  StrategyStatus,
  StrategyTemplateView,
  StrategyVersion,
  StrategyCode,
  StrategyCodeView,
  TelegramPairingCode,
  TelegramStatus,
  TradingAccount,
} from "./types";
import { fmt, getCopy } from "./i18n";

const DEFAULT_EXECUTOR_URL = (
  (import.meta.env.VITE_EXECUTOR_URL as string | undefined) ?? "http://127.0.0.1:8787"
).replace(/\/$/, "");

const URL_STORAGE_KEY = "arbpilot.executor.url";

/** 当前连接的执行器地址；用户可在设置页改成自有服务器上的执行器。 */
export function getExecutorUrl(): string {
  return (localStorage.getItem(URL_STORAGE_KEY) || DEFAULT_EXECUTOR_URL).replace(/\/$/, "");
}

export function setExecutorUrl(url: string): void {
  const next = url.trim().replace(/\/$/, "");
  if (next && next !== DEFAULT_EXECUTOR_URL) {
    localStorage.setItem(URL_STORAGE_KEY, next);
  } else {
    localStorage.removeItem(URL_STORAGE_KEY);
  }
}

export function isLocalExecutor(): boolean {
  return /^https?:\/\/(127\.0\.0\.1|localhost)(:|\/|$)/.test(getExecutorUrl());
}

const TOKEN_STORAGE_KEY = "arbpilot.executor.token";
/** 官方托管模式在构建时注入默认令牌；源码本地运行回退到开发令牌。 */
export const DEFAULT_TOKEN =
  (import.meta.env.VITE_EXECUTOR_TOKEN as string | undefined) ?? "dev-local-token";

export function getApiToken(): string {
  return localStorage.getItem(TOKEN_STORAGE_KEY) || DEFAULT_TOKEN;
}

export function setApiToken(token: string): void {
  localStorage.setItem(TOKEN_STORAGE_KEY, token);
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(
  path: string,
  init: RequestInit = {},
  acceptedStatuses: number[] = [],
): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${getExecutorUrl()}${path}`, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${getApiToken()}`,
        ...(init.headers ?? {}),
      },
    });
  } catch {
    throw new ApiError(0, getCopy().errors.unreachable);
  }

  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;

  if (!response.ok && !acceptedStatuses.includes(response.status)) {
    const message = payload?.error?.message ?? fmt(getCopy().errors.http, { status: response.status });
    throw new ApiError(response.status, message);
  }

  return payload as T;
}

export const api = {
  health: () => request<HealthResponse>("/api/v1/health"),

  opportunities: () => request<Opportunity[]>("/api/v1/opportunities"),

  candles: (symbol: string, interval: string, limit: number, venue?: string) => {
    const params = new URLSearchParams({
      symbol,
      interval,
      limit: String(limit),
    });
    if (venue) {
      params.set("venue", venue);
    }
    return request<Candle[]>(`/api/v1/candles?${params.toString()}`);
  },

  positions: () => request<Position[]>("/api/v1/positions"),

  orders: () => request<Order[]>("/api/v1/orders"),

  /** 导入的交易所账户、余额与权益汇总。 */
  accounts: () => request<AccountsResponse>("/api/v1/accounts"),

  importAccount: (body: ImportAccountRequest) =>
    request<TradingAccount>("/api/v1/accounts", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  removeAccount: (id: string) =>
    request<null>(`/api/v1/accounts/${id}`, { method: "DELETE" }),

  refreshAccount: (id: string) =>
    request<TradingAccount>(`/api/v1/accounts/${id}/refresh`, { method: "POST" }),

  accountHistory: (hours: number) =>
    request<EquityHistoryResponse>(`/api/v1/accounts/history?hours=${hours}`),

  strategies: () => request<StrategyRecord[]>("/api/v1/strategies"),

  strategyTemplates: () => request<StrategyTemplateView[]>("/api/v1/templates"),

  instantiateTemplate: (key: string, name?: string) =>
    request<StrategyRecord>(`/api/v1/templates/${encodeURIComponent(key)}/instantiate`, {
      method: "POST",
      body: JSON.stringify({ name }),
    }),

  createStrategy: (body: {
    definition?: StrategyDefinition;
    name?: string;
    source?: StrategySource;
  }) =>
    request<StrategyRecord>("/api/v1/strategies", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  updateStrategy: (id: string, status: StrategyStatus) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ status }),
    }),

  updateStrategyDefinition: (id: string, definition: StrategyDefinition) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}/definition`, {
      method: "PATCH",
      body: JSON.stringify({ definition }),
    }),

  updateStrategyScope: (
    id: string,
    body: {
      spot_venue: string;
      perp_venue: string;
      symbols: string[];
      event_sources?: string[];
      event_keywords?: string[];
      event_window_secs?: number;
    },
  ) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}/scope`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),

  validateStrategy: (id: string) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}/validate`, { method: "POST" }),

  backtestStrategy: (id: string, days: number) =>
    request<BacktestReport>(`/api/v1/strategies/${id}/backtest`, {
      method: "POST",
      body: JSON.stringify({ days }),
    }),

  strategyVersions: (id: string) =>
    request<StrategyVersion[]>(`/api/v1/strategies/${id}/versions`),

  rollbackStrategy: (id: string, version: number) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}/rollback`, {
      method: "POST",
      body: JSON.stringify({ version }),
    }),

  deleteStrategy: (id: string) =>
    request<null>(`/api/v1/strategies/${id}`, { method: "DELETE" }),

  // 策略代码：自然语言生成、查看、编辑、编译与部署；运行仍走统一风控。
  strategyCode: (id: string) => request<StrategyCodeView>(`/api/v1/strategies/${id}/code`),

  saveStrategyCode: (id: string, body: { source: string; notes?: string }) =>
    request<StrategyCode>(`/api/v1/strategies/${id}/code`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),

  generateStrategyCode: (
    id: string,
    prompt: string,
    history?: { role: "user" | "assistant"; content: string }[],
  ) =>
    request<StrategyCode>(`/api/v1/strategies/${id}/code/generate`, {
      method: "POST",
      body: JSON.stringify({ prompt, history: history ?? [] }),
    }),

  compileStrategyCode: (id: string) =>
    request<CompileStrategyCodeResponse>(`/api/v1/strategies/${id}/code/compile`, {
      method: "POST",
    }),

  deployStrategyCode: (id: string) =>
    request<DeployStrategyCodeResponse>(`/api/v1/strategies/${id}/code/deploy`, {
      method: "POST",
    }),

  /** 运行实例与事件：策略在哪个交易所组合上跑、跑了多久、期间发生了什么。 */
  strategyRuns: (id: string) =>
    request<StrategyRunsResponse>(`/api/v1/strategies/${id}/runs`),

  // 策略市场：作者卡片、发布、订阅、表现与结算。
  marketAuthors: () => request<StrategyAuthorCard[]>("/api/v1/market/authors"),

  createMarketAuthor: (body: {
    display_name: string;
    headline?: string;
    bio?: string;
    payout_account?: string;
  }) =>
    request<StrategyAuthorCard>("/api/v1/market/authors", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  updateMarketAuthor: (
    id: string,
    body: { display_name?: string; headline?: string; bio?: string; payout_account?: string },
  ) =>
    request<StrategyAuthorCard>(`/api/v1/market/authors/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),

  publications: (options: { includeHidden?: boolean; authorId?: string } = {}) => {
    const params = new URLSearchParams();
    if (options.includeHidden) {
      params.set("include_hidden", "true");
    }
    if (options.authorId) {
      params.set("author_id", options.authorId);
    }
    const query = params.toString();
    return request<StrategyPublication[]>(
      `/api/v1/market/publications${query ? `?${query}` : ""}`,
    );
  },

  createPublication: (body: CreatePublicationRequest) =>
    request<StrategyPublication>("/api/v1/market/publications", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  publication: (id: string) => request<StrategyPublication>(`/api/v1/market/publications/${id}`),

  updatePublication: (id: string, body: UpdatePublicationRequest) =>
    request<StrategyPublication>(`/api/v1/market/publications/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),

  publicationPerformance: (id: string) =>
    request<StrategyPerformanceRecord[]>(`/api/v1/market/publications/${id}/performance`),

  createPerformance: (id: string, body: CreatePerformanceRequest) =>
    request<StrategyPerformanceRecord>(`/api/v1/market/publications/${id}/performance`, {
      method: "POST",
      body: JSON.stringify(body),
    }),

  leaderboard: () => request<LeaderboardEntry[]>("/api/v1/market/leaderboard"),

  /** 订阅只保存订阅者本地身份与支付引用，不上传任何账号密码。 */
  subscribe: (publicationId: string, subscriberId: string, paymentRef?: string) =>
    request<StrategySubscription>(`/api/v1/market/publications/${publicationId}/subscribe`, {
      method: "POST",
      body: JSON.stringify({ subscriber_id: subscriberId, payment_ref: paymentRef ?? null }),
    }),

  subscriptions: (subscriberId: string) =>
    request<StrategySubscription[]>(
      `/api/v1/market/subscriptions?subscriber_id=${encodeURIComponent(subscriberId)}`,
    ),

  /** 已订阅策略的交付物：冻结定义 + wasm 模块字节。 */
  publicationBundle: (publicationId: string, subscriberId: string) =>
    request<PublicationBundle>(
      `/api/v1/market/publications/${publicationId}/bundle?subscriber_id=${encodeURIComponent(subscriberId)}`,
    ),

  /** 把已订阅的策略导入为自己的自托管策略，代码在本机执行器运行。 */
  importPublication: (publicationId: string, subscriberId: string) =>
    request<StrategyRecord>(`/api/v1/market/publications/${publicationId}/import`, {
      method: "POST",
      body: JSON.stringify({ subscriber_id: subscriberId }),
    }),

  cancelSubscription: (id: string) =>
    request<StrategySubscription>(`/api/v1/market/subscriptions/${id}`, { method: "DELETE" }),

  settlements: (authorId?: string) =>
    request<StrategySettlement[]>(
      `/api/v1/market/settlements${authorId ? `?author_id=${encodeURIComponent(authorId)}` : ""}`,
    ),

  createSettlement: (authorId: string, period?: string) =>
    request<StrategySettlement>("/api/v1/market/settlements", {
      method: "POST",
      body: JSON.stringify({ author_id: authorId, period: period ?? null }),
    }),

  updateSettlement: (id: string, status: SettlementStatus, note?: string) =>
    request<StrategySettlement>(`/api/v1/market/settlements/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ status, note: note ?? null }),
    }),

  // 沙箱试运行：用当前行情快照执行一次已部署的模块，不产生任何订单。
  sandboxDryRun: (artifactId: string, body: SandboxDryRunRequest) =>
    request<SandboxRunReport>(`/api/v1/sandbox/artifacts/${artifactId}/dry-run`, {
      method: "POST",
      body: JSON.stringify(body),
    }),

  riskLimits: () => request<RiskLimits>("/api/v1/risk/limits"),

  /** 策略范围可选的交易所与交易对，来自执行器数据库配置。 */
  marketConfig: () => request<MarketConfig>("/api/v1/config/market"),

  /** 消息通知配置：新闻推送与资金费率套利提醒的开关、阈值与渠道。 */
  notificationSettings: () =>
    request<NotificationSettings>("/api/v1/config/notifications"),

  updateNotificationSettings: (settings: NotificationSettings) =>
    request<NotificationSettings>("/api/v1/config/notifications", {
      method: "PUT",
      body: JSON.stringify(settings),
    }),

  updateRiskLimits: (limits: RiskLimits) =>
    request<RiskLimits>("/api/v1/risk/limits", {
      method: "PUT",
      body: JSON.stringify(limits),
    }),

  fundingStatus: () => request<FundingStatus[]>("/api/v1/funding/status"),

  fundingHistory: (symbol: string | undefined, limit: number, venue: string) => {
    const params = new URLSearchParams({ limit: String(limit), venue });
    if (symbol) {
      params.set("symbol", symbol);
    }
    return request<FundingRate[]>(`/api/v1/funding/history?${params.toString()}`);
  },

  /** 时间窗口内各交易对的累计资金费率，一次请求覆盖全部币种。 */
  fundingSummary: (secs: number) =>
    request<FundingSummary[]>(`/api/v1/funding/summary?secs=${secs}`),

  /** 上币公告：Binance / Upbit / OKX / Bybit / Bitget 的采集结果与来源状态。 */
  news: (limit = 200) => request<NewsResponse>(`/api/v1/news?limit=${limit}`),

  telegramStatus: () => request<TelegramStatus>("/api/v1/telegram"),

  /** 执行器的离线授权状态；授权令牌由账号服务签发后写入执行器。 */
  licenseStatus: () => request<ExecutorLicense>("/api/v1/license"),

  installLicense: (token: string) =>
    request<ExecutorLicense>("/api/v1/license", {
      method: "POST",
      body: JSON.stringify({ token }),
    }),

  clearLicense: () => request<ExecutorLicense>("/api/v1/license", { method: "DELETE" }),

  createTelegramPairing: () =>
    request<TelegramPairingCode>("/api/v1/telegram/pairing", { method: "POST" }),

  revokeTelegramBinding: (userId: number) =>
    request<TelegramStatus>(`/api/v1/telegram/bindings/${userId}`, { method: "DELETE" }),

  pause: (reason?: string) =>
    request<ControlResponse>("/api/v1/control/pause", {
      method: "POST",
      body: JSON.stringify({ reason: reason ?? null }),
    }),

  resume: () =>
    request<ControlResponse>("/api/v1/control/resume", {
      method: "POST",
      body: JSON.stringify({}),
    }),

  createOrder: (body: CreateOrderRequest, idempotencyKey: string) =>
    request<CreateOrderResponse>(
      "/api/v1/orders",
      {
        method: "POST",
        headers: { "Idempotency-Key": idempotencyKey },
        body: JSON.stringify(body),
      },
      [422],
    ),

  eventsUrl: () =>
    `${getExecutorUrl().replace(/^http/, "ws")}/api/v1/events?token=${encodeURIComponent(getApiToken())}`,
};

/** 用于展示，不做精度敏感计算。 */
export function toNumber(value: Decimal | null | undefined): number {
  if (value === null || value === undefined) {
    return 0;
  }
  return typeof value === "number" ? value : Number(value);
}
