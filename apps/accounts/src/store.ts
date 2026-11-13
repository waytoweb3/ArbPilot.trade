import { createHash, randomBytes } from "node:crypto";

import { config } from "./config";
import { pool } from "./db";
import { HttpError } from "./http";
import type { OAuthProfile } from "./oauth";

export interface UserRow {
  id: string;
  email: string | null;
  name: string;
  avatar_url: string | null;
}

export interface SubscriptionView {
  id: string;
  plan: string;
  status: string;
  provider: string;
  paymentRef: string | null;
  providerOrderId: string | null;
  currentPeriodEnd: string | null;
  cancelAtPeriodEnd: boolean;
  createdAt: string;
}

export interface Entitlement {
  plans: string[];
  status: "active" | "canceling" | "past_due" | "none";
  expiresAt: string | null;
  subscriptions: SubscriptionView[];
}

export interface AuthorProfileView {
  displayName: string;
  headline: string;
  bio: string;
  payoutAccount: string | null;
  executorAuthorId: string | null;
}

export function hashToken(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}

function iso(value: unknown): string | null {
  if (value instanceof Date) {
    return value.toISOString();
  }
  return typeof value === "string" ? value : null;
}

// ---------------------------------------------------------------------------
// 用户与身份
// ---------------------------------------------------------------------------

export async function findOrCreateUser(
  provider: string,
  profile: OAuthProfile,
): Promise<UserRow> {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");

    const existing = await client.query<UserRow>(
      `SELECT u.id, u.email, u.name, u.avatar_url
         FROM identities i
         JOIN users u ON u.id = i.user_id
        WHERE i.provider = $1 AND i.subject = $2`,
      [provider, profile.subject],
    );

    if (existing.rows[0]) {
      const user = existing.rows[0];
      const merged = await client.query<UserRow>(
        `UPDATE users
            SET email = COALESCE(email, $2),
                name = CASE WHEN name = '' THEN $3 ELSE name END,
                avatar_url = COALESCE(avatar_url, $4),
                updated_at = now()
          WHERE id = $1
          RETURNING id, email, name, avatar_url`,
        [user.id, profile.email, profile.name, profile.avatarUrl],
      );
      await client.query("COMMIT");
      return merged.rows[0] ?? user;
    }

    // 同一邮箱视为同一用户，把新身份挂到已有账号上。
    let userId: string | null = null;
    if (profile.email) {
      const byEmail = await client.query<{ id: string }>(
        "SELECT id FROM users WHERE lower(email) = lower($1)",
        [profile.email],
      );
      userId = byEmail.rows[0]?.id ?? null;
    }

    if (!userId) {
      const created = await client.query<{ id: string }>(
        `INSERT INTO users (email, name, avatar_url)
         VALUES ($1, $2, $3)
         RETURNING id`,
        [profile.email, profile.name, profile.avatarUrl],
      );
      userId = created.rows[0]!.id;
    }

    await client.query(
      `INSERT INTO identities (user_id, provider, subject, email)
       VALUES ($1, $2, $3, $4)
       ON CONFLICT (provider, subject) DO NOTHING`,
      [userId, provider, profile.subject, profile.email],
    );

    const merged = await client.query<UserRow>(
      `UPDATE users
          SET email = COALESCE(email, $2),
              name = CASE WHEN name = '' THEN $3 ELSE name END,
              avatar_url = COALESCE(avatar_url, $4),
              updated_at = now()
        WHERE id = $1
        RETURNING id, email, name, avatar_url`,
      [userId, profile.email, profile.name, profile.avatarUrl],
    );

    await client.query("COMMIT");
    return merged.rows[0]!;
  } catch (error) {
    await client.query("ROLLBACK");
    throw error;
  } finally {
    client.release();
  }
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

