import { ArrowUpCircle, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { useHealth } from "../../hooks/useApi";
import { checkAppUpdate, isNewerVersion } from "../../lib/appUpdate";
import { fetchManifest, isDesktopRuntime } from "../../lib/deploy";
import { fmt, useI18n } from "../../lib/i18n";
import { Button } from "../ui/button";

const DISMISS_KEY = "arbpilot.update.dismissed";
const CHECK_INTERVAL_MS = 30 * 60 * 1000;

interface DismissedVersions {
  client?: string;
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

/** 启动后自动检查客户端与执行器的新版本，发现更新时在顶部提示升级入口。 */
export function UpdateBanner() {
  const { t } = useI18n();
  const navigate = useNavigate();
  const desktop = isDesktopRuntime();
  const health = useHealth();
  const [clientLatest, setClientLatest] = useState<string | null>(null);
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

  useEffect(() => {
    if (!desktop) {
      return;
    }
    let active = true;
    const check = () => {
      checkAppUpdate()
        .then((pending) => {
          if (active) {
            setClientLatest(pending?.info.version ?? null);
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
  }, [desktop]);

  const dismiss = (kind: keyof DismissedVersions, version: string) => {
    setDismissed((current) => {
      const next = { ...current, [kind]: version };
      localStorage.setItem(DISMISS_KEY, JSON.stringify(next));
      return next;
    });
  };

  const currentExecutor = health.data?.version ?? null;
  const clientOutdated = Boolean(clientLatest && dismissed.client !== clientLatest);
  const executorOutdated = Boolean(
    executorLatest &&
      currentExecutor &&
      isNewerVersion(executorLatest, currentExecutor) &&
      dismissed.executor !== executorLatest,
  );

  if (!clientOutdated && !executorOutdated) {
    return null;
  }

  return (
    <div className="space-y-2 px-6 pt-4">
      {clientOutdated && clientLatest && (
        <div className="flex items-start gap-3 rounded-lg border border-accent/30 bg-accent/10 px-4 py-2.5">
          <ArrowUpCircle size={15} className="mt-0.5 shrink-0 text-accent" />
          <div className="min-w-0 flex-1">
            <p className="text-xs font-medium text-fg">
              {fmt(t.updates.clientTitle, { version: clientLatest })}
            </p>
            <p className="mt-0.5 text-[11px] leading-relaxed text-muted">{t.updates.clientBody}</p>
          </div>
          <Button size="sm" onClick={() => navigate("/settings?tab=client")}>
            {t.updates.action}
          </Button>
          <button
            type="button"
            onClick={() => dismiss("client", clientLatest)}
            title={t.updates.dismiss}
            aria-label={t.updates.dismiss}
            className="mt-0.5 shrink-0 rounded p-0.5 text-subtle transition-colors hover:bg-hover hover:text-body"
          >
            <X size={13} />
          </button>
        </div>
      )}

      {executorOutdated && executorLatest && currentExecutor && (
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
          <Button size="sm" onClick={() => navigate("/settings?tab=deploy")}>
            {t.updates.action}
          </Button>
          <button
            type="button"
            onClick={() => dismiss("executor", executorLatest)}
            title={t.updates.dismiss}
            aria-label={t.updates.dismiss}
            className="mt-0.5 shrink-0 rounded p-0.5 text-subtle transition-colors hover:bg-hover hover:text-body"
          >
            <X size={13} />
          </button>
        </div>
      )}
    </div>
  );
}
