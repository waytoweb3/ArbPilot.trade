import { useEffect, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { fetchLicense, type LicenseInfo, type LicenseStatus } from "../lib/payments";

const POLL_INTERVAL_MS = 2_500;
const MAX_ATTEMPTS = 72;

function isSettled(status: LicenseStatus): boolean {
  return status !== "pending" && status !== "unknown";
}

export default function CheckoutSuccessPage() {
  const { t } = useI18n();
  const [params] = useSearchParams();
  const ref = params.get("ref") ?? "";
  const [license, setLicense] = useState<LicenseInfo | null>(null);
  const [timedOut, setTimedOut] = useState(false);

  useEffect(() => {
    if (!ref) {
      return;
    }
    let alive = true;
    let attempts = 0;
    let timer = 0;

    const poll = async () => {
      try {
        const next = await fetchLicense(ref);
        if (!alive) {
          return;
        }
        setLicense(next);
        if (isSettled(next.status)) {
          return;
        }
      } catch {
        // 网络抖动时继续轮询，避免误报失败。
      }
      attempts += 1;
      if (attempts >= MAX_ATTEMPTS) {
        if (alive) {
          setTimedOut(true);
        }
        return;
      }
      timer = window.setTimeout(() => void poll(), POLL_INTERVAL_MS);
    };

    void poll();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [ref]);

  if (!ref) {
    return (
      <Shell title={t.checkout.missing} description={t.checkout.missingDesc}>
        <Actions />
      </Shell>
    );
  }

  const status = license?.status ?? "pending";

  if (status === "missing") {
    return (
      <Shell title={t.checkout.missing} description={t.checkout.missingDesc}>
        <Actions />
      </Shell>
    );
  }

  if (status === "active") {
    return (
      <Shell title={t.checkout.active} description={t.checkout.activeDescGeneric}>
        <Details license={license} />
        <Actions />
      </Shell>
    );
  }

  if (status === "canceling" || status === "canceled") {
    return (
      <Shell
        title={t.checkout.canceled}
        description={status === "canceling" ? t.checkout.canceling : t.checkout.canceledDesc}
      >
        <Details license={license} />
        <Actions />
      </Shell>
    );
  }

  if (status === "past_due") {
    return (
      <Shell title={t.checkout.pastDue} description={t.checkout.slow}>
        <Details license={license} />
        <Actions />
      </Shell>
    );
  }

  if (status === "expired") {
    return (
      <Shell title={t.checkout.expired} description={t.checkout.missingDesc}>
        <Details license={license} />
        <Actions />
      </Shell>
    );
  }

  return (
    <Shell title={t.checkout.title} description={timedOut ? t.checkout.slow : t.checkout.pending}>
      <div className="mt-8 flex items-center gap-3 font-mono text-[11px] uppercase tracking-[0.16em] text-muted">
        <span className="relative flex h-2 w-2">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-accent/60" />
          <span className="relative inline-flex h-2 w-2 rounded-full bg-accent-solid" />
        </span>
        {t.checkout.status} · {status}
      </div>
      <Actions />
    </Shell>
  );
}

function Shell({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  const { t } = useI18n();
  return (
    <div className="page pb-24 pt-32">
      <div className="mx-auto max-w-xl">
        <p className="eyebrow">{t.checkout.eyebrow}</p>
        <h1 className="mt-4 text-[clamp(1.7rem,3.4vw,2.4rem)] font-semibold leading-[1.2] tracking-[-0.02em] text-fg">
          {title}
        </h1>
        <p className="mt-4 text-[13.5px] leading-[1.9] text-body">{description}</p>
        {children}
      </div>
    </div>
  );
}

function Details({ license }: { license: LicenseInfo | null }) {
  const { t, locale } = useI18n();
  if (!license) {
    return null;
  }
  const planLabel =
    license.plan === "arbitrage"
      ? "自托管部署"
      : license.plan === "strategy"
        ? "策略创建附加包"
        : "—";

  return (
    <dl className="mt-8 grid grid-cols-2 gap-x-6 gap-y-4 rounded-2xl border border-line bg-panel p-5 text-[12px]">
      <div>
        <dt className="text-[10.5px] uppercase tracking-[0.12em] text-subtle">{t.checkout.plan}</dt>
        <dd className="mt-1 text-body">{planLabel}</dd>
      </div>
      <div>
        <dt className="text-[10.5px] uppercase tracking-[0.12em] text-subtle">
          {t.checkout.status}
        </dt>
        <dd className="mt-1 font-mono text-body">{license.status}</dd>
      </div>
      {license.expiresAt && (
        <div className="col-span-2">
          <dt className="text-[10.5px] uppercase tracking-[0.12em] text-subtle">
            {t.checkout.expires}
          </dt>
          <dd className="mt-1 font-mono text-body">
            {new Date(license.expiresAt).toLocaleString(locale === "zh" ? "zh-CN" : "en-US")}
          </dd>
        </div>
      )}
      <div className="col-span-2">
        <dt className="text-[10.5px] uppercase tracking-[0.12em] text-subtle">Ref</dt>
        <dd className="mt-1 break-all font-mono text-[11px] text-muted">{license.ref}</dd>
      </div>
    </dl>
  );
}

function Actions() {
  const { t } = useI18n();
  return (
    <div className="mt-8 flex flex-wrap gap-3">
      <Link
        to="/"
        className="rounded-full bg-accent-solid px-5 py-2.5 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-solid-hover"
      >
        {t.checkout.back}
      </Link>
      <Link
        to="/#pricing"
        className="rounded-full border border-line-strong px-5 py-2.5 text-[13px] text-body transition-colors hover:border-accent/50 hover:text-fg"
      >
        {t.checkout.pricing}
      </Link>
    </div>
  );
}
