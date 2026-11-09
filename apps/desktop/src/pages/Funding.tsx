import { ArrowDown, ArrowUp, ChevronLeft, ChevronRight, History, Search, WifiOff } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useFundingStatus, useFundingSummary } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatAnnualizedFunding,
  formatCountdown,
  formatDateTime,
  formatFundingRate,
  formatInterval,
  formatNumber,
} from "../lib/format";
import { fmt, useI18n, type Copy } from "../lib/i18n";
import type { Decimal, FundingSymbolStatus } from "../lib/types";
import { cn } from "../lib/utils";

type PeriodId = "current" | "1d" | "7d" | "30d";
type RateFilter = "all" | "positive" | "negative";
type SortKey = "symbol" | "price" | "avg" | `venue:${string}`;

const PERIODS: { id: PeriodId; secs: number | null }[] = [
  { id: "current", secs: null },
  { id: "1d", secs: 86_400 },
  { id: "7d", secs: 7 * 86_400 },
  { id: "30d", secs: 30 * 86_400 },
];

const PAGE_SIZES = [10, 20, 50, 100];

interface SymbolRow {
  symbol: string;
  base: string;
  quote: string;
  byVenue: Map<string, FundingSymbolStatus>;
  price: Decimal | null;
  intervalSecs: number;
  nextAt: number | null;
}

interface PeriodRate {
  rate: number;
  count: number;
}

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

/** 价格越小数位越多，保持表格列宽稳定。 */
function priceDigits(value: number): number {
  if (value >= 1_000) return 2;
  if (value >= 1) return 3;
  if (value >= 0.01) return 5;
  return 8;
}

type Flash = "up" | "down";

/**
 * 对比相邻两次状态刷新中每个交易所的当期费率，返回刚发生变化的单元格。
 * 仅在「当期」页签生效，避免区间累计值因补历史而误闪。
 */
function useRateFlashes(rows: SymbolRow[], venues: string[], period: PeriodId) {
  const previous = useRef<Map<string, number>>(new Map());
  const [flashes, setFlashes] = useState<Map<string, Flash>>(() => new Map());

  useEffect(() => {
    if (period !== "current") {
      previous.current = new Map();
      setFlashes(new Map());
      return;
    }

    const next = new Map<string, number>();
    const changed = new Map<string, Flash>();
    for (const row of rows) {
      for (const venue of venues) {
        const entry = row.byVenue.get(venue);
        const value = entry?.predicted_rate ?? entry?.last_funding_rate;
        if (value === null || value === undefined) {
          continue;
        }
        const rate = toNumber(value);
        if (!Number.isFinite(rate)) {
          continue;
        }
        const key = `${venue}:${row.symbol}`;
        next.set(key, rate);
        const before = previous.current.get(key);
        if (before !== undefined && before !== rate) {
          changed.set(key, rate > before ? "up" : "down");
        }
      }
    }
    previous.current = next;

    if (changed.size === 0) {
      setFlashes(new Map());
      return;
    }
    setFlashes(changed);
    const timer = window.setTimeout(() => setFlashes(new Map()), 1_400);
    return () => window.clearTimeout(timer);
  }, [rows, venues, period]);

  return flashes;
}

