import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { FolderOpen, Lock, ShieldAlert, Trash2, Users, Wallet } from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../components/ui/card";
import { BillingCard } from "../components/BillingCard";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import DocsPage from "./Docs";
import {
  useAccounts,
  useClearAccounts,
  useCreateMarketAuthor,
  useCreateSettlement,
  useCreateTelegramPairing,
  useMarketAuthors,
  useNotificationSettings,
  useRevokeTelegramBinding,
  useRiskLimits,
  useSettlements,
  useTelegramStatus,
  useUpdateMarketAuthor,
  useUpdateNotificationSettings,
  useUpdateRiskLimits,
  useUpdateSettlement,
} from "../hooks/useApi";
import { useAccount } from "../hooks/useAccount";
import { isDesktopRuntime } from "../lib/deploy";
import {
  getApiToken,
  getExecutorMode,
  getExecutorUrl,
  hasLocalExecutor,
  localExecutorDatabase,
  normalizeExecutorUrl,
  setApiToken,
  setExecutorMode,
  setExecutorUrl,
  type ExecutorMode,
} from "../lib/api";
import { formatDateTime, formatUsd } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import { openExternal } from "../lib/openExternal";
import type {
  NotificationSettings,
  RiskLimits,
  SettlementStatus,
  VenueRiskLimits,
} from "../lib/types";
import { cn } from "../lib/utils";

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

const smallInputClass =
  "w-full rounded-md border border-line bg-panel-soft px-2 py-1 text-xs text-body outline-none focus:border-accent/60";

type SettingsTab =
  "connection" | "risk" | "account" | "author" | "notify" | "telegram";

const SETTINGS_TABS: SettingsTab[] = [
  "connection",
  "risk",
  "account",
  "author",
  "notify",
  "telegram",
];

const isSettingsTab = (value: string | null): value is SettingsTab =>
  SETTINGS_TABS.includes(value as SettingsTab);

export default function SettingsPage() {
  const queryClient = useQueryClient();
  const [mode, setMode] = useState<ExecutorMode>(getExecutorMode());
  const [token, setToken] = useState(getApiToken());
  const [address, setAddress] = useState(getExecutorUrl());
  const [saved, setSaved] = useState(false);
  const loopback =
    mode === "self" &&
    /^https?:\/\/(127\.0\.0\.1|localhost)(:|\/|$)/.test(address.trim());
  const { t } = useI18n();
  // 支持从升级提示直接跳到连接标签页：/settings?tab=connection
  const [searchParams, setSearchParams] = useSearchParams();
  const requested = searchParams.get("tab");
  const tab: SettingsTab = isSettingsTab(requested) ? requested : "connection";
  const setTab = (next: SettingsTab) => {
    const params = new URLSearchParams(searchParams);
    params.set("tab", next);
    setSearchParams(params, { replace: true });
  };

  /** 切换模式时载入该模式自己的地址与令牌，两者互不覆盖。 */
  const switchMode = (next: ExecutorMode) => {
    setMode(next);
    setAddress(getExecutorUrl(next));
    setToken(getApiToken(next));
  };

  const save = () => {
    const prevMode = getExecutorMode();
    const prevUrl = getExecutorUrl();
    setExecutorMode(mode);
    // 托管模式的令牌与地址由用户自己填写；非托管模式使用内置执行器与官方数据接口。
    if (mode === "self") {
      const nextUrl = normalizeExecutorUrl(address);
      setExecutorUrl(nextUrl);
      setAddress(nextUrl);
      setApiToken(token.trim());
    }
    if (
      mode !== prevMode ||
      (mode === "self" && getExecutorUrl() !== prevUrl)
    ) {
      window.location.reload();
      return;
    }
    setSaved(true);
    queryClient.invalidateQueries();
    window.setTimeout(() => setSaved(false), 2_000);
  };

  const modeOptions: { id: ExecutorMode; label: string }[] = [
    ...(hasLocalExecutor()
      ? [{ id: "local" as ExecutorMode, label: t.settings.localExecutor }]
      : []),
    { id: "self", label: t.settings.self },
  ];

  const tabs: { id: SettingsTab; label: string }[] = [
    { id: "connection", label: t.settings.tabConnection },
    { id: "risk", label: t.settings.tabRisk },
    { id: "account", label: t.settings.tabAccount },
    { id: "author", label: t.settings.tabAuthor },
    { id: "notify", label: t.settings.tabNotify },
    { id: "telegram", label: t.settings.tabTelegram },
  ];

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.settings.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.settings.subtitle}</p>
      </header>

      <div className="flex flex-wrap gap-1.5">
        {tabs.map((item) => (
          <button
            key={item.id}
            type="button"
            onClick={() => setTab(item.id)}
            className={cn(
              "rounded-lg border px-3 py-1.5 text-xs transition-colors",
              tab === item.id
                ? "border-accent/40 bg-accent/10 font-medium text-fg"
                : "border-line text-muted hover:bg-hover hover:text-body",
            )}
          >
            {item.label}
          </button>
        ))}
      </div>

      {tab === "connection" && (
        <div className="space-y-4">
          <Card>
            <CardHeader>
              <div>
                <CardTitle>{t.settings.connection}</CardTitle>
                <CardDescription>{t.settings.connectionDesc}</CardDescription>
              </div>
              <Badge tone="info">
                {mode === "local"
                  ? t.settings.localExecutor
                  : loopback
                    ? t.settings.local
                    : t.settings.remote}
              </Badge>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-1.5">
                <span className="text-xs text-muted">{t.settings.mode}</span>
                <div className="flex gap-1.5">
                  {modeOptions.map((item) => (
                    <button
                      key={item.id}
                      type="button"
                      onClick={() => switchMode(item.id)}
                      className={cn(
                        "rounded-lg border px-3 py-1.5 text-xs transition-colors",
                        mode === item.id
                          ? "border-accent/40 bg-accent/10 font-medium text-fg"
                          : "border-line text-muted hover:bg-hover hover:text-body",
                      )}
                    >
                      {item.label}
                    </button>
                  ))}
                </div>
                <span className="block text-[11px] leading-relaxed text-muted">
                  {t.settings.modeHint}
                </span>
                {mode === "self" && (
                  <div className="flex gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2">
                    <ShieldAlert size={14} className="mt-0.5 shrink-0 text-warning" />
                    <p className="text-[11px] leading-relaxed text-warning">
                      {t.settings.selfHostedWarning}
                    </p>
                  </div>
                )}
              </div>

              {mode === "local" ? (
                <div className="space-y-1.5">
                  <span className="text-xs text-muted">
                    {t.settings.address}
                  </span>
                  <div className={`${inputClass} font-mono text-muted`}>
                    {getExecutorUrl("local")}
                  </div>
                  <span className="text-xs text-muted">
                    {t.settings.database}
                  </span>
                  <div className={`${inputClass} font-mono text-muted`}>
                    {localExecutorDatabase()}
                  </div>
                  <span className="block text-[11px] leading-relaxed text-muted">
                    {t.settings.localExecutorNote}
                  </span>
                </div>
              ) : (
                <>
                  <label className="block space-y-1.5">
                    <span className="text-xs text-muted">
                      {t.settings.address}
                    </span>
                    <input
                      className={`${inputClass} font-mono`}
                      value={address}
                      onChange={(event) => setAddress(event.target.value)}
                      placeholder="http://45.76.177.242:8787 或 https://exec.example.com"
                    />
                    <span className="block text-[11px] leading-relaxed text-muted">
                      {t.settings.addressHint}
                    </span>
                  </label>
                  <label className="block space-y-1.5">
                    <span className="text-xs text-muted">
                      {t.settings.token}
                    </span>
                    <input
                      className={`${inputClass} font-mono`}
                      type="password"
                      value={token}
                      onChange={(event) => setToken(event.target.value)}
                      placeholder="ARBPILOT_API_TOKEN"
                    />
                    <span className="block text-[11px] leading-relaxed text-muted">
                      {t.settings.tokenNote}
                    </span>
                  </label>
                </>
              )}

              <div className="flex items-center gap-3">
                <Button size="sm" onClick={save}>
                  {t.settings.save}
                </Button>
                {saved && (
                  <span className="text-xs text-accent">
                    {t.settings.saved}
                  </span>
                )}
              </div>
            </CardContent>
          </Card>
          <CredentialStorageCard local={getExecutorMode() === "local"} />
          {mode === "self" && <DocsPage embedded />}
        </div>
      )}

      {tab === "risk" && <RiskLimitsCard />}

      {tab === "account" && <BillingCard />}

      {tab === "author" && <AuthorSettings />}

      {tab === "notify" && <NotificationCard />}

      {tab === "telegram" && <TelegramCard />}
    </div>
  );
}

