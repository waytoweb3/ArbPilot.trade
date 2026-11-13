import { config } from "./config";

/** 支付服务只读取发布定价与作者列表，并回写订阅状态；不接触策略代码或交易凭证。 */

export interface StrategyPublicationView {
  id: string;
  title: string;
  author_id: string;
  status: "private" | "published" | "withdrawn";
  pricing: {
    model: "free" | "one_time";
    monthly_price_usd: string | number;
    trial_days: number;
  };
}

export interface StrategyAuthorView {
  author_id: string;
  display_name: string;
}

export interface SubscriptionPatch {
  status: "pending_payment" | "trial" | "active" | "cancelled" | "expired";
  current_period_end?: string;
  trial_ends_at?: string;
  payment_ref?: string;
}

export class ExecutorError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ExecutorError";
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${config.executorUrl}${path}`, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${config.executorToken}`,
        ...init?.headers,
      },
    });
  } catch {
    throw new ExecutorError(0, "executor unreachable");
  }

  const text = await response.text();
  const payload = text ? (JSON.parse(text) as unknown) : null;
  if (!response.ok) {
    const message =
      payload && typeof payload === "object" && "message" in payload
        ? String((payload as { message: unknown }).message)
        : `executor: ${response.status}`;
    throw new ExecutorError(response.status, message);
  }
  return payload as T;
}

export function fetchPublication(id: string): Promise<StrategyPublicationView> {
  return call<StrategyPublicationView>(`/api/v1/market/publications/${encodeURIComponent(id)}`);
}

export function updateSubscription(id: string, patch: SubscriptionPatch): Promise<unknown> {
  return call(`/api/v1/market/subscriptions/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: JSON.stringify(patch),
  });
}

export function listAuthors(): Promise<StrategyAuthorView[]> {
  return call<StrategyAuthorView[]>("/api/v1/market/authors");
}

/** 幂等：同一作者同一周期已存在结算单时返回已有记录。 */
export function createSettlement(authorId: string, period?: string): Promise<unknown> {
  return call("/api/v1/market/settlements", {
    method: "POST",
    body: JSON.stringify({ author_id: authorId, period }),
  });
}
