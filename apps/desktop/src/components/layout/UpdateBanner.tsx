import { ArrowUpCircle, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { useHealth } from "../../hooks/useApi";
import { isNewerVersion } from "../../lib/appUpdate";
import { fetchManifest } from "../../lib/deploy";
import { fmt, useI18n } from "../../lib/i18n";
import { Button } from "../ui/button";

const DISMISS_KEY = "arbpilot.update.dismissed";
const CHECK_INTERVAL_MS = 30 * 60 * 1000;

interface DismissedVersions {
  executor?: string;
}

function readDismissed(): DismissedVersions {
  try {
    const raw = localStorage.getItem(DISMISS_KEY);
    return raw ? (JSON.parse(raw) as DismissedVersions) : {};
  } catch {
    return {};
  }
}

/** 启动后自动检查执行器新版本，发现更新时在顶部提示升级入口。 */
export function UpdateBanner() {
  const { t } = useI18n();
  const navigate = useNavigate();
  const health = useHealth();
  const [executorLatest, setExecutorLatest] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<DismissedVersions>(readDismissed);

  useEffect(() => {
    let active = true;
    const check = () => {
      fetchManifest()
        .then((manifest) => {
          if (active) {
            setExecutorLatest(manifest.version);
          }
        })
        .catch(() => undefined);
    };
    check();
    const timer = window.setInterval(check, CHECK_INTERVAL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, []);

  const dismiss = (version: string) => {
    setDismissed((current) => {
      const next = { ...current, executor: version };
      localStorage.setItem(DISMISS_KEY, JSON.stringify(next));
      return next;
    });
  };

  const currentExecutor = health.data?.version ?? null;
  const executorOutdated = Boolean(
    executorLatest &&
    currentExecutor &&
    isNewerVersion(executorLatest, currentExecutor) &&
    dismissed.executor !== executorLatest,
  );

  if (!executorOutdated || !executorLatest || !currentExecutor) {
    return null;
  }

  return (
    <div className="space-y-2 px-6 pt-4">
      <div className="flex items-start gap-3 rounded-lg border border-warning/30 bg-warning/10 px-4 py-2.5">
        <ArrowUpCircle size={15} className="mt-0.5 shrink-0 text-warning" />
        <div className="min-w-0 flex-1">
          <p className="text-xs font-medium text-fg">
            {fmt(t.updates.executorTitle, { version: executorLatest })}
          </p>
          <p className="mt-0.5 text-[11px] leading-relaxed text-muted">
            {fmt(t.updates.executorBody, { current: currentExecutor })}
          </p>
        </div>
        <Button size="sm" onClick={() => navigate("/settings?tab=connection")}>
          {t.updates.action}
        </Button>
        <button
          type="button"
          onClick={() => dismiss(executorLatest)}
          title={t.updates.dismiss}
          aria-label={t.updates.dismiss}
          className="mt-0.5 shrink-0 rounded p-0.5 text-subtle transition-colors hover:bg-hover hover:text-body"
        >
          <X size={13} />
        </button>
      </div>
    </div>
  );
}
