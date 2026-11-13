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

export type PlanId = "arbitrage";

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
  /** 账号服务地址；置空可退回旧版 ref 授权模式。 */
  accountsUrl: (process.env.ACCOUNTS_URL ?? "http://127.0.0.1:8789").replace(/\/$/, ""),
  accountsInternalToken: process.env.ACCOUNTS_INTERNAL_TOKEN ?? "dev-internal-token",
  /** 策略加密支付（NOWPayments）：未配置 API Key 时加密收银台返回 503。 */
  nowpaymentsApiKey: process.env.NOWPAYMENTS_API_KEY ?? "",
  nowpaymentsIpnSecret: process.env.NOWPAYMENTS_IPN_SECRET ?? "",
  nowpaymentsApiUrl: (process.env.NOWPAYMENTS_API_URL ?? "https://api.nowpayments.io/v1").replace(
    /\/$/,
    "",
  ),
  /** 配置出款账号后，作者分成会在收款确认时自动转到其地址。 */
  nowpaymentsPayoutEmail: process.env.NOWPAYMENTS_PAYOUT_EMAIL ?? "",
  nowpaymentsPayoutPassword: process.env.NOWPAYMENTS_PAYOUT_PASSWORD ?? "",
  /** 冷启动结束时间（ISO）；未设置时按冷启动 10% 计。 */
  commissionColdStartUntil: process.env.STRATEGY_COMMISSION_COLD_START_UNTIL ?? "",
} as const;

/**
 * `arbitrage` 是 $19/月的「自托管部署」，包含套利/新闻、在本机运行策略与使用策略市场。
 * 创建与发布策略对所有登录用户免费；策略市场里的单个策略由作者自行定价，
 * 平台按成交额抽成（冷启动 6 个月 10%，之后 20%）。
 */
export const plans: PlanDefinition[] = [
  {
    id: "arbitrage",
    billingPeriod: "monthly",
    amount: process.env.WAFFO_PRICE_ARBITRAGE ?? "19.00",
    name: { zh: "ArbPilot · 自托管部署", en: "ArbPilot · Self-hosted Deployment" },
    productId: process.env.WAFFO_PRODUCT_ARBITRAGE ?? "",
  },
];

export function findPlan(id: string): PlanDefinition | undefined {
  return plans.find((plan) => plan.id === id);
}
