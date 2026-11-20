import { sign } from "node:crypto";

import { config, licenseKeys } from "./config";
import type { Entitlement } from "./store";

export const LICENSE_TOKEN_PREFIX = "arb1";

/** 已知付费套餐；未知套餐名不会写进令牌，避免脏数据放大权益。 */
const KNOWN_PLANS = new Set(["arbitrage"]);

/**
 * 统一权益口径：
 * - 只有 `active` / `canceling` 且未到期才有效；
 * - `canceling` 表示已退订，但可用到当期结束（期末失效）；
 * - `past_due` / `none` 一律不发放。
 */
export function effectivePlans(entitlement: Entitlement, now = Date.now()): string[] {
  if (entitlement.status !== "active" && entitlement.status !== "canceling") {
    return [];
  }
  if (entitlement.expiresAt) {
    const expires = Date.parse(entitlement.expiresAt);
    if (Number.isFinite(expires) && expires <= now) {
      return [];
    }
  }
  return entitlement.plans.filter((plan) => KNOWN_PLANS.has(plan));
}

export interface LicenseClaims {
  /** 用户 ID */
  sub: string;
  email: string | null;
  /** 当前有效的套餐列表；为空表示免费用户 */
  plans: string[];
  status: Entitlement["status"];
  iat: number;
  exp: number;
  v: 1;
}

export interface IssuedLicense {
  token: string;
  claims: LicenseClaims;
  expiresAt: string;
}

/**
 * 签发 Ed25519 签名令牌。私钥只留在账号服务，执行器只需内置公钥即可离线校验，
 * 因此用户本机无法伪造权益。
 */
export function issueLicenseToken(input: {
  userId: string;
  email: string | null;
  entitlement: Entitlement;
}): IssuedLicense {
  const now = Math.floor(Date.now() / 1000);
  const exp = now + Math.round(config.licenseTtlHours * 3600);
  const claims: LicenseClaims = {
    sub: input.userId,
    email: input.email,
    plans: effectivePlans(input.entitlement),
    status: input.entitlement.status,
    iat: now,
    exp,
    v: 1,
  };

  const payload = Buffer.from(JSON.stringify(claims)).toString("base64url");
  const message = `${LICENSE_TOKEN_PREFIX}.${payload}`;
  const signature = sign(null, Buffer.from(message), licenseKeys.privateKey);
  return {
    token: `${message}.${signature.toString("base64url")}`,
    claims,
    expiresAt: new Date(exp * 1000).toISOString(),
  };
}

export function publicKeyInfo(): {
  algorithm: "ed25519";
  publicKey: string;
  ephemeral: boolean;
} {
  return {
    algorithm: "ed25519",
    publicKey: licenseKeys.publicKeyDer,
    ephemeral: licenseKeys.ephemeral,
  };
}
