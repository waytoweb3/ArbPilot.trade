import { useSyncExternalStore } from "react";

import type { NewsItem } from "./types";

/** 与执行器 `news.rs` 中的来源标识保持一致。 */
export const NEWS_SOURCES = ["binance", "upbit", "okx", "bybit", "bitget"] as const;
export type NewsSource = (typeof NEWS_SOURCES)[number];

const STORAGE_KEY = "arbpilot.news.prefs";

interface NewsPrefs {
  /** 每个来源是否订阅。 */
  subscriptions: Record<string, boolean>;
  /** 每个来源最近一次已读时间（毫秒时间戳）。 */
  readAt: Record<string, number>;
}

const DEFAULT_PREFS: NewsPrefs = {
  subscriptions: Object.fromEntries(NEWS_SOURCES.map((source) => [source, true])),
  readAt: {},
};

/** 首次使用时把已读时间设为当前时刻，避免历史公告一次性堆成未读。 */
function freshReadAt(): Record<string, number> {
  const now = Date.now();
  return Object.fromEntries(NEWS_SOURCES.map((source) => [source, now]));
}

function load(): NewsPrefs {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      return { subscriptions: { ...DEFAULT_PREFS.subscriptions }, readAt: freshReadAt() };
    }
    const parsed = JSON.parse(raw) as Partial<NewsPrefs>;
    return {
      subscriptions: { ...DEFAULT_PREFS.subscriptions, ...(parsed.subscriptions ?? {}) },
      readAt: { ...(parsed.readAt ?? {}) },
    };
  } catch {
    return { subscriptions: { ...DEFAULT_PREFS.subscriptions }, readAt: freshReadAt() };
  }
}

let prefs = load();
const listeners = new Set<() => void>();

function commit(next: NewsPrefs) {
  prefs = next;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function snapshot(): NewsPrefs {
  return prefs;
}

/** 读取订阅与已读状态，跨菜单与页面的未读数保持一致。 */
export function useNewsPrefs(): NewsPrefs {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

export function isSubscribed(state: NewsPrefs, source: string): boolean {
  return state.subscriptions[source] ?? false;
}

export function toggleSubscription(source: string): void {
  commit({
    ...prefs,
    subscriptions: { ...prefs.subscriptions, [source]: !isSubscribed(prefs, source) },
  });
}

/** 把指定来源的已读时间推到当下，之前到达的消息不再计入未读。 */
export function markRead(sources: string[]): void {
  if (sources.length === 0) {
    return;
  }
  const now = Date.now();
  const readAt = { ...prefs.readAt };
  for (const source of sources) {
    readAt[source] = now;
  }
  commit({ ...prefs, readAt });
}

/** 已订阅来源中晚于已读时间的条目数。 */
export function countUnread(state: NewsPrefs, items: NewsItem[]): number {
  let total = 0;
  for (const item of items) {
    if (!isSubscribed(state, item.source)) {
      continue;
    }
    const readAt = state.readAt[item.source] ?? 0;
    if (Date.parse(item.published_at) > readAt) {
      total += 1;
    }
  }
  return total;
}
