export const PAYMENTS_URL = (
  (import.meta.env.VITE_PAYMENTS_URL as string | undefined) ?? "http://127.0.0.1:8788"
).replace(/\/$/, "");

export type BillingPlanId = "arbitrage" | "strategy";

export interface BillingPlan {
  id: BillingPlanId;
  amount: string;
  billingPeriod: "monthly";
  name: { zh: string; en: string };
  /** 附加包必须先拥有该基础套餐；基础版为 null。 */
  requires?: BillingPlanId | null;
  addon?: boolean;
}

export class BillingError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "BillingError";
  }
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
  try {
    return (await response.json()) as Record<string, unknown>;
  } catch {
    return null;
  }
}

export async function fetchPlans(): Promise<BillingPlan[]> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/plans`);
  } catch {
    throw new BillingError(0, "payments service unreachable");
  }
  const payload = await readJson(response);
  if (!response.ok || !payload || !Array.isArray(payload.plans)) {
    throw new BillingError(response.status, `plans: ${response.status}`);
  }
  return payload.plans as BillingPlan[];
}

export async function createCheckout(input: {
  plan: BillingPlanId;
  email?: string;
  locale: "zh" | "en";
  darkMode: boolean;
  /** 账号服务签发的会话令牌；收银台会绑定到该账号，未登录将被拒绝。 */
  session: string;
}): Promise<{ ref: string; checkoutUrl: string }> {
  const { session, ...body } = input;
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/checkout`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${session}` },
      body: JSON.stringify(body),
    });
  } catch {
    throw new BillingError(0, "payments service unreachable");
  }
  const payload = await readJson(response);
  const checkoutUrl = typeof payload?.checkoutUrl === "string" ? payload.checkoutUrl : null;
  const ref = typeof payload?.ref === "string" ? payload.ref : null;
  if (!response.ok || !checkoutUrl || !ref) {
    const message = typeof payload?.message === "string" ? payload.message : `checkout: ${response.status}`;
    throw new BillingError(response.status, message);
  }
  return { ref, checkoutUrl };
}

export interface StrategyCheckout {
  ref: string;
  checkoutUrl: string;
  expiresAt: string;
  amount: string;
}

/** 按策略发布定价创建收银台会话；订阅已由执行器创建为待支付。 */
export async function createStrategyCheckout(input: {
  publicationId: string;
  subscriberId: string;
  subscriptionId: string;
  locale: "zh" | "en";
  /** 账号服务签发的会话令牌；订阅人必须与登录账号一致。 */
  session: string;
}): Promise<StrategyCheckout> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/strategy-checkout`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${input.session}` },
      body: JSON.stringify({
        publication_id: input.publicationId,
        subscriber_id: input.subscriberId,
        subscription_id: input.subscriptionId,
        locale: input.locale,
      }),
    });
  } catch {
    throw new BillingError(0, "payments service unreachable");
  }
  const payload = await readJson(response);
  const checkoutUrl = typeof payload?.checkoutUrl === "string" ? payload.checkoutUrl : null;
  const ref = typeof payload?.ref === "string" ? payload.ref : null;
  if (!response.ok || !checkoutUrl || !ref) {
    const message =
      typeof payload?.message === "string" ? payload.message : `strategy checkout: ${response.status}`;
    throw new BillingError(response.status, message);
  }
  return {
    ref,
    checkoutUrl,
    expiresAt: typeof payload?.expiresAt === "string" ? payload.expiresAt : "",
    amount: typeof payload?.amount === "string" ? payload.amount : "",
  };
}
