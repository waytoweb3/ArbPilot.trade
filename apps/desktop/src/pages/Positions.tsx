import { useState } from "react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import { useClosePosition, useOrders, usePositions } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatNumber,
  formatTime,
  formatUsd,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { Decimal, Position } from "../lib/types";

export default function PositionsPage() {
  const positions = usePositions();
  const orders = useOrders();
  const closePosition = useClosePosition();
  const { t } = useI18n();
  /** 待确认平仓的仓位；不为空时展示确认弹窗。 */
  const [closing, setClosing] = useState<Position | null>(null);

  const totalNotional = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.quantity) * toNumber(position.mark_price),
    0,
  );
  const totalPnl = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.unrealized_pnl),
    0,
  );
  const closeError = closePosition.error
    ? closePosition.error instanceof Error
      ? closePosition.error.message
      : String(closePosition.error)
    : null;

  const confirmClose = () => {
    if (!closing) {
      return;
    }
    closePosition.mutate(closing.id, { onSuccess: () => setClosing(null) });
  };

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.positions.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.positions.subtitle}</p>
      </header>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.positions.current}</CardTitle>
            <CardDescription>
              {fmt(t.positions.summary, {
                count: positions.data?.length ?? 0,
                value: formatUsd(totalNotional),
                pnl: formatSignedPnl(totalPnl),
              })}
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {(positions.data ?? []).length === 0 ? (
            <EmptyState text={t.positions.empty} />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[900px] border-collapse text-sm">
                <thead>
                  <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                    <th className="px-4 py-2 font-medium">{t.positions.venue}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.symbol}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.side}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.quantity}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.entry}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.mark}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.leverage}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.notional}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.pnl}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.action}</th>
                  </tr>
                </thead>
                <tbody>
                  {(positions.data ?? []).map((position) => (
                    <tr key={position.id} className="border-t border-line">
                      <td className="px-4 py-2.5 text-body">{position.venue}</td>
                      <td className="px-4 py-2.5 font-medium text-fg">{position.symbol}</td>
                      <td className="px-4 py-2.5">
                        <Badge tone={position.side === "buy" ? "success" : "danger"}>
                          {position.side === "buy" ? t.positions.long : t.positions.short}
                        </Badge>
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-body">
                        {formatNumber(position.quantity, 6)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">
                        {formatUsd(position.entry_price)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">
                        {formatUsd(position.mark_price)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">
                        {formatNumber(position.leverage, 2)}x
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-body">
                        {formatUsd(toNumber(position.quantity) * toNumber(position.mark_price))}
                      </td>
                      <td
                        className={`px-4 py-2.5 text-right font-mono text-xs ${pnlTone(position.unrealized_pnl)}`}
                      >
                        {formatSignedPnl(position.unrealized_pnl)}
                      </td>
                      <td className="px-4 py-2.5 text-right">
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={closePosition.isPending}
                          onClick={() => {
                            closePosition.reset();
                            setClosing(position);
                          }}
                        >
                          {t.positions.close}
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.positions.history}</CardTitle>
            <CardDescription>{t.positions.historyDesc}</CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {(orders.data ?? []).length === 0 ? (
            <EmptyState text={t.positions.ordersEmpty} />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[900px] border-collapse text-sm">
                <thead>
                  <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                    <th className="px-4 py-2 font-medium">{t.positions.order}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.symbol}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.legs}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.origin}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.notional}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.status}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.time}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.note}</th>
                  </tr>
                </thead>
                <tbody>
                  {(orders.data ?? []).map((order) => (
                    <tr key={order.id} className="border-t border-line align-top">
                      <td className="px-4 py-2.5 font-mono text-[11px] text-muted">
                        {order.id.slice(0, 8)}
                      </td>
                      <td className="px-4 py-2.5 text-body">{order.intent.legs[0]?.symbol ?? "—"}</td>
                      <td className="px-4 py-2.5 font-mono text-[11px] text-muted">
                        {order.intent.legs
                          .map(
                            (leg) =>
                              `${leg.venue} ${leg.side === "buy" ? t.positions.buy : t.positions.sell}`,
                          )
                          .join(" / ")}
                      </td>
                      <td className="px-4 py-2.5 text-xs text-muted">{order.intent.origin}</td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-body">
                        {formatUsd(
                          order.intent.legs.reduce(
                            (sum, leg) => sum + toNumber(leg.quantity) * toNumber(leg.limit_price),
                            0,
                          ),
                        )}
                      </td>
                      <td className="px-4 py-2.5">
                        <Badge tone={orderStatusTone[order.status]}>{orderStatusLabel(order.status)}</Badge>
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-[11px] text-muted">
                        {formatTime(order.created_at)}
                      </td>
                      <td className="max-w-[220px] px-4 py-2.5 text-[11px] text-warning">
                        {order.note ?? "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>

      <ConfirmDialog
        open={closing !== null}
        title={t.positions.closeTitle}
        message={
          closing
            ? fmt(t.positions.closeMessage, {
                venue: closing.venue,
                symbol: closing.symbol,
                quantity: formatNumber(closing.quantity, 6),
              })
            : ""
        }
        confirmLabel={t.positions.closeConfirm}
        cancelLabel={t.positions.closeCancel}
        pending={closePosition.isPending}
        error={closeError}
        onConfirm={confirmClose}
        onCancel={() => {
          closePosition.reset();
          setClosing(null);
        }}
      />
    </div>
  );
}

function EmptyState({ text }: { text: string }) {
  return <p className="px-4 py-10 text-center text-sm text-muted">{text}</p>;
}

// 浮动盈亏：盈利显示正号并用强调色，亏损显示负号并用危险色。
function formatSignedPnl(value: Decimal): string {
  const amount = toNumber(value);
  const sign = amount > 0 ? "+" : amount < 0 ? "-" : "";
  return `${sign}${formatUsd(Math.abs(amount))}`;
}

function pnlTone(value: Decimal): string {
  const amount = toNumber(value);
  if (amount > 0) return "text-accent";
  if (amount < 0) return "text-danger";
  return "text-muted";
}
