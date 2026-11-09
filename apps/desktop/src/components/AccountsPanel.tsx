import { Plus, RefreshCw, ShieldAlert, Trash2, Wallet } from "lucide-react";
import { useMemo, useState, type FormEvent } from "react";

import {
  useAccountHistory,
  useAccounts,
  useImportAccount,
  useRefreshAccount,
  useRemoveAccount,
} from "../hooks/useApi";
import { toNumber } from "../lib/api";
import { formatDateTime, formatNumber, formatUsd } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { AccountKind, AccountVenue, EquityPoint, TradingAccount } from "../lib/types";
import { cn } from "../lib/utils";
import { AccountEquityChart } from "./AccountEquityChart";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { ConfirmDialog } from "./ui/confirm-dialog";

/** CEX 用 API Key 签名；DEX 与 Aster 用钱包地址查询余额，用签名私钥下单。 */
const VENUES: { id: AccountVenue; kind: AccountKind }[] = [
  { id: "binance", kind: "api_key" },
  { id: "okx", kind: "api_key" },
  { id: "bybit", kind: "api_key" },
  { id: "bitget", kind: "api_key" },
  { id: "gate", kind: "api_key" },
  { id: "aster", kind: "wallet" },
  { id: "hyperliquid", kind: "wallet" },
  { id: "lighter", kind: "wallet" },
];
const RANGES = [24, 168, 720] as const;

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

/** 总览页的交易账户面板：导入 Key、余额汇总、权益走势。 */
export function AccountsPanel() {
  const { t } = useI18n();
  const accounts = useAccounts();
  const [hours, setHours] = useState<number>(168);
  const history = useAccountHistory(hours);
  const removeAccount = useRemoveAccount();
  const refreshAccount = useRefreshAccount();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [pendingRemove, setPendingRemove] = useState<string | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);

  const rangeLabels: Record<number, string> = {
    24: t.accounts.range24h,
    168: t.accounts.range7d,
    720: t.accounts.range30d,
  };
  const venueLabel = (venue: string) =>
    (t.accounts.venues as Record<string, string>)[venue] ?? venue;

  const points = history.data?.points ?? [];
  const change = useMemo(() => computeChange(points), [points]);
  const list = accounts.data?.accounts ?? [];

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.accounts.title}</CardTitle>
          <CardDescription>{t.accounts.subtitle}</CardDescription>
        </div>
        {list.length > 0 && (
          <Button size="sm" variant="secondary" onClick={() => setDialogOpen(true)}>
            <Plus size={14} />
            {t.accounts.emptyCta}
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-5">
        {accounts.isError ? (
          <p className="py-8 text-center text-xs text-muted">{t.accounts.unavailable}</p>
        ) : list.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-10">
            <Wallet size={28} className="text-subtle" />
            <p className="max-w-md text-center text-xs leading-relaxed text-muted">
              {t.accounts.empty}
            </p>
            <Button size="sm" onClick={() => setDialogOpen(true)}>
              {t.accounts.emptyCta}
            </Button>
          </div>
        ) : (
          <>
            <div className="flex flex-wrap items-end justify-between gap-4">
              <div>
                <p className="text-xs text-muted">{t.accounts.totalEquity}</p>
                <p className="mt-1 text-2xl font-semibold text-fg">
                  {formatUsd(accounts.data?.total_equity_usd)}
                </p>
                <div className="mt-1.5 flex items-center gap-2 text-[11px] text-muted">
                  {change ? (
                    <Badge tone={change.pct >= 0 ? "success" : "danger"}>
                      {change.pct >= 0 ? "+" : ""}
                      {change.pct.toFixed(2)}%
                    </Badge>
                  ) : (
                    <span>{t.accounts.noChange}</span>
                  )}
                  <span>{fmt(t.accounts.changeHint, { value: rangeLabels[hours] })}</span>
                </div>
              </div>
              <div className="flex items-center gap-1">
                {RANGES.map((option) => (
                  <button
                    key={option}
                    type="button"
                    onClick={() => setHours(option)}
                    className={cn(
                      "rounded-md px-2.5 py-1 text-xs transition-colors",
                      hours === option
                        ? "bg-accent/10 font-medium text-fg"
                        : "text-muted hover:bg-hover hover:text-body",
                    )}
                  >
                    {rangeLabels[option]}
                  </button>
                ))}
              </div>
            </div>

            <div className="rounded-lg border border-line bg-panel-soft/40 p-2">
              <div className="px-2 pt-1">
                <p className="text-xs font-medium text-body">{t.accounts.chartTitle}</p>
                <p className="text-[11px] text-muted">{t.accounts.chartDesc}</p>
              </div>
              {points.length > 0 ? (
                <AccountEquityChart points={points} />
              ) : (
                <p className="py-14 text-center text-xs text-muted">{t.accounts.chartEmpty}</p>
              )}
            </div>

            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {list.map((account) => (
                <AccountCard
                  key={account.id}
                  account={account}
                  venueLabel={venueLabel(account.venue)}
                  refreshing={refreshAccount.isPending && refreshAccount.variables === account.id}
                  onRefresh={() => refreshAccount.mutate(account.id)}
                  onRemove={() => {
                    setRemoveError(null);
                    setPendingRemove(account.id);
                  }}
                />
              ))}
            </div>
          </>
        )}
      </CardContent>

      {dialogOpen && <ImportAccountDialog onClose={() => setDialogOpen(false)} />}

      <ConfirmDialog
        open={pendingRemove !== null}
        title={t.accounts.remove}
        message={t.accounts.removeConfirm}
        confirmLabel={t.accounts.remove}
        cancelLabel={t.accounts.cancel}
        pending={removeAccount.isPending}
        error={removeError}
        onCancel={() => {
          if (removeAccount.isPending) {
            return;
          }
          setPendingRemove(null);
          setRemoveError(null);
        }}
        onConfirm={() => {
          if (!pendingRemove) {
            return;
          }
          setRemoveError(null);
          removeAccount.mutate(pendingRemove, {
            onSuccess: () => setPendingRemove(null),
            onError: (error) =>
              setRemoveError(
                fmt(t.accounts.removeFailed, {
                  message: error instanceof Error ? error.message : String(error),
                }),
              ),
          });
        }}
      />
    </Card>
  );
}

