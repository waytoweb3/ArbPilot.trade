import {
  CandlestickSeries,
  ColorType,
  createChart,
  type CandlestickData,
  type IChartApi,
  type ISeriesApi,
  type UTCTimestamp,
} from "lightweight-charts";
import { useEffect, useRef } from "react";

import { toNumber } from "../lib/api";
import { useTheme } from "../lib/theme";
import type { Candle, PriceTick } from "../lib/types";

export interface PriceSeries {
  /** 交易所标识，也是实时价格的键。 */
  venue: string;
  label: string;
  up: string;
  down: string;
  candles: Candle[];
}

interface PriceChartProps {
  /** 双腿永续 K 线，同一张图叠加。 */
  series: PriceSeries[];
  /** 当前周期，实时 tick 按它对齐 K 线边界。 */
  interval: string;
  /** 交易所 → 最新中间价，来自执行器 WebSocket。 */
  live?: Record<string, PriceTick>;
  /** 变化时重新适配视图，避免每次刷新都重置缩放。 */
  fitKey?: string;
  heightClass?: string;
}

const INTERVAL_SECS: Record<string, number> = {
  "30s": 30,
  "1m": 60,
  "5m": 300,
  "10m": 600,
  "30m": 1_800,
  "1h": 3_600,
  "4h": 14_400,
};

/** 同一时间戳只保留最后一根，保证时间严格递增。 */
function candleData(candles: Candle[]): CandlestickData<UTCTimestamp>[] {
  const byTime = new Map<number, CandlestickData<UTCTimestamp>>();
  for (const candle of candles) {
    const time = Math.floor(new Date(candle.open_time).getTime() / 1000) as UTCTimestamp;
    byTime.set(time, {
      time,
      open: toNumber(candle.open),
      high: toNumber(candle.high),
      low: toNumber(candle.low),
      close: toNumber(candle.close),
    });
  }
  return [...byTime.values()].sort((left, right) => Number(left.time) - Number(right.time));
}

/** 双腿永续 K 线叠加图；最新一根由 WebSocket 实时价格驱动。 */
export function PriceChart({
  series,
  interval,
  live,
  fitKey,
  heightClass = "h-[240px] w-full",
}: PriceChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<Map<string, ISeriesApi<"Candlestick">>>(new Map());
  const lastRef = useRef<Map<string, CandlestickData<UTCTimestamp>>>(new Map());
  const fittedRef = useRef<string | null>(null);
  const { resolved } = useTheme();

  // 交易所组合变化时重建图表；颜色由各腿固定，不随主题变化。
  const venues = series.map((item) => item.venue).join("|");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const dark = resolved === "dark";
    const axis = dark ? "#8291a5" : "#64748b";
    const grid = dark ? "rgba(39, 54, 74, 0.45)" : "rgba(203, 213, 225, 0.55)";
    const border = dark ? "#1b2532" : "#e2e8f0";

    const chart = createChart(container, {
      autoSize: true,
      layout: {
        background: { type: ColorType.Solid, color: "transparent" },
        textColor: axis,
        fontSize: 11,
      },
      grid: {
        vertLines: { color: grid },
        horzLines: { color: grid },
      },
      rightPriceScale: { borderColor: border },
      timeScale: { borderColor: border, timeVisible: true, secondsVisible: interval === "30s" },
    });
    chartRef.current = chart;
    seriesRef.current = new Map();
    lastRef.current = new Map();
    fittedRef.current = null;

    for (const item of series) {
      const api = chart.addSeries(CandlestickSeries, {
        upColor: item.up,
        downColor: item.down,
        borderVisible: false,
        wickUpColor: item.up,
        wickDownColor: item.down,
        priceLineVisible: false,
      });
      seriesRef.current.set(item.venue, api);
    }

    return () => {
      chart.remove();
      chartRef.current = null;
      seriesRef.current = new Map();
      lastRef.current = new Map();
    };
    // 只在主题或交易所组合变化时重建，K 线数据由下面的 effect 写入。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resolved, venues, interval]);

  // 历史 K 线写入；刷新时只替换数据，不重置用户的缩放。
  useEffect(() => {
    for (const item of series) {
      const api = seriesRef.current.get(item.venue);
      if (!api) {
        continue;
      }
      const data = candleData(item.candles);
      api.setData(data);
      if (data.length > 0) {
        lastRef.current.set(item.venue, data[data.length - 1]);
      } else {
        lastRef.current.delete(item.venue);
      }
    }
    if (fitKey && fittedRef.current !== fitKey && series.some((item) => item.candles.length > 0)) {
      fittedRef.current = fitKey;
      chartRef.current?.timeScale().fitContent();
    }
  }, [series, fitKey, resolved, interval]);

  // 实时 tick：更新当前 K 线，跨周期时开新的一根。
  useEffect(() => {
    if (!live) {
      return;
    }
    const secs = INTERVAL_SECS[interval] ?? 60;
    for (const [venue, tick] of Object.entries(live)) {
      const api = seriesRef.current.get(venue);
      if (!api || !Number.isFinite(tick.price) || tick.price <= 0) {
        continue;
      }
      const bucket = (Math.floor(tick.ts / 1_000 / secs) * secs) as UTCTimestamp;
      const last = lastRef.current.get(venue);
      if (last && bucket < last.time) {
        continue;
      }
      const next: CandlestickData<UTCTimestamp> =
        last && bucket === last.time
          ? {
              time: last.time,
              open: last.open,
              high: Math.max(last.high, tick.price),
              low: Math.min(last.low, tick.price),
              close: tick.price,
            }
          : {
              time: bucket,
              open: last?.close ?? tick.price,
              high: tick.price,
              low: tick.price,
              close: tick.price,
            };
      api.update(next);
      lastRef.current.set(venue, next);
    }
  }, [live, interval]);

  return <div ref={containerRef} className={heightClass} />;
}
