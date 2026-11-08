import { useEffect, useMemo, useRef, useState } from "react";

import { useI18n } from "../lib/i18n";
import { fetchFundingStatus, type FundingVenueStatus } from "../lib/funding";

const REFRESH_MS = 30_000;
/** 每页行数；与客户端一样用分页代替一次性渲染整市场。 */
const PAGE_SIZE = 50;
/** 费率变化高亮时长，与客户端 1.4s 保持一致。 */
const FLASH_MS = 1_400;

function toNumber(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** 当期费率是小数形式，展示为百分比并保留 4 位小数。 */
function formatRate(value: number): string {
  const percent = value * 100;
  return `${percent > 0 ? "+" : ""}${percent.toFixed(4)}%`;
}

function formatAnnualized(value: number): string {
  return `${value > 0 ? "+" : ""}${value.toFixed(2)}%`;
}

function formatCountdown(target: number, now: number): string {
  const minutes = Math.ceil((target - now) / 60_000);
  if (minutes <= 0) return "—";
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return hours > 0 ? `${hours}h ${String(rest).padStart(2, "0")}m` : `${rest}m`;
}

type Flash = "up" | "down";

type Cell = { rate: number | null; annualized: number | null; next: number | null };

type Row = {
  symbol: string;
  cells: Record<string, Cell>;
  maxAnnualized: number | null;
  next: number | null;
};

type SortKey = "symbol" | "max" | "next" | { venue: string };

type Sort = { key: SortKey; dir: "asc" | "desc" };

function sameKey(left: SortKey, right: SortKey): boolean {
  if (typeof left === "string" || typeof right === "string") return left === right;
  return left.venue === right.venue;
}

/** 表头点击：首次按该列降序（交易对按升序），再次点击切换方向。 */
function nextSort(current: Sort, key: SortKey): Sort {
  if (sameKey(current.key, key)) {
    return { key, dir: current.dir === "asc" ? "desc" : "asc" };
  }
  return { key, dir: key === "symbol" ? "asc" : "desc" };
}

function sortValue(row: Row, key: SortKey): number | string | null {
  if (key === "symbol") return row.symbol;
  if (key === "max") return row.maxAnnualized;
  if (key === "next") return row.next;
  return row.cells[key.venue]?.rate ?? null;
}

export default function FundingPage() {
  const { t, locale } = useI18n();
  const text = t.fundingPage;

  const [venues, setVenues] = useState<FundingVenueStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<Sort>({ key: "max", dir: "desc" });
  const [page, setPage] = useState(0);
  const [flashes, setFlashes] = useState<Map<string, Flash>>(() => new Map());
  const previousRates = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    const controller = new AbortController();
    let alive = true;

    const load = async () => {
      try {
        const data = await fetchFundingStatus(controller.signal);
        if (!alive) return;
        setVenues(data);
        setError(null);
        setUpdatedAt(Date.now());
      } catch (cause) {
        if (!alive || controller.signal.aborted) return;
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    };

    void load();
    const timer = window.setInterval(load, REFRESH_MS);
    return () => {
      alive = false;
      controller.abort();
      window.clearInterval(timer);
    };
  }, [reloadKey]);

  // 对比相邻两次刷新，费率上涨闪绿、下跌闪红，与客户端行为一致。
  useEffect(() => {
    if (!venues) return;

    const next = new Map<string, number>();
    const changed = new Map<string, Flash>();
    for (const venue of venues) {
      if (!venue.enabled) continue;
      for (const item of venue.symbols) {
        const rate = toNumber(item.predicted_rate) ?? toNumber(item.last_funding_rate);
        if (rate === null) continue;
        const key = `${venue.venue}:${item.symbol}`;
        next.set(key, rate);
        const before = previousRates.current.get(key);
        if (before !== undefined && before !== rate) {
          changed.set(key, rate > before ? "up" : "down");
        }
      }
    }
    previousRates.current = next;

    if (changed.size === 0) {
      setFlashes(new Map());
      return;
    }
    setFlashes(changed);
    const timer = window.setTimeout(() => setFlashes(new Map()), FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [venues]);

  const { activeVenues, rows, totalPairs, pollSecs } = useMemo(() => {
    const active = (venues ?? []).filter(
      (venue) => venue.enabled && venue.symbols.length > 0,
    );

    const merged = new Map<string, Row>();
    for (const venue of active) {
      for (const item of venue.symbols) {
        const rate = toNumber(item.predicted_rate) ?? toNumber(item.last_funding_rate);
        const annualized = toNumber(item.annualized_pct);
        const next = item.next_funding_at ? Date.parse(item.next_funding_at) : null;
        const row =
          merged.get(item.symbol) ??
          ({ symbol: item.symbol, cells: {}, maxAnnualized: null, next: null } satisfies Row);
        row.cells[venue.venue] = {
          rate,
          annualized,
          next: next !== null && Number.isFinite(next) ? next : null,
        };
        if (annualized !== null) {
          row.maxAnnualized =
            row.maxAnnualized === null ? annualized : Math.max(row.maxAnnualized, annualized);
        }
        if (row.cells[venue.venue].next !== null) {
          row.next =
            row.next === null
              ? row.cells[venue.venue].next
              : Math.min(row.next, row.cells[venue.venue].next!);
        }
        merged.set(item.symbol, row);
      }
    }

    const keyword = query.trim().toUpperCase();
    let list = [...merged.values()];
    if (keyword) {
      list = list.filter((row) => row.symbol.includes(keyword));
    }
    const factor = sort.dir === "asc" ? 1 : -1;
    list.sort((left, right) => {
      const leftValue = sortValue(left, sort.key);
      const rightValue = sortValue(right, sort.key);
      if (typeof leftValue === "string" || typeof rightValue === "string") {
        return String(leftValue).localeCompare(String(rightValue)) * factor;
      }
      // 空值始终排在最后，避免翻页时跳到末尾。
      if (leftValue === null && rightValue === null) return 0;
      if (leftValue === null) return 1;
      if (rightValue === null) return -1;
      return (leftValue - rightValue) * factor;
    });

    const polls = active.map((venue) => venue.poll_secs).filter((secs) => secs > 0);
    return {
      activeVenues: active,
      rows: list,
      totalPairs: list.length,
      pollSecs: polls.length > 0 ? Math.min(...polls) : 0,
    };
  }, [venues, query, sort]);

  const pages = Math.max(1, Math.ceil(rows.length / PAGE_SIZE));
  const currentPage = Math.min(page, pages - 1);

  useEffect(() => {
    setPage(0);
  }, [query, sort]);

  const visibleRows = rows.slice(currentPage * PAGE_SIZE, currentPage * PAGE_SIZE + PAGE_SIZE);

  const sortState = (key: SortKey) => (sameKey(sort.key, key) ? sort.dir : null);

  return (
    <section className="page pb-24 pt-32">
      <p className="eyebrow">{text.eyebrow}</p>
      <h1 className="mt-4 text-[clamp(1.9rem,3.6vw,2.8rem)] font-semibold leading-[1.15] tracking-[-0.02em] text-fg">
        {text.title}
      </h1>
      <p className="mt-5 max-w-3xl text-[14px] leading-[1.9] text-body">{text.desc}</p>

      <div className="mt-10 grid grid-cols-2 gap-px overflow-hidden rounded-xl bg-line sm:grid-cols-4">
        <Stat label={text.statVenues} value={String(activeVenues.length || "—")} />
        <Stat label={text.statPairs} value={totalPairs > 0 ? totalPairs.toLocaleString() : "—"} />
        <Stat label={text.statRefresh} value={pollSecs > 0 ? `${pollSecs}s` : "30s"} />
        <Stat
          label={text.updated}
          value={
            updatedAt
              ? new Date(updatedAt).toLocaleTimeString(locale === "zh" ? "zh-CN" : "en-US", {
                  hour12: false,
                })
              : "—"
          }
        />
      </div>

      <div className="mt-8">
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={text.search}
          className="w-full rounded-xl border border-line bg-panel px-3.5 py-2.5 text-[12.5px] text-fg outline-none transition-colors placeholder:text-subtle focus:border-line-strong sm:max-w-xs"
        />
      </div>

      {error && !venues ? (
        <div className="mt-6 rounded-2xl border border-line bg-panel p-6">
          <p className="text-[13px] text-danger">{text.failed}</p>
          <p className="mt-2 font-mono text-[11px] text-subtle">{error}</p>
          <button
            type="button"
            onClick={() => setReloadKey((key) => key + 1)}
            className="mt-4 rounded-full border border-line px-3.5 py-1.5 text-[12px] text-body transition-colors hover:border-line-strong hover:text-fg"
          >
            {text.retry}
          </button>
        </div>
      ) : !venues ? (
        <p className="mt-6 rounded-2xl border border-line bg-panel p-6 text-[13px] text-muted">
          {text.loading}
        </p>
      ) : activeVenues.length === 0 ? (
        <p className="mt-6 rounded-2xl border border-line bg-panel p-6 text-[13px] text-muted">
          {text.noData}
        </p>
      ) : (
        <div className="mt-6 overflow-hidden rounded-2xl border border-line bg-panel">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[820px] border-collapse text-[11.5px]">
              <thead>
                <tr className="border-b border-line">
                  <HeaderCell
                    label={text.pair}
                    align="left"
                    state={sortState("symbol")}
                    onClick={() => setSort((current) => nextSort(current, "symbol"))}
                  />
                  {activeVenues.map((venue) => (
                    <HeaderCell
                      key={venue.venue}
                      label={venue.venue}
                      state={sortState({ venue: venue.venue })}
                      onClick={() =>
                        setSort((current) => nextSort(current, { venue: venue.venue }))
                      }
                    />
                  ))}
                  <HeaderCell
                    label={text.maxAnnualized}
                    state={sortState("max")}
                    onClick={() => setSort((current) => nextSort(current, "max"))}
                  />
                  <HeaderCell
                    label={text.nextSettlement}
                    align="right"
                    state={sortState("next")}
                    onClick={() => setSort((current) => nextSort(current, "next"))}
                  />
                </tr>
              </thead>
              <tbody>
                {visibleRows.map((row) => (
                  <tr key={row.symbol} className="border-b border-line/60 last:border-0">
                    <td className="px-4 py-2.5 font-mono text-[12px] text-fg">{row.symbol}</td>
                    {activeVenues.map((venue) => {
                      const cell = row.cells[venue.venue];
                      const rate = cell?.rate ?? null;
                      const flash = flashes.get(`${venue.venue}:${row.symbol}`);
                      return (
                        <td
                          key={venue.venue}
                          title={
                            cell?.annualized !== null && cell?.annualized !== undefined
                              ? formatAnnualized(cell.annualized)
                              : undefined
                          }
                          className={`px-3 py-2.5 text-right font-mono text-[11.5px] ${
                            rate === null
                              ? "text-subtle"
                              : rate > 0
                                ? "text-accent"
                                : rate < 0
                                  ? "text-danger"
                                  : "text-body"
                          } ${flash === "up" ? "funding-flash-up" : flash === "down" ? "funding-flash-down" : ""}`}
                        >
                          {rate === null ? "—" : formatRate(rate)}
                        </td>
                      );
                    })}
                    <td className="px-3 py-2.5 text-right font-mono text-[11.5px] text-body">
                      {row.maxAnnualized === null ? "—" : formatAnnualized(row.maxAnnualized)}
                    </td>
                    <td className="px-4 py-2.5 text-right font-mono text-[11px] text-muted">
                      {row.next === null || updatedAt === null
                        ? "—"
                        : formatCountdown(row.next, updatedAt)}
                    </td>
                  </tr>
                ))}
                {visibleRows.length === 0 && (
                  <tr>
                    <td
                      colSpan={activeVenues.length + 3}
                      className="px-4 py-10 text-center text-[12.5px] text-muted"
                    >
                      {text.empty}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line px-4 py-3 text-[11.5px] text-muted">
            <span>
              {totalPairs.toLocaleString()} {text.pairsUnit} · {text.page} {currentPage + 1} /{" "}
              {pages}
            </span>
            <div className="flex items-center gap-2">
              <button
                type="button"
                disabled={currentPage === 0}
                onClick={() => setPage(currentPage - 1)}
                className={pagerClass}
              >
                ← {text.prev}
              </button>
              <button
                type="button"
                disabled={currentPage >= pages - 1}
                onClick={() => setPage(currentPage + 1)}
                className={pagerClass}
              >
                {text.next} →
              </button>
            </div>
          </div>
        </div>
      )}

      <p className="mt-4 text-[11px] leading-[1.8] text-subtle">
        {text.sortHint} · {text.source}
      </p>
    </section>
  );
}

function HeaderCell({
  label,
  align = "right",
  state,
  onClick,
}: {
  label: string;
  align?: "left" | "right";
  state: "asc" | "desc" | null;
  onClick: () => void;
}) {
  return (
    <th
      aria-sort={state === null ? "none" : state === "asc" ? "ascending" : "descending"}
      className={`px-3 py-3 ${align === "left" ? "text-left" : "text-right"} font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle first:pl-4 last:pr-4`}
    >
      <button
        type="button"
        onClick={onClick}
        className={`inline-flex items-center gap-1 uppercase transition-colors hover:text-fg ${
          state === null ? "" : "text-fg"
        }`}
      >
        {label}
        <span aria-hidden className={state === null ? "text-subtle/40" : "text-accent"}>
          {state === "asc" ? "↑" : state === "desc" ? "↓" : "↕"}
        </span>
      </button>
    </th>
  );
}

const pagerClass =
  "rounded-full border border-line px-3 py-1.5 text-[11.5px] text-body transition-colors hover:border-line-strong hover:text-fg disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line disabled:hover:text-body";

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="bg-panel-soft px-4 py-3">
      <p className="text-[10px] uppercase tracking-[0.12em] text-subtle">{label}</p>
      <p className="mt-1 font-mono text-[13px] text-body">{value}</p>
    </div>
  );
}
