import { ArrowUpCircle } from "lucide-react";
import { useEffect, useState } from "react";

import {
  currentAppVersion,
  relaunchApp,
  type DownloadProgress,
} from "../../lib/appUpdate";
import { useAppUpdateDialog } from "../../lib/appUpdateDialog";
import { fmt, useI18n } from "../../lib/i18n";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";

/** 发现客户端新版本时自动弹出的更新窗口；关闭后可通过左下角红点再次打开。 */
export function UpdateDialog() {
  const { t } = useI18n();
  const pending = useAppUpdateDialog((state) => state.pending);
  const open = useAppUpdateDialog((state) => state.open);
  const close = useAppUpdateDialog((state) => state.close);

  const [version, setVersion] = useState("");
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [installed, setInstalled] = useState(false);
  const [busy, setBusy] = useState<"install" | "restart" | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    currentAppVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, []);

  const latest = pending?.info.version;
  useEffect(() => {
    setProgress(null);
    setInstalled(false);
    setError(null);
    setBusy(null);
  }, [latest]);

  if (!open || !pending) {
    return null;
  }

  const install = async () => {
    setBusy("install");
    setError(null);
    try {
      await pending.install(setProgress);
      setInstalled(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const restart = async () => {
    setBusy("restart");
    setError(null);
    try {
      await relaunchApp();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setBusy(null);
    }
  };

  const percent = progress?.total
    ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
    : null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      role="dialog"
      aria-modal="true"
    >
      <div className="w-full max-w-md rounded-xl border border-line bg-panel shadow-xl">
        <div className="flex items-center gap-2 border-b border-line px-4 py-3">
          <ArrowUpCircle size={16} className="shrink-0 text-accent" />
          <p className="flex-1 text-sm font-semibold text-fg">
            {t.settings.update}
          </p>
          <Badge tone="neutral">v{pending.info.version}</Badge>
        </div>
        <div className="space-y-3 p-4">
          {version && (
            <p className="text-[11px] text-muted">
              {t.settings.updateCurrent} v{version} → {t.settings.updateLatest}{" "}
              v{pending.info.version}
            </p>
          )}
          <p className="text-xs text-body">
            {fmt(t.settings.updateAvailable, { version: pending.info.version })}
          </p>
          {pending.info.notes && (
            <div className="max-h-40 overflow-y-auto rounded-lg border border-line bg-panel-soft px-3 py-2">
              <p className="text-[11px] font-medium text-muted">
                {t.settings.updateNotes}
              </p>
              <p className="mt-1 whitespace-pre-wrap text-[11px] leading-relaxed text-body">
                {pending.info.notes}
              </p>
            </div>
          )}
          {progress && (
            <div className="space-y-1">
              <div className="h-1.5 overflow-hidden rounded-full bg-panel-soft">
                <div
                  className="h-full rounded-full bg-accent transition-all"
                  style={{ width: `${percent ?? 5}%` }}
                />
              </div>
              <p className="text-[11px] text-muted">
                {percent !== null
                  ? fmt(t.settings.updatePercent, { value: percent })
                  : t.settings.updateInstalling}
              </p>
            </div>
          )}
          {installed && (
            <p className="text-xs text-accent">{t.settings.updateInstalled}</p>
          )}
          {error && (
            <p className="break-words text-xs text-danger">
              {fmt(t.settings.updateFailed, { message: error })}
            </p>
          )}
          <p className="text-[11px] leading-relaxed text-muted">
            {t.settings.updateNote}
          </p>
          <div className="flex justify-end gap-2">
            <Button
              size="sm"
              variant="secondary"
              onClick={close}
              disabled={busy !== null}
            >
              {t.settings.updateLater}
            </Button>
            {installed ? (
              <Button size="sm" onClick={restart} disabled={busy !== null}>
                {busy === "restart"
                  ? t.settings.updateRestarting
                  : t.settings.updateRestart}
              </Button>
            ) : (
              <Button size="sm" onClick={install} disabled={busy !== null}>
                {busy === "install"
                  ? t.settings.updateInstalling
                  : t.settings.updateAction}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
