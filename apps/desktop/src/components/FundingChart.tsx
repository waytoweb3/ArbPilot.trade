import {
  ColorType,
  createChart,
  createSeriesMarkers,
  HistogramSeries,
  LineSeries,
  LineStyle,
  type LineData,
  type UTCTimestamp,
} from "lightweight-charts";
import { useEffect, useRef } from "react";

import { toNumber } from "../lib/api";
import { useTheme } from "../lib/theme";
import type { FundingRate } from "../lib/types";

interface FundingChartProps {
  rates: FundingRate[];
  /** 对比腿的费率曲线，用于双腿套利页面。 */
  compare?: FundingRate[];
  /** 净费率柱状数据，`value` 已是百分数。 */
  net?: { time: number; value: number }[];
  /** 图表高度，默认 240px。 */
  heightClass?: string;
  /** 当前高亮的数据点时间（Unix 秒）。 */
  selectedTime?: number | null;
  /** 点击数据点时回调对应时间；点空白处回调 `null`。 */
  onSelectTime?: (time: number | null) => void;
}

/** 同一时间戳只保留最后一个点，避免 lightweight-charts 因时间乱序报错。 */
function toLineData(rates: FundingRate[]): LineData<UTCTimestamp>[] {
  const byTime = new Map<number, number>();
  for (const rate of rates) {
    byTime.set(Math.floor(new Date(rate.funding_time).getTime() / 1000), toNumber(rate.funding_rate) * 100);
  }
  return [...byTime.entries()]
    .sort((left, right) => left[0] - right[0])
    .map(([time, value]) => ({ time: time as UTCTimestamp, value }));
}

/** 资金费率走势折线（通常按日聚合），负值表示空头向多头收取。 */
export function FundingChart({
  rates,
  compare,
  net,
  heightClass = "h-[240px] w-full",
  selectedTime,
  onSelectTime,
}: FundingChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const { resolved } = useTheme();

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const dark = resolved === "dark";
    const axis = dark ? "#8291a5" : "#64748b";
    const grid = dark ? "rgba(39, 54, 74, 0.45)" : "rgba(203, 213, 225, 0.55)";
    const border = dark ? "#1b2532" : "#e2e8f0";
    const accent = dark ? "#60a5fa" : "#2563eb";
    const compareColor = dark ? "#fbbf24" : "#d97706";
    const netUp = dark ? "#34d399" : "#059669";
    const netDown = dark ? "#fb7185" : "#e11d48";

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
      timeScale: { borderColor: border, timeVisible: false, secondsVisible: false },
    });

    const priceFormat = { type: "price" as const, precision: 4, minMove: 0.0001 };

    // 柱状放在底层，折线压在上面，避免遮挡。
    if (net && net.length > 0) {
      const histogram = chart.addSeries(HistogramSeries, {
        priceFormat,
        priceLineVisible: false,
        lastValueVisible: false,
        base: 0,
      });
      histogram.setData(
        [...net]
          .sort((left, right) => left.time - right.time)
          .map((point) => ({
            time: point.time as UTCTimestamp,
            value: point.value,
            color: point.value >= 0 ? netUp : netDown,
          })),
      );
    }

    const series = chart.addSeries(LineSeries, {
      color: accent,
      lineWidth: 2,
      priceFormat,
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: true,
    });
    const data = toLineData(rates);
    series.setData(data);

    if (compare && compare.length > 0) {
      const compareSeries = chart.addSeries(LineSeries, {
        color: compareColor,
        lineWidth: 2,
        priceFormat,
        priceLineVisible: false,
        lastValueVisible: true,
        crosshairMarkerVisible: true,
      });
      compareSeries.setData(toLineData(compare));
    }

    if (data.length > 0) {
      series.createPriceLine({
        price: 0,
        color: border,
        lineWidth: 1,
        lineStyle: LineStyle.Dashed,
        axisLabelVisible: false,
        title: "",
      });
      chart.timeScale().fitContent();
    }

    const markerTime =
      selectedTime !== null &&
      selectedTime !== undefined &&
      data.some((point) => Number(point.time) === selectedTime);
    createSeriesMarkers(
      series,
      markerTime
        ? [
            {
              time: selectedTime as UTCTimestamp,
              position: "inBar",
              color: accent,
              shape: "circle",
              size: 1.4,
            },
          ]
        : [],
    );

    if (onSelectTime) {
      chart.subscribeClick((param) => {
        onSelectTime(param.time ? Number(param.time) : null);
      });
    }

    return () => chart.remove();
  }, [rates, compare, net, resolved, selectedTime, onSelectTime]);

  return <div ref={containerRef} className={heightClass} />;
}
