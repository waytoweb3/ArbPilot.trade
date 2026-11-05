import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

import { config, type PlanId } from "./config";

export type LicenseStatus = "pending" | "active" | "canceling" | "canceled" | "past_due" | "expired";

export interface License {
  ref: string;
  status: LicenseStatus;
  plan: PlanId | null;
  email: string | null;
  orderId: string | null;
  productName: string | null;
  createdAt: string;
  updatedAt: string;
  expiresAt: string | null;
}

interface StoreShape {
  licenses: Record<string, License>;
  events: string[];
}

const file = resolve(config.dataDir, "licenses.json");

function emptyStore(): StoreShape {
  return { licenses: {}, events: [] };
}

function load(): StoreShape {
  if (!existsSync(file)) {
    return emptyStore();
  }
  try {
    const parsed = JSON.parse(readFileSync(file, "utf8")) as Partial<StoreShape>;
    return { licenses: parsed.licenses ?? {}, events: parsed.events ?? [] };
  } catch {
    return emptyStore();
  }
}

const store = load();

function persist(): void {
  mkdirSync(config.dataDir, { recursive: true });
  writeFileSync(file, JSON.stringify(store, null, 2));
}

/** 已过期的 active/canceling 授权按 expired 返回，无需等 webhook。 */
export function withEffectiveStatus(license: License): License {
  if (
    (license.status === "active" || license.status === "canceling") &&
    license.expiresAt &&
    Date.parse(license.expiresAt) < Date.now()
  ) {
    return { ...license, status: "expired" };
  }
  return license;
}

export function getLicense(ref: string): License | null {
  const license = store.licenses[ref];
  return license ? withEffectiveStatus(license) : null;
}

export function createPendingLicense(ref: string, plan: PlanId, email: string | null): License {
  const now = new Date().toISOString();
  const license: License = {
    ref,
    status: "pending",
    plan,
    email,
    orderId: null,
    productName: null,
    createdAt: now,
    updatedAt: now,
    expiresAt: null,
  };
  store.licenses[ref] = license;
  persist();
  return license;
}

export function upsertLicense(
  ref: string,
  patch: Partial<Omit<License, "ref" | "createdAt">>,
): License {
  const now = new Date().toISOString();
  const current = store.licenses[ref] ?? {
    ref,
    status: "pending" as LicenseStatus,
    plan: null,
    email: null,
    orderId: null,
    productName: null,
    createdAt: now,
    updatedAt: now,
    expiresAt: null,
  };
  const next: License = { ...current, ...patch, ref, updatedAt: now };
  store.licenses[ref] = next;
  persist();
  return next;
}

/** Webhook 可能重投，按投递 ID 去重。 */
export function isProcessedEvent(id: string): boolean {
  return store.events.includes(id);
}

export function markProcessedEvent(id: string): void {
  store.events.push(id);
  if (store.events.length > 1000) {
    store.events.splice(0, store.events.length - 1000);
  }
  persist();
}
