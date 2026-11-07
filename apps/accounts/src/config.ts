import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  type KeyObject,
} from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const appDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 读取 apps/accounts/.env，已存在的环境变量优先。 */
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

function csv(value: string | undefined, fallback: string[]): string[] {
  const items = (value ?? fallback.join(","))
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
  return items;
}

function readPem(name: string): string {
  const base64 = process.env[`${name}_BASE64`];
  if (base64) {
    return Buffer.from(base64, "base64").toString("utf8");
  }
  const inline = process.env[name];
  if (inline) {
    return inline.replaceAll("\\n", "\n");
  }
  const file = process.env[`${name}_FILE`];
  if (file && existsSync(file)) {
    return readFileSync(file, "utf8");
  }
  return "";
}

const port = Number(process.env.PORT ?? 8789);

export interface OAuthProviderConfig {
  id: string;
  label: string;
  clientId: string;
  clientSecret: string;
  authorizeUrl: string;
  tokenUrl: string;
  userInfoUrl: string;
  scope: string;
  redirectUri: string;
}

function buildProvider(
  id: string,
  label: string,
  defaults: Pick<
    OAuthProviderConfig,
    "authorizeUrl" | "tokenUrl" | "userInfoUrl" | "scope"
  >,
  publicBaseUrl: string,
): OAuthProviderConfig | null {
  const clientId = process.env[`OAUTH_${id.toUpperCase()}_CLIENT_ID`];
  const clientSecret = process.env[`OAUTH_${id.toUpperCase()}_CLIENT_SECRET`];
  if (!clientId || !clientSecret) {
    return null;
  }
  return {
    id,
    label,
    clientId,
    clientSecret,
    ...defaults,
    redirectUri:
      process.env[`OAUTH_${id.toUpperCase()}_REDIRECT_URI`] ??
      `${publicBaseUrl}/api/auth/${id}/callback`,
  };
}

const publicBaseUrl = (process.env.PUBLIC_BASE_URL ?? `http://127.0.0.1:${port}`).replace(
  /\/$/,
  "",
);
const siteUrl = (process.env.SITE_URL ?? "http://localhost:1421").replace(/\/$/, "");

const defaultOrigins = [
  "http://localhost:1420",
  "http://127.0.0.1:1420",
  "http://localhost:1421",
  "http://127.0.0.1:1421",
  "tauri://localhost",
  "http://tauri.localhost",
];

const deviceLimits: Record<string, number> = {
  free: Number(process.env.DEVICE_LIMIT_FREE ?? 1),
  arbitrage: Number(process.env.DEVICE_LIMIT_ARBITRAGE ?? 2),
  strategy: Number(process.env.DEVICE_LIMIT_STRATEGY ?? 3),
};

export const config = {
  port,
  publicBaseUrl,
  siteUrl,
  databaseUrl:
    process.env.DATABASE_URL ?? "postgres://arbpilot:arbpilot@127.0.0.1:5432/arbpilot",
  origins: csv(process.env.ALLOWED_ORIGINS, defaultOrigins),
  /** 支付服务回写订阅状态时使用的共享密钥，避免内部接口裸露。 */
  internalToken: process.env.INTERNAL_TOKEN ?? "dev-internal-token",
  /** 运营后台（/admin）的访问令牌；留空则后台接口禁用。 */
  adminToken: process.env.ADMIN_TOKEN ?? "",
  paymentsUrl: (process.env.PAYMENTS_URL ?? "http://127.0.0.1:8788").replace(/\/$/, ""),
  executorUrl: (process.env.EXECUTOR_URL ?? "http://127.0.0.1:8787").replace(/\/$/, ""),
  executorToken: process.env.EXECUTOR_TOKEN ?? "dev-local-token",
  sessionTtlDays: Number(process.env.SESSION_TTL_DAYS ?? 30),
  deviceCodeTtlSecs: Number(process.env.DEVICE_CODE_TTL_SECS ?? 900),
  /** 各套餐允许同时在线的设备数；未订阅只允许 1 台（用于登录与查看）。 */
  deviceLimits,
  /** 权益令牌有效期；执行器可额外放宽离线宽限期。 */
  licenseTtlHours: Number(process.env.LICENSE_TTL_HOURS ?? 24),
  /** 托管与非托管用户统一使用 Google 第三方登录。 */
  providers: [
    buildProvider(
      "google",
      "Google",
      {
        authorizeUrl: "https://accounts.google.com/o/oauth2/v2/auth",
        tokenUrl: "https://oauth2.googleapis.com/token",
        userInfoUrl: "https://openidconnect.googleapis.com/v1/userinfo",
        scope: "openid email profile",
      },
      publicBaseUrl,
    ),
  ].filter((provider): provider is OAuthProviderConfig => provider !== null),
  siteOrigin: (() => {
    try {
      return new URL(siteUrl).origin;
    } catch {
      return siteUrl;
    }
  })(),
} as const;

export interface LicenseKeys {
  privateKey: KeyObject;
  publicKey: KeyObject;
  /** base64 编码的 SPKI DER，便于执行器直接内置校验。 */
  publicKeyDer: string;
  ephemeral: boolean;
}

function resolveLicenseKeys(): LicenseKeys {
  const privatePem = readPem("LICENSE_PRIVATE_KEY");
  if (privatePem) {
    try {
      const privateKey = createPrivateKey(privatePem);
      const publicPem = readPem("LICENSE_PUBLIC_KEY");
      const publicKey = publicPem ? createPublicKey(publicPem) : createPublicKey(privateKey);
      return {
        privateKey,
        publicKey,
        publicKeyDer: publicKey.export({ type: "spki", format: "der" }).toString("base64"),
        ephemeral: false,
      };
    } catch (error) {
      console.error("[accounts] LICENSE_PRIVATE_KEY 解析失败，改用临时密钥：", error);
    }
  }

  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const publicKeyDer = publicKey.export({ type: "spki", format: "der" }).toString("base64");
  console.warn(
    "[accounts] 未配置 LICENSE_PRIVATE_KEY，已生成临时签名密钥；重启后旧权益令牌全部失效。",
  );
  console.warn(`[accounts] 临时公钥（SPKI base64）：${publicKeyDer}`);
  return { privateKey, publicKey, publicKeyDer, ephemeral: true };
}

export const licenseKeys = resolveLicenseKeys();

export function findProvider(id: string): OAuthProviderConfig | undefined {
  return config.providers.find((provider) => provider.id === id);
}

/** 只允许回跳到受信任的来源，防止 session token 被带到任意站点。 */
export function isAllowedRedirect(value: string): boolean {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return false;
  }
  if (config.origins.includes(url.origin) || url.origin === config.siteOrigin) {
    return true;
  }
  // 桌面端本地回环回调。
  return /^http:\/\/(127\.0\.0\.1|localhost):\d+$/.test(url.origin);
}
