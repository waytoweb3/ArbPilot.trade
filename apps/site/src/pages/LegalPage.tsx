import { Link } from "react-router-dom";

import { legalDocs, type LegalDoc } from "../lib/legal";
import { useI18n } from "../lib/i18n";

export default function LegalPage({ slug }: { slug: LegalDoc["slug"] }) {
  const { t, locale } = useI18n();
  const doc = legalDocs[locale][slug];

  return (
    <div className="page pb-24 pt-28">
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-line pb-8">
        <div>
          <p className="eyebrow">{t.legal[slug]}</p>
          <h1 className="mt-3 text-[clamp(1.9rem,3.6vw,2.8rem)] font-semibold leading-[1.15] tracking-[-0.02em] text-fg">
            {doc.title}
          </h1>
        </div>
        <Link
          to="/"
          className="rounded-full border border-line px-4 py-2 text-[12px] text-muted transition-colors hover:border-line-strong hover:text-fg"
        >
          ← {t.legal.back}
        </Link>
      </div>

      <div className="mt-8 flex flex-wrap gap-x-6 gap-y-1 font-mono text-[10.5px] uppercase tracking-[0.14em] text-subtle">
        <span>
          {t.legal.updated} · {doc.updated}
        </span>
        <span>
          {t.legal.version} · {doc.version}
        </span>
      </div>

      <div className="mt-12 grid gap-12 lg:grid-cols-[240px_1fr]">
        <aside className="hidden lg:block">
          <div className="sticky top-24">
            <p className="font-mono text-[10px] uppercase tracking-[0.18em] text-subtle">
              {t.legal.toc}
            </p>
            <ul className="mt-4 space-y-2 border-l border-line pl-4 text-[12px] text-body">
              {doc.sections.map((section) => (
                <li key={section.heading}>
                  <a
                    href={`#${sectionAnchor(section.heading)}`}
                    className="block leading-relaxed transition-colors hover:text-fg"
                  >
                    {section.heading}
                  </a>
                </li>
              ))}
            </ul>
            <div className="mt-8 border-t border-line pt-4 text-[11px] text-subtle">
              <p className="font-mono text-[10px] uppercase tracking-[0.18em]">{t.legal.related}</p>
              <Link
                to={slug === "privacy" ? "/terms" : "/privacy"}
                className="mt-2 block text-muted transition-colors hover:text-fg"
              >
                {slug === "privacy" ? t.legal.terms : t.legal.privacy} →
              </Link>
            </div>
          </div>
        </aside>

        <article className="max-w-3xl">
          <div className="space-y-4 text-[13.5px] leading-[1.95] text-body">
            {doc.intro.map((paragraph) => (
              <p key={paragraph}>{paragraph}</p>
            ))}
          </div>

          <div className="mt-12 space-y-10">
            {doc.sections.map((section) => (
              <section
                key={section.heading}
                id={sectionAnchor(section.heading)}
                className="scroll-mt-24"
              >
                <h2 className="text-[16px] font-semibold tracking-[-0.01em] text-fg">
                  {section.heading}
                </h2>

                {section.paragraphs && (
                  <div className="mt-3 space-y-3 text-[13px] leading-[1.95] text-body">
                    {section.paragraphs.map((paragraph) => (
                      <p key={paragraph}>{paragraph}</p>
                    ))}
                  </div>
                )}

                {section.bullets && (
                  <ul className="mt-3 space-y-2 text-[13px] leading-[1.9] text-body">
                    {section.bullets.map((item) => (
                      <li key={item} className="flex gap-2.5">
                        <span className="mt-[9px] h-1 w-1 shrink-0 rounded-full bg-accent-solid" />
                        <span>{item}</span>
                      </li>
                    ))}
                  </ul>
                )}

                {section.table && (
                  <div className="mt-4 overflow-x-auto rounded-xl border border-line">
                    <table className="w-full border-collapse text-left text-[12px]">
                      <thead>
                        <tr className="bg-panel-soft">
                          {section.table.columns.map((column) => (
                            <th
                              key={column}
                              className="border-b border-line px-4 py-2.5 font-mono text-[10px] uppercase tracking-[0.12em] text-muted"
                            >
                              {column}
                            </th>
                          ))}
                        </tr>
                      </thead>
                      <tbody>
                        {section.table.rows.map((row) => (
                          <tr key={row.join("|")} className="border-b border-line last:border-0">
                            {row.map((cell, index) => (
                              <td
                                key={`${row[0]}-${index}`}
                                className={`px-4 py-2.5 align-top leading-relaxed ${
                                  index === 0 ? "text-body" : "text-muted"
                                }`}
                              >
                                {cell}
                              </td>
                            ))}
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </section>
            ))}
          </div>

          <p className="mt-12 border-t border-line pt-6 text-[11px] leading-[1.9] text-subtle">
            {doc.footnote}
          </p>
        </article>
      </div>
    </div>
  );
}

function sectionAnchor(heading: string): string {
  const slug = heading
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 60);
  return `section-${slug}`;
}
