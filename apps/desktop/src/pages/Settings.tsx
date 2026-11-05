import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { BillingCard } from "../components/BillingCard";
import {
  useCreateTelegramPairing,
  useRevokeTelegramBinding,
  useRiskLimits,
  useTelegramStatus,
  useUpdateRiskLimits,
} from "../hooks/useApi";
import { DEFAULT_TOKEN, getApiToken, getExecutorUrl, isLocalExecutor, setApiToken, setExecutorUrl } from "../lib/api";
import { formatDateTime } from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type { RiskLimits } from "../lib/types";

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

export default function SettingsPage() {
  const queryClient = useQueryClient();
  const [token, setToken] = useState(getApiToken());
  const [address, setAddress] = useState(getExecutorUrl());
  const [saved, setSaved] = useState(false);
  const local = isLocalExecutor();
  const { t } = useI18n();

  const save = () => {
    const changed = address.trim().replace(/\/$/, "") !== getExecutorUrl();
    setExecutorUrl(address);
    setApiToken(token.trim() || DEFAULT_TOKEN);
    if (changed) {
      window.location.reload();
      return;
    }
    setSaved(true);
    queryClient.invalidateQueries();
    window.setTimeout(() => setSaved(false), 2_000);
  };

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-fg">{t.settings.title}</h1>
        <p className="mt-1 text-sm text-muted">{t.settings.subtitle}</p>
      </header>

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div>
              <CardTitle>{t.settings.connection}</CardTitle>
              <CardDescription>{t.settings.connectionDesc}</CardDescription>
            </div>
            <Badge tone="info">{local ? t.settings.local : t.settings.remote}</Badge>
          </CardHeader>
          <CardContent className="space-y-4">
            <label className="block space-y-1.5">
              <span className="text-xs text-muted">{t.settings.address}</span>
              <input
                className={`${inputClass} font-mono`}
                value={address}
                onChange={(event) => setAddress(event.target.value)}
                placeholder="http://127.0.0.1:8787"
              />
              <span className="block text-[11px] leading-relaxed text-muted">
                {t.settings.addressHint}
              </span>
            </label>
            <label className="block space-y-1.5">
              <span className="text-xs text-muted">{t.settings.token}</span>
              <input
                className={`${inputClass} font-mono`}
                type="password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
                placeholder="ARBPILOT_API_TOKEN"
              />
            </label>
            <div className="flex items-center gap-3">
              <Button size="sm" onClick={save}>
                {t.settings.save}
              </Button>
              {saved && <span className="text-xs text-accent">{t.settings.saved}</span>}
            </div>
            <p className="text-[11px] leading-relaxed text-muted">{t.settings.tokenNote}</p>
          </CardContent>
        </Card>

        <RiskLimitsCard />
      </div>

      <BillingCard />

      <TelegramCard />

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.settings.security}</CardTitle>
            <CardDescription>{t.settings.securityDesc}</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          <ul className="list-disc space-y-1.5 pl-5 text-xs leading-relaxed text-muted">
            <li>{t.settings.security1}</li>
            <li>{t.settings.security2}</li>
            <li>{t.settings.security3}</li>
            <li>{t.settings.security4}</li>
          </ul>
        </CardContent>
      </Card>
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
      {hint && <span className="block text-[11px] leading-relaxed text-subtle">{hint}</span>}
    </label>
  );
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
  };
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
      },
      { onSuccess: () => setDraft(null) },
    );
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.settings.limits}</CardTitle>
          <CardDescription>{t.settings.limitsDesc}</CardDescription>
        </div>
        <div className="flex items-center gap-2">
          {limits.isError && <Badge tone="danger">{t.settings.notConnected}</Badge>}
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
            return <p className="py-8 text-center text-xs text-muted">{t.settings.unavailable}</p>;
          }
          if (!draft) {
            return (
              <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
                <LimitItem
                  label={t.settings.maxOrder}
                  value={`$${Number(limits.data.max_order_notional_usd).toLocaleString()}`}
                />
                <LimitItem
                  label={t.settings.maxTotal}
                  value={`$${Number(limits.data.max_total_notional_usd).toLocaleString()}`}
                />
                <LimitItem label={t.settings.maxLeverage} value={`${Number(limits.data.max_leverage)}x`} />
                <LimitItem
                  label={t.settings.maxSlippage}
                  value={`${Number(limits.data.max_slippage_bps)} bps`}
                />
                <LimitItem
                  label={t.settings.quoteAge}
                  value={fmt(t.settings.seconds, { value: limits.data.max_quote_age_secs })}
                />
                <LimitItem label={t.settings.maxLegs} value={String(limits.data.max_legs)} />
                <LimitItem
                  label={t.settings.symbols}
                  value={limits.data.allowed_symbols.join(" / ") || t.settings.unlimited}
                />
                <LimitItem
                  label={t.settings.venues}
                  value={limits.data.allowed_venues.join(" / ") || t.settings.unlimited}
                />
              </dl>
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
                  onChange={(value) => setDraft({ ...draft, maxLeverage: value })}
                />
                <LimitsField
                  label={t.settings.maxSlippage}
                  value={draft.maxSlippage}
                  onChange={(value) => setDraft({ ...draft, maxSlippage: value })}
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
              <div className="flex items-center gap-3">
                <Button size="sm" onClick={save} disabled={update.isPending}>
                  {t.settings.limitsSave}
                </Button>
                <Button size="sm" variant="ghost" onClick={cancel} disabled={update.isPending}>
                  {t.settings.limitsCancel}
                </Button>
                {update.isSuccess && <span className="text-xs text-accent">{t.settings.limitsSaved}</span>}
                {update.isError && <span className="text-xs text-danger">{update.error.message}</span>}
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
          {data?.bot_username && <Badge tone="info">@{data.bot_username}</Badge>}
          <Badge tone={data?.enabled ? "success" : "neutral"}>
            {data?.enabled ? t.settings.telegramEnabled : t.settings.telegramDisabled}
          </Badge>
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {status.isError ? (
          <p className="py-4 text-center text-xs text-muted">{t.settings.unavailable}</p>
        ) : !data ? null : (
          <>
            {!data.enabled && (
              <p className="rounded-lg border border-line bg-panel-soft px-3 py-2 text-[11px] leading-relaxed text-muted">
                {t.settings.telegramNotConfigured}
              </p>
            )}

            <div className="flex flex-wrap items-center gap-3">
              <Button size="sm" onClick={() => pairing.mutate()} disabled={pairing.isPending}>
                {pairing.isPending ? t.settings.telegramPairing : t.settings.telegramPairingAction}
              </Button>
              {data.enabled && data.polling && (
                <Badge tone="success">{t.settings.telegramPolling}</Badge>
              )}
            </div>

            {pairing.data && (
              <div className="rounded-lg border border-accent/30 bg-accent/5 px-3 py-3">
                <p className="text-[11px] text-muted">{t.settings.telegramPairingHint}</p>
                <p className="mt-1 font-mono text-lg tracking-[0.15em] text-accent">
                  /bind {pairing.data.code}
                </p>
                <p className="mt-1 text-[11px] text-muted">
                  {fmt(t.settings.telegramExpiresAt, {
                    value: formatDateTime(pairing.data.expires_at),
                  })}
                </p>
              </div>
            )}
            {pairing.isError && <p className="text-xs text-danger">{pairing.error.message}</p>}
            {data.enabled && data.last_error && (
              <p className="text-xs text-warning">{data.last_error}</p>
            )}

            {data.enabled && (
              <div>
                <p className="mb-2 text-xs text-muted">{t.settings.telegramBindings}</p>
                {data.bindings.length === 0 ? (
                  <p className="text-xs text-subtle">{t.settings.telegramEmpty}</p>
                ) : (
                  <ul className="divide-y divide-line rounded-lg border border-line">
                    {data.bindings.map((binding) => (
                      <li
                        key={binding.telegram_user_id}
                        className="flex flex-wrap items-center justify-between gap-2 px-3 py-2"
                      >
                        <div>
                          <p className="text-sm text-body">
                            {binding.username ? `@${binding.username}` : binding.telegram_user_id}
                          </p>
                          <p className="mt-0.5 font-mono text-[11px] text-subtle">
                            ID {binding.telegram_user_id} · {formatDateTime(binding.paired_at)}
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
                            onClick={() => revoke.mutate(binding.telegram_user_id)}
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
        <p className="text-[11px] leading-relaxed text-muted">{t.settings.telegramNote}</p>
      </CardContent>
    </Card>
  );
}
