import { useState } from "react";

import { useBillingPlans, useCreateCheckout, useLicense } from "../hooks/useBilling";
import {
  BillingError,
  getLicenseRef,
  PAYMENTS_URL,
  setLicenseRef,
  type BillingPlan,
  type BillingPlanId,
  type LicenseStatus,
} from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import { useTheme } from "../lib/theme";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

const FALLBACK_PLANS: BillingPlan[] = [
  {
    id: "arbitrage",
    amount: "49.00",
    billingPeriod: "monthly",
    name: { zh: "ArbPilot · Arbitrage & News", en: "ArbPilot · Arbitrage & News" },
  },
  {
    id: "strategy",
    amount: "99.00",
    billingPeriod: "monthly",
    name: { zh: "ArbPilot · Strategy", en: "ArbPilot · Strategy" },
  },
];

type Tone = "success" | "warning" | "danger" | "neutral" | "info";

const statusTones: Record<LicenseStatus, Tone> = {
  pending: "info",
  active: "success",
  canceling: "warning",
  canceled: "neutral",
  past_due: "danger",
  expired: "neutral",
  unknown: "neutral",
};

export function BillingCard() {
  const { t, locale } = useI18n();
  const { resolved } = useTheme();
  const [ref, setRef] = useState<string | null>(() => getLicenseRef());
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const plansQuery = useBillingPlans();
  const licenseQuery = useLicense(ref);
  const checkout = useCreateCheckout();

  const plans = plansQuery.data?.length ? plansQuery.data : FALLBACK_PLANS;
  const license = licenseQuery.data;
  const status: LicenseStatus = license?.status ?? (ref ? "pending" : "unknown");
  const statusLabel = t.settings.billingStatuses[status];

  const subscribe = (plan: BillingPlanId) => {
    setError(null);
    setCopied(false);
    checkout.mutate(
      { plan, locale, darkMode: resolved === "dark" },
      {
        onSuccess: ({ ref: nextRef, checkoutUrl: nextUrl }) => {
          setLicenseRef(nextRef);
          setRef(nextRef);
          setCheckoutUrl(nextUrl);
          window.open(nextUrl, "_blank", "noopener,noreferrer");
        },
        onError: (cause) => {
          if (cause instanceof BillingError && cause.status === 503) {
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

  const planLabel = (id: string | null | undefined) =>
    id === "arbitrage"
      ? t.settings.billingArbitrage
      : id === "strategy"
        ? t.settings.billingStrategy
        : "—";

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.billing}</CardTitle>
          <CardDescription>{t.settings.billingDesc}</CardDescription>
        </div>
        {ref && <Badge tone={statusTones[status]}>{statusLabel}</Badge>}
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

        {ref ? (
          <div className="rounded-xl border border-line bg-panel-soft px-4 py-3.5">
            <div className="flex items-center justify-between">
              <p className="text-[11px] uppercase tracking-[0.14em] text-subtle">
                {t.settings.billingStatus}
              </p>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void licenseQuery.refetch()}
                disabled={licenseQuery.isFetching}
              >
                {t.settings.billingRefresh}
              </Button>
            </div>
            <dl className="mt-3 grid grid-cols-3 gap-x-6 gap-y-3 text-xs">
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingPlan}</dt>
                <dd className="mt-0.5 text-body">{planLabel(license?.plan)}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingStatus}</dt>
                <dd className="mt-0.5 text-body">{statusLabel}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-muted">{t.settings.billingExpires}</dt>
                <dd className="mt-0.5 font-mono text-[11px] text-body">
                  {license?.expiresAt
                    ? new Date(license.expiresAt).toLocaleDateString(
                        locale === "zh" ? "zh-CN" : "en-US",
                      )
                    : "—"}
                </dd>
              </div>
              <div className="col-span-3">
                <dt className="text-[11px] text-muted">{t.settings.billingRef}</dt>
                <dd className="mt-0.5 break-all font-mono text-[10.5px] text-subtle">{ref}</dd>
              </div>
            </dl>
          </div>
        ) : (
          <p className="text-xs text-muted">{t.settings.billingNone}</p>
        )}

        <div className="grid gap-3 sm:grid-cols-2">
          {plans.map((plan) => {
            const featured = plan.id === "strategy";
            const label = planLabel(plan.id);
            return (
              <div
                key={plan.id}
                className={`flex flex-col justify-between rounded-xl border px-4 py-3.5 ${
                  featured ? "border-accent/30 bg-accent/[0.04]" : "border-line bg-panel-soft"
                }`}
              >
                <div className="flex items-baseline justify-between">
                  <span className="text-[11px] text-muted">{label}</span>
                  <span className="font-mono text-sm text-fg">${plan.amount}</span>
                </div>
                <Button
                  size="sm"
                  variant={featured ? "primary" : "secondary"}
                  className="mt-3"
                  onClick={() => subscribe(plan.id)}
                  disabled={checkout.isPending}
                >
                  {checkout.isPending && checkout.variables?.plan === plan.id
                    ? t.settings.billingOpening
                    : fmt(t.settings.billingSubscribe, { plan: label })}
                </Button>
              </div>
            );
          })}
        </div>

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
