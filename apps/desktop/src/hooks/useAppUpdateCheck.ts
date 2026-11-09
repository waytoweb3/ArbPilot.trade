import { useEffect } from "react";

import { checkAppUpdate } from "../lib/appUpdate";
import { useAppUpdateDialog } from "../lib/appUpdateDialog";
import { isDesktopRuntime } from "../lib/deploy";

const CHECK_INTERVAL_MS = 30 * 60 * 1000;

/** 启动后自动检查客户端新版本；发现更新时自动弹窗。 */
export function useAppUpdateCheck() {
  useEffect(() => {
    if (!isDesktopRuntime()) {
      return;
    }
    let active = true;
    const check = () => {
      checkAppUpdate()
        .then((pending) => {
          if (active) {
            useAppUpdateDialog.getState().setPending(pending);
          }
        })
        .catch(() => undefined);
    };
    check();
    const timer = window.setInterval(check, CHECK_INTERVAL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, []);
}