export default function FundingPage() {
  const status = useFundingStatus();
  const navigate = useNavigate();
  const { t } = useI18n();
  const statuses = useMemo(() => status.data ?? [], [status.data]);

  const [period, setPeriod] = useState<PeriodId>("current");
  const [filter, setFilter] = useState<RateFilter>("all");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<{ key: SortKey; dir: "asc" | "desc" }>({
    key: "symbol",
    dir: "asc",
  });
  const [pageSize, setPageSize] = useState(PAGE_SIZES[0]);
  const [customSize, setCustomSize] = useState("");
  const [page, setPage] = useState(0);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, []);

  const venues = useMemo(() => statuses.map((item) => item.venue), [statuses]);

  // 每个币种一行，交易所作为列，与 Coinglass 的矩阵行情表一致。
  const rows = useMemo<SymbolRow[]>(() => {
    const grouped = new Map<string, Map<string, FundingSymbolStatus>>();
    for (const item of statuses) {
      for (const entry of item.symbols) {
        let byVenue = grouped.get(entry.symbol);
        if (!byVenue) {
          byVenue = new Map();
          grouped.set(entry.symbol, byVenue);
        }
        byVenue.set(item.venue, entry);
      }
    }

    return [...grouped.entries()].map(([symbol, byVenue]) => {
      const entries = [...byVenue.values()];
      const price = entries.find((entry) => entry.mark_price !== null)?.mark_price ?? null;
      const nextAt = Math.min(
        ...entries.map((entry) =>
          entry.next_funding_at === null
            ? Number.POSITIVE_INFINITY
            : Date.parse(entry.next_funding_at),
        ),
      );
      return {
        symbol,
        ...splitSymbol(symbol),
        byVenue,
        price,
        intervalSecs: entries[0]?.funding_interval_secs ?? 8 * 3_600,
        nextAt: Number.isFinite(nextAt) ? nextAt : null,
      };
    });
  }, [statuses]);

  const flashes = useRateFlashes(rows, venues, period);

  const activePeriod = PERIODS.find((item) => item.id === period) ?? PERIODS[0];
  const summary = useFundingSummary(activePeriod.secs);

  // 区间累计由后端一次聚合返回：币种变多后不再逐币拉历史。
  const periodRates = useMemo(() => {
    const map = new Map<string, PeriodRate>();
    if (activePeriod.secs === null) {
      return map;
    }
    for (const item of summary.data ?? []) {
      map.set(`${item.venue}:${item.symbol}`, {
        rate: toNumber(item.rate),
        count: item.count,
      });
    }
    return map;
  }, [activePeriod.secs, summary.data]);

  const rateFor = (row: SymbolRow, venue: string): PeriodRate | null => {
    if (period === "current") {
      const entry = row.byVenue.get(venue);
      if (!entry) {
        return null;
      }
      const value = entry.predicted_rate ?? entry.last_funding_rate;
      return value === null ? null : { rate: toNumber(value), count: 0 };
    }
    return periodRates.get(`${venue}:${row.symbol}`) ?? null;
  };

  const averageRate = (row: SymbolRow): number | null => {
    const values = venues
      .map((venue) => rateFor(row, venue)?.rate)
      .filter((value): value is number => value !== undefined);
    if (values.length === 0) {
      return null;
    }
    return values.reduce((sum, value) => sum + value, 0) / values.length;
  };

  const sortValue = (row: SymbolRow): number | null => {
    if (sort.key === "price") {
      return row.price === null ? null : toNumber(row.price);
    }
    if (sort.key === "avg") {
      return averageRate(row);
    }
    return rateFor(row, sort.key.slice("venue:".length))?.rate ?? null;
  };

  const toggleSort = (key: SortKey) => {
    setSort((current) =>
      current.key === key
        ? { key, dir: current.dir === "asc" ? "desc" : "asc" }
        : { key, dir: key === "symbol" ? "asc" : "desc" },
    );
  };

  const keyword = query.trim().toUpperCase();
  // 每秒的倒计时刷新不应触发重新筛选与排序，这里按实际依赖缓存结果。
  const visibleRows = useMemo(
    () =>
      rows
        .filter((row) => {
          if (keyword && !row.symbol.includes(keyword) && !row.base.includes(keyword)) {
            return false;
          }
          if (filter === "all") {
            return true;
          }
          const average = averageRate(row);
          if (average === null) {
            return false;
          }
          return filter === "positive" ? average > 0 : average < 0;
        })
        .sort((left, right) => {
          const direction = sort.dir === "asc" ? 1 : -1;
          if (sort.key === "symbol") {
            return left.symbol.localeCompare(right.symbol) * direction;
          }
          const leftValue = sortValue(left);
          const rightValue = sortValue(right);
          if (leftValue === null && rightValue === null) {
            return 0;
          }
          if (leftValue === null) {
            return 1;
          }
          if (rightValue === null) {
            return -1;
          }
          return (leftValue - rightValue) * direction;
        }),
    // 依赖覆盖 row / venues / periodRates，rateFor 与 averageRate 均由它们派生。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [rows, keyword, filter, sort, venues, period, periodRates],
  );

  const { highest, lowest, rankCount } = useMemo(() => {
    const ranking = rows.flatMap((row) =>
      venues.flatMap((venue) => {
        const value = rateFor(row, venue);
        if (value === null) {
          return [];
        }
        return [
          {
            symbol: row.symbol,
            base: row.base,
            venue,
            rate: value.rate,
            intervalSecs: row.byVenue.get(venue)?.funding_interval_secs ?? row.intervalSecs,
          },
        ];
      }),
    );
    const sorted = [...ranking].sort((left, right) => right.rate - left.rate);
    // 费率序列较少时均分给两张卡片，避免最高/最低重复展示同一条记录。
    const perCard = Math.min(5, Math.max(1, Math.floor(ranking.length / 2)));
    return {
      highest: sorted.slice(0, perCard),
      lowest: sorted.slice(-perCard).reverse(),
      rankCount: ranking.length,
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows, venues, period, periodRates]);

  const rankSuffix = period === "current" ? "" : ` · ${periodLabel(period, t)}`;

  const pageCount = Math.max(1, Math.ceil(visibleRows.length / pageSize));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRows = useMemo(
    () => visibleRows.slice(currentPage * pageSize, (currentPage + 1) * pageSize),
    [visibleRows, currentPage, pageSize],
  );

  // 搜索、筛选、排序或每页条数变化时回到第一页。
  useEffect(() => {
    setPage(0);
  }, [keyword, filter, sort.key, sort.dir, pageSize, period]);

  const openHistory = (symbol: string, venue?: string) => {
    const params = new URLSearchParams({ symbol });
    if (venue) {
      params.set("venue", venue);
    }
    navigate(`/funding-history?${params.toString()}`);
  };

  /** 打开双腿交易页：腿 1 用点击的交易所，腿 2 取费率差最大的交易所。 */
  const openTrade = (symbol: string, clickedVenue?: string) => {
    const row = rows.find((item) => item.symbol === symbol);
    if (!row) {
      return;
    }
    const priced = venues
      .map((venue) => ({ venue, rate: rateFor(row, venue)?.rate ?? null }))
      .filter((entry): entry is { venue: string; rate: number } => entry.rate !== null);
    if (priced.length === 0) {
      openHistory(symbol, clickedVenue);
      return;
    }
    const first = priced.find((entry) => entry.venue === clickedVenue) ?? priced[0];
    const second = priced
      .filter((entry) => entry.venue !== first.venue)
      .sort((left, right) => Math.abs(right.rate - first.rate) - Math.abs(left.rate - first.rate))[0];
    const query = new URLSearchParams({ symbol });
    query.set("venue1", first.venue);
    if (second) {
      query.set("venue2", second.venue);
      // 费率低的一腿做多、高的一腿做空，才能收到资金费。
      query.set("direction", first.rate <= second.rate ? "long1_short2" : "short1_long2");
    }
    navigate(`/trade?${query.toString()}`);
  };

  const collecting = statuses.some((item) => item.enabled);
  const storedRows = statuses.reduce((sum, item) => sum + item.stored_rows, 0);
  const lastSuccess =
    statuses
      .map((item) => item.last_success_at)
      .filter((value): value is string => Boolean(value))
      .sort()
      .at(-1) ?? null;

  const sortIcon = (key: SortKey) => {
    if (sort.key !== key) {
      return null;
    }
    return sort.dir === "asc" ? <ArrowUp size={11} /> : <ArrowDown size={11} />;
  };

  return (
    <div className="space-y-4">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold text-fg">{t.funding.title}</h1>
          <p className="mt-1 text-sm text-muted">{t.funding.subtitle}</p>
        </div>
        {statuses.length > 0 && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted">
            <span className="inline-flex items-center gap-1.5">
              <span
                className={cn(
                  "h-1.5 w-1.5 rounded-full",
                  collecting ? "bg-accent live-dot" : "bg-subtle",
                )}
              />
              {collecting ? t.funding.enabled : t.funding.disabled}
            </span>
            {venues.length > 0 && (
              <span>{fmt(t.funding.venuesActive, { count: venues.length })}</span>
            )}
            <span>{fmt(t.funding.storedRows, { value: formatNumber(storedRows, 0) })}</span>
            <span>{fmt(t.funding.lastSuccess, { value: formatDateTime(lastSuccess) })}</span>
          </div>
        )}
      </header>

      {status.isError && (
        <div className="flex items-center gap-2 rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          <WifiOff size={14} />
          {t.funding.unavailable}
        </div>
      )}

      {statuses
        .filter((item) => item.last_error)
        .map((item) => (
          <p
            key={item.venue}
            className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger"
          >
            {fmt(t.funding.error, { message: `${item.venue}: ${item.last_error}` })}
          </p>
        ))}

      {rankCount > 0 && (
        <section className="grid gap-3 md:grid-cols-2">
          <RankCard
            title={`${t.funding.highest}${rankSuffix}`}
            entries={highest}
            onSelect={openTrade}
          />
          <RankCard
            title={`${t.funding.lowest}${rankSuffix}`}
            entries={lowest}
            onSelect={openTrade}
          />
        </section>
      )}

      <Card>
        <CardHeader className="flex-wrap gap-3">
          <div>
            <CardTitle>{t.funding.tableTitle}</CardTitle>
            <CardDescription>{t.funding.tableDesc}</CardDescription>
          </div>
          <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
            {PERIODS.map((item) => (
              <button
                key={item.id}
                type="button"
                onClick={() => setPeriod(item.id)}
                className={cn(
                  "rounded-md px-2.5 py-1 text-xs transition-colors",
                  period === item.id
                    ? "bg-panel font-medium text-fg shadow-sm"
                    : "text-muted hover:text-body",
                )}
              >
                {periodLabel(item.id, t)}
              </button>
            ))}
          </div>
        </CardHeader>

        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line px-4 py-2">
          <div className="flex items-center gap-1">
            {(
              [
                ["all", t.funding.filterAll],
                ["positive", t.funding.filterPositive],
                ["negative", t.funding.filterNegative],
              ] as [RateFilter, string][]
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                onClick={() => setFilter(id)}
                className={cn(
                  "rounded-md px-2 py-1 text-xs transition-colors",
                  filter === id
                    ? "bg-accent/10 font-medium text-fg"
                    : "text-muted hover:bg-hover hover:text-body",
                )}
              >
                {label}
              </button>
            ))}
          </div>
          <label className="relative">
            <Search
              size={13}
              className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-subtle"
            />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t.funding.searchPlaceholder}
              className="w-44 rounded-md border border-line bg-panel-soft py-1 pr-2 pl-7 text-xs text-body outline-none placeholder:text-subtle focus:border-accent/60"
            />
          </label>
        </div>

        {rows.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-muted">{t.funding.empty}</div>
        ) : visibleRows.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-muted">{t.funding.noMatch}</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[560px] border-collapse text-sm">
              <thead>
                <tr className="border-b border-line text-[11px] tracking-wide text-muted uppercase">
                  <th className="w-10 px-3 py-2 text-left font-medium">#</th>
                  <th className="sticky left-0 z-10 bg-panel px-3 py-2 text-left font-medium">
                    <button
                      type="button"
                      onClick={() => toggleSort("symbol")}
                      className={cn(
                        "inline-flex items-center gap-1 uppercase",
                        sort.key === "symbol" ? "text-fg" : "hover:text-body",
                      )}
                    >
                      {t.funding.colSymbol}
                      {sortIcon("symbol")}
                    </button>
                  </th>
                  <th className="px-3 py-2 text-right font-medium">
                    <button
                      type="button"
                      onClick={() => toggleSort("price")}
                      className={cn(
                        "inline-flex items-center gap-1 uppercase",
                        sort.key === "price" ? "text-fg" : "hover:text-body",
                      )}
                    >
                      {t.funding.colPrice}
                      {sortIcon("price")}
                    </button>
                  </th>
                  {venues.map((venue) => {
                    const key: SortKey = `venue:${venue}`;
                    return (
                      <th key={venue} className="px-3 py-2 text-right font-medium">
                        <button
                          type="button"
                          onClick={() => toggleSort(key)}
                          className={cn(
                            "inline-flex items-center gap-1.5 uppercase",
                            sort.key === key ? "text-fg" : "hover:text-body",
                          )}
                        >
                          <span className="flex h-4 w-4 items-center justify-center rounded-sm border border-line bg-panel-soft text-[9px] font-semibold text-body">
                            {venue.slice(0, 1).toUpperCase()}
                          </span>
                          {venue}
                          {sortIcon(key)}
                        </button>
                      </th>
                    );
                  })}
                  <th className="w-9 px-2 py-2" />
                </tr>
              </thead>
              <tbody>
                {pageRows.map((row, index) => {
                  const rowNumber = currentPage * pageSize + index + 1;
                  return (
                    <tr
                      key={row.symbol}
                      onClick={() => openTrade(row.symbol)}
                      title={t.funding.openTrade}
                      className="group cursor-pointer border-b border-line/70 transition-colors hover:bg-hover"
                    >
                      <td className="px-3 py-2.5 font-mono text-[11px] text-subtle">
                        {rowNumber}
                      </td>
                      <td className="sticky left-0 z-10 bg-panel px-3 py-2.5 transition-colors group-hover:bg-hover">
                        <div className="flex items-center gap-2.5">
                          <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-line bg-panel-soft text-[10px] font-semibold text-body">
                            {row.base.slice(0, 2)}
                          </span>
                          <div className="min-w-0">
                            <div className="flex items-baseline gap-1 whitespace-nowrap">
                              <span className="text-sm font-semibold text-fg">{row.base}</span>
                              <span className="text-[11px] text-subtle">/{row.quote}</span>
                            </div>
                            <div className="mt-0.5 flex items-center gap-1.5 font-mono text-[10px] text-subtle">
                              <span
                                title={
                                  [...row.byVenue.entries()]
                                    .filter(([, entry]) => entry.next_funding_at !== null)
                                    .map(
                                      ([venue, entry]) =>
                                        `${venue} ${formatDateTime(entry.next_funding_at)}`,
                                    )
                                    .join(" · ") || undefined
                                }
                              >
                                {formatCountdown(row.nextAt, now)}
                              </span>
                            </div>
                          </div>
                        </div>
                      </td>
                      <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                        {row.price === null
                          ? "—"
                          : formatNumber(row.price, priceDigits(toNumber(row.price)))}
                      </td>
                      {venues.map((venue) => {
                        const value = rateFor(row, venue);
                        const entry = row.byVenue.get(venue);
                        const flash = flashes.get(`${venue}:${row.symbol}`);
                        // 各交易所结算间隔不同，单元格只用它自己的间隔折算年化。
                        const intervalSecs = entry?.funding_interval_secs ?? row.intervalSecs;
                        const predicted =
                          period === "current" &&
                          entry !== undefined &&
                          entry.predicted_rate !== null;
                        return (
                          <td
                            key={venue}
                            onClick={(event) => {
                              event.stopPropagation();
                              openTrade(row.symbol, venue);
                            }}
                            className={cn(
                              "cursor-pointer px-3 py-2.5 text-right align-top",
                              flash === "up" && "funding-flash-up",
                              flash === "down" && "funding-flash-down",
                            )}
                          >
                            {value === null ? (
                              <div className="font-mono text-[13px] text-subtle">—</div>
                            ) : (
                              <>
                                <div
                                  className={cn(
                                    "font-mono text-[13px] font-medium",
                                    value.rate >= 0 ? "text-accent" : "text-danger",
                                  )}
                                  title={[
                                    fmt(t.funding.settleInterval, {
                                      value: formatInterval(intervalSecs),
                                    }),
                                    period === "current"
                                      ? predicted
                                        ? t.funding.predicted
                                        : t.funding.settledTag
                                      : null,
                                    entry?.next_funding_at
                                      ? `${t.funding.nextFunding} ${formatDateTime(entry.next_funding_at)}`
                                      : null,
                                    flash === "up"
                                      ? t.funding.flashUp
                                      : flash === "down"
                                        ? t.funding.flashDown
                                        : null,
                                  ]
                                    .filter(Boolean)
                                    .join(" · ")}
                                >
                                  {formatFundingRate(value.rate)}
                                  {predicted && (
                                    <sup className="ml-0.5 text-[9px] font-normal text-subtle">
                                      P
                                    </sup>
                                  )}
                                  {flash && (
                                    <span className="ml-0.5 text-[9px]" aria-hidden>
                                      {flash === "up" ? "▲" : "▼"}
                                    </span>
                                  )}
                                </div>
                                <div className="mt-0.5 font-mono text-[10px] text-subtle">
                                  <span>{formatInterval(intervalSecs)}</span>
                                  <span className="mx-1">·</span>
                                  {period === "current"
                                    ? fmt(t.funding.annualizedShort, {
                                        value: formatAnnualizedFunding(value.rate, intervalSecs),
                                      })
                                    : fmt(t.funding.settlements, { count: value.count })}
                                </div>
                              </>
                            )}
                          </td>
                        );
                      })}
                      <td className="px-2 py-2.5 text-right">
                        <button
                          type="button"
                          aria-label={t.funding.openHistory}
                          title={t.funding.openHistory}
                          onClick={(event) => {
                            event.stopPropagation();
                            openHistory(
                              row.symbol,
                              venues.find((venue) => rateFor(row, venue) !== null),
                            );
                          }}
                          className="rounded-md p-1 text-subtle transition-colors hover:bg-hover hover:text-body"
                        >
                          <History size={13} />
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-2 text-[11px] text-muted">
          <div className="flex items-center gap-2">
            <span>{fmt(t.funding.rowsCount, { count: visibleRows.length })}</span>
            <select
              value={PAGE_SIZES.includes(pageSize) ? String(pageSize) : "custom"}
              onChange={(event) => {
                setPageSize(Number(event.target.value));
                setCustomSize("");
              }}
              className="rounded-md border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-body outline-none focus:border-accent/60"
            >
              {PAGE_SIZES.map((value) => (
                <option key={value} value={value}>
                  {fmt(t.funding.perPage, { count: value })}
                </option>
              ))}
              <option value="custom" disabled>
                —
              </option>
            </select>
            <input
              type="number"
              min={1}
              max={500}
              value={customSize}
              placeholder={t.funding.customPerPage}
              aria-label={t.funding.customPerPage}
              onChange={(event) => {
                const text = event.target.value;
                setCustomSize(text);
                if (text === "") {
                  setPageSize(PAGE_SIZES[0]);
                  return;
                }
                const next = Number(text);
                if (Number.isFinite(next) && next >= 1) {
                  setPageSize(Math.min(500, Math.floor(next)));
                }
              }}
              className="w-24 rounded-md border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-body outline-none focus:border-accent/60"
            />
          </div>
          <div className="flex items-center gap-1">
            <button
              type="button"
              aria-label={t.funding.prevPage}
              disabled={currentPage === 0}
              onClick={() => setPage(currentPage - 1)}
              className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
            >
              <ChevronLeft size={13} />
            </button>
            <span className="px-1 font-mono text-[11px]">
              {fmt(t.funding.pageInfo, { page: currentPage + 1, pages: pageCount })}
            </span>
            <button
              type="button"
              aria-label={t.funding.nextPage}
              disabled={currentPage >= pageCount - 1}
              onClick={() => setPage(currentPage + 1)}
              className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
            >
              <ChevronRight size={13} />
            </button>
          </div>
          <span>{fmt(t.funding.poll, { value: statuses[0]?.poll_secs ?? 60 })}</span>
        </div>
      </Card>
    </div>
  );
}

function periodLabel(id: PeriodId, t: Copy): string {
  switch (id) {
    case "1d":
      return t.funding.period1d;
    case "7d":
      return t.funding.period7d;
    case "30d":
      return t.funding.period30d;
    default:
      return t.funding.periodCurrent;
  }
}

function RankCard({
  title,
  entries,
  onSelect,
}: {
  title: string;
  entries: { symbol: string; base: string; venue: string; rate: number; intervalSecs: number }[];
  onSelect: (symbol: string, venue: string) => void;
}) {
  return (
    <Card>
      <CardHeader className="py-2.5">
        <CardTitle className="text-xs">{title}</CardTitle>
      </CardHeader>
      <CardContent className="divide-y divide-line p-0">
        {entries.map((entry) => (
          <button
            key={`${entry.venue}:${entry.symbol}`}
            type="button"
            onClick={() => onSelect(entry.symbol, entry.venue)}
            className="flex w-full items-center justify-between gap-3 px-4 py-2 text-left transition-colors hover:bg-hover"
          >
            <span className="flex min-w-0 items-baseline gap-2">
              <span className="text-sm font-medium text-fg">{entry.base}</span>
              <span className="truncate text-[10px] text-subtle uppercase">
                {entry.venue} · {formatInterval(entry.intervalSecs)}
              </span>
            </span>
            <span
              className={cn(
                "font-mono text-xs font-medium",
                entry.rate >= 0 ? "text-accent" : "text-danger",
              )}
            >
              {formatFundingRate(entry.rate)}
            </span>
          </button>
        ))}
      </CardContent>
    </Card>
  );
}
