import { randomUUID, timingSafeEqual } from "node:crypto";
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";

import {
  WebhookEventType,
  WaffoPancake,
  verifyWebhook,
  type CashierLanguage,
} from "@waffo/pancake-ts";

import { config, findPlan, plans } from "./config";
import { notifyCheckout, notifyLicense, resolveUser } from "./accounts";
import {
  ExecutorError,
  createSettlement,
  fetchPublication,
  listAuthors,
  updateSubscription,
  type SubscriptionPatch,
} from "./executor";
import {
  createPendingLicense,
  getLicense,
  isProcessedEvent,
  markProcessedEvent,
  upsertLicense,
  type LicenseStatus,
} from "./licenses";
import { ensureProducts } from "./products";
import {
  ensureStrategyProduct,
  getStrategyCheckout,
  saveStrategyCheckout,
  type StrategyCheckoutStatus,
} from "./strategy";

function createClient(): WaffoPancake | null {
  if (!config.merchantId || !config.privateKey) {
    console.warn(
      "[payments] 缺少 WAFFO_PRIVATE_KEY，收银台接口将返回 503；/api/plans 仍可用。",
    );
    return null;
  }
  try {
    return new WaffoPancake({
      merchantId: config.merchantId,
      privateKey: config.privateKey,
      environment: config.environment,
    });
  } catch (error) {
    console.error("[payments] Pancake 客户端初始化失败：", error);
    return null;
  }
}

const client = createClient();

function applyCors(req: IncomingMessage, res: ServerResponse): void {
  const origin = req.headers.origin;
  if (origin) {
    const allowed = config.origins.includes("*") || config.origins.includes(origin);
    if (allowed) {
      res.setHeader("Access-Control-Allow-Origin", origin);
      res.setHeader("Vary", "Origin");
    }
  }
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Authorization, Content-Type");
  res.setHeader("Access-Control-Max-Age", "600");
}

function sendJson(res: ServerResponse, status: number, payload: unknown): void {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  res.end(body);
}

function sendText(res: ServerResponse, status: number, body: string): void {
  res.writeHead(status, { "Content-Type": "text/plain; charset=utf-8" });
  res.end(body);
}

async function readRaw(req: IncomingMessage, limit: number): Promise<string> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += buffer.length;
    if (size > limit) {
      throw new Error("请求体过大");
    }
    chunks.push(buffer);
  }
  return Buffer.concat(chunks).toString("utf8");
}

