import { ArrowUpRight, ChevronLeft, ChevronRight } from "lucide-react";
import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import { useClosePosition, useOrders, usePositions, useStrategies } from "../hooks/useApi";
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

const PAGE_SIZES = [10, 20, 50];

export default function PositionsPage() {
  const positions = usePositions();
  const orders = useOrders();
  const strategies = useStrategies();
  const closePosition = useClosePosition();
  const { t } = useI18n();
  const navigate = useNavigate();
  /** 待确认平仓的仓位；不为空时展示确认弹窗。 */
  const [closing, setClosing] = useState<Position | null>(null);
  // 两张表格各自维护分页状态，切换每页条数时回到第一页。
  const [positionPageSize, setPositionPageSize] = useState(PAGE_SIZES[0]);
  const [positionPage, setPositionPage] = useState(0);
  const [orderPageSize, setOrderPageSize] = useState(PAGE_SIZES[0]);
  const [orderPage, setOrderPage] = useState(0);

  const positionRows = positions.data ?? [];
  const orderRows = orders.data ?? [];
  // 仓位来源由「策略名 · 触发规则名」组成，策略重命名后列表跟随更新。
  const strategyNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const record of strategies.data ?? []) {
      names.set(record.id, record.definition.name);
    }
    return names;
  }, [strategies.data]);
  const positionPageCount = Math.max(1, Math.ceil(positionRows.length / positionPageSize));
  const positionCurrentPage = Math.min(positionPage, positionPageCount - 1);
  const visiblePositions = positionRows.slice(
    positionCurrentPage * positionPageSize,
    (positionCurrentPage + 1) * positionPageSize,
  );
  const orderPageCount = Math.max(1, Math.ceil(orderRows.length / orderPageSize));
  const orderCurrentPage = Math.min(orderPage, orderPageCount - 1);
  const visibleOrders = orderRows.slice(
    orderCurrentPage * orderPageSize,
    (orderCurrentPage + 1) * orderPageSize,
  );

  const totalNotional = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.quantity) * toNumber(position.mark_price),
    0,
  );
  const totalPnl = (positions.data ?? []).reduce(
    (sum, position) => sum + toNumber(position.unrealized_pnl),
    0,
  );

  // 同一次双腿下单产生的仓位共享 group_id，点击这类仓位可回到资金费率交易页。
  const groupMembers = useMemo(() => {
    const groups = new Map<string, Position[]>();
    for (const position of positions.data ?? []) {
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
    return groups;
  }, [positions.data]);

  /** 用仓位组的双腿还原交易页的多空方向与交易所参数。 */
  const openGroupTrade = (position: Position) => {
    if (!position.group_id) {
      return;
    }
    const members = groupMembers.get(position.group_id) ?? [position];
    const longLeg = members.find((leg) => leg.side === "buy");
    const shortLeg = members.find((leg) => leg.side === "sell");
    const params = new URLSearchParams({ symbol: position.symbol });
    if (longLeg) {
      params.set("venue1", longLeg.venue);
    }
    if (shortLeg) {
      params.set("venue2", shortLeg.venue);
    }
    if (!longLeg && !shortLeg) {
      params.set("venue1", position.venue);
    }
    params.set("direction", "long1_short2");
    navigate(`/trade?${params.toString()}`);
  };

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
          {positionRows.length === 0 ? (
            <EmptyState text={t.positions.empty} />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[980px] border-collapse text-sm">
                <thead>
                  <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                    <th className="px-4 py-2 font-medium">{t.positions.venue}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.symbol}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.side}</th>
                    <th className="px-4 py-2 font-medium">{t.positions.source}</th>
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
                  {visiblePositions.map((position) => {
                    const canOpenTrade = Boolean(position.group_id);
                    const source = positionSource(
                      position,
                      strategyNames,
                      t.positions.manualSource,
                    );
                    return (
                      <tr
                        key={position.id}
                        onClick={canOpenTrade ? () => openGroupTrade(position) : undefined}
                        title={canOpenTrade ? t.positions.openTrade : undefined}
                        className={
                          canOpenTrade
                            ? "cursor-pointer border-t border-line transition-colors hover:bg-hover"
                            : "border-t border-line"
                        }
                      >
                        <td className="px-4 py-2.5 text-body">{position.venue}</td>
                        <td className="px-4 py-2.5 font-medium text-fg">
                          <span className="inline-flex items-center gap-1">
                            {position.symbol}
                            {canOpenTrade && <ArrowUpRight size={12} className="text-subtle" />}
                          </span>
                        </td>
                        <td className="px-4 py-2.5">
                          <Badge tone={position.side === "buy" ? "success" : "danger"}>
                            {position.side === "buy" ? t.positions.long : t.positions.short}
                          </Badge>
                        </td>
                        <td className="max-w-[180px] px-4 py-2.5 text-xs text-muted">
                          <span className="block truncate" title={source}>
                            {source}
                          </span>
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
                            onClick={(event) => {
                              event.stopPropagation();
                              closePosition.reset();
                              setClosing(position);
                            }}
                          >
                            {t.positions.close}
                          </Button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
          {positionRows.length > 0 && (
            <TablePager
              totalLabel={fmt(t.positions.rowsCount, { count: positionRows.length })}
              pageLabel={fmt(t.positions.pageInfo, {
                page: positionCurrentPage + 1,
                pages: positionPageCount,
              })}
              prevLabel={t.positions.prevPage}
              nextLabel={t.positions.nextPage}
              perPageLabel={(size) => fmt(t.positions.perPage, { count: size })}
              page={positionCurrentPage}
              pageCount={positionPageCount}
              pageSize={positionPageSize}
              onPageSize={(size) => {
                setPositionPageSize(size);
                setPositionPage(0);
              }}
              onPage={setPositionPage}
            />
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
          {orderRows.length === 0 ? (
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
                  {visibleOrders.map((order) => (
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
          {orderRows.length > 0 && (
            <TablePager
              totalLabel={fmt(t.positions.rowsCount, { count: orderRows.length })}
              pageLabel={fmt(t.positions.pageInfo, {
                page: orderCurrentPage + 1,
                pages: orderPageCount,
              })}
              prevLabel={t.positions.prevPage}
              nextLabel={t.positions.nextPage}
              perPageLabel={(size) => fmt(t.positions.perPage, { count: size })}
              page={orderCurrentPage}
              pageCount={orderPageCount}
              pageSize={orderPageSize}
              onPageSize={(size) => {
                setOrderPageSize(size);
                setOrderPage(0);
              }}
              onPage={setOrderPage}
            />
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

function TablePager({
  totalLabel,
  pageLabel,
  prevLabel,
  nextLabel,
  perPageLabel,
  page,
  pageCount,
  pageSize,
  onPageSize,
  onPage,
}: {
  totalLabel: string;
  pageLabel: string;
  prevLabel: string;
  nextLabel: string;
  perPageLabel: (size: number) => string;
  page: number;
  pageCount: number;
  pageSize: number;
  onPageSize: (size: number) => void;
  onPage: (page: number) => void;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-2 text-[11px] text-muted">
      <div className="flex items-center gap-2">
        <span>{totalLabel}</span>
        <select
          value={pageSize}
          onChange={(event) => onPageSize(Number(event.target.value))}
          className="rounded-md border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-body outline-none focus:border-accent/60"
        >
          {PAGE_SIZES.map((value) => (
            <option key={value} value={value}>
              {perPageLabel(value)}
            </option>
          ))}
        </select>
      </div>
      <div className="flex items-center gap-1">
        <button
          type="button"
          aria-label={prevLabel}
          disabled={page === 0}
          onClick={() => onPage(page - 1)}
          className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
        >
          <ChevronLeft size={13} />
        </button>
        <span className="px-1 font-mono text-[11px]">{pageLabel}</span>
        <button
          type="button"
          aria-label={nextLabel}
          disabled={page >= pageCount - 1}
          onClick={() => onPage(page + 1)}
          className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
        >
          <ChevronRight size={13} />
        </button>
      </div>
    </div>
  );
}

// 仓位来源：规则触发的仓位展示「策略名 · 规则名」，其余标注为手动。
function positionSource(
  position: Position,
  strategyNames: Map<string, string>,
  manualLabel: string,
): string {
  const parts: string[] = [];
  if (position.strategy_id) {
    parts.push(strategyNames.get(position.strategy_id) ?? position.strategy_id.slice(0, 8));
  }
  if (position.source) {
    parts.push(position.source);
  }
  return parts.length > 0 ? parts.join(" · ") : manualLabel;
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
