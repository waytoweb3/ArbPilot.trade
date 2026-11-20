import { ChevronLeft, ChevronRight, ShieldAlert } from "lucide-react";
import { Fragment, useMemo, useState } from "react";
import { Link } from "react-router-dom";

import {
  useCancelOrder,
  useCloseAllPositions,
  useClosePosition,
  usePanic,
  useReconcileOrders,
  useReplaceOrder,
} from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatNumber,
  formatTime,
  formatUsd,
  orderStatusLabel,
  orderStatusTone,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { Order, Position } from "../lib/types";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";
import { CardContent } from "./ui/card";

type BottomTab = "positions" | "openOrders" | "history" | "trades";

/** 仍在执行中的订单状态：与仓位页的历史订单口径相反。 */
const OPEN_ORDER_STATUSES = new Set([
  "pending_risk",
  "submitting",
  "partially_filled",
  "unwinding",
]);

/** 面板每页条数。 */
const BOTTOM_PAGE_SIZE = 10;

/**
 * 持仓 / 当前委托 / 历史委托 / 成交记录四页签面板。
 * 交易页与策略运行卡片共用，传入的数据由调用方按交易对或策略过滤。
 */
export function PositionsOrdersPanel({
  positions,
  orders,
}: {
  positions: Position[];
  orders: Order[];
}) {
  const { t } = useI18n();
  const closePosition = useClosePosition();
  const closeAll = useCloseAllPositions();
  const cancelOrder = useCancelOrder();
  const replaceOrder = useReplaceOrder();
  const reconcile = useReconcileOrders();
  const panic = usePanic();
  // 平仓相关的错误（单腿 / 一键）统一展示在持仓面板顶部。
  const positionCloseError = closeAll.error ?? closePosition.error;
  // 改单：一次只展开一行，避免多行表单互相覆盖。
  const [editing, setEditing] = useState<{
    id: string;
    price: string;
    quantity: string;
  } | null>(null);
  // 一键脱困必须手动输入确认码，避免误触清仓。
  const [panicOpen, setPanicOpen] = useState(false);
  const [panicInput, setPanicInput] = useState("");

  // 仓位组筛选：下拉选择后只看该组仓位；分组消失（已平仓）时自动回落到全部。
  const [tab, setTab] = useState<BottomTab>("positions");
  const [groupFilter, setGroupFilter] = useState("");
  const [page, setPage] = useState(0);

  // 同一次双腿下单产生的仓位带同一个 group_id，可整组平仓。
  const positionGroups = useMemo(() => {
    const groups = new Map<string, Position[]>();
    for (const position of positions) {
      if (!position.group_id) {
        continue;
      }
      const members = groups.get(position.group_id);
      if (members) {
        members.push(position);
      } else {
        groups.set(position.group_id, [position]);
      }
    }
    return [...groups.entries()];
  }, [positions]);

  const activeGroupFilter = positionGroups.some(([groupId]) => groupId === groupFilter)
    ? groupFilter
    : "";
  const filteredPositions = useMemo(
    () =>
      activeGroupFilter
        ? positions.filter((position) => position.group_id === activeGroupFilter)
        : positions,
    [positions, activeGroupFilter],
  );

  const openOrders = useMemo(
    () => orders.filter((order) => OPEN_ORDER_STATUSES.has(order.status)),
    [orders],
  );
  const closedOrders = useMemo(
    () => orders.filter((order) => !OPEN_ORDER_STATUSES.has(order.status)),
    [orders],
  );
  // 成交记录直接从双腿的成交回报里展开，无需额外接口。
  const fills = useMemo(
    () =>
      orders.flatMap((order) =>
        order.legs
          .filter((leg) => toNumber(leg.filled_quantity) > 0)
          .map((leg) => ({ order, leg })),
      ),
    [orders],
  );
  const orderList = tab === "history" ? closedOrders : openOrders;
  const tabs = [
    { id: "positions" as const, label: t.trade.bottomPositions, count: positions.length },
    { id: "openOrders" as const, label: t.trade.bottomOpenOrders, count: openOrders.length },
    { id: "history" as const, label: t.trade.bottomHistory, count: closedOrders.length },
    { id: "trades" as const, label: t.trade.bottomTrades, count: fills.length },
  ];

  // 当前标签页的总条数决定分页；页码越界时自动收敛到最后一页。
  const count =
    tab === "positions" ? filteredPositions.length : tab === "trades" ? fills.length : orderList.length;
  const pageCount = Math.max(1, Math.ceil(count / BOTTOM_PAGE_SIZE));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRows = <T,>(rows: T[]) =>
    rows.slice(currentPage * BOTTOM_PAGE_SIZE, currentPage * BOTTOM_PAGE_SIZE + BOTTOM_PAGE_SIZE);

  return (
    <div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-2 py-1.5">
        <div className="flex min-w-0 items-center gap-1 overflow-x-auto">
          {tabs.map((item) => (
            <button
              key={item.id}
              type="button"
              onClick={() => {
                setTab(item.id);
                setPage(0);
              }}
              className={cn(
                "flex shrink-0 items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition-colors",
                tab === item.id
                  ? "bg-panel-soft font-medium text-fg"
                  : "text-muted hover:text-body",
              )}
            >
              {item.label}
              {item.count > 0 && (
                <span
                  className={cn(
                    "rounded-full px-1.5 py-0.5 font-mono text-[10px]",
                    tab === item.id
                      ? "bg-accent/15 text-accent"
                      : "bg-panel-soft text-subtle",
                  )}
                >
                  {item.count}
                </span>
              )}
            </button>
          ))}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            type="button"
            disabled={reconcile.isPending}
            onClick={() => reconcile.mutate()}
            className="text-[11px] text-muted transition-colors hover:text-body disabled:opacity-50"
          >
            {t.trade.reconcile}
          </button>
          <button
            type="button"
            onClick={() => {
              setPanicInput("");
              setPanicOpen(true);
            }}
            className="flex items-center gap-1 rounded-md border border-danger/40 px-2 py-1 text-[11px] text-danger transition-colors hover:bg-danger/10"
          >
            <ShieldAlert size={12} />
            {t.trade.panic}
          </button>
          <Link to="/positions" className="pr-1 text-[11px] text-muted hover:text-body">
            {t.trade.viewAll}
          </Link>
        </div>
      </div>
      {(reconcile.data || reconcile.error || panic.data || panic.error) && (
        <p className="border-b border-line px-4 py-1.5 text-[11px] text-muted">
          {reconcile.data &&
            `${fmt(t.trade.reconcileDone, { count: reconcile.data.incomplete_orders.length })} · ${
              reconcile.data.paused ? t.trade.reconcilePaused : t.trade.reconcileClear
            }`}
          {reconcile.error && <span className="text-danger">{reconcile.error.message}</span>}
          {panic.data &&
            ` ${fmt(t.trade.panicDone, {
              orders: panic.data.cancelled_orders,
              positions: panic.data.closed_positions,
            })}`}
          {panic.error && <span className="text-danger">{panic.error.message}</span>}
        </p>
      )}
      <CardContent className="p-0">
        {tab === "positions" && (
          <>
            {positions.length > 0 && (
              <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2">
                <select
                  value={activeGroupFilter}
                  onChange={(event) => {
                    setGroupFilter(event.target.value);
                    setPage(0);
                  }}
                  aria-label={t.trade.groupLabel}
                  className="rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px] text-body outline-none focus:border-accent/60"
                >
                  <option value="">{t.trade.groupFilterAll}</option>
                  {positionGroups.map(([groupId, members]) => (
                    <option key={groupId} value={groupId}>
                      {groupId.slice(0, 8)} · {members.length} {t.trade.groupLegs}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  disabled={closeAll.isPending}
                  onClick={() => closeAll.mutate(positions)}
                  className="ml-auto rounded-md border border-danger/40 px-2 py-1 text-[11px] text-danger transition-colors hover:bg-danger/10 disabled:opacity-50"
                >
                  {closeAll.isPending ? t.trade.closeAllClosing : t.trade.closeAll}
                </button>
              </div>
            )}
            {positionCloseError && (
              <p className="border-b border-line px-4 py-1.5 text-[11px] text-danger">
                {positionCloseError instanceof Error ? positionCloseError.message : t.errors.unknown}
              </p>
            )}
            {filteredPositions.length === 0 ? (
              <p className="px-4 py-8 text-center text-xs text-muted">{t.trade.positionsEmpty}</p>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full min-w-[720px] border-collapse text-xs">
                  <thead>
                    <tr className="border-b border-line text-[10px] tracking-wide text-muted uppercase">
                      <th className="px-4 py-2 text-left font-medium">{t.positions.venue}</th>
                      <th className="px-4 py-2 text-left font-medium">{t.trade.groupLabel}</th>
                      <th className="px-4 py-2 text-left font-medium">{t.positions.side}</th>
                      <th className="px-4 py-2 text-right font-medium">{t.positions.quantity}</th>
                      <th className="px-4 py-2 text-right font-medium">{t.positions.entry}</th>
                      <th className="px-4 py-2 text-right font-medium">{t.positions.mark}</th>
                      <th className="px-4 py-2 text-right font-medium">{t.positions.pnl}</th>
                      <th className="px-4 py-2 text-right font-medium">{t.positions.action}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {pageRows(filteredPositions).map((position) => (
                      <tr key={position.id} className="border-b border-line/70 last:border-0">
                        <td className="px-4 py-2 font-mono text-body uppercase">{position.venue}</td>
                        <td className="px-4 py-2 font-mono text-[10px] text-muted">
                          {position.group_id ? position.group_id.slice(0, 8) : "—"}
                        </td>
                        <td className="px-4 py-2">
                          <Badge tone={position.side === "buy" ? "success" : "danger"}>
                            {position.side === "buy" ? t.positions.long : t.positions.short}
                          </Badge>
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-body">
                          {formatNumber(position.quantity, 6)}
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-body">
                          {formatNumber(position.entry_price, 2)}
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-body">
                          {formatNumber(position.mark_price, 2)}
                        </td>
                        <td
                          className={cn(
                            "px-4 py-2 text-right font-mono",
                            toNumber(position.unrealized_pnl) >= 0 ? "text-accent" : "text-danger",
                          )}
                        >
                          {formatUsd(position.unrealized_pnl)}
                        </td>
                        <td className="px-4 py-2 text-right">
                          <button
                            type="button"
                            disabled={closePosition.isPending}
                            onClick={() => closePosition.mutate(position.id)}
                            className="text-danger transition-opacity hover:opacity-80 disabled:opacity-50"
                          >
                            {t.positions.close}
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}

        {(tab === "openOrders" || tab === "history") &&
          (orderList.length === 0 ? (
            <p className="px-4 py-8 text-center text-xs text-muted">
              {tab === "openOrders" ? t.trade.ordersEmpty : t.trade.historyEmpty}
            </p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[760px] border-collapse text-xs">
                <thead>
                  <tr className="border-b border-line text-[10px] tracking-wide text-muted uppercase">
                    <th className="px-4 py-2 text-left font-medium">{t.positions.order}</th>
                    <th className="px-4 py-2 text-left font-medium">{t.positions.legs}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.notional}</th>
                    <th className="px-4 py-2 text-left font-medium">{t.positions.status}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.time}</th>
                    <th className="px-4 py-2 text-left font-medium">{t.positions.note}</th>
                    {tab === "openOrders" && (
                      <th className="px-4 py-2 text-right font-medium">
                        {t.positions.action}
                      </th>
                    )}
                  </tr>
                </thead>
                <tbody>
                  {pageRows(orderList).map((order) => (
                    <Fragment key={order.id}>
                      <tr className="border-b border-line/70 last:border-0">
                        <td className="px-4 py-2 font-mono text-[11px] text-muted">
                          {order.id.slice(0, 8)}
                        </td>
                        <td className="px-4 py-2 font-mono text-[11px] text-muted">
                          {order.intent.legs
                            .map(
                              (leg) =>
                                `${leg.venue} ${leg.side === "buy" ? t.positions.buy : t.positions.sell}`,
                            )
                            .join(" / ")}
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-body">
                          {formatUsd(
                            order.intent.legs.reduce(
                              (sum, leg) => sum + toNumber(leg.quantity) * toNumber(leg.limit_price),
                              0,
                            ),
                          )}
                        </td>
                        <td className="px-4 py-2">
                          <Badge tone={orderStatusTone[order.status]}>
                            {orderStatusLabel(order.status)}
                          </Badge>
                        </td>
                        <td className="px-4 py-2 text-right font-mono text-[11px] text-muted">
                          {formatTime(order.created_at)}
                        </td>
                        <td className="max-w-[220px] px-4 py-2 text-[11px] text-warning">
                          {order.note ?? "—"}
                        </td>
                        {tab === "openOrders" && (
                          <td className="whitespace-nowrap px-4 py-2 text-right">
                            <div className="flex items-center justify-end gap-2">
                              <button
                                type="button"
                                disabled={replaceOrder.isPending}
                                onClick={() => {
                                  const leg = order.intent.legs[0];
                                  setEditing({
                                    id: order.id,
                                    price: leg ? String(toNumber(leg.limit_price)) : "",
                                    quantity: leg ? String(toNumber(leg.quantity)) : "",
                                  });
                                }}
                                className="text-muted transition-colors hover:text-body disabled:opacity-50"
                              >
                                {t.trade.replaceOrder}
                              </button>
                              <button
                                type="button"
                                disabled={cancelOrder.isPending}
                                onClick={() => cancelOrder.mutate(order.id)}
                                className="text-danger transition-opacity hover:opacity-80 disabled:opacity-50"
                              >
                                {cancelOrder.isPending
                                  ? t.trade.cancelOrdering
                                  : t.trade.cancelOrder}
                              </button>
                            </div>
                          </td>
                        )}
                      </tr>
                      {tab === "openOrders" && editing?.id === order.id && (
                        <tr className="border-b border-line/70 bg-panel-soft/60">
                          <td colSpan={7} className="px-4 py-2">
                            <div className="flex flex-wrap items-end gap-2">
                              <label className="space-y-1">
                                <span className="block text-[10px] text-muted">
                                  {t.trade.replacePrice}
                                </span>
                                <input
                                  value={editing.price}
                                  onChange={(event) =>
                                    setEditing({ ...editing, price: event.target.value })
                                  }
                                  className="w-28 rounded-md border border-line bg-panel px-2 py-1 font-mono text-[11px] text-body outline-none focus:border-accent/60"
                                />
                              </label>
                              <label className="space-y-1">
                                <span className="block text-[10px] text-muted">
                                  {t.trade.replaceQty}
                                </span>
                                <input
                                  value={editing.quantity}
                                  onChange={(event) =>
                                    setEditing({ ...editing, quantity: event.target.value })
                                  }
                                  className="w-28 rounded-md border border-line bg-panel px-2 py-1 font-mono text-[11px] text-body outline-none focus:border-accent/60"
                                />
                              </label>
                              <button
                                type="button"
                                disabled={replaceOrder.isPending}
                                onClick={() => {
                                  const price = Number(editing.price);
                                  if (!Number.isFinite(price) || price <= 0) {
                                    return;
                                  }
                                  const quantity = editing.quantity.trim()
                                    ? editing.quantity.trim()
                                    : null;
                                  replaceOrder.mutate(
                                    {
                                      id: editing.id,
                                      limitPrice: editing.price.trim(),
                                      quantity,
                                    },
                                    { onSuccess: () => setEditing(null) },
                                  );
                                }}
                                className="rounded-md border border-accent/40 px-2 py-1 text-[11px] text-accent transition-colors hover:bg-accent/10 disabled:opacity-50"
                              >
                                {t.trade.replaceSubmit}
                              </button>
                              <button
                                type="button"
                                onClick={() => setEditing(null)}
                                className="rounded-md border border-line px-2 py-1 text-[11px] text-muted transition-colors hover:bg-hover"
                              >
                                {t.trade.replaceCancel}
                              </button>
                              <span className="text-[10px] text-muted">{t.trade.replaceHint}</span>
                            </div>
                            {replaceOrder.error && (
                              <p className="mt-1 text-[11px] text-danger">
                                {replaceOrder.error.message}
                              </p>
                            )}
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  ))}
                </tbody>
              </table>
            </div>
          ))}

        {tab === "trades" &&
          (fills.length === 0 ? (
            <p className="px-4 py-8 text-center text-xs text-muted">{t.trade.tradesEmpty}</p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[760px] border-collapse text-xs">
                <thead>
                  <tr className="border-b border-line text-[10px] tracking-wide text-muted uppercase">
                    <th className="px-4 py-2 text-left font-medium">{t.positions.order}</th>
                    <th className="px-4 py-2 text-left font-medium">{t.positions.venue}</th>
                    <th className="px-4 py-2 text-left font-medium">{t.positions.side}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.trade.orderPrice}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.trade.orderQty}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.notional}</th>
                    <th className="px-4 py-2 text-right font-medium">{t.positions.time}</th>
                  </tr>
                </thead>
                <tbody>
                  {pageRows(fills).map(({ order, leg }) => (
                    <tr
                      key={`${order.id}-${leg.intent.venue}`}
                      className="border-b border-line/70 last:border-0"
                    >
                      <td className="px-4 py-2 font-mono text-[11px] text-muted">
                        {order.id.slice(0, 8)}
                      </td>
                      <td className="px-4 py-2 font-mono text-body uppercase">
                        {leg.intent.venue}
                      </td>
                      <td className="px-4 py-2">
                        <Badge tone={leg.intent.side === "buy" ? "success" : "danger"}>
                          {leg.intent.side === "buy" ? t.positions.long : t.positions.short}
                        </Badge>
                      </td>
                      <td className="px-4 py-2 text-right font-mono text-body">
                        {leg.average_price === null ? "—" : formatNumber(leg.average_price, 4)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono text-body">
                        {formatNumber(leg.filled_quantity, 6)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono text-body">
                        {leg.average_price === null
                          ? "—"
                          : formatUsd(toNumber(leg.filled_quantity) * toNumber(leg.average_price))}
                      </td>
                      <td className="px-4 py-2 text-right font-mono text-[11px] text-muted">
                        {formatTime(order.created_at)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ))}

        {count > BOTTOM_PAGE_SIZE && (
          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-1.5 text-[11px] text-muted">
            <span>{fmt(t.trade.pagerCount, { count })}</span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                aria-label={t.trade.prevPage}
                disabled={currentPage === 0}
                onClick={() => setPage(currentPage - 1)}
                className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
              >
                <ChevronLeft size={13} />
              </button>
              <span className="px-1 font-mono text-[11px]">
                {fmt(t.trade.pageInfo, { page: currentPage + 1, pages: pageCount })}
              </span>
              <button
                type="button"
                aria-label={t.trade.nextPage}
                disabled={currentPage >= pageCount - 1}
                onClick={() => setPage(currentPage + 1)}
                className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
              >
                <ChevronRight size={13} />
              </button>
            </div>
          </div>
        )}
      </CardContent>

      {panicOpen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          role="dialog"
          aria-modal="true"
        >
          <div className="w-full max-w-sm rounded-xl border border-line bg-panel shadow-xl">
            <div className="border-b border-line px-4 py-3">
              <p className="text-sm font-semibold text-fg">{t.trade.panicTitle}</p>
            </div>
            <div className="space-y-3 p-4">
              <p className="text-xs leading-relaxed text-muted">{t.trade.panicDesc}</p>
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.trade.panicInputLabel}</span>
                <input
                  autoFocus
                  value={panicInput}
                  onChange={(event) => setPanicInput(event.target.value)}
                  placeholder={t.trade.panicHint}
                  className="w-full rounded-md border border-line bg-panel-soft px-2 py-1.5 font-mono text-xs text-body outline-none focus:border-danger/60"
                />
              </label>
              {panic.error && (
                <p className="break-words text-[11px] leading-relaxed text-danger">
                  {panic.error.message}
                </p>
              )}
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setPanicOpen(false)}
                  disabled={panic.isPending}
                  className="rounded-md border border-line px-3 py-1.5 text-xs text-muted transition-colors hover:bg-hover disabled:opacity-50"
                >
                  {t.trade.panicCancel}
                </button>
                <button
                  type="button"
                  disabled={panic.isPending || panicInput.trim() !== "PANIC"}
                  onClick={() =>
                    panic.mutate(undefined, {
                      onSuccess: () => {
                        setPanicOpen(false);
                        setPanicInput("");
                      },
                    })
                  }
                  className="rounded-md bg-danger px-3 py-1.5 text-xs font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-40"
                >
                  {t.trade.panicConfirm}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
