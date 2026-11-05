import { useState } from "react";

import { useCreateOrder } from "../hooks/useApi";
import {
  formatFundingRate,
  formatPercentValue,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { CreateOrderResponse } from "../lib/types";
import { newIdempotencyKey } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

interface TradePanelProps {
  symbol: string;
  longVenue: string;
  shortVenue: string;
  netRate: number | null;
  netAnnualizedPct: number | null;
}

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

/** 开仓前必须二次确认；提交后由执行器重新做风控校验。 */
export function TradePanel({ symbol, longVenue, shortVenue, netRate, netAnnualizedPct }: TradePanelProps) {
  const mutation = useCreateOrder();
  const [notional, setNotional] = useState("500");
  const [leverage, setLeverage] = useState("1");
  const [confirmed, setConfirmed] = useState(false);
  const [result, setResult] = useState<CreateOrderResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { t } = useI18n();

  const ready = Boolean(symbol) && Boolean(longVenue) && Boolean(shortVenue);

  const submit = () => {
    if (!ready) {
      return;
    }
    setError(null);
    setResult(null);

    mutation.mutate(
      {
        body: {
          symbol,
          long_venue: longVenue,
          short_venue: shortVenue,
          notional_usd: Number(notional),
          leverage: Number(leverage),
        },
        idempotencyKey: newIdempotencyKey(),
      },
      {
        onSuccess: (response) => {
          setResult(response);
          setConfirmed(false);
        },
        onError: (mutationError) => {
          setError(mutationError instanceof Error ? mutationError.message : t.errors.unknown);
        },
      },
    );
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.trade.title}</CardTitle>
          <CardDescription>{t.trade.desc}</CardDescription>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="rounded-lg border border-line bg-panel-soft p-3 text-xs">
          <div className="flex items-center justify-between">
            <span className="font-medium text-body">{symbol || "—"}</span>
            <span className="font-mono text-accent">{formatPercentValue(netAnnualizedPct)}</span>
          </div>
          <div className="mt-2 grid grid-cols-2 gap-2 text-muted">
            <span>{fmt(t.trade.long, { value: longVenue.toUpperCase() || "—" })}</span>
            <span>{fmt(t.trade.short, { value: shortVenue.toUpperCase() || "—" })}</span>
            <span>{t.trade.netPerSettleLabel}</span>
            <span className="font-mono text-body">{formatFundingRate(netRate)}</span>
          </div>
        </div>

        <label className="block space-y-1.5">
          <span className="text-xs text-muted">{t.trade.notional}</span>
          <input
            className={inputClass}
            type="number"
            min={10}
            step={10}
            value={notional}
            onChange={(event) => setNotional(event.target.value)}
          />
        </label>

        <label className="block space-y-1.5">
          <span className="text-xs text-muted">{t.trade.leverage}</span>
          <input
            className={inputClass}
            type="number"
            min={1}
            max={10}
            step={1}
            value={leverage}
            onChange={(event) => setLeverage(event.target.value)}
          />
        </label>

        <label className="flex items-start gap-2 text-xs text-muted">
          <input
            type="checkbox"
            className="mt-0.5 accent-emerald-600 dark:accent-emerald-400"
            checked={confirmed}
            onChange={(event) => setConfirmed(event.target.checked)}
          />
          {t.trade.confirm}
        </label>

        <Button
          className="w-full"
          disabled={!ready || !confirmed || mutation.isPending}
          onClick={submit}
        >
          {mutation.isPending ? t.trade.submitting : t.trade.submit}
        </Button>

        {error && (
          <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
            {error}
          </p>
        )}

        {result && (
          <div className="space-y-2 rounded-lg border border-line bg-panel-soft p-3 text-xs">
            <div className="flex items-center justify-between">
              <span className="text-muted">{t.trade.orderStatus}</span>
              <Badge tone={orderStatusTone[result.order.status]}>
                {orderStatusLabel(result.order.status)}
              </Badge>
            </div>
            <p className="font-mono text-muted">{result.order.id}</p>
            {result.order.note && <p className="text-warning">{result.order.note}</p>}
            {result.decision?.decision === "approved" && (
              <p className="text-accent">
                {fmt(t.trade.approved, {
                  value: Number(result.decision.estimated_notional_usd).toFixed(2),
                })}
              </p>
            )}
            {result.decision?.decision === "rejected" && (
              <ul className="list-disc space-y-1 pl-4 text-danger">
                {result.decision.violations.map((violation) => (
                  <li key={violation.code}>{violation.message}</li>
                ))}
              </ul>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
