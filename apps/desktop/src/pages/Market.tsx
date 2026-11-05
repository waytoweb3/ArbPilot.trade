import { useMemo, useState } from "react";
import { Store, TrendingUp, Wallet } from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import {
  useCancelSubscription,
  useLeaderboard,
  usePublicationPerformance,
  usePublications,
  useSubscribe,
  useSubscriptions,
} from "../hooks/useApi";
import { useAccount } from "../hooks/useAccount";
import { formatDateTime, formatNumber, formatPercentValue, formatUsd } from "../lib/format";
import { BillingError, PAYMENTS_URL, createStrategyCheckout } from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import { useSessionStore } from "../lib/session";
import type {
  PerformanceSource,
  PublicationStatus,
  RiskLevel,
  SubscriptionStatus,
} from "../lib/types";
import { cn } from "../lib/utils";

const SUBSCRIBER_KEY = "arbpilot.market.subscriber";

/** 未登录时用本机匿名身份；登录后改用账号 ID，购买记录可跨设备同步。 */
function localSubscriberId(): string {
  const existing = localStorage.getItem(SUBSCRIBER_KEY);
  if (existing) {
    return existing;
  }
  const generated = crypto.randomUUID();
  localStorage.setItem(SUBSCRIBER_KEY, generated);
  return generated;
}

function messageOf(error: unknown): string | null {
  if (!error) {
    return null;
  }
  return error instanceof Error ? error.message : String(error);
}

