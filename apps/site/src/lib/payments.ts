export const PAYMENTS_URL = (
  (import.meta.env.VITE_PAYMENTS_URL as string | undefined) ?? "http://127.0.0.1:8788"
).replace(/\/$/, "");

export type PlanId = "arbitrage" | "strategy";

export interface PlanInfo {
  id: PlanId;
  amount: string;
  billingPeriod: "monthly";
  name: { zh: string; en: string };
}

export type LicenseStatus =
  | "pending"
  | "active"
  | "canceling"
  | "canceled"
  | "past_due"
  | "expired"
  | "missing"
  | "unknown";

export interface LicenseInfo {
  ref: string;
  status: LicenseStatus;
  plan: PlanId | null;
  email: string | null;
  expiresAt: string | null;
  updatedAt: string;
}

export async function fetchLicense(ref: string): Promise<LicenseInfo> {
  const response = await fetch(`${PAYMENTS_URL}/api/licenses/${encodeURIComponent(ref)}`);
  if (response.status === 404) {
    // 订单在创建收银台时就会落库，404 说明 ref 无效，不应继续轮询。
    return {
      ref,
      status: "missing",
      plan: null,
      email: null,
      expiresAt: null,
      updatedAt: "",
    };
  }
  if (!response.ok) {
    throw new Error(`license: ${response.status}`);
  }
  return (await response.json()) as LicenseInfo;
}
