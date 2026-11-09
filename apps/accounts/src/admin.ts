import { pool } from "./db";

/**
 * 运营后台数据：按用户聚合注册信息、订阅与授权。
 * 只读取账号库，不接触执行器的交易数据与交易所密钥。
 */

export interface AdminSubscription {
  id: string;
  plan: string;
  status: string;
  provider: string;
  providerOrderId: string | null;
  paymentRef: string | null;
  currentPeriodEnd: string | null;
  cancelAtPeriodEnd: boolean;
  createdAt: string | null;
  updatedAt: string | null;
}

export interface AdminLicense {
  ref: string;
  plan: string | null;
  status: string;
  email: string | null;
  expiresAt: string | null;
  createdAt: string | null;
  updatedAt: string | null;
}

export interface AdminUser {
  id: string;
  email: string | null;
  name: string;
  avatarUrl: string | null;
  createdAt: string;
  lastSeenAt: string | null;
  providers: string[];
  isAuthor: boolean;
  /** 是否产生过订阅（含已取消/已过期），用于区分免费与付费用户。 */
  paid: boolean;
  plans: string[];
  /** active / canceling / past_due / pending / expired / canceled / none */
  status: string;
  currentPeriodEnd: string | null;
  subscriptions: AdminSubscription[];
  licenses: AdminLicense[];
}

export interface AdminSummary {
  totalUsers: number;
  freeUsers: number;
  paidUsers: number;
  activeSubscriptions: number;
  pastDueSubscriptions: number;
  newUsers7d: number;
  newUsers30d: number;
}

export interface AdminOverview {
  generatedAt: string;
  summary: AdminSummary;
  users: AdminUser[];
  truncated: boolean;
}

interface AdminRow {
  id: string;
  email: string | null;
  name: string;
  avatar_url: string | null;
  created_at: Date;
  last_seen_at: Date | null;
  providers: string[];
  is_author: boolean;
  subscriptions: AdminSubscription[];
  licenses: AdminLicense[];
}

const LIMIT = 5000;

function iso(value: Date | string | null): string | null {
  if (value instanceof Date) {
    return value.toISOString();
  }
  return typeof value === "string" ? value : null;
}

/** 计费周期已过但状态尚未回写时，按过期展示。 */
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

const STATUS_PRIORITY = ["active", "canceling", "past_due", "pending", "expired", "canceled"];

function overallStatus(
  subscriptions: AdminSubscription[],
  licenses: AdminLicense[],
): string {
  const statuses = [
    ...subscriptions.map((item) => effectiveStatus(item.status, item.currentPeriodEnd)),
    ...licenses.map((item) => effectiveStatus(item.status, item.expiresAt)),
  ];
  if (statuses.length === 0) {
    return "none";
  }
  return STATUS_PRIORITY.find((status) => statuses.includes(status)) ?? statuses[0]!;
}

const ENTITLED = new Set(["active", "canceling", "past_due"]);

export async function adminOverview(): Promise<AdminOverview> {
  const { rows } = await pool.query<AdminRow>(
    `SELECT
       u.id,
       u.email,
       u.name,
       u.avatar_url,
       u.created_at,
       (SELECT max(s.last_seen_at)
          FROM sessions s
         WHERE s.user_id = u.id AND s.revoked_at IS NULL) AS last_seen_at,
      COALESCE((SELECT array_agg(DISTINCT i.provider)
                   FROM identities i WHERE i.user_id = u.id), '{}') AS providers,
       EXISTS (SELECT 1 FROM author_profiles ap WHERE ap.user_id = u.id) AS is_author,
       COALESCE((SELECT json_agg(json_build_object(
                   'id', s.id,
                   'plan', s.plan,
                   'status', s.status,
                   'provider', s.provider,
                   'providerOrderId', s.provider_order_id,
                   'paymentRef', s.payment_ref,
                   'currentPeriodEnd', s.current_period_end,
                   'cancelAtPeriodEnd', s.cancel_at_period_end,
                   'createdAt', s.created_at,
                   'updatedAt', s.updated_at
                 ) ORDER BY s.updated_at DESC)
                   FROM subscriptions s WHERE s.user_id = u.id), '[]'::json) AS subscriptions,
       COALESCE((SELECT json_agg(json_build_object(
                   'ref', l.ref,
                   'plan', l.plan,
                   'status', l.status,
                   'email', l.email,
                   'expiresAt', l.expires_at,
                   'createdAt', l.created_at,
                   'updatedAt', l.updated_at
                 ) ORDER BY l.updated_at DESC)
                   FROM licenses l WHERE l.user_id = u.id), '[]'::json) AS licenses
     FROM users u
     ORDER BY u.created_at DESC
     LIMIT ${LIMIT + 1}`,
  );

  const truncated = rows.length > LIMIT;
  const users: AdminUser[] = rows.slice(0, LIMIT).map((row) => {
    const status = overallStatus(row.subscriptions, row.licenses);
    const paid =
      row.subscriptions.length > 0 ||
      row.licenses.some((license) => ENTITLED.has(license.status));
    const plans = [
      ...new Set(
        [
          ...row.subscriptions.map((item) => item.plan),
          ...row.licenses.map((item) => item.plan ?? ""),
        ].filter(Boolean),
      ),
    ];
    const currentPeriodEnd =
      row.subscriptions
        .map((item) => item.currentPeriodEnd)
        .filter((value): value is string => Boolean(value))
        .sort()
        .at(-1) ?? null;

    return {
      id: row.id,
      email: row.email,
      name: row.name,
      avatarUrl: row.avatar_url,
      createdAt: iso(row.created_at) ?? "",
      lastSeenAt: iso(row.last_seen_at),
      providers: row.providers,
      isAuthor: row.is_author,
      paid,
      plans,
      status,
      currentPeriodEnd,
      subscriptions: row.subscriptions,
      licenses: row.licenses,
    };
  });

  const now = Date.now();
  const day = 86_400_000;
  const summary: AdminSummary = {
    totalUsers: users.length,
    freeUsers: users.filter((user) => !user.paid).length,
    paidUsers: users.filter((user) => user.paid).length,
    activeSubscriptions: users.reduce(
      (total, user) =>
        total +
        user.subscriptions.filter(
          (item) => effectiveStatus(item.status, item.currentPeriodEnd) === "active",
        ).length,
      0,
    ),
    pastDueSubscriptions: users.reduce(
      (total, user) =>
        total +
        user.subscriptions.filter(
          (item) => effectiveStatus(item.status, item.currentPeriodEnd) === "past_due",
        ).length,
      0,
    ),
    newUsers7d: users.filter((user) => now - Date.parse(user.createdAt) <= 7 * day).length,
    newUsers30d: users.filter((user) => now - Date.parse(user.createdAt) <= 30 * day).length,
  };

  return { generatedAt: new Date().toISOString(), summary, users, truncated };
}
