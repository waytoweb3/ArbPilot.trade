import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useOrders, usePositions } from "../hooks/useApi";
import {
  formatNumber,
  formatTime,
  formatUsd,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { toNumber } from "../lib/api";
export default function PositionsPage() {
  const positions = usePositions();
  const orders = useOrders();

  const totalNotional = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.quantity) * toNumber(position.mark_price),
    0,
  );

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-white">仓位与订单</h1>
        <p className="mt-1 text-sm text-slate-500">
          订阅过期或执行器暂停时，查看仓位与撤单能力始终保留。
        </p>
      </header>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>当前仓位</CardTitle>
            <CardDescription>
              {positions.data?.length ?? 0} 条 · 名义价值合计 {formatUsd(totalNotional)}
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {(positions.data ?? []).length === 0 ? (
            <EmptyState text="暂无仓位。模拟开仓成交后会出现在这里。" />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[820px] border-collapse text-sm">
                <thead>
                  <tr className="text-left text-[11px] tracking-wide text-slate-500 uppercase">
                    <th className="px-4 py-2 font-medium">交易所</th>
                    <th className="px-4 py-2 font-medium">交易对</th>
                    <th className="px-4 py-2 font-medium">方向</th>
                    <th className="px-4 py-2 text-right font-medium">数量</th>
                    <th className="px-4 py-2 text-right font-medium">开仓价</th>
                    <th className="px-4 py-2 text-right font-medium">标记价</th>
                    <th className="px-4 py-2 text-right font-medium">杠杆</th>
                    <th className="px-4 py-2 text-right font-medium">名义价值</th>
                  </tr>
                </thead>
                <tbody>
                  {(positions.data ?? []).map((position) => (
                    <tr key={position.id} className="border-t border-ink-700/50">
                      <td className="px-4 py-2.5 text-slate-300">{position.venue}</td>
                      <td className="px-4 py-2.5 font-medium text-slate-100">{position.symbol}</td>
                      <td className="px-4 py-2.5">
                        <Badge tone={position.side === "buy" ? "success" : "danger"}>
                          {position.side === "buy" ? "多" : "空"}
                        </Badge>
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">
                        {formatNumber(position.quantity, 6)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-400">
                        {formatUsd(position.entry_price)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-400">
                        {formatUsd(position.mark_price)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-400">
                        {formatNumber(position.leverage, 2)}x
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">
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
            <CardTitle>订单历史</CardTitle>
            <CardDescription>包含风控拒绝与补偿中的订单</CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {(orders.data ?? []).length === 0 ? (
            <EmptyState text="暂无订单记录。" />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[900px] border-collapse text-sm">
                <thead>
                  <tr className="text-left text-[11px] tracking-wide text-slate-500 uppercase">
                    <th className="px-4 py-2 font-medium">订单</th>
                    <th className="px-4 py-2 font-medium">交易对</th>
                    <th className="px-4 py-2 font-medium">双腿</th>
                    <th className="px-4 py-2 font-medium">来源</th>
                    <th className="px-4 py-2 text-right font-medium">名义价值</th>
                    <th className="px-4 py-2 font-medium">状态</th>
                    <th className="px-4 py-2 text-right font-medium">时间</th>
                    <th className="px-4 py-2 font-medium">备注</th>
                  </tr>
                </thead>
                <tbody>
                  {(orders.data ?? []).map((order) => (
                    <tr key={order.id} className="border-t border-ink-700/50 align-top">
                      <td className="px-4 py-2.5 font-mono text-[11px] text-slate-500">
                        {order.id.slice(0, 8)}
                      </td>
                      <td className="px-4 py-2.5 text-slate-200">
                        {order.intent.legs[0]?.symbol ?? "—"}
                      </td>
                      <td className="px-4 py-2.5 font-mono text-[11px] text-slate-400">
                        {order.intent.legs
                          .map((leg) => `${leg.venue} ${leg.side === "buy" ? "买" : "卖"}`)
                          .join(" / ")}
                      </td>
                      <td className="px-4 py-2.5 text-xs text-slate-400">{order.intent.origin}</td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">
                        {formatUsd(
                          order.intent.legs.reduce(
                            (sum, leg) => sum + toNumber(leg.quantity) * toNumber(leg.limit_price),
                            0,
                          ),
                        )}
                      </td>
                      <td className="px-4 py-2.5">
                        <Badge tone={orderStatusTone[order.status]}>
                          {orderStatusLabel[order.status]}
                        </Badge>
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-[11px] text-slate-500">
                        {formatTime(order.created_at)}
                      </td>
                      <td className="max-w-[220px] px-4 py-2.5 text-[11px] text-amber-300/90">
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
  return <p className="px-4 py-10 text-center text-sm text-slate-500">{text}</p>;
}
