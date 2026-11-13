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
import { formatDateTime, formatUsd } from "../lib/format";
import {
  BillingError,
  PAYMENTS_URL,
  createCryptoStrategyCheckout,
  createStrategyCheckout,
  type CryptoPayCurrency,
} from "../lib/billing";
import { fmt, useI18n } from "../lib/i18n";
import { openExternal } from "../lib/openExternal";
import { useSessionStore } from "../lib/session";
import type {
  ParamSpec,
  ParamValue,
  ParamValues,
  ParamsSchema,
  PublicationStatus,
  RiskLevel,
  StrategyRecord,
  StrategySubscription,
  SubscriptionStatus,
} from "../lib/types";

const SUBSCRIBER_KEY = "arbpilot.market.subscriber";

/** 参数表单的输入框样式；与策略页保持一致。 */
const inputClass =
  "w-full rounded-md border border-line bg-panel px-2 py-1 text-xs text-fg outline-none focus:border-accent";

/** 把一个参数值格式化成输入框文本；数字统一走字符串以保留精度。 */
function paramText(value: unknown): string {
  if (value === null || value === undefined) {
    return "";
  }
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** 按参数类型解析输入；解析失败返回 undefined，由调用方提示。 */
function parseParamValue(spec: ParamSpec, raw: string): unknown {
  const text = raw.trim();
  if (typeof spec.param_type === "object" && "enum" in spec.param_type) {
    return spec.param_type.enum.options.includes(text) ? text : undefined;
  }
  switch (spec.param_type) {
    case "boolean":
      return text === "true" ? true : text === "false" ? false : undefined;
    case "integer": {
      const number = Number(text);
      return Number.isInteger(number) ? number : undefined;
    }
    case "number":
    case "percent": {
      const number = Number(text);
      return Number.isFinite(number) ? text : undefined;
    }
    case "string_list": {
      try {
        const parsed = JSON.parse(text);
        return Array.isArray(parsed) ? parsed : undefined;
      } catch {
        return undefined;
      }
    }
    default:
      return text;
  }
}

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
  // Skill 版导入前先让买家填参数：表单按交付的参数空间渲染。
  const [paramTarget, setParamTarget] = useState<{
    subscription: StrategySubscription;
    schema: ParamsSchema;
  } | null>(null);
  const [paramDraft, setParamDraft] = useState<Record<string, string>>({});
  const [paramError, setParamError] = useState<string | null>(null);

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
      // 旧数据没有来源字段：沙箱发布按模块哈希匹配，技能发布按模板标识匹配。
      if (publication?.artifact_sha256) {
        return Boolean(
          strategy.sandbox?.artifact_hash === publication.artifact_sha256 &&
            strategy.definition.name === subscription.publication_title,
        );
      }
      return Boolean(
        publication?.skill?.template_key &&
          strategy.skill?.template_key === publication.skill.template_key &&
          strategy.definition.name === subscription.publication_title,
      );
    });
  };

  /** 提交买家参数并生成实例；成功后跳到策略页启动。 */
  const runImport = (
    publicationId: string,
    paramValues?: ParamValues,
  ) => {
    setParamError(null);
    importPublication.mutate(
      { publicationId, subscriberId: subscriber, paramValues },
      {
        onSuccess: () => {
          setParamTarget(null);
          navigate("/strategies");
        },
        onError: (error) => setParamError(messageOf(error)),
      },
    );
  };

  /** 有可改参数时先打开表单，否则直接导入（参数全锁死时等同原配置版）。 */
  const startImport = (subscription: StrategySubscription) => {
    const publication = (publications.data ?? []).find(
      (item) => item.id === subscription.publication_id,
    );
    const schema = publication?.skill?.params_schema ?? null;
    const editable = (schema?.params ?? []).filter((spec) => spec.editable);
    if (schema && editable.length) {
      setParamDraft(
        Object.fromEntries(
          editable.map((spec) => [`${spec.rule_key}/${spec.path}`, paramText(spec.default)]),
        ),
      );
      setParamError(null);
      setParamTarget({ subscription, schema });
      return;
    }
    runImport(subscription.publication_id);
  };

  /** 校验表单并按参数类型转成后端要的形态。 */
  const submitParams = () => {
    if (!paramTarget) {
      return;
    }
    const values: ParamValue[] = [];
    for (const spec of paramTarget.schema.params) {
      if (!spec.editable) {
        continue;
      }
      const raw = paramDraft[`${spec.rule_key}/${spec.path}`] ?? "";
      const parsed = parseParamValue(spec, raw);
      if (parsed === undefined) {
        setParamError(`${spec.label} 的取值不合法`);
        return;
      }
      values.push({ rule_key: spec.rule_key, path: spec.path, value: parsed });
    }
    runImport(paramTarget.subscription.publication_id, { values });
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

  /** 加密买断：一次付清买断价，确认后由 IPN 回写永久授权。 */
  const startCryptoCheckout = async (
    publicationId: string,
    subscriptionId: string,
    payCurrency: CryptoPayCurrency,
  ): Promise<boolean> => {
    setCheckoutFor(publicationId);
    setCheckoutUrl(null);
    setCheckoutError(null);
    if (!session) {
      setCheckoutError(copy.checkoutLoginRequired);
      return false;
    }
    try {
      const created = await createCryptoStrategyCheckout({
        publicationId,
        subscriberId: subscriber,
        subscriptionId,
        payCurrency,
        session,
      });
      setCheckoutUrl(created.invoiceUrl);
      void openExternal(created.invoiceUrl);
      return true;
    } catch (cause) {
      if (cause instanceof BillingError && cause.status === 401) {
        setCheckoutError(copy.checkoutLoginRequired);
      } else if (cause instanceof BillingError && cause.status === 503) {
        setCheckoutError(copy.cryptoNeedsKey);
      } else if (cause instanceof BillingError && cause.status === 0) {
        setCheckoutError(fmt(copy.checkoutUnreachable, { url: PAYMENTS_URL }));
      } else if (
        cause instanceof BillingError &&
        cause.message.includes("收款地址")
      ) {
        setCheckoutError(copy.cryptoAuthorWalletMissing);
      } else {
        setCheckoutError(
          fmt(copy.cryptoFailed, {
            message: cause instanceof Error ? cause.message : String(cause),
          }),
        );
      }
      return false;
    }
  };

  /** 加密购买失败时撤销刚创建的待支付订阅，避免按钮被隐藏。 */
  const buyCrypto = (publicationId: string, payCurrency: CryptoPayCurrency) =>
    subscribe.mutate(
      { publicationId, subscriberId: subscriber },
      {
        onSuccess: async (subscription) => {
          const started = await startCryptoCheckout(publicationId, subscription.id, payCurrency);
          if (!started) {
            cancelSubscription.mutate(subscription.id);
          }
        },
      },
    );

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
            <table className="w-full min-w-[480px] border-collapse text-sm">
              <thead>
                <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                  <th className="px-4 py-2 font-medium">{copy.rank}</th>
                  <th className="px-4 py-2 font-medium">{copy.strategy}</th>
                  <th className="px-4 py-2 font-medium">{copy.author}</th>
                  <th className="px-4 py-2 text-right font-medium">{copy.subscribers}</th>
                </tr>
              </thead>
              <tbody>
                {leaderboard.data.map((entry, index) => (
                  <tr key={entry.publication_id} className="border-t border-line">
                    <td className="px-4 py-2.5 text-muted">{index + 1}</td>
                    <td className="px-4 py-2.5">
                      <p className="font-medium text-fg">{entry.title}</p>
                      <p className="text-[11px] text-subtle">
                        v{entry.strategy_version} · {riskLabel(entry.risk_level)}
                      </p>
                    </td>
                    <td className="px-4 py-2.5 text-body">{entry.author_name}</td>
                    <td className="px-4 py-2.5 text-right font-medium text-fg">
                      {entry.subscriber_count}
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
              const price = Number(publication.pricing.monthly_price_usd);
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
                            : fmt(copy.buyout, {
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
                                    publication.pricing.model === "one_time" &&
                                    price > 0
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
                      {!current && price > 0 && publication.status === "published" && (
                        <div className="flex gap-1.5">
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={subscribe.isPending}
                            onClick={() => buyCrypto(publication.id, "usdttrc20")}
                          >
                            {copy.cryptoUsdt}
                          </Button>
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={subscribe.isPending}
                            onClick={() => buyCrypto(publication.id, "usdc")}
                          >
                            {copy.cryptoUsdc}
                          </Button>
                        </div>
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
                  <p className="min-w-0 flex-1 truncate text-xs font-medium text-fg">
                    {publication?.title ?? subscription.publication_title}
                  </p>
                  <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
                    {subscription.status !== "active" && (
                      <Badge tone={subscriptionTone[subscription.status]}>
                        {copy.subscriptionStatus[subscription.status]}
                      </Badge>
                    )}
                    {entitled &&
                      (publication &&
                      !publication.artifact_sha256 &&
                      !publication.skill ? (
                        <span className="max-w-[190px] text-right text-[10.5px] leading-snug text-subtle">
                          {copy.importUnavailable}
                        </span>
                      ) : imported.length ? (
                        <span className="text-[11px] text-muted">{copy.imported}</span>
                      ) : (
                        <Button
                          size="sm"
                          onClick={() => startImport(subscription)}
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
                  <span className="shrink-0 text-right text-[11px] text-muted">
                    {fmt(copy.subscriptionStarted, {
                      value: formatDateTime(subscription.started_at),
                    })}
                  </span>
                </div>
              );
            })
          ) : (
            <p className="text-xs text-muted">{copy.subscriptionsEmpty}</p>
          )}
        </CardContent>
      </Card>

      {paramTarget && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          role="dialog"
          aria-modal="true"
        >
          <div className="w-full max-w-md rounded-xl border border-line bg-panel shadow-xl">
            <div className="border-b border-line px-4 py-3">
              <p className="text-sm font-semibold text-fg">配置参数</p>
              <p className="mt-0.5 text-[11px] text-muted">
                这个策略按「逻辑 + 参数空间」交付，填好参数后生成属于你的实例。
              </p>
            </div>
            <div className="max-h-[60vh] space-y-3 overflow-y-auto p-4">
              {paramTarget.schema.params.map((spec) => {
                const key = `${spec.rule_key}/${spec.path}`;
                if (!spec.editable) {
                  return (
                    <div
                      key={key}
                      className="flex items-center justify-between gap-3 text-[11px] text-muted"
                    >
                      <span>{spec.label}</span>
                      <span className="font-mono">{paramText(spec.default)}（锁定）</span>
                    </div>
                  );
                }
                const enumType =
                  typeof spec.param_type === "object" && spec.param_type !== null
                    ? spec.param_type
                    : null;
                return (
                  <label key={key} className="block space-y-1">
                    <span className="flex items-center justify-between text-[11px] text-muted">
                      <span>{spec.label}</span>
                      <span className="font-mono">
                        {paramText(spec.min)} ~ {paramText(spec.max)}
                      </span>
                    </span>
                    {enumType ? (
                      <select
                        className={inputClass}
                        value={paramDraft[key] ?? ""}
                        onChange={(event) =>
                          setParamDraft((draft) => ({
                            ...draft,
                            [key]: event.target.value,
                          }))
                        }
                      >
                        {enumType.enum.options.map((option: string) => (
                          <option key={option} value={option}>
                            {option}
                          </option>
                        ))}
                      </select>
                    ) : spec.param_type === "boolean" ? (
                      <select
                        className={inputClass}
                        value={paramDraft[key] ?? "false"}
                        onChange={(event) =>
                          setParamDraft((draft) => ({
                            ...draft,
                            [key]: event.target.value,
                          }))
                        }
                      >
                        <option value="true">true</option>
                        <option value="false">false</option>
                      </select>
                    ) : (
                      <input
                        className={inputClass}
                        inputMode={
                          spec.param_type === "integer" ||
                          spec.param_type === "number" ||
                          spec.param_type === "percent"
                            ? "decimal"
                            : "text"
                        }
                        value={paramDraft[key] ?? ""}
                        onChange={(event) =>
                          setParamDraft((draft) => ({
                            ...draft,
                            [key]: event.target.value,
                          }))
                        }
                      />
                    )}
                  </label>
                );
              })}
              {paramError && (
                <p className="text-[11px] leading-snug text-danger">{paramError}</p>
              )}
            </div>
            <div className="flex items-center justify-end gap-2 border-t border-line px-4 py-3">
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setParamTarget(null);
                  setParamError(null);
                }}
                disabled={importPublication.isPending}
              >
                取消
              </Button>
              <Button
                size="sm"
                onClick={submitParams}
                disabled={importPublication.isPending}
              >
                {importPublication.isPending ? copy.importing : "生成实例"}
              </Button>
            </div>
          </div>
        </div>
      )}

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
