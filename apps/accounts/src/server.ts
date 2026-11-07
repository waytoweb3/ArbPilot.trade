import { timingSafeEqual } from "node:crypto";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";

import { adminOverview } from "./admin";
import { config, isAllowedRedirect } from "./config";
import { closePool, migrate, pool } from "./db";
import {
  HttpError,
  applyCors,
  bearer,
  readJson,
  redirect,
  sendJson,
  sendText,
  text,
} from "./http";
import { issueLicenseToken, publicKeyInfo } from "./license";
import {
  consumeFlow,
  finishFlow,
  listProviders,
  startFlow,
} from "./oauth";
import {
  activateDevice,
  applyLicenseEvent,
  approveDeviceCode,
  authorProfile,
  claimDeviceCode,
  createDeviceCode,
  createPendingCheckout,
  createSession,
  denyDeviceCode,
  deviceIds,
  deviceOf,
  entitlements,
  findOrCreateUser,
  listDevices,
  listSubscriptions,
  markCanceling,
  pollDeviceCode,
  revokeDevice,
  revokeSession,
  saveAuthorProfile,
  userFromToken,
  type UserRow,
} from "./store";

// ---------------------------------------------------------------------------
// 鉴权辅助
// ---------------------------------------------------------------------------

async function requireUser(req: IncomingMessage): Promise<UserRow> {
  const token = bearer(req);
  const user = token ? await userFromToken(token) : null;
  if (!user) {
    throw new HttpError(401, "请先登录", "unauthorized");
  }
  return user;
}

function requireInternal(req: IncomingMessage): void {
  const header = req.headers["x-internal-token"];
  const value = Array.isArray(header) ? header[0] : header;
  const expected = Buffer.from(config.internalToken);
  const actual = Buffer.from(value ?? "");
  if (expected.length !== actual.length || !timingSafeEqual(expected, actual)) {
    throw new HttpError(401, "内部接口需要有效令牌", "forbidden");
  }
}

/** 运营后台：用 ADMIN_TOKEN 作为 Bearer 令牌，未配置时直接禁用接口。 */
function requireAdmin(req: IncomingMessage): void {
  if (!config.adminToken) {
    throw new HttpError(
      503,
      "运营后台未启用：请在账号服务配置 ADMIN_TOKEN 后重启",
      "admin_disabled",
    );
  }
  const token = bearer(req);
  const expected = Buffer.from(config.adminToken);
  const actual = Buffer.from(token ?? "");
  if (expected.length !== actual.length || !timingSafeEqual(expected, actual)) {
    throw new HttpError(401, "管理令牌无效", "unauthorized");
  }
}

function userPayload(user: UserRow): Record<string, unknown> {
  return {
    id: user.id,
    email: user.email,
    name: user.name,
    avatarUrl: user.avatar_url,
  };
}

function normalizeCode(value: string): string {
  return value.replace(/[^A-Za-z0-9]/g, "").toUpperCase();
}

// ---------------------------------------------------------------------------
// 执行器：作者身份
// ---------------------------------------------------------------------------

interface ExecutorAuthor {
  author_id: string;
  display_name: string;
}

async function callExecutor<T>(path: string, init?: RequestInit): Promise<T> {
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
    throw new HttpError(502, "执行器不可达，无法同步作者信息", "executor_unreachable");
  }
  const body = await response.text();
  const payload = body ? (JSON.parse(body) as unknown) : null;
  if (!response.ok) {
    const message =
      payload && typeof payload === "object" && "message" in payload
        ? String((payload as { message: unknown }).message)
        : `executor: ${response.status}`;
    throw new HttpError(502, message, "executor_error");
  }
  return payload as T;
}

