import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 读取 apps/payments/.env，已存在的环境变量优先。 */
function loadDotEnv(): void {
  const file = resolve(appDir, ".env");
  if (!existsSync(file)) {
    return;
  }
  for (const line of readFileSync(file, "utf8").split(/\r?\n/)) {
    const match = /^\s*([A-Za-z0-9_]+)\s*=\s*(.*)$/.exec(line);
    if (!match) {
      continue;
    }
    const key = match[1]!;
    if (process.env[key] !== undefined) {
      continue;
    }
    let value = match[2]!.trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    process.env[key] = value.replaceAll("\\n", "\n");
  }
}

loadDotEnv();

function readPrivateKey(): string {
  const base64 = process.env.WAFFO_PRIVATE_KEY_BASE64;
  if (base64) {
    return Buffer.from(base64, "base64").toString("utf8");
  }
  const inline = process.env.WAFFO_PRIVATE_KEY;
  if (inline) {
    return inline.replaceAll("\\n", "\n");
  }
  const file = process.env.WAFFO_PRIVATE_KEY_FILE;
  if (file && existsSync(file)) {
    return readFileSync(file, "utf8");
  }
  return "";
}

function list(value: string | undefined, fallback: string[]): string[] {
  const items = (value ?? fallback.join(","))
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
  return items;
}

const port = Number(process.env.PORT ?? 8788);

export type PlanId = "arbitrage" | "strategy";

export interface PlanDefinition {
  id: PlanId;
  billingPeriod: "monthly";
  /** 展示金额（最小货币单位之上的显示值，如 USD 的 "29.00"） */
  amount: string;
  name: { zh: string; en: string };
  productId: string;
}

export const config = {
  port,
  environment: process.env.WAFFO_ENVIRONMENT === "test" ? ("test" as const) : ("prod" as const),
  merchantId: process.env.WAFFO_MERCHANT_ID ?? "MER_2tL8OGKHTKuNYq4nL9STls",
  privateKey: readPrivateKey(),
  storeId: process.env.WAFFO_STORE_ID ?? "STO_4KnR0sTssq1XPnts82FLDh",
  currency: process.env.WAFFO_CURRENCY ?? "USD",
  siteUrl: (process.env.SITE_URL ?? "http://localhost:1421").replace(/\/$/, ""),
  publicBaseUrl: (process.env.PUBLIC_BASE_URL ?? `http://127.0.0.1:${port}`).replace(/\/$/, ""),
  origins: list(process.env.ALLOWED_ORIGINS, [
    "http://localhost:1420",
    "http://127.0.0.1:1420",
    "http://localhost:1421",
    "http://127.0.0.1:1421",
    "tauri://localhost",
    "http://tauri.localhost",
  ]),
  dataDir: resolve(appDir, ".data"),
  /** 本机执行器地址；策略发布定价与订阅状态回写都走这里。 */
  executorUrl: (process.env.EXECUTOR_URL ?? "http://127.0.0.1:8787").replace(/\/$/, ""),
  executorToken: process.env.EXECUTOR_TOKEN ?? "dev-local-token",
} as const;

export const plans: PlanDefinition[] = [
  {
    id: "arbitrage",
    billingPeriod: "monthly",
    amount: process.env.WAFFO_PRICE_ARBITRAGE ?? "49.00",
    name: { zh: "ArbPilot · Arbitrage & News", en: "ArbPilot · Arbitrage & News" },
    productId: process.env.WAFFO_PRODUCT_ARBITRAGE ?? "",
  },
  {
    id: "strategy",
    billingPeriod: "monthly",
    amount: process.env.WAFFO_PRICE_STRATEGY ?? "99.00",
    name: { zh: "ArbPilot · Strategy", en: "ArbPilot · Strategy" },
    productId: process.env.WAFFO_PRODUCT_STRATEGY ?? "",
  },
];

export function findPlan(id: string): PlanDefinition | undefined {
  return plans.find((plan) => plan.id === id);
}
