import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  TaxCategory,
  type WaffoPancake,
} from "@waffo/pancake-ts";

import { config } from "./config";
import type { StrategyPublicationView } from "./executor";
import { publishProduct } from "./products";

/**
 * 策略买断的商品与订单引用。
 *
 * 每个发布只创建一个一次性商品；买家付款后永久获得该版本授权，不存在续费。
 * 这里只保存业务标识、金额与支付订单号；策略代码、交易所密钥与交易权限
 * 始终留在执行器一侧，支付服务从不接触。
 */

export type StrategyCheckoutStatus = "pending" | "active" | "cancelled" | "expired" | "past_due";

export interface StrategyCheckout {
  ref: string;
  publicationId: string;
  subscriptionId: string;
  subscriberId: string;
  authorId: string;
  title: string;
  amount: string;
  status: StrategyCheckoutStatus;
  orderId: string | null;
  currentPeriodEnd: string | null;
  createdAt: string;
  updatedAt: string;
}

interface CheckoutStoreShape {
  checkouts: Record<string, StrategyCheckout>;
}

const checkoutFile = resolve(config.dataDir, "strategy-checkouts.json");
const productFile = resolve(config.dataDir, "strategy-products.json");

function loadCheckouts(): CheckoutStoreShape {
  if (!existsSync(checkoutFile)) {
    return { checkouts: {} };
  }
  try {
    const parsed = JSON.parse(readFileSync(checkoutFile, "utf8")) as Partial<CheckoutStoreShape>;
    return { checkouts: parsed.checkouts ?? {} };
  } catch {
    return { checkouts: {} };
  }
}

const checkouts = loadCheckouts();

function persistCheckouts(): void {
  mkdirSync(config.dataDir, { recursive: true });
  writeFileSync(checkoutFile, JSON.stringify(checkouts, null, 2));
}

export function getStrategyCheckout(ref: string): StrategyCheckout | null {
  return checkouts.checkouts[ref] ?? null;
}

export function saveStrategyCheckout(checkout: StrategyCheckout): StrategyCheckout {
  const next = { ...checkout, updatedAt: new Date().toISOString() };
  checkouts.checkouts[next.ref] = next;
  persistCheckouts();
  return next;
}

/** 发布定价可能被作者修改，缓存键带上金额以生成新的一次性商品。 */
type ProductCache = Record<string, string>;

function readProductCache(): ProductCache {
  if (!existsSync(productFile)) {
    return {};
  }
  try {
    return JSON.parse(readFileSync(productFile, "utf8")) as ProductCache;
  } catch {
    return {};
  }
}

function writeProductCache(cache: ProductCache): void {
  mkdirSync(config.dataDir, { recursive: true });
  writeFileSync(productFile, JSON.stringify(cache, null, 2));
}

const creating = new Map<string, Promise<string>>();

function parseAmount(publication: StrategyPublicationView): string {
  const amount = Number(publication.pricing.monthly_price_usd);
  if (!Number.isFinite(amount) || amount <= 0) {
    throw new Error("该策略没有有效的买断价");
  }
  return amount.toFixed(2);
}

/**
 * 每个「发布 + 金额」对应一个店铺一次性商品。
 * 正式环境可用 WAFFO_STORE_ID 指向同一店铺，并把缓存文件持久化。
 */
export function ensureStrategyProduct(
  client: WaffoPancake,
  publication: StrategyPublicationView,
): Promise<string> {
  const amount = parseAmount(publication);
  const key = `${publication.id}:${amount}`;
  const cached = readProductCache()[key];
  if (cached) {
    return Promise.resolve(cached);
  }

  const inFlight = creating.get(key);
  if (inFlight) {
    return inFlight;
  }

  const task = (async () => {
    if (!config.storeId) {
      throw new Error("缺少 WAFFO_STORE_ID，无法创建策略买断商品");
    }
    const { product } = await client.onetimeProducts.create({
      storeId: config.storeId,
      name: `ArbPilot Strategy · ${publication.title}`.slice(0, 120),
      description: "一次性买断，永久使用该发布版本",
      prices: {
        [config.currency]: { amount, taxCategory: TaxCategory.DigitalGoods },
      },
      metadata: { arbpilot_kind: "strategy", publication_id: publication.id },
    });
    await publishProduct(client, product.id, "onetime");
    const cache = readProductCache();
    cache[key] = product.id;
    writeProductCache(cache);
    console.log(`[payments] 已创建策略买断商品 ${key}: ${product.id}`);
    return product.id;
  })().finally(() => {
    creating.delete(key);
  });

  creating.set(key, task);
  return task;
}