async function syncExecutorAuthor(
  executorAuthorId: string | null,
  profile: { displayName: string; headline: string; bio: string; payoutAccount: string | null },
): Promise<string> {
  if (executorAuthorId) {
    const updated = await callExecutor<ExecutorAuthor>(
      `/api/v1/market/authors/${encodeURIComponent(executorAuthorId)}`,
      {
        method: "PATCH",
        body: JSON.stringify({
          display_name: profile.displayName,
          headline: profile.headline,
          bio: profile.bio,
          payout_account: profile.payoutAccount ?? "",
        }),
      },
    );
    return updated.author_id;
  }

  const created = await callExecutor<ExecutorAuthor>("/api/v1/market/authors", {
    method: "POST",
    body: JSON.stringify({
      display_name: profile.displayName,
      headline: profile.headline,
      bio: profile.bio,
      payout_account: profile.payoutAccount,
    }),
  });
  return created.author_id;
}

// ---------------------------------------------------------------------------
// 支付服务
// ---------------------------------------------------------------------------

async function cancelProviderSubscription(orderId: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`${config.paymentsUrl}/api/subscriptions/cancel`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Internal-Token": config.internalToken,
      },
      body: JSON.stringify({ order_id: orderId }),
    });
  } catch {
    throw new HttpError(502, "收款服务不可达，暂时无法取消订阅", "payments_unreachable");
  }
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as { message?: string } | null;
    throw new HttpError(
      502,
      payload?.message ?? `收款服务返回 ${response.status}`,
      "cancel_failed",
    );
  }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

async function handleAuthCallback(
  res: ServerResponse,
  providerId: string,
  url: URL,
): Promise<void> {
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const denied = url.searchParams.get("error");
  if (!state) {
    sendText(res, 400, "缺少 state");
    return;
  }

  const flow = consumeFlow(state);
  if (!flow || flow.provider !== providerId) {
    sendText(res, 400, "登录会话已失效，请重新发起登录");
    return;
  }

  // 用户在授权页点取消时，跳回确认页并提示，而不是停在纯文本错误页。
  if (denied || !code) {
    const target = new URL(flow.returnUri);
    target.searchParams.set(
      "auth_error",
      url.searchParams.get("error_description") ?? denied ?? "授权未完成",
    );
    redirect(res, target.toString());
    return;
  }

  try {
    const profile = await finishFlow(providerId, code, flow);
    const user = await findOrCreateUser(providerId, profile);
    const session = await createSession(user.id, "web");
    const target = new URL(flow.returnUri);
    target.searchParams.set("session", session.token);
    redirect(res, target.toString());
  } catch (error) {
    const message = error instanceof Error ? error.message : "登录失败";
    const target = new URL(flow.returnUri);
    target.searchParams.set("auth_error", message);
    redirect(res, target.toString());
  }
}

async function handleDeviceStart(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const body = await readJson(req);
  const fingerprint = text(body.fingerprint);
  if (!fingerprint) {
    throw new HttpError(400, "缺少设备指纹", "invalid_request");
  }

  const code = await createDeviceCode({
    clientName: text(body.client_name) ?? "ArbPilot Desktop",
    platform: text(body.platform) ?? "",
    fingerprint,
  });

  sendJson(res, 201, {
    device_code: code.deviceCode,
    user_code: code.userCode,
    verification_uri: `${config.siteUrl}/device`,
    verification_uri_complete: `${config.siteUrl}/device?code=${code.userCode}`,
    expires_in: config.deviceCodeTtlSecs,
    interval: 3,
  });
}

async function handleDevicePoll(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const body = await readJson(req);
  const deviceCode = text(body.device_code);
  if (!deviceCode) {
    throw new HttpError(400, "缺少 device_code", "invalid_request");
  }

  const row = await pollDeviceCode(deviceCode);
  if (!row) {
    throw new HttpError(404, "配对请求不存在", "invalid_device_code");
  }
  if (row.expires_at.getTime() < Date.now() && row.status === "pending") {
    sendJson(res, 200, { status: "expired" });
    return;
  }
  if (row.status !== "approved") {
    sendJson(res, 200, { status: row.status });
    return;
  }
  if (!row.user_id) {
    sendJson(res, 200, { status: "denied" });
    return;
  }

  const claimed = await claimDeviceCode(row.id);
  if (!claimed) {
    throw new HttpError(409, "该配对码已被使用", "already_claimed");
  }

  const device = row.device_id ? await deviceOf(row.device_id) : null;
  const session = await createSession(row.user_id, "device");
  const userRow = await userFromToken(session.token);
  const devices = await listDevices(row.user_id);

  sendJson(res, 200, {
    status: "approved",
    token: session.token,
    expires_at: session.expiresAt,
    user: userRow ? userPayload(userRow) : null,
    device: device ?? devices[0] ?? null,
  });
}

