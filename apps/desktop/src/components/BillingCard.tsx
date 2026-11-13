import { useEffect, useState } from "react";

import {
  useAccount,
  useAccountSubscriptions,
  useCancelAccountSubscription,
  useExecutorLicense,
  useLogout,
  useSyncLicense,
} from "../hooks/useAccount";
import { useBillingPlans, useCreateCheckout } from "../hooks/useBilling";
import { useAccountDialog } from "../lib/accountDialog";
import {
  BillingError,
  PAYMENTS_URL,
  getAuthorWallet,
  saveAuthorWallet,
  type BillingPlan,
  type BillingPlanId,
  type CryptoPayCurrency,
} from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import { openExternal } from "../lib/openExternal";
import { useSessionStore } from "../lib/session";
import { useTheme } from "../lib/theme";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

const FALLBACK_PLANS: BillingPlan[] = [
  {
    id: "arbitrage",
    amount: "19.00",
    billingPeriod: "monthly",
    name: {
      zh: "ArbPilot · 自托管部署",
      en: "ArbPilot · Self-hosted Deployment",
    },
  },
];

type Tone = "success" | "warning" | "danger" | "neutral" | "info";

const statusTones: Record<string, Tone> = {
  pending: "info",
  active: "success",
  canceling: "warning",
  canceled: "neutral",
  past_due: "danger",
  expired: "neutral",
};

