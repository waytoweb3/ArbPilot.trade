import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { APP_VERSION, DISCORD_URL, DOWNLOAD_BASE } from "../lib/links";

function SectionHeading({
  eyebrow,
  title,
  description,
}: {
  eyebrow: string;
  title: ReactNode;
  description?: string;
}) {
  return (
    <div className="max-w-3xl">
      <p className="eyebrow">{eyebrow}</p>
      <h2 className="text-balance mt-4 text-[clamp(1.9rem,3.4vw,2.7rem)] font-semibold leading-[1.15] tracking-[-0.02em] text-fg">
        {title}
      </h2>
      {description && (
        <p className="mt-5 max-w-2xl text-[14px] leading-[1.9] text-body">{description}</p>
      )}
    </div>
  );
}

function BulletList({ items }: { items: string[] }) {
  return (
    <ul className="mt-8 space-y-3 text-[13px] text-body">
      {items.map((item) => (
        <li key={item} className="flex gap-3">
          <span className="mt-[7px] h-1 w-1 shrink-0 rounded-full bg-accent-solid" />
          {item}
        </li>
      ))}
    </ul>
  );
}

/** 交易介绍：把模块输出到实际下单的链路摊开，避免「一键交易」的误解。 */
function TradeFlow({ title, steps }: { title: string; steps: string[] }) {
  return (
    <div>
      <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-subtle">{title}</p>
      <ol className="mt-4 space-y-2.5">
        {steps.map((step, index) => (
          <li key={step} className="flex items-center gap-3">
            <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-line font-mono text-[9px] text-muted">
              {String(index + 1).padStart(2, "0")}
            </span>
            <span className="text-[12.5px] text-body">{step}</span>
            {index < steps.length - 1 && (
              <span aria-hidden className="text-[10px] text-subtle">
                ↓
              </span>
            )}
          </li>
        ))}
      </ol>
    </div>
  );
}

export function VenuesStrip() {
  const venues = [
    "BINANCE",
    "OKX",
    "BYBIT",
    "BITGET",
    "GATE.IO",
    "ASTER",
    "HYPERLIQUID",
    "LIGHTER",
  ];
  const { t } = useI18n();

  return (
    <section className="border-y border-line bg-panel-soft py-10">
      <div className="page">
        <p className="eyebrow">{t.venues.eyebrow}</p>
        <div className="mt-6 grid grid-cols-2 gap-y-5 sm:grid-cols-3 lg:grid-cols-6">
          {venues.map((venue, index) => (
            <div key={venue} className="flex items-baseline gap-2.5">
              <span className="font-mono text-[10px] text-subtle">
                {String(index + 1).padStart(2, "0")}
              </span>
              <span className="font-mono text-[12px] tracking-[0.14em] text-muted">{venue}</span>
            </div>
          ))}
        </div>
        <p className="mt-6 text-[11px] leading-relaxed text-subtle">{t.venues.note}</p>
      </div>
    </section>
  );
}

const fundingVenues = ["BINANCE", "OKX", "BYBIT", "BITGET", "GATE"];

/** 界面演示用的当期费率，仅为了展示表格结构与配色。 */
const fundingRows = [
  { symbol: "BTCUSDT", rates: ["+0.0100%", "+0.0091%", "+0.0125%", "+0.0078%", "+0.0112%"] },
  { symbol: "ETHUSDT", rates: ["+0.0087%", "+0.0104%", "+0.0069%", "+0.0093%", "+0.0075%"] },
  { symbol: "SOLUSDT", rates: ["-0.0042%", "-0.0061%", "-0.0038%", "-0.0055%", "-0.0070%"] },
  { symbol: "DOGEUSDT", rates: ["+0.0231%", "+0.0197%", "+0.0254%", "+0.0218%", "+0.0186%"] },
];

