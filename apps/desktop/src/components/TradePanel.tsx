import { useState } from "react";

import { useAccounts, useCreateOrder, useMinOrder, useRiskLimits } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatFundingRate,
  formatPercentValue,
  formatUsd,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { CreateOrderResponse } from "../lib/types";
import { cn, newIdempotencyKey } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

interface TradePanelProps {
  symbol: string;
  longVenue: string;
  shortVenue: string;
  longRate: number | null;
  shortRate: number | null;
  netRate: number | null;
  netAnnualizedPct: number | null;
  longPrice: number | null;
  shortPrice: number | null;
}

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";
const chipClass =
  "rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px] text-muted transition-colors hover:border-accent/50 hover:text-fg";
const NOTIONAL_PRESETS = [200, 500, 1_000, 2_000, 5_000];

/** 有效最小下单额：名义额与「最小数量 × 当前价」取较大值，并按数量步长向上取整。 */
function effectiveMinNotional(
  spec: {
    min_quantity?: number | string | null;
    min_notional_usd?: number | string | null;
    step_size?: number | string | null;
  } | null | undefined,
  price: number | null,
): number | null {
  if (!spec) {
    return null;
  }
  // 下单数量按步长向下取整，门槛名义额换算成数量后需向上取整到步长，否则恰好
  // 等于门槛的金额取整后会低于交易所要求（如 Lighter BTC 5 位精度、10 USDC 门槛）。
  const step = toNumber(spec.step_size ?? 0);
  const roundUp = (value: number): number =>
    step > 0 ? Math.ceil(value / step - 1e-9) * step : value;
  const candidates: number[] = [];
  if (spec.min_notional_usd !== undefined && spec.min_notional_usd !== null) {
    const minimum = toNumber(spec.min_notional_usd);
    candidates.push(
      price && price > 0 ? roundUp(minimum / price) * price : minimum,
    );
  }
  if (
    spec.min_quantity !== undefined &&
    spec.min_quantity !== null &&
    price &&
    price > 0
  ) {
    candidates.push(roundUp(toNumber(spec.min_quantity)) * price);
  }
  const positive = candidates.filter((value) => Number.isFinite(value) && value > 0);
  return positive.length > 0 ? Math.max(...positive) : null;
}