export function BillingCard() {
  const { t, locale } = useI18n();
  const { resolved } = useTheme();
  const token = useSessionStore((state) => state.token);
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [walletCurrency, setWalletCurrency] = useState<CryptoPayCurrency>("usdttrc20");
  const [walletAddress, setWalletAddress] = useState("");
  const [walletStatus, setWalletStatus] = useState<string | null>(null);
  const [walletError, setWalletError] = useState<string | null>(null);
  const [walletSaving, setWalletSaving] = useState(false);

  const account = useAccount();
  const subscriptions = useAccountSubscriptions();
  const cancelSubscription = useCancelAccountSubscription();
  const licenseQuery = useExecutorLicense();
  const syncLicense = useSyncLicense();
  const plansQuery = useBillingPlans();
  const checkout = useCreateCheckout();
  const logout = useLogout();
  const openAccountDialog = useAccountDialog((state) => state.show);

  /** 退出当前账号后立刻开始新的设备登录，浏览器里换另一个账号确认即可。 */
  const switchAccount = async () => {
    try {
      await logout.mutateAsync();
    } catch {
      // 账号服务不可达时本地令牌已清除，仍然允许重新登录。
    }
    openAccountDialog(true);
  };

  const plans = plansQuery.data?.length ? plansQuery.data : FALLBACK_PLANS;
  const me = account.data;
  const entitlement = me?.entitlement;
  const license = licenseQuery.data;
  const activePlans = entitlement?.plans ?? [];
  const dateText = (value: string | null | undefined) =>
    value ? new Date(value).toLocaleDateString(locale === "zh" ? "zh-CN" : "en-US") : "—";
  const secondsText = (value: number | null | undefined) =>
    value ? new Date(value * 1000).toLocaleDateString(locale === "zh" ? "zh-CN" : "en-US") : "—";

  const planLabel = (id: string | null | undefined) =>
    id === "arbitrage" ? t.settings.billingArbitrage : t.settings.accountPlanNone;
  const activePlanText = activePlans.length
    ? activePlans.map(planLabel).join(" · ")
    : t.settings.accountPlanNone;

  const entitlementStatus =
    entitlement && entitlement.status !== "none"
      ? t.settings.billingStatuses[entitlement.status]
      : t.settings.accountPlanNone;

  const licenseText = !license
    ? "—"
    : !license.installed
      ? t.settings.billingLicenseMissing
      : license.in_grace
        ? fmt(t.settings.billingLicenseGrace, { value: secondsText(license.grace_until) })
        : license.valid
          ? fmt(t.settings.billingLicenseValid, { value: secondsText(license.expires_at) })
          : t.settings.billingLicenseExpired;

  const subscribe = (plan: BillingPlanId) => {
    setError(null);
    setCopied(false);
    if (!token) {
      setError(t.settings.billingRequireLogin);
      return;
    }
    checkout.mutate(
      { plan, locale, darkMode: resolved === "dark", session: token },
      {
        onSuccess: ({ checkoutUrl: nextUrl }) => {
          setCheckoutUrl(nextUrl);
          void openExternal(nextUrl);
        },
        onError: (cause) => {
          if (cause instanceof BillingError && cause.status === 401) {
            setError(t.settings.billingRequireLogin);
          } else if (cause instanceof BillingError && cause.status === 503) {
            setError(t.settings.billingNeedsKey);
          } else if (cause instanceof BillingError && cause.status === 0) {
            setError(fmt(t.settings.billingUnreachable, { url: PAYMENTS_URL }));
          } else {
            setError(fmt(t.settings.billingFailed, { message: cause.message }));
          }
        },
      },
    );
  };

  const copy = async () => {
    if (!checkoutUrl) {
      return;
    }
    try {
      await navigator.clipboard.writeText(checkoutUrl);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2_000);
    } catch {
      setCopied(false);
    }
  };

  const cancelError = cancelSubscription.error
    ? fmt(t.settings.accountSubFailed, {
        message:
          cancelSubscription.error instanceof Error
            ? cancelSubscription.error.message
            : String(cancelSubscription.error),
      })
    : null;

  useEffect(() => {
    if (!token) {
      setWalletAddress("");
      return;
    }
    let cancelled = false;
    getAuthorWallet(token)
      .then((wallet) => {
        if (cancelled || !wallet) {
          return;
        }
        setWalletCurrency(wallet.currency);
        setWalletAddress(wallet.address);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [token]);

  const submitWallet = async () => {
    if (!token) {
      return;
    }
    setWalletStatus(null);
    setWalletError(null);
    setWalletSaving(true);
    try {
      const wallet = await saveAuthorWallet({
        session: token,
        currency: walletCurrency,
        address: walletAddress,
      });
      setWalletAddress(wallet.address);
      setWalletStatus(t.settings.cryptoWalletSaved);
    } catch (cause) {
      setWalletError(
        cause instanceof BillingError && cause.status === 400
          ? t.settings.cryptoWalletInvalid
          : cause instanceof Error
            ? cause.message
            : String(cause),
      );
    } finally {
      setWalletSaving(false);
    }
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.billing}</CardTitle>
          <CardDescription>{t.settings.billingDesc}</CardDescription>
        </div>
        {token && (
          <Badge tone={activePlans.length ? "success" : "neutral"}>{activePlanText}</Badge>
        )}
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
          <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
            {t.settings.billingSelfHostedTitle}
          </p>
          <ul className="mt-2 space-y-1.5 text-[11.5px] leading-relaxed text-muted">
            {t.settings.billingSelfHosted.map((item) => (
              <li key={item} className="flex gap-2">
                <span className="mt-[6px] h-1 w-1 shrink-0 rounded-full bg-accent-solid" />
                {item}
              </li>
            ))}
          </ul>
        </div>

        <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
          <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
            {t.settings.billingLoginTitle}
          </p>
          <ul className="mt-2 space-y-1.5 text-[11.5px] leading-relaxed text-muted">
            {t.settings.billingLoginReasons.map((item) => (
              <li key={item} className="flex gap-2">
                <span className="mt-[6px] h-1 w-1 shrink-0 rounded-full bg-accent-solid" />
                {item}
              </li>
            ))}
          </ul>
        </div>

        {token ? (
          <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="min-w-0">
                <p className="truncate text-xs font-medium text-fg">
                  {me?.user.name ?? t.settings.accountSignedIn}
                </p>
                {me?.user.email && (
                  <p className="mt-0.5 truncate text-[11px] text-subtle">{me.user.email}</p>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1">
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void switchAccount()}
                  disabled={logout.isPending}
                >
                  {t.settings.accountSwitch}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => logout.mutate()}
                  disabled={logout.isPending}
                >
                  {t.settings.accountLogout}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void account.refetch()}
                  disabled={account.isFetching}
                >
                  {t.settings.accountRefresh}
                </Button>
              </div>
            </div>
            <dl className="mt-3 grid grid-cols-3 gap-x-6 gap-y-3 text-xs">
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingPlan}</dt>
                <dd className="mt-0.5 text-body">{activePlanText}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingStatus}</dt>
                <dd className="mt-0.5 text-body">{entitlementStatus}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingExpires}</dt>
                <dd className="mt-0.5 font-mono text-[11px] text-body">
                  {dateText(entitlement?.expiresAt)}
                </dd>
              </div>
            </dl>
          </div>
        ) : (
          <p className="rounded-xl border border-line bg-panel-soft px-4 py-3 text-[11.5px] leading-relaxed text-muted">
            {t.settings.billingLoginPrompt}
          </p>
        )}

        <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
          <div className="flex items-center justify-between">
            <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
              {t.settings.billingLicenseTitle}
            </p>
            <div className="flex items-center gap-2">
              {license && (
                <Badge tone={license.valid ? "success" : license.installed ? "warning" : "neutral"}>
                  {license.required
                    ? t.settings.billingLicenseEnforced
                    : t.settings.billingLicenseOptional}
                </Badge>
              )}
              <Button
                size="sm"
                variant="ghost"
                onClick={() => syncLicense.mutate(token)}
                disabled={syncLicense.isPending || licenseQuery.isFetching}
              >
                {syncLicense.isPending
                  ? t.settings.billingLicenseSyncing
                  : t.settings.billingLicenseSync}
              </Button>
            </div>
          </div>
          <dl className="mt-3 grid grid-cols-3 gap-x-6 gap-y-3 text-xs">
            <div>
              <dt className="text-[11px] text-muted">{t.settings.billingStatus}</dt>
              <dd className="mt-0.5 text-body">{licenseText}</dd>
            </div>
            <div>
              <dt className="text-[11px] text-muted">{t.settings.billingPlan}</dt>
              <dd className="mt-0.5 text-body">
                {license?.plans.length ? license.plans.map(planLabel).join(" · ") : "—"}
              </dd>
            </div>
            <div>
              <dt className="text-[11px] text-muted">{t.settings.accountSignedIn}</dt>
              <dd className="mt-0.5 truncate font-mono text-[11px] text-body">
                {license?.email ?? "—"}
              </dd>
            </div>
          </dl>
          <p className="mt-3 text-[11px] leading-relaxed text-subtle">
            {t.settings.billingLicenseSyncDesc}
          </p>
          {license?.error && <p className="mt-2 text-[11px] text-danger">{license.error}</p>}
        </div>

        <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
          <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
            {t.settings.accountSubscriptions}
          </p>
          {subscriptions.data?.subscriptions.length ? (
            <ul className="mt-2 divide-y divide-line text-xs">
              {subscriptions.data.subscriptions.map((item) => (
                <li key={item.id} className="flex items-center justify-between gap-3 py-2">
                  <div className="min-w-0">
                    <p className="truncate text-body">{planLabel(item.plan)}</p>
                    <p className="mt-0.5 text-[11px] text-subtle">
                      {t.settings.accountExpires} {dateText(item.currentPeriodEnd)}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Badge tone={statusTones[item.status] ?? "neutral"}>
                      {t.settings.billingStatuses[
                        item.status as keyof typeof t.settings.billingStatuses
                      ] ?? item.status}
                    </Badge>
                    {item.status === "active" && !item.cancelAtPeriodEnd && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => cancelSubscription.mutate(item.id)}
                        disabled={cancelSubscription.isPending}
                      >
                        {cancelSubscription.isPending
                          ? t.settings.accountSubCanceling
                          : t.settings.accountSubCancel}
                      </Button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-2 text-xs text-muted">{t.settings.accountSubEmpty}</p>
          )}
          {cancelError && <p className="mt-2 text-[11px] text-danger">{cancelError}</p>}
        </div>

        <div className="grid gap-3">
          {plans.map((item) => {
            const label = planLabel(item.id);
            return (
              <div
                key={item.id}
                className="flex flex-col justify-between rounded-xl border border-accent/30 bg-accent/[0.04] px-4 py-3.5"
              >
                <div className="flex items-baseline justify-between">
                  <span className="text-[11px] text-muted">{label}</span>
                  <span className="font-mono text-sm text-fg">${item.amount}</span>
                </div>
                <Button
                  size="sm"
                  variant="primary"
                  className="mt-3"
                  onClick={() => subscribe(item.id)}
                  disabled={checkout.isPending}
                >
                  {checkout.isPending && checkout.variables?.plan === item.id
                    ? t.settings.billingOpening
                    : fmt(t.settings.billingSubscribe, { plan: label })}
                </Button>
              </div>
            );
          })}
        </div>

        <p className="text-[11px] leading-relaxed text-muted">{t.settings.billingMarketNote}</p>

        {token && (
          <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
            <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
              {t.settings.cryptoWalletTitle}
            </p>
            <p className="mt-1 text-[11px] leading-relaxed text-muted">
              {t.settings.cryptoWalletDesc}
            </p>
            <div className="mt-2.5 flex flex-wrap items-center gap-2">
              <select
                value={walletCurrency}
                onChange={(event) => setWalletCurrency(event.target.value as CryptoPayCurrency)}
                className="h-8 rounded-lg border border-line bg-canvas px-2 text-[11px] text-fg outline-none focus:border-accent"
              >
                <option value="usdttrc20">USDT · TRC20</option>
                <option value="usdc">USDC · ERC20</option>
              </select>
              <input
                value={walletAddress}
                onChange={(event) => setWalletAddress(event.target.value)}
                placeholder={t.settings.cryptoWalletAddress}
                spellCheck={false}
                className="h-8 min-w-0 flex-1 rounded-lg border border-line bg-canvas px-2 font-mono text-[11px] text-fg outline-none placeholder:text-subtle focus:border-accent"
              />
              <Button
                size="sm"
                variant="secondary"
                onClick={() => void submitWallet()}
                disabled={walletSaving || !walletAddress.trim()}
              >
                {walletSaving ? t.settings.cryptoWalletSaving : t.settings.cryptoWalletSave}
              </Button>
            </div>
            {walletStatus && <p className="mt-2 text-[11px] text-success">{walletStatus}</p>}
            {walletError && <p className="mt-2 text-[11px] text-danger">{walletError}</p>}
          </div>
        )}

        {checkoutUrl && (
          <div className="rounded-xl border border-line bg-panel-soft px-4 py-3">
            <p className="text-[11px] text-muted">{t.settings.billingOpenManual}</p>
            <div className="mt-2 flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-subtle">
                {checkoutUrl}
              </code>
              <Button size="sm" variant="secondary" onClick={() => void copy()}>
                {copied ? t.settings.billingCopied : t.settings.billingCopy}
              </Button>
            </div>
          </div>
        )}

        {error && (
          <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-[11px] leading-relaxed text-danger">
            {error}
          </p>
        )}

        <p className="text-[11px] leading-relaxed text-muted">{t.settings.billingNote}</p>
      </CardContent>
    </Card>
  );
}
