import { createHmac, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

import { config } from "./config";
import type { StrategyPublicationView } from "./executor";
import { updateSubscription } from "./executor";

/**
 * 策略的加密支付通道（一次性买断）。
 *
 * 买家一次付清买断价，USDT-TRC20 或 USDC；平台账户收款后按抽成比例
 * 把作者分成转到作者填写的地址。付款确认后订阅永久有效，不涉及续费。
 * 策略代码与交易所密钥不经过这里。
 */

export type CryptoCurrency = "usdttrc20" | "usdc";
export type CryptoCheckoutStatus = "pending" | "paid" | "failed" | "expired" | "refunded";

export interface AuthorWallet {
  accountId: string;
  currency: CryptoCurrency;
  address: string;
  updatedAt: string;
}

export interface CryptoCheckout {
  ref: string;
  invoiceId: string;
  invoiceUrl: string;
  publicationId: string;
  subscriptionId: string;
  subscriberId: string;
  authorId: string;
  title: string;
  amountUsd: string;
  payCurrency: CryptoCurrency;
  status: CryptoCheckoutStatus;
  createdAt: string;
  updatedAt: string;
}

export interface LedgerEntry {
  ref: string;
  publicationId: string;
  authorId: string;
  grossUsd: string;
  commissionRate: number;
  commissionUsd: string;
  authorShareUsd: string;
  currency: CryptoCurrency;
  address: string | null;
  payoutStatus: "pending" | "sent" | "skipped" | "failed";
  payoutId: string | null;
  createdAt: string;
  updatedAt: string;
}

function loadJson<T>(file: string, key: string, fallback: T): T {
  if (!existsSync(file)) {
    return fallback;
  }
  try {
    const parsed = JSON.parse(readFileSync(file, "utf8")) as Record<string, unknown>;
    return (parsed[key] as T) ?? fallback;
  } catch {
    return fallback;
  }
}

function saveJson(file: string, key: string, value: unknown): void {
  mkdirSync(config.dataDir, { recursive: true });
  writeFileSync(file, JSON.stringify({ [key]: value }, null, 2));
}

const walletFile = resolve(config.dataDir, "author-wallets.json");
const checkoutFile = resolve(config.dataDir, "crypto-checkouts.json");
const ledgerFile = resolve(config.dataDir, "crypto-ledger.json");

const wallets = loadJson<Record<string, AuthorWallet>>(walletFile, "wallets", {});
const checkouts = loadJson<Record<string, CryptoCheckout>>(checkoutFile, "checkouts", {});
const ledger = loadJson<Record<string, LedgerEntry>>(ledgerFile, "entries", {});

const ADDRESS_PATTERNS: Record<CryptoCurrency, RegExp> = {
  usdttrc20: /^T[1-9A-HJ-NP-Za-km-z]{33}$/,
  usdc: /^0x[0-9a-fA-F]{40}$/,
};

export function isCryptoCurrency(value: unknown): value is CryptoCurrency {
  return value === "usdttrc20" || value === "usdc";
}

export function normalizeAddress(currency: CryptoCurrency, address: string): string | null {
  const trimmed = address.trim();
  return ADDRESS_PATTERNS[currency].test(trimmed) ? trimmed : null;
}

export function getWallet(accountId: string): AuthorWallet | null {
  return wallets[accountId] ?? null;
}

export function saveWallet(wallet: AuthorWallet): AuthorWallet {
  wallets[wallet.accountId] = wallet;
  saveJson(walletFile, "wallets", wallets);
  return wallet;
}

export function getCryptoCheckout(ref: string): CryptoCheckout | null {
  return checkouts[ref] ?? null;
}

export function saveCryptoCheckout(checkout: CryptoCheckout): CryptoCheckout {
  const next = { ...checkout, updatedAt: new Date().toISOString() };
  checkouts[next.ref] = next;
  saveJson(checkoutFile, "checkouts", checkouts);
  return next;
}

/** 冷启动期 6 个月内 10%，之后 20%；未配置结束时间时按冷启动处理。 */
export function commissionRate(at = new Date()): number {
  const until = config.commissionColdStartUntil
    ? Date.parse(config.commissionColdStartUntil)
    : Number.NaN;
  if (Number.isFinite(until)) {
    return at.getTime() < until ? 0.1 : 0.2;
  }
  return 0.1;
}

function amountOf(publication: StrategyPublicationView): number {
  const price = Number(publication.pricing.monthly_price_usd);
  if (!Number.isFinite(price) || price <= 0) {
    throw new Error("该策略没有有效的买断价");
  }
  return Math.round(price * 100) / 100;
}

async function nowpayments<T>(path: string, init: RequestInit & { token?: string }): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    "x-api-key": config.nowpaymentsApiKey,
  };
  if (init.token) {
    headers.Authorization = `Bearer ${init.token}`;
  }
  let response: Response;
  try {
    response = await fetch(`${config.nowpaymentsApiUrl}${path}`, { ...init, headers });
  } catch {
    throw new Error("NOWPayments unreachable");
  }
  const text = await response.text();
  const payload = text ? (JSON.parse(text) as Record<string, unknown>) : {};
  if (!response.ok) {
    const message =
      typeof payload.message === "string" ? payload.message : `nowpayments: ${response.status}`;
    throw new Error(message);
  }
  return payload as T;
}

