import { isDesktopRuntime } from "./deploy";

/** 客户端更新清单地址，与 tauri.conf.json 里 plugins.updater.endpoints 保持一致。 */
export const APP_UPDATE_ENDPOINT = "https://download.arbpilot.trade/desktop/latest.json";

export interface AppUpdateInfo {
  version: string;
  notes?: string;
  date?: string;
}

export interface DownloadProgress {
  downloaded: number;
  total?: number;
}

export interface PendingUpdate {
  info: AppUpdateInfo;
  /** 下载并安装；调用后需要重启客户端才会切到新版本。 */
  install: (onProgress: (progress: DownloadProgress) => void) => Promise<void>;
}

/** 当前客户端版本；浏览器预览时回退到构建时注入的版本号。 */
export async function currentAppVersion(): Promise<string> {
  if (!isDesktopRuntime()) {
    return (import.meta.env.VITE_APP_VERSION as string | undefined) ?? "0.1.0";
  }
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}

/** 读取服务器上的 latest.json 并与当前版本比对，已是最新时返回 null。 */
export async function checkAppUpdate(): Promise<PendingUpdate | null> {
  if (!isDesktopRuntime()) throw new Error("update.desktopOnly");
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  if (!update) return null;

  let total: number | undefined;
  let downloaded = 0;

  return {
    info: {
      version: update.version,
      notes: update.body || undefined,
      date: update.date,
    },
    install: async (onProgress) => {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? total;
          onProgress({ downloaded, total });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          onProgress({ downloaded, total });
        } else if (event.event === "Finished") {
          onProgress({ downloaded: total ?? downloaded, total });
        }
      });
    },
  };
}

/** 安装完成后重启客户端，切换到新版本。 */
export async function relaunchApp(): Promise<void> {
  if (!isDesktopRuntime()) throw new Error("update.desktopOnly");
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("app_restart");
}
