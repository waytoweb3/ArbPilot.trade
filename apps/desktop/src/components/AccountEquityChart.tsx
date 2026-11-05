import {
  AreaSeries,
  ColorType,
  createChart,
  type AreaData,
  type UTCTimestamp,
} from "lightweight-charts";
import { useEffect, useRef } from "react";

import { toNumber } from "../lib/api";
import { useTheme } from "../lib/theme";
import type { EquityPoint } from "../lib/types";

interface AccountEquityChartProps {
  points: EquityPoint[];
}

/** 已导入账户的合计美元权益走势。 */
export function AccountEquityChart({ points }: AccountEquityChartProps) {
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
    const accent = dark ? "#34d399" : "#059669";

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
      timeScale: { borderColor: border, timeVisible: true, secondsVisible: false },
    });

    const series = chart.addSeries(AreaSeries, {
      lineColor: accent,
      topColor: dark ? "rgba(52, 211, 153, 0.24)" : "rgba(5, 150, 105, 0.18)",
      bottomColor: "rgba(52, 211, 153, 0)",
      lineWidth: 2,
      priceFormat: { type: "price", precision: 2, minMove: 0.01 },
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: true,
    });

    // 图表要求时间严格升序且不重复。
    const byTime = new Map<number, AreaData<UTCTimestamp>>();
    for (const point of points) {
      const time = Math.floor(new Date(point.observed_at).getTime() / 1000);
      byTime.set(time, { time: time as UTCTimestamp, value: toNumber(point.equity_usd) });
    }
    const data = [...byTime.values()].sort((left, right) => Number(left.time) - Number(right.time));

    series.setData(data);
    if (data.length > 0) {
      chart.timeScale().fitContent();
    }

    return () => chart.remove();
  }, [points, resolved]);

  return <div ref={containerRef} className="h-[220px] w-full" />;
}
