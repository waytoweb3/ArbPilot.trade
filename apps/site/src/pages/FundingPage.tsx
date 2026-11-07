import { useEffect, useMemo, useState } from "react";

import { useI18n } from "../lib/i18n";
import { fetchFundingStatus, type FundingVenueStatus } from "../lib/funding";

const REFRESH_MS = 30_000;
/** 表格一次最多渲染的行数，避免整市场几千个合约拖慢首屏。 */
const MAX_ROWS = 100;

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

type Cell = { rate: number | null; annualized: number | null; next: number | null };

type Row = {
  symbol: string;
  cells: Record<string, Cell>;
  maxAnnualized: number | null;
  next: number | null;
};

export default function FundingPage() {
  const { t, locale } = useI18n();
  const text = t.fundingPage;

  const [venues, setVenues] = useState<FundingVenueStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<"annualized" | "symbol">("annualized");

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
    list.sort((left, right) => {
      if (sort === "symbol") return left.symbol.localeCompare(right.symbol);
      const leftValue = left.maxAnnualized ?? Number.NEGATIVE_INFINITY;
      const rightValue = right.maxAnnualized ?? Number.NEGATIVE_INFINITY;
      return rightValue - leftValue;
    });

    const polls = active.map((venue) => venue.poll_secs).filter((secs) => secs > 0);
    return {
      activeVenues: active,
      rows: list,
      totalPairs: list.length,
      pollSecs: polls.length > 0 ? Math.min(...polls) : 0,
    };
  }, [venues, query, sort]);

  const visibleRows = rows.slice(0, MAX_ROWS);

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

      <div className="mt-8 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={text.search}
          className="w-full rounded-xl border border-line bg-panel px-3.5 py-2.5 text-[12.5px] text-fg outline-none transition-colors placeholder:text-subtle focus:border-line-strong sm:max-w-xs"
        />
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => setSort("annualized")}
            className={toggleClass(sort === "annualized")}
          >
            {text.sortAnnualized}
          </button>
          <button
            type="button"
            onClick={() => setSort("symbol")}
            className={toggleClass(sort === "symbol")}
          >
            {text.sortSymbol}
          </button>
          <button type="button" onClick={() => setReloadKey((key) => key + 1)} className={toggleClass(false)}>
            {text.refresh}
          </button>
        </div>
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
                  <th className="px-4 py-3 text-left font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle">
                    {text.pair}
                  </th>
                  {activeVenues.map((venue) => (
                    <th
                      key={venue.venue}
                      className="px-3 py-3 text-right font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle"
                    >
                      {venue.venue}
                    </th>
                  ))}
                  <th className="px-3 py-3 text-right font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle">
                    {text.maxAnnualized}
                  </th>
                  <th className="px-4 py-3 text-right font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle">
                    {text.nextSettlement}
                  </th>
                </tr>
              </thead>
              <tbody>
                {visibleRows.map((row) => (
                  <tr key={row.symbol} className="border-b border-line/60 last:border-0">
                    <td className="px-4 py-2.5 font-mono text-[12px] text-fg">{row.symbol}</td>
                    {activeVenues.map((venue) => {
                      const cell = row.cells[venue.venue];
                      const rate = cell?.rate ?? null;
                      return (
                        <td
                          key={venue.venue}
                          title={cell?.annualized !== null && cell?.annualized !== undefined
                            ? formatAnnualized(cell.annualized)
                            : undefined}
                          className={`px-3 py-2.5 text-right font-mono text-[11.5px] ${
                            rate === null ? "text-subtle" : rate > 0 ? "text-accent" : rate < 0 ? "text-danger" : "text-body"
                          }`}
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
                    <td colSpan={activeVenues.length + 3} className="px-4 py-10 text-center text-[12.5px] text-muted">
                      {text.empty}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>
      )}

      <p className="mt-4 text-[11px] leading-[1.8] text-subtle">
        {totalPairs > MAX_ROWS ? `${text.limited} ` : ""}
        {text.source}
      </p>
    </section>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="bg-panel-soft px-4 py-3">
      <p className="text-[10px] uppercase tracking-[0.12em] text-subtle">{label}</p>
      <p className="mt-1 font-mono text-[13px] text-body">{value}</p>
    </div>
  );
}

function toggleClass(active: boolean): string {
  return `rounded-full border px-3.5 py-1.5 text-[12px] transition-colors ${
    active
      ? "border-line-strong bg-hover text-fg"
      : "border-line text-muted hover:border-line-strong hover:text-fg"
  }`;
}