function AccountCard({
  account,
  venueLabel,
  refreshing,
  onRefresh,
  onRemove,
}: {
  account: TradingAccount;
  venueLabel: string;
  refreshing: boolean;
  onRefresh: () => void;
  onRemove: () => void;
}) {
  const { t } = useI18n();
  const snapshot = account.snapshot;
  const failed = Boolean(snapshot?.error);

  return (
    <div className="rounded-lg border border-line p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium text-fg">{account.label}</p>
          <p className="truncate text-[11px] text-muted">
            {venueLabel} ·{" "}
            {fmt(account.kind === "wallet" ? t.accounts.walletLabel : t.accounts.keyLabel, {
              value: account.api_key_preview,
            })}{" "}
            ·{" "}
            <span className={account.can_trade ? "text-success" : "text-subtle"}>
              {account.can_trade ? t.accounts.tradeEnabled : t.accounts.readOnly}
            </span>
          </p>
        </div>
        <Badge tone={failed ? "danger" : snapshot ? "success" : "neutral"}>
          {failed ? t.accounts.statusError : snapshot ? t.accounts.statusOk : t.accounts.neverUpdated}
        </Badge>
      </div>

      <p className="mt-2 text-lg font-semibold text-fg">{formatUsd(snapshot?.equity_usd)}</p>

      {failed ? (
        <p className="mt-1 break-words text-[11px] leading-relaxed text-danger">{snapshot?.error}</p>
      ) : (
        <ul className="mt-2 space-y-1">
          {(snapshot?.balances ?? []).slice(0, 3).map((balance) => (
            <li key={balance.asset} className="flex items-center justify-between gap-2 text-[11px]">
              <span className="text-muted">{balance.asset}</span>
              <span className="truncate font-mono text-body">
                {formatNumber(toNumber(balance.free) + toNumber(balance.locked), 6)} ·{" "}
                {balance.usd_value === null ? "—" : formatUsd(balance.usd_value)}
              </span>
            </li>
          ))}
          {(snapshot?.balances ?? []).length === 0 && (
            <li className="text-[11px] text-muted">{t.accounts.balancesEmpty}</li>
          )}
        </ul>
      )}

      <div className="mt-3 flex items-center justify-between gap-2">
        <span className="truncate text-[11px] text-subtle">
          {snapshot
            ? fmt(t.accounts.updatedAt, { value: formatDateTime(snapshot.observed_at) })
            : t.accounts.neverUpdated}
        </span>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={onRefresh}
            disabled={refreshing}
            title={t.accounts.refresh}
            className="rounded-md p-1 text-muted transition-colors hover:bg-hover hover:text-fg disabled:text-subtle"
          >
            <RefreshCw size={13} className={refreshing ? "animate-spin" : undefined} />
          </button>
          <button
            type="button"
            onClick={onRemove}
            title={t.accounts.remove}
            className="rounded-md p-1 text-muted transition-colors hover:bg-danger/10 hover:text-danger"
          >
            <Trash2 size={13} />
          </button>
        </div>
      </div>
    </div>
  );
}