export function FundingSection() {
  const { t } = useI18n();

  const stats = [
    { label: t.funding.stat1, value: t.funding.stat1Value },
    { label: t.funding.stat2, value: t.funding.stat2Value },
    { label: t.funding.stat3, value: t.funding.stat3Value },
  ];

  return (
    <section id="funding" className="page scroll-mt-24 py-24">
      <div className="grid gap-12 lg:grid-cols-[0.85fr_1.15fr] lg:items-center">
        <div>
          <SectionHeading
            eyebrow={t.funding.eyebrow}
            title={
              <>
                {t.funding.title1}
                <br />
                <span className="accent-serif text-accent">{t.funding.title2}</span>
              </>
            }
            description={t.funding.desc}
          />
          <BulletList items={t.funding.bullets} />

          <div className="mt-8 grid grid-cols-3 gap-px overflow-hidden rounded-xl bg-line">
            {stats.map((stat) => (
              <div key={stat.label} className="bg-panel-soft px-3 py-3">
                <p className="text-[10px] uppercase tracking-[0.12em] text-subtle">{stat.label}</p>
                <p className="mt-1 font-mono text-[13px] text-body">{stat.value}</p>
              </div>
            ))}
          </div>
        </div>

        <div className="card p-5">
          <div className="flex items-center justify-between gap-3">
            <p className="font-mono text-[11px] tracking-[0.14em] text-body">{t.funding.panelTitle}</p>
            <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-muted">
              {t.funding.demo}
            </span>
          </div>

          <div className="mt-4 overflow-x-auto">
            <table className="w-full min-w-[420px] border-collapse text-[11.5px]">
              <thead>
                <tr className="border-b border-line">
                  <th className="py-2 pr-2 text-left font-mono text-[10px] font-normal uppercase tracking-[0.12em] text-subtle">
                    {t.funding.pairLabel}
                  </th>
                  {fundingVenues.map((venue) => (
                    <th
                      key={venue}
                      className="px-2 py-2 text-right font-mono text-[10px] font-normal tracking-[0.08em] text-subtle"
                    >
                      {venue}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {fundingRows.map((row) => (
                  <tr key={row.symbol} className="border-b border-line/60 last:border-0">
                    <td className="py-2.5 pr-2 font-mono text-[11.5px] text-body">{row.symbol}</td>
                    {row.rates.map((rate, index) => (
                      <td
                        key={fundingVenues[index]}
                        className={`px-2 py-2.5 text-right font-mono ${
                          rate.startsWith("-") ? "text-danger" : "text-accent"
                        }`}
                      >
                        {rate}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <p className="mt-4 text-[10px] leading-relaxed text-subtle">{t.funding.note}</p>
        </div>
      </div>
    </section>
  );
}

export function NewsSection() {
  const { t } = useI18n();

  return (
    <section id="news" className="scroll-mt-24 border-y border-line bg-panel-soft py-24">
      <div className="page">
        <div className="grid gap-10 lg:grid-cols-[1fr_auto] lg:items-end">
          <SectionHeading
            eyebrow={t.news.eyebrow}
            title={
              <>
                {t.news.title1}
                <br />
                <span className="accent-serif text-accent">{t.news.title2}</span>
              </>
            }
            description={t.news.desc}
          />
          <BulletList items={t.news.bullets} />
        </div>

        <div className="mt-12 grid gap-4 lg:grid-cols-[1.15fr_0.85fr]">
          <div className="card p-5">
            <div className="flex items-center justify-between gap-3">
              <p className="font-mono text-[11px] tracking-[0.14em] text-body">
                {t.news.panelTitle}
              </p>
              <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-muted">
                {t.news.demo}
              </span>
            </div>

            <ul className="mt-4 divide-y divide-line/60">
              {t.news.items.map((item) => (
                <li key={item.title} className="flex gap-4 py-3.5 first:pt-1 last:pb-1">
                  <span className="font-mono text-[10px] text-subtle">{item.time}</span>
                  <div className="min-w-0 flex-1">
                    <p className="text-[12.5px] leading-[1.7] text-body">{item.title}</p>
                    <div className="mt-2 flex flex-wrap items-center gap-2">
                      <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-muted">
                        {item.tag}
                      </span>
                      <span className="font-mono text-[10px] text-accent">→ {item.symbol}</span>
                    </div>
                  </div>
                </li>
              ))}
            </ul>
          </div>

          <div className="card p-5">
            <p className="text-[12.5px] leading-[1.8] text-body">{t.news.tradeDesc}</p>
            <div className="mt-5">
              <TradeFlow title={t.news.tradeTitle} steps={t.news.flow} />
            </div>
            <p className="mt-5 text-[10px] leading-relaxed text-subtle">{t.news.note}</p>
          </div>
        </div>
      </div>
    </section>
  );
}

export function StrategySection() {
  const { t } = useI18n();

  const metrics = [
    { label: t.strategy.metricAnnual, value: "+18.4%" },
    { label: t.strategy.metricDrawdown, value: "-4.2%" },
    { label: t.strategy.metricSharpe, value: "2.11" },
    { label: t.strategy.metricWinRate, value: "63%" },
  ];

  return (
    <section id="strategy" className="page scroll-mt-24 py-24">
      <div className="grid gap-12 lg:grid-cols-[0.85fr_1.15fr] lg:items-start">
        <div>
          <SectionHeading
            eyebrow={t.strategy.eyebrow}
            title={
              <>
                {t.strategy.title1}
                <br />
                <span className="accent-serif text-accent">{t.strategy.title2}</span>
              </>
            }
            description={t.strategy.desc}
          />
          <BulletList items={t.strategy.bullets} />
        </div>

        <div className="card p-5">
          <div className="flex items-center justify-between gap-3">
            <p className="font-mono text-[11px] tracking-[0.14em] text-body">
              {t.strategy.panelTitle}
            </p>
            <span className="rounded-full border border-line px-2 py-0.5 text-[10px] text-muted">
              {t.strategy.demo}
            </span>
          </div>

          <div className="mt-4 grid gap-px overflow-hidden rounded-xl bg-line sm:grid-cols-4">
            {metrics.map((metric) => (
              <div key={metric.label} className="bg-panel-soft px-3 py-3">
                <p className="text-[10px] uppercase tracking-[0.12em] text-subtle">{metric.label}</p>
                <p className="mt-1 font-mono text-[13px] text-body">{metric.value}</p>
              </div>
            ))}
          </div>

          <ul className="mt-4 space-y-2.5">
            {t.strategy.templates.map((template) => (
              <li
                key={template.name}
                className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-xl border border-line px-4 py-3"
              >
                <span className="text-[12.5px] font-semibold text-fg">{template.name}</span>
                <span className="text-[11.5px] leading-[1.7] text-muted">{template.body}</span>
              </li>
            ))}
          </ul>

          <div className="mt-5">
            <TradeFlow title={t.strategy.tradeTitle} steps={t.strategy.flow} />
          </div>
          <p className="mt-5 text-[10px] leading-relaxed text-subtle">{t.strategy.note}</p>
        </div>
      </div>
    </section>
  );
}

export function DownloadSection() {
  const { t } = useI18n();

  const builds = [
    {
      platform: t.download.macos,
      detail: t.download.macosArm,
      file: `${DOWNLOAD_BASE}/ArbPilot_${APP_VERSION}_aarch64.dmg`,
    },
    {
      platform: t.download.windows,
      detail: t.download.windowsDesc,
      file: `${DOWNLOAD_BASE}/ArbPilot_${APP_VERSION}_x64-setup.exe`,
    },
  ];

  return (
    <section id="download" className="scroll-mt-24 border-t border-line py-24">
      <div className="page">
        <SectionHeading
          eyebrow={t.download.eyebrow}
          title={
            <>
              {t.download.title1}
              <br />
              <span className="accent-serif text-accent">{t.download.title2}</span>
            </>
          }
          description={t.download.desc}
        />

        <div className="mt-12">
          <div className="rounded-2xl border border-line bg-panel p-6">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted">
                {t.download.version}
              </p>
              <span className="rounded-full border border-accent/30 bg-accent/10 px-2 py-0.5 font-mono text-[10px] text-accent">
                v{APP_VERSION}
              </span>
            </div>

            <div className="mt-5 grid gap-2 sm:grid-cols-2">
              {builds.map((build) => (
                <a
                  key={build.file}
                  href={build.file}
                  className="group flex items-center justify-between gap-3 rounded-xl border border-line bg-panel-soft px-4 py-3 transition-colors hover:border-accent/40"
                >
                  <span>
                    <span className="block text-[13px] font-medium text-fg">{build.platform}</span>
                    <span className="mt-0.5 block text-[11px] text-subtle">{build.detail}</span>
                  </span>
                  <span className="flex items-center gap-2 text-[11px] text-muted transition-colors group-hover:text-accent">
                    {t.download.download}
                    <span aria-hidden>↓</span>
                  </span>
                </a>
              ))}
            </div>

            <p className="mt-5 text-[10.5px] leading-relaxed text-subtle">{t.download.note}</p>
          </div>
        </div>
      </div>
    </section>
  );
}

export function Footer() {
  const { t } = useI18n();
  const anchors = ["/#top", "/#funding", "/#news", "/#strategy"];

  return (
    <footer className="border-t border-line py-14">
      <div className="page">
        <div className="grid gap-10 md:grid-cols-2 lg:grid-cols-[1.4fr_1fr_1fr_0.8fr]">
          <div>
            <div className="flex items-center gap-2.5">
              <span className="flex h-7 w-7 items-center justify-center rounded-md bg-accent/15 ring-1 ring-accent/30">
                <svg
                  viewBox="0 0 24 24"
                  className="h-3.5 w-3.5 text-accent"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M19.07 4.93A10 10 0 0 0 6.99 3.34" />
                  <path d="M2.29 9.62A10 10 0 1 0 21.31 8.35" />
                  <path d="M16.24 7.76A6 6 0 1 0 8.23 16.67" />
                  <path d="M17.99 11.66A6 6 0 0 1 15.77 16.67" />
                  <circle cx="12" cy="12" r="2" />
                  <path d="m13.41 10.59 5.66-5.66" />
                  <path d="M4 6h.01" />
                  <path d="M12 18h.01" />
                </svg>
              </span>
              <span className="text-sm font-semibold text-fg">ArbPilot</span>
            </div>
            <p className="mt-4 max-w-xs text-[12px] leading-[1.8] text-muted">{t.footer.body}</p>
          </div>

          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-subtle">
              {t.footer.product}
            </p>
            <ul className="mt-4 space-y-2.5 text-[12.5px] text-muted">
              {t.footer.links.map((label, index) => (
                <li key={label}>
                  <Link to={anchors[index]} className="transition-colors hover:text-fg">
                    {label}
                  </Link>
                </li>
              ))}
            </ul>
          </div>

          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-subtle">
              {t.footer.resources}
            </p>
            <ul className="mt-4 space-y-2.5 text-[12.5px] text-muted">
              <li>
                <a
                  href={DISCORD_URL}
                  target="_blank"
                  rel="noreferrer"
                  className="transition-colors hover:text-fg"
                >
                  {t.footer.discord}
                </a>
              </li>
            </ul>
          </div>

          <div>
            <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-subtle">
              {t.footer.legal}
            </p>
            <ul className="mt-4 space-y-2.5 text-[12.5px] text-muted">
              <li>
                <Link to="/privacy" className="transition-colors hover:text-fg">
                  {t.footer.privacy}
                </Link>
              </li>
              <li>
                <Link to="/terms" className="transition-colors hover:text-fg">
                  {t.footer.terms}
                </Link>
              </li>
            </ul>
          </div>
        </div>

        <div className="mt-12 border-t border-line pt-6">
          <p className="text-[11px] leading-[1.8] text-subtle">{t.footer.disclaimer}</p>
          <p className="mt-4 font-mono text-[10px] text-subtle">{t.footer.copyright}</p>
        </div>
      </div>
    </footer>
  );
}
