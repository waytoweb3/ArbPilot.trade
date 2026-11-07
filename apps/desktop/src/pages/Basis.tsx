import { ArrowDown, ArrowUp, ChevronLeft, ChevronRight, Search, WifiOff } from "lucide-react";
import { useMemo, useState } from "react";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useBasisOpportunities } from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatBps,
  formatDateTime,
  formatPercentValue,
  formatUsd,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { Opportunity } from "../lib/types";
import { cn } from "../lib/utils";

type SortKey = "annualized" | "basis" | "expiry" | "net" | "symbol";

const PAGE_SIZES = [10, 20, 50];

const QUOTES = ["USDT", "USDC", "USD"];

/** `BTCUSDT` → base `BTC`、quote `USDT`。 */
function splitSymbol(symbol: string): { base: string; quote: string } {
  for (const quote of QUOTES) {
    if (symbol.endsWith(quote) && symbol.length > quote.length) {
      return { base: symbol.slice(0, -quote.length), quote };
    }
  }
  return { base: symbol, quote: "" };
}

function annualizedValue(opportunity: Opportunity): number | null {
  if (opportunity.annualized_basis_pct === null || opportunity.annualized_basis_pct === undefined) {
    return null;
  }
  return toNumber(opportunity.annualized_basis_pct);
}

function expiryTime(opportunity: Opportunity): number | null {
  if (!opportunity.expiry) {
    return null;
  }
  const parsed = Date.parse(opportunity.expiry);
  return Number.isFinite(parsed) ? parsed : null;
}

function daysLeft(opportunity: Opportunity, now: number): number | null {
  const expiry = expiryTime(opportunity);
  if (expiry === null) {
    return null;
  }
  return Math.max(0, (expiry - now) / 86_400_000);
}