export default function MarketPage() {
  const { t, locale } = useI18n();
  const copy = t.market;
  const account = useAccount();
  const accountId = account.data?.user.id;
  const session = useSessionStore((state) => state.token);
  const subscriber = useMemo(
    () => accountId ?? localSubscriberId(),
    [accountId],
  );
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [checkoutError, setCheckoutError] = useState<string | null>(null);
  const [checkoutFor, setCheckoutFor] = useState<string | null>(null);

  // 市场只展示公开可见的发布，私有草稿与作者管理都在「策略」和「设置」里。
  const publications = usePublications(false);
  const leaderboard = useLeaderboard();
  const subscriptions = useSubscriptions(subscriber);

  const subscribe = useSubscribe();
  const cancelSubscription = useCancelSubscription();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const performance = usePublicationPerformance(selectedId);

  const selected = publications.data?.find((publication) => publication.id === selectedId) ?? null;
  const currentSubscription = selected
    ? (subscriptions.data ?? []).find(
        (subscription) =>
          subscription.publication_id === selected.id && subscription.subscriber_id === subscriber,
      )
    : undefined;

  const error = messageOf(subscribe.error) ?? messageOf(cancelSubscription.error);

  /** 付费发布先创建待支付订阅，再拉起收银台；支付成功后由 webhook 回写授权。 */
  const startCheckout = async (publicationId: string, subscriptionId: string) => {
    setCheckoutFor(publicationId);
    setCheckoutUrl(null);
    setCheckoutError(null);
    // 订阅必须绑定登录账号，否则无法跨设备恢复购买。
    if (!session) {
      setCheckoutError(copy.checkoutLoginRequired);
      return;
    }
    try {
      const created = await createStrategyCheckout({
        publicationId,
        subscriberId: subscriber,
        subscriptionId,
        locale,
        session,
      });
      setCheckoutUrl(created.checkoutUrl);
      window.open(created.checkoutUrl, "_blank", "noopener,noreferrer");
    } catch (cause) {
      if (cause instanceof BillingError && cause.status === 401) {
        setCheckoutError(copy.checkoutLoginRequired);
      } else if (cause instanceof BillingError && cause.status === 503) {
        setCheckoutError(copy.checkoutNeedsKey);
      } else if (cause instanceof BillingError && cause.status === 0) {
        setCheckoutError(fmt(copy.checkoutUnreachable, { url: PAYMENTS_URL }));
      } else {
        setCheckoutError(
          fmt(copy.checkoutFailed, {
            message: cause instanceof Error ? cause.message : String(cause),
          }),
        );
      }
    }
  };

  const riskLabel = (level: RiskLevel) =>
    level === "low" ? copy.riskLow : level === "medium" ? copy.riskMedium : copy.riskHigh;

  const statusLabel: Record<PublicationStatus, string> = {
    private: copy.statusPrivate,
    published: copy.statusPublished,
    withdrawn: copy.statusWithdrawn,
  };

  const statusTone: Record<PublicationStatus, "success" | "warning" | "neutral"> = {
    private: "neutral",
    published: "success",
    withdrawn: "warning",
  };

  const sourceLabel: Record<PerformanceSource, string> = {
    author_backtest: copy.sourceAuthorBacktest,
    platform_replay: copy.sourcePlatformReplay,
    authorized_live: copy.sourceLive,
  };

  const subscriptionTone: Record<SubscriptionStatus, "success" | "warning" | "info" | "neutral" | "danger"> = {
    pending_payment: "warning",
    trial: "info",
    active: "success",
    cancelled: "neutral",
    expired: "danger",
  };

  return (
    <div className="space-y-4">
      <header>
        <h1 className="text-lg font-semibold text-fg">{copy.title}</h1>
        <p className="mt-1 max-w-3xl text-xs text-muted">{copy.subtitle}</p>
      </header>

      {error && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {fmt(copy.actionFailed, { message: error })}
        </div>
      )}

      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <TrendingUp size={15} className="text-accent" />
            <div>
              <CardTitle>{copy.leaderboardTitle}</CardTitle>
              <CardDescription>{copy.leaderboardDesc}</CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent className="overflow-x-auto p-0">
          {leaderboard.data?.length ? (
            <table className="w-full min-w-[900px] border-collapse text-sm">
              <thead>
                <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                  <th className="px-4 py-2 font-medium">{copy.rank}</th>
                  <th className="px-4 py-2 font-medium">{copy.strategy}</th>
                  <th className="px-4 py-2 font-medium">{copy.author}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.score}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.reproducibility}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.sample}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.stability}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.transparency}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.fields.returnPct}</th>
                </tr>
              </thead>
              <tbody>
                {leaderboard.data.map((entry, index) => (
                  <tr
                    key={entry.publication_id}
                    className={cn(
                      "cursor-pointer border-t border-line hover:bg-hover",
                      entry.publication_id === selectedId && "bg-accent/5",
                    )}
                    onClick={() => setSelectedId(entry.publication_id)}
                  >
                    <td className="px-4 py-2.5 text-muted">{index + 1}</td>
                    <td className="px-4 py-2.5">
                      <p className="font-medium text-fg">{entry.title}</p>
                      <p className="text-[11px] text-subtle">
                        v{entry.strategy_version} · {riskLabel(entry.risk_level)} ·{" "}
                        {fmt(copy.totalTrades, { count: entry.total_trades })}
                      </p>
                    </td>
                    <td className="px-4 py-2.5 text-body">{entry.author_name}</td>
                    <td className="px-4 py-2.5 text-right font-medium text-fg">
                      {formatNumber(entry.composite_score, 1)}
                    </td>
                    <td className="px-4 py-2.5 text-right text-muted">
                      {formatNumber(entry.reproducibility_score, 1)}
                    </td>
                    <td className="px-4 py-2.5 text-right text-muted">
                      {formatNumber(entry.sample_score, 1)}
                    </td>
                    <td className="px-4 py-2.5 text-right text-muted">
                      {formatNumber(entry.execution_stability_score, 1)}
                    </td>
                    <td className="px-4 py-2.5 text-right text-muted">
                      {formatNumber(entry.transparency_score, 1)}
                    </td>
                    <td className="px-4 py-2.5 text-right text-body">
                      {formatPercentValue(entry.return_pct)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <p className="px-4 py-6 text-xs text-muted">{copy.leaderboardEmpty}</p>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 xl:grid-cols-[1.05fr_0.95fr]">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Store size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.publicationsTitle}</CardTitle>
                <CardDescription>{copy.publicationsDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            {publications.data?.length ? (
              publications.data.map((publication) => (
                <button
                  key={publication.id}
                  type="button"
                  onClick={() => setSelectedId(publication.id)}
                  className={cn(
                    "w-full rounded-lg border px-3 py-2.5 text-left transition-colors",
                    publication.id === selectedId
                      ? "border-accent/50 bg-accent/5"
                      : "border-line hover:bg-hover",
                  )}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-sm font-medium text-fg">{publication.title}</span>
                    <Badge tone={statusTone[publication.status]}>
                      {statusLabel[publication.status]}
                    </Badge>
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted">
                    <span>{publication.author_name}</span>
                    <span>{fmt(copy.versionTag, { version: publication.strategy_version })}</span>
                    <span>{riskLabel(publication.risk_level)}</span>
                    <span>
                      {publication.pricing.model === "free"
                        ? copy.free
                        : fmt(copy.monthly, {
                            price: formatUsd(publication.pricing.monthly_price_usd),
                          })}
                    </span>
                  </div>
                  <p className="mt-1 line-clamp-2 text-xs text-subtle">{publication.summary}</p>
                </button>
              ))
            ) : (
              <p className="py-4 text-xs text-muted">{copy.publicationsEmpty}</p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{copy.detailTitle}</CardTitle>
            {selected && <Badge tone={statusTone[selected.status]}>{statusLabel[selected.status]}</Badge>}
          </CardHeader>
          <CardContent className="space-y-3">
            {selected ? (
              <>
                <div>
                  <p className="text-sm font-semibold text-fg">{selected.title}</p>
                  <p className="mt-0.5 text-xs text-muted">{selected.summary}</p>
                </div>

                <div className="flex flex-wrap gap-1.5">
                  <Badge tone="neutral">{riskLabel(selected.risk_level)}</Badge>
                  <Badge tone="neutral">
                    {selected.source_visibility === "open_source" ? copy.openSource : copy.blackBox}
                  </Badge>
                  <Badge tone="neutral">
                    {selected.visibility === "public" ? copy.visibilityPublic : copy.visibilityInvite}
                  </Badge>
                  <Badge tone="info">
                    {selected.pricing.model === "free"
                      ? copy.free
                      : fmt(copy.monthly, {
                          price: formatUsd(selected.pricing.monthly_price_usd),
                        })}
                  </Badge>
                  {selected.pricing.trial_days > 0 && (
                    <Badge tone="warning">
                      {fmt(copy.trialDays, { days: selected.pricing.trial_days })}
                    </Badge>
                  )}
                </div>

                <div className="space-y-1.5 text-xs">
                  <p className="text-muted">
                    {copy.suggestedNotional.replace(
                      "{value}",
                      formatUsd(selected.suggested_max_notional_usd),
                    )}
                  </p>
                  <p className="text-muted">
                    {copy.runtime}: <span className="text-body">{selected.runtime}</span>
                  </p>
                  <p className="text-muted">
                    {copy.supportedVenues}:{" "}
                    <span className="text-body">{selected.supported_venues.join(" · ")}</span>
                  </p>
                  <p className="text-muted">
                    {copy.supportedSymbols}:{" "}
                    <span className="text-body">{selected.supported_symbols.join(" · ")}</span>
                  </p>
                  <p className="text-muted">
                    {copy.dataSources}:{" "}
                    <span className="text-body">{selected.data_sources.join(" · ")}</span>
                  </p>
                </div>

                <div>
                  <p className="text-xs font-medium text-fg">{copy.logicTitle}</p>
                  <p className="mt-1 whitespace-pre-wrap text-xs text-muted">{selected.logic_summary}</p>
                </div>

                {selected.known_limits.length > 0 && (
                  <div>
                    <p className="text-xs font-medium text-fg">{copy.knownLimits}</p>
                    <ul className="mt-1 list-disc space-y-0.5 pl-4 text-xs text-muted">
                      {selected.known_limits.map((limit) => (
                        <li key={limit}>{limit}</li>
                      ))}
                    </ul>
                  </div>
                )}

                <div>
                  <p className="text-xs font-medium text-fg">{copy.changelog}</p>
                  <ul className="mt-1 space-y-0.5 text-[11px] text-muted">
                    {selected.changelog.map((entry) => (
                      <li key={`${entry.version}-${entry.at}`}>
                        v{entry.version} · {formatDateTime(entry.at)} · {entry.summary}
                      </li>
                    ))}
                  </ul>
                </div>

                <div className="flex flex-wrap items-center gap-2 border-t border-line pt-3">
                  {currentSubscription ? (
                    <>
                      <Badge tone={subscriptionTone[currentSubscription.status]}>
                        {copy.subscriptionStatus[currentSubscription.status]}
                      </Badge>
                      {currentSubscription.trial_ends_at && (
                        <span className="text-[11px] text-muted">
                          {fmt(copy.subscriptionEntitled, {
                            value: formatDateTime(currentSubscription.trial_ends_at),
                          })}
                        </span>
                      )}
                      {currentSubscription.status !== "cancelled" && (
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={() => cancelSubscription.mutate(currentSubscription.id)}
                          disabled={cancelSubscription.isPending}
                        >
                          {copy.cancelSubscription}
                        </Button>
                      )}
                    </>
                  ) : (
                    <Button
                      size="sm"
                      onClick={() =>
                        subscribe.mutate(
                          { publicationId: selected.id, subscriberId: subscriber },
                          {
                            onSuccess: (subscription) => {
                              const price = Number(selected.pricing.monthly_price_usd);
                              if (selected.pricing.model === "monthly" && price > 0) {
                                void startCheckout(selected.id, subscription.id);
                              }
                            },
                          },
                        )
                      }
                      disabled={selected.status !== "published" || subscribe.isPending}
                    >
                      {subscribe.isPending ? copy.subscribing : copy.subscribe}
                    </Button>
                  )}
                </div>
                {!accountId && <p className="text-[11px] text-subtle">{copy.subscribeLoginHint}</p>}
                {selected.pricing.model === "monthly" && (
                  <div className="space-y-1">
                    <p className="text-[11px] text-subtle">{copy.subscribePendingHint}</p>
                    {checkoutError && checkoutFor === selected.id && (
                      <p className="text-[11px] leading-relaxed text-danger">{checkoutError}</p>
                    )}
                    {checkoutUrl && checkoutFor === selected.id && (
                      <a
                        href={checkoutUrl}
                        target="_blank"
                        rel="noreferrer"
                        className="block truncate font-mono text-[10.5px] text-accent hover:underline"
                      >
                        {copy.openCheckout} · {checkoutUrl}
                      </a>
                    )}
                  </div>
                )}

                <div className="border-t border-line pt-3">
                  <p className="text-xs font-medium text-fg">{copy.performanceTitle}</p>
                  <p className="mt-0.5 text-[11px] text-muted">{copy.performanceDesc}</p>
                  <div className="mt-2 space-y-1.5">
                    {performance.data?.length ? (
                      performance.data.map((record) => (
                        <div
                          key={record.id}
                          className="rounded-md border border-line bg-panel-soft px-2 py-1.5 text-[11px]"
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-body">
                              {sourceLabel[record.source]} · v{record.strategy_version}
                            </span>
                            <Badge tone={record.reproducible ? "success" : "neutral"}>
                              {record.reproducible ? copy.reproducible : copy.notReproducible}
                            </Badge>
                          </div>
                          <p className="mt-0.5 text-muted">
                            {fmt(copy.periodRange, {
                              start: formatDateTime(record.period_start),
                              end: formatDateTime(record.period_end),
                            })}
                          </p>
                          <p className="mt-0.5 text-muted">
                            {formatPercentValue(record.return_pct)} · {copy.fields.drawdown} -{" "}
                            {formatNumber(record.max_drawdown_pct, 2)}% ·{" "}
                            {fmt(copy.totalTrades, { count: record.trades })}
                          </p>
                        </div>
                      ))
                    ) : (
                      <p className="text-[11px] text-muted">{copy.performanceEmpty}</p>
                    )}
                  </div>
                </div>
              </>
            ) : (
              <p className="py-4 text-xs text-muted">{copy.publicationsEmpty}</p>
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <Wallet size={15} className="text-accent" />
            <div>
              <CardTitle>{copy.subscriptionsTitle}</CardTitle>
              <CardDescription>{copy.subscriptionsDesc}</CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-2">
          {subscriptions.data?.length ? (
            subscriptions.data.map((subscription) => (
              <div
                key={subscription.id}
                className="flex items-center justify-between gap-2 rounded-lg border border-line px-3 py-2"
              >
                <div className="min-w-0">
                  <p className="truncate text-xs font-medium text-fg">
                    {subscription.publication_title}
                  </p>
                  <p className="text-[11px] text-muted">
                    {formatUsd(subscription.monthly_price_usd)} ·{" "}
                    {subscription.current_period_end
                      ? fmt(copy.subscriptionEntitled, {
                          value: formatDateTime(subscription.current_period_end),
                        })
                      : formatDateTime(subscription.updated_at)}
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <Badge tone={subscriptionTone[subscription.status]}>
                    {copy.subscriptionStatus[subscription.status]}
                  </Badge>
                  {subscription.status !== "cancelled" && (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => cancelSubscription.mutate(subscription.id)}
                      disabled={cancelSubscription.isPending}
                    >
                      {copy.cancelSubscription}
                    </Button>
                  )}
                </div>
              </div>
            ))
          ) : (
            <p className="text-xs text-muted">{copy.subscriptionsEmpty}</p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
