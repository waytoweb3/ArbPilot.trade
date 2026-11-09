import { isDesktopRuntime } from "./deploy";

/**
 * 用系统默认浏览器打开外部链接。
 *
 * Tauri 的 WebView 不处理 window.open，必须交给 opener 插件；
 * 浏览器预览（vite dev）下没有 Tauri 运行时，回退到 window.open。
 */
export async function openExternal(url: string): Promise<void> {
  if (!isDesktopRuntime()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}
