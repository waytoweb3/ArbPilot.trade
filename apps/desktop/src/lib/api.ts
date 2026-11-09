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

const OFFICIAL_DATA_URL = "https://exec.arbpilot.trade";
const SELF_URL_DEFAULT = "http://127.0.0.1:8787";

/**
 * 官方只读数据接口：资金费率、新闻与行情。
 * 非托管模式（内置执行器）默认使用官方地址，可用 VITE_ARBPILOT_DATA_URL 覆盖；
 * 托管模式（自建执行器）的请求全部走用户自己的服务器。
 */
const DATA_URL = (
  (import.meta.env.VITE_ARBPILOT_DATA_URL as string | undefined) ?? OFFICIAL_DATA_URL
).replace(/\/$/, "");

/** 桌面内置执行器与用户自己服务器上的远程执行器是两套独立配置。 */
export type ExecutorMode = "local" | "self";

const MODE_KEY = "arbpilot.executor.mode";
const URL_STORAGE_KEY = "arbpilot.executor.url";
const TOKEN_STORAGE_KEY = "arbpilot.executor.token";

/** 内置执行器的连接参数，由桌面端启动时注入。 */
let localRuntime: { baseUrl: string; token: string; database: string } | null = null;

function hasTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * 桌面端启动时调用：连接随包启动的本地执行器，用户无需任何配置。
 * 浏览器里（vite dev）没有 Tauri 运行时，直接跳过。
 */
export async function bootstrapDesktopExecutor(): Promise<void> {
  if (!hasTauriRuntime()) {
    return;
  }
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const config = await invoke<{
      base_url: string;
      token: string;
      database: string;
    } | null>("executor_local_config");
    if (!config?.base_url || !config.token) {
      return;
    }
    localRuntime = {
      baseUrl: config.base_url.replace(/\/$/, ""),
      token: config.token,
      database: config.database,
    };
    // 首次启动默认连内置执行器；用户已在设置里选择其他模式时保持不动。
    if (!localStorage.getItem(MODE_KEY)) {
      setExecutorMode("local");
    }
  } catch {
    // 内置执行器不可用时保持现有配置，由设置页提示用户手动填写地址。
  }
}

/** 内置执行器是否已就绪，设置页据此展示本地模式。 */
export function hasLocalExecutor(): boolean {
  return localRuntime !== null;
}

/** 内置执行器的数据库文件，仅桌面端可用。 */
export function localExecutorDatabase(): string {
  return localRuntime?.database ?? "";
}

export function getExecutorMode(): ExecutorMode {
  const stored = localStorage.getItem(MODE_KEY);
  if (stored === "self") {
    return "self";
  }
  // 内置执行器不可用时不要停留在本地模式，回退到远程执行器。
  if (stored === "local") {
    return localRuntime ? "local" : "self";
  }
  // 旧版本可能存过 "hosted"（官方只读实例）：官方地址不再作为连接目标，
  // 统一按自托管处理，地址取用户自己配置的服务器。
  return localRuntime ? "local" : "self";
}

export function setExecutorMode(mode: ExecutorMode): void {
  localStorage.setItem(MODE_KEY, mode);
}

/**
 * 规范化执行器地址：允许只填 IP 或域名（可带端口），缺省协议补 http://，去掉末尾斜杠。
 * 例：`45.76.177.242:8787` → `http://45.76.177.242:8787`
 */
export function normalizeExecutorUrl(input: string): string {
  const value = input.trim().replace(/\/+$/, "");
  if (!value) {
    return "";
  }
  return /^https?:\/\//i.test(value) ? value : `http://${value}`;
}

/** 当前模式的执行器地址；托管模式使用用户填写的服务器地址（域名或 IP）。 */
export function getExecutorUrl(mode: ExecutorMode = getExecutorMode()): string {
  if (mode === "local" && localRuntime) {
    return localRuntime.baseUrl;
  }
  return normalizeExecutorUrl(localStorage.getItem(URL_STORAGE_KEY) || SELF_URL_DEFAULT);
}

export function setExecutorUrl(url: string): void {
  const next = url.trim().replace(/\/$/, "");
  if (next) {
    localStorage.setItem(URL_STORAGE_KEY, next);
  } else {
    localStorage.removeItem(URL_STORAGE_KEY);
  }
}

/** 官方只读数据接口在构建时注入的令牌；源码本地运行回退到开发令牌。 */
const DEFAULT_TOKEN =
  (import.meta.env.VITE_EXECUTOR_TOKEN as string | undefined) ?? "dev-local-token";

export function getApiToken(mode: ExecutorMode = getExecutorMode()): string {
  if (mode === "local" && localRuntime) {
    return localRuntime.token;
  }
  return localStorage.getItem(TOKEN_STORAGE_KEY) ?? "";
}