function ImportAccountDialog({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const mutation = useImportAccount();
  const [venue, setVenue] = useState<AccountVenue>("binance");
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiSecret, setApiSecret] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [walletAddress, setWalletAddress] = useState("");
  const [signerKey, setSignerKey] = useState("");
  const [keyIndex, setKeyIndex] = useState("");
  const [accountIndex, setAccountIndex] = useState("");
  const [error, setError] = useState<string | null>(null);

  const kind = VENUES.find((item) => item.id === venue)?.kind ?? "api_key";
  const needsWallet = kind === "wallet";
  const needsPassphrase = venue === "okx" || venue === "bitget";
  const isLighter = venue === "lighter";
  const parsedKeyIndex = parseIndex(keyIndex);
  const parsedAccountIndex = parseIndex(accountIndex);
  const indexInvalid =
    isLighter &&
    ((keyIndex.trim().length > 0 && parsedKeyIndex === undefined) ||
      (accountIndex.trim().length > 0 && parsedAccountIndex === undefined));
  // 与执行器的导入校验保持一致：Hyperliquid 32 字节、Lighter API Key 40 字节。
  const signerKeyInvalid =
    needsWallet &&
    signerKey.trim().length > 0 &&
    !(venue === "lighter"
      ? /^(0x)?[0-9a-fA-F]{80}$/.test(signerKey.trim())
      : /^(0x)?[0-9a-fA-F]{64}$/.test(signerKey.trim()));
  const canSubmit =
    (needsWallet
      ? walletAddress.trim().length > 0 && signerKey.trim().length > 0
      : apiKey.trim().length > 0 &&
        apiSecret.trim().length > 0 &&
        (!needsPassphrase || passphrase.trim().length > 0)) &&
    !indexInvalid &&
    !signerKeyInvalid;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit) {
      return;
    }
    setError(null);
    mutation.mutate(
      {
        venue,
        label: label.trim() || undefined,
        api_key: needsWallet ? undefined : apiKey.trim(),
        api_secret: needsWallet ? undefined : apiSecret.trim(),
        passphrase: needsPassphrase ? passphrase.trim() : undefined,
        wallet_address: needsWallet ? walletAddress.trim() : undefined,
        // DEX 必须提供私钥才能签名下单，导入时即校验。
        signer_key: needsWallet ? signerKey.trim() : undefined,
        key_index: isLighter ? parsedKeyIndex : undefined,
        account_index: isLighter ? parsedAccountIndex : undefined,
      },
      {
        onSuccess: onClose,
        onError: (mutationError) =>
          setError(mutationError instanceof Error ? mutationError.message : String(mutationError)),
      },
    );
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      role="dialog"
      aria-modal="true"
    >
      <div className="w-full max-w-md rounded-xl border border-line bg-panel shadow-xl">
        <div className="border-b border-line px-4 py-3">
          <p className="text-sm font-semibold text-fg">{t.accounts.importTitle}</p>
          <p className="mt-0.5 text-[11px] leading-relaxed text-muted">{t.accounts.importDesc}</p>
        </div>

        <form onSubmit={submit} className="space-y-3 p-4">
          <label className="block space-y-1.5">
            <span className="text-xs text-muted">{t.accounts.venue}</span>
            <select
              className={inputClass}
              value={venue}
              onChange={(event) => setVenue(event.target.value as AccountVenue)}
            >
              {VENUES.map((item) => (
                <option key={item.id} value={item.id}>
                  {t.accounts.venues[item.id]}
                </option>
              ))}
            </select>
          </label>

          <label className="block space-y-1.5">
            <span className="text-xs text-muted">{t.accounts.label}</span>
            <input
              className={inputClass}
              value={label}
              onChange={(event) => setLabel(event.target.value)}
              placeholder={t.accounts.labelPlaceholder}
            />
          </label>

          {needsWallet ? (
            <>
              <label className="block space-y-1.5">
                <span className="text-xs text-muted">
                  {venue === "lighter"
                    ? t.accounts.lighterAddress
                    : venue === "aster"
                      ? t.accounts.asterAddress
                      : t.accounts.walletAddress}
                </span>
                <input
                  className={`${inputClass} font-mono`}
                  value={walletAddress}
                  onChange={(event) => setWalletAddress(event.target.value)}
                  placeholder={venue === "lighter" ? "0x… / 12345" : "0x…"}
                  autoComplete="off"
                  spellCheck={false}
                />
                <span className="block text-[11px] leading-relaxed text-muted">
                  {t.accounts.walletHint}
                </span>
              </label>

              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.accounts.signerKey}</span>
                <input
                  className={`${inputClass} font-mono`}
                  type="password"
                  value={signerKey}
                  onChange={(event) => setSignerKey(event.target.value)}
                  placeholder={t.accounts.signerKeyPlaceholder}
                  autoComplete="off"
                  spellCheck={false}
                />
                <span className="block text-[11px] leading-relaxed text-muted">
                  {venue === "lighter"
                    ? t.accounts.signerKeyLighterHint
                    : venue === "aster"
                      ? t.accounts.signerKeyAsterHint
                      : t.accounts.signerKeyHint}
                </span>
                {signerKeyInvalid && (
                  <span className="block text-[11px] text-danger">
                    {t.accounts.signerKeyInvalid}
                  </span>
                )}
              </label>

              {isLighter && (
                <div className="grid grid-cols-2 gap-3">
                  <label className="block space-y-1.5">
                    <span className="text-xs text-muted">{t.accounts.lighterKeyIndex}</span>
                    <input
                      className={`${inputClass} font-mono`}
                      value={keyIndex}
                      onChange={(event) => setKeyIndex(event.target.value)}
                      placeholder="0"
                      inputMode="numeric"
                      autoComplete="off"
                    />
                  </label>
                  <label className="block space-y-1.5">
                    <span className="text-xs text-muted">{t.accounts.lighterAccountIndex}</span>
                    <input
                      className={`${inputClass} font-mono`}
                      value={accountIndex}
                      onChange={(event) => setAccountIndex(event.target.value)}
                      placeholder={t.accounts.lighterAccountIndexPlaceholder}
                      inputMode="numeric"
                      autoComplete="off"
                    />
                  </label>
                </div>
              )}
              {indexInvalid && (
                <p className="text-[11px] text-danger">{t.accounts.indexInvalid}</p>
              )}

              <div className="flex gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2">
                <ShieldAlert size={14} className="mt-0.5 shrink-0 text-warning" />
                <p className="text-[11px] leading-relaxed text-warning">
                  {t.accounts.signerSecurity}
                </p>
              </div>
            </>
          ) : (
            <>
              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.accounts.apiKey}</span>
                <input
                  className={`${inputClass} font-mono`}
                  value={apiKey}
                  onChange={(event) => setApiKey(event.target.value)}
                  autoComplete="off"
                  spellCheck={false}
                />
              </label>

              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.accounts.apiSecret}</span>
                <input
                  className={`${inputClass} font-mono`}
                  type="password"
                  value={apiSecret}
                  onChange={(event) => setApiSecret(event.target.value)}
                  autoComplete="off"
                />
              </label>
            </>
          )}

          {needsPassphrase && (
            <label className="block space-y-1.5">
              <span className="text-xs text-muted">{t.accounts.passphrase}</span>
              <input
                className={`${inputClass} font-mono`}
                type="password"
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
                autoComplete="off"
              />
              <span className="block text-[11px] text-muted">{t.accounts.passphraseHint}</span>
            </label>
          )}

          <p className="text-[11px] leading-relaxed text-muted">{t.accounts.secretNote}</p>

          {error && (
            <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-[11px] leading-relaxed text-danger">
              {error}
            </p>
          )}

          <div className="flex items-center justify-end gap-2 pt-1">
            <Button type="button" variant="secondary" size="sm" onClick={onClose}>
              {t.accounts.cancel}
            </Button>
            <Button type="submit" size="sm" disabled={!canSubmit || mutation.isPending}>
              {mutation.isPending ? t.accounts.submitting : t.accounts.submit}
            </Button>
          </div>
        </form>
      </div>
    </div>
  );
}

function parseIndex(raw: string): number | undefined {
  const text = raw.trim();
  if (!text) {
    return undefined;
  }
  const value = Number(text);
  return Number.isInteger(value) && value >= 0 ? value : undefined;
}

function computeChange(points: EquityPoint[]): { pct: number } | null {
  if (points.length < 2) {
    return null;
  }
  const first = toNumber(points[0].equity_usd);
  const last = toNumber(points[points.length - 1].equity_usd);
  if (!Number.isFinite(first) || first <= 0) {
    return null;
  }
  return { pct: ((last - first) / first) * 100 };
}