/** 展示凭证文件的加密方式、路径与权限，并支持一键清除全部凭证。 */
function CredentialStorageCard({ local }: { local: boolean }) {
  const { t } = useI18n();
  const accounts = useAccounts();
  const clearAccounts = useClearAccounts();
  const [confirming, setConfirming] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const storage = accounts.data?.storage;
  const loaded = Boolean(accounts.data);
  const count = accounts.data?.accounts.length ?? 0;
  const encryptionLabel = !storage
    ? "—"
    : !storage.encrypted
      ? t.settings.credentialsUnavailable
      : storage.backend === "keychain"
        ? t.settings.credentialsKeychain
        : t.settings.credentialsKeyFile;

  const reveal = async () => {
    if (!storage?.path) {
      return;
    }
    setNotice(null);
    try {
      if (!isDesktopRuntime() || !local) {
        await navigator.clipboard.writeText(storage.path);
        setNotice(t.settings.credentialsPathCopied);
        return;
      }
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(storage.path);
    } catch {
      setNotice(t.settings.credentialsOpenFailed);
    }
  };

  const clearAll = () => {
    setError(null);
    clearAccounts.mutate(undefined, {
      onSuccess: (result) => {
        setConfirming(false);
        setNotice(fmt(t.settings.credentialsCleared, { count: result.removed }));
      },
      onError: (mutationError) =>
        setError(
          mutationError instanceof Error ? mutationError.message : String(mutationError),
        ),
    });
  };

  return (
    <>
      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.settings.credentials}</CardTitle>
            <CardDescription>{t.settings.credentialsDesc}</CardDescription>
          </div>
          <Badge tone={!storage ? "neutral" : storage.encrypted ? "success" : "warning"}>
            <Lock size={11} />
            {encryptionLabel}
          </Badge>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="space-y-1">
            <span className="text-xs text-muted">{t.settings.credentialsPath}</span>
            <div className={cn(inputClass, "break-all font-mono text-xs text-muted")}>
              {storage?.path ?? "—"}
            </div>
            {!local && (
              <span className="block text-[11px] leading-relaxed text-muted">
                {t.settings.credentialsServerPath}
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-x-6 gap-y-2 text-xs">
            <div className="flex items-center gap-2">
              <span className="text-muted">{t.settings.credentialsPermissions}</span>
              <span className="rounded bg-panel-soft px-2 py-0.5 font-mono text-fg">
                {storage?.permissions
                  ? `-${storage.permissions}`
                  : storage?.exists
                    ? "—"
                    : t.settings.credentialsNotCreated}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-muted">{t.settings.credentialsEncryption}</span>
              <span className="text-fg">{encryptionLabel}</span>
            </div>
          </div>

          {storage?.permissions && (
            <span className="block text-[11px] leading-relaxed text-muted">
              {t.settings.credentialsPermissionsNote}
            </span>
          )}

          <div className="flex flex-wrap items-center gap-2 pt-1">
            {local && storage?.exists && (
              <Button size="sm" variant="secondary" onClick={reveal}>
                <FolderOpen size={13} />
                {t.settings.credentialsOpenFolder}
              </Button>
            )}
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                setError(null);
                setNotice(null);
                setConfirming(true);
              }}
              disabled={!loaded || count === 0 || clearAccounts.isPending}
            >
              <Trash2 size={13} />
              {t.settings.credentialsClear}
            </Button>
          </div>

          {loaded && count === 0 && (
            <p className="text-[11px] leading-relaxed text-muted">
              {t.settings.credentialsClearEmpty}
            </p>
          )}
          {notice && <p className="text-xs text-accent">{notice}</p>}
          {error && <p className="text-xs text-danger">{error}</p>}
        </CardContent>
      </Card>

      <ConfirmDialog
        open={confirming}
        title={t.settings.credentialsClearConfirmTitle}
        message={t.settings.credentialsClearConfirm}
        confirmLabel={t.settings.credentialsClearConfirmAction}
        cancelLabel={t.settings.cancel}
        pending={clearAccounts.isPending}
        error={error}
        onCancel={() => {
          if (clearAccounts.isPending) {
            return;
          }
          setConfirming(false);
          setError(null);
        }}
        onConfirm={clearAll}
      />
    </>
  );
}

