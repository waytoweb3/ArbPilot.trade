import {
  CandlestickSeries,
  ColorType,
  createChart,
  type CandlestickData,
  type UTCTimestamp,
} from "lightweight-charts";
import { useEffect, useRef } from "react";

interface PriceChartProps {
  symbol: string;
}

/**
 * 模拟行情图表：用于界面联调。
 * 接入真实行情后，将 buildCandles 替换为执行器推送的 K 线数据。
 */
function buildCandles(symbol: string): CandlestickData<UTCTimestamp>[] {
  let seed = Array.from(symbol).reduce((acc, char) => acc + char.charCodeAt(0), 7);
  const random = () => {
    seed = (seed * 9301 + 49297) % 233280;
    return seed / 233280;
  };

  let price = symbol.startsWith("ETH") ? 3_200 : 68_000;
  const now = Math.floor(Date.now() / 1000);
  const candles: CandlestickData<UTCTimestamp>[] = [];

  for (let index = 179; index >= 0; index -= 1) {
    const open = price;
    const close = Math.max(open + (random() - 0.5) * open * 0.004, open * 0.9);
    const high = Math.max(open, close) * (1 + random() * 0.0015);
    const low = Math.min(open, close) * (1 - random() * 0.0015);
    candles.push({
      time: (now - index * 300) as UTCTimestamp,
      open,
      high,
      low,
      close,
    });
    price = close;
  }

  return candles;
}

export function PriceChart({ symbol }: PriceChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }

    const chart = createChart(container, {
      autoSize: true,
      layout: {
        background: { type: ColorType.Solid, color: "transparent" },
        textColor: "#8291a5",
        fontSize: 11,
      },
      grid: {
        vertLines: { color: "rgba(39, 54, 74, 0.45)" },
        horzLines: { color: "rgba(39, 54, 74, 0.45)" },
      },
      rightPriceScale: { borderColor: "#1b2532" },
      timeScale: { borderColor: "#1b2532", timeVisible: true, secondsVisible: false },
    });

    const series = chart.addSeries(CandlestickSeries, {
      upColor: "#34d399",
      downColor: "#f87171",
      borderVisible: false,
      wickUpColor: "#34d399",
      wickDownColor: "#f87171",
    });

    series.setData(buildCandles(symbol));
    chart.timeScale().fitContent();

    return () => chart.remove();
  }, [symbol]);

  return <div ref={containerRef} className="h-[260px] w-full" />;
}
