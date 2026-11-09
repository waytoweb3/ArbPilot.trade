import { useDepth } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import { formatBps, formatNumber } from "../lib/format";
import { useI18n } from "../lib/i18n";
import type { DepthLevel } from "../lib/types";
import { cn } from "../lib/utils";

interface OrderBookDepthProps {
  venue: string;
  symbol: string;
  /** 顶部展示的腿标签，例如「做多 · BINANCE」。 */
  label: string;
  rows?: number;
}

/** 价格越小数位越多，保持展示稳定。 */
function priceDigits(value: number): number {
  if (value >= 1_000) return 2;
  if (value >= 1) return 3;
  if (value >= 0.01) return 5;
  return 8;
}

/**
 * 单腿多档盘口：卖盘在上、买盘在下，数量统一为币本位。
 *
 * 数据来自执行器 `/api/v1/depth`，由各交易所的公开行情接口实时拉取。
 */
export function OrderBookDepth({ venue, symbol, label, rows = 8 }: OrderBookDepthProps) {
  const { t } = useI18n();
  const depth = useDepth(venue, symbol, rows);
  const bids = (depth.data?.bids ?? []).slice(0, rows);
  const asks = (depth.data?.asks ?? []).slice(0, rows).reverse();

  const quantities = [...bids, ...asks].map((level) => toNumber(level.quantity));
  const maxQuantity = quantities.length > 0 ? Math.max(...quantities) : 0;

  const bestBid = bids[0] ? toNumber(bids[0].price) : null;
  const bestAsk = asks.length > 0 ? toNumber(asks[asks.length - 1].price) : null;
  const spreadBps =
    bestBid !== null && bestAsk !== null && bestBid > 0
      ? ((bestAsk - bestBid) / bestBid) * 10_000
      : null;

  const renderRow = (level: DepthLevel, side: "bid" | "ask") => {
    const price = toNumber(level.price);
    const quantity = toNumber(level.quantity);
    const width = maxQuantity > 0 ? Math.min(100, (quantity / maxQuantity) * 100) : 0;
    return (
      <div key={`${side}-${level.price}`} className="relative flex items-center justify-between px-2 py-[3px]">
        <span
          aria-hidden
          className={cn(
            "absolute inset-y-0 right-0 rounded-sm",
            side === "bid" ? "bg-accent/10" : "bg-danger/10",
          )}
          style={{ width: `${width}%` }}
        />
        <span
          className={cn(
            "relative font-mono",
            side === "bid" ? "text-accent" : "text-danger",
          )}
        >
          {formatNumber(price, priceDigits(price))}
        </span>
        <span className="relative font-mono text-body">{formatNumber(quantity, 4)}</span>
      </div>
    );
  };

  return (
    <div className="rounded-lg border border-line bg-panel-soft">
      <div className="flex items-center justify-between border-b border-line px-2.5 py-1.5">
        <span className="text-[11px] font-medium text-body">{label}</span>
        <span className="font-mono text-[10px] text-muted">
          {t.trade.depthSpread} {spreadBps === null ? "—" : formatBps(spreadBps)}
        </span>
      </div>
      {bids.length === 0 && asks.length === 0 ? (
        <p className="px-2.5 py-6 text-center text-[11px] text-muted">{t.trade.depthEmpty}</p>
      ) : (
        <div className="py-1 text-[11px]">
          <div className="flex items-center justify-between px-2 pb-1 text-[10px] text-subtle">
            <span>{t.trade.depthPrice}</span>
            <span>{t.trade.depthQty}</span>
          </div>
          <div>{asks.map((level) => renderRow(level, "ask"))}</div>
          <div className="my-1 flex items-center justify-between border-y border-line px-2 py-1 font-mono text-[10px] text-muted">
            <span>{t.trade.depthAsk}</span>
            <span>{t.trade.depthBid}</span>
          </div>
          <div>{bids.map((level) => renderRow(level, "bid"))}</div>
        </div>
      )}
    </div>
  );
}