export async function createSession(
  userId: string,
  kind: "web" | "device",
): Promise<{ token: string; expiresAt: string }> {
  const token = randomBytes(32).toString("base64url");
  const expiresAt = new Date(Date.now() + config.sessionTtlDays * 86_400_000);
  await pool.query(
    `INSERT INTO sessions (user_id, token_hash, kind, expires_at)
     VALUES ($1, $2, $3, $4)`,
    [userId, hashToken(token), kind, expiresAt],
  );
  return { token, expiresAt: expiresAt.toISOString() };
}

export async function userFromToken(token: string): Promise<UserRow | null> {
  const { rows } = await pool.query<UserRow>(
    `SELECT u.id, u.email, u.name, u.avatar_url
       FROM sessions s
       JOIN users u ON u.id = s.user_id
      WHERE s.token_hash = $1
        AND s.revoked_at IS NULL
        AND s.expires_at > now()`,
    [hashToken(token)],
  );
  const user = rows[0] ?? null;
  if (user) {
    // 会话活跃时间用于风控与设备列表展示，失败不影响请求。
    void pool
      .query("UPDATE sessions SET last_seen_at = now() WHERE token_hash = $1", [hashToken(token)])
      .catch(() => undefined);
  }
  return user;
}

export async function revokeSession(token: string): Promise<void> {
  await pool.query(
    "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    [hashToken(token)],
  );
}

// ---------------------------------------------------------------------------
// 订阅与权益
// ---------------------------------------------------------------------------

function effectiveStatus(status: string, periodEnd: string | null): string {
  if (
    (status === "active" || status === "canceling") &&
    periodEnd &&
    Date.parse(periodEnd) <= Date.now()
  ) {
    return "expired";
  }
  return status;
}

export async function listSubscriptions(userId: string): Promise<SubscriptionView[]> {
  const { rows } = await pool.query<{
    id: string;
    plan: string;
    status: string;
    provider: string;
    payment_ref: string | null;
    provider_order_id: string | null;
    current_period_end: Date | null;
    cancel_at_period_end: boolean;
    created_at: Date;
  }>(
    `SELECT id, plan, status, provider, payment_ref, provider_order_id,
            current_period_end, cancel_at_period_end, created_at
       FROM subscriptions
      WHERE user_id = $1
      ORDER BY updated_at DESC`,
    [userId],
  );

  return rows.map((row) => {
    const currentPeriodEnd = iso(row.current_period_end);
    return {
      id: row.id,
      plan: row.plan,
      status: effectiveStatus(row.status, currentPeriodEnd),
      provider: row.provider,
      paymentRef: row.payment_ref,
      providerOrderId: row.provider_order_id,
      currentPeriodEnd,
      cancelAtPeriodEnd: row.cancel_at_period_end,
      createdAt: iso(row.created_at) ?? "",
    };
  });
}

export async function entitlements(userId: string): Promise<Entitlement> {
  const subscriptions = await listSubscriptions(userId);
  const plans = [
    ...new Set(
      subscriptions
        .filter((item) => item.status === "active" || item.status === "canceling")
        .map((item) => item.plan),
    ),
  ];

  let status: Entitlement["status"] = "none";
  if (subscriptions.some((item) => item.status === "active")) {
    status = "active";
  } else if (plans.length > 0) {
    status = "canceling";
  } else if (subscriptions.some((item) => item.status === "past_due")) {
    status = "past_due";
  }

  const expiresAt =
    subscriptions
      .filter((item) => plans.includes(item.plan) && item.currentPeriodEnd)
      .map((item) => item.currentPeriodEnd!)
      .sort()
      .at(-1) ?? null;

  return {
    plans,
    status,
    expiresAt,
    subscriptions,
  };
}

// ---------------------------------------------------------------------------
// 设备码（桌面端登录）
// ---------------------------------------------------------------------------

export interface DeviceCodeRow {
  id: string;
  user_id: string | null;
  status: string;
  expires_at: Date;
  fingerprint: string;
  client_name: string;
  platform: string;
}

