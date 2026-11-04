import type {
  AccountSnapshot,
  ControlResponse,
  CreateOrderRequest,
  CreateOrderResponse,
  Decimal,
  HealthResponse,
  Opportunity,
  Order,
  Position,
  RiskLimits,
  StrategyRecord,
  StrategyStatus,
} from "./types";

export const EXECUTOR_URL = (
  (import.meta.env.VITE_EXECUTOR_URL as string | undefined) ?? "http://127.0.0.1:8787"
).replace(/\/$/, "");

const TOKEN_STORAGE_KEY = "arbpilot.executor.token";
export const DEFAULT_TOKEN = "dev-local-token";

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
    response = await fetch(`${EXECUTOR_URL}${path}`, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${getApiToken()}`,
        ...(init.headers ?? {}),
      },
    });
  } catch {
    throw new ApiError(0, "无法连接执行器，请确认本地执行器已启动");
  }

  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;

  if (!response.ok && !acceptedStatuses.includes(response.status)) {
    const message = payload?.error?.message ?? `执行器返回 ${response.status}`;
    throw new ApiError(response.status, message);
  }

  return payload as T;
}

export const api = {
  health: () => request<HealthResponse>("/api/v1/health"),

  opportunities: () => request<Opportunity[]>("/api/v1/opportunities"),

  positions: () => request<Position[]>("/api/v1/positions"),

  orders: () => request<Order[]>("/api/v1/orders"),

  account: () => request<AccountSnapshot>("/api/v1/account"),

  strategies: () => request<StrategyRecord[]>("/api/v1/strategies"),

  updateStrategy: (id: string, status: StrategyStatus) =>
    request<StrategyRecord>(`/api/v1/strategies/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ status }),
    }),

  riskLimits: () => request<RiskLimits>("/api/v1/risk/limits"),

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
    `${EXECUTOR_URL.replace(/^http/, "ws")}/api/v1/events?token=${encodeURIComponent(getApiToken())}`,
};

/** 用于展示，不做精度敏感计算。 */
export function toNumber(value: Decimal | null | undefined): number {
  if (value === null || value === undefined) {
    return 0;
  }
  return typeof value === "number" ? value : Number(value);
}
