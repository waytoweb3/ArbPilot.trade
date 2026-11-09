import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useMutation } from "@tanstack/react-query";
import { Loader2, LogOut, RefreshCw, UserRound, X } from "lucide-react";

import { useAccount, useLogout } from "../../hooks/useAccount";
import { useAccountDialog } from "../../lib/accountDialog";
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
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { GoogleMark } from "../ui/google-mark";

type PollState = "pending" | "expired" | "denied";

/**
 * 顶栏账号入口：未登录时用浏览器设备码登录，登录后显示账号与套餐。
 *
 * 登录只用于订阅授权与策略购买同步，不涉及交易所凭证与订单。
 * 弹窗与菜单都渲染到 body，避免 header 的 backdrop-filter 影响 fixed 定位。
 */
export function AccountButton() {
  const { t } = useI18n();
  const token = useSessionStore((state) => state.token);
  const setToken = useSessionStore((state) => state.setToken);
  const account = useAccount();
  const logout = useLogout();

  const buttonRef = useRef<HTMLButtonElement>(null);
  const open = useAccountDialog((state) => state.open);
  const switching = useAccountDialog((state) => state.switching);
  const showLogin = useAccountDialog((state) => state.show);
  const closeLogin = useAccountDialog((state) => state.close);
  const [menu, setMenu] = useState<{ top: number; right: number } | null>(null);
  const [device, setDevice] = useState<DeviceCodeStart | null>(null);
  const [state, setState] = useState<PollState>("pending");

  const start = useMutation({
    mutationFn: startDeviceLogin,
    onSuccess: (data) => {
      setDevice(data);
      setState("pending");
    },
  });

  // 浏览器确认后由轮询换到会话令牌。
  useEffect(() => {
    if (!open || !device || state !== "pending") {
      return;
    }
    const timer = window.setInterval(() => {
      void (async () => {
        try {
          const result = await pollDeviceLogin(device.device_code);
          if (result.status === "approved" && result.token) {
            setToken(result.token);
            setDevice(null);
            closeLogin();
          } else if (result.status === "expired" || result.status === "denied") {
            setState(result.status);
          }
        } catch {
          // 网络抖动时保持等待，下一轮继续轮询。
        }
      })();
    }, Math.max(device.interval, 2) * 1000);
    return () => window.clearInterval(timer);
  }, [open, device, state, setToken, closeLogin]);

  // 弹窗可能由顶栏或设置页打开，打开时就申请新的配对码。
  useEffect(() => {
    if (!open) {
      return;
    }
    setDevice(null);
    setState("pending");
    start.reset();
    start.mutate();
    // start 是稳定引用，只需在弹窗打开时触发一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const openLogin = () => {
    setMenu(null);
    showLogin(false);
  };

  const switchAccount = async () => {
    setMenu(null);
    try {
      await logout.mutateAsync();
    } catch {
      // 账号服务不可达时本地令牌已清除，仍然允许重新登录。
    }
    showLogin(true);
  };

  const toggleMenu = () => {
    if (menu) {
      setMenu(null);
      return;
    }
    const rect = buttonRef.current?.getBoundingClientRect();
    if (rect) {
      setMenu({ top: rect.bottom + 6, right: window.innerWidth - rect.right });
    }
  };

  const startError = start.error;
  const errorText = startError
    ? startError instanceof AccountError && startError.status === 0
      ? fmt(t.settings.accountUnreachable, { url: ACCOUNTS_URL })
      : fmt(t.settings.accountLoginFailed, { message: startError.message })
    : null;

  const me = account.data;
  const loginUrl = device
    ? `${device.verification_uri_complete}&provider=google${switching ? "&switch=1" : ""}`
    : "";
  const plan = me?.entitlement.plans[0];
  const planLabel =
    plan === "arbitrage"
      ? t.settings.billingArbitrage
      : plan === "strategy"
        ? t.settings.billingStrategy
        : t.settings.accountPlanNone;

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        onClick={token ? toggleMenu : openLogin}
        title={
          token ? (me?.user.name ?? t.settings.accountSignedIn) : t.settings.accountContinueGoogle
        }
        aria-label={
          token ? (me?.user.name ?? t.settings.accountSignedIn) : t.settings.accountContinueGoogle
        }
        aria-haspopup={token ? "menu" : undefined}
        aria-expanded={token ? Boolean(menu) : undefined}
        className="h-8 w-8 shrink-0 overflow-hidden rounded-full border border-line-strong bg-panel-soft text-muted transition-colors hover:border-accent/60 hover:text-fg"
      >
        {me?.user.avatarUrl ? (
          <img src={me.user.avatarUrl} alt="" className="h-full w-full object-cover" />
        ) : (
          <UserRound size={15} className="mx-auto" />
        )}
      </button>

      {menu &&
        createPortal(
          <>
            <div className="fixed inset-0 z-40" onClick={() => setMenu(null)} />
            <div
              role="menu"
              style={{ top: menu.top, right: menu.right }}
              className="fixed z-50 w-60 rounded-xl border border-line bg-panel p-3 shadow-xl"
            >
              <p className="truncate text-xs font-medium text-fg">
                {me?.user.name ?? t.settings.accountSignedIn}
              </p>
              {me?.user.email && (
                <p className="mt-0.5 truncate text-[11px] text-subtle">{me.user.email}</p>
              )}
              <div className="mt-2 flex items-center justify-between text-[11px] text-muted">
                <span>{t.settings.accountPlan}</span>
                <Badge tone={plan ? "success" : "neutral"}>{planLabel}</Badge>
              </div>
              <button
                type="button"
                role="menuitem"
                onClick={() => void switchAccount()}
                className="mt-1 flex w-full items-center gap-1.5 rounded-lg px-2 py-1.5 text-left text-xs text-muted hover:bg-hover hover:text-fg"
              >
                <RefreshCw size={13} />
                {t.settings.accountSwitch}
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenu(null);
                  logout.mutate();
                }}
                className="mt-1 flex w-full items-center gap-1.5 rounded-lg px-2 py-1.5 text-left text-xs text-muted hover:bg-hover hover:text-fg"
              >
                <LogOut size={13} />
                {t.settings.accountLogout}
              </button>
            </div>
          </>,
          document.body,
        )}

      {open &&
        createPortal(
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
            role="dialog"
            aria-modal="true"
          >
            <div className="w-full max-w-sm rounded-xl border border-line bg-panel shadow-xl">
              <div className="flex items-start justify-between gap-3 border-b border-line px-4 py-3">
                <div>
                  <p className="text-sm font-semibold text-fg">{t.settings.accountLoginTitle}</p>
                  <p className="mt-0.5 text-[11px] leading-relaxed text-muted">
                    {t.settings.accountLoginDesc}
                  </p>
                </div>
                <Button
                  size="sm"
                  variant="ghost"
                  className="w-7 shrink-0 px-0"
                  onClick={closeLogin}
                  aria-label={t.accounts.cancel}
                >
                  <X size={14} />
                </Button>
              </div>

              <div className="space-y-3 p-4 text-xs text-muted">
                {start.isPending && (
                  <p className="flex items-center gap-2">
                    <Loader2 size={13} className="animate-spin" />
                    {t.settings.accountLoginStarting}
                  </p>
                )}

                {errorText && <p className="text-danger">{errorText}</p>}

                {device && (
                  <>
                    {switching && (
                      <p className="text-warning">{t.settings.accountSwitchHint}</p>
                    )}
                    <p>{t.settings.accountLoginCodeHint}</p>
                    <div className="break-all rounded-lg border border-line bg-panel-soft px-3 py-2 font-mono text-[11px] text-body">
                      {loginUrl}
                    </div>
                    <Button
                      size="sm"
                      onClick={() => void openExternal(loginUrl)}
                    >
                      <GoogleMark />
                      {t.settings.accountContinueGoogle}
                    </Button>
                    <p className="font-mono text-lg tracking-[0.3em] text-fg">{device.user_code}</p>

                    {state === "pending" && (
                      <p className="flex items-center gap-2 text-subtle">
                        <Loader2 size={12} className="animate-spin" />
                        {t.settings.accountLoginWaiting}
                      </p>
                    )}
                    {state === "expired" && (
                      <p className="text-warning">{t.settings.accountLoginExpired}</p>
                    )}
                    {state === "denied" && (
                      <p className="text-danger">{t.settings.accountLoginDenied}</p>
                    )}
                    {(state === "expired" || state === "denied") && (
                      <Button size="sm" variant="secondary" onClick={() => start.mutate()}>
                        {t.settings.accountRetry}
                      </Button>
                    )}
                  </>
                )}
              </div>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
