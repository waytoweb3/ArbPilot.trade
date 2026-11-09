import { useEffect, useState } from "react";

import { api } from "../lib/api";
import type { PriceTick } from "../lib/types";

export interface PriceLeg {
  venue: string;
  symbol: string;
}

/**
 * 订阅双腿永续的实时中间价；断线自动重连（指数退避上限 10s）。
 * 返回以交易所为键的最新价格。
 */
export function usePriceStream(legs: PriceLeg[]): Record<string, PriceTick> {
  const [ticks, setTicks] = useState<Record<string, PriceTick>>({});
  const key = legs
    .filter((leg) => leg.venue && leg.symbol)
    .map((leg) => `${leg.venue}:${leg.symbol}`)
    .join("|");

  useEffect(() => {
    const entries = key
      ? key.split("|").map((item) => {
          const [venue, symbol] = item.split(":");
          return { venue, symbol };
        })
      : [];
    if (entries.length === 0) {
      setTicks({});
      return;
    }

    let disposed = false;
    const sockets: WebSocket[] = [];
    const timers: number[] = [];

    const connect = (venue: string, symbol: string, attempt: number) => {
      if (disposed) {
        return;
      }
      const socket = new WebSocket(api.pricesUrl(venue, symbol));
      sockets.push(socket);

      socket.onmessage = (event) => {
        try {
          const payload = JSON.parse(String(event.data)) as {
            type?: string;
            venue?: string;
            price?: string;
            ts?: number;
          };
          if (payload.type !== "price_tick" || !payload.venue || !payload.price || !payload.ts) {
            return;
          }
          const price = Number(payload.price);
          if (!Number.isFinite(price) || price <= 0) {
            return;
          }
          setTicks((previous) => ({
            ...previous,
            [payload.venue as string]: { price, ts: payload.ts as number },
          }));
        } catch {
          // 忽略无法解析的消息，保持连接。
        }
      };

      socket.onclose = () => {
        if (disposed) {
          return;
        }
        const next = Math.min(attempt + 1, 5);
        timers.push(
          window.setTimeout(
            () => connect(venue, symbol, next),
            Math.min(1_000 * 2 ** next, 10_000),
          ),
        );
      };
    };

    setTicks({});
    for (const { venue, symbol } of entries) {
      connect(venue, symbol, 0);
    }

    return () => {
      disposed = true;
      timers.forEach((timer) => window.clearTimeout(timer));
      sockets.forEach((socket) => socket.close());
    };
  }, [key]);

  return ticks;
}
