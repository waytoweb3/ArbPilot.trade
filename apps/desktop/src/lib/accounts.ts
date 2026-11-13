export const ACCOUNTS_URL = (
  (import.meta.env.VITE_ACCOUNTS_URL as string | undefined) ?? "http://127.0.0.1:8789"
).replace(/\/$/, "");

const SESSION_KEY = "arbpilot.session";

export interface AccountUser {
  id: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
}

export interface AccountEntitlement {
  plans: string[];
  status: "active" | "canceling" | "past_due" | "none";
  expiresAt: string | null;
}

export interface AccountMe {
  user: AccountUser;
  entitlement: AccountEntitlement;
}

export interface AccountSubscription {
  id: string;
  plan: string;
  status: string;
  provider: string;
  paymentRef: string | null;
  providerOrderId: string | null;
  currentPeriodEnd: string | null;
  cancelAtPeriodEnd: boolean;
  createdAt?: string;
}

export interface DeviceCodeStart {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string;
  expires_in: number;
  interval: number;
}

export type DeviceCodeStatus = "pending" | "approved" | "denied" | "expired";

export interface DeviceCodePoll {
  status: DeviceCodeStatus;
  token?: string;
  expires_at?: string;
  user?: AccountUser | null;
}

export class AccountError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly code?: string,
  ) {
    super(message);
    this.name = "AccountError";
  }
}

export function getSessionToken(): string | null {
  return localStorage.getItem(SESSION_KEY);
}

export function setSessionToken(token: string | null): void {
  if (token) {
    localStorage.setItem(SESSION_KEY, token);
  } else {
    localStorage.removeItem(SESSION_KEY);
  }
}

async function request<T>(
  path: string,
  init?: RequestInit,
  token?: string | null,
): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${ACCOUNTS_URL}${path}`, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        ...init?.headers,
      },
    });
  } catch {
    throw new AccountError(0, "accounts service unreachable");
  }

  const text = await response.text();
  const payload = text ? (JSON.parse(text) as Record<string, unknown>) : null;
  if (!response.ok) {
    const message =
      typeof payload?.message === "string" ? payload.message : `accounts: ${response.status}`;
    const code = typeof payload?.error === "string" ? payload.error : undefined;
    throw new AccountError(response.status, message, code);
  }
  return payload as T;
}

export function fetchMe(token: string): Promise<AccountMe> {
  return request<AccountMe>("/api/me", undefined, token);
}

export function startDeviceLogin(): Promise<DeviceCodeStart> {
  return request<DeviceCodeStart>("/api/auth/device/start", {
    method: "POST",
    body: JSON.stringify({ client_name: "ArbPilot Desktop" }),
  });
}

export function pollDeviceLogin(deviceCode: string): Promise<DeviceCodePoll> {
  return request<DeviceCodePoll>("/api/auth/device/poll", {
    method: "POST",
    body: JSON.stringify({ device_code: deviceCode }),
  });
}

export async function logout(token: string): Promise<void> {
  try {
    await request("/api/auth/logout", { method: "POST" }, token);
  } finally {
    setSessionToken(null);
  }
}

export function fetchSubscriptions(token: string): Promise<{ subscriptions: AccountSubscription[] }> {
  return request("/api/me/subscriptions", undefined, token);
}

export function cancelSubscription(
  token: string,
  subscriptionId: string,
): Promise<{ subscription_id: string; status: string; message: string }> {
  return request(
    "/api/me/billing/cancel",
    { method: "POST", body: JSON.stringify({ subscription_id: subscriptionId }) },
    token,
  );
}

/** 账号服务签发的短期授权令牌，由执行器用公钥离线校验。 */
export interface LicenseGrant {
  token: string;
  expires_at: string;
  claims: {
    sub: string;
    email: string | null;
    plans: string[];
    status: string;
    exp: number;
  };
}

export function fetchLicenseToken(token: string): Promise<LicenseGrant> {
  return request<LicenseGrant>("/api/me/license-token", { method: "POST" }, token);
}

// ---------------------------------------------------------------------------
// 作者身份
// ---------------------------------------------------------------------------

export interface AuthorProfile {
  displayName: string;
  headline: string;
  bio: string;
  payoutAccount: string | null;
  executorAuthorId: string | null;
}

export function fetchAuthorProfile(token: string): Promise<{ author: AuthorProfile | null }> {
  return request("/api/me/author", undefined, token);
}

export function saveAuthorProfile(
  token: string,
  input: { displayName: string; headline: string; bio: string; payoutAccount: string | null },
): Promise<AuthorProfile> {
  return request(
    "/api/me/author",
    {
      method: "PUT",
      body: JSON.stringify({
        display_name: input.displayName,
        headline: input.headline,
        bio: input.bio,
        payout_account: input.payoutAccount,
      }),
    },
    token,
  );
}
