import { ArrowLeft } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { FundingChart } from "../components/FundingChart";
import { TradePanel } from "../components/TradePanel";
import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useFundingHistory, useFundingStatus } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatAnnualizedFunding,
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

function annualizedPct(rate: number, intervalSecs: number): number {
  return rate * ((365 * 24 * 3_600) / intervalSecs) * 100;
}

function rateOf(entry: FundingSymbolStatus | null): number | null {
  const value = entry?.predicted_rate ?? entry?.last_funding_rate ?? null;
  return value === null ? null : toNumber(value);
}

function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="rounded-lg border border-line bg-panel-soft px-3 py-2">
      <p className="text-[11px] text-muted">{label}</p>
      <p className="mt-1 font-mono text-sm text-fg">{value}</p>
      {hint !== undefined && <p className="mt-0.5 text-[10px] text-subtle">{hint}</p>}
    </div>
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

  const setParam = (key: string, value: string) => {
    const next = new URLSearchParams(params);
    next.set(key, value);
    setParams(next);
  };

  const legs = [
    {
      index: 1,
      venue: venue1,
      role: direction === "long1_short2" ? "long" : "short",
      entry: direction === "long1_short2" ? longEntry : shortEntry,
      rate: direction === "long1_short2" ? longRate : shortRate,
      points: direction === "long1_short2" ? longPoints : shortPoints,
    },
    {
      index: 2,
      venue: venue2,
      role: direction === "long1_short2" ? "short" : "long",
      entry: direction === "long1_short2" ? shortEntry : longEntry,
      rate: direction === "long1_short2" ? shortRate : longRate,
      points: direction === "long1_short2" ? shortPoints : longPoints,
    },
  ] as const;

  const ready = Boolean(symbol) && available.length >= 2 && Boolean(venue1) && Boolean(venue2);

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

        <div className="flex items-center gap-1 rounded-lg border border-line bg-panel-soft p-0.5">
          {(
            [
              { id: "long1_short2", label: t.trade.long1Short2 },
              { id: "short1_long2", label: t.trade.short1Long2 },
            ] as const
          ).map((item) => (
            <button
              key={item.id}
              type="button"
              disabled={!ready}
              onClick={() => setParam("direction", item.id)}
              className={cn(
                "rounded-md px-2.5 py-1 text-xs transition-colors disabled:opacity-40",
                direction === item.id
                  ? "bg-panel font-medium text-fg shadow-sm"
                  : "text-muted hover:text-body",
              )}
            >
              {item.label}
            </button>
          ))}
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
          <section className="grid gap-4 lg:grid-cols-2">
            {legs.map((leg) => {
              const long = leg.role === "long";
              return (
                <Card key={leg.index} className="min-w-0">
                  <CardHeader className="flex-wrap gap-2">
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
                          <option key={venue} value={venue} disabled={venue === legs[2 - leg.index].venue}>
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
                  </CardHeader>
                  <CardContent className="grid grid-cols-2 gap-3">
                    <Stat
                      label={t.trade.currentRate}
                      value={
                        <span className={cn(long ? "text-accent" : "text-danger")}>
                          {formatFundingRate(leg.rate)}
                        </span>
                      }
                      hint={
                        leg.rate !== null && leg.entry
                          ? formatAnnualizedFunding(leg.rate, leg.entry.funding_interval_secs)
                          : "—"
                      }
                    />
                    <Stat
                      label={t.trade.lastSettled}
                      value={formatFundingRate(leg.entry?.last_funding_rate ?? null)}
                      hint={
                        leg.entry?.last_funding_time
                          ? formatDateTime(leg.entry.last_funding_time)
                          : t.trade.never
                      }
                    />
                    <Stat
                      label={t.trade.settleInterval}
                      value={formatInterval(leg.entry?.funding_interval_secs ?? 8 * 3_600)}
                      hint={
                        leg.entry?.next_funding_at
                          ? fmt(t.trade.nextFunding, {
                              value: formatCountdown(Date.parse(leg.entry.next_funding_at), now),
                            })
                          : t.trade.never
                      }
                    />
                    <Stat
                      label={t.trade.markPrice}
                      value={
                        leg.entry?.mark_price == null
                          ? "—"
                          : formatNumber(leg.entry.mark_price, 2)
                      }
                      hint={
                        leg.entry?.next_funding_at
                          ? formatDateTime(leg.entry.next_funding_at)
                          : undefined
                      }
                    />
                  </CardContent>
                </Card>
              );
            })}
          </section>

          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
            <div className="min-w-0 space-y-4">
              <Card>
                <CardHeader>
                  <div>
                    <CardTitle>{t.trade.netTitle}</CardTitle>
                    <CardDescription>{t.trade.netDesc}</CardDescription>
                  </div>
                  <Badge tone={(netAnn ?? 0) >= 0 ? "success" : "danger"}>
                    {formatPercentValue(netAnn)}
                  </Badge>
                </CardHeader>
                <CardContent className="grid gap-3 sm:grid-cols-3">
                  <Stat label={t.trade.netAnnualizedLabel} value={formatPercentValue(netAnn)} />
                  <Stat
                    label={t.trade.netPerSettleLabel}
                    value={formatFundingRate(netRate)}
                    hint={sameInterval ? undefined : t.trade.intervalMismatch}
                  />
                  <Stat
                    label={t.trade.priceGapLabel}
                    value={
                      priceGapBps === null
                        ? "—"
                        : `${priceGapBps > 0 ? "+" : ""}${priceGapBps.toFixed(1)} bps`
                    }
                  />
                </CardContent>
              </Card>

              <section className="grid gap-4 lg:grid-cols-2">
                {[
                  { key: "long", venue: longVenue, points: longPoints },
                  { key: "short", venue: shortVenue, points: shortPoints },
                ].map((item) => (
                  <Card key={item.key} className="min-w-0">
                    <CardHeader className="py-2.5">
                      <div>
                        <CardTitle className="text-xs">
                          {fmt(t.trade.chartTitle, { venue: item.venue.toUpperCase() })}
                        </CardTitle>
                        <CardDescription>{t.trade.chartDesc}</CardDescription>
                      </div>
                    </CardHeader>
                    <CardContent className="p-2">
                      {item.points.length > 0 ? (
                        <FundingChart rates={item.points} />
                      ) : (
                        <p className="py-12 text-center text-xs text-muted">{t.trade.noChart}</p>
                      )}
                    </CardContent>
                  </Card>
                ))}
              </section>
            </div>

            <TradePanel
              symbol={symbol}
              longVenue={longVenue}
              shortVenue={shortVenue}
              netRate={netRate}
              netAnnualizedPct={netAnn}
            />
          </div>
        </>
      )}
    </div>
  );
}