/** 开仓前必须二次确认；提交后由执行器重新做风控校验。 */
export function TradePanel({
  symbol,
  longVenue,
  shortVenue,
  longRate,
  shortRate,
  netRate,
  netAnnualizedPct,
  longPrice,
  shortPrice,
}: TradePanelProps) {
  const mutation = useCreateOrder();
  const accounts = useAccounts();
  const limits = useRiskLimits();
  const longMinOrder = useMinOrder(longVenue, symbol);
  const shortMinOrder = useMinOrder(shortVenue, symbol);
  const [notional, setNotional] = useState("500");
  const [leverageDraft, setLeverageDraft] = useState<string | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [result, setResult] = useState<CreateOrderResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { t } = useI18n();

  const ready = Boolean(symbol) && Boolean(longVenue) && Boolean(shortVenue);

  const maxNotional = toNumber(limits.data?.max_order_notional_usd);
  const notionalValue = Number(notional);
  const validSize = Number.isFinite(notionalValue) && notionalValue > 0;
  // 各交易所按币种返回最小下单限制；两腿金额相同，取更严格的作为门槛。
  const longMinNotional = effectiveMinNotional(longMinOrder.data, longPrice);
  const shortMinNotional = effectiveMinNotional(shortMinOrder.data, shortPrice);
  const legMinNotional = Math.max(longMinNotional ?? 0, shortMinNotional ?? 0);

  const legAccount = (venue: string) =>
    (accounts.data?.accounts ?? []).find((account) => account.venue === venue) ?? null;
  const longAccount = legAccount(longVenue);
  const shortAccount = legAccount(shortVenue);
  /** 优先使用合约账户可用保证金；接口未提供时退回账户权益。 */
  const availableOf = (account: ReturnType<typeof legAccount>): number | null => {
    const available = account?.snapshot?.available_margin_usd;
    if (available !== undefined && available !== null) {
      return toNumber(available);
    }
    const equity = account?.snapshot?.equity_usd;
    return equity === undefined || equity === null ? null : toNumber(equity);
  };
  const longAvailable = availableOf(longAccount);
  const shortAvailable = availableOf(shortAccount);

  // 风控配置的杠杆上限：全局上限与两腿的交易所覆盖取最严格值，口径与执行器风控一致。
  const maxLeverage = (() => {
    const global = toNumber(limits.data?.max_leverage);
    if (!(global > 0)) {
      return 1;
    }
    const overrides = [longVenue, shortVenue]
      .map((venue) => toNumber(limits.data?.venue_overrides?.[venue.toLowerCase()]?.max_leverage))
      .filter((value) => value > 0);
    return overrides.length > 0 ? Math.min(global, ...overrides) : global;
  })();
  // 杠杆可手动输入，未填写时用风控值。下单会带上该值，执行器开仓前会把交易所杠杆
  // 写为该值，因此保证金预览与两腿显示都统一按这个值估算，避免与交易所当前的旧杠杆
  // （如 3x）不一致。
  const draftLeverage = leverageDraft === null ? maxLeverage : Number(leverageDraft);
  const leverageValue = Number.isFinite(draftLeverage) && draftLeverage > 0 ? draftLeverage : maxLeverage;
  const leverageText = leverageDraft ?? String(maxLeverage);
  const leverageOverLimit = leverageValue > maxLeverage;

  const marginAt = (leverage: number): number | null =>
    validSize ? notionalValue / leverage : null;
  const longMargin = marginAt(leverageValue);
  const shortMargin = marginAt(leverageValue);
  const totalMargin =
    longMargin === null || shortMargin === null ? null : longMargin + shortMargin;
  const perSettle = netRate !== null && validSize ? notionalValue * netRate : null;
  const annualIncome =
    netAnnualizedPct !== null && validSize ? (notionalValue * netAnnualizedPct) / 100 : null;
  const longQty = longPrice && validSize ? notionalValue / longPrice : null;
  const shortQty = shortPrice && validSize ? notionalValue / shortPrice : null;

  const insufficient =
    (longMargin !== null && longAvailable !== null && longAvailable < longMargin) ||
    (shortMargin !== null && shortAvailable !== null && shortAvailable < shortMargin);
  const overLimit =
    maxNotional > 0 && validSize && notionalValue > maxNotional;
  const belowMinimum =
    legMinNotional > 0 && validSize && notionalValue < legMinNotional;

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
          leverage: leverageValue,
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
            <span
              className={cn(
                "font-mono",
                (netAnnualizedPct ?? 0) >= 0 ? "text-accent" : "text-danger",
              )}
            >
              {formatPercentValue(netAnnualizedPct)}
            </span>
          </div>
          <div className="mt-2 space-y-1.5 text-muted">
            <div className="flex items-center justify-between gap-2">
              <span className="flex items-center gap-1.5">
                <Badge tone="success">{t.trade.longBadge}</Badge>
                {longVenue.toUpperCase() || "—"}
              </span>
              <span className="font-mono text-body">{formatFundingRate(longRate)}</span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="flex items-center gap-1.5">
                <Badge tone="danger">{t.trade.shortBadge}</Badge>
                {shortVenue.toUpperCase() || "—"}
              </span>
              <span className="font-mono text-body">{formatFundingRate(shortRate)}</span>
            </div>
            <div className="flex items-center justify-between gap-2 border-t border-line pt-1.5">
              <span>{t.trade.netPerSettleLabel}</span>
              <span className="font-mono text-body">{formatFundingRate(netRate)}</span>
            </div>
          </div>
        </div>

        <div className="space-y-1.5">
          {[
            {
              index: 1,
              venue: longVenue,
              account: longAccount,
              available: longAvailable,
              leverage: leverageValue,
              minNotional: longMinNotional,
            },
            {
              index: 2,
              venue: shortVenue,
              account: shortAccount,
              available: shortAvailable,
              leverage: leverageValue,
              minNotional: shortMinNotional,
            },
          ].map((leg) => (
            <div key={leg.index} className="space-y-1">
              <div className="flex items-center justify-between gap-2 rounded-md border border-line bg-panel-soft px-2.5 py-1.5 text-[11px]">
                <span className="text-muted">
                  {leg.venue.toUpperCase()} · {t.trade.availableMargin}
                </span>
                {!leg.account ? (
                  <span className="text-warning">{fmt(t.trade.accountMissing, { venue: leg.venue.toUpperCase() })}</span>
                ) : (
                  <span className="flex items-center gap-2 font-mono text-body">
                    <span>{leg.available === null ? "—" : formatUsd(leg.available)}</span>
                    <span className="text-subtle">
                      {t.trade.leverage} {leg.leverage}x
                    </span>
                  </span>
                )}
              </div>
              <div className="flex items-center justify-between gap-2 px-2.5 text-[11px]">
                <span className="text-muted">
                  {leg.venue.toUpperCase()} · {t.trade.minOrderLabel}
                </span>
                <span className="font-mono text-body">
                  {leg.minNotional === null ? "—" : formatUsd(leg.minNotional, 2)}
                </span>
              </div>
            </div>
          ))}
        </div>

        <div className="space-y-2">
          <div className="flex items-baseline justify-between">
            <span className="text-xs text-muted">{t.trade.notional}</span>
            <span className="text-[10px] text-subtle">{t.trade.perLegHint}</span>
          </div>
          <input
            className={inputClass}
            type="number"
            min={legMinNotional || 10}
            step={10}
            value={notional}
            onChange={(event) => setNotional(event.target.value)}
          />
          <div className="flex flex-wrap gap-1.5">
            {NOTIONAL_PRESETS.map((value) => (
              <button
                key={value}
                type="button"
                onClick={() => setNotional(String(value))}
                className={cn(
                  chipClass,
                  Number(notional) === value && "border-accent/50 text-fg",
                )}
              >
                {value.toLocaleString("en-US")}
              </button>
            ))}
          </div>
        </div>

        <div className="space-y-2">
          <div className="flex items-baseline justify-between">
            <span className="text-xs text-muted">{t.trade.leverage}</span>
            <span className="text-[10px] text-subtle">
              {fmt(t.trade.leverageHint, { value: `${maxLeverage}x` })}
            </span>
          </div>
          <input
            className={inputClass}
            type="number"
            min={1}
            step={1}
            value={leverageText}
            onChange={(event) => setLeverageDraft(event.target.value)}
          />
        </div>

        <div className="rounded-lg border border-line bg-panel-soft">
          <p className="border-b border-line px-3 py-1.5 text-[11px] font-medium text-body">
            {t.trade.previewTitle}
          </p>
          <dl className="divide-y divide-line text-xs">
            <div className="flex items-center justify-between px-3 py-1.5">
              <dt className="text-muted">
                {t.trade.margin} · {t.trade.marginBoth}
              </dt>
              <dd className="font-mono text-body">{totalMargin === null ? "—" : formatUsd(totalMargin)}</dd>
            </div>
            <div className="flex items-center justify-between px-3 py-1.5">
              <dt className="text-muted">{t.trade.perPeriodIncome}</dt>
              <dd className="font-mono text-body">{perSettle === null ? "—" : formatUsd(perSettle, 4)}</dd>
            </div>
            <div className="flex items-center justify-between px-3 py-1.5">
              <dt className="text-muted">{t.trade.annualIncome}</dt>
              <dd className="font-mono text-body">{annualIncome === null ? "—" : formatUsd(annualIncome)}</dd>
            </div>
            <div className="flex items-center justify-between px-3 py-1.5">
              <dt className="text-muted">{t.trade.qtyPerLeg}</dt>
              <dd className="font-mono text-body">
                {longQty === null || shortQty === null
                  ? "—"
                  : `${longQty.toFixed(4)} / ${shortQty.toFixed(4)}`}
              </dd>
            </div>
          </dl>
          <p className="border-t border-line px-3 py-1.5 text-[10px] text-subtle">{t.trade.estimateNote}</p>
        </div>

        {netRate !== null && netRate < 0 && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            {t.trade.negativeWarning}
          </p>
        )}
        {insufficient && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            {t.trade.balanceShort}
          </p>
        )}
        {overLimit && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            {fmt(t.trade.notionalLimit, { value: formatUsd(maxNotional) })}
          </p>
        )}
        {belowMinimum && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            {fmt(t.trade.minNotional, { value: formatUsd(legMinNotional) })}
          </p>
        )}
        {leverageOverLimit && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
            {fmt(t.trade.leverageLimit, { value: `${maxLeverage}x` })}
          </p>
        )}

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
          disabled={!ready || !confirmed || insufficient || belowMinimum || mutation.isPending}
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
            {/* 风控拒绝的原因由下方 violations 结构化展示，避免同一句话出现两次。 */}
            {result.order.note && result.decision?.decision !== "rejected" && (
              <p className="text-warning">{result.order.note}</p>
            )}
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
