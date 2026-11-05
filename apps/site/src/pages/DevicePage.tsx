import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { useI18n } from "../lib/i18n";

const ACCOUNTS_URL = (
  (import.meta.env.VITE_ACCOUNTS_URL as string | undefined) ?? "http://127.0.0.1:8789"
).replace(/\/$/, "");

const SESSION_KEY = "arbpilot.session";

const copy = {
  zh: {
    eyebrow: "桌面端登录",
    title: "在浏览器中确认登录",
    desc: "账号只用于订阅授权、设备数量与策略购买同步。API Key、签名私钥、订单与数据库始终留在你的机器上。",
    code: "配对码",
    google: "使用 Google 登录",
    github: "使用 GitHub 登录",
    redirecting: "正在跳转到授权页面…",
    approve: "允许这台设备登录",
    deny: "拒绝",
    approved: "已授权，可以回到 ArbPilot 桌面端。",
    denied: "已拒绝本次登录。",
    missing: "缺少配对码，请回到桌面端重新发起登录。",
    noProvider: "账号服务尚未配置第三方登录，请设置 OAUTH_GOOGLE_CLIENT_ID/SECRET 后重启。",
    failed: "操作失败：{message}",
    back: "返回首页",
  },
  en: {
    eyebrow: "Desktop sign-in",
    title: "Confirm sign-in in your browser",
    desc: "The account is only used for licensing, device limits and purchased strategies. API keys, signing keys, orders and your database stay on your machine.",
    code: "Pairing code",
    google: "Continue with Google",
    github: "Continue with GitHub",
    redirecting: "Redirecting to the provider…",
    approve: "Allow this device",
    deny: "Deny",
    approved: "Approved. You can return to the ArbPilot desktop app.",
    denied: "This sign-in was denied.",
    missing: "Missing pairing code. Start sign-in again from the desktop app.",
    noProvider:
      "No third-party sign-in is configured yet. Set OAUTH_GOOGLE_CLIENT_ID/SECRET and restart the accounts service.",
    failed: "Something went wrong: {message}",
    back: "Back to home",
  },
} as const;

type Provider = { id: string; label: string };

