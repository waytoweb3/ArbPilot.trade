import { ArrowLeft, ArrowLeftRight } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { FundingChart } from "../components/FundingChart";
import { ConditionalOrderPanel } from "../components/ConditionalOrderPanel";
import { OrderBookDepth } from "../components/OrderBookDepth";
import { PriceChart, type PriceSeries } from "../components/PriceChart";
import { TradePanel } from "../components/TradePanel";
import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import {
  useCandles,
  useClosePositionGroup,
  useFundingHistory,
  useFundingStatus,
  usePositions,
} from "../hooks/useApi";
import { usePriceStream } from "../hooks/usePriceStream";
import { toNumber } from "../lib/api";
import {
  formatAnnualizedFunding,
  formatBps,
  formatCountdown,
  formatDateTime,
  formatFundingRate,
  formatInterval,
  formatNumber,
  formatPercentValue,
  formatUsd,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { FundingRate, FundingSymbolStatus, Position } from "../lib/types";
import { cn } from "../lib/utils";

type Direction = "long1_short2" | "short1_long2";
type ChartMode = "price" | "funding";
type FundingRange = 30 | 90 | 180 | "all";

const PRICE_INTERVALS = ["30s", "1m", "5m", "10m"] as const;
const FUNDING_RANGES: FundingRange[] = [30, 90, 180, "all"];
const QUOTES = ["USDT", "USDC", "USD"];

/** `BTCUSDT` → base `BTC`、quote `USDT`。 */
function splitSymbol(symbol: string): { base: string; quote: string } {
  for (const quote of QUOTES) {
    if (symbol.endsWith(quote) && symbol.length > quote.length) {
      return { base: symbol.slice(0, -quote.length), quote };
    }
  }
  return { base: symbol, quote: "" };
}

/** 价格越小数位越多，保持展示稳定。 */
function priceDigits(value: number): number {
  if (value >= 1_000) return 2;
  if (value >= 1) return 3;
  if (value >= 0.01) return 5;
  return 8;
}

/** 每期费率按本地自然日汇总，折线图只画每天的总费率。 */
function dailyRates(rates: FundingRate[], symbol: string, venue: string): FundingRate[] {
  const buckets = new Map<string, { time: number; total: number }>();
  for (const rate of rates) {
    const date = new Date(rate.funding_time);
    const key = `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
    const value = toNumber(rate.funding_rate);
    const bucket = buckets.get(key);
    if (bucket) {
      bucket.total += value;
    } else {
      buckets.set(key, {
        time: Math.floor(
          new Date(date.getFullYear(), date.getMonth(), date.getDate(), 12).getTime() / 1000,
        ),
        total: value,
      });
    }
  }
  return [...buckets.values()]
    .sort((left, right) => left.time - right.time)
    .map((day) => ({
      venue,
      symbol,
      funding_time: new Date(day.time * 1000).toISOString(),
      funding_rate: String(day.total),
      mark_price: null,
    }));
}

function dayKey(rate: FundingRate): string {
  const date = new Date(rate.funding_time);
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
}

function annualizedPct(rate: number, intervalSecs: number): number {
  return rate * ((365 * 24 * 3_600) / intervalSecs) * 100;
}

function rateOf(entry: FundingSymbolStatus | null): number | null {
  const value = entry?.predicted_rate ?? entry?.last_funding_rate ?? null;
  return value === null ? null : toNumber(value);
}

function Stat({
  label,
  value,
  hint,
}: {
  label: string;
  value: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <div className="rounded-lg border border-line bg-panel-soft px-3 py-2">
      <p className="text-[11px] text-muted">{label}</p>
      <p className="mt-1 font-mono text-sm text-fg">{value}</p>
      {hint !== undefined && <p className="mt-0.5 text-[10px] text-subtle">{hint}</p>}
    </div>
  );
}

function LegendDot({ color, label }: { color: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span className="h-2 w-2 rounded-full" style={{ backgroundColor: color }} />
      {label}
    </span>
  );
}

export default function TradePage() {
  const { t } = useI18n();
  const [params, setParams] = useSearchParams();
  const status = useFundingStatus();

  const statuses = useMemo(() => status.data ?? [], [status.data]);
  const symbol = params.get("symbol") ?? "";

  const entryOf = (venue: string): FundingSymbolStatus | null =>
    statuses.find((item) => item.venue === venue)?.symbols.find((entry) => entry.symbol === symbol) ??
    null;

  const available = useMemo(
    () =>
      statuses
        .filter((item) => item.symbols.some((entry) => entry.symbol === symbol))
        .map((item) => item.venue),
    [statuses, symbol],
  );

  const requested1 = params.get("venue1");
  const requested2 = params.get("venue2");
  const venue1 = requested1 && available.includes(requested1) ? requested1 : (available[0] ?? "");
  const venue2 =
    requested2 && available.includes(requested2) && requested2 !== venue1
      ? requested2
      : (available.find((venue) => venue !== venue1) ?? "");

  const direction: Direction =
    params.get("direction") === "short1_long2" ? "short1_long2" : "long1_short2";
  const longVenue = direction === "long1_short2" ? venue1 : venue2;
  const shortVenue = direction === "long1_short2" ? venue2 : venue1;

  const longEntry = entryOf(longVenue);
  const shortEntry = entryOf(shortVenue);

  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, []);

  const longRate = rateOf(longEntry);
  const shortRate = rateOf(shortEntry);

  const longAnn =
    longRate !== null && longEntry
      ? annualizedPct(longRate, longEntry.funding_interval_secs)
      : null;
  const shortAnn =
    shortRate !== null && shortEntry
      ? annualizedPct(shortRate, shortEntry.funding_interval_secs)
      : null;
  const netRate = longRate !== null && shortRate !== null ? shortRate - longRate : null;
  const netAnn = longAnn !== null && shortAnn !== null ? shortAnn - longAnn : null;

  const priceGapBps =
    longEntry?.mark_price != null && shortEntry?.mark_price != null
      ? ((toNumber(shortEntry.mark_price) - toNumber(longEntry.mark_price)) /
          toNumber(longEntry.mark_price)) *
        10_000
      : null;

  const sameInterval =
    longEntry !== null &&
    shortEntry !== null &&
    longEntry.funding_interval_secs === shortEntry.funding_interval_secs;

  const nextAt = useMemo(() => {
    const times = [longEntry?.next_funding_at, shortEntry?.next_funding_at]
      .filter((value): value is string => Boolean(value))
      .map((value) => Date.parse(value));
    return times.length > 0 ? Math.min(...times) : null;
  }, [longEntry, shortEntry]);

  const longHistory = useFundingHistory(
    symbol && longVenue ? symbol : undefined,
    1_000,
    longVenue || "binance",
  );
  const shortHistory = useFundingHistory(
    symbol && shortVenue ? symbol : undefined,
    1_000,
    shortVenue || "binance",
  );

  const longPoints = useMemo(
    () => dailyRates(longHistory.data ?? [], symbol, longVenue),
    [longHistory.data, symbol, longVenue],
  );
  const shortPoints = useMemo(
    () => dailyRates(shortHistory.data ?? [], symbol, shortVenue),
    [shortHistory.data, symbol, shortVenue],
  );

  const [chartMode, setChartMode] = useState<ChartMode>("price");
  const [priceInterval, setPriceInterval] = useState<(typeof PRICE_INTERVALS)[number]>("1m");
  const [fundingRange, setFundingRange] = useState<FundingRange>(90);

  const longCandles = useCandles(symbol, priceInterval, 240, longVenue);
  const shortCandles = useCandles(symbol, priceInterval, 240, shortVenue);
  const priceLegs = useMemo(
    () => [
      { venue: longVenue, symbol },
      { venue: shortVenue, symbol },
    ],
    [longVenue, shortVenue, symbol],
  );
  const livePrices = usePriceStream(priceLegs);

  const longView = useMemo(() => {
    if (fundingRange === "all") {
      return longPoints;
    }
    const since = Date.now() - fundingRange * 86_400_000;
    return longPoints.filter((point) => Date.parse(point.funding_time) >= since);
  }, [longPoints, fundingRange]);

  const shortView = useMemo(() => {
    if (fundingRange === "all") {
      return shortPoints;
    }
    const since = Date.now() - fundingRange * 86_400_000;
    return shortPoints.filter((point) => Date.parse(point.funding_time) >= since);
  }, [shortPoints, fundingRange]);

  // 净费率柱：按自然日对齐两腿，空腿减多腿。
  const netBars = useMemo(() => {
    const shortByDay = new Map(shortView.map((point) => [dayKey(point), toNumber(point.funding_rate)]));
    return longView.flatMap((point) => {
      const shortValue = shortByDay.get(dayKey(point));
      if (shortValue === undefined) {
        return [];
      }
      return [
        {
          time: Math.floor(new Date(point.funding_time).getTime() / 1000),
          value: (shortValue - toNumber(point.funding_rate)) * 100,
        },
      ];
    });
  }, [longView, shortView]);

  // 同标的最优组合：枚举交易所两两配对，按净年化排序取前三个。
  const combos = useMemo(() => {
    const list: { longVenue: string; shortVenue: string; net: number }[] = [];
    for (const long of available) {
      for (const short of available) {
        if (long === short) {
          continue;
        }
        const longStatus = entryOf(long);
        const shortStatus = entryOf(short);
        const longValue = rateOf(longStatus);
        const shortValue = rateOf(shortStatus);
        if (longValue === null || shortValue === null || !longStatus || !shortStatus) {
          continue;
        }
        list.push({
          longVenue: long,
          shortVenue: short,
          net:
            annualizedPct(shortValue, shortStatus.funding_interval_secs) -
            annualizedPct(longValue, longStatus.funding_interval_secs),
        });
      }
    }
    return list.sort((left, right) => right.net - left.net).slice(0, 3);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [available, statuses, symbol]);

  const positions = usePositions();
  const closeGroup = useClosePositionGroup();

  const symbolPositions = useMemo(
    () => (positions.data ?? []).filter((position) => position.symbol === symbol),
    [positions.data, symbol],
  );

  // 同一次双腿下单产生的仓位带同一个 group_id，可整组平仓。
  const positionGroups = useMemo(() => {
    const groups = new Map<string, Position[]>();
    for (const position of symbolPositions) {
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
  }, [symbolPositions]);

  const setParam = (key: string, value: string) => {
    const next = new URLSearchParams(params);
    next.set(key, value);
    setParams(next);
  };

  const applyCombo = (combo: { longVenue: string; shortVenue: string }) => {
    const next = new URLSearchParams(params);
    next.set("venue1", combo.longVenue);
    next.set("venue2", combo.shortVenue);
    next.set("direction", "long1_short2");
    setParams(next);
  };

  const legs = [
    {
      index: 1,
      venue: venue1,
      role: direction === "long1_short2" ? "long" : "short",
      entry: direction === "long1_short2" ? longEntry : shortEntry,
      rate: direction === "long1_short2" ? longRate : shortRate,
    },
    {
      index: 2,
      venue: venue2,
      role: direction === "long1_short2" ? "short" : "long",
      entry: direction === "long1_short2" ? shortEntry : longEntry,
      rate: direction === "long1_short2" ? shortRate : longRate,
    },
  ] as const;

  const ready = Boolean(symbol) && available.length >= 2 && Boolean(venue1) && Boolean(venue2);
  const { base, quote } = splitSymbol(symbol);
  const hasFundingHistory = longView.length > 0 || shortView.length > 0;

  // 双腿永续 K 线叠加：多腿蓝色、空腿橙色，实时价格写入各自的最新一根。
  const priceSeries = useMemo<PriceSeries[]>(() => {
    const list: PriceSeries[] = [];
    if (longVenue) {
      list.push({
        venue: longVenue,
        label: `${t.trade.legendLong} ${longVenue.toUpperCase()}`,
        up: "#3b82f6",
        down: "#1d4ed8",
        candles: longCandles.data ?? [],
      });
    }
    if (shortVenue && shortVenue !== longVenue) {
      list.push({
        venue: shortVenue,
        label: `${t.trade.legendShort} ${shortVenue.toUpperCase()}`,
        up: "#f59e0b",
        down: "#b45309",
        candles: shortCandles.data ?? [],
      });
    }
    return list;
  }, [
    longVenue,
    shortVenue,
    longCandles.data,
    shortCandles.data,
    t.trade.legendLong,
    t.trade.legendShort,
  ]);

  const hasPriceData = priceSeries.some((item) => item.candles.length > 0);
  const liveLabel = (item: PriceSeries) => {
    const tick = livePrices[item.venue];
    return tick
      ? `${item.label} ${formatNumber(tick.price, priceDigits(tick.price))}`
      : item.label;
  };

  return (
    <div className="space-y-4">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <Link
            to="/funding"
            className="inline-flex items-center gap-1 text-xs text-muted transition-colors hover:text-body"
          >
            <ArrowLeft size={13} />
            {t.funding.backToFunding}
          </Link>
          <h1 className="mt-2 text-xl font-semibold text-fg">
            {t.trade.pageTitle}
            {symbol && <span className="ml-2 font-mono text-base text-body">{symbol}</span>}
          </h1>
          <p className="mt-1 text-sm text-muted">
            {fmt(t.trade.pageDesc, {
              pair: `${venue1 ? venue1.toUpperCase() : "—"} / ${venue2 ? venue2.toUpperCase() : "—"}`,
            })}
          </p>
        </div>
      </header>

      {!ready ? (
        <Card>
          <CardContent className="px-4 py-10 text-center text-sm text-muted">
            {t.trade.noPair}
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <div className="flex flex-wrap items-start justify-between gap-3 px-4 py-3">
              <div className="flex items-center gap-3">
                <span className="flex h-10 w-10 items-center justify-center rounded-full border border-line bg-panel-soft text-xs font-semibold text-body">
                  {base.slice(0, 2)}
                </span>
                <div>
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="text-lg font-semibold text-fg">
                      {base}
                      <span className="text-sm text-muted">/{quote}</span>
                    </h2>
                    <Badge tone={(netAnn ?? 0) >= 0 ? "success" : "danger"}>
                      {t.trade.netAnnualizedLabel} {formatPercentValue(netAnn)}
                    </Badge>
                  </div>
                  <p className="mt-0.5 text-[11px] text-muted">
                    {symbol} · {longVenue.toUpperCase()} / {shortVenue.toUpperCase()}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
                  {(
                    [
                      { id: "long1_short2", long: venue1, short: venue2 },
                      { id: "short1_long2", long: venue2, short: venue1 },
                    ] as const
                  ).map((item) => (
                    <button
                      key={item.id}
                      type="button"
                      onClick={() => setParam("direction", item.id)}
                      className={cn(
                        "rounded-md px-2.5 py-1 text-xs transition-colors",
                        direction === item.id
                          ? "bg-panel font-medium text-fg shadow-sm"
                          : "text-muted hover:text-body",
                      )}
                    >
                      {item.long.toUpperCase()} {t.trade.longBadge} / {item.short.toUpperCase()}{" "}
                      {t.trade.shortBadge}
                    </button>
                  ))}
                </div>
                <button
                  type="button"
                  aria-label={t.trade.swapDirection}
                  title={t.trade.swapDirection}
                  onClick={() =>
                    setParam("direction", direction === "long1_short2" ? "short1_long2" : "long1_short2")
                  }
                  className="rounded-md border border-line p-1.5 text-muted transition-colors hover:bg-hover hover:text-body"
                >
                  <ArrowLeftRight size={13} />
                </button>
              </div>
            </div>

            <div className="grid gap-px border-t border-line bg-line sm:grid-cols-2">
              {legs.map((leg) => {
                const long = leg.role === "long";
                const intervalSecs = leg.entry?.funding_interval_secs ?? 8 * 3_600;
                const markPrice =
                  leg.entry?.mark_price === null || leg.entry?.mark_price === undefined
                    ? null
                    : toNumber(leg.entry.mark_price);
                return (
                  <div key={leg.index} className="bg-panel px-4 py-3">
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex items-center gap-2">
                        <Badge tone={long ? "success" : "danger"}>
                          {long ? t.trade.longBadge : t.trade.shortBadge}
                        </Badge>
                        <select
                          value={leg.venue}
                          aria-label={fmt(t.trade.legLabel, { index: leg.index })}
                          onChange={(event) => setParam(`venue${leg.index}`, event.target.value)}
                          className="rounded-md border border-line bg-panel-soft px-2 py-1 font-mono text-xs text-fg uppercase outline-none focus:border-accent/60"
                        >
                          {available.map((venue) => (
                            <option
                              key={venue}
                              value={venue}
                              disabled={venue === legs[2 - leg.index].venue}
                            >
                              {venue.toUpperCase()}
                            </option>
                          ))}
                        </select>
                      </div>
                      <Link
                        to={`/funding-history?symbol=${symbol}&venue=${leg.venue}`}
                        className="text-[11px] text-muted transition-colors hover:text-body"
                      >
                        {t.funding.openHistory}
                      </Link>
                    </div>
                    <div className="mt-2 flex items-end justify-between gap-3">
                      <div>
                        <p
                          className={cn(
                            "font-mono text-lg font-medium",
                            (leg.rate ?? 0) >= 0 ? "text-accent" : "text-danger",
                          )}
                        >
                          {formatFundingRate(leg.rate)}
                        </p>
                        <p className="mt-0.5 text-[10px] text-subtle">
                          {fmt(t.funding.annualizedShort, {
                            value: formatAnnualizedFunding(leg.rate, intervalSecs),
                          })}{" "}
                          · {formatInterval(intervalSecs)}
                        </p>
                      </div>
                      <div className="text-right text-[10px] text-muted">
                        <p>
                          {t.trade.markPrice}{" "}
                          <span className="font-mono text-body">
                            {markPrice === null ? "—" : formatNumber(markPrice, priceDigits(markPrice))}
                          </span>
                        </p>
                        <p className="mt-0.5">
                          {fmt(t.trade.nextFunding, {
                            value: formatCountdown(
                              leg.entry?.next_funding_at ? Date.parse(leg.entry.next_funding_at) : null,
                              now,
                            ),
                          })}
                        </p>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          </Card>

          {combos.length > 0 && (
            <Card>
              <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-2.5">
                <div>
                  <p className="text-xs font-medium text-body">{t.trade.relatedTitle}</p>
                  <p className="text-[10px] text-subtle">{t.trade.relatedDesc}</p>
                </div>
                <div className="flex flex-wrap gap-2">
                  {combos.map((combo) => (
                    <button
                      key={`${combo.longVenue}:${combo.shortVenue}`}
                      type="button"
                      onClick={() => applyCombo(combo)}
                      className="flex items-center gap-2 rounded-md border border-line bg-panel-soft px-2.5 py-1 text-[11px] transition-colors hover:border-accent/50"
                    >
                      <span className="font-mono text-body">
                        {combo.longVenue.toUpperCase()} − {combo.shortVenue.toUpperCase()}
                      </span>
                      <span
                        className={cn(
                          "font-mono",
                          combo.net >= 0 ? "text-accent" : "text-danger",
                        )}
                      >
                        {formatPercentValue(combo.net)}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            </Card>
          )}

          <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
            <Stat
              label={t.trade.netAnnualizedLabel}
              value={
                <span className={(netAnn ?? 0) >= 0 ? "text-accent" : "text-danger"}>
                  {formatPercentValue(netAnn)}
                </span>
              }
              hint={sameInterval ? undefined : t.trade.intervalMismatch}
            />
            <Stat label={t.trade.netPerSettleLabel} value={formatFundingRate(netRate)} />
            <Stat
              label={t.trade.priceGapLabel}
              value={priceGapBps === null ? "—" : formatBps(priceGapBps)}
            />
            <Stat
              label={t.trade.nextSettle}
              value={formatCountdown(nextAt, now)}
              hint={nextAt === null ? undefined : formatDateTime(new Date(nextAt).toISOString())}
            />
          </section>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>{t.trade.positionsTitle}</CardTitle>
                <CardDescription>{fmt(t.positions.summary, { count: symbolPositions.length, value: "—", pnl: "—" })}</CardDescription>
              </div>
              <Link to="/positions" className="text-[11px] text-muted hover:text-body">
                {t.trade.viewAll}
              </Link>
            </CardHeader>
            <CardContent className="p-0">
              {positionGroups.length > 0 && (
                <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2">
                  {positionGroups.map(([groupId, members]) => (
                    <div
                      key={groupId}
                      className="flex items-center gap-2 rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px]"
                    >
                      <span className="text-muted">{t.trade.groupLabel}</span>
                      <span className="font-mono text-body">{groupId.slice(0, 8)}</span>
                      <span className="text-subtle">
                        {members.length} {t.trade.groupLegs}
                      </span>
                      <button
                        type="button"
                        disabled={closeGroup.isPending}
                        onClick={() => closeGroup.mutate(groupId)}
                        className="text-danger transition-opacity hover:opacity-80 disabled:opacity-50"
                      >
                        {closeGroup.isPending ? t.trade.groupClosing : t.trade.closeGroup}
                      </button>
                    </div>
                  ))}
                </div>
              )}
              {closeGroup.error && (
                <p className="border-b border-line px-4 py-1.5 text-[11px] text-danger">
                  {closeGroup.error instanceof Error
                    ? closeGroup.error.message
                    : t.errors.unknown}
                </p>
              )}
              {symbolPositions.length === 0 ? (
                <p className="px-4 py-8 text-center text-xs text-muted">{t.trade.positionsEmpty}</p>
              ) : (
                <div className="overflow-x-auto">
                  <table className="w-full min-w-[620px] border-collapse text-xs">
                    <thead>
                      <tr className="border-b border-line text-[10px] tracking-wide text-muted uppercase">
                        <th className="px-4 py-2 text-left font-medium">{t.positions.venue}</th>
                        <th className="px-4 py-2 text-left font-medium">{t.trade.groupLabel}</th>
                        <th className="px-4 py-2 text-left font-medium">{t.positions.side}</th>
                        <th className="px-4 py-2 text-right font-medium">{t.positions.quantity}</th>
                        <th className="px-4 py-2 text-right font-medium">{t.positions.entry}</th>
                        <th className="px-4 py-2 text-right font-medium">{t.positions.mark}</th>
                        <th className="px-4 py-2 text-right font-medium">{t.positions.pnl}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {symbolPositions.map((position: Position) => (
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
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </CardContent>
          </Card>

          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px]">
            <div className="min-w-0 space-y-4">
              <Card>
                <CardHeader className="flex-wrap gap-2">
                  <div>
                    <CardTitle>
                      {chartMode === "price" ? t.trade.chartPrice : t.trade.fundingChartTitle}
                    </CardTitle>
                    <CardDescription>
                      {chartMode === "price" ? t.trade.priceChartDesc : t.trade.fundingChartDesc}
                    </CardDescription>
                  </div>
                  <div className="flex flex-wrap items-center gap-2">
                    <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
                      {(
                        [
                          ["price", t.trade.chartPrice],
                          ["funding", t.trade.chartFunding],
                        ] as [ChartMode, string][]
                      ).map(([id, label]) => (
                        <button
                          key={id}
                          type="button"
                          onClick={() => setChartMode(id)}
                          className={cn(
                            "rounded-md px-2.5 py-1 text-xs transition-colors",
                            chartMode === id
                              ? "bg-panel font-medium text-fg shadow-sm"
                              : "text-muted hover:text-body",
                          )}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                    <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
                      {chartMode === "price"
                        ? PRICE_INTERVALS.map((interval) => (
                            <button
                              key={interval}
                              type="button"
                              onClick={() => setPriceInterval(interval)}
                              className={cn(
                                "rounded-md px-2 py-1 font-mono text-[11px] transition-colors",
                                priceInterval === interval
                                  ? "bg-panel font-medium text-fg shadow-sm"
                                  : "text-muted hover:text-body",
                              )}
                            >
                              {interval}
                            </button>
                          ))
                        : FUNDING_RANGES.map((range) => (
                            <button
                              key={String(range)}
                              type="button"
                              onClick={() => setFundingRange(range)}
                              className={cn(
                                "rounded-md px-2 py-1 text-[11px] transition-colors",
                                fundingRange === range
                                  ? "bg-panel font-medium text-fg shadow-sm"
                                  : "text-muted hover:text-body",
                              )}
                            >
                              {range === "all"
                                ? t.trade.rangeAll
                                : range === 30
                                  ? t.trade.range30d
                                  : range === 90
                                    ? t.trade.range90d
                                    : t.trade.range180d}
                            </button>
                          ))}
                    </div>
                  </div>
                </CardHeader>
                <CardContent className="p-3">
                  {chartMode === "price" ? (
                    <>
                      <div className="mb-2 flex flex-wrap items-center gap-4 px-1 text-[11px] text-muted">
                        {priceSeries.map((item) => (
                          <LegendDot key={item.venue} color={item.up} label={liveLabel(item)} />
                        ))}
                      </div>
                      <PriceChart
                        series={priceSeries}
                        interval={priceInterval}
                        live={livePrices}
                        fitKey={`${longVenue}:${shortVenue}:${symbol}:${priceInterval}`}
                        heightClass="h-[320px] w-full"
                      />
                      {!hasPriceData && (
                        <p className="pt-2 text-center text-xs text-muted">{t.trade.priceEmpty}</p>
                      )}
                    </>
                  ) : (
                    <>
                      <div className="mb-2 flex flex-wrap items-center gap-4 px-1 text-[11px] text-muted">
                        <LegendDot color="#2563eb" label={`${t.trade.legendLong} ${longVenue.toUpperCase()}`} />
                        <LegendDot color="#d97706" label={`${t.trade.legendShort} ${shortVenue.toUpperCase()}`} />
                        <LegendDot color="#059669" label={t.trade.legendNet} />
                      </div>
                      {hasFundingHistory ? (
                        <FundingChart
                          rates={longView}
                          compare={shortView}
                          net={netBars}
                          heightClass="h-[320px] w-full"
                        />
                      ) : (
                        <p className="py-24 text-center text-xs text-muted">{t.trade.noChart}</p>
                      )}
                    </>
                  )}
                </CardContent>
              </Card>

              <Card>
                <CardHeader>
                  <div>
                    <CardTitle>{t.trade.depthTitle}</CardTitle>
                    <CardDescription>{t.trade.depthDesc}</CardDescription>
                  </div>
                </CardHeader>
                <CardContent className="grid gap-3 sm:grid-cols-2">
                  <OrderBookDepth
                    venue={longVenue}
                    symbol={symbol}
                    label={`${t.trade.longBadge} · ${longVenue.toUpperCase()}`}
                  />
                  <OrderBookDepth
                    venue={shortVenue}
                    symbol={symbol}
                    label={`${t.trade.shortBadge} · ${shortVenue.toUpperCase()}`}
                  />
                </CardContent>
              </Card>
            </div>

            <div className="h-fit space-y-4 xl:sticky xl:top-4">
              <TradePanel
                symbol={symbol}
                longVenue={longVenue}
                shortVenue={shortVenue}
                longRate={longRate}
                shortRate={shortRate}
                netRate={netRate}
                netAnnualizedPct={netAnn}
                longPrice={longEntry?.mark_price == null ? null : toNumber(longEntry.mark_price)}
                shortPrice={shortEntry?.mark_price == null ? null : toNumber(shortEntry.mark_price)}
              />
              <ConditionalOrderPanel
                symbol={symbol}
                longVenue={longVenue}
                shortVenue={shortVenue}
                netAnnualizedPct={netAnn}
                longPrice={longEntry?.mark_price == null ? null : toNumber(longEntry.mark_price)}
              />
            </div>
          </div>
        </>
      )}
    </div>
  );
}
