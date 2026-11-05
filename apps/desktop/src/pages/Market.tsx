import { useMemo, useState } from "react";
import { Plus, Store, TrendingUp, Users, Wallet } from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import {
  useCancelSubscription,
  useCreateMarketAuthor,
  useCreatePerformance,
  useCreatePublication,
  useCreateSettlement,
  useLeaderboard,
  useMarketAuthors,
  usePublicationPerformance,
  usePublications,
  useSettlements,
  useStrategies,
  useSubscribe,
  useSubscriptions,
  useUpdatePublication,
  useUpdateSettlement,
} from "../hooks/useApi";
import { formatDateTime, formatNumber, formatPercentValue, formatUsd } from "../lib/format";
import { BillingError, PAYMENTS_URL, createStrategyCheckout } from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import type {
  MarketVisibility,
  PerformanceSource,
  PricingModel,
  PublicationStatus,
  RiskLevel,
  SettlementStatus,
  SourceVisibility,
  SubscriptionStatus,
} from "../lib/types";
import { cn } from "../lib/utils";

const inputClass =
  "w-full rounded-md border border-line bg-panel-soft px-2 py-1 text-xs text-body outline-none focus:border-accent/60";

const SUBSCRIBER_KEY = "arbpilot.market.subscriber";

/** 订阅者身份只保存在本机，不上传任何账号或支付凭证。 */
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
  const subscriber = useMemo(localSubscriberId, []);
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [checkoutError, setCheckoutError] = useState<string | null>(null);
  const [checkoutFor, setCheckoutFor] = useState<string | null>(null);

  const authors = useMarketAuthors();
  const strategies = useStrategies();
  const [includeHidden, setIncludeHidden] = useState(false);
  const publications = usePublications(includeHidden);
  const leaderboard = useLeaderboard();
  const subscriptions = useSubscriptions(subscriber);

  const createAuthor = useCreateMarketAuthor();
  const createPublication = useCreatePublication();
  const updatePublication = useUpdatePublication();
  const createPerformance = useCreatePerformance();
  const subscribe = useSubscribe();
  const cancelSubscription = useCancelSubscription();
  const createSettlement = useCreateSettlement();
  const updateSettlement = useUpdateSettlement();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const performance = usePublicationPerformance(selectedId);

  const [authorId, setAuthorId] = useState("");
  const activeAuthorId = authorId || authors.data?.[0]?.author_id || "";
  const settlements = useSettlements(activeAuthorId || undefined);

  const [authorForm, setAuthorForm] = useState({
    display_name: "",
    headline: "",
    bio: "",
    payout_account: "",
  });

  const [draft, setDraft] = useState({
    strategy_id: "",
    title: "",
    summary: "",
    logic: "",
    risk: "medium" as RiskLevel,
    notional: "1000",
    visibility: "invite_only" as MarketVisibility,
    sourceVisibility: "open_source" as SourceVisibility,
    pricing: "monthly" as PricingModel,
    price: "19",
    trial: "7",
    limits: "",
  });

  const [perfForm, setPerfForm] = useState({
    source: "author_backtest" as PerformanceSource,
    trades: "30",
    sample: "30",
    winRate: "55",
    returnPct: "4",
    drawdown: "3",
    netPnl: "120",
  });

  const selected = publications.data?.find((publication) => publication.id === selectedId) ?? null;
  const currentSubscription = selected
    ? (subscriptions.data ?? []).find(
        (subscription) =>
          subscription.publication_id === selected.id && subscription.subscriber_id === subscriber,
      )
    : undefined;

  const error =
    messageOf(createAuthor.error) ??
    messageOf(createPublication.error) ??
    messageOf(updatePublication.error) ??
    messageOf(createPerformance.error) ??
    messageOf(subscribe.error) ??
    messageOf(cancelSubscription.error) ??
    messageOf(createSettlement.error) ??
    messageOf(updateSettlement.error);

  /** 付费发布先创建待支付订阅，再拉起收银台；支付成功后由 webhook 回写授权。 */
  const startCheckout = async (publicationId: string, subscriptionId: string) => {
    setCheckoutFor(publicationId);
    setCheckoutUrl(null);
    setCheckoutError(null);
    try {
      const session = await createStrategyCheckout({
        publicationId,
        subscriberId: subscriber,
        subscriptionId,
        locale,
      });
      setCheckoutUrl(session.checkoutUrl);
      window.open(session.checkoutUrl, "_blank", "noopener,noreferrer");
    } catch (cause) {
      if (cause instanceof BillingError && cause.status === 503) {
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

  const settlementTone: Record<SettlementStatus, "success" | "warning" | "danger"> = {
    pending: "warning",
    paid: "success",
    disputed: "danger",
  };

  const settlementLabel: Record<SettlementStatus, string> = {
    pending: copy.statusPending,
    paid: copy.statusPaid,
    disputed: copy.statusDisputed,
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
            <label className="flex shrink-0 items-center gap-1.5 text-xs text-muted">
              <input
                type="checkbox"
                checked={includeHidden}
                onChange={(event) => setIncludeHidden(event.target.checked)}
              />
              {copy.showHidden}
            </label>
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
            <CardTitle>{copy.publicationsTitle}</CardTitle>
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
                  {selected.status !== "published" ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() =>
                        updatePublication.mutate({ id: selected.id, body: { action: "publish" } })
                      }
                      disabled={updatePublication.isPending}
                    >
                      {copy.publish}
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="danger"
                      onClick={() =>
                        updatePublication.mutate({ id: selected.id, body: { action: "withdraw" } })
                      }
                      disabled={updatePublication.isPending}
                    >
                      {copy.withdraw}
                    </Button>
                  )}
                </div>
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

                  <div className="mt-3 grid grid-cols-3 gap-2">
                    <label className="col-span-3 text-[11px] text-muted">
                      {copy.addPerformance}
                      <select
                        className={inputClass}
                        value={perfForm.source}
                        onChange={(event) =>
                          setPerfForm({ ...perfForm, source: event.target.value as PerformanceSource })
                        }
                      >
                        <option value="author_backtest">{copy.sourceAuthorBacktest}</option>
                        <option value="platform_replay">{copy.sourcePlatformReplay}</option>
                      </select>
                    </label>
                    {(
                      [
                        ["trades", copy.fields.trades],
                        ["sample", copy.fields.sample],
                        ["winRate", copy.fields.winRate],
                        ["returnPct", copy.fields.returnPct],
                        ["drawdown", copy.fields.drawdown],
                        ["netPnl", copy.fields.netPnl],
                      ] as const
                    ).map(([key, label]) => (
                      <label key={key} className="text-[11px] text-muted">
                        {label}
                        <input
                          className={inputClass}
                          value={perfForm[key]}
                          onChange={(event) =>
                            setPerfForm({ ...perfForm, [key]: event.target.value })
                          }
                        />
                      </label>
                    ))}
                  </div>
                  <Button
                    size="sm"
                    variant="secondary"
                    className="mt-2"
                    disabled={createPerformance.isPending}
                    onClick={() => {
                      const now = new Date();
                      const start = new Date(now.getTime() - 30 * 24 * 3_600_000);
                      createPerformance.mutate({
                        id: selected.id,
                        body: {
                          source: perfForm.source,
                          period_start: start.toISOString(),
                          period_end: now.toISOString(),
                          trades: Number(perfForm.trades) || 0,
                          win_rate_pct: Number(perfForm.winRate) || 0,
                          net_pnl_usd: Number(perfForm.netPnl) || 0,
                          return_pct: Number(perfForm.returnPct) || 0,
                          max_drawdown_pct: Number(perfForm.drawdown) || 0,
                          fee_bps_per_leg: 4,
                          slippage_bps: 1,
                          sample_size: Number(perfForm.sample) || 0,
                        },
                      });
                    }}
                  >
                    {copy.addPerformance}
                  </Button>
                </div>
              </>
            ) : (
              <p className="py-4 text-xs text-muted">{copy.publicationsEmpty}</p>
            )}
          </CardContent>
        </Card>
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Users size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.authorsTitle}</CardTitle>
                <CardDescription>{copy.authorsDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-3">
            {authors.data?.length ? (
              <div className="flex flex-wrap gap-2">
                {authors.data.map((author) => (
                  <button
                    key={author.author_id}
                    type="button"
                    onClick={() => setAuthorId(author.author_id)}
                    className={cn(
                      "rounded-lg border px-3 py-2 text-left text-xs transition-colors",
                      author.author_id === activeAuthorId
                        ? "border-accent/50 bg-accent/5"
                        : "border-line hover:bg-hover",
                    )}
                  >
                    <span className="block font-medium text-fg">{author.display_name}</span>
                    {author.headline && (
                      <span className="block text-[11px] text-muted">{author.headline}</span>
                    )}
                  </button>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted">{copy.authorsEmpty}</p>
            )}

            <div className="grid gap-2 sm:grid-cols-2">
              <label className="text-[11px] text-muted">
                {copy.displayName}
                <input
                  className={inputClass}
                  value={authorForm.display_name}
                  onChange={(event) =>
                    setAuthorForm({ ...authorForm, display_name: event.target.value })
                  }
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.headline}
                <input
                  className={inputClass}
                  value={authorForm.headline}
                  onChange={(event) =>
                    setAuthorForm({ ...authorForm, headline: event.target.value })
                  }
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.bio}
                <textarea
                  className={cn(inputClass, "min-h-16")}
                  value={authorForm.bio}
                  onChange={(event) => setAuthorForm({ ...authorForm, bio: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.payout}
                <input
                  className={inputClass}
                  placeholder={copy.payoutHint}
                  value={authorForm.payout_account}
                  onChange={(event) =>
                    setAuthorForm({ ...authorForm, payout_account: event.target.value })
                  }
                />
              </label>
            </div>
            <Button
              size="sm"
              disabled={!authorForm.display_name.trim() || createAuthor.isPending}
              onClick={() =>
                createAuthor.mutate(
                  {
                    display_name: authorForm.display_name.trim(),
                    headline: authorForm.headline,
                    bio: authorForm.bio,
                    payout_account: authorForm.payout_account || undefined,
                  },
                  {
                    onSuccess: () => {
                      setAuthorForm({ display_name: "", headline: "", bio: "", payout_account: "" });
                    },
                  },
                )
              }
            >
              <Plus size={13} />
              {copy.saveAuthor}
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <CardTitle>{copy.publishTitle}</CardTitle>
              <CardDescription>{copy.publishDesc}</CardDescription>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="grid gap-2 sm:grid-cols-2">
              <label className="text-[11px] text-muted">
                {copy.strategy}
                <select
                  className={inputClass}
                  value={draft.strategy_id}
                  onChange={(event) => setDraft({ ...draft, strategy_id: event.target.value })}
                >
                  <option value="">—</option>
                  {(strategies.data ?? []).map((record) => (
                    <option key={record.id} value={record.id}>
                      {record.definition.name} · v{record.version}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.authorCard}
                <select
                  className={inputClass}
                  value={activeAuthorId}
                  onChange={(event) => setAuthorId(event.target.value)}
                >
                  {(authors.data ?? []).map((author) => (
                    <option key={author.author_id} value={author.author_id}>
                      {author.display_name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.titleLabel}
                <input
                  className={inputClass}
                  value={draft.title}
                  onChange={(event) => setDraft({ ...draft, title: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.summaryLabel}
                <input
                  className={inputClass}
                  value={draft.summary}
                  onChange={(event) => setDraft({ ...draft, summary: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.logicLabel}
                <textarea
                  className={cn(inputClass, "min-h-20")}
                  value={draft.logic}
                  onChange={(event) => setDraft({ ...draft, logic: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.riskLevel}
                <select
                  className={inputClass}
                  value={draft.risk}
                  onChange={(event) => setDraft({ ...draft, risk: event.target.value as RiskLevel })}
                >
                  <option value="low">{copy.riskLow}</option>
                  <option value="medium">{copy.riskMedium}</option>
                  <option value="high">{copy.riskHigh}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.suggestedNotionalLabel}
                <input
                  className={inputClass}
                  value={draft.notional}
                  onChange={(event) => setDraft({ ...draft, notional: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.visibilityLabel}
                <select
                  className={inputClass}
                  value={draft.visibility}
                  onChange={(event) =>
                    setDraft({ ...draft, visibility: event.target.value as MarketVisibility })
                  }
                >
                  <option value="invite_only">{copy.visibilityInvite}</option>
                  <option value="public">{copy.visibilityPublic}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.sourceVisibility}
                <select
                  className={inputClass}
                  value={draft.sourceVisibility}
                  onChange={(event) =>
                    setDraft({ ...draft, sourceVisibility: event.target.value as SourceVisibility })
                  }
                >
                  <option value="open_source">{copy.openSource}</option>
                  <option value="black_box">{copy.blackBox}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.pricingLabel}
                <select
                  className={inputClass}
                  value={draft.pricing}
                  onChange={(event) =>
                    setDraft({ ...draft, pricing: event.target.value as PricingModel })
                  }
                >
                  <option value="free">{copy.freePlan}</option>
                  <option value="monthly">{copy.monthlyPlan}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.priceLabel}
                <input
                  className={inputClass}
                  value={draft.price}
                  disabled={draft.pricing === "free"}
                  onChange={(event) => setDraft({ ...draft, price: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.trialLabel}
                <input
                  className={inputClass}
                  value={draft.trial}
                  onChange={(event) => setDraft({ ...draft, trial: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.knownLimitsLabel}
                <textarea
                  className={cn(inputClass, "min-h-16")}
                  value={draft.limits}
                  onChange={(event) => setDraft({ ...draft, limits: event.target.value })}
                />
              </label>
            </div>
            <Button
              size="sm"
              disabled={
                createPublication.isPending ||
                !draft.strategy_id ||
                !activeAuthorId ||
                !draft.title.trim() ||
                !draft.summary.trim() ||
                draft.logic.trim().length < 40
              }
              onClick={() => {
                createPublication.mutate(
                  {
                    strategy_id: draft.strategy_id,
                    author_id: activeAuthorId,
                    title: draft.title.trim(),
                    summary: draft.summary.trim(),
                    logic_summary: draft.logic.trim(),
                    risk_level: draft.risk,
                    suggested_max_notional_usd: Number(draft.notional) || 0,
                    known_limits: draft.limits
                      .split("\n")
                      .map((line) => line.trim())
                      .filter(Boolean),
                    source_visibility: draft.sourceVisibility,
                    visibility: draft.visibility,
                    pricing: {
                      model: draft.pricing,
                      monthly_price_usd:
                        draft.pricing === "free" ? 0 : Number(draft.price) || 0,
                      trial_days: draft.pricing === "free" ? 0 : Number(draft.trial) || 0,
                    },
                  },
                  {
                    onSuccess: (publication) => {
                      setIncludeHidden(true);
                      setSelectedId(publication.id);
                      setDraft({ ...draft, title: "", summary: "", logic: "", limits: "" });
                    },
                  },
                );
              }}
            >
              <Plus size={13} />
              {createPublication.isPending ? copy.creating : copy.createDraft}
            </Button>
          </CardContent>
        </Card>
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
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

        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Wallet size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.settlementsTitle}</CardTitle>
                <CardDescription>
                  {copy.settlementsDesc.replace(
                    "{bps}",
                    String(settlements.data?.[0]?.platform_fee_bps ?? 2000),
                  )}
                </CardDescription>
              </div>
            </div>
            <Button
              size="sm"
              variant="secondary"
              disabled={!activeAuthorId || createSettlement.isPending}
              onClick={() => activeAuthorId && createSettlement.mutate({ authorId: activeAuthorId })}
            >
              {copy.createSettlement}
            </Button>
          </CardHeader>
          <CardContent className="space-y-2">
            {settlements.data?.length ? (
              settlements.data.map((settlement) => (
                <div
                  key={settlement.id}
                  className="rounded-lg border border-line px-3 py-2 text-xs"
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-medium text-fg">
                      {settlement.period} ·{" "}
                      {fmt(copy.subscriptionCount, { count: settlement.subscription_count })}
                    </span>
                    <Badge tone={settlementTone[settlement.status]}>
                      {settlementLabel[settlement.status]}
                    </Badge>
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted">
                    <span>
                      {copy.gross} {formatUsd(settlement.gross_usd)}
                    </span>
                    <span>
                      {copy.platformFee} {formatUsd(settlement.platform_fee_usd)}
                    </span>
                    <span className="text-body">
                      {copy.net} {formatUsd(settlement.net_usd)}
                    </span>
                  </div>
                  <div className="mt-1.5 flex gap-1.5">
                    {settlement.status !== "paid" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          updateSettlement.mutate({ id: settlement.id, status: "paid" })
                        }
                      >
                        {copy.markPaid}
                      </Button>
                    )}
                    {settlement.status !== "disputed" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          updateSettlement.mutate({ id: settlement.id, status: "disputed" })
                        }
                      >
                        {copy.markDisputed}
                      </Button>
                    )}
                  </div>
                </div>
              ))
            ) : (
              <p className="text-xs text-muted">{copy.settlementsEmpty}</p>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
