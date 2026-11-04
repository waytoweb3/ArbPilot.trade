import { toNumber } from "./api";
import type { Decimal, OrderStatus, StrategyStatus } from "./types";

export function formatUsd(value: Decimal | null | undefined, digits = 2): string {
  return `$${toNumber(value).toLocaleString("en-US", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}`;
}

export function formatNumber(value: Decimal | null | undefined, digits = 4): string {
  return toNumber(value).toLocaleString("en-US", {
    minimumFractionDigits: 0,
    maximumFractionDigits: digits,
  });
}

export function formatBps(value: Decimal | null | undefined): string {
  const numeric = toNumber(value);
  return `${numeric > 0 ? "+" : ""}${numeric.toFixed(1)} bps`;
}

export function formatPercent(value: Decimal | null | undefined, digits = 4): string {
  return `${(toNumber(value) * 100).toFixed(digits)}%`;
}

export function formatTime(value: string | null | undefined): string {
  if (!value) {
    return "—";
  }
  return new Date(value).toLocaleTimeString("zh-CN", { hour12: false });
}

export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return "—";
  }
  return new Date(value).toLocaleString("zh-CN", { hour12: false });
}

export function formatUptime(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m ${seconds % 60}s`;
}

export const orderStatusLabel: Record<OrderStatus, string> = {
  pending_risk: "风控检查中",
  rejected: "风控拒绝",
  submitting: "提交中",
  partially_filled: "部分成交",
  filled: "已成交",
  unwinding: "补偿中",
  cancelled: "已取消",
  failed: "失败",
};

export const orderStatusTone: Record<OrderStatus, "success" | "warning" | "danger" | "neutral" | "info"> = {
  pending_risk: "info",
  rejected: "danger",
  submitting: "info",
  partially_filled: "warning",
  filled: "success",
  unwinding: "warning",
  cancelled: "neutral",
  failed: "danger",
};

export const strategyStatusLabel: Record<StrategyStatus, string> = {
  draft: "草稿",
  running: "运行中",
  paused: "已暂停",
  stopped: "已停止",
};

export const opportunityKindLabel: Record<string, string> = {
  funding_rate: "资金费率",
  cex_cex_spread: "跨所价差",
  cex_dex_spread: "CEX-DEX",
};