/** 只保存自托管实例的令牌。 */
export function setApiToken(token: string): void {
  localStorage.setItem(TOKEN_STORAGE_KEY, token);
}

const LICENSE_TOKEN_KEY = "arbpilot.license.token";

/** 账号服务签发的用户令牌；访问官方只读数据接口时按用户校验套餐。 */
export function getLicenseToken(): string {
  return localStorage.getItem(LICENSE_TOKEN_KEY) ?? "";
}

export function setLicenseToken(token: string | null): void {
  if (token) {
    localStorage.setItem(LICENSE_TOKEN_KEY, token);
  } else {
    localStorage.removeItem(LICENSE_TOKEN_KEY);
  }
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

async function send<T>(
  url: string,
  headers: Record<string, string>,
  init: RequestInit,
  acceptedStatuses: number[],
): Promise<T> {
  let response: Response;
  try {
    response = await fetch(url, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        ...headers,
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

async function request<T>(
  path: string,
  init: RequestInit = {},
  acceptedStatuses: number[] = [],
): Promise<T> {
  // 内置执行器不校验用户授权（也没有公钥可验签），不要附带仅供托管实例使用的令牌。
  const license = getExecutorMode() === "local" ? "" : getLicenseToken();
  return send<T>(
    `${getExecutorUrl()}${path}`,
    {
      Authorization: `Bearer ${getApiToken()}`,
      ...(license ? { "X-ArbPilot-License": license } : {}),
    },
    init,
    acceptedStatuses,
  );
}

/**
 * 只读数据请求：内置执行器模式下优先走官方数据接口，不可用时回退到本机采集结果。
 * 交易类请求（下单、账户、策略、风控）始终使用 `request`。
 */
async function dataRequest<T>(
  path: string,
  init: RequestInit = {},
  acceptedStatuses: number[] = [],
): Promise<T> {
  if (getExecutorMode() !== "local" || !DATA_URL || DATA_URL === getExecutorUrl("local")) {
    return request<T>(path, init, acceptedStatuses);
  }
  const license = getLicenseToken();
  try {
    return await send<T>(
      `${DATA_URL}${path}`,
      {
        Authorization: `Bearer ${DEFAULT_TOKEN}`,
        ...(license ? { "X-ArbPilot-License": license } : {}),
      },
      init,
      acceptedStatuses,
    );
  } catch (error) {
    // 官方数据接口不可达或当前账号无权访问时，用内置执行器的本地采集兜底。
    if (error instanceof ApiError && [0, 401, 402, 403].includes(error.status)) {
      return request<T>(path, init, acceptedStatuses);
    }
    throw error;
  }
}

export const api = {
  health: () => request<HealthResponse>("/api/v1/health"),

  opportunities: () => dataRequest<Opportunity[]>("/api/v1/opportunities"),

  /** 交割合约基差排行：现货多 + 交割空。 */
  basisOpportunities: () => dataRequest<Opportunity[]>("/api/v1/opportunities/basis"),

  candles: (symbol: string, interval: string, limit: number, venue?: string) => {
    const params = new URLSearchParams({
      symbol,
      interval,
      limit: String(limit),
    });
    if (venue) {
      params.set("venue", venue);
    }
    return dataRequest<Candle[]>(`/api/v1/candles?${params.toString()}`);
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

  fundingStatus: () => dataRequest<FundingStatus[]>("/api/v1/funding/status"),

  fundingHistory: (symbol: string | undefined, limit: number, venue: string) => {
    const params = new URLSearchParams({ limit: String(limit), venue });
    if (symbol) {
      params.set("symbol", symbol);
    }
    return dataRequest<FundingRate[]>(`/api/v1/funding/history?${params.toString()}`);
  },

  /** 时间窗口内各交易对的累计资金费率，一次请求覆盖全部币种。 */
  fundingSummary: (secs: number) =>
    dataRequest<FundingSummary[]>(`/api/v1/funding/summary?secs=${secs}`),

  /** 上币公告：Binance / Upbit / OKX / Bybit / Bitget 的采集结果与来源状态。 */
  news: (limit = 200) => dataRequest<NewsResponse>(`/api/v1/news?limit=${limit}`),

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

  eventsUrl: () => {
    const params = new URLSearchParams({ token: getApiToken() });
    // 与 request 保持一致：内置执行器不需要用户授权令牌。
    const license = getExecutorMode() === "local" ? "" : getLicenseToken();
    if (license) {
      params.set("license", license);
    }
    return `${getExecutorUrl().replace(/^http/, "ws")}/api/v1/events?${params.toString()}`;
  },
};

/** 用于展示，不做精度敏感计算。 */
export function toNumber(value: Decimal | null | undefined): number {
  if (value === null || value === undefined) {
    return 0;
  }
  return typeof value === "number" ? value : Number(value);
}
