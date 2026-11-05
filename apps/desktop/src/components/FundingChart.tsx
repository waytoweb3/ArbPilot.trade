import {
  ColorType,
  createChart,
  createSeriesMarkers,
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
  /** 当前高亮的数据点时间（Unix 秒）。 */
  selectedTime?: number | null;
  /** 点击数据点时回调对应时间；点空白处回调 `null`。 */
  onSelectTime?: (time: number | null) => void;
}

/** 资金费率走势折线（通常按日聚合），负值表示空头向多头收取。 */
export function FundingChart({ rates, selectedTime, onSelectTime }: FundingChartProps) {
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

    const series = chart.addSeries(LineSeries, {
      color: accent,
      lineWidth: 2,
      priceFormat: { type: "price", precision: 4, minMove: 0.0001 },
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: true,
    });

    // 无论入参顺序如何，图表都要求时间升序。
    const data: LineData<UTCTimestamp>[] = rates
      .map((rate) => ({
        time: Math.floor(new Date(rate.funding_time).getTime() / 1000) as UTCTimestamp,
        value: toNumber(rate.funding_rate) * 100,
      }))
      .sort((left, right) => Number(left.time) - Number(right.time));

    series.setData(data);
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
  }, [rates, resolved, selectedTime, onSelectTime]);

  return <div ref={containerRef} className="h-[240px] w-full" />;
}