export default function BasisPage() {
  const query = useBasisOpportunities();
  const { t } = useI18n();

  const [keyword, setKeyword] = useState("");
  const [sort, setSort] = useState<{ key: SortKey; dir: "asc" | "desc" }>({
    key: "annualized",
    dir: "desc",
  });
  const [pageSize, setPageSize] = useState(PAGE_SIZES[0]);
  const [page, setPage] = useState(0);

  const rows = useMemo(() => query.data ?? [], [query.data]);
  const now = Date.now();

  const search = keyword.trim().toUpperCase();
  const visible = useMemo(
    () =>
      rows
        .filter((row) => {
          if (!search) {
            return true;
          }
          return (
            row.symbol.includes(search) ||
            (row.delivery_contract ?? "").toUpperCase().includes(search) ||
            row.short_venue.toUpperCase().includes(search)
          );
        })
        .sort((left, right) => {
          const direction = sort.dir === "asc" ? 1 : -1;
          if (sort.key === "symbol") {
            return left.symbol.localeCompare(right.symbol) * direction;
          }
          const pick = (row: Opportunity): number | null => {
            switch (sort.key) {
              case "basis":
                return toNumber(row.gross_spread_bps);
              case "net":
                return toNumber(row.net_edge_bps);
              case "expiry":
                return expiryTime(row);
              default:
                return annualizedValue(row);
            }
          };
          const leftValue = pick(left);
          const rightValue = pick(right);
          if (leftValue === null && rightValue === null) {
            return 0;
          }
          if (leftValue === null) {
            return 1;
          }
          if (rightValue === null) {
            return -1;
          }
          return (leftValue - rightValue) * direction;
        }),
    [rows, search, sort],
  );

  const { highest, nearest } = useMemo(() => {
    const byAnnualized = [...rows]
      .filter((row) => annualizedValue(row) !== null)
      .sort((left, right) => (annualizedValue(right) ?? 0) - (annualizedValue(left) ?? 0));
    const byExpiry = [...rows]
      .filter((row) => expiryTime(row) !== null)
      .sort((left, right) => (expiryTime(left) ?? 0) - (expiryTime(right) ?? 0));
    return {
      highest: byAnnualized.slice(0, 5),
      nearest: byExpiry.slice(0, 5),
    };
  }, [rows]);

  const pageCount = Math.max(1, Math.ceil(visible.length / pageSize));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRows = visible.slice(currentPage * pageSize, (currentPage + 1) * pageSize);

  const toggleSort = (key: SortKey) => {
    setPage(0);
    setSort((current) =>
      current.key === key
        ? { key, dir: current.dir === "asc" ? "desc" : "asc" }
        : { key, dir: key === "symbol" ? "asc" : "desc" },
    );
  };

  const sortIcon = (key: SortKey) => {
    if (sort.key !== key) {
      return null;
    }
    return sort.dir === "asc" ? <ArrowUp size={11} /> : <ArrowDown size={11} />;
  };

  const header = (key: SortKey, label: string) => (
    <button
      type="button"
      onClick={() => toggleSort(key)}
      className={cn(
        "inline-flex items-center gap-1 uppercase",
        sort.key === key ? "text-fg" : "hover:text-body",
      )}
    >
      {label}
      {sortIcon(key)}
    </button>
  );

  return (
    <div className="space-y-4">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.basis.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.basis.subtitle}</p>
      </header>

      {query.isError && (
        <div className="flex items-center gap-2 rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          <WifiOff size={14} />
          {t.basis.unavailable}
        </div>
      )}

      {rows.length > 0 && (
        <section className="grid gap-3 md:grid-cols-2">
          <RankCard title={t.basis.highest} entries={highest} now={now} primary="annualized" />
          <RankCard title={t.basis.nearest} entries={nearest} now={now} primary="expiry" />
        </section>
      )}

      <Card>
        <CardHeader className="flex-wrap gap-3">
          <div>
            <CardTitle>{t.basis.tableTitle}</CardTitle>
            <CardDescription>{t.basis.tableDesc}</CardDescription>
          </div>
          <label className="relative">
            <Search
              size={13}
              className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-subtle"
            />
            <input
              value={keyword}
              onChange={(event) => {
                setKeyword(event.target.value);
                setPage(0);
              }}
              placeholder={t.basis.searchPlaceholder}
              className="w-52 rounded-md border border-line bg-panel-soft py-1 pr-2 pl-7 text-xs text-body outline-none placeholder:text-subtle focus:border-accent/60"
            />
          </label>
        </CardHeader>

        {rows.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-muted">{t.basis.empty}</div>
        ) : visible.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-muted">{t.basis.noMatch}</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[820px] border-collapse text-sm">
              <thead>
                <tr className="border-b border-line text-[11px] tracking-wide text-muted uppercase">
                  <th className="w-10 px-3 py-2 text-left font-medium">#</th>
                  <th className="px-3 py-2 text-left font-medium">
                    {header("symbol", t.basis.colSymbol)}
                  </th>
                  <th className="px-3 py-2 text-left font-medium">{t.basis.colContract}</th>
                  <th className="px-3 py-2 text-right font-medium">
                    {header("expiry", t.basis.colExpiry)}
                  </th>
                  <th className="px-3 py-2 text-right font-medium">
                    {header("basis", t.basis.colBasis)}
                  </th>
                  <th className="px-3 py-2 text-right font-medium">
                    {header("annualized", t.basis.colAnnualized)}
                  </th>
                  <th className="px-3 py-2 text-right font-medium">
                    {header("net", t.basis.colNet)}
                  </th>
                  <th className="px-3 py-2 text-right font-medium">{t.basis.colNotional}</th>
                  <th className="px-3 py-2 text-right font-medium">{t.basis.colObserved}</th>
                </tr>
              </thead>
              <tbody>
                {pageRows.map((row, index) => {
                  const { base, quote } = splitSymbol(row.symbol);
                  const annualized = annualizedValue(row);
                  const days = daysLeft(row, now);
                  return (
                    <tr
                      key={row.id}
                      className="border-b border-line/70 transition-colors hover:bg-hover"
                    >
                      <td className="px-3 py-2.5 font-mono text-[11px] text-subtle">
                        {currentPage * pageSize + index + 1}
                      </td>
                      <td className="px-3 py-2.5">
                        <div className="flex items-start gap-2.5">
                          <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-line bg-panel-soft text-[10px] font-semibold text-body">
                            {base.slice(0, 2)}
                          </span>
                          <div className="min-w-0">
                            <div className="flex items-baseline gap-1 whitespace-nowrap">
                              <span className="text-sm font-semibold text-fg">{base}</span>
                              <span className="text-[11px] text-subtle">/{quote}</span>
                            </div>
                            <div className="mt-0.5 font-mono text-[10px] text-subtle">
                              {row.long_venue} → {row.short_venue}
                            </div>
                          </div>
                        </div>
                      </td>
                      <td className="px-3 py-2.5 font-mono text-xs text-body">
                        {row.delivery_contract ?? "—"}
                      </td>
                      <td className="px-3 py-2.5 text-right">
                        <div className="font-mono text-xs text-body">
                          {formatDateTime(row.expiry ?? undefined)}
                        </div>
                        {days !== null && (
                          <div className="mt-0.5 font-mono text-[10px] text-subtle">
                            {fmt(t.basis.daysLeft, { days: days.toFixed(1) })}
                          </div>
                        )}
                      </td>
                      <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                        {formatBps(row.gross_spread_bps)}
                      </td>
                      <td
                        className={cn(
                          "px-3 py-2.5 text-right font-mono text-[13px] font-medium",
                          (annualized ?? 0) >= 0 ? "text-accent" : "text-danger",
                        )}
                      >
                        {formatPercentValue(annualized)}
                      </td>
                      <td className="px-3 py-2.5 text-right font-mono text-xs text-muted">
                        {formatBps(row.net_edge_bps)}
                      </td>
                      <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                        {formatUsd(row.max_notional_usd, 0)}
                      </td>
                      <td className="px-3 py-2.5 text-right font-mono text-[10px] text-subtle">
                        {formatDateTime(row.observed_at)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-4 py-2 text-[11px] text-muted">
          <div className="flex items-center gap-2">
            <span>{fmt(t.basis.rowsCount, { count: visible.length })}</span>
            <select
              value={pageSize}
              onChange={(event) => {
                setPageSize(Number(event.target.value));
                setPage(0);
              }}
              className="rounded-md border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-body outline-none focus:border-accent/60"
            >
              {PAGE_SIZES.map((value) => (
                <option key={value} value={value}>
                  {fmt(t.basis.perPage, { count: value })}
                </option>
              ))}
            </select>
          </div>
          <div className="flex items-center gap-1">
            <button
              type="button"
              aria-label={t.basis.prevPage}
              disabled={currentPage === 0}
              onClick={() => setPage(currentPage - 1)}
              className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
            >
              <ChevronLeft size={13} />
            </button>
            <span className="px-1 font-mono text-[11px]">
              {fmt(t.basis.pageInfo, { page: currentPage + 1, pages: pageCount })}
            </span>
            <button
              type="button"
              aria-label={t.basis.nextPage}
              disabled={currentPage >= pageCount - 1}
              onClick={() => setPage(currentPage + 1)}
              className="rounded-md border border-line p-1 transition-colors enabled:hover:bg-hover disabled:opacity-40"
            >
              <ChevronRight size={13} />
            </button>
          </div>
        </div>
      </Card>
    </div>
  );
}

function RankCard({
  title,
  entries,
  now,
  primary,
}: {
  title: string;
  entries: Opportunity[];
  now: number;
  primary: "annualized" | "expiry";
}) {
  const { t } = useI18n();
  return (
    <Card>
      <CardHeader className="py-2.5">
        <CardTitle className="text-xs">{title}</CardTitle>
      </CardHeader>
      <CardContent className="divide-y divide-line p-0">
        {entries.map((entry) => {
          const base = splitSymbol(entry.symbol).base;
          const days = daysLeft(entry, now);
          return (
            <div
              key={entry.id}
              className="flex w-full items-center justify-between gap-3 px-4 py-2 text-left"
            >
              <span className="flex min-w-0 items-baseline gap-2">
                <span className="text-sm font-medium text-fg">{base}</span>
                <span className="truncate text-[10px] text-subtle uppercase">
                  {entry.short_venue}
                </span>
              </span>
              <span className="text-right">
                {primary === "annualized" ? (
                  <span className="font-mono text-xs font-medium text-accent">
                    {fmt(t.basis.rankAnnualized, {
                      value: formatPercentValue(entry.annualized_basis_pct),
                    })}
                  </span>
                ) : (
                  <span className="font-mono text-xs text-body">
                    {days === null ? "—" : fmt(t.basis.daysLeft, { days: days.toFixed(1) })}
                  </span>
                )}
              </span>
            </div>
          );
        })}
      </CardContent>
    </Card>
  );
}
