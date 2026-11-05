import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useOrders, usePositions } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatNumber,
  formatTime,
  formatUsd,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";

export default function PositionsPage() {
  const positions = usePositions();
  const orders = useOrders();
  const { t } = useI18n();

  const totalNotional = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.quantity) * toNumber(position.mark_price),
    0,
  );

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
              })}
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {(positions.data ?? []).length === 0 ? (
            <EmptyState text={t.positions.empty} />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[820px] border-collapse text-sm">
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
    </div>
  );
}

function EmptyState({ text }: { text: string }) {
  return <p className="px-4 py-10 text-center text-sm text-muted">{text}</p>;
}
