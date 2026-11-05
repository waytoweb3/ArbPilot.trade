import { useEffect, useState } from "react";

const HISTORY_LENGTH = 48;

export type SpreadSnapshot = {
  binanceBid: number;
  binanceAsk: number;
  bybitBid: number;
  bybitAsk: number;
  grossBps: number;
  netBps: number;
  history: number[];
  updatedAt: string;
};

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function round(value: number, digits: number) {
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

function nowLabel() {
  const now = new Date();
  return `${now.toTimeString().slice(0, 8)}.${String(now.getMilliseconds()).padStart(3, "0")}`;
}

export function makeSnapshot(previous?: SpreadSnapshot): SpreadSnapshot {
  const base = clamp((previous?.binanceBid ?? 68_420) + (Math.random() - 0.5) * 24, 62_000, 74_000);
  const tick = 5 + Math.random() * 7;
  const binanceBid = round(base - tick, 1);
  const binanceAsk = round(base + tick, 1);

  // 另一家交易所整体偏移 8~62 bps，形成可执行价差
  const offsetBps = 8 + Math.random() * 54;
  const shift = (base * offsetBps) / 10_000;
  const bybitBid = round(base + shift - tick, 1);
  const bybitAsk = round(base + shift + tick, 1);

  const grossBps = round(((bybitBid - binanceAsk) / binanceAsk) * 10_000, 1);
  const netBps = round(grossBps - 8 - 2, 1);
  const history = [...(previous?.history ?? []), grossBps].slice(-HISTORY_LENGTH);

  return {
    binanceBid,
    binanceAsk,
    bybitBid,
    bybitAsk,
    grossBps,
    netBps,
    history,
    updatedAt: nowLabel(),
  };
}

/** 首页演示用的行情快照：纯前端模拟，不代表真实成交。 */
export function useSpreadDemo() {
  const [snapshot, setSnapshot] = useState<SpreadSnapshot>(() => makeSnapshot());

  useEffect(() => {
    const timer = window.setInterval(() => {
      setSnapshot((previous) => makeSnapshot(previous));
    }, 1600);
    return () => window.clearInterval(timer);
  }, []);

  return snapshot;
}

export function formatPrice(value: number) {
  return value.toLocaleString("en-US", {
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
  });
}

export function formatBps(value: number) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)}`;
}