async function handleDeviceApprove(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const user = await requireUser(req);
  const body = await readJson(req);
  const code = text(body.user_code);
  if (!code) {
    throw new HttpError(400, "缺少配对码", "invalid_request");
  }

  const userCode = `${normalizeCode(code).slice(0, 4)}-${normalizeCode(code).slice(4)}`;
  const { rows } = await pool.query<{
    fingerprint: string;
    client_name: string;
    platform: string;
  }>(
    "SELECT fingerprint, client_name, platform FROM device_codes WHERE user_code = $1 AND status = 'pending' AND expires_at > now()",
    [userCode],
  );
  const pending = rows[0];
  if (!pending) {
    throw new HttpError(404, "配对码不存在或已过期", "invalid_user_code");
  }

  const device = await activateDevice(user.id, {
    fingerprint: pending.fingerprint,
    name: pending.client_name || "ArbPilot Desktop",
    platform: pending.platform,
  });
  await approveDeviceCode(userCode, user.id, device);
  sendJson(res, 200, { status: "approved", device });
}

async function handleDeviceDeny(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const user = await requireUser(req);
  const body = await readJson(req);
  const code = text(body.user_code);
  if (!code) {
    throw new HttpError(400, "缺少配对码", "invalid_request");
  }
  const userCode = `${normalizeCode(code).slice(0, 4)}-${normalizeCode(code).slice(4)}`;
  await denyDeviceCode(userCode, user.id);
  sendJson(res, 200, { status: "denied" });
}

async function handleLicenseToken(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const user = await requireUser(req);
  const body = await readJson(req);
  const fingerprint = text(body.device_fingerprint);

  const devices = await listDevices(user.id);
  if (fingerprint && !devices.some((device) => device.fingerprint === fingerprint)) {
    throw new HttpError(403, "该设备尚未激活", "device_not_activated");
  }

  const entitlement = await entitlements(user.id);
  const issued = issueLicenseToken({
    userId: user.id,
    email: user.email,
    entitlement,
    devices: await deviceIds(user.id),
    deviceFingerprint: fingerprint,
  });

  sendJson(res, 200, {
    token: issued.token,
    claims: issued.claims,
    expires_at: issued.expiresAt,
    public_key: publicKeyInfo(),
  });
}

async function handleBillingCancel(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const user = await requireUser(req);
  const body = await readJson(req);
  const subscriptionId = text(body.subscription_id);
  if (!subscriptionId) {
    throw new HttpError(400, "缺少 subscription_id", "invalid_request");
  }

  const subscriptions = await listSubscriptions(user.id);
  const subscription = subscriptions.find((item) => item.id === subscriptionId);
  if (!subscription) {
    throw new HttpError(404, "订阅不存在", "not_found");
  }
  if (!subscription.providerOrderId) {
    throw new HttpError(409, "该订阅缺少支付订单号，请联系支持取消", "missing_order");
  }

  await cancelProviderSubscription(subscription.providerOrderId);
  await markCanceling(user.id, subscription.id);
  sendJson(res, 200, {
    subscription_id: subscription.id,
    status: "canceling",
    message: "已提交取消，服务将保留到当前计费周期结束",
  });
}

async function handleSaveAuthor(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const user = await requireUser(req);
  const body = await readJson(req);
  const displayName = text(body.display_name);
  if (!displayName) {
    throw new HttpError(400, "作者名称不能为空", "invalid_request");
  }

  const existing = await authorProfile(user.id);
  const draft = {
    displayName,
    headline: text(body.headline) ?? existing?.headline ?? "",
    bio: text(body.bio) ?? existing?.bio ?? "",
    payoutAccount: text(body.payout_account) ?? existing?.payoutAccount ?? null,
  };

  const executorAuthorId = await syncExecutorAuthor(
    existing?.executorAuthorId ?? null,
    draft,
  );
  const saved = await saveAuthorProfile(user.id, { ...draft, executorAuthorId });
  sendJson(res, 200, saved);
}

