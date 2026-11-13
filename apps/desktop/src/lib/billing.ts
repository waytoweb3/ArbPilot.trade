export const PAYMENTS_URL = (
  (import.meta.env.VITE_PAYMENTS_URL as string | undefined) ?? "http://127.0.0.1:8788"
).replace(/\/$/, "");

export type BillingPlanId = "arbitrage";

export interface BillingPlan {
  id: BillingPlanId;
  amount: string;
  billingPeriod: "monthly";
  name: { zh: string; en: string };
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

export type CryptoPayCurrency = "usdttrc20" | "usdc";

export interface CryptoStrategyCheckout {
  ref: string;
  invoiceUrl: string;
  amount: string;
}

/** 策略买断（加密）：一次付清买断价，付款确认后由 IPN 回写永久授权。 */
export async function createCryptoStrategyCheckout(input: {
  publicationId: string;
  subscriberId: string;
  subscriptionId: string;
  payCurrency: CryptoPayCurrency;
  session: string;
}): Promise<CryptoStrategyCheckout> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/crypto/strategy-checkout`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${input.session}` },
      body: JSON.stringify({
        publication_id: input.publicationId,
        subscriber_id: input.subscriberId,
        subscription_id: input.subscriptionId,
        pay_currency: input.payCurrency,
      }),
    });
  } catch {
    throw new BillingError(0, "payments service unreachable");
  }
  const payload = await readJson(response);
  const invoiceUrl = typeof payload?.invoiceUrl === "string" ? payload.invoiceUrl : null;
  const ref = typeof payload?.ref === "string" ? payload.ref : null;
  if (!response.ok || !invoiceUrl || !ref) {
    const message =
      typeof payload?.message === "string"
        ? payload.message
        : `crypto checkout: ${response.status}`;
    throw new BillingError(response.status, message);
  }
  return {
    ref,
    invoiceUrl,
    amount: typeof payload?.amount === "string" ? payload.amount : "",
  };
}

export interface AuthorWallet {
  accountId: string;
  currency: CryptoPayCurrency;
  address: string;
  updatedAt: string;
}

export async function getAuthorWallet(session: string): Promise<AuthorWallet | null> {
  const response = await fetch(`${PAYMENTS_URL}/api/crypto/wallet`, {
    headers: { Authorization: `Bearer ${session}` },
  });
  const payload = await readJson(response);
  if (!response.ok) {
    throw new BillingError(response.status, `wallet: ${response.status}`);
  }
  return (payload?.wallet as AuthorWallet | null) ?? null;
}

export async function saveAuthorWallet(input: {
  session: string;
  currency: CryptoPayCurrency;
  address: string;
}): Promise<AuthorWallet> {
  const response = await fetch(`${PAYMENTS_URL}/api/crypto/wallet`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${input.session}` },
    body: JSON.stringify({ currency: input.currency, address: input.address }),
  });
  const payload = await readJson(response);
  const wallet = payload?.wallet as AuthorWallet | undefined;
  if (!response.ok || !wallet) {
    const message =
      typeof payload?.message === "string" ? payload.message : `wallet: ${response.status}`;
    throw new BillingError(response.status, message);
  }
  return wallet;
}
