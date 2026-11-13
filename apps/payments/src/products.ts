import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  BillingPeriod,
  TaxCategory,
  type WaffoPancake,
} from "@waffo/pancake-ts";

import { config, plans, type PlanId } from "./config";

type ProductIds = Partial<Record<PlanId, string>>;

const file = resolve(config.dataDir, "products.json");

function readIds(): ProductIds {
  if (!existsSync(file)) {
    return {};
  }
  try {
    return JSON.parse(readFileSync(file, "utf8")) as ProductIds;
  } catch {
    return {};
  }
}

function writeIds(ids: ProductIds): void {
  mkdirSync(config.dataDir, { recursive: true });
  writeFileSync(file, JSON.stringify(ids, null, 2));
}

let pending: Promise<Record<PlanId, string>> | null = null;

/**
 * 发布商品。测试环境密钥创建的商品只有测试版本，需要 publish 提升到生产；
 * 生产环境密钥创建的商品本身就是生产版本，没有测试版本可发布，平台会返回
 * "No test version found"，这属于正常情况。
 */
export async function publishProduct(
  client: WaffoPancake,
  id: string,
  kind: "subscription" | "onetime" = "subscription",
): Promise<void> {
  try {
    if (kind === "onetime") {
      await client.onetimeProducts.publish({ id });
    } else {
      await client.subscriptionProducts.publish({ id });
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : "";
    if (!message.includes("No test version found")) {
      throw error;
    }
  }
}

/**
 * 订阅商品 ID 的解析顺序：环境变量 → 本地缓存 → 在店铺内创建并发布。
 * 建议正式环境用 WAFFO_PRODUCT_ARBITRAGE 固定，避免重复建品。
 */
export function ensureProducts(client: WaffoPancake): Promise<Record<PlanId, string>> {
  pending ??= provision(client).catch((error: unknown) => {
    pending = null;
    throw error;
  });
  return pending;
}

async function provision(client: WaffoPancake): Promise<Record<PlanId, string>> {
  if (!config.storeId) {
    throw new Error("缺少 WAFFO_STORE_ID，无法创建订阅商品");
  }

  const ids = readIds();
  for (const plan of plans) {
    if (plan.productId) {
      ids[plan.id] = plan.productId;
    }
  }

  let created = false;
  for (const plan of plans) {
    if (ids[plan.id]) {
      continue;
    }
    const { product } = await client.subscriptionProducts.create({
      storeId: config.storeId,
      name: plan.name.en,
      billingPeriod: BillingPeriod.Monthly,
      prices: {
        [config.currency]: { amount: plan.amount, taxCategory: TaxCategory.SaaS },
      },
      metadata: { arbpilot_plan: plan.id },
    });
    // 测试环境密钥新建的版本先落在 test，发布后才在收银台生效；生产密钥无需发布。
    await publishProduct(client, product.id);
    ids[plan.id] = product.id;
    created = true;
    console.log(`[payments] 已创建订阅商品 ${plan.id}: ${product.id}`);
  }

  if (created) {
    writeIds(ids);
  }

  return { arbitrage: ids.arbitrage! };
}