async function handleInternalLicense(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  requireInternal(req);
  const body = await readJson(req, 256 * 1024);
  const ref = text(body.ref);
  if (!ref) {
    throw new HttpError(400, "缺少 ref", "invalid_request");
  }

  await applyLicenseEvent({
    ref,
    userId: text(body.user_id),
    plan: text(body.plan),
    status: text(body.status) ?? "pending",
    email: text(body.email),
    orderId: text(body.order_id),
    currentPeriodEnd: text(body.current_period_end),
  });
  sendJson(res, 200, { ok: true });
}

async function handleInternalCheckout(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  requireInternal(req);
  const body = await readJson(req);
  const ref = text(body.ref);
  const userId = text(body.user_id);
  const plan = text(body.plan);
  if (!ref || !userId || !plan) {
    throw new HttpError(400, "ref、user_id、plan 均为必填", "invalid_request");
  }
  await createPendingCheckout({ ref, userId, plan });
  sendJson(res, 201, { ok: true });
}

/** 校验调用方带来的账号会话，供支付服务解析付款人。 */
async function handleInternalSession(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  requireInternal(req);
  const token = bearer(req);
  const user = token ? await userFromToken(token) : null;
  if (!user) {
    throw new HttpError(401, "会话无效", "unauthorized");
  }
  sendJson(res, 200, userPayload(user));
}