export async function createDeviceCode(input: {
  clientName: string;
  platform: string;
  fingerprint: string;
}): Promise<{ deviceCode: string; userCode: string; expiresAt: string }> {
  const deviceCode = randomBytes(32).toString("base64url");
  const alphabet = "BCDFGHJKLMNPQRSTVWXZ23456789";
  let userCode = "";
  for (const byte of randomBytes(8)) {
    userCode += alphabet[byte % alphabet.length];
  }
  userCode = `${userCode.slice(0, 4)}-${userCode.slice(4)}`;

  const expiresAt = new Date(Date.now() + config.deviceCodeTtlSecs * 1000);
  await pool.query(
    `INSERT INTO device_codes
       (device_code_hash, user_code, client_name, platform, fingerprint, expires_at)
     VALUES ($1, $2, $3, $4, $5, $6)`,
    [hashToken(deviceCode), userCode, input.clientName, input.platform, input.fingerprint, expiresAt],
  );

  return { deviceCode, userCode, expiresAt: expiresAt.toISOString() };
}

export async function pollDeviceCode(deviceCode: string): Promise<DeviceCodeRow | null> {
  const { rows } = await pool.query<DeviceCodeRow>(
    `SELECT id, user_id, status, expires_at, fingerprint, client_name, platform
       FROM device_codes
      WHERE device_code_hash = $1`,
    [hashToken(deviceCode)],
  );
  return rows[0] ?? null;
}

export async function approveDeviceCode(
  userCode: string,
  userId: string,
): Promise<void> {
  const result = await pool.query(
    `UPDATE device_codes
        SET status = 'approved', user_id = $2, approved_at = now()
      WHERE user_code = $1 AND status = 'pending' AND expires_at > now()`,
    [userCode, userId],
  );
  if (!result.rowCount) {
    throw new HttpError(404, "配对码不存在或已过期", "invalid_user_code");
  }
}

export async function denyDeviceCode(userCode: string, userId: string): Promise<void> {
  await pool.query(
    `UPDATE device_codes
        SET status = 'denied', user_id = $2
      WHERE user_code = $1 AND status = 'pending'`,
    [userCode, userId],
  );
}

/** 抢占式领取，保证同一设备码只会兑换出一个会话。 */
export async function claimDeviceCode(id: string): Promise<boolean> {
  const result = await pool.query(
    "UPDATE device_codes SET status = 'claimed' WHERE id = $1 AND status = 'approved'",
    [id],
  );
  return Boolean(result.rowCount);
}

// ---------------------------------------------------------------------------
// 支付回写
// ---------------------------------------------------------------------------

export interface LicenseEvent {
  ref: string;
  userId: string | null;
  plan: string | null;
  status: string;
  email: string | null;
  orderId: string | null;
  currentPeriodEnd: string | null;
}

export async function applyLicenseEvent(event: LicenseEvent): Promise<void> {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");

    await client.query(
      `INSERT INTO licenses (ref, user_id, plan, status, email, expires_at, updated_at)
       VALUES ($1, $2, $3, $4, $5, $6, now())
       ON CONFLICT (ref) DO UPDATE
         SET user_id = COALESCE(EXCLUDED.user_id, licenses.user_id),
             plan = COALESCE(EXCLUDED.plan, licenses.plan),
             status = EXCLUDED.status,
             email = COALESCE(EXCLUDED.email, licenses.email),
             expires_at = COALESCE(EXCLUDED.expires_at, licenses.expires_at),
             updated_at = now()`,
      [
        event.ref,
        event.userId,
        event.plan,
        event.status,
        event.email,
        event.currentPeriodEnd,
      ],
    );

    if (event.userId && event.plan) {
      const existing = await client.query<{ id: string }>(
        `SELECT id FROM subscriptions
          WHERE user_id = $1 AND plan = $2 AND provider = 'waffo'
          ORDER BY updated_at DESC LIMIT 1`,
        [event.userId, event.plan],
      );

      const cancelAtPeriodEnd = event.status === "canceling";

      if (existing.rows[0]) {
        await client.query(
          `UPDATE subscriptions
              SET status = $2,
                  provider_order_id = COALESCE($3, provider_order_id),
                  payment_ref = COALESCE($4, payment_ref),
                  current_period_end = COALESCE($5, current_period_end),
                  cancel_at_period_end = $6,
                  updated_at = now()
            WHERE id = $1`,
          [
            existing.rows[0].id,
            event.status,
            event.orderId,
            event.ref,
            event.currentPeriodEnd,
            cancelAtPeriodEnd,
          ],
        );
      } else {
        await client.query(
          `INSERT INTO subscriptions
             (user_id, plan, status, provider, provider_order_id, payment_ref,
              current_period_end, cancel_at_period_end)
           VALUES ($1, $2, $3, 'waffo', $4, $5, $6, $7)`,
          [
            event.userId,
            event.plan,
            event.status,
            event.orderId,
            event.ref,
            event.currentPeriodEnd,
            cancelAtPeriodEnd,
          ],
        );
      }
    }

    await client.query("COMMIT");
  } catch (error) {
    await client.query("ROLLBACK");
    throw error;
  } finally {
    client.release();
  }
}