/** 创建固定汇率发票；买家按发票地址付款，确认后由 IPN 回写订阅。 */
export async function createInvoice(params: {
  publication: StrategyPublicationView;
  subscriptionId: string;
  subscriberId: string;
  payCurrency: CryptoCurrency;
}): Promise<{ ref: string; amountUsd: number; invoiceId: string; invoiceUrl: string }> {
  const amount = amountOf(params.publication);
  const ref = randomUUID();
  const invoice = await nowpayments<{ id: string; invoice_url: string }>("/invoice", {
    method: "POST",
    body: JSON.stringify({
      price_amount: amount,
      price_currency: "usd",
      pay_currency: params.payCurrency,
      order_id: ref,
      order_description: `ArbPilot strategy · ${params.publication.title} · lifetime`.slice(0, 250),
      ipn_callback_url: `${config.publicBaseUrl}/api/crypto/ipn`,
      success_url: config.siteUrl,
      cancel_url: config.siteUrl,
      is_fixed_rate: true,
      is_fee_paid_by_user: false,
    }),
  });

  if (!invoice.id || !invoice.invoice_url) {
    throw new Error("NOWPayments 未返回发票地址");
  }
  return { ref, amountUsd: amount, invoiceId: invoice.id, invoiceUrl: invoice.invoice_url };
}

/** IPN 签名：对 JSON 键递归排序后取 HMAC-SHA512。 */
export function verifyIpn(raw: string, signature: string | undefined): boolean {
  if (!config.nowpaymentsIpnSecret || !signature) {
    return false;
  }
  try {
    const expected = createHmac("sha512", config.nowpaymentsIpnSecret)
      .update(JSON.stringify(sortKeys(JSON.parse(raw))))
      .digest("hex");
    return expected === signature;
  } catch {
    return false;
  }
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(sortKeys);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value as Record<string, unknown>)
        .sort()
        .map((key) => [key, sortKeys((value as Record<string, unknown>)[key])]),
    );
  }
  return value;
}

function recordLedger(checkout: CryptoCheckout): LedgerEntry {
  const existing = ledger[checkout.ref];
  if (existing) {
    return existing;
  }
  const rate = commissionRate();
  const gross = Number(checkout.amountUsd);
  const commission = Math.round(gross * rate * 100) / 100;
  const share = Math.round((gross - commission) * 100) / 100;
  const wallet = getWallet(checkout.authorId);
  const entry: LedgerEntry = {
    ref: checkout.ref,
    publicationId: checkout.publicationId,
    authorId: checkout.authorId,
    grossUsd: gross.toFixed(2),
    commissionRate: rate,
    commissionUsd: commission.toFixed(2),
    authorShareUsd: share.toFixed(2),
    currency: wallet?.currency ?? checkout.payCurrency,
    address: wallet?.address ?? null,
    payoutStatus: wallet ? "pending" : "skipped",
    payoutId: null,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
  };
  ledger[entry.ref] = entry;
  saveJson(ledgerFile, "entries", ledger);
  return entry;
}

/** 配置了出款账号时自动把作者分成打到其地址；否则留在 pending 等人工出款。 */
async function payoutAuthor(entry: LedgerEntry): Promise<void> {
  if (!entry.address || !config.nowpaymentsPayoutEmail || !config.nowpaymentsPayoutPassword) {
    return;
  }
  try {
    const auth = await nowpayments<{ token: string }>("/auth", {
      method: "POST",
      body: JSON.stringify({
        email: config.nowpaymentsPayoutEmail,
        password: config.nowpaymentsPayoutPassword,
      }),
    });
    const result = await nowpayments<{ id?: string }>("/payout", {
      method: "POST",
      token: auth.token,
      body: JSON.stringify({
        withdrawals: [
          {
            address: entry.address,
            currency: entry.currency,
            amount: Number(entry.authorShareUsd),
          },
        ],
        ipn_callback_url: `${config.publicBaseUrl}/api/crypto/ipn`,
      }),
    });
    entry.payoutStatus = "sent";
    entry.payoutId = result.id ?? null;
  } catch (error) {
    console.error("[payments] 作者分成出款失败：", error);
    entry.payoutStatus = "failed";
  }
  entry.updatedAt = new Date().toISOString();
  ledger[entry.ref] = entry;
  saveJson(ledgerFile, "entries", ledger);
}

/** 处理 IPN：确认收款后开通永久授权并记一笔作者应收。 */
export async function handleIpnPayment(payload: Record<string, unknown>): Promise<void> {
  const ref = typeof payload.order_id === "string" ? payload.order_id : "";
  const status = typeof payload.payment_status === "string" ? payload.payment_status : "";
  const checkout = ref ? checkouts[ref] : null;
  if (!checkout) {
    return;
  }

  if (status === "finished" || status === "confirmed") {
    if (checkout.status !== "paid") {
      // 买断不设到期时间：状态置为 active 即永久有效。
      await updateSubscription(checkout.subscriptionId, {
        status: "active",
        payment_ref: checkout.ref,
      });
      saveCryptoCheckout({ ...checkout, status: "paid" });
      const entry = recordLedger(checkout);
      await payoutAuthor(entry);
    }
    return;
  }

  if (status === "failed" || status === "expired") {
    saveCryptoCheckout({ ...checkout, status: status });
    return;
  }

  if (status === "refunded") {
    await updateSubscription(checkout.subscriptionId, { status: "cancelled" });
    saveCryptoCheckout({ ...checkout, status: "refunded" });
  }

  // waiting / confirming / partially_paid 保持 pending，等下一次回调。
}
