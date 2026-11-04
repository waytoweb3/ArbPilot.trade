import { useState } from "react";

import { useCreateOrder } from "../hooks/useApi";
import { formatBps, orderStatusLabel, orderStatusTone } from "../lib/format";
import type { CreateOrderResponse, Opportunity } from "../lib/types";
import { newIdempotencyKey } from "../lib/utils";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

interface TradePanelProps {
  opportunity: Opportunity | null;
}

const inputClass =
  "w-full rounded-lg border border-ink-600 bg-ink-850 px-3 py-2 text-sm text-slate-100 outline-none transition-colors focus:border-emerald-400/60";

/** 开仓前必须二次确认；提交后由执行器重新做风控校验。 */
export function TradePanel({ opportunity }: TradePanelProps) {
  const mutation = useCreateOrder();
  const [notional, setNotional] = useState("500");
  const [leverage, setLeverage] = useState("1");
  const [confirmed, setConfirmed] = useState(false);
  const [result, setResult] = useState<CreateOrderResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const submit = () => {
    if (!opportunity) {
      return;
    }
    setError(null);
    setResult(null);

    mutation.mutate(
      {
        body: {
          symbol: opportunity.symbol,
          long_venue: opportunity.long_venue,
          short_venue: opportunity.short_venue,
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
          setError(mutationError instanceof Error ? mutationError.message : "下单失败");
        },
      },
    );
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>下单确认</CardTitle>
          <CardDescription>所有订单都经过执行器风控，无法绕过。</CardDescription>
        </div>
        <Badge tone="info">模拟盘</Badge>
      </CardHeader>
      <CardContent className="space-y-4">
        {opportunity ? (
          <div className="rounded-lg border border-ink-700 bg-ink-850/70 p-3 text-xs">
            <div className="flex items-center justify-between">
              <span className="font-medium text-slate-200">{opportunity.symbol}</span>
              <span className="font-mono text-emerald-300">
                {formatBps(opportunity.net_edge_bps)} 净收益
              </span>
            </div>
            <div className="mt-2 grid grid-cols-2 gap-2 text-slate-400">
              <span>多头：{opportunity.long_venue}</span>
              <span>空头：{opportunity.short_venue}</span>
              <span>费用：{formatBps(opportunity.estimated_fee_bps)}</span>
              <span>滑点：{formatBps(opportunity.estimated_slippage_bps)}</span>
            </div>
          </div>
        ) : (
          <p className="rounded-lg border border-dashed border-ink-600 px-3 py-6 text-center text-xs text-slate-500">
            先在机会列表中选择一行，再填写下单参数。
          </p>
        )}

        <label className="block space-y-1.5">
          <span className="text-xs text-slate-400">每条腿名义价值（USD）</span>
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
          <span className="text-xs text-slate-400">杠杆</span>
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

        <label className="flex items-start gap-2 text-xs text-slate-400">
          <input
            type="checkbox"
            className="mt-0.5 accent-emerald-500"
            checked={confirmed}
            onChange={(event) => setConfirmed(event.target.checked)}
          />
          我已核对交易所、方向、金额与杠杆，并理解套利存在滑点与双腿成交风险。
        </label>

        <Button
          className="w-full"
          disabled={!opportunity || !confirmed || mutation.isPending}
          onClick={submit}
        >
          {mutation.isPending ? "提交中…" : "二次确认并开仓"}
        </Button>

        {error && (
          <p className="rounded-lg border border-rose-400/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-300">
            {error}
          </p>
        )}

        {result && (
          <div className="space-y-2 rounded-lg border border-ink-700 bg-ink-850/70 p-3 text-xs">
            <div className="flex items-center justify-between">
              <span className="text-slate-400">订单状态</span>
              <Badge tone={orderStatusTone[result.order.status]}>
                {orderStatusLabel[result.order.status]}
              </Badge>
            </div>
            <p className="font-mono text-slate-500">{result.order.id}</p>
            {result.order.note && <p className="text-amber-300">{result.order.note}</p>}
            {result.decision?.decision === "approved" && (
              <p className="text-emerald-300">
                风控通过，预计名义价值 {Number(result.decision.estimated_notional_usd).toFixed(2)} USD
              </p>
            )}
            {result.decision?.decision === "rejected" && (
              <ul className="list-disc space-y-1 pl-4 text-rose-300">
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
