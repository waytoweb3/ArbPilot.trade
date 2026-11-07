import type { ReactNode } from "react";

import { useI18n } from "../lib/i18n";
import type { PlanId, PlanInfo } from "../lib/payments";

const FALLBACK_PLANS: PlanInfo[] = [
  {
    id: "arbitrage",
    amount: "49.00",
    billingPeriod: "monthly",
    name: { zh: "Arbitrage & News", en: "Arbitrage & News" },
  },
  {
    id: "strategy",
    amount: "99.00",
    billingPeriod: "monthly",
    name: { zh: "Strategy", en: "Strategy" },
  },
];

function splitAmount(amount: string): { major: string; minor: string } {
  const [major, minor = "00"] = amount.split(".");
  return { major, minor };
}

function PlanCard({
  name,
  desc,
  features,
  amount,
  free,
  recommended,
  action,
}: {
  name: string;
  desc: string;
  features: string[];
  amount: string;
  free?: boolean;
  recommended?: boolean;
  action: ReactNode;
}) {
  const { t } = useI18n();
  const price = splitAmount(amount);

  return (
    <div
      className={`flex flex-col rounded-2xl border p-6 ${
        recommended ? "border-accent/35 bg-panel" : "border-line bg-panel"
      }`}
    >
      <div className="flex items-center justify-between gap-3">
        <p className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted">{name}</p>
        {recommended && (
          <span className="shrink-0 rounded-full border border-accent/30 bg-accent/10 px-2 py-0.5 text-[10px] text-accent">
            {t.pricing.popular}
          </span>
        )}
      </div>

      <p className="mt-6 flex items-baseline gap-1 text-fg">
        <span className="text-[15px] text-muted">$</span>
        <span className="font-mono text-[34px] leading-none tracking-tight">{price.major}</span>
        {!free && <span className="font-mono text-[15px] text-muted">.{price.minor}</span>}
        <span className="ml-2 text-[11px] text-subtle">
          {free ? t.pricing.freeForever : t.pricing.perMonth}
        </span>
      </p>

      <p className="mt-3 text-[11.5px] leading-relaxed text-subtle">{desc}</p>

      <ul className="mt-5 flex-1 space-y-2 text-[12px] text-body">
        {features.map((feature) => (
          <li key={feature} className="flex gap-2.5">
            <span className="mt-[6px] h-1 w-1 shrink-0 rounded-full bg-accent-solid" />
            {feature}
          </li>
        ))}
      </ul>

      <div className="mt-6">{action}</div>
    </div>
  );
}

const primaryCta =
  "inline-flex w-full items-center justify-center rounded-full bg-accent-solid px-5 py-2.5 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover";
const secondaryCta =
  "inline-flex w-full items-center justify-center rounded-full border border-line-strong px-5 py-2.5 text-[13px] font-semibold text-body transition-colors hover:border-accent/50 hover:text-fg";

/** 官网只展示价格，订阅与支付都在桌面端完成。 */
export default function Pricing() {
  const { t } = useI18n();

  const amountFor = (id: PlanId) =>
    FALLBACK_PLANS.find((plan) => plan.id === id)?.amount ?? "0.00";

  const copy: Record<PlanId, { name: string; desc: string; features: string[]; cta: string }> = {
    arbitrage: {
      name: t.pricing.arbitrageName,
      desc: t.pricing.arbitrageDesc,
      features: t.pricing.arbitrageFeatures,
      cta: t.pricing.arbitrageCta,
    },
    strategy: {
      name: t.pricing.strategyName,
      desc: t.pricing.strategyDesc,
      features: t.pricing.strategyFeatures,
      cta: t.pricing.strategyCta,
    },
  };

  return (
    <section id="pricing" className="scroll-mt-24 border-y border-line bg-panel-soft py-24">
      <div className="page">
        <div className="max-w-3xl">
          <p className="eyebrow">{t.pricing.eyebrow}</p>
          <h2 className="text-balance mt-4 text-[clamp(1.9rem,3.4vw,2.7rem)] font-semibold leading-[1.15] tracking-[-0.02em] text-fg">
            {t.pricing.title1}
            <br />
            <span className="accent-serif text-accent">{t.pricing.title2}</span>
          </h2>
          <p className="mt-5 max-w-2xl text-[14px] leading-[1.9] text-body">{t.pricing.desc}</p>
        </div>

        <div className="mt-12 grid gap-4 lg:grid-cols-3">
          <PlanCard
            name={t.pricing.freeName}
            desc={t.pricing.freeDesc}
            features={t.pricing.freeFeatures}
            amount="0"
            free
            action={
              <a href="#download" className={secondaryCta}>
                {t.pricing.freeCta}
              </a>
            }
          />

          {(["arbitrage", "strategy"] as const).map((id) => (
            <PlanCard
              key={id}
              name={copy[id].name}
              desc={copy[id].desc}
              features={copy[id].features}
              amount={amountFor(id)}
              recommended={id === "strategy"}
              action={
                <a href="#download" className={id === "strategy" ? primaryCta : secondaryCta}>
                  {copy[id].cta}
                </a>
              }
            />
          ))}
        </div>

        <div className="mt-6 grid gap-3 text-[10.5px] leading-relaxed text-subtle lg:grid-cols-3">
          <p>{t.pricing.note}</p>
          <p>{t.pricing.payment}</p>
          <p>{t.pricing.policies}</p>
        </div>
      </div>
    </section>
  );
}
