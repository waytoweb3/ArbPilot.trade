import { Link } from "react-router-dom";

import { useI18n } from "../lib/i18n";

export default function NotFoundPage() {
  const { t } = useI18n();

  return (
    <div className="page flex min-h-[70vh] flex-col justify-center pb-24 pt-28">
      <p className="eyebrow">{t.notFound.eyebrow}</p>
      <h1 className="mt-4 text-[clamp(2rem,4vw,3rem)] font-semibold leading-[1.1] tracking-[-0.02em] text-fg">
        {t.notFound.title}
      </h1>
      <p className="mt-5 max-w-xl text-[13.5px] leading-[1.9] text-body">{t.notFound.body}</p>
      <div className="mt-8">
        <Link
          to="/"
          className="inline-flex rounded-full bg-accent-solid px-5 py-2.5 text-[12.5px] font-medium text-on-accent transition-colors hover:bg-accent-solid-hover"
        >
          {t.notFound.back}
        </Link>
      </div>
    </div>
  );
}