/** 用户资料与结算都只处理平台账户 ID，不接触私钥或银行账号。 */
function AuthorSettings() {
  const { t } = useI18n();
  const copy = t.market;
  const authors = useMarketAuthors();
  const account = useAccount();
  const createAuthor = useCreateMarketAuthor();
  const updateAuthor = useUpdateMarketAuthor();
  const createSettlement = useCreateSettlement();
  const updateSettlement = useUpdateSettlement();

  const [selectedId, setSelectedId] = useState("");
  const active =
    authors.data?.find((author) => author.author_id === selectedId) ??
    authors.data?.[0] ??
    null;
  const activeId = active?.author_id ?? "";
  const settlements = useSettlements(activeId || undefined);
  // 收款标识固定使用当前登录的平台账号 ID，不允许手动修改。
  const payoutAccount = account.data?.user.id ?? active?.payout_account ?? "";

  const [form, setForm] = useState({ display_name: "", bio: "" });

  // 切换用户或保存成功后，用最新数据回填表单。
  useEffect(() => {
    if (!active) {
      return;
    }
    setForm({ display_name: active.display_name, bio: active.bio });
  }, [active?.author_id, active?.updated_at]);

  const pending = createAuthor.isPending || updateAuthor.isPending;
  const error = [
    createAuthor.error,
    updateAuthor.error,
    createSettlement.error,
    updateSettlement.error,
  ].find(Boolean);

  const saveAuthor = () => {
    const name = form.display_name.trim();
    if (!name || pending) {
      return;
    }
    const body = {
      display_name: name,
      bio: form.bio,
      ...(payoutAccount ? { payout_account: payoutAccount } : {}),
    };
    if (active) {
      updateAuthor.mutate({ id: active.author_id, body });
    } else {
      createAuthor.mutate(body);
    }
  };

  const settlementTone: Record<
    SettlementStatus,
    "success" | "warning" | "danger"
  > = {
    pending: "warning",
    paid: "success",
    disputed: "danger",
  };

  const settlementLabel: Record<SettlementStatus, string> = {
    pending: copy.statusPending,
    paid: copy.statusPaid,
    disputed: copy.statusDisputed,
  };

  return (
    <div className="space-y-4">
      {error && (
        <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {fmt(copy.actionFailed, {
            message: error instanceof Error ? error.message : String(error),
          })}
        </p>
      )}

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Users size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.authorsTitle}</CardTitle>
                <CardDescription>{copy.authorsDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-3">
            {authors.data?.length ? (
              <div className="flex flex-wrap gap-2">
                {authors.data.map((author) => (
                  <button
                    key={author.author_id}
                    type="button"
                    onClick={() => setSelectedId(author.author_id)}
                    className={cn(
                      "rounded-lg border px-3 py-2 text-left text-xs transition-colors",
                      author.author_id === activeId
                        ? "border-accent/50 bg-accent/5"
                        : "border-line hover:bg-hover",
                    )}
                  >
                    <span className="block font-medium text-fg">
                      {author.display_name}
                    </span>
                  </button>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted">{copy.authorsEmpty}</p>
            )}

            <div className="grid gap-2">
              <label className="text-[11px] text-muted">
                {copy.displayName}
                <input
                  className={smallInputClass}
                  value={form.display_name}
                  onChange={(event) =>
                    setForm({ ...form, display_name: event.target.value })
                  }
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.bio}
                <textarea
                  className={cn(smallInputClass, "min-h-16")}
                  value={form.bio}
                  onChange={(event) =>
                    setForm({ ...form, bio: event.target.value })
                  }
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.payout}
                <input
                  className={cn(
                    smallInputClass,
                    "cursor-not-allowed opacity-70",
                  )}
                  placeholder={copy.payoutHint}
                  value={payoutAccount}
                  readOnly
                />
              </label>
            </div>
            <Button
              size="sm"
              disabled={!form.display_name.trim() || pending}
              onClick={saveAuthor}
            >
              {pending ? copy.creating : copy.saveAuthor}
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <Wallet size={15} className="text-accent" />
              <div>
                <CardTitle>{copy.settlementsTitle}</CardTitle>
                <CardDescription>
                  {copy.settlementsDesc.replace(
                    "{bps}",
                    String(settlements.data?.[0]?.platform_fee_bps ?? 2000),
                  )}
                </CardDescription>
              </div>
            </div>
            <Button
              size="sm"
              variant="secondary"
              disabled={!activeId || createSettlement.isPending}
              onClick={() =>
                activeId && createSettlement.mutate({ authorId: activeId })
              }
            >
              {copy.createSettlement}
            </Button>
          </CardHeader>
          <CardContent className="space-y-2">
            {settlements.data?.length ? (
              settlements.data.map((settlement) => (
                <div
                  key={settlement.id}
                  className="rounded-lg border border-line px-3 py-2 text-xs"
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-medium text-fg">
                      {settlement.period} ·{" "}
                      {fmt(copy.subscriptionCount, {
                        count: settlement.subscription_count,
                      })}
                    </span>
                    <Badge tone={settlementTone[settlement.status]}>
                      {settlementLabel[settlement.status]}
                    </Badge>
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted">
                    <span>
                      {copy.gross} {formatUsd(settlement.gross_usd)}
                    </span>
                    <span>
                      {copy.platformFee}{" "}
                      {formatUsd(settlement.platform_fee_usd)}
                    </span>
                    <span className="text-body">
                      {copy.net} {formatUsd(settlement.net_usd)}
                    </span>
                  </div>
                  <div className="mt-1.5 flex gap-1.5">
                    {settlement.status !== "paid" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          updateSettlement.mutate({
                            id: settlement.id,
                            status: "paid",
                          })
                        }
                      >
                        {copy.markPaid}
                      </Button>
                    )}
                    {settlement.status !== "disputed" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          updateSettlement.mutate({
                            id: settlement.id,
                            status: "disputed",
                          })
                        }
                      >
                        {copy.markDisputed}
                      </Button>
                    )}
                  </div>
                </div>
              ))
            ) : (
              <p className="text-xs text-muted">{copy.settlementsEmpty}</p>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

function LimitsField({
  label,
  hint,
  value,
  onChange,
  step,
}: {
  label: string;
  hint?: string;
  value: string;
  onChange: (value: string) => void;
  step?: string;
}) {
  return (
    <label className="block space-y-1.5">
      <span className="text-[11px] text-muted">{label}</span>
      <input
        className={`${inputClass} font-mono`}
        type="number"
        min="0"
        step={step ?? "any"}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
      {hint && (
        <span className="block text-[11px] leading-relaxed text-subtle">
          {hint}
        </span>
      )}
    </label>
  );
}

interface VenueLimitsDraft {
  venue: string;
  maxOrder: string;
  maxLeverage: string;
  maxSlippage: string;
  maxLegs: string;
  symbols: string;
}

interface LimitsDraft {
  maxOrder: string;
  maxTotal: string;
  maxLeverage: string;
  maxSlippage: string;
  quoteAge: string;
  maxLegs: string;
  symbols: string;
  venues: string;
  overrides: VenueLimitsDraft[];
}

function toDraft(limits: RiskLimits): LimitsDraft {
  return {
    maxOrder: String(limits.max_order_notional_usd),
    maxTotal: String(limits.max_total_notional_usd),
    maxLeverage: String(limits.max_leverage),
    maxSlippage: String(limits.max_slippage_bps),
    quoteAge: String(limits.max_quote_age_secs),
    maxLegs: String(limits.max_legs),
    symbols: limits.allowed_symbols.join(", "),
    venues: limits.allowed_venues.join(", "),
    overrides: Object.entries(limits.venue_overrides ?? {}).map(
      ([venue, value]) => ({
        venue,
        maxOrder:
          value.max_order_notional_usd === undefined
            ? ""
            : String(value.max_order_notional_usd),
        maxLeverage:
          value.max_leverage === undefined ? "" : String(value.max_leverage),
        maxSlippage:
          value.max_slippage_bps === undefined
            ? ""
            : String(value.max_slippage_bps),
        maxLegs: value.max_legs === undefined ? "" : String(value.max_legs),
        symbols: value.allowed_symbols?.join(", ") ?? "",
      }),
    ),
  };
}

/** 空字段不提交，保持「沿用全局」。 */
function toVenueOverrides(
  overrides: VenueLimitsDraft[],
): Record<string, VenueRiskLimits> {
  const result: Record<string, VenueRiskLimits> = {};
  for (const item of overrides) {
    const venue = item.venue.trim().toLowerCase();
    if (!venue) continue;
    const value: VenueRiskLimits = {};
    if (item.maxOrder.trim()) {
      value.max_order_notional_usd = Number(item.maxOrder);
    }
    if (item.maxLeverage.trim()) {
      value.max_leverage = Number(item.maxLeverage);
    }
    if (item.maxSlippage.trim()) {
      value.max_slippage_bps = Number(item.maxSlippage);
    }
    if (item.maxLegs.trim()) {
      value.max_legs = Number(item.maxLegs);
    }
    if (item.symbols.trim()) {
      value.allowed_symbols = splitList(item.symbols);
    }
    result[venue] = value;
  }
  return result;
}

function splitList(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function RiskLimitsCard() {
  const { t } = useI18n();
  const limits = useRiskLimits();
  const update = useUpdateRiskLimits();
  const [draft, setDraft] = useState<LimitsDraft | null>(null);

  const startEdit = () => {
    if (!limits.data) return;
    update.reset();
    setDraft(toDraft(limits.data));
  };

  const cancel = () => {
    update.reset();
    setDraft(null);
  };

  const save = () => {
    if (!draft) return;
    update.mutate(
      {
        max_order_notional_usd: Number(draft.maxOrder),
        max_total_notional_usd: Number(draft.maxTotal),
        max_leverage: Number(draft.maxLeverage),
        max_slippage_bps: Number(draft.maxSlippage),
        max_quote_age_secs: Number(draft.quoteAge),
        max_legs: Number(draft.maxLegs),
        allowed_symbols: splitList(draft.symbols),
        allowed_venues: splitList(draft.venues),
        venue_overrides: toVenueOverrides(draft.overrides),
      },
      { onSuccess: () => setDraft(null) },
    );
  };

  const addOverride = () => {
    if (!draft) return;
    setDraft({
      ...draft,
      overrides: [
        ...draft.overrides,
        {
          venue: "",
          maxOrder: "",
          maxLeverage: "",
          maxSlippage: "",
          maxLegs: "",
          symbols: "",
        },
      ],
    });
  };

  const updateOverride = (index: number, patch: Partial<VenueLimitsDraft>) => {
    if (!draft) return;
    setDraft({
      ...draft,
      overrides: draft.overrides.map((item, itemIndex) =>
        itemIndex === index ? { ...item, ...patch } : item,
      ),
    });
  };

  const removeOverride = (index: number) => {
    if (!draft) return;
    setDraft({
      ...draft,
      overrides: draft.overrides.filter((_, itemIndex) => itemIndex !== index),
    });
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.limits}</CardTitle>
          <CardDescription>{t.settings.limitsDesc}</CardDescription>
        </div>
        <div className="flex items-center gap-2">
          {limits.isError && (
            <Badge tone="danger">{t.settings.notConnected}</Badge>
          )}
          {limits.data && !draft && (
            <Button size="sm" variant="secondary" onClick={startEdit}>
              {t.settings.limitsEdit}
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {(() => {
          if (!limits.data) {
            return (
              <p className="py-8 text-center text-xs text-muted">
                {t.settings.unavailable}
              </p>
            );
          }
          if (!draft) {
            const overrides = Object.entries(limits.data.venue_overrides ?? {});
            return (
              <div className="space-y-5">
                <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
                  <LimitItem
                    label={t.settings.maxOrder}
                    value={`$${Number(limits.data.max_order_notional_usd).toLocaleString()}`}
                  />
                  <LimitItem
                    label={t.settings.maxTotal}
                    value={`$${Number(limits.data.max_total_notional_usd).toLocaleString()}`}
                  />
                  <LimitItem
                    label={t.settings.maxLeverage}
                    value={`${Number(limits.data.max_leverage)}x`}
                  />
                  <LimitItem
                    label={t.settings.maxSlippage}
                    value={`${Number(limits.data.max_slippage_bps)} bps`}
                  />
                  <LimitItem
                    label={t.settings.quoteAge}
                    value={fmt(t.settings.seconds, {
                      value: limits.data.max_quote_age_secs,
                    })}
                  />
                  <LimitItem
                    label={t.settings.maxLegs}
                    value={String(limits.data.max_legs)}
                  />
                  <LimitItem
                    label={t.settings.symbols}
                    value={
                      limits.data.allowed_symbols.join(" / ") ||
                      t.settings.unlimited
                    }
                  />
                  <LimitItem
                    label={t.settings.venues}
                    value={
                      limits.data.allowed_venues.join(" / ") ||
                      t.settings.unlimited
                    }
                  />
                </dl>
                <div className="border-t border-line pt-4">
                  <p className="text-xs font-medium text-body">
                    {t.settings.venueOverrides}
                  </p>
                  {overrides.length === 0 ? (
                    <p className="mt-1 text-[11px] text-subtle">
                      {t.settings.venueOverridesEmpty}
                    </p>
                  ) : (
                    <div className="mt-2 space-y-2">
                      {overrides.map(([venue, value]) => (
                        <div
                          key={venue}
                          className="rounded-lg border border-line bg-panel-soft/60 px-3 py-2"
                        >
                          <p className="font-mono text-xs text-body">{venue}</p>
                          <dl className="mt-1.5 grid grid-cols-2 gap-x-4 gap-y-1.5 sm:grid-cols-5">
                            <LimitItem
                              label={t.settings.maxOrder}
                              value={
                                value.max_order_notional_usd === undefined
                                  ? t.settings.venueInherit
                                  : `$${Number(value.max_order_notional_usd).toLocaleString()}`
                              }
                            />
                            <LimitItem
                              label={t.settings.maxLeverage}
                              value={
                                value.max_leverage === undefined
                                  ? t.settings.venueInherit
                                  : `${Number(value.max_leverage)}x`
                              }
                            />
                            <LimitItem
                              label={t.settings.maxSlippage}
                              value={
                                value.max_slippage_bps === undefined
                                  ? t.settings.venueInherit
                                  : `${Number(value.max_slippage_bps)} bps`
                              }
                            />
                            <LimitItem
                              label={t.settings.maxLegs}
                              value={
                                value.max_legs === undefined
                                  ? t.settings.venueInherit
                                  : String(value.max_legs)
                              }
                            />
                            <LimitItem
                              label={t.settings.symbols}
                              value={
                                value.allowed_symbols === undefined
                                  ? t.settings.venueInherit
                                  : value.allowed_symbols.join(" / ") ||
                                    t.settings.unlimited
                              }
                            />
                          </dl>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            );
          }
          return (
            <div className="space-y-4">
              <div className="grid gap-3 sm:grid-cols-2">
                <LimitsField
                  label={t.settings.maxOrder}
                  value={draft.maxOrder}
                  onChange={(value) => setDraft({ ...draft, maxOrder: value })}
                />
                <LimitsField
                  label={t.settings.maxTotal}
                  value={draft.maxTotal}
                  onChange={(value) => setDraft({ ...draft, maxTotal: value })}
                />
                <LimitsField
                  label={t.settings.maxLeverage}
                  value={draft.maxLeverage}
                  step="0.1"
                  onChange={(value) =>
                    setDraft({ ...draft, maxLeverage: value })
                  }
                />
                <LimitsField
                  label={t.settings.maxSlippage}
                  value={draft.maxSlippage}
                  onChange={(value) =>
                    setDraft({ ...draft, maxSlippage: value })
                  }
                />
                <LimitsField
                  label={t.settings.quoteAge}
                  value={draft.quoteAge}
                  step="1"
                  onChange={(value) => setDraft({ ...draft, quoteAge: value })}
                />
                <LimitsField
                  label={t.settings.maxLegs}
                  hint={t.settings.limitsLegsHint}
                  value={draft.maxLegs}
                  step="1"
                  onChange={(value) => setDraft({ ...draft, maxLegs: value })}
                />
                <LimitsField
                  label={t.settings.symbols}
                  hint={t.settings.limitsSymbolsHint}
                  value={draft.symbols}
                  onChange={(value) => setDraft({ ...draft, symbols: value })}
                />
                <LimitsField
                  label={t.settings.venues}
                  hint={t.settings.limitsVenuesHint}
                  value={draft.venues}
                  onChange={(value) => setDraft({ ...draft, venues: value })}
                />
              </div>
              <div className="space-y-3 rounded-lg border border-line p-3">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div>
                    <p className="text-xs font-medium text-body">
                      {t.settings.venueOverrides}
                    </p>
                    <p className="mt-0.5 text-[11px] text-muted">
                      {t.settings.venueOverridesDesc}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={addOverride}
                    disabled={update.isPending}
                  >
                    {t.settings.venueAdd}
                  </Button>
                </div>
                {draft.overrides.length === 0 ? (
                  <p className="text-[11px] text-subtle">
                    {t.settings.venueOverridesEmpty}
                  </p>
                ) : (
                  <div className="space-y-3">
                    {draft.overrides.map((item, index) => (
                      <div
                        key={index}
                        className="space-y-2.5 rounded-md border border-line bg-panel-soft/60 p-3"
                      >
                        <div className="flex items-end gap-2">
                          <label className="block flex-1 space-y-1.5">
                            <span className="text-[11px] text-muted">
                              {t.settings.venueName}
                            </span>
                            <input
                              className={`${inputClass} font-mono`}
                              value={item.venue}
                              placeholder={t.settings.venueNamePlaceholder}
                              onChange={(event) =>
                                updateOverride(index, {
                                  venue: event.target.value,
                                })
                              }
                            />
                          </label>
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => removeOverride(index)}
                            disabled={update.isPending}
                          >
                            {t.settings.venueRemove}
                          </Button>
                        </div>
                        <div className="grid gap-2.5 sm:grid-cols-2">
                          <LimitsField
                            label={t.settings.maxOrder}
                            hint={t.settings.venueInherit}
                            value={item.maxOrder}
                            onChange={(value) =>
                              updateOverride(index, { maxOrder: value })
                            }
                          />
                          <LimitsField
                            label={t.settings.maxLeverage}
                            hint={t.settings.venueInherit}
                            step="0.1"
                            value={item.maxLeverage}
                            onChange={(value) =>
                              updateOverride(index, { maxLeverage: value })
                            }
                          />
                          <LimitsField
                            label={t.settings.maxSlippage}
                            hint={t.settings.venueInherit}
                            value={item.maxSlippage}
                            onChange={(value) =>
                              updateOverride(index, { maxSlippage: value })
                            }
                          />
                          <LimitsField
                            label={t.settings.maxLegs}
                            hint={t.settings.venueInherit}
                            step="1"
                            value={item.maxLegs}
                            onChange={(value) =>
                              updateOverride(index, { maxLegs: value })
                            }
                          />
                          <LimitsField
                            label={t.settings.symbols}
                            hint={t.settings.venueInherit}
                            value={item.symbols}
                            onChange={(value) =>
                              updateOverride(index, { symbols: value })
                            }
                          />
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-3">
                <Button size="sm" onClick={save} disabled={update.isPending}>
                  {t.settings.limitsSave}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={cancel}
                  disabled={update.isPending}
                >
                  {t.settings.limitsCancel}
                </Button>
                {update.isSuccess && (
                  <span className="text-xs text-accent">
                    {t.settings.limitsSaved}
                  </span>
                )}
                {update.isError && (
                  <span className="text-xs text-danger">
                    {update.error.message}
                  </span>
                )}
              </div>
            </div>
          );
        })()}
      </CardContent>
    </Card>
  );
}

function LimitItem({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[11px] text-muted">{label}</dt>
      <dd className="mt-0.5 font-mono text-xs text-body">{value}</dd>
    </div>
  );
}

/** 全部新闻来源；接口返回空列表时表示「全部来源」。 */
const NEWS_SOURCE_IDS = ["binance", "upbit", "okx", "bybit", "bitget"] as const;

interface NotificationDraft {
  newsEnabled: boolean;
  newsSources: string[];
  fundingEnabled: boolean;
  fundingThreshold: string;
  fundingCooldownHours: string;
  telegramEnabled: boolean;
  desktopEnabled: boolean;
}

function toNotificationDraft(
  settings: NotificationSettings,
): NotificationDraft {
  return {
    newsEnabled: settings.news_enabled,
    newsSources: settings.news_sources.length
      ? settings.news_sources
      : [...NEWS_SOURCE_IDS],
    fundingEnabled: settings.funding_enabled,
    fundingThreshold: String(Number(settings.funding_min_annualized_pct)),
    fundingCooldownHours: String(
      Math.round((settings.funding_cooldown_secs / 3600) * 10) / 10,
    ),
    telegramEnabled: settings.telegram_enabled,
    desktopEnabled: settings.desktop_enabled,
  };
}

/** 消息开关与消息配置：新闻推送、资金费率套利提醒的条件与渠道。 */
function NotificationCard() {
  const { t } = useI18n();
  const settings = useNotificationSettings();
  const update = useUpdateNotificationSettings();
  const [draft, setDraft] = useState<NotificationDraft | null>(null);
  const [section, setSection] = useState<"news" | "funding">("news");

  // 首次拿到配置后回填表单；保存成功后不清空，便于继续微调。
  useEffect(() => {
    if (settings.data && !draft) {
      setDraft(toNotificationDraft(settings.data));
    }
  }, [settings.data, draft]);

  const toggleSource = (source: string) => {
    if (!draft) return;
    const sources = draft.newsSources.includes(source)
      ? draft.newsSources.filter((item) => item !== source)
      : [...draft.newsSources, source];
    setDraft({ ...draft, newsSources: sources });
  };

  const save = () => {
    if (!draft) return;
    update.mutate({
      news_enabled: draft.newsEnabled,
      news_sources: draft.newsSources,
      funding_enabled: draft.fundingEnabled,
      funding_min_annualized_pct: Number(draft.fundingThreshold) || 0,
      funding_cooldown_secs: Math.round(
        (Number(draft.fundingCooldownHours) || 0) * 3600,
      ),
      telegram_enabled: draft.telegramEnabled,
      desktop_enabled: draft.desktopEnabled,
    });
  };

  const sourceName = (source: (typeof NEWS_SOURCE_IDS)[number]) =>
    t.news.sources[source].name;

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.notifyTitle}</CardTitle>
          <CardDescription>{t.settings.notifyDesc}</CardDescription>
        </div>
        {settings.isError && (
          <Badge tone="danger">{t.settings.notConnected}</Badge>
        )}
      </CardHeader>
      <CardContent className="space-y-4">
        {!draft ? (
          <p className="py-8 text-center text-xs text-muted">
            {settings.isError
              ? t.settings.notifyUnavailable
              : t.settings.unavailable}
          </p>
        ) : (
          <>
            <div className="flex flex-wrap gap-1.5">
              {(
                [
                  { id: "news", label: t.settings.notifyTabNews },
                  { id: "funding", label: t.settings.notifyTabFunding },
                ] as const
              ).map((item) => (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => setSection(item.id)}
                  className={cn(
                    "rounded-lg border px-3 py-1.5 text-xs transition-colors",
                    section === item.id
                      ? "border-accent/40 bg-accent/10 font-medium text-fg"
                      : "border-line text-muted hover:bg-hover hover:text-body",
                  )}
                >
                  {item.label}
                </button>
              ))}
            </div>

            {section === "news" ? (
              <div className="space-y-3">
                <NotifyToggle
                  label={t.settings.notifyNews}
                  hint={t.settings.notifyNewsHint}
                  checked={draft.newsEnabled}
                  onChange={(checked) =>
                    setDraft({ ...draft, newsEnabled: checked })
                  }
                />

                <div className="space-y-2">
                  <p className="text-[11px] text-muted">
                    {t.settings.notifyNewsSources}
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {NEWS_SOURCE_IDS.map((source) => {
                      const active = draft.newsSources.includes(source);
                      return (
                        <button
                          key={source}
                          type="button"
                          disabled={!draft.newsEnabled}
                          onClick={() => toggleSource(source)}
                          className={cn(
                            "rounded-lg border px-2.5 py-1 text-xs transition-colors disabled:opacity-50",
                            active
                              ? "border-accent/40 bg-accent/10 text-fg"
                              : "border-line text-muted hover:bg-hover",
                          )}
                        >
                          {sourceName(source)}
                        </button>
                      );
                    })}
                  </div>
                </div>
              </div>
            ) : (
              <div className="space-y-3">
                <NotifyToggle
                  label={t.settings.notifyFunding}
                  hint={t.settings.notifyFundingHint}
                  checked={draft.fundingEnabled}
                  onChange={(checked) =>
                    setDraft({ ...draft, fundingEnabled: checked })
                  }
                />

                <div className="grid gap-3 sm:grid-cols-2">
                  <label className="block space-y-1.5">
                    <span className="text-[11px] text-muted">
                      {t.settings.notifyThreshold}
                    </span>
                    <input
                      className={`${inputClass} font-mono`}
                      type="number"
                      min="0"
                      step="0.1"
                      disabled={!draft.fundingEnabled}
                      value={draft.fundingThreshold}
                      onChange={(event) =>
                        setDraft({
                          ...draft,
                          fundingThreshold: event.target.value,
                        })
                      }
                    />
                    <span className="block text-[11px] leading-relaxed text-subtle">
                      {t.settings.notifyThresholdHint}
                    </span>
                  </label>
                  <label className="block space-y-1.5">
                    <span className="text-[11px] text-muted">
                      {t.settings.notifyCooldown}
                    </span>
                    <input
                      className={`${inputClass} font-mono`}
                      type="number"
                      min="0"
                      step="0.5"
                      disabled={!draft.fundingEnabled}
                      value={draft.fundingCooldownHours}
                      onChange={(event) =>
                        setDraft({
                          ...draft,
                          fundingCooldownHours: event.target.value,
                        })
                      }
                    />
                    <span className="block text-[11px] leading-relaxed text-subtle">
                      {t.settings.notifyCooldownHint}
                    </span>
                  </label>
                </div>
              </div>
            )}

            <div className="space-y-2">
              <p className="text-[11px] text-muted">
                {t.settings.notifyChannels}
              </p>
              <div className="grid gap-2 sm:grid-cols-2">
                <NotifyToggle
                  label={t.settings.notifyTelegram}
                  hint={t.settings.notifyTelegramHint}
                  checked={draft.telegramEnabled}
                  onChange={(checked) =>
                    setDraft({ ...draft, telegramEnabled: checked })
                  }
                />
                <NotifyToggle
                  label={t.settings.notifyDesktop}
                  hint={t.settings.notifyDesktopHint}
                  checked={draft.desktopEnabled}
                  onChange={(checked) =>
                    setDraft({ ...draft, desktopEnabled: checked })
                  }
                />
              </div>
            </div>

            <div className="flex items-center gap-3">
              <Button size="sm" onClick={save} disabled={update.isPending}>
                {t.settings.notifySave}
              </Button>
              {update.isSuccess && (
                <span className="text-xs text-accent">
                  {t.settings.notifySaved}
                </span>
              )}
              {update.isError && (
                <span className="text-xs text-danger">
                  {update.error.message}
                </span>
              )}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function NotifyToggle({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-start justify-between gap-3 rounded-lg border border-line px-3 py-2.5">
      <span className="min-w-0">
        <span className="block text-xs font-medium text-body">{label}</span>
        <span className="mt-0.5 block text-[11px] leading-relaxed text-muted">
          {hint}
        </span>
      </span>
      <input
        type="checkbox"
        className="mt-0.5 h-4 w-4 shrink-0 cursor-pointer accent-accent"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
    </label>
  );
}

function TelegramCard() {
  const { t } = useI18n();
  const status = useTelegramStatus();
  const pairing = useCreateTelegramPairing();
  const revoke = useRevokeTelegramBinding();
  const data = status.data;

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.telegram}</CardTitle>
          <CardDescription>{t.settings.telegramDesc}</CardDescription>
        </div>
        <div className="flex items-center gap-2">
          {data?.bot_username && (
            <Badge tone="info">@{data.bot_username}</Badge>
          )}
          <Badge tone={data?.enabled ? "success" : "neutral"}>
            {data?.enabled
              ? t.settings.telegramEnabled
              : t.settings.telegramDisabled}
          </Badge>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {status.isError ? (
          <p className="py-4 text-center text-xs text-muted">
            {t.settings.unavailable}
          </p>
        ) : !data ? null : (
          <>
            {!data.enabled && (
              <p className="rounded-lg border border-line bg-panel-soft px-3 py-2 text-[11px] leading-relaxed text-muted">
                {t.settings.telegramNotConfigured}
              </p>
            )}

            <div className="flex flex-wrap items-center gap-3">
              <Button
                size="sm"
                onClick={() => pairing.mutate()}
                disabled={pairing.isPending}
              >
                {pairing.isPending
                  ? t.settings.telegramPairing
                  : t.settings.telegramPairingAction}
              </Button>
              {data.enabled && data.polling && (
                <Badge tone="success">{t.settings.telegramPolling}</Badge>
              )}
            </div>

            {pairing.data && (
              <div className="rounded-lg border border-accent/30 bg-accent/5 px-3 py-3">
                <p className="text-[11px] text-muted">
                  {t.settings.telegramPairingHint}
                </p>
                <p className="mt-1 font-mono text-lg tracking-[0.15em] text-accent">
                  /bind {pairing.data.code}
                </p>
                <p className="mt-1 text-[11px] text-muted">
                  {fmt(t.settings.telegramExpiresAt, {
                    value: formatDateTime(pairing.data.expires_at),
                  })}
                </p>
                {data.bot_username && (
                  <Button
                    size="sm"
                    variant="secondary"
                    className="mt-2"
                    onClick={() => {
                      const command = `/bind ${pairing.data?.code ?? ""}`;
                      void openExternal(
                        `https://t.me/${data.bot_username}?text=${encodeURIComponent(command)}`,
                      );
                    }}
                  >
                    {t.settings.telegramOpenInBrowser}
                  </Button>
                )}
              </div>
            )}
            {pairing.isError && (
              <p className="text-xs text-danger">{pairing.error.message}</p>
            )}
            {data.enabled && data.last_error && (
              <p className="text-xs text-warning">{data.last_error}</p>
            )}

            {data.enabled && (
              <div>
                <p className="mb-2 text-xs text-muted">
                  {t.settings.telegramBindings}
                </p>
                {data.bindings.length === 0 ? (
                  <p className="text-xs text-subtle">
                    {t.settings.telegramEmpty}
                  </p>
                ) : (
                  <ul className="divide-y divide-line rounded-lg border border-line">
                    {data.bindings.map((binding) => (
                      <li
                        key={binding.telegram_user_id}
                        className="flex flex-wrap items-center justify-between gap-2 px-3 py-2"
                      >
                        <div>
                          <p className="text-sm text-body">
                            {binding.username
                              ? `@${binding.username}`
                              : binding.telegram_user_id}
                          </p>
                          <p className="mt-0.5 font-mono text-[11px] text-subtle">
                            ID {binding.telegram_user_id} ·{" "}
                            {formatDateTime(binding.paired_at)}
                          </p>
                        </div>
                        <div className="flex items-center gap-2">
                          {binding.permissions.map((permission) => (
                            <Badge key={permission} tone="neutral">
                              {permission}
                            </Badge>
                          ))}
                          <Button
                            size="sm"
                            variant="danger"
                            disabled={revoke.isPending}
                            onClick={() =>
                              revoke.mutate(binding.telegram_user_id)
                            }
                          >
                            {t.settings.telegramRevoke}
                          </Button>
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
          </>
        )}
        <p className="text-[11px] leading-relaxed text-muted">
          {t.settings.telegramNote}
        </p>
      </CardContent>
    </Card>
  );
}
