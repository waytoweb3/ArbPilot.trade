import { useMemo, useState } from "react";
import { Store, TrendingUp, Wallet } from "lucide-react";
import { useNavigate } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import {
  useCancelSubscription,
  useDeleteStrategy,
  useImportPublication,
  useLeaderboard,
  usePublications,
  useStrategies,
  useSubscribe,
  useSubscriptions,
} from "../hooks/useApi";
import { useAccount } from "../hooks/useAccount";
import { formatDateTime, formatNumber, formatPercentValue, formatUsd } from "../lib/format";
import { BillingError, PAYMENTS_URL, createStrategyCheckout } from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import { openExternal } from "../lib/openExternal";
import { useSessionStore } from "../lib/session";
import type {
  PublicationStatus,
  RiskLevel,
  StrategyRecord,
  StrategySubscription,
  SubscriptionStatus,
} from "../lib/types";

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
  const [cancelTarget, setCancelTarget] = useState<{
    subscription: StrategySubscription;
    strategies: StrategyRecord[];
  } | null>(null);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);

  // 市场只展示公开可见的发布，私有草稿与作者管理都在「策略」和「设置」里。
  const publications = usePublications(false);
  const leaderboard = useLeaderboard();
  const subscriptions = useSubscriptions(subscriber);
  const strategies = useStrategies();

  const subscribe = useSubscribe();
  const cancelSubscription = useCancelSubscription();
  const importPublication = useImportPublication();
  const deleteStrategy = useDeleteStrategy();
  const navigate = useNavigate();

  /** 当前用户对某个发布的订阅，用于在列表内直接展示状态。 */
  const subscriptionFor = (publicationId: string) =>
    (subscriptions.data ?? []).find(
      (subscription) =>
        subscription.publication_id === publicationId &&
        subscription.subscriber_id === subscriber,
    );

  /**
   * 「我的策略」中由某次发布导入的策略。
   * 新数据带 source_publication_id；旧数据没有来源字段，用模块哈希与标题兜底。
   */
  const importedFrom = (subscription: StrategySubscription): StrategyRecord[] => {
    const publication = (publications.data ?? []).find(
      (item) => item.id === subscription.publication_id,
    );
    return (strategies.data ?? []).filter((strategy) => {
      if (strategy.source_publication_id) {
        return strategy.source_publication_id === subscription.publication_id;
      }
      return Boolean(
        publication?.artifact_sha256 &&
          strategy.sandbox?.artifact_hash === publication.artifact_sha256 &&
          strategy.definition.name === subscription.publication_title,
      );
    });
  };

  /** 退订前先清空由该策略导入的本机策略；删除完成后才真正取消订阅。 */
  const requestCancel = (subscription: StrategySubscription) => {
    const imported = importedFrom(subscription);
    setCancelError(null);
    if (!imported.length) {
      cancelSubscription.mutate(subscription.id);
      return;
    }
    setCancelTarget({ subscription, strategies: imported });
  };

  const confirmCancel = async () => {
    if (!cancelTarget) {
      return;
    }
    const blocked = cancelTarget.strategies.find(
      (strategy) => strategy.status === "running" || strategy.runtime.open,
    );
    if (blocked) {
      setCancelError(
        fmt(copy.cancelCleanupRunning, { name: blocked.definition.name }),
      );
      return;
    }
    setCancelling(true);
    setCancelError(null);
    try {
      for (const strategy of cancelTarget.strategies) {
        await deleteStrategy.mutateAsync(strategy.id);
      }
      await cancelSubscription.mutateAsync(cancelTarget.subscription.id);
      setCancelTarget(null);
    } catch (cause) {
      setCancelError(
        fmt(copy.cancelCleanupFailed, {
          message: cause instanceof Error ? cause.message : String(cause),
        }),
      );
    } finally {
      setCancelling(false);
    }
  };

  const error =
    messageOf(subscribe.error) ??
    messageOf(cancelSubscription.error) ??
    messageOf(importPublication.error);

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
      void openExternal(created.checkoutUrl);
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
                  <tr key={entry.publication_id} className="border-t border-line">
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
            publications.data.map((publication) => {
              const current = subscriptionFor(publication.id);
              const monthly = Number(publication.pricing.monthly_price_usd);
              return (
                <div
                  key={publication.id}
                  className="rounded-lg border border-line px-3 py-2.5"
                >
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="truncate text-sm font-medium text-fg">
                          {publication.title}
                        </span>
                        <Badge tone={statusTone[publication.status]}>
                          {statusLabel[publication.status]}
                        </Badge>
                        {current && (
                          <Badge tone={subscriptionTone[current.status]}>
                            {copy.subscriptionStatus[current.status]}
                          </Badge>
                        )}
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
                    </div>

                    <div className="shrink-0 space-y-1.5">
                      {!current && (
                        <Button
                          size="sm"
                          onClick={() =>
                            subscribe.mutate(
                              { publicationId: publication.id, subscriberId: subscriber },
                              {
                                onSuccess: (subscription) => {
                                  if (
                                    publication.pricing.model === "monthly" &&
                                    monthly > 0
                                  ) {
                                    void startCheckout(publication.id, subscription.id);
                                  }
                                },
                              },
                            )
                          }
                          disabled={publication.status !== "published" || subscribe.isPending}
                        >
                          {subscribe.isPending ? copy.subscribing : copy.subscribe}
                        </Button>
                      )}
                    </div>
                  </div>

                  {checkoutFor === publication.id && checkoutError && (
                    <p className="mt-1 text-[11px] leading-relaxed text-danger">{checkoutError}</p>
                  )}
                  {checkoutFor === publication.id && checkoutUrl && (
                    <a
                      href={checkoutUrl}
                      target="_blank"
                      rel="noreferrer"
                      className="mt-1 block truncate font-mono text-[10.5px] text-accent hover:underline"
                    >
                      {copy.openCheckout} · {checkoutUrl}
                    </a>
                  )}
                </div>
              );
            })
          ) : (
            <p className="py-4 text-xs text-muted">{copy.publicationsEmpty}</p>
          )}
          {!accountId && <p className="text-[11px] text-subtle">{copy.subscribeLoginHint}</p>}
        </CardContent>
      </Card>

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
            subscriptions.data.map((subscription) => {
              const publication = (publications.data ?? []).find(
                (item) => item.id === subscription.publication_id,
              );
              const imported = importedFrom(subscription);
              const entitled =
                subscription.status === "trial" || subscription.status === "active";
              return (
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
                  <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
                    <Badge tone={subscriptionTone[subscription.status]}>
                      {copy.subscriptionStatus[subscription.status]}
                    </Badge>
                    {entitled &&
                      (publication && !publication.artifact_sha256 ? (
                        <span className="max-w-[190px] text-right text-[10.5px] leading-snug text-subtle">
                          {copy.importUnavailable}
                        </span>
                      ) : imported.length ? (
                        <span className="text-[11px] text-muted">{copy.imported}</span>
                      ) : (
                        <Button
                          size="sm"
                          onClick={() =>
                            importPublication.mutate(
                              {
                                publicationId: subscription.publication_id,
                                subscriberId: subscriber,
                              },
                              {
                                // 导入后策略出现在自己的执行器，直接跳到策略页启动。
                                onSuccess: () => navigate("/strategies"),
                              },
                            )
                          }
                          disabled={importPublication.isPending}
                        >
                          {importPublication.isPending ? copy.importing : copy.importStrategy}
                        </Button>
                      ))}
                    {entitled && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => requestCancel(subscription)}
                        disabled={cancelSubscription.isPending || cancelling}
                      >
                        {copy.cancelSubscription}
                      </Button>
                    )}
                  </div>
                </div>
              );
            })
          ) : (
            <p className="text-xs text-muted">{copy.subscriptionsEmpty}</p>
          )}
        </CardContent>
      </Card>

      <ConfirmDialog
        open={cancelTarget !== null}
        title={copy.cancelCleanupTitle}
        message={
          cancelTarget
            ? fmt(copy.cancelCleanupMessage, {
                count: cancelTarget.strategies.length,
                names: cancelTarget.strategies
                  .map((strategy) => strategy.definition.name)
                  .join("、"),
              })
            : ""
        }
        confirmLabel={copy.cancelCleanupConfirm}
        cancelLabel={copy.cancelCleanupKeep}
        pending={cancelling}
        error={cancelError}
        onConfirm={() => void confirmCancel()}
        onCancel={() => {
          if (!cancelling) {
            setCancelTarget(null);
            setCancelError(null);
          }
        }}
      />
    </div>
  );
}
