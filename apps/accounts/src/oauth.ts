import { createHash, randomBytes } from "node:crypto";

import { config, findProvider, type OAuthProviderConfig } from "./config";
import { HttpError } from "./http";
import { outboundFetch } from "./outbound";

export interface OAuthProfile {
  subject: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
}

interface PendingFlow {
  provider: string;
  /** 登录成功后浏览器回跳的前端地址（带配对码），与 OAuth 回调地址无关。 */
  returnUri: string;
  codeVerifier: string;
  createdAt: number;
}

const FLOW_TTL_MS = 10 * 60_000;
const flows = new Map<string, PendingFlow>();

function pruneFlows(): void {
  const now = Date.now();
  for (const [key, flow] of flows) {
    if (now - flow.createdAt > FLOW_TTL_MS) {
      flows.delete(key);
    }
  }
}

function base64url(buffer: Buffer): string {
  return buffer.toString("base64url");
}

export function listProviders(): Array<{ id: string; label: string }> {
  return config.providers.map((provider) => ({ id: provider.id, label: provider.label }));
}

export function startFlow(
  providerId: string,
  returnUri: string,
): { authorizeUrl: string } {
  const provider = findProvider(providerId);
  if (!provider) {
    throw new HttpError(404, `未启用登录方式 ${providerId}`, "unknown_provider");
  }

  pruneFlows();
  const codeVerifier = base64url(randomBytes(48));
  const state = base64url(randomBytes(24));
  flows.set(state, {
    provider: provider.id,
    returnUri,
    codeVerifier,
    createdAt: Date.now(),
  });

  const url = new URL(provider.authorizeUrl);
  url.searchParams.set("client_id", provider.clientId);
  url.searchParams.set("redirect_uri", provider.redirectUri);
  url.searchParams.set("response_type", "code");
  url.searchParams.set("scope", provider.scope);
  url.searchParams.set("state", state);
  url.searchParams.set(
    "code_challenge",
    base64url(createHash("sha256").update(codeVerifier).digest()),
  );
  url.searchParams.set("code_challenge_method", "S256");
  if (provider.id === "google") {
    url.searchParams.set("access_type", "online");
    url.searchParams.set("prompt", "select_account");
  }

  return { authorizeUrl: url.toString() };
}

export function consumeFlow(state: string): PendingFlow | null {
  const flow = flows.get(state);
  if (!flow) {
    return null;
  }
  flows.delete(state);
  if (Date.now() - flow.createdAt > FLOW_TTL_MS) {
    return null;
  }
  return flow;
}

async function exchangeCode(
  provider: OAuthProviderConfig,
  code: string,
  redirectUri: string,
  codeVerifier: string,
): Promise<string> {
  const body = new URLSearchParams({
    client_id: provider.clientId,
    client_secret: provider.clientSecret,
    code,
    redirect_uri: redirectUri,
    code_verifier: codeVerifier,
    grant_type: "authorization_code",
  });

  const response = await outboundFetch(provider.tokenUrl, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      Accept: "application/json",
    },
    body: body.toString(),
  });

  const payload = await response.json<{
    access_token?: string;
    error?: string;
    error_description?: string;
  }>();

  if (!response.ok || !payload?.access_token) {
    throw new HttpError(
      502,
      `登录凭证交换失败：${payload?.error_description ?? payload?.error ?? response.status}`,
      "oauth_exchange_failed",
    );
  }
  return payload.access_token;
}

async function fetchGoogleProfile(token: string): Promise<OAuthProfile> {
  const response = await outboundFetch("https://openidconnect.googleapis.com/v1/userinfo", {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!response.ok) {
    throw new HttpError(502, `读取 Google 账号失败：${response.status}`, "oauth_profile_failed");
  }
  const user = await response.json<{
    sub: string;
    email?: string;
    name?: string;
    picture?: string;
  }>();
  if (!user) {
    throw new HttpError(502, "读取 Google 账号失败：响应不是 JSON", "oauth_profile_failed");
  }

  return {
    subject: user.sub,
    email: user.email ?? null,
    name: user.name ?? user.email ?? "ArbPilot 用户",
    avatarUrl: user.picture ?? null,
  };
}

export async function finishFlow(
  providerId: string,
  code: string,
  flow: PendingFlow,
): Promise<OAuthProfile> {
  const provider = findProvider(providerId);
  if (!provider) {
    throw new HttpError(404, `未启用登录方式 ${providerId}`, "unknown_provider");
  }
  const token = await exchangeCode(provider, code, provider.redirectUri, flow.codeVerifier);
  return fetchGoogleProfile(token);
}
