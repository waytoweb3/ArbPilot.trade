import { useState } from "react";

import {
  useCancelConditionalOrder,
  useConditionalOrders,
  useCreateConditionalOrder,
} from "../hooks/useApi";
import { toNumber } from "../lib/api";
import { formatDateTime, formatNumber, formatPercentValue } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type {
  ConditionalOrder,
  ConditionalStatus,
  TriggerDirection,
  TriggerKind,
} from "../lib/types";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

interface ConditionalOrderPanelProps {
  symbol: string;
  longVenue: string;
  shortVenue: string;
  /** 当前净年化（百分比数值），用作资金费率阈值的默认参考。 */
  netAnnualizedPct: number | null;
  /** 多腿当前中间价，用作价格阈值的默认参考。 */
  longPrice: number | null;
}

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

function priceDigits(value: number): number {
  if (value >= 1_000) return 2;
  if (value >= 1) return 3;
  if (value >= 0.01) return 5;
  return 8;
}

/** 条件单：达到净年化或价格阈值后，由执行器后台自动提交双腿订单。 */
export function ConditionalOrderPanel({
  symbol,
  longVenue,
  shortVenue,
  netAnnualizedPct,
  longPrice,
}: ConditionalOrderPanelProps) {
  const { t } = useI18n();
  const created = useCreateConditionalOrder();
  const cancelled = useCancelConditionalOrder();
  const orders = useConditionalOrders();

  const [kind, setKind] = useState<TriggerKind>("funding");
  const [direction, setDirection] = useState<TriggerDirection>("above");
  const [value, setValue] = useState("");
  const [notional, setNotional] = useState("500");
  const [expires, setExpires] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const ready = Boolean(symbol) && Boolean(longVenue) && Boolean(shortVenue);
  const currentValue = kind === "funding" ? netAnnualizedPct : longPrice;
  const numericValue = Number(value);
  const numericNotional = Number(notional);
  const validValue = value.trim() !== "" && Number.isFinite(numericValue);
  const validNotional = Number.isFinite(numericNotional) && numericNotional > 0;

  const statusMeta: Record<ConditionalStatus, { tone: "success" | "warning" | "danger" | "neutral" | "info"; label: string }> = {
    active: { tone: "warning", label: t.trade.conditionalStatusActive },
    triggered: { tone: "success", label: t.trade.conditionalStatusTriggered },
    expired: { tone: "neutral", label: t.trade.conditionalStatusExpired },
    cancelled: { tone: "neutral", label: t.trade.conditionalStatusCancelled },
    rejected: { tone: "danger", label: t.trade.conditionalStatusRejected },
  };

  const triggerText = (order: ConditionalOrder): string => {
    const dir =
      order.trigger_direction === "above" ? t.trade.conditionalAbove : t.trade.conditionalBelow;
    if (order.trigger_kind === "funding") {
      return `${t.trade.conditionalKindFunding} ${dir} ${formatPercentValue(order.trigger_value)}`;
    }
    const threshold = toNumber(order.trigger_value);
    return `${t.trade.conditionalKindPrice} ${dir} ${formatNumber(threshold, priceDigits(threshold))}`;
  };

  const submit = () => {
    if (!ready || !validValue || !validNotional) {
      return;
    }
    setError(null);
    setNotice(null);
    created.mutate(
      {
        symbol,
        long_venue: longVenue,
        short_venue: shortVenue,
        notional_usd: numericNotional,
        leverage: 1,
        trigger_kind: kind,
        trigger_direction: direction,
        trigger_value: numericValue,
        expires_at: expires ? new Date(expires).toISOString() : null,
      },
      {
        onSuccess: () => {
          setValue("");
          setExpires("");
          setNotice(t.trade.conditionalCreated);
        },
        onError: (mutationError) =>
          setError(mutationError instanceof Error ? mutationError.message : t.errors.unknown),
      },
    );
  };

  const symbolOrders = (orders.data ?? []).filter((order) => order.symbol === symbol);

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.trade.conditionalTitle}</CardTitle>
          <CardDescription>{t.trade.conditionalDesc}</CardDescription>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="grid grid-cols-2 gap-2">
          <div className="space-y-1">
            <span className="text-[11px] text-muted">{t.trade.conditionalKind}</span>
            <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
              {(
                [
                  ["funding", t.trade.conditionalKindFunding],
                  ["price", t.trade.conditionalKindPrice],
                ] as [TriggerKind, string][]
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setKind(id)}
                  className={cn(
                    "flex-1 rounded-md px-2 py-1 text-[11px] transition-colors",
                    kind === id
                      ? "bg-panel font-medium text-fg shadow-sm"
                      : "text-muted hover:text-body",
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <div className="space-y-1">
            <span className="text-[11px] text-muted">{t.trade.conditionalDirection}</span>
            <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
              {(
                [
                  ["above", t.trade.conditionalAbove],
                  ["below", t.trade.conditionalBelow],
                ] as [TriggerDirection, string][]
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setDirection(id)}
                  className={cn(
                    "flex-1 rounded-md px-2 py-1 text-[11px] transition-colors",
                    direction === id
                      ? "bg-panel font-medium text-fg shadow-sm"
                      : "text-muted hover:text-body",
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="space-y-1">
          <div className="flex items-baseline justify-between">
            <span className="text-[11px] text-muted">{t.trade.conditionalValue}</span>
            <span className="text-[10px] text-subtle">
              {kind === "funding"
                ? fmt(t.trade.conditionalFundingHint, { value: formatPercentValue(netAnnualizedPct) })
                : fmt(t.trade.conditionalPriceHint, {
                    value:
                      longPrice === null ? "—" : formatNumber(longPrice, priceDigits(longPrice)),
                  })}
            </span>
          </div>
          <input
            className={inputClass}
            type="number"
            step="any"
            value={value}
            placeholder={
              currentValue === null
                ? ""
                : kind === "funding"
                  ? currentValue.toFixed(2)
                  : currentValue.toFixed(priceDigits(currentValue))
            }
            onChange={(event) => setValue(event.target.value)}
          />
        </div>

        <div className="grid grid-cols-2 gap-2">
          <div className="space-y-1">
            <span className="text-[11px] text-muted">{t.trade.conditionalNotional}</span>
            <input
              className={inputClass}
              type="number"
              min={10}
              step={10}
              value={notional}
              onChange={(event) => setNotional(event.target.value)}
            />
          </div>
          <div className="space-y-1">
            <span className="text-[11px] text-muted">{t.trade.conditionalExpiry}</span>
            <input
              className={inputClass}
              type="datetime-local"
              value={expires}
              onChange={(event) => setExpires(event.target.value)}
            />
          </div>
        </div>

        <p className="text-[10px] text-subtle">{t.trade.conditionalExpiryHint}</p>

        <Button
          className="w-full"
          variant="secondary"
          disabled={!ready || !validValue || !validNotional || created.isPending}
          onClick={submit}
        >
          {created.isPending ? t.trade.conditionalSubmitting : t.trade.conditionalSubmit}
        </Button>

        {error && (
          <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
            {error}
          </p>
        )}
        {notice && (
          <p className="rounded-lg border border-accent/30 bg-accent/10 px-3 py-2 text-xs text-accent">
            {notice}
          </p>
        )}

        <div className="space-y-1.5 border-t border-line pt-2">
          <p className="text-[11px] font-medium text-body">{t.trade.conditionalActiveTitle}</p>
          {symbolOrders.length === 0 ? (
            <p className="py-2 text-center text-[11px] text-muted">{t.trade.conditionalEmpty}</p>
          ) : (
            <ul className="space-y-1.5">
              {symbolOrders.map((order) => {
                const meta = statusMeta[order.status];
                return (
                  <li
                    key={order.id}
                    className="rounded-md border border-line bg-panel-soft px-2.5 py-1.5 text-[11px]"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-mono text-body">{triggerText(order)}</span>
                      <Badge tone={meta.tone}>{meta.label}</Badge>
                    </div>
                    <div className="mt-1 flex items-center justify-between gap-2 text-[10px] text-muted">
                      <span>
                        {t.trade.conditionalExpiry}:{" "}
                        {order.expires_at
                          ? formatDateTime(order.expires_at)
                          : t.trade.conditionalNoExpiry}
                      </span>
                      {order.status === "active" && (
                        <button
                          type="button"
                          disabled={cancelled.isPending}
                          onClick={() => cancelled.mutate(order.id)}
                          className="text-danger transition-opacity hover:opacity-80 disabled:opacity-50"
                        >
                          {t.trade.conditionalCancel}
                        </button>
                      )}
                    </div>
                    {order.note && <p className="mt-0.5 text-[10px] text-warning">{order.note}</p>}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
