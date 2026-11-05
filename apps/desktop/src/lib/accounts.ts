export const ACCOUNTS_URL = (
  (import.meta.env.VITE_ACCOUNTS_URL as string | undefined) ?? "http://127.0.0.1:8789"
).replace(/\/$/, "");

const SESSION_KEY = "arbpilot.session";
const FINGERPRINT_KEY = "arbpilot.device.fingerprint";

export interface AccountUser {
  id: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
}

export interface AccountDevice {
  id: string;
  name: string;
  platform: string;
  fingerprint: string;
  createdAt: string;
  lastSeenAt: string;
}

export interface AccountEntitlement {
  plans: string[];
  status: "active" | "canceling" | "past_due" | "none";
  expiresAt: string | null;
  deviceLimit: number;
}

export interface AccountMe {
  user: AccountUser;
  entitlement: AccountEntitlement;
  devices: AccountDevice[];
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
  device?: AccountDevice | null;
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

/** 每台安装生成一次设备指纹，用于设备数量限制与撤销。 */
export function deviceFingerprint(): string {
  const existing = localStorage.getItem(FINGERPRINT_KEY);
  if (existing) {
    return existing;
  }
  const generated = crypto.randomUUID();
  localStorage.setItem(FINGERPRINT_KEY, generated);
  return generated;
}

export function devicePlatform(): string {
  const agent = navigator.userAgent;
  if (agent.includes("Mac")) return "macos";
  if (agent.includes("Win")) return "windows";
  if (agent.includes("Linux")) return "linux";
  return "unknown";
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
    body: JSON.stringify({
      fingerprint: deviceFingerprint(),
      client_name: "ArbPilot Desktop",
      platform: devicePlatform(),
    }),
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

export function activateDevice(token: string): Promise<{ device: AccountDevice }> {
  return request(
    "/api/me/devices",
    {
      method: "POST",
      body: JSON.stringify({
        fingerprint: deviceFingerprint(),
        name: "ArbPilot Desktop",
        platform: devicePlatform(),
      }),
    },
    token,
  );
}

export function revokeDevice(token: string, deviceId: string): Promise<{ ok: true }> {
  return request(`/api/me/devices/${encodeURIComponent(deviceId)}`, { method: "DELETE" }, token);
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
    device: string | null;
    exp: number;
  };
}

/** 换取授权令牌；设备需先用 activateDevice 登记，否则返回 403。 */
export function fetchLicenseToken(token: string, fingerprint: string): Promise<LicenseGrant> {
  return request<LicenseGrant>(
    "/api/me/license-token",
    { method: "POST", body: JSON.stringify({ device_fingerprint: fingerprint }) },
    token,
  );
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
