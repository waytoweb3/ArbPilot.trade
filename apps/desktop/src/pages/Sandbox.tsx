import { useState } from "react";
import { Boxes, Cpu, FileUp, Play, ShieldCheck, Unlink } from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import {
  useBindSandbox,
  useSandboxArtifacts,
  useSandboxDryRun,
  useStrategies,
  useUnbindSandbox,
  useUploadSandboxArtifact,
} from "../hooks/useApi";
import { formatDateTime, formatNumber } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { SandboxDecision, SandboxRunStatus } from "../lib/types";
import { cn } from "../lib/utils";

const inputClass =
  "w-full rounded-md border border-line bg-panel-soft px-2 py-1 text-xs text-body outline-none focus:border-accent/60";

const ABI_VERSION = 1;

function decisionText(decision: SandboxDecision): string {
  switch (decision.action) {
    case "open":
      return `Open ${decision.symbol} · $${formatNumber(decision.notional_usd, 2)}`;
    case "close":
      return `Close ${decision.symbol}`;
    default:
      return "Hold";
  }
}

export default function SandboxPage() {
  const { t } = useI18n();
  const copy = t.sandbox;

  const artifacts = useSandboxArtifacts();
  const strategies = useStrategies();
  const upload = useUploadSandboxArtifact();
  const bind = useBindSandbox();
  const unbind = useUnbindSandbox();
  const dryRun = useSandboxDryRun();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [bindStrategyId, setBindStrategyId] = useState("");
  const [dryRunStrategyId, setDryRunStrategyId] = useState("");
  const [symbol, setSymbol] = useState("");

  const selectedArtifact =
    artifacts.data?.find((artifact) => artifact.id === selectedId) ?? artifacts.data?.[0] ?? null;
  const activeStrategyId = bindStrategyId || strategies.data?.[0]?.id || "";
  const activeStrategy = strategies.data?.find((record) => record.id === activeStrategyId) ?? null;
  const binding = activeStrategy?.sandbox ?? null;
  const boundArtifact = binding
    ? (artifacts.data ?? []).find((artifact) => artifact.id === binding.artifact_id)
    : null;

  const error =
    (upload.error instanceof Error && upload.error.message) ||
    (bind.error instanceof Error && bind.error.message) ||
    (unbind.error instanceof Error && unbind.error.message) ||
    (dryRun.error instanceof Error && dryRun.error.message) ||
    null;

  const statusLabel: Record<SandboxRunStatus, string> = {
    ok: copy.statusOk,
    invalid: copy.statusInvalid,
    error: copy.statusError,
    timeout: copy.statusTimeout,
  };

  const statusTone: Record<SandboxRunStatus, "success" | "warning" | "danger"> = {
    ok: "success",
    invalid: "warning",
    error: "danger",
    timeout: "danger",
  };

  const report = dryRun.data ?? null;

  return (
    <div className="space-y-4">
      <header>
        <h1 className="text-lg font-semibold text-fg">{copy.title}</h1>
        <p className="mt-1 max-w-3xl text-xs text-muted">{copy.subtitle}</p>
      </header>

      {error && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {fmt(copy.actionFailed, { message: error })}
        </div>
      )}

      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <ShieldCheck size={15} className="text-accent" />
            <div>
              <CardTitle>{copy.isolationTitle}</CardTitle>
              <CardDescription>{copy.isolationDesc}</CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <ul className="grid gap-1.5 text-xs text-muted sm:grid-cols-2">
            {copy.isolationPoints.map((point) => (
              <li key={point} className="flex gap-2">
                <span className="mt-1.5 h-1 w-1 shrink-0 rounded-full bg-accent" />
                {point}
              </li>
            ))}
          </ul>
        </CardContent>
      </Card>

      <div className="grid gap-4 xl:grid-cols-[0.9fr_1.1fr]">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <FileUp size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.uploadTitle}</CardTitle>
                <CardDescription>{fmt(copy.uploadDesc, { abi: ABI_VERSION })}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            <label className="block text-[11px] text-muted">
              {copy.nameLabel}
              <input
                className={inputClass}
                value={name}
                onChange={(event) => setName(event.target.value)}
              />
            </label>
            <label className="block text-[11px] text-muted">
              {copy.fileLabel}
              <input
                type="file"
                accept=".wasm,application/wasm"
                className={cn(inputClass, "py-1.5")}
                onChange={(event) => setFile(event.target.files?.[0] ?? null)}
              />
            </label>
            <p className="text-[11px] text-subtle">
              {fmt(copy.uploadHint, { size: formatNumber(512, 0) })}
            </p>
            <Button
              size="sm"
              disabled={!file || upload.isPending}
              onClick={async () => {
                if (!file) {
                  return;
                }
                const bytes = await file.arrayBuffer();
                upload.mutate(
                  { name: name.trim() || file.name.replace(/\.wasm$/i, ""), bytes },
                  {
                    onSuccess: (artifact) => {
                      setSelectedId(artifact.id);
                      setName("");
                      setFile(null);
                    },
                  },
                );
              }}
            >
              <FileUp size={13} />
              {upload.isPending ? copy.uploading : copy.upload}
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Boxes size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.artifactsTitle}</CardTitle>
                <CardDescription>{copy.artifactsEmpty}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            {artifacts.data?.length ? (
              artifacts.data.map((artifact) => (
                <button
                  key={artifact.id}
                  type="button"
                  onClick={() => setSelectedId(artifact.id)}
                  className={cn(
                    "w-full rounded-lg border px-3 py-2 text-left transition-colors",
                    artifact.id === selectedArtifact?.id
                      ? "border-accent/50 bg-accent/5"
                      : "border-line hover:bg-hover",
                  )}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-xs font-medium text-fg">{artifact.name}</span>
                    <Badge tone={artifact.abi_version === ABI_VERSION ? "success" : "warning"}>
                      {fmt(copy.abiLabel, { value: artifact.abi_version ?? "—" })}
                    </Badge>
                  </div>
                  <p className="mt-0.5 font-mono text-[11px] text-subtle">
                    {fmt(copy.hashLabel, { value: artifact.sha256.slice(0, 16) })}
                  </p>
                  <p className="mt-0.5 text-[11px] text-muted">
                    {fmt(copy.sizeLabel, { value: formatNumber(artifact.size_bytes / 1024, 1) })} ·{" "}
                    {formatDateTime(artifact.created_at)}
                  </p>
                </button>
              ))
            ) : (
              <p className="py-2 text-xs text-muted">{copy.artifactsEmpty}</p>
            )}
          </CardContent>
        </Card>
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Cpu size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.bindTitle}</CardTitle>
                <CardDescription>{copy.bindDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            <label className="block text-[11px] text-muted">
              {t.market.strategy}
              <select
                className={inputClass}
                value={activeStrategyId}
                onChange={(event) => setBindStrategyId(event.target.value)}
              >
                {(strategies.data ?? []).map((record) => (
                  <option key={record.id} value={record.id}>
                    {record.definition.name} · v{record.version} · {record.status}
                  </option>
                ))}
              </select>
            </label>

            <div className="rounded-lg border border-line bg-panel-soft px-3 py-2 text-xs">
              {binding ? (
                <>
                  <p className="text-body">
                    {fmt(copy.boundTo, {
                      value: boundArtifact?.name ?? binding.artifact_hash.slice(0, 12),
                    })}
                  </p>
                  <p className="mt-0.5 font-mono text-[11px] text-subtle">
                    {binding.artifact_hash.slice(0, 24)}
                  </p>
                  <p className="mt-0.5 text-[11px] text-muted">
                    {copy.limitFuel} {formatNumber(binding.limits.max_fuel, 0)} ·{" "}
                    {copy.limitMemory} {formatNumber(binding.limits.max_memory_bytes / 1024 / 1024, 0)} MiB
                    · {copy.limitTimeout} {binding.limits.timeout_ms} ms · {copy.limitDecisions}{" "}
                    {binding.limits.max_decisions}
                  </p>
                </>
              ) : (
                <p className="text-muted">{copy.notBound}</p>
              )}
            </div>

            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                disabled={!selectedArtifact || !activeStrategyId || bind.isPending}
                onClick={() => {
                  if (!selectedArtifact || !activeStrategyId) {
                    return;
                  }
                  bind.mutate({
                    strategyId: activeStrategyId,
                    body: { artifact_id: selectedArtifact.id },
                  });
                }}
              >
                {copy.bind}
              </Button>
              <Button
                size="sm"
                variant="secondary"
                disabled={!binding || !activeStrategyId || unbind.isPending}
                onClick={() => activeStrategyId && unbind.mutate(activeStrategyId)}
              >
                <Unlink size={13} />
                {copy.unbind}
              </Button>
            </div>
            <p className="text-[11px] text-subtle">{copy.limitsDesc}</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Play size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.dryRunTitle}</CardTitle>
                <CardDescription>{copy.dryRunDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="grid gap-2 sm:grid-cols-2">
              <label className="text-[11px] text-muted">
                {copy.strategyOptional}
                <select
                  className={inputClass}
                  value={dryRunStrategyId}
                  onChange={(event) => setDryRunStrategyId(event.target.value)}
                >
                  <option value="">—</option>
                  {(strategies.data ?? []).map((record) => (
                    <option key={record.id} value={record.id}>
                      {record.definition.name} · v{record.version}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.symbolLabel}
                <input
                  className={inputClass}
                  placeholder="BTCUSDT"
                  value={symbol}
                  onChange={(event) => setSymbol(event.target.value)}
                />
              </label>
            </div>

            <Button
              size="sm"
              disabled={!selectedArtifact || dryRun.isPending}
              onClick={() => {
                if (!selectedArtifact) {
                  return;
                }
                dryRun.mutate({
                  artifactId: selectedArtifact.id,
                  body: {
                    strategy_id: dryRunStrategyId || undefined,
                    symbol: symbol.trim() || undefined,
                  },
                });
              }}
            >
              <Play size={13} />
              {dryRun.isPending ? copy.running : copy.dryRun}
            </Button>

            {report && (
              <div className="space-y-2 rounded-lg border border-line bg-panel-soft p-3 text-xs">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-muted">{copy.reportStatus}</span>
                  <Badge tone={statusTone[report.status]}>{statusLabel[report.status]}</Badge>
                </div>
                <div className="grid grid-cols-2 gap-1 text-[11px] text-muted">
                  <span>{fmt(copy.reportFuel, { used: formatNumber(report.fuel_consumed, 0), limit: formatNumber(report.fuel_limit, 0) })}</span>
                  <span>{fmt(copy.reportDuration, { value: report.duration_ms })}</span>
                  <span>{fmt(copy.reportMemory, { value: formatNumber(report.max_memory_bytes / 1024 / 1024, 0) })}</span>
                  <span>{fmt(copy.reportOutput, { value: report.output_bytes })}</span>
                </div>
                {report.error && <p className="text-danger">{report.error}</p>}
                <div>
                  <p className="text-[11px] font-medium text-fg">{copy.reportDecisions}</p>
                  {report.decisions.length ? (
                    <ul className="mt-1 space-y-0.5 text-[11px] text-body">
                      {report.decisions.map((decision, index) => (
                        <li key={`${decision.action}-${index}`}>
                          {decisionText(decision)}
                          {decision.reason ? ` · ${decision.reason}` : ""}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className="text-[11px] text-muted">{copy.reportEmpty}</p>
                  )}
                </div>
                {report.logs.length > 0 && (
                  <div>
                    <p className="text-[11px] font-medium text-fg">{copy.reportLogs}</p>
                    <ul className="mt-1 max-h-32 space-y-0.5 overflow-y-auto font-mono text-[11px] text-subtle">
                      {report.logs.map((line, index) => (
                        <li key={`${index}-${line}`}>{line}</li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
