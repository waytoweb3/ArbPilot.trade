import { useI18n } from "../lib/i18n";
import LiveSpread from "./LiveSpread";

export default function Hero() {
  const { t } = useI18n();

  return (
    <section id="top" className="relative overflow-hidden pb-20 pt-32 sm:pt-36">
      <div aria-hidden className="grid-backdrop absolute inset-0 opacity-60" />
      <div
        aria-hidden
        className="glow pointer-events-none absolute -top-48 left-1/2 h-[520px] w-[900px] -translate-x-1/2 rounded-full blur-[140px]"
      />

      <div className="page relative grid items-center gap-14 lg:grid-cols-[1.05fr_0.95fr]">
        <div>
          <p className="eyebrow">{t.hero.eyebrow}</p>

          <h1 className="text-balance mt-6 text-[clamp(2.6rem,5.2vw,4.3rem)] font-semibold leading-[1.06] tracking-[-0.02em] text-fg">
            {t.hero.title1}
            <br />
            <span className="accent-serif text-accent">{t.hero.title2}</span>
          </h1>

          <p className="mt-7 max-w-xl text-[15px] leading-[1.85] text-body">{t.hero.body}</p>

          <div className="mt-8 flex flex-wrap items-center gap-3">
            <a
              href="#pricing"
              className="inline-flex items-center gap-2 rounded-full bg-accent-solid px-6 py-3 text-sm font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover"
            >
              {t.hero.cta1}
            </a>
            <a
              href="#funding"
              className="inline-flex items-center gap-2 rounded-full border border-line-strong px-6 py-3 text-sm text-body transition-colors hover:border-accent/50 hover:text-fg"
            >
              {t.hero.cta2}
            </a>
          </div>

          <ul className="mt-9 flex flex-wrap gap-x-7 gap-y-3 text-[13px] text-muted">
            {t.hero.guarantees.map((item) => (
              <li key={item} className="flex items-center gap-2">
                <span className="flex h-4 w-4 items-center justify-center rounded-full bg-accent/15 text-[10px] text-accent">
                  ✓
                </span>
                {item}
              </li>
            ))}
          </ul>
        </div>

        <LiveSpread />
      </div>
    </section>
  );
}
