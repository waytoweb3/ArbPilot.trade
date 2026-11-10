/**
 * 系统级通知：把执行器推送的消息同时发到操作系统通知中心。
 *
 * 仅在 Tauri 环境可用；浏览器预览或用户拒绝授权时静默失败，
 * 应用内通知卡片仍然照常展示。
 */

/** 授权结果缓存，避免每条通知都申请一次权限。 */
let granted: boolean | null = null;

async function ensurePermission(): Promise<boolean> {
  if (granted !== null) {
    return granted;
  }
  try {
    const { isPermissionGranted, requestPermission } = await import(
      "@tauri-apps/plugin-notification"
    );
    granted =
      (await isPermissionGranted()) ||
      (await requestPermission()) === "granted";
  } catch {
    granted = false;
  }
  return granted;
}

/** 发送一条系统通知；失败时不影响应用内提示。 */
export async function notifySystem(title: string, body: string): Promise<void> {
  if (!(await ensurePermission())) {
    return;
  }
  try {
    const { sendNotification } = await import(
      "@tauri-apps/plugin-notification"
    );
    sendNotification({ title, body });
  } catch {
    // 系统通知失败时保持静默，应用内卡片仍可见。
  }
}
