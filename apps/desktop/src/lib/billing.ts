export const PAYMENTS_URL = (
  (import.meta.env.VITE_PAYMENTS_URL as string | undefined) ?? "http://127.0.0.1:8788"
).replace(/\/$/, "");

export type BillingPlanId = "arbitrage" | "strategy";

export type LicenseStatus =
  | "pending"
  | "active"
  | "canceling"
  | "canceled"
  | "past_due"
  | "expired"
  | "unknown";

export interface BillingPlan {
  id: BillingPlanId;
  amount: string;
  billingPeriod: "monthly";
  name: { zh: string; en: string };
}

export interface License {
  ref: string;
  status: LicenseStatus;
  plan: BillingPlanId | null;
  email: string | null;
  expiresAt: string | null;
  updatedAt: string;
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

const REF_KEY = "arbpilot.license.ref";

export function getLicenseRef(): string | null {
  return localStorage.getItem(REF_KEY);
}

export function setLicenseRef(ref: string | null): void {
  if (ref) {
    localStorage.setItem(REF_KEY, ref);
  } else {
    localStorage.removeItem(REF_KEY);
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
}): Promise<{ ref: string; checkoutUrl: string }> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/checkout`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
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

export async function fetchLicense(ref: string): Promise<License> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/licenses/${encodeURIComponent(ref)}`);
  } catch {
    throw new BillingError(0, "payments service unreachable");
  }
  if (response.status === 404) {
    return { ref, status: "unknown", plan: null, email: null, expiresAt: null, updatedAt: "" };
  }
  const payload = await readJson(response);
  if (!response.ok || !payload) {
    throw new BillingError(response.status, `license: ${response.status}`);
  }
  return payload as unknown as License;
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
}): Promise<StrategyCheckout> {
  let response: Response;
  try {
    response = await fetch(`${PAYMENTS_URL}/api/strategy-checkout`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
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