export async function createPendingCheckout(input: {
  ref: string;
  userId: string;
  plan: string;
}): Promise<void> {
  await pool.query(
    `INSERT INTO licenses (ref, user_id, plan, status)
     VALUES ($1, $2, $3, 'pending')
     ON CONFLICT (ref) DO UPDATE
       SET user_id = EXCLUDED.user_id, plan = EXCLUDED.plan, updated_at = now()`,
    [input.ref, input.userId, input.plan],
  );
}

export async function markCanceling(userId: string, subscriptionId: string): Promise<void> {
  await pool.query(
    `UPDATE subscriptions
        SET status = 'canceling', cancel_at_period_end = true, updated_at = now()
      WHERE id = $1 AND user_id = $2`,
    [subscriptionId, userId],
  );
}

// ---------------------------------------------------------------------------
// 作者身份
// ---------------------------------------------------------------------------

export async function authorProfile(userId: string): Promise<AuthorProfileView | null> {
  const { rows } = await pool.query<{
    display_name: string;
    headline: string;
    bio: string;
    payout_account: string | null;
    executor_author_id: string | null;
  }>(
    `SELECT display_name, headline, bio, payout_account, executor_author_id
       FROM author_profiles WHERE user_id = $1`,
    [userId],
  );
  const row = rows[0];
  if (!row) {
    return null;
  }
  return {
    displayName: row.display_name,
    headline: row.headline,
    bio: row.bio,
    payoutAccount: row.payout_account,
    executorAuthorId: row.executor_author_id,
  };
}

export async function saveAuthorProfile(
  userId: string,
  input: {
    displayName: string;
    headline: string;
    bio: string;
    payoutAccount: string | null;
    executorAuthorId?: string | null;
  },
): Promise<AuthorProfileView> {
  const { rows } = await pool.query<{
    display_name: string;
    headline: string;
    bio: string;
    payout_account: string | null;
    executor_author_id: string | null;
  }>(
    `INSERT INTO author_profiles
       (user_id, display_name, headline, bio, payout_account, executor_author_id)
     VALUES ($1, $2, $3, $4, $5, $6)
     ON CONFLICT (user_id) DO UPDATE
       SET display_name = EXCLUDED.display_name,
           headline = EXCLUDED.headline,
           bio = EXCLUDED.bio,
           payout_account = EXCLUDED.payout_account,
           executor_author_id = COALESCE(EXCLUDED.executor_author_id, author_profiles.executor_author_id),
           updated_at = now()
     RETURNING display_name, headline, bio, payout_account, executor_author_id`,
    [
      userId,
      input.displayName,
      input.headline,
      input.bio,
      input.payoutAccount,
      input.executorAuthorId ?? null,
    ],
  );
  const row = rows[0]!;
  return {
    displayName: row.display_name,
    headline: row.headline,
    bio: row.bio,
    payoutAccount: row.payout_account,
    executorAuthorId: row.executor_author_id,
  };
}
