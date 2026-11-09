import { useEffect, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Loader2, Radar } from "lucide-react";

import {
  ACCOUNTS_URL,
  AccountError,
  pollDeviceLogin,
  startDeviceLogin,
  type DeviceCodeStart,
} from "../../lib/accounts";
import { fmt, useI18n } from "../../lib/i18n";
import { openExternal } from "../../lib/openExternal";
import { useSessionStore } from "../../lib/session";
import { Button } from "../ui/button";
import { GoogleMark } from "../ui/google-mark";

type PollState = "pending" | "expired" | "denied";

/**
 * 未登录时的整窗登录门：拿到 Google 会话前不渲染任何功能页面。
 *
 * 复用顶栏账号入口的设备码流程，登录成功后写入会话令牌，App 才会挂载路由。
 */
export function LoginGate() {
  const { t } = useI18n();
  const setToken = useSessionStore((state) => state.setToken);
  const [device, setDevice] = useState<DeviceCodeStart | null>(null);
  const [state, setState] = useState<PollState>("pending");

  const start = useMutation({
    mutationFn: startDeviceLogin,
    onSuccess: (data) => {
      setDevice(data);
      setState("pending");
      void openExternal(`${data.verification_uri_complete}&provider=google`);
    },
  });

  useEffect(() => {
    if (!device || state !== "pending") {
      return;
    }
    const timer = window.setInterval(() => {
      void (async () => {
        try {
          const result = await pollDeviceLogin(device.device_code);
          if (result.status === "approved" && result.token) {
            setToken(result.token);
            setDevice(null);
          } else if (result.status === "expired" || result.status === "denied") {
            setState(result.status);
          }
        } catch {
          // 网络抖动时保持等待，下一轮继续轮询。
        }
      })();
    }, Math.max(device.interval, 2) * 1000);
    return () => window.clearInterval(timer);
  }, [device, state, setToken]);

  const startError = start.error;
  const errorText = startError
    ? startError instanceof AccountError && startError.status === 0
      ? fmt(t.settings.accountUnreachable, { url: ACCOUNTS_URL })
      : fmt(t.settings.accountLoginFailed, { message: startError.message })
    : null;

  const loginUrl = device
    ? `${device.verification_uri_complete}&provider=google`
    : "";

  return (
    <div className="flex h-full items-center justify-center overflow-y-auto px-6 py-10">
      <div className="w-full max-w-md rounded-xl border border-line bg-panel p-6 shadow-xl">
        <div className="flex items-center gap-2.5">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-accent/15 text-accent">
            <Radar size={19} />
          </div>
          <div className="min-w-0">
            <p className="text-sm font-semibold tracking-wide text-fg">ArbPilot</p>
            <p className="truncate text-[10px] tracking-[0.2em] text-subtle uppercase">
              {t.app.tagline}
            </p>
          </div>
        </div>

        <h1 className="mt-6 text-base font-semibold text-fg">{t.settings.signInRequiredTitle}</h1>
        <p className="mt-2 text-xs leading-relaxed text-muted">{t.settings.signInRequiredDesc}</p>

        <p className="mt-3 rounded-lg border border-line bg-panel-soft px-3 py-2 text-[11px] leading-relaxed text-subtle">
          {t.settings.signInRequiredHint}
        </p>

        <div className="mt-5 space-y-3">
          {start.isPending && (
            <p className="flex items-center gap-2 text-xs text-muted">
              <Loader2 size={13} className="animate-spin" />
              {t.settings.accountLoginStarting}
            </p>
          )}

          {errorText && <p className="text-xs text-danger">{errorText}</p>}

          {device && (
            <>
              <p className="text-xs text-muted">{t.settings.accountLoginCodeHint}</p>
              <div className="break-all rounded-lg border border-line bg-panel-soft px-3 py-2 font-mono text-[11px] text-body">
                {loginUrl}
              </div>
              <div className="flex items-center gap-3">
                <Button
                  size="md"
                  variant="secondary"
                  onClick={() => void openExternal(loginUrl)}
                >
                  <GoogleMark />
                  {t.settings.accountLoginOpen}
                </Button>
                <p className="font-mono text-lg tracking-[0.3em] text-fg">{device.user_code}</p>
              </div>

              {state === "pending" && (
                <p className="flex items-center gap-2 text-xs text-subtle">
                  <Loader2 size={12} className="animate-spin" />
                  {t.settings.accountLoginWaiting}
                </p>
              )}
              {state === "expired" && (
                <p className="text-xs text-warning">{t.settings.accountLoginExpired}</p>
              )}
              {state === "denied" && (
                <p className="text-xs text-danger">{t.settings.accountLoginDenied}</p>
              )}
              {(state === "expired" || state === "denied") && (
                <Button size="md" variant="secondary" onClick={() => start.mutate()}>
                  {t.settings.accountRetry}
                </Button>
              )}
            </>
          )}

          {!device && !start.isPending && (
            <Button size="md" onClick={() => start.mutate()}>
              <GoogleMark />
              {t.settings.accountContinueGoogle}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