/** 运营后台：返回全部注册用户及其订阅/设备明细。 */
async function handleAdminUsers(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  requireAdmin(req);
  sendJson(res, 200, await adminOverview());
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

const server = createServer((req, res) => {
  applyCors(req, res);

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url ?? "/", config.publicBaseUrl);
  const pathname = url.pathname;
  const segments = pathname.split("/").filter(Boolean);

  void (async () => {
    try {
      if (req.method === "GET" && pathname === "/healthz") {
        sendJson(res, 200, {
          ok: true,
          providers: listProviders().map((provider) => provider.id),
          licenseKey: publicKeyInfo(),
        });
        return;
      }

      if (req.method === "GET" && pathname === "/api/auth/providers") {
        sendJson(res, 200, { providers: listProviders() });
        return;
      }

      if (req.method === "GET" && pathname === "/api/license/public-key") {
        sendJson(res, 200, publicKeyInfo());
        return;
      }

      // /api/auth/:provider/start
      if (
        req.method === "POST" &&
        segments.length === 4 &&
        segments[0] === "api" &&
        segments[1] === "auth" &&
        segments[3] === "start" &&
        // device 是设备码登录，不是 OAuth 提供方。
        segments[2] !== "device"
      ) {
        const body = await readJson(req);
        const redirectUri = text(body.redirect_uri) ?? `${config.siteUrl}/`;
        if (!isAllowedRedirect(redirectUri)) {
          throw new HttpError(400, "redirect_uri 不在允许范围内", "invalid_redirect");
        }
        sendJson(res, 200, startFlow(segments[2]!, redirectUri));
        return;
      }

      // /api/auth/:provider/callback
      if (
        req.method === "GET" &&
        segments.length === 4 &&
        segments[0] === "api" &&
        segments[1] === "auth" &&
        segments[3] === "callback"
      ) {
        await handleAuthCallback(res, segments[2]!, url);
        return;
      }

      if (req.method === "POST" && pathname === "/api/auth/device/start") {
        await handleDeviceStart(req, res);
        return;
      }
      if (req.method === "POST" && pathname === "/api/auth/device/poll") {
        await handleDevicePoll(req, res);
        return;
      }
      if (req.method === "POST" && pathname === "/api/auth/device/approve") {
        await handleDeviceApprove(req, res);
        return;
      }
      if (req.method === "POST" && pathname === "/api/auth/device/deny") {
        await handleDeviceDeny(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/auth/logout") {
        const token = bearer(req);
        if (token) {
          await revokeSession(token);
        }
        sendJson(res, 200, { ok: true });
        return;
      }

      if (req.method === "GET" && pathname === "/api/me") {
        const user = await requireUser(req);
        const [entitlement, devices] = await Promise.all([
          entitlements(user.id),
          listDevices(user.id),
        ]);
        sendJson(res, 200, {
          user: userPayload(user),
          entitlement: {
            plans: entitlement.plans,
            status: entitlement.status,
            expiresAt: entitlement.expiresAt,
            deviceLimit: entitlement.deviceLimit,
          },
          devices,
        });
        return;
      }

      if (req.method === "GET" && pathname === "/api/me/subscriptions") {
        const user = await requireUser(req);
        sendJson(res, 200, { subscriptions: await listSubscriptions(user.id) });
        return;
      }

      if (req.method === "POST" && pathname === "/api/me/license-token") {
        await handleLicenseToken(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/me/billing/cancel") {
        await handleBillingCancel(req, res);
        return;
      }

      if (pathname === "/api/me/devices" && req.method === "GET") {
        const user = await requireUser(req);
        sendJson(res, 200, { devices: await listDevices(user.id) });
        return;
      }

      if (pathname === "/api/me/devices" && req.method === "POST") {
        const user = await requireUser(req);
        const body = await readJson(req);
        const fingerprint = text(body.fingerprint);
        if (!fingerprint) {
          throw new HttpError(400, "缺少设备指纹", "invalid_request");
        }
        const device = await activateDevice(user.id, {
          fingerprint,
          name: text(body.name) ?? "ArbPilot Desktop",
          platform: text(body.platform) ?? "",
        });
        sendJson(res, 200, { device });
        return;
      }

      if (
        req.method === "DELETE" &&
        segments.length === 4 &&
        segments[0] === "api" &&
        segments[1] === "me" &&
        segments[2] === "devices"
      ) {
        const user = await requireUser(req);
        await revokeDevice(user.id, segments[3]!);
        sendJson(res, 200, { ok: true });
        return;
      }

      if (req.method === "GET" && pathname === "/api/me/author") {
        const user = await requireUser(req);
        sendJson(res, 200, { author: await authorProfile(user.id) });
        return;
      }

      if (req.method === "PUT" && pathname === "/api/me/author") {
        await handleSaveAuthor(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/internal/licenses") {
        await handleInternalLicense(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/internal/checkout") {
        await handleInternalCheckout(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/internal/session") {
        await handleInternalSession(req, res);
        return;
      }

      if (req.method === "GET" && pathname === "/api/admin/users") {
        await handleAdminUsers(req, res);
        return;
      }

      sendJson(res, 404, { error: "not_found" });
    } catch (error) {
      if (error instanceof HttpError) {
        if (!res.headersSent) {
          sendJson(res, error.status, {
            error: error.code ?? "request_failed",
            message: error.message,
          });
        }
        return;
      }
      console.error("[accounts] 请求处理失败：", error);
      if (!res.headersSent) {
        sendJson(res, 500, {
          error: "internal_error",
          message: error instanceof Error ? error.message : "未知错误",
        });
      }
    }
  })();
});

async function main(): Promise<void> {
  await migrate();
  server.listen(config.port, () => {
    console.log(
      `[accounts] 账号服务已启动 http://127.0.0.1:${config.port}（登录方式：${
        config.providers.map((provider) => provider.id).join(", ") || "未配置"
      }）`,
    );
    if (config.providers.length === 0) {
      console.log(
        "[accounts] 提示：配置 OAUTH_GITHUB_CLIENT_ID/SECRET 或 OAUTH_GOOGLE_CLIENT_ID/SECRET 后才能登录。",
      );
    }
  });
}

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    server.close(() => {
      void closePool().finally(() => process.exit(0));
    });
  });
}

void main().catch(async (error) => {
  console.error("[accounts] 启动失败：", error);
  await closePool().catch(() => undefined);
  process.exit(1);
});
