import { ArrowLeft, ArrowLeftRight } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { FundingChart } from "../components/FundingChart";
import { ConditionalOrderPanel } from "../components/ConditionalOrderPanel";
import { OrderBookDepth } from "../components/OrderBookDepth";
import { PositionsOrdersPanel } from "../components/PositionsOrdersPanel";
import { PriceChart, type PriceSeries } from "../components/PriceChart";
import { TradePanel } from "../components/TradePanel";
import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import {
  useCandles,
  useFundingHistory,
  useFundingStatus,
  useOrders,
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
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { FundingRate, FundingSymbolStatus } from "../lib/types";
import { cn } from "../lib/utils";

type Direction = "long1_short2" | "short1_long2";
type ChartMode = "price" | "funding";
/** 资金费率图表的展示窗口，单位为小时。 */
type FundingRange = 1 | 4 | 8 | 24;

const PRICE_INTERVALS = ["30s", "1m", "5m", "10m", "30m", "1h", "4h"] as const;

const FUNDING_RANGES: FundingRange[] = [1, 4, 8, 24];

/** 盘口与图表共用同一高度，宽屏下底边对齐。 */
const WORKSPACE_CARD_HEIGHT =
  "xl:flex xl:h-[calc(100vh_-_330px)] xl:min-h-[520px] xl:max-h-[700px] xl:flex-col";
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

  // 交易页可直接切换币种，只保留至少两个交易所有报价的标的。
  const symbolOptions = useMemo(() => {
    const counts = new Map<string, number>();
    for (const item of statuses) {
      for (const entry of item.symbols) {
        counts.set(entry.symbol, (counts.get(entry.symbol) ?? 0) + 1);
      }
    }
    return [...counts.entries()]
      .filter(([, count]) => count >= 2)
      .map(([value]) => value)
      .sort();
  }, [statuses]);

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

  const longPoints = longHistory.data ?? [];
  const shortPoints = shortHistory.data ?? [];

  const [chartMode, setChartMode] = useState<ChartMode>("price");
  const [priceInterval, setPriceInterval] = useState<(typeof PRICE_INTERVALS)[number]>("1m");
  const [fundingRange, setFundingRange] = useState<FundingRange>(24);
  // 交易对支持手输：本地草稿 + 联想列表，回车或点选后才提交到 URL 参数。
  const [symbolDraft, setSymbolDraft] = useState(symbol);
  const [symbolListOpen, setSymbolListOpen] = useState(false);

  useEffect(() => {
    setSymbolDraft(symbol);
  }, [symbol]);

  const symbolMatches = useMemo(() => {
    const query = symbolDraft.trim().toUpperCase();
    const options = query ? symbolOptions.filter((item) => item.includes(query)) : symbolOptions;
    return options.slice(0, 50);
  }, [symbolDraft, symbolOptions]);

  const commitSymbol = (value: string) => {
    const next = value.trim().toUpperCase();
    setSymbolDraft(next);
    setSymbolListOpen(false);
    if (next && next !== symbol) {
      setParam("symbol", next);
    }
  };

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

  // 部分交易所（如 OKX）的资金费接口不带标记价，用实时盘口中间价兜底。
  const priceOf = (venue: string, entry: FundingSymbolStatus | null): number | null => {
    const markPrice = entry?.mark_price;
    if (markPrice != null) {
      return toNumber(markPrice);
    }
    const live = livePrices[venue]?.price;
    return live !== undefined && Number.isFinite(live) && live > 0 ? live : null;
  };

  const longPrice = priceOf(longVenue, longEntry);
  const shortPrice = priceOf(shortVenue, shortEntry);
  const priceGapBps =
    longPrice !== null && longPrice > 0 && shortPrice !== null
      ? ((shortPrice - longPrice) / longPrice) * 10_000
      : null;

  const longView = useMemo(() => {
    const since = Date.now() - fundingRange * 3_600_000;
    return longPoints.filter((point) => Date.parse(point.funding_time) >= since);
  }, [longPoints, fundingRange]);

  const shortView = useMemo(() => {
    const since = Date.now() - fundingRange * 3_600_000;
    return shortPoints.filter((point) => Date.parse(point.funding_time) >= since);
  }, [shortPoints, fundingRange]);

  // 净费率柱：按结算时刻对齐两腿，空腿减多腿。
  const netBars = useMemo(() => {
    const settlementKey = (point: FundingRate) =>
      Math.floor(new Date(point.funding_time).getTime() / 1000);
    const shortByTime = new Map(
      shortView.map((point) => [settlementKey(point), toNumber(point.funding_rate)]),
    );
    return longView.flatMap((point) => {
      const shortValue = shortByTime.get(settlementKey(point));
      if (shortValue === undefined) {
        return [];
      }
      return [
        {
          time: settlementKey(point),
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
  const symbolPositions = useMemo(
    () => (positions.data ?? []).filter((position) => position.symbol === symbol),
    [positions.data, symbol],
  );

  const orders = useOrders();
  const symbolOrders = useMemo(
    () =>
      (orders.data ?? []).filter((order) =>
        order.intent.legs.some((leg) => leg.symbol === symbol),
      ),
    [orders.data, symbol],
  );

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
        <div className="flex items-center gap-2">
          <span className="text-[11px] text-muted">{t.trade.symbolLabel}</span>
          <div className="relative">
            <input
              value={symbolDraft}
              aria-label={t.trade.symbolLabel}
              autoComplete="off"
              spellCheck={false}
              placeholder="BTCUSDT"
              onChange={(event) => {
                setSymbolDraft(event.target.value.toUpperCase());
                setSymbolListOpen(true);
              }}
              onFocus={() => setSymbolListOpen(true)}
              onBlur={() => setSymbolListOpen(false)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  commitSymbol(symbolDraft);
                } else if (event.key === "Escape") {
                  setSymbolDraft(symbol);
                  setSymbolListOpen(false);
                }
              }}
              className="w-[180px] rounded-md border border-line bg-panel-soft px-2 py-1.5 font-mono text-xs text-fg uppercase outline-none focus:border-accent/60"
            />
            {symbolListOpen && symbolMatches.length > 0 && (
              <ul className="absolute top-full left-0 z-20 mt-1 max-h-60 w-[180px] overflow-auto rounded-md border border-line bg-panel py-1 shadow-lg">
                {symbolMatches.map((item) => (
                  <li key={item}>
                    <button
                      type="button"
                      // mousedown 先于 input 的 blur 触发，避免点击被关闭联想列表吞掉。
                      onMouseDown={(event) => {
                        event.preventDefault();
                        commitSymbol(item);
                      }}
                      className={cn(
                        "w-full px-2 py-1 text-left font-mono text-xs uppercase text-body transition-colors hover:bg-hover",
                        item === symbol && "text-accent",
                      )}
                    >
                      {item}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
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
                const markPrice = priceOf(leg.venue, leg.entry);
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

          {/* 工作区：左侧「图表 + 盘口」并排，底部面板横跨这两列，右侧下单 */}
          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
            {/* content-start 防止行高被右侧下单列拉高后摊到两行之间 */}
            <div className="order-1 grid min-w-0 gap-4 xl:grid-cols-[minmax(0,1fr)_240px] xl:content-start">
              <Card className={cn(WORKSPACE_CARD_HEIGHT, "xl:col-start-1 xl:row-start-1")}>
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
                              {range === 1
                                ? t.trade.range1h
                                : range === 4
                                  ? t.trade.range4h
                                  : range === 8
                                    ? t.trade.range8h
                                    : t.trade.range1d}
                            </button>
                          ))}
                    </div>
                  </div>
                </CardHeader>
                <CardContent className="p-3 xl:flex xl:min-h-0 xl:flex-1 xl:flex-col">
                  {chartMode === "price" ? (
                    <>
                      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-4 px-1 text-[11px] text-muted">
                        {priceSeries.map((item) => (
                          <LegendDot key={item.venue} color={item.up} label={liveLabel(item)} />
                        ))}
                      </div>
                      <div className="xl:min-h-0 xl:flex-1">
                        <PriceChart
                          series={priceSeries}
                          interval={priceInterval}
                          live={livePrices}
                          fitKey={`${longVenue}:${shortVenue}:${symbol}:${priceInterval}`}
                          heightClass="h-[320px] w-full xl:h-full"
                        />
                      </div>
                      {!hasPriceData && (
                        <p className="shrink-0 pt-2 text-center text-xs text-muted">{t.trade.priceEmpty}</p>
                      )}
                    </>
                  ) : (
                    <>
                      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-4 px-1 text-[11px] text-muted">
                        <LegendDot color="#2563eb" label={`${t.trade.legendLong} ${longVenue.toUpperCase()}`} />
                        <LegendDot color="#d97706" label={`${t.trade.legendShort} ${shortVenue.toUpperCase()}`} />
                        <LegendDot color="#059669" label={t.trade.legendNet} />
                      </div>
                      {hasFundingHistory ? (
                        <div className="xl:min-h-0 xl:flex-1">
                          <FundingChart
                            rates={longView}
                            compare={shortView}
                            net={netBars}
                            heightClass="h-[320px] w-full xl:h-full"
                          />
                        </div>
                      ) : (
                        <p className="py-24 text-center text-xs text-muted">{t.trade.noChart}</p>
                      )}
                    </>
                  )}
                </CardContent>
              </Card>

              {/* 盘口：图表右侧，与图表等高 */}
              <div className="order-3 min-w-0 xl:col-start-2 xl:row-start-1">
                <Card className={WORKSPACE_CARD_HEIGHT}>
                  <CardHeader className="flex-wrap gap-2">
                    <div>
                      <CardTitle>{t.trade.depthTitle}</CardTitle>
                      <CardDescription>{t.trade.depthDesc}</CardDescription>
                    </div>
                  </CardHeader>
                  <CardContent className="grid gap-3 p-3 sm:grid-cols-2 xl:min-h-0 xl:flex-1 xl:grid-cols-1 xl:grid-rows-2">
                    <OrderBookDepth
                      venue={longVenue}
                      symbol={symbol}
                      label={`${t.trade.longBadge} · ${longVenue.toUpperCase()}`}
                      className="min-h-0"
                    />
                    <OrderBookDepth
                      venue={shortVenue}
                      symbol={symbol}
                      label={`${t.trade.shortBadge} · ${shortVenue.toUpperCase()}`}
                      className="min-h-0"
                    />
                  </CardContent>
                </Card>
              </div>

          {/* 持仓 / 委托 / 成交面板：横跨图表与盘口两列 */}
          <Card className="order-2 min-w-0 xl:col-span-2 xl:row-start-2">
            <PositionsOrdersPanel positions={symbolPositions} orders={symbolOrders} />
          </Card>
            </div>

            <div className="order-2 h-fit space-y-4 xl:col-start-2 xl:row-start-1 xl:sticky xl:top-4">
              <TradePanel
                symbol={symbol}
                longVenue={longVenue}
                shortVenue={shortVenue}
                longRate={longRate}
                shortRate={shortRate}
                netRate={netRate}
                netAnnualizedPct={netAnn}
                longPrice={longPrice}
                shortPrice={shortPrice}
              />
              <ConditionalOrderPanel
                symbol={symbol}
                longVenue={longVenue}
                shortVenue={shortVenue}
                netAnnualizedPct={netAnn}
                longPrice={longPrice}
              />
            </div>

          </div>
        </>
      )}
    </div>
  );
}
