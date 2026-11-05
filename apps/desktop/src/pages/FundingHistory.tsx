import { ArrowLeft, ChevronLeft, ChevronRight, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { FundingChart } from "../components/FundingChart";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useFundingHistory, useFundingStatus } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatAnnualizedFunding,
  formatDateTime,
  formatFundingRate,
  formatInterval,
  formatNumber,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { FundingRate } from "../lib/types";
import { cn } from "../lib/utils";

const PAGE_SIZES = [10, 20, 50, 100];
const DAY_RANGES = [3, 7, 14, 30];
const DEFAULT_DAYS = 30;

/** 本地时区的日期键，如 `2026-11-04`。 */
function dayKeyOf(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

/** 当天中午的 Unix 秒，用作图表的日粒度时间点。 */
function dayTimeOf(date: Date): number {
  return Math.floor(
    new Date(date.getFullYear(), date.getMonth(), date.getDate(), 12).getTime() / 1000,
  );
}

interface DailyPoint {
  key: string;
  time: number;
  total: number;
}

export default function FundingHistoryPage() {
  const { t } = useI18n();
  const [params, setParams] = useSearchParams();
  const status = useFundingStatus();

  const symbol = params.get("symbol") ?? "";
  const statuses = status.data ?? [];
  const venues = useMemo(() => statuses.map((item) => item.venue), [statuses]);
  const requested = params.get("venue");
  const venue = requested && venues.includes(requested) ? requested : (venues[0] ?? "binance");

  const days = DAY_RANGES.includes(Number(params.get("days")))
    ? Number(params.get("days"))
    : DEFAULT_DAYS;
  // 图表始终取满保留窗口（约 1 个月），不受天数筛选与分页影响。
  const history = useFundingHistory(symbol || undefined, 1_000, venue);
  const rates = history.data ?? [];

  const [selectedDay, setSelectedDay] = useState<string | null>(null);
  const [pageSize, setPageSize] = useState(PAGE_SIZES[0]);
  const [customSize, setCustomSize] = useState("");
  const [page, setPage] = useState(0);
  useEffect(() => {
    setSelectedDay(null);
  }, [symbol, venue]);

  const intervalSecs =
    statuses
      .find((item) => item.venue === venue)
      ?.symbols.find((entry) => entry.symbol === symbol)?.funding_interval_secs ?? 8 * 3_600;

  // 按自然日聚合，图表只画每天的总费率。
  const daily = useMemo<DailyPoint[]>(() => {
    const buckets = new Map<string, DailyPoint>();
    for (const rate of rates) {
      const date = new Date(rate.funding_time);
      const key = dayKeyOf(date);
      const value = toNumber(rate.funding_rate);
      const bucket = buckets.get(key);
      if (bucket) {
        bucket.total += value;
      } else {
        buckets.set(key, { key, time: dayTimeOf(date), total: value });
      }
    }
    return [...buckets.values()].sort((left, right) => left.time - right.time);
  }, [rates]);

  const chartRates = useMemo<FundingRate[]>(
    () =>
      daily.map((day) => ({
        venue,
        symbol,
        funding_time: new Date(day.time * 1000).toISOString(),
        funding_rate: String(day.total),
        mark_price: null,
      })),
    [daily, symbol, venue],
  );

  const selectedTime = daily.find((day) => day.key === selectedDay)?.time ?? null;

  const handleSelectTime = useCallback(
    (time: number | null) => {
      if (time === null) {
        setSelectedDay(null);
        return;
      }
      // 折线图点击位置未必正好落在数据点上，取最近的一天。
      let nearest: DailyPoint | null = null;
      for (const day of daily) {
        if (nearest === null || Math.abs(day.time - time) < Math.abs(nearest.time - time)) {
          nearest = day;
        }
      }
      if (nearest === null) {
        setSelectedDay(null);
        return;
      }
      const key = nearest.key;
      setSelectedDay((current) => (current === key ? null : key));
    },
    [daily],
  );

  // 以已入库的最新结算时间为锚，回溯最近 N 天，再叠加点击折线选中的那一天。
  const cutoff = useMemo(() => {
    let latest = 0;
    for (const rate of rates) {
      const time = new Date(rate.funding_time).getTime();
      if (time > latest) {
        latest = time;
      }
    }
    return latest - days * 86_400_000;
  }, [rates, days]);

  const filteredRates = useMemo(
    () =>
      rates.filter((rate) => {
        const date = new Date(rate.funding_time);
        if (date.getTime() < cutoff) {
          return false;
        }
        return selectedDay === null || dayKeyOf(date) === selectedDay;
      }),
    [rates, cutoff, selectedDay],
  );

  const pageCount = Math.max(1, Math.ceil(filteredRates.length / pageSize));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRates = useMemo(
    () => filteredRates.slice(currentPage * pageSize, (currentPage + 1) * pageSize),
    [filteredRates, currentPage, pageSize],
  );

  // 切换天数、选中日期或每页条数后回到第一页。
  useEffect(() => {
    setPage(0);
  }, [days, selectedDay, pageSize, symbol, venue]);

  const switchVenue = (next: string) => {
    const query = new URLSearchParams(params);
    query.set("venue", next);
    setParams(query);
  };

  const switchDays = (next: number) => {
    const query = new URLSearchParams(params);
    query.set("days", String(next));
    setParams(query);
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
            {t.funding.historyPageTitle}
            {symbol && <span className="ml-2 font-mono text-base text-body">{symbol}</span>}
          </h1>
          <p className="mt-1 text-sm text-muted">
            {fmt(t.funding.historyPageDesc, {
              symbol: symbol || "—",
              venue: venue.toUpperCase(),
            })}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex flex-wrap items-center gap-1">
            {venues.map((item) => (
              <button
                key={item}
                type="button"
                onClick={() => switchVenue(item)}
                className={cn(
                  "rounded-md px-2 py-1 text-xs transition-colors",
                  venue === item
                    ? "bg-accent/10 font-medium text-fg"
                    : "text-muted hover:bg-hover hover:text-body",
                )}
              >
                {item.toUpperCase()}
              </button>
            ))}
          </div>
          <select
            value={days}
            onChange={(event) => switchDays(Number(event.target.value))}
            className="rounded-md border border-line bg-panel-soft px-2 py-1 text-xs text-body outline-none focus:border-accent/60"
          >
            {DAY_RANGES.map((value) => (
              <option key={value} value={value}>
                {fmt(t.funding.days, { count: value })}
              </option>
            ))}
          </select>
        </div>
      </header>

      {!symbol ? (
        <Card>
          <CardContent className="px-4 py-10 text-center text-sm text-muted">
            {t.funding.empty}
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <CardHeader className="flex-wrap gap-2">
              <div>
                <CardTitle>{t.funding.chartTitle}</CardTitle>
                <CardDescription>
                  {t.funding.chartDesc} · {t.funding.chartDailyHint}
                </CardDescription>
              </div>
              <span className="text-[11px] text-muted">
                {fmt(t.funding.settleInterval, { value: formatInterval(intervalSecs) })}
              </span>
            </CardHeader>
            <CardContent className="p-2">
              {chartRates.length > 0 ? (
                <FundingChart
                  rates={chartRates}
                  selectedTime={selectedTime}
                  onSelectTime={handleSelectTime}
                />
              ) : (
                <p className="py-16 text-center text-sm text-muted">{t.funding.empty}</p>
              )}
            </CardContent>
            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-2 text-[11px] text-muted">
              <div className="flex items-center gap-2">
                <span>{fmt(t.funding.rows, { count: filteredRates.length })}</span>
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
            </div>
          </Card>

          <Card>
            <CardHeader className="flex-wrap gap-2">
              <div>
                <CardTitle>{t.funding.historyTitle}</CardTitle>
                <CardDescription>
                  {fmt(t.funding.historyDesc, { count: filteredRates.length })}
                </CardDescription>
              </div>
              {selectedDay && (
                <button
                  type="button"
                  onClick={() => setSelectedDay(null)}
                  className="inline-flex items-center gap-1 rounded-md bg-accent/10 px-2 py-1 text-[11px] text-fg transition-colors hover:bg-accent/15"
                >
                  {selectedDay}
                  <X size={11} />
                  <span className="text-muted">{t.funding.clearFilter}</span>
                </button>
              )}
            </CardHeader>
            <CardContent className="p-0">
              {filteredRates.length === 0 ? (
                <p className="px-4 py-10 text-center text-sm text-muted">{t.funding.empty}</p>
              ) : (
                <div className="overflow-x-auto">
                  <table className="w-full min-w-[560px] border-collapse text-sm">
                    <thead>
                      <tr className="text-left text-[11px] tracking-wide text-muted uppercase">
                        <th className="px-4 py-2 font-medium">{t.funding.colTime}</th>
                        <th className="px-4 py-2 font-medium">{t.funding.colSymbol}</th>
                        <th className="px-4 py-2 text-right font-medium">{t.funding.colRate}</th>
                        <th className="px-4 py-2 text-right font-medium">
                          {t.funding.colAnnualized}
                        </th>
                        <th className="px-4 py-2 text-right font-medium">
                          {t.funding.colMarkPrice}
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {pageRates.map((rate: FundingRate) => (
                        <tr
                          key={`${rate.venue}-${rate.symbol}-${rate.funding_time}`}
                          className="border-t border-line transition-colors hover:bg-hover"
                        >
                          <td className="px-4 py-2.5 font-mono text-xs text-muted">
                            {formatDateTime(rate.funding_time)}
                          </td>
                          <td className="px-4 py-2.5 font-medium text-fg">{rate.symbol}</td>
                          <td
                            className={cn(
                              "px-4 py-2.5 text-right font-mono text-xs",
                              toNumber(rate.funding_rate) >= 0 ? "text-accent" : "text-danger",
                            )}
                          >
                            {formatFundingRate(rate.funding_rate)}
                          </td>
                          <td className="px-4 py-2.5 text-right font-mono text-xs text-body">
                            {formatAnnualizedFunding(rate.funding_rate, intervalSecs)}
                          </td>
                          <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">
                            {rate.mark_price === null ? "—" : formatNumber(rate.mark_price, 2)}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </CardContent>
          </Card>
        </>
      )}
    </div>
  );
}