/** 桌面端设备码登录的浏览器确认页。 */
export default function DevicePage() {
  const { locale } = useI18n();
  const text = copy[locale];
  const [params] = useSearchParams();
  const userCode = (params.get("code") ?? "").toUpperCase();
  const wanted = params.get("provider") ?? "google";
  /** 桌面端切换账号：忽略浏览器里缓存的旧会话，强制重新登录并选择账号。 */
  const switching = params.get("switch") === "1";
  const authError = params.get("auth_error");

  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [session, setSession] = useState<string | null>(() => {
    if (switching) {
      localStorage.removeItem(SESSION_KEY);
      return null;
    }
    const fromUrl = params.get("session");
    if (fromUrl) {
      localStorage.setItem(SESSION_KEY, fromUrl);
      // 从地址栏抹掉会话令牌，避免留在浏览历史里。
      const clean = new URL(window.location.href);
      clean.searchParams.delete("session");
      window.history.replaceState({}, "", clean.toString());
      return fromUrl;
    }
    return localStorage.getItem(SESSION_KEY);
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(
    authError ? text.failed.replace("{message}", authError) : null,
  );
  const [result, setResult] = useState<"approved" | "denied" | null>(null);
  const autoStarted = useRef(false);

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const response = await fetch(`${ACCOUNTS_URL}/api/auth/providers`);
        const payload = (await response.json()) as { providers?: Provider[] };
        if (alive) {
          setProviders(payload.providers ?? []);
        }
      } catch {
        if (alive) {
          setProviders([]);
        }
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const startProvider = async (id: string) => {
    setBusy(true);
    setError(null);
    try {
      const redirectUri = `${window.location.origin}/device?code=${encodeURIComponent(userCode)}`;
      const response = await fetch(`${ACCOUNTS_URL}/api/auth/${id}/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ redirect_uri: redirectUri }),
      });
      if (!response.ok) {
        const payload = (await response.json().catch(() => null)) as { message?: string } | null;
        throw new Error(payload?.message ?? String(response.status));
      }
      const payload = (await response.json()) as { authorizeUrl: string };
      window.location.href = payload.authorizeUrl;
    } catch (cause) {
      setError(text.failed.replace("{message}", cause instanceof Error ? cause.message : String(cause)));
      setBusy(false);
    }
  };

  // 桌面端已经指明用哪个提供方时，登录后直接跳到授权页。
  useEffect(() => {
    if (autoStarted.current || session || !providers || !userCode) {
      return;
    }
    if (providers.some((item) => item.id === wanted)) {
      autoStarted.current = true;
      void startProvider(wanted);
    }
    // startProvider 依赖 providers 与 userCode，这里只应触发一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providers, session, userCode, wanted]);

  const decide = async (action: "approve" | "deny") => {
    if (!session) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const response = await fetch(`${ACCOUNTS_URL}/api/auth/device/${action}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${session}`,
        },
        body: JSON.stringify({ user_code: userCode }),
      });
      if (!response.ok) {
        // 本地缓存的会话已失效时清掉，让用户重新走第三方登录。
        if (response.status === 401) {
          localStorage.removeItem(SESSION_KEY);
          setSession(null);
        }
        const payload = (await response.json().catch(() => null)) as { message?: string } | null;
        throw new Error(payload?.message ?? String(response.status));
      }
      setResult(action === "approve" ? "approved" : "denied");
      if (action === "deny") {
        localStorage.removeItem(SESSION_KEY);
      }
    } catch (cause) {
      setError(text.failed.replace("{message}", cause instanceof Error ? cause.message : String(cause)));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page pb-24 pt-32">
      <div className="mx-auto max-w-xl">
        <p className="eyebrow">{text.eyebrow}</p>
        <h1 className="mt-4 text-[clamp(1.7rem,3.4vw,2.4rem)] font-semibold leading-[1.2] tracking-[-0.02em] text-fg">
          {text.title}
        </h1>
        <p className="mt-4 text-[13.5px] leading-[1.9] text-body">{text.desc}</p>

        {!userCode ? (
          <p className="mt-6 rounded-2xl border border-line bg-panel p-5 text-[12.5px] text-warning">
            {text.missing}
          </p>
        ) : (
          <div className="mt-6 rounded-2xl border border-line bg-panel p-5">
            <p className="text-[10.5px] uppercase tracking-[0.12em] text-subtle">{text.code}</p>
            <p className="mt-1 font-mono text-2xl tracking-[0.28em] text-fg">{userCode}</p>

            {error && <p className="mt-4 text-[12px] text-danger">{error}</p>}

            {result === "approved" && (
              <p className="mt-4 text-[13px] text-accent">{text.approved}</p>
            )}
            {result === "denied" && (
              <p className="mt-4 text-[13px] text-warning">{text.denied}</p>
            )}

            {!result && session && (
              <div className="mt-5 flex flex-wrap gap-3">
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void decide("approve")}
                  className="rounded-full bg-accent-solid px-5 py-2.5 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover disabled:opacity-60"
                >
                  {text.approve}
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void decide("deny")}
                  className="rounded-full border border-line-strong px-5 py-2.5 text-[13px] text-body transition-colors hover:border-accent/50 hover:text-fg disabled:opacity-60"
                >
                  {text.deny}
                </button>
              </div>
            )}

            {!result && !session && providers && providers.length === 0 && (
              <p className="mt-5 text-[12.5px] text-warning">{text.noProvider}</p>
            )}

            {!result && !session && providers && providers.length > 0 && (
              <div className="mt-5 flex flex-wrap gap-3">
                {providers.map((provider) => (
                  <button
                    key={provider.id}
                    type="button"
                    disabled={busy}
                    onClick={() => void startProvider(provider.id)}
                    className="inline-flex items-center gap-2 rounded-full border border-line-strong px-5 py-2.5 text-[13px] text-body transition-colors hover:border-accent/50 hover:text-fg disabled:opacity-60"
                  >
                    {provider.id === "google" ? <GoogleMark /> : null}
                    {provider.id === "google" ? text.google : text.github}
                  </button>
                ))}
              </div>
            )}

            {busy && !session && <p className="mt-4 text-[12px] text-muted">{text.redirecting}</p>}
          </div>
        )}

        <Link
          to="/"
          className="mt-8 inline-block text-[12.5px] text-muted underline-offset-4 hover:text-fg hover:underline"
        >
          {text.back}
        </Link>
      </div>
    </div>
  );
}

function GoogleMark() {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
      <path
        fill="#4285F4"
        d="M23.5 12.3c0-.8-.1-1.6-.2-2.3H12v4.5h6.5a5.6 5.6 0 0 1-2.4 3.6v3h3.9c2.3-2.1 3.5-5.2 3.5-8.8Z"
      />
      <path
        fill="#34A853"
        d="M12 24c3.2 0 5.9-1.1 7.9-2.9l-3.9-3c-1.1.7-2.4 1.2-4 1.2-3.1 0-5.7-2.1-6.6-4.9H1.4v3.1A12 12 0 0 0 12 24Z"
      />
      <path fill="#FBBC05" d="M5.4 14.4a7.2 7.2 0 0 1 0-4.6V6.7H1.4a12 12 0 0 0 0 10.8l4-3.1Z" />
      <path
        fill="#EA4335"
        d="M12 4.8c1.8 0 3.4.6 4.6 1.8l3.4-3.4A12 12 0 0 0 1.4 6.7l4 3.1C6.3 6.9 8.9 4.8 12 4.8Z"
      />
    </svg>
  );
}
