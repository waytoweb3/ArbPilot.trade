import { config } from "./config";

/**
 * 与账号服务的内部集成：解析付款人、登记待支付订单、回写订阅状态。
 * 账号服务不可用时退化为旧版 ref 模式，不阻断收款。
 */

export interface AccountUser {
  id: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
  entitlement?: { plans: string[]; status: string };
}

let warned = false;

function warnOnce(error: unknown): void {
  if (warned) {
    return;
  }
  warned = true;
  console.warn(
    `[payments] 账号服务不可用（${config.accountsUrl}），授权将只记录在本地 ref 上：`,
    error instanceof Error ? error.message : error,
  );
}

async function post(path: string, payload: unknown, authorization?: string): Promise<Response> {
  return fetch(`${config.accountsUrl}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Internal-Token": config.accountsInternalToken,
      ...(authorization ? { Authorization: authorization } : {}),
    },
    body: JSON.stringify(payload),
  });
}

/** 用账号会话解析付款人；未登录或账号服务不可用时返回 null。 */
export async function resolveUser(authorization?: string): Promise<AccountUser | null> {
  if (!config.accountsUrl || !authorization) {
    return null;
  }
  try {
    const response = await post("/api/internal/session", {}, authorization);
    if (!response.ok) {
      return null;
    }
    return (await response.json()) as AccountUser;
  } catch (error) {
    warnOnce(error);
    return null;
  }
}

export interface CheckoutNotice {
  ref: string;
  userId: string;
  plan: string;
}

export async function notifyCheckout(input: CheckoutNotice): Promise<void> {
  if (!config.accountsUrl) {
    return;
  }
  try {
    const response = await post("/api/internal/checkout", {
      ref: input.ref,
      user_id: input.userId,
      plan: input.plan,
    });
    if (!response.ok) {
      throw new Error(`accounts: ${response.status}`);
    }
  } catch (error) {
    warnOnce(error);
  }
}

export interface LicenseNotice {
  ref: string;
  userId: string | null;
  plan: string | null;
  status: string;
  email: string | null;
  orderId: string | null;
  currentPeriodEnd: string | null;
}

export async function notifyLicense(input: LicenseNotice): Promise<void> {
  if (!config.accountsUrl) {
    return;
  }
  try {
    const response = await post("/api/internal/licenses", {
      ref: input.ref,
      user_id: input.userId,
      plan: input.plan,
      status: input.status,
      email: input.email,
      order_id: input.orderId,
      current_period_end: input.currentPeriodEnd,
    });
    if (!response.ok) {
      throw new Error(`accounts: ${response.status}`);
    }
  } catch (error) {
    warnOnce(error);
  }
}
