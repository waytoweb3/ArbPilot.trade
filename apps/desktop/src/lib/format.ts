import { toNumber } from "./api";
import { getCopy, getLocale } from "./i18n";
import type { Decimal, OrderStatus, StrategyStatus } from "./types";

function localeTag(): string {
  return getLocale() === "zh" ? "zh-CN" : "en-US";
}

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

/** 资金费率本身是比例，展示为百分比并带符号。 */
export function formatFundingRate(value: Decimal | null | undefined): string {
  if (value === null || value === undefined) {
    return "—";
  }
  const percent = toNumber(value) * 100;
  return `${percent > 0 ? "+" : ""}${percent.toFixed(4)}%`;
}

/** 资金费率按实际结算间隔折算年化，默认按 Binance 常见的 8 小时。 */
export function formatAnnualizedFunding(
  value: Decimal | null | undefined,
  intervalSecs = 8 * 3_600,
): string {
  if (value === null || value === undefined) {
    return "—";
  }
  const percent = toNumber(value) * ((365 * 24 * 3_600) / intervalSecs) * 100;
  return `${percent > 0 ? "+" : ""}${percent.toFixed(2)}%`;
}

/** 已经折算成百分数的数值（如 10.95 表示 10.95%）。 */
export function formatPercentValue(value: Decimal | null | undefined, digits = 2): string {
  if (value === null || value === undefined) {
    return "—";
  }
  const numeric = toNumber(value);
  return `${numeric > 0 ? "+" : ""}${numeric.toFixed(digits)}%`;
}

/** 结算间隔展示，如 28800 秒 → 8h。 */
export function formatInterval(seconds: number): string {
  // 交易所常返回相差 1 秒的间隔（如 14399），先就近取整到分钟再展示。
  const secs = Math.round(seconds / 60) * 60;
  const hours = secs / 3_600;
  return `${Number.isInteger(hours) ? hours : hours.toFixed(1)}h`;
}

export function formatTime(value: string | null | undefined): string {
  if (!value) {
    return "—";
  }
  return new Date(value).toLocaleTimeString(localeTag(), { hour12: false });
}

export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return "—";
  }
  return new Date(value).toLocaleString(localeTag(), { hour12: false });
}

export function formatUptime(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m ${seconds % 60}s`;
}

export function orderStatusLabel(status: OrderStatus): string {
  return getCopy().status.order[status] ?? status;
}

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

export function strategyStatusLabel(status: StrategyStatus): string {
  return getCopy().status.strategy[status] ?? status;
}

/** 距离下次结算的倒计时，形如 `02:31:05`。 */
export function formatCountdown(target: number | null, now: number): string {
  if (target === null) {
    return "—";
  }
  const remaining = target - now;
  if (!Number.isFinite(remaining) || remaining <= 0) {
    return "—";
  }
  const total = Math.floor(remaining / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  return [hours, minutes, seconds].map((value) => String(value).padStart(2, "0")).join(":");
}

export function opportunityKindLabel(kind: string): string {
  const labels: Record<string, string> = getCopy().status.kind;
  return labels[kind] ?? kind;
}