interface CheckoutBody {
  plan?: unknown;
  email?: unknown;
  locale?: unknown;
  darkMode?: unknown;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function localeOf(value: unknown): CashierLanguage | undefined {
  return value === "zh" ? "zh-Hans" : value === "en" ? "en" : undefined;
}

async function handleCheckout(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const raw = await readRaw(req, 64 * 1024);
  const body = raw ? (JSON.parse(raw) as CheckoutBody) : {};
  const authorization = req.headers.authorization;
  const user = await resolveUser(Array.isArray(authorization) ? authorization[0] : authorization);

  // 订阅必须绑定到账号：授权、设备数与跨设备恢复都以账号为准。
  if (!user) {
    sendJson(res, 401, {
      error: "login_required",
      message: "请先登录 ArbPilot 账号后再订阅",
    });
    return;
  }

  const plan = findPlan(typeof body.plan === "string" ? body.plan : "");
  if (!plan) {
    sendJson(res, 400, { error: "unknown_plan", message: "plan 必须是 arbitrage 或 strategy" });
    return;
  }
  // 策略创建是附加包，必须先有 $19/月 的自托管部署基础订阅。
  if (
    plan.addon &&
    plan.requires &&
    !(user.entitlement?.plans ?? []).includes(plan.requires)
  ) {
    sendJson(res, 409, {
      error: "base_plan_required",
      message: "请先订阅自托管部署，再购买策略创建附加包",
    });
    return;
  }
  if (!client) {
    sendJson(res, 503, {
      error: "payments_not_configured",
      message: "服务端未配置 WAFFO_PRIVATE_KEY，暂时无法创建收银台会话",
    });
    return;
  }

  const email =
    user.email ??
    (typeof body.email === "string" && body.email.includes("@") ? body.email.trim() : undefined);
  const language = localeOf(body.locale);
  const darkMode = typeof body.darkMode === "boolean" ? body.darkMode : undefined;

  const ref = randomUUID();
  const products = await ensureProducts(client);

  const session = await client.checkout.authenticated.create({
    productId: products[plan.id],
    currency: config.currency,
    // 用账号 ID 作为 buyerIdentity，订单始终绑定到同一用户，换邮箱也无法重置试用。
    buyerIdentity: user.id,
    buyerEmail: email,
    successUrl: `${config.siteUrl}/checkout/success?ref=${ref}`,
    metadata: {
      arbpilot_ref: ref,
      plan: plan.id,
      user_id: user.id,
    },
    orderMerchantExternalId: ref,
    language,
    darkMode,
  });

  createPendingLicense(ref, plan.id, email ?? null);
  await notifyCheckout({ ref, userId: user.id, plan: plan.id });

  sendJson(res, 201, { ref, checkoutUrl: session.checkoutUrl, expiresAt: session.expiresAt });
}

interface StrategyCheckoutBody {
  publication_id?: unknown;
  subscriber_id?: unknown;
  subscription_id?: unknown;
  email?: unknown;
  locale?: unknown;
  darkMode?: unknown;
}

/** 按策略发布定价创建收银台会话；订阅本身已在执行器创建为待支付。 */
async function handleStrategyCheckout(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  const raw = await readRaw(req, 64 * 1024);
  const body = raw ? (JSON.parse(raw) as StrategyCheckoutBody) : {};
  const authorization = req.headers.authorization;
  const user = await resolveUser(Array.isArray(authorization) ? authorization[0] : authorization);

  const publicationId = text(body.publication_id);
  const subscriptionId = text(body.subscription_id);

  // 策略订阅同样绑定账号，未登录不允许创建收银台。
  if (!user) {
    sendJson(res, 401, {
      error: "login_required",
      message: "请先登录 ArbPilot 账号后再订阅策略",
    });
    return;
  }

  // 订阅人必须是当前账号，避免替他人付款或买到别人名下。
  const subscriberId = text(body.subscriber_id) ?? user.id;
  if (subscriberId !== user.id) {
    sendJson(res, 409, {
      error: "subscriber_mismatch",
      message: "订阅人需要与当前登录账号一致，请在桌面端重新发起订阅",
    });
    return;
  }

  if (!publicationId || !subscriptionId) {
    sendJson(res, 400, {
      error: "invalid_request",
      message: "publication_id、subscription_id 均为必填",
    });
    return;
  }
  if (!client) {
    sendJson(res, 503, {
      error: "payments_not_configured",
      message: "服务端未配置 WAFFO_PRIVATE_KEY，暂时无法创建收银台会话",
    });
    return;
  }

  let publication;
  try {
    publication = await fetchPublication(publicationId);
  } catch (error) {
    const status = error instanceof ExecutorError && error.status === 404 ? 404 : 502;
    sendJson(res, status, {
      error: "publication_unavailable",
      message: error instanceof Error ? error.message : "无法读取策略发布信息",
    });
    return;
  }

  if (publication.status !== "published") {
    sendJson(res, 409, { error: "not_published", message: "该策略当前未发布，无法订阅" });
    return;
  }
  const amount = Number(publication.pricing.monthly_price_usd);
  if (publication.pricing.model !== "monthly" || !Number.isFinite(amount) || amount <= 0) {
    sendJson(res, 400, { error: "not_billable", message: "该策略没有可支付的月费定价" });
    return;
  }

  const productId = await ensureStrategyProduct(client, publication);
  const ref = randomUUID();
  const email =
    user.email ??
    (typeof body.email === "string" && body.email.includes("@") ? body.email.trim() : undefined);
  const session = await client.checkout.authenticated.create({
    productId,
    currency: config.currency,
    buyerIdentity: user.id,
    buyerEmail: email,
    successUrl: `${config.siteUrl}/checkout/success?ref=${ref}`,
    metadata: {
      arbpilot_ref: ref,
      kind: "strategy",
      publication_id: publication.id,
      subscription_id: subscriptionId,
      subscriber_id: subscriberId,
      author_id: publication.author_id,
      user_id: user.id,
    },
    orderMerchantExternalId: ref,
    language: localeOf(body.locale),
    darkMode: typeof body.darkMode === "boolean" ? body.darkMode : undefined,
  });

  saveStrategyCheckout({
    ref,
    publicationId: publication.id,
    subscriptionId,
    subscriberId,
    authorId: publication.author_id,
    title: publication.title,
    amount: amount.toFixed(2),
    status: "pending",
    orderId: null,
    currentPeriodEnd: null,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
  });

  sendJson(res, 201, {
    ref,
    checkoutUrl: session.checkoutUrl,
    expiresAt: session.expiresAt,
    amount: amount.toFixed(2),
  });
}

interface PancakeWebhookData {
  orderId?: string;
  buyerEmail?: string;
  orderMetadata?: Record<string, string>;
  orderMerchantExternalId?: string;
  productName?: string;
  currentPeriodEnd?: string;
}

function statusFromEvent(eventType: string): LicenseStatus | null {
  switch (eventType) {
    case WebhookEventType.OrderCompleted:
    case WebhookEventType.SubscriptionActivated:
    case WebhookEventType.SubscriptionRenewed:
    case WebhookEventType.SubscriptionRecovered:
    case WebhookEventType.SubscriptionPlanChanged:
    case WebhookEventType.SubscriptionUncanceled:
      return "active";
    case WebhookEventType.SubscriptionCanceling:
      return "canceling";
    case WebhookEventType.SubscriptionCanceled:
      return "canceled";
    case WebhookEventType.SubscriptionPastDue:
      return "past_due";
    default:
      return null;
  }
}

interface StrategyStatusMapping {
  local: StrategyCheckoutStatus;
  executor: SubscriptionPatch["status"];
}

function strategyStatus(status: LicenseStatus): StrategyStatusMapping | null {
  switch (status) {
    case "active":
      return { local: "active", executor: "active" };
    // 取消后仍可用到当前计费周期结束。
    case "canceling":
      return { local: "cancelled", executor: "cancelled" };
    case "canceled":
      return { local: "expired", executor: "expired" };
    // 逾期即失去授权，补缴后由 active 事件恢复。
    case "past_due":
      return { local: "past_due", executor: "pending_payment" };
    default:
      return null;
  }
}

/** 将支付事件回写为执行器的订阅状态；失败会让支付平台重投。 */
async function syncStrategySubscription(
  eventType: string,
  data: PancakeWebhookData,
  ref: string,
  status: LicenseStatus,
): Promise<void> {
  const subscriptionId = text(data.orderMetadata?.subscription_id);
  const mapped = strategyStatus(status);
  if (!subscriptionId || !mapped) {
    console.warn(`[payments] 策略事件 ${eventType} 缺少订阅引用，已跳过（ref=${ref}）`);
    return;
  }

  const patch: SubscriptionPatch = { status: mapped.executor, payment_ref: data.orderId ?? ref };
  if (data.currentPeriodEnd) {
    patch.current_period_end = data.currentPeriodEnd;
  } else if (mapped.executor === "active") {
    // 月订阅事件未带周期时按 30 天续期，避免激活后立即过期。
    patch.current_period_end = new Date(Date.now() + 30 * 86_400_000).toISOString();
  }
  await updateSubscription(subscriptionId, patch);

  const local = getStrategyCheckout(ref);
  if (local) {
    saveStrategyCheckout({
      ...local,
      status: mapped.local,
      orderId: data.orderId ?? local.orderId,
      currentPeriodEnd: data.currentPeriodEnd ?? local.currentPeriodEnd,
    });
  }
  console.log(`[payments] ${eventType} → 策略订阅 ${subscriptionId} (${mapped.executor})`);
}

async function handleWebhook(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const raw = await readRaw(req, 1024 * 1024);
  const header = req.headers["x-waffo-signature"];
  const signature = Array.isArray(header) ? header[0] : header;

  let event;
  try {
    event = verifyWebhook(raw, signature, { environment: config.environment });
  } catch {
    sendText(res, 401, "Invalid signature");
    return;
  }

  if (isProcessedEvent(event.id)) {
    sendText(res, 200, "OK");
    return;
  }

  const data = (event.data ?? {}) as PancakeWebhookData;
  const ref = data.orderMetadata?.arbpilot_ref ?? data.orderMerchantExternalId;
  const status = statusFromEvent(event.eventType);

  if (ref && status && data.orderMetadata?.kind === "strategy") {
    await syncStrategySubscription(event.eventType, data, ref, status);
    markProcessedEvent(event.id);
    sendText(res, 200, "OK");
    return;
  }

  if (ref && status) {
    const planId = data.orderMetadata?.plan;
    const patch: Parameters<typeof upsertLicense>[1] = { status };
    if (planId === "arbitrage" || planId === "strategy") {
      patch.plan = planId;
    }
    if (data.buyerEmail) {
      patch.email = data.buyerEmail;
    }
    if (data.orderId) {
      patch.orderId = data.orderId;
    }
    if (data.productName) {
      patch.productName = data.productName;
    }
    if (data.currentPeriodEnd) {
      patch.expiresAt = data.currentPeriodEnd;
    }
    upsertLicense(ref, patch);
    await notifyLicense({
      ref,
      userId: data.orderMetadata?.user_id ?? null,
      plan: planId === "arbitrage" || planId === "strategy" ? planId : null,
      status,
      email: data.buyerEmail ?? null,
      orderId: data.orderId ?? null,
      currentPeriodEnd: data.currentPeriodEnd ?? null,
    });
    console.log(`[payments] ${event.eventType} → ${ref} (${status})`);
  } else {
    console.log(`[payments] 忽略事件 ${event.eventType}（ref=${ref ?? "无"}）`);
  }

  markProcessedEvent(event.id);
  sendText(res, 200, "OK");
}

interface SettlementRunBody {
  period?: unknown;
}

/**
 * 为所有作者生成指定月份的结算单。执行器按平台服务费 bps 扣费并做幂等，
 * 可由外部 cron 在每月初调用，重复调用不会重复结算。
 */
async function handleSettlementRun(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const raw = await readRaw(req, 64 * 1024);
  const body = raw ? (JSON.parse(raw) as SettlementRunBody) : {};
  const period = text(body.period) ?? undefined;
  if (period && !/^\d{4}-(0[1-9]|1[0-2])$/.test(period)) {
    sendJson(res, 400, { error: "invalid_period", message: "period 必须为 YYYY-MM" });
    return;
  }

  let authors;
  try {
    authors = await listAuthors();
  } catch (error) {
    sendJson(res, 502, {
      error: "executor_unavailable",
      message: error instanceof Error ? error.message : "无法读取作者列表",
    });
    return;
  }

  const results = [];
  for (const author of authors) {
    try {
      const settlement = await createSettlement(author.author_id, period);
      results.push({ ...author, status: "created", settlement });
    } catch (error) {
      // 没有付费订阅的作者会被执行器拒绝，属于正常的跳过。
      results.push({
        ...author,
        status: "skipped",
        message: error instanceof Error ? error.message : "结算失败",
      });
    }
  }

  sendJson(res, 200, { period: period ?? null, results });
}

function requireInternal(req: IncomingMessage, res: ServerResponse): boolean {
  const header = req.headers["x-internal-token"];
  const value = Array.isArray(header) ? header[0] : header;
  const expected = Buffer.from(config.accountsInternalToken);
  const actual = Buffer.from(value ?? "");
  if (expected.length !== actual.length || !timingSafeEqual(expected, actual)) {
    sendJson(res, 401, { error: "forbidden", message: "内部接口需要有效令牌" });
    return false;
  }
  return true;
}

interface CancelBody {
  order_id?: unknown;
}

/** 账号服务代用户取消订阅；Pancake 返回 canceling 后由 webhook 落最终状态。 */
async function handleCancelSubscription(
  req: IncomingMessage,
  res: ServerResponse,
): Promise<void> {
  if (!requireInternal(req, res)) {
    return;
  }
  const raw = await readRaw(req, 32 * 1024);
  const body = raw ? (JSON.parse(raw) as CancelBody) : {};
  const orderId = text(body.order_id);
  if (!orderId) {
    sendJson(res, 400, { error: "invalid_request", message: "order_id 为必填" });
    return;
  }
  if (!client) {
    sendJson(res, 503, {
      error: "payments_not_configured",
      message: "服务端未配置 WAFFO_PRIVATE_KEY，无法取消订阅",
    });
    return;
  }

  try {
    const result = await client.orders.cancelSubscription({ orderId });
    console.log(`[payments] 取消订阅 ${orderId} → ${result.status}`);
    sendJson(res, 200, result);
  } catch (error) {
    sendJson(res, 502, {
      error: "cancel_failed",
      message: error instanceof Error ? error.message : "取消订阅失败",
    });
  }
}

const server = createServer((req, res) => {
  applyCors(req, res);

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const url = new URL(req.url ?? "/", config.publicBaseUrl);
  const { pathname } = url;

  void (async () => {
    try {
      if (req.method === "GET" && pathname === "/healthz") {
        sendJson(res, 200, {
          ok: true,
          configured: Boolean(client),
          environment: config.environment,
          storeId: config.storeId,
          currency: config.currency,
        });
        return;
      }

      if (req.method === "GET" && pathname === "/api/plans") {
        sendJson(res, 200, {
          currency: config.currency,
          plans: plans.map((plan) => ({
            id: plan.id,
            amount: plan.amount,
            billingPeriod: plan.billingPeriod,
            name: plan.name,
            requires: plan.requires,
            addon: plan.addon,
          })),
        });
        return;
      }

      if (req.method === "POST" && pathname === "/api/checkout") {
        await handleCheckout(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/strategy-checkout") {
        await handleStrategyCheckout(req, res);
        return;
      }

      if (req.method === "GET" && pathname.startsWith("/api/strategy-checkouts/")) {
        const ref = decodeURIComponent(pathname.slice("/api/strategy-checkouts/".length));
        const checkout = getStrategyCheckout(ref);
        if (!checkout) {
          sendJson(res, 404, { ref, status: "unknown" });
          return;
        }
        sendJson(res, 200, checkout);
        return;
      }

      if (req.method === "POST" && pathname === "/api/market/settlements/run") {
        await handleSettlementRun(req, res);
        return;
      }

      if (req.method === "POST" && pathname === "/api/subscriptions/cancel") {
        await handleCancelSubscription(req, res);
        return;
      }

      if (req.method === "GET" && pathname.startsWith("/api/licenses/")) {
        const ref = decodeURIComponent(pathname.slice("/api/licenses/".length));
        const license = getLicense(ref);
        if (!license) {
          sendJson(res, 404, { ref, status: "unknown" });
          return;
        }
        sendJson(res, 200, license);
        return;
      }

      if (req.method === "POST" && pathname === "/api/webhooks/pancake") {
        await handleWebhook(req, res);
        return;
      }

      sendJson(res, 404, { error: "not_found" });
    } catch (error) {
      const message = error instanceof Error ? error.message : "未知错误";
      console.error("[payments] 请求处理失败：", error);
      if (!res.headersSent) {
        sendJson(res, 500, { error: "internal_error", message });
      }
    }
  })();
});

server.listen(config.port, () => {
  console.log(
    `[payments] Pancake 收款服务已启动 http://127.0.0.1:${config.port}（${config.environment}）`,
  );
  if (!client) {
    console.log("[payments] 提示：填入 WAFFO_PRIVATE_KEY 后才能创建收银台会话。");
  }
});
