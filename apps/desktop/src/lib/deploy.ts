import type { UnlistenFn } from "@tauri-apps/api/event";

/** 发布包与 manifest.json 的下载根地址，与 deploy/package.sh 的默认值保持一致。 */
export const RELEASE_BASE_URL = "https://download.arbpilot.trade/executor";
const SERVER_KEY = "arbpilot.deploy.server";

export type AuthKind = "password" | "key";

export interface ServerForm {
  host: string;
  port: string;
  username: string;
  auth: AuthKind;
  installDir: string;
}

export interface ServerInput {
  host: string;
  port: number;
  username: string;
  auth: AuthKind;
  password?: string;
  privateKey?: string;
  passphrase?: string;
  installDir?: string;
}

export interface ServerCredentials {
  password?: string;
  privateKey?: string;
  passphrase?: string;
}

export interface ReleaseManifest {
  version: string;
  bundleUrl: string;
  sha256: string;
  size?: number;
  releasedAt?: string;
  notes?: string;
}

export interface ProbeResult {
  version: string | null;
  isRoot: boolean;
  installDir: string;
}

/** 浏览器里打开（vite dev）时没有 Tauri 运行时，部署相关操作只能给出提示。 */
export function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isDesktopRuntime()) {
    throw new Error("deploy.desktopOnly");
  }
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(command, args);
}

export function loadServerForm(): Partial<ServerForm> {
  try {
    const raw = localStorage.getItem(SERVER_KEY);
    return raw ? (JSON.parse(raw) as Partial<ServerForm>) : {};
  } catch {
    return {};
  }
}

/** 只保存地址与用户名等非敏感字段，密码与私钥写入系统钥匙串。 */
export function saveServerForm(form: ServerForm): void {
  localStorage.setItem(SERVER_KEY, JSON.stringify(form));
}

export async function fetchManifest(): Promise<ReleaseManifest> {
  const response = await fetch(`${RELEASE_BASE_URL}/manifest.json`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`manifest HTTP ${response.status}`);
  }
  const raw = (await response.json()) as Record<string, unknown>;
  const version = String(raw.version ?? "").trim();
  const bundleUrl = String(raw.bundle_url ?? raw.bundleUrl ?? "").trim();
  const sha256 = String(raw.sha256 ?? "").trim();
  if (!version || !bundleUrl || !sha256) {
    throw new Error("manifest 格式不正确");
  }
  return {
    version,
    bundleUrl,
    sha256,
    size: typeof raw.size === "number" ? raw.size : undefined,
    releasedAt: raw.released_at
      ? String(raw.released_at)
      : raw.releasedAt
        ? String(raw.releasedAt)
        : undefined,
    notes: raw.notes ? String(raw.notes) : undefined,
  };
}

export const saveCredentials = (server: ServerInput) =>
  invokeCommand<void>("server_credentials_save", { server });

export const loadCredentials = (server: ServerInput) =>
  invokeCommand<ServerCredentials | null>("server_credentials_load", { server });

export const deleteCredentials = (server: ServerInput) =>
  invokeCommand<void>("server_credentials_delete", { server });

export const probeServer = (server: ServerInput) =>
  invokeCommand<ProbeResult>("server_probe", { server });

export const deployServer = (server: ServerInput, plan: ReleaseManifest) =>
  invokeCommand<string>("server_deploy", {
    server,
    plan: { version: plan.version, bundleUrl: plan.bundleUrl, sha256: plan.sha256 },
  });

export async function onDeployLog(handler: (chunk: string) => void): Promise<UnlistenFn> {
  if (!isDesktopRuntime()) {
    return () => undefined;
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<string>("deploy-log", (event) => handler(event.payload));
}
