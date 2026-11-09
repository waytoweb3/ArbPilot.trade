import { useEffect, useState } from "react";
import {
  Bot,
  Code2,
  FlaskConical,
  History,
  Pause,
  Play,
  Plus,
  Rocket,
  Save,
  ShieldCheck,
  Sparkles,
  Store,
  Trash2,
} from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import { useAccount } from "../hooks/useAccount";
import {
  useBacktestStrategy,
  useCompileStrategyCode,
  useCreatePerformance,
  useCreatePublication,
  useCreateStrategy,
  useDeleteStrategy,
  useDeployStrategyCode,
  useGenerateStrategyCode,
  useInstantiateTemplate,
  useMarketAuthors,
  useMarketConfig,
  usePublicationPerformance,
  usePublications,
  useRollbackStrategy,
  useSandboxDryRun,
  useSaveStrategyCode,
  useStrategies,
  useStrategyCode,
  useStrategyRuns,
  useStrategyTemplates,
  useStrategyVersions,
  useUpdatePublication,
  useUpdateStrategy,
  useUpdateStrategyScope,
  useValidateStrategy,
} from "../hooks/useApi";
import { toNumber } from "../lib/api";
import {
  formatDateTime,
  formatNumber,
  formatPercentValue,
  formatUsd,
  strategyStatusLabel,
} from "../lib/format";
import { fmt, useI18n } from "../lib/i18n";
import type {
  BacktestReport,
  MarketVisibility,
  PerformanceSource,
  PricingModel,
  PublicationStatus,
  RiskLevel,
  SandboxDecision,
  SandboxRunStatus,
  SourceVisibility,
  StrategyEventLevel,
  StrategyRecord,
  StrategySource,
  StrategyStatus,
} from "../lib/types";
import { cn } from "../lib/utils";

const statusTone: Record<StrategyStatus, "success" | "warning" | "neutral" | "danger"> = {
  running: "success",
  paused: "warning",
  draft: "neutral",
  stopped: "danger",
};

const eventTone: Record<StrategyEventLevel, string> = {
  info: "bg-accent",
  warning: "bg-warning",
  error: "bg-danger",
};

const inputClass =
  "w-full rounded-md border border-line bg-panel-soft px-2 py-1 text-xs text-body outline-none focus:border-accent/60";

const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** 现货腿标识去掉 `-spot` 后缀后即为交易所配置里的交易对清单键。 */
const baseVenue = (venue: string) => venue.replace(/-spot$/, "");

/** 当前值不在配置里时也保留，避免下拉显示为空。 */
function withCurrent(values: string[], current: string): string[] {
  if (!current || values.includes(current)) {
    return values;
  }
  return [current, ...values];
}

interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

const chatStorageKey = (id: string) => `arbpilot.strategy-chat.${id}`;

/** 读取该策略的 AI 对话记录；解析失败时按空对话处理。 */
function loadChat(id: string): ChatMessage[] {
  try {
    const parsed: unknown = JSON.parse(window.localStorage.getItem(chatStorageKey(id)) ?? "[]");
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter(
      (item): item is ChatMessage =>
        typeof item === "object" &&
        item !== null &&
        typeof (item as ChatMessage).content === "string" &&
        ((item as ChatMessage).role === "user" || (item as ChatMessage).role === "assistant"),
    );
  } catch {
    return [];
  }
}


interface CreateDraft {
  name: string;
  prompt: string;
}

function CreateStrategyForm({
  pending,
  onCreate,
}: {
  pending: boolean;
  onCreate: (name: string, prompt: string) => void;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState<CreateDraft>({ name: "", prompt: "" });
  const [error, setError] = useState<string | null>(null);

  const submit = () => {
    const name = draft.name.trim();
    if (!name) {
      setError(t.strategies.createNameRequired);
      return;
    }
    setError(null);
    onCreate(name, draft.prompt.trim());
  };

  return (
    <div className="space-y-3">
      <label className="block space-y-1">
        <span className="text-[11px] text-muted">{t.strategies.createName}</span>
        <input
          className={inputClass}
          value={draft.name}
          placeholder={t.strategies.createNamePlaceholder}
          onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
        />
      </label>
      <label className="block space-y-1">
        <span className="text-[11px] text-muted">{t.strategies.codePrompt}</span>
        <textarea
          className={cn(inputClass, "h-24 resize-y")}
          value={draft.prompt}
          placeholder={t.strategies.codePromptPlaceholder}
          onChange={(event) => setDraft((current) => ({ ...current, prompt: event.target.value }))}
        />
      </label>
      {error && <p className="text-[11px] text-danger">{error}</p>}
      <Button size="sm" disabled={pending} onClick={submit}>
        <Sparkles size={13} />
        {pending ? t.strategies.creating : t.strategies.createSubmit}
      </Button>
    </div>
  );
}

function StrategyCodeCard({ record, running }: { record: StrategyRecord; running: boolean }) {
  const { t } = useI18n();
  const codeQuery = useStrategyCode(record.id);
  const saveCode = useSaveStrategyCode();
  const generateCode = useGenerateStrategyCode();
  const compileCode = useCompileStrategyCode();
  const deployCode = useDeployStrategyCode();

  const [source, setSource] = useState("");
  const [input, setInput] = useState("");
  const [chat, setChat] = useState<ChatMessage[]>(() => loadChat(record.id));
  const [notice, setNotice] = useState<{ tone: "ok" | "error"; text: string } | null>(null);

  const view = codeQuery.data;
  const code = view?.code ?? null;
  const template = view?.template ?? "";
  const codeUpdatedAt = code?.updated_at ?? null;

  useEffect(() => {
    if (!view) {
      return;
    }
    setSource(view.code?.source ?? view.template);
  }, [record.id, codeUpdatedAt, template]);

  useEffect(() => {
    window.localStorage.setItem(chatStorageKey(record.id), JSON.stringify(chat));
  }, [record.id, chat]);

  const dirty = code ? source !== code.source : source !== template;
  const pending =
    saveCode.isPending || generateCode.isPending || compileCode.isPending || deployCode.isPending;

  const send = async () => {
    const text = input.trim();
    if (!text) {
      setNotice({ tone: "error", text: t.strategies.codePromptRequired });
      return;
    }
    setNotice(null);
    setInput("");
    // 把历史对话一并提交，让 AI 在现有代码上继续迭代。
    const history = chat;
    setChat((current) => [...current, { role: "user", content: text }]);
    try {
      await generateCode.mutateAsync({ id: record.id, prompt: text, history });
      setChat((current) => [
        ...current,
        { role: "assistant", content: t.strategies.chatAssistant },
      ]);
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
      setInput(text);
    }
  };

  const save = async () => {
    setNotice(null);
    try {
      await saveCode.mutateAsync({ id: record.id, source });
      setNotice({ tone: "ok", text: t.strategies.saved });
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    }
  };

  const compile = async () => {
    setNotice(null);
    try {
      if (dirty) {
        await saveCode.mutateAsync({ id: record.id, source });
      }
      await compileCode.mutateAsync(record.id);
      setNotice({ tone: "ok", text: t.strategies.codeCompileOk });
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    }
  };

  const deploy = async () => {
    setNotice(null);
    try {
      if (dirty) {
        await saveCode.mutateAsync({ id: record.id, source });
      }
      await deployCode.mutateAsync(record.id);
      setNotice({ tone: "ok", text: t.strategies.codeDeployOk });
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    }
  };

  const originLabel = !code
    ? t.strategies.codeOriginTemplate
    : code.origin === "ai"
      ? t.strategies.codeOriginAi
      : t.strategies.codeOriginUser;

  return (
    <Card>
      <CardHeader className="flex-wrap">
        <div>
          <CardTitle>{t.strategies.codeTitle}</CardTitle>
          <CardDescription>{t.strategies.codeDesc}</CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <Badge tone="neutral">{originLabel}</Badge>
          {code?.artifact_sha256 && (
            <Badge tone="info">
              {fmt(t.strategies.codeArtifact, { hash: code.artifact_sha256.slice(0, 12) })}
            </Badge>
          )}
          {code &&
            (code.deployed_at ? (
              <Badge tone="success">
                {fmt(t.strategies.codeDeployedAt, { value: formatDateTime(code.deployed_at) })}
              </Badge>
            ) : (
              <Badge tone="warning">{t.strategies.codeNotDeployed}</Badge>
            ))}
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="space-y-2">
          <span className="text-[11px] text-muted">{t.strategies.codePrompt}</span>
          <div className="max-h-56 space-y-2 overflow-y-auto rounded-md border border-line bg-panel-soft p-2">
            {chat.length === 0 ? (
              <p className="text-[11px] text-muted">{t.strategies.chatEmpty}</p>
            ) : (
              chat.map((message, index) => (
                <div
                  key={`${message.role}-${index}`}
                  className={cn(
                    "flex",
                    message.role === "user" ? "justify-end" : "justify-start",
                  )}
                >
                  <p
                    className={cn(
                      "max-w-[85%] whitespace-pre-wrap rounded-lg px-2 py-1 text-[11px] text-body",
                      message.role === "user" ? "bg-accent/15" : "bg-panel",
                    )}
                  >
                    {message.content}
                  </p>
                </div>
              ))
            )}
            {generateCode.isPending && (
              <p className="text-[11px] text-muted">{t.strategies.chatSending}</p>
            )}
          </div>
          <div className="flex flex-col gap-2 md:flex-row md:items-end">
            <textarea
              className={cn(inputClass, "h-20 flex-1 resize-y")}
              value={input}
              placeholder={t.strategies.codePromptPlaceholder}
              disabled={running}
              onChange={(event) => setInput(event.target.value)}
            />
            <Button size="sm" variant="secondary" disabled={running || pending} onClick={send}>
              <Sparkles size={13} />
              {generateCode.isPending ? t.strategies.chatSending : t.strategies.chatSend}
            </Button>
          </div>
        </div>

        <div className="space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[11px] text-muted">{t.strategies.codeSource}</span>
            <Code2 size={13} className="text-muted" />
            {!code && <span className="text-[11px] text-muted">{t.strategies.codeTemplateHint}</span>}
          </div>
          <textarea
            className={cn(inputClass, "h-80 resize-y font-mono text-[11px] leading-relaxed")}
            spellCheck={false}
            value={source}
            onChange={(event) => setSource(event.target.value)}
          />
        </div>

        {running && <p className="text-[11px] text-warning">{t.strategies.codeNeedStop}</p>}

        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" variant="secondary" disabled={running || pending} onClick={save}>
            <Save size={13} />
            {saveCode.isPending ? t.strategies.codeSaving : t.strategies.codeSave}
          </Button>
          <Button size="sm" variant="secondary" disabled={running || pending} onClick={compile}>
            <ShieldCheck size={13} />
            {compileCode.isPending ? t.strategies.codeCompiling : t.strategies.codeCompile}
          </Button>
          <Button size="sm" disabled={running || pending} onClick={deploy}>
            <Rocket size={13} />
            {deployCode.isPending ? t.strategies.codeDeploying : t.strategies.codeDeploy}
          </Button>
        </div>

        {notice && (
          <p
            className={cn(
              "rounded-md border px-2 py-1 text-[11px]",
              notice.tone === "ok"
                ? "border-accent/30 bg-accent/10 text-accent"
                : "border-danger/30 bg-danger/10 text-danger",
            )}
          >
            {notice.text}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function StrategyScopeCard({ record, running }: { record: StrategyRecord; running: boolean }) {
  const { t } = useI18n();
  const config = useMarketConfig();
  const saveScope = useUpdateStrategyScope();

  const spotOptions = (config.data?.venues ?? []).filter(
    (item) => item.segment === "spot",
  );
  const perpOptions = (config.data?.venues ?? []).filter(
    (item) => item.segment === "perp",
  );

  const currentSpot =
    record.definition.universe.venues.find((venue) => venue.endsWith("-spot")) ?? "";
  const currentPerp =
    record.definition.universe.venues.find((venue) => !venue.endsWith("-spot")) ?? "";

  // 当前策略正在使用的交易所即使后来在数据库里被停用，也保留在下拉中，避免显示为空。
  const spotValues = withCurrent(spotOptions.map((item) => item.venue), currentSpot);
  const perpValues = withCurrent(perpOptions.map((item) => item.venue), currentPerp);

  const [spot, setSpot] = useState(currentSpot || spotValues[0] || "");
  const [perp, setPerp] = useState(currentPerp || perpValues[0] || "");
  const [selected, setSelected] = useState<string[]>(record.definition.universe.symbols);
  const [eventSources, setEventSources] = useState(
    (record.definition.data.event_sources ?? []).join(", "),
  );
  const [eventKeywords, setEventKeywords] = useState(
    (record.definition.data.event_keywords ?? []).join(", "),
  );
  const [eventWindow, setEventWindow] = useState(
    String(record.definition.data.event_window_secs ?? 86_400),
  );
  const [notice, setNotice] = useState<{ tone: "ok" | "error"; text: string } | null>(null);

  // 保存范围或回滚版本后，用最新定义同步表单。
  useEffect(() => {
    setSpot(currentSpot || spotValues[0] || "");
    setPerp(currentPerp || perpValues[0] || "");
    setSelected(record.definition.universe.symbols);
    setEventSources((record.definition.data.event_sources ?? []).join(", "));
    setEventKeywords((record.definition.data.event_keywords ?? []).join(", "));
    setEventWindow(String(record.definition.data.event_window_secs ?? 86_400));
  }, [record.id, record.version]);

  // 配置异步加载完成时补齐空选项，避免下拉有选项但值为空。
  useEffect(() => {
    setSpot((value) => value || spotValues[0] || "");
    setPerp((value) => value || perpValues[0] || "");
  }, [config.data]);

  const venueLabel = (venue: string) =>
    (config.data?.venues ?? []).find((item) => item.venue === venue)?.display_name || venue;

  // 现货与永续都配置了清单时取交集；只有一侧有数据时直接用该侧。
  const symbolOptions = (() => {
    const all = config.data?.symbols ?? {};
    const perpSymbols = all[baseVenue(perp)] ?? [];
    const spotSymbols = all[baseVenue(spot)] ?? [];
    if (perpSymbols.length > 0 && spotSymbols.length > 0) {
      const allowed = new Set(spotSymbols);
      return perpSymbols.filter((symbol) => allowed.has(symbol));
    }
    return perpSymbols.length > 0 ? perpSymbols : spotSymbols;
  })();
  const availableSymbols = symbolOptions.filter((symbol) => !selected.includes(symbol));

  const addSymbol = (symbol: string) => {
    if (!symbol || selected.includes(symbol)) {
      return;
    }
    setSelected((previous) => [...previous, symbol]);
  };

  const removeSymbol = (symbol: string) => {
    setSelected((previous) => previous.filter((item) => item !== symbol));
  };

  // 逗号分隔的来源与关键词，保存前去掉空白项。
  const splitList = (value: string) =>
    value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);

  const submit = () => {
    if (!spot || !perp) {
      setNotice({ tone: "error", text: t.strategies.scopeVenuesEmpty });
      return;
    }
    if (selected.length === 0) {
      setNotice({ tone: "error", text: t.strategies.scopeRequired });
      return;
    }
    const window = Number.parseInt(eventWindow, 10);
    if (!Number.isFinite(window) || window < 60 || window > 604_800) {
      setNotice({ tone: "error", text: t.strategies.scopeEventWindowInvalid });
      return;
    }
    setNotice(null);
    saveScope.mutate(
      {
        id: record.id,
        spot_venue: spot,
        perp_venue: perp,
        symbols: selected,
        event_sources: splitList(eventSources),
        event_keywords: splitList(eventKeywords),
        event_window_secs: window,
      },
      {
        onSuccess: () => setNotice({ tone: "ok", text: t.strategies.scopeSaved }),
        onError: (error) => setNotice({ tone: "error", text: errorText(error) }),
      },
    );
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.strategies.scopeTitle}</CardTitle>
          <CardDescription>{t.strategies.scopeDesc}</CardDescription>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {config.data && spotOptions.length === 0 && perpOptions.length === 0 && (
          <p className="rounded-md border border-warning/30 bg-warning/10 px-2 py-1 text-[11px] text-warning">
            {t.strategies.scopeVenuesEmpty}
          </p>
        )}

        <div className="grid gap-3 md:grid-cols-2">
          <label className="block space-y-1">
            <span className="text-[11px] text-muted">{t.strategies.scopeSpot}</span>
            <select
              className={inputClass}
              value={spot}
              disabled={running}
              onChange={(event) => setSpot(event.target.value)}
            >
              {spotValues.map((venue) => (
                <option key={venue} value={venue}>
                  {venueLabel(venue)}
                </option>
              ))}
            </select>
          </label>
          <label className="block space-y-1">
            <span className="text-[11px] text-muted">{t.strategies.scopePerp}</span>
            <select
              className={inputClass}
              value={perp}
              disabled={running}
              onChange={(event) => setPerp(event.target.value)}
            >
              {perpValues.map((venue) => (
                <option key={venue} value={venue}>
                  {venueLabel(venue)}
                </option>
              ))}
            </select>
          </label>
        </div>

        <label className="block space-y-1">
          <span className="text-[11px] text-muted">{t.strategies.scopeSymbols}</span>
          <select
            className={inputClass}
            value=""
            disabled={running || availableSymbols.length === 0}
            onChange={(event) => addSymbol(event.target.value)}
          >
            <option value="">
              {availableSymbols.length === 0
                ? t.strategies.scopeSymbolsEmpty
                : t.strategies.scopeSymbolsPlaceholder}
            </option>
            {availableSymbols.map((symbol) => (
              <option key={symbol} value={symbol}>
                {symbol}
              </option>
            ))}
          </select>
        </label>

        {selected.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {selected.map((symbol) => (
              <button
                key={symbol}
                type="button"
                disabled={running}
                onClick={() => removeSymbol(symbol)}
                className="rounded-full border border-accent/40 bg-accent/10 px-2 py-0.5 text-[11px] text-accent transition-colors hover:border-danger/40 hover:bg-danger/10 hover:text-danger"
              >
                {symbol} <span aria-hidden>×</span>
              </button>
            ))}
          </div>
        )}

        {record.definition.kind === "custom" && (
          <div className="space-y-2 rounded-md border border-border/60 p-3">
            <div>
              <p className="text-[11px] font-medium">{t.strategies.scopeEventsTitle}</p>
              <p className="text-[11px] text-muted">{t.strategies.scopeEventsDesc}</p>
            </div>
            <div className="grid gap-3 md:grid-cols-2">
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.scopeEventSources}</span>
                <input
                  className={inputClass}
                  value={eventSources}
                  disabled={running}
                  placeholder={t.strategies.scopeEventSourcesPlaceholder}
                  onChange={(event) => setEventSources(event.target.value)}
                />
              </label>
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.scopeEventKeywords}</span>
                <input
                  className={inputClass}
                  value={eventKeywords}
                  disabled={running}
                  placeholder={t.strategies.scopeEventKeywordsPlaceholder}
                  onChange={(event) => setEventKeywords(event.target.value)}
                />
              </label>
            </div>
            <label className="block space-y-1 md:w-40">
              <span className="text-[11px] text-muted">{t.strategies.scopeEventWindow}</span>
              <input
                className={inputClass}
                type="number"
                min={60}
                max={604_800}
                value={eventWindow}
                disabled={running}
                onChange={(event) => setEventWindow(event.target.value)}
              />
            </label>
          </div>
        )}

        {running && <p className="text-[11px] text-warning">{t.strategies.scopeNeedStop}</p>}

        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" disabled={running || saveScope.isPending} onClick={submit}>
            <Play size={13} />
            {saveScope.isPending ? t.strategies.scopeSaving : t.strategies.scopeSave}
          </Button>
          {notice && (
            <p
              className={cn(
                "rounded-md border px-2 py-1 text-[11px]",
                notice.tone === "ok"
                  ? "border-accent/30 bg-accent/10 text-accent"
                  : "border-danger/30 bg-danger/10 text-danger",
              )}
            >
              {notice.text}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function StrategyPublishCard({ record }: { record: StrategyRecord }) {
  const { t } = useI18n();
  const copy = t.market;
  const authors = useMarketAuthors();
  const publications = usePublications(true);
  const createPublication = useCreatePublication();
  const updatePublication = useUpdatePublication();
  const createPerformance = useCreatePerformance();

  const own = (publications.data ?? []).filter((item) => item.strategy_id === record.id);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = own.find((item) => item.id === selectedId) ?? own[0] ?? null;
  const performance = usePublicationPerformance(selected?.id ?? null);

  const [authorId, setAuthorId] = useState("");
  const activeAuthorId = authorId || authors.data?.[0]?.author_id || "";

  const [draft, setDraft] = useState({
    title: "",
    summary: "",
    logic: "",
    risk: "medium" as RiskLevel,
    notional: "1000",
    visibility: "invite_only" as MarketVisibility,
    sourceVisibility: "open_source" as SourceVisibility,
    pricing: "monthly" as PricingModel,
    price: "19",
    trial: "3",
    limits: "",
  });

  const [perfForm, setPerfForm] = useState({
    source: "author_backtest" as PerformanceSource,
    trades: "30",
    sample: "30",
    winRate: "55",
    returnPct: "4",
    drawdown: "3",
    netPnl: "120",
  });

  const error = [createPublication.error, updatePublication.error, createPerformance.error].find(
    Boolean,
  );

  const statusLabel: Record<PublicationStatus, string> = {
    private: copy.statusPrivate,
    published: copy.statusPublished,
    withdrawn: copy.statusWithdrawn,
  };

  const statusTone: Record<PublicationStatus, "success" | "warning" | "neutral"> = {
    private: "neutral",
    published: "success",
    withdrawn: "warning",
  };

  const sourceLabel: Record<PerformanceSource, string> = {
    author_backtest: copy.sourceAuthorBacktest,
    platform_replay: copy.sourcePlatformReplay,
    authorized_live: copy.sourceLive,
  };

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <Store size={15} className="text-accent" />
          <div>
            <CardTitle>{copy.publishTitle}</CardTitle>
            <CardDescription>{copy.publishDesc}</CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {error && (
          <p className="rounded-md border border-danger/30 bg-danger/10 px-2 py-1 text-[11px] text-danger">
            {errorText(error)}
          </p>
        )}

        {!authors.data?.length ? (
          <p className="rounded-md border border-line bg-panel-soft px-2 py-1.5 text-[11px] text-muted">
            {copy.authorMissing}
          </p>
        ) : (
          <>
            <div className="grid gap-2 sm:grid-cols-2">
              <label className="text-[11px] text-muted">
                {copy.authorCard}
                <select
                  className={inputClass}
                  value={activeAuthorId}
                  onChange={(event) => setAuthorId(event.target.value)}
                >
                  {(authors.data ?? []).map((author) => (
                    <option key={author.author_id} value={author.author_id}>
                      {author.display_name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.titleLabel}
                <input
                  className={inputClass}
                  value={draft.title}
                  onChange={(event) => setDraft({ ...draft, title: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.summaryLabel}
                <input
                  className={inputClass}
                  value={draft.summary}
                  onChange={(event) => setDraft({ ...draft, summary: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.logicLabel}
                <textarea
                  className={cn(inputClass, "min-h-20")}
                  value={draft.logic}
                  onChange={(event) => setDraft({ ...draft, logic: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.riskLevel}
                <select
                  className={inputClass}
                  value={draft.risk}
                  onChange={(event) => setDraft({ ...draft, risk: event.target.value as RiskLevel })}
                >
                  <option value="low">{copy.riskLow}</option>
                  <option value="medium">{copy.riskMedium}</option>
                  <option value="high">{copy.riskHigh}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.suggestedNotionalLabel}
                <input
                  className={inputClass}
                  value={draft.notional}
                  onChange={(event) => setDraft({ ...draft, notional: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.visibilityLabel}
                <select
                  className={inputClass}
                  value={draft.visibility}
                  onChange={(event) =>
                    setDraft({ ...draft, visibility: event.target.value as MarketVisibility })
                  }
                >
                  <option value="invite_only">{copy.visibilityInvite}</option>
                  <option value="public">{copy.visibilityPublic}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.sourceVisibility}
                <select
                  className={inputClass}
                  value={draft.sourceVisibility}
                  onChange={(event) =>
                    setDraft({ ...draft, sourceVisibility: event.target.value as SourceVisibility })
                  }
                >
                  <option value="open_source">{copy.openSource}</option>
                  <option value="black_box">{copy.blackBox}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.pricingLabel}
                <select
                  className={inputClass}
                  value={draft.pricing}
                  onChange={(event) =>
                    setDraft({ ...draft, pricing: event.target.value as PricingModel })
                  }
                >
                  <option value="free">{copy.freePlan}</option>
                  <option value="monthly">{copy.monthlyPlan}</option>
                </select>
              </label>
              <label className="text-[11px] text-muted">
                {copy.priceLabel}
                <input
                  className={inputClass}
                  value={draft.price}
                  disabled={draft.pricing === "free"}
                  onChange={(event) => setDraft({ ...draft, price: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted">
                {copy.trialLabel}
                <input
                  className={inputClass}
                  value={draft.trial}
                  onChange={(event) => setDraft({ ...draft, trial: event.target.value })}
                />
              </label>
              <label className="text-[11px] text-muted sm:col-span-2">
                {copy.knownLimitsLabel}
                <textarea
                  className={cn(inputClass, "min-h-16")}
                  value={draft.limits}
                  onChange={(event) => setDraft({ ...draft, limits: event.target.value })}
                />
              </label>
            </div>
            <Button
              size="sm"
              disabled={
                createPublication.isPending ||
                !activeAuthorId ||
                !draft.title.trim() ||
                !draft.summary.trim() ||
                draft.logic.trim().length < 40
              }
              onClick={() => {
                createPublication.mutate(
                  {
                    strategy_id: record.id,
                    author_id: activeAuthorId,
                    title: draft.title.trim(),
                    summary: draft.summary.trim(),
                    logic_summary: draft.logic.trim(),
                    risk_level: draft.risk,
                    suggested_max_notional_usd: Number(draft.notional) || 0,
                    known_limits: draft.limits
                      .split("\n")
                      .map((line) => line.trim())
                      .filter(Boolean),
                    source_visibility: draft.sourceVisibility,
                    visibility: draft.visibility,
                    pricing: {
                      model: draft.pricing,
                      monthly_price_usd:
                        draft.pricing === "free" ? 0 : Number(draft.price) || 0,
                      trial_days: draft.pricing === "free" ? 0 : Number(draft.trial) || 0,
                    },
                  },
                  {
                    onSuccess: (publication) => {
                      setSelectedId(publication.id);
                      setDraft({ ...draft, title: "", summary: "", logic: "", limits: "" });
                    },
                  },
                );
              }}
            >
              <Plus size={13} />
              {createPublication.isPending ? copy.creating : copy.createDraft}
            </Button>
          </>
        )}

        {own.length > 0 && (
          <div className="space-y-1.5 border-t border-line pt-3">
            <p className="text-[11px] text-muted">{copy.myPublications}</p>
            {own.map((publication) => (
              <button
                key={publication.id}
                type="button"
                onClick={() => setSelectedId(publication.id)}
                className={cn(
                  "w-full rounded-md border px-2 py-1.5 text-left transition-colors",
                  publication.id === selected?.id
                    ? "border-accent/50 bg-accent/5"
                    : "border-line hover:bg-hover",
                )}
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate text-xs font-medium text-fg">
                    {publication.title}
                  </span>
                  <Badge tone={statusTone[publication.status]}>
                    {statusLabel[publication.status]}
                  </Badge>
                </div>
                <p className="text-[11px] text-muted">
                  {fmt(copy.versionTag, { version: publication.strategy_version })} ·{" "}
                  {publication.pricing.model === "free"
                    ? copy.free
                    : fmt(copy.monthly, {
                        price: formatUsd(publication.pricing.monthly_price_usd),
                      })}
                </p>
              </button>
            ))}
          </div>
        )}

        {selected && (
          <div className="flex flex-wrap items-center gap-2 border-t border-line pt-3">
            {selected.status !== "published" ? (
              <Button
                size="sm"
                onClick={() =>
                  updatePublication.mutate({ id: selected.id, body: { action: "publish" } })
                }
                disabled={updatePublication.isPending}
              >
                {copy.publish}
              </Button>
            ) : (
              <Button
                size="sm"
                variant="danger"
                onClick={() =>
                  updatePublication.mutate({ id: selected.id, body: { action: "withdraw" } })
                }
                disabled={updatePublication.isPending}
              >
                {copy.withdraw}
              </Button>
            )}
          </div>
        )}

        {selected && (
          <div className="border-t border-line pt-3">
            <p className="text-xs font-medium text-fg">{copy.performanceTitle}</p>
            <p className="mt-0.5 text-[11px] text-muted">{copy.performanceDesc}</p>
            <div className="mt-2 space-y-1.5">
              {performance.data?.length ? (
                performance.data.map((item) => (
                  <div
                    key={item.id}
                    className="rounded-md border border-line bg-panel-soft px-2 py-1.5 text-[11px]"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-body">
                        {sourceLabel[item.source]} · v{item.strategy_version}
                      </span>
                      <Badge tone={item.reproducible ? "success" : "neutral"}>
                        {item.reproducible ? copy.reproducible : copy.notReproducible}
                      </Badge>
                    </div>
                    <p className="mt-0.5 text-muted">
                      {fmt(copy.periodRange, {
                        start: formatDateTime(item.period_start),
                        end: formatDateTime(item.period_end),
                      })}
                    </p>
                    <p className="mt-0.5 text-muted">
                      {formatPercentValue(item.return_pct)} · {copy.fields.drawdown} -{" "}
                      {formatNumber(item.max_drawdown_pct, 2)}% ·{" "}
                      {fmt(copy.totalTrades, { count: item.trades })}
                    </p>
                  </div>
                ))
              ) : (
                <p className="text-[11px] text-muted">{copy.performanceEmpty}</p>
              )}
            </div>

            <div className="mt-3 grid grid-cols-3 gap-2">
              <label className="col-span-3 text-[11px] text-muted">
                {copy.addPerformance}
                <select
                  className={inputClass}
                  value={perfForm.source}
                  onChange={(event) =>
                    setPerfForm({ ...perfForm, source: event.target.value as PerformanceSource })
                  }
                >
                  <option value="author_backtest">{copy.sourceAuthorBacktest}</option>
                  <option value="platform_replay">{copy.sourcePlatformReplay}</option>
                </select>
              </label>
              {(
                [
                  ["trades", copy.fields.trades],
                  ["sample", copy.fields.sample],
                  ["winRate", copy.fields.winRate],
                  ["returnPct", copy.fields.returnPct],
                  ["drawdown", copy.fields.drawdown],
                  ["netPnl", copy.fields.netPnl],
                ] as const
              ).map(([key, label]) => (
                <label key={key} className="text-[11px] text-muted">
                  {label}
                  <input
                    className={inputClass}
                    value={perfForm[key]}
                    onChange={(event) => setPerfForm({ ...perfForm, [key]: event.target.value })}
                  />
                </label>
              ))}
            </div>
            <Button
              size="sm"
              variant="secondary"
              className="mt-2"
              disabled={createPerformance.isPending}
              onClick={() => {
                const now = new Date();
                const start = new Date(now.getTime() - 30 * 24 * 3_600_000);
                createPerformance.mutate({
                  id: selected.id,
                  body: {
                    source: perfForm.source,
                    period_start: start.toISOString(),
                    period_end: now.toISOString(),
                    trades: Number(perfForm.trades) || 0,
                    win_rate_pct: Number(perfForm.winRate) || 0,
                    net_pnl_usd: Number(perfForm.netPnl) || 0,
                    return_pct: Number(perfForm.returnPct) || 0,
                    max_drawdown_pct: Number(perfForm.drawdown) || 0,
                    fee_bps_per_leg: 4,
                    slippage_bps: 1,
                    sample_size: Number(perfForm.sample) || 0,
                  },
                });
              }}
            >
              {copy.addPerformance}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function EquityCurve({ report }: { report: BacktestReport }) {
  const { t } = useI18n();
  const points = report.equity_curve;
  if (points.length < 2) {
    return <p className="text-xs text-muted">{t.strategies.report.noCurve}</p>;
  }

  const values = points.map((point) => toNumber(point.cumulative_pnl_usd));
  const min = Math.min(...values, 0);
  const max = Math.max(...values, 0);
  const span = max - min || 1;
  const polyline = values
    .map((value, index) => {
      const x = (index / (values.length - 1)) * 100;
      const y = 100 - ((value - min) / span) * 100;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");
  const positive = values[values.length - 1] >= 0;

  return (
    <div className="rounded-lg border border-line bg-panel-soft p-2">
      <svg viewBox="0 0 100 100" preserveAspectRatio="none" className="h-40 w-full">
        <polyline
          points={polyline}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.4}
          vectorEffect="non-scaling-stroke"
          className={positive ? "text-accent" : "text-danger"}
        />
      </svg>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="space-y-0.5">
      <p className="text-[11px] text-muted">{label}</p>
      <p className="font-mono text-xs text-body">{value}</p>
    </div>
  );
}

function SourceBadge({ source }: { source: StrategySource }) {
  const { t } = useI18n();
  const label =
    source === "built_in"
      ? t.strategies.sourceBuiltIn
      : source === "ai"
        ? t.strategies.sourceAi
        : t.strategies.sourceManual;
  return <Badge tone="neutral">{label}</Badge>;
}

const dryRunTone: Record<SandboxRunStatus, "success" | "warning" | "danger"> = {
  ok: "success",
  invalid: "warning",
  error: "danger",
  timeout: "danger",
};

function decisionText(decision: SandboxDecision): string {
  switch (decision.action) {
    case "open":
      return `Open ${decision.symbol} · ${formatUsd(decision.notional_usd)}`;
    case "close":
      return `Close ${decision.symbol}`;
    default:
      return decision.reason ? `Hold · ${decision.reason}` : "Hold";
  }
}

/// 运行情况：跑在哪个交易所组合、托管在哪台执行器、最近事件与试运行结果。
function StrategyRuntimeCard({ record }: { record: StrategyRecord }) {
  const { t } = useI18n();
  const copy = t.strategies;
  const { runtime } = record;
  const runsQuery = useStrategyRuns(record.id);
  const dryRun = useSandboxDryRun();

  const runs = runsQuery.data?.runs ?? [];
  const runEvents = runsQuery.data?.events ?? [];
  // 已入库的运行事件优先；老数据没有实例时回落到策略自身的运行事件。
  const events = runEvents.length
    ? [...runEvents].reverse().map((event) => ({
        at: event.created_at,
        level: event.level,
        message: event.message,
      }))
    : [...runtime.events].reverse();
  const artifactId = record.sandbox?.artifact_id ?? null;
  const report = dryRun.data ?? null;
  const hosted = (record.run_scope ?? "self_hosted") === "platform";

  const dryRunStatusLabel: Record<SandboxRunStatus, string> = {
    ok: copy.dryRunOk,
    invalid: copy.dryRunInvalid,
    error: copy.dryRunError,
    timeout: copy.dryRunTimeout,
  };

  return (
    <Card>
      <CardHeader className="flex-wrap">
        <div>
          <CardTitle>{copy.runtimeTitle}</CardTitle>
          <CardDescription>{copy.runtimeDesc}</CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <Badge tone={hosted ? "info" : "neutral"}>
            {hosted ? copy.runScopePlatform : copy.runScopeSelfHosted}
          </Badge>
          <Badge tone="info">{fmt(copy.signals, { count: runtime.signals })}</Badge>
          <Badge tone="neutral">{fmt(copy.orders, { count: runtime.orders })}</Badge>
        </div>
      </CardHeader>
      <CardContent className="space-y-3 text-xs text-muted">
        <p className="text-[11px] text-subtle">
          {copy.runScope}：{copy.runScopeDesc}
        </p>
        <div className="grid gap-2 md:grid-cols-2">
          <span>
            {fmt(copy.lastEvaluated, {
              value: formatDateTime(runtime.last_evaluated_at),
            })}
          </span>
          <span>
            {runtime.open ? (
              <>
                {copy.openPosition}：{runtime.open.symbol} ·{" "}
                {runtime.open.long_venue && runtime.open.short_venue
                  ? `${runtime.open.long_venue} / ${runtime.open.short_venue}`
                  : runtime.open.venue ?? "—"}{" "}
                · {formatUsd(runtime.open.notional_usd)}
              </>
            ) : (
              copy.noOpenPosition
            )}
          </span>
        </div>
        {runtime.last_error && (
          <p className="text-danger">{fmt(copy.lastError, { message: runtime.last_error })}</p>
        )}

        <div>
          <p className="mb-1 text-[11px] text-muted">{copy.runsTitle}</p>
          {runs.length === 0 ? (
            <p className="text-[11px] text-muted">{copy.runsEmpty}</p>
          ) : (
            <ul className="space-y-1">
              {runs.slice(0, 5).map((run) => (
                <li
                  key={run.id}
                  className="flex flex-wrap items-center gap-2 rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px]"
                >
                  <span className="font-mono text-body">
                    {run.long_venue} / {run.short_venue}
                  </span>
                  <span className="text-muted">v{run.strategy_version}</span>
                  <Badge tone={statusTone[run.status]}>{strategyStatusLabel(run.status)}</Badge>
                  <Badge tone={run.scope === "platform" ? "info" : "neutral"}>
                    {run.scope === "platform" ? copy.runScopePlatform : copy.runScopeSelfHosted}
                  </Badge>
                  <span className="text-muted">
                    {fmt(copy.runStarted, { value: formatDateTime(run.started_at) })}
                  </span>
                  <span className="text-subtle">
                    {run.stopped_at
                      ? fmt(copy.runStopped, { value: formatDateTime(run.stopped_at) })
                      : fmt(copy.runHeartbeat, { value: formatDateTime(run.last_heartbeat_at) })}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div>
          <p className="mb-1 text-[11px] text-muted">{copy.events}</p>
          {events.length === 0 ? (
            <p className="text-[11px] text-muted">{copy.eventsEmpty}</p>
          ) : (
            <ul className="space-y-1">
              {events.slice(0, 20).map((event, index) => (
                <li
                  key={`${event.at}-${index}`}
                  className="flex items-start gap-2 rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px]"
                >
                  <span
                    className={cn("mt-1 h-1.5 w-1.5 shrink-0 rounded-full", eventTone[event.level])}
                  />
                  <span className="font-mono text-muted">{formatDateTime(event.at)}</span>
                  <span className="text-body">{event.message}</span>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="space-y-2 border-t border-line pt-3">
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="secondary"
              disabled={!artifactId || dryRun.isPending}
              onClick={() => artifactId && dryRun.mutate({ artifactId, body: { strategy_id: record.id } })}
            >
              <FlaskConical size={13} />
              {dryRun.isPending ? copy.dryRunning : copy.dryRun}
            </Button>
            <span className="text-[11px] text-subtle">
              {artifactId ? copy.dryRunDesc : copy.dryRunNeedDeploy}
            </span>
          </div>
          {report && (
            <div className="space-y-1 rounded-lg border border-line bg-panel-soft p-2 text-[11px]">
              <div className="flex items-center gap-2">
                <span className="text-muted">{copy.dryRunStatus}</span>
                <Badge tone={dryRunTone[report.status]}>{dryRunStatusLabel[report.status]}</Badge>
                <span className="text-muted">
                  {fmt(copy.dryRunFuel, {
                    used: formatNumber(report.fuel_consumed, 0),
                    limit: formatNumber(report.fuel_limit, 0),
                  })}
                </span>
                <span className="text-muted">
                  {fmt(copy.dryRunDuration, { value: report.duration_ms })}
                </span>
              </div>
              {report.error && <p className="text-danger">{report.error}</p>}
              <p className="font-medium text-fg">{copy.dryRunDecisions}</p>
              {report.decisions.length ? (
                <ul className="space-y-0.5 text-body">
                  {report.decisions.map((decision, index) => (
                    <li key={`${decision.action}-${index}`}>{decisionText(decision)}</li>
                  ))}
                </ul>
              ) : (
                <p className="text-muted">{copy.dryRunEmpty}</p>
              )}
              {report.logs.length > 0 && (
                <>
                  <p className="font-medium text-fg">{copy.dryRunLogs}</p>
                  <ul className="max-h-28 space-y-0.5 overflow-y-auto font-mono text-subtle">
                    {report.logs.map((line, index) => (
                      <li key={`${index}-${line}`}>{line}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

export default function StrategiesPage() {
  const { t } = useI18n();
  const account = useAccount();
  const plans = account.data?.entitlement?.plans ?? [];
  const hasBasePlan = plans.includes("arbitrage");
  /** 策略创建是 +$10/月 附加包；基础版每个计费周期含 3 天免费试用。 */
  const canCreateStrategy = hasBasePlan && plans.includes("strategy");
  const strategies = useStrategies();
  const updateStrategy = useUpdateStrategy();
  const createStrategy = useCreateStrategy();
  const instantiateTemplate = useInstantiateTemplate();
  const templates = useStrategyTemplates();
  const generateCode = useGenerateStrategyCode();
  const validateStrategy = useValidateStrategy();
  const backtestStrategy = useBacktestStrategy();
  const rollbackStrategy = useRollbackStrategy();
  const deleteStrategy = useDeleteStrategy();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [backtestDays, setBacktestDays] = useState(30);
  /** 删除/回滚的二次确认；Tauri 的 macOS WebView 不支持 window.confirm。 */
  const [confirmAction, setConfirmAction] = useState<{
    message: string;
    label: string;
    run: () => void;
  } | null>(null);

  const list = strategies.data ?? [];
  const selected = creating ? null : (list.find((record) => record.id === selectedId) ?? list[0] ?? null);

  const actionError = [
    updateStrategy.error,
    createStrategy.error,
    instantiateTemplate.error,
    generateCode.error,
    validateStrategy.error,
    backtestStrategy.error,
    rollbackStrategy.error,
    deleteStrategy.error,
  ].find(Boolean);

  const startCreate = () => {
    if (!canCreateStrategy) {
      return;
    }
    setCreating(true);
    setSelectedId(null);
  };

  return (
    <div className="space-y-6">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold text-fg">{t.strategies.title}</h1>
          <p className="mt-1 text-sm text-muted">{t.strategies.subtitle}</p>
        </div>
        <Button size="sm" onClick={startCreate} disabled={!canCreateStrategy}>
          <Plus size={13} />
          {t.strategies.create}
        </Button>
      </header>

      {!canCreateStrategy && (
        <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs leading-relaxed text-warning">
          {hasBasePlan
            ? t.strategies.createTrialEnded
            : t.strategies.createRequiresPlan}
        </p>
      )}

      {actionError && (
        <p className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {actionError.message}
        </p>
      )}

      <div className="grid gap-4 lg:grid-cols-[300px_minmax(0,1fr)]">
        <Card className="h-fit">
          <CardHeader>
            <div>
              <CardTitle>{t.strategies.listTitle}</CardTitle>
              <CardDescription>{t.strategies.listDesc}</CardDescription>
            </div>
          </CardHeader>
          <CardContent className="space-y-2 p-2">
            {list.length === 0 && (
              <p className="px-2 py-6 text-center text-xs text-muted">{t.strategies.empty}</p>
            )}
            {list.map((record) => {
              const active = !creating && selected?.id === record.id;
              return (
                <button
                  key={record.id}
                  type="button"
                  onClick={() => {
                    setCreating(false);
                    setSelectedId(record.id);
                  }}
                  className={cn(
                    "w-full space-y-2 rounded-lg border px-3 py-2 text-left transition-colors",
                    active
                      ? "border-accent/40 bg-accent/10"
                      : "border-line bg-panel-soft hover:border-accent/30",
                  )}
                >
                  <div className="flex items-center gap-2">
                    <span className="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-panel text-body">
                      <Bot size={14} />
                    </span>
                    <span className="min-w-0 flex-1 truncate text-xs font-medium text-fg">
                      {record.definition.name}
                    </span>
                  </div>
                  <div className="flex flex-wrap items-center gap-1">
                    <Badge tone={statusTone[record.status]}>
                      {strategyStatusLabel(record.status)}
                    </Badge>
                    <span className="text-[11px] text-muted">
                      {fmt(t.strategies.version, { version: record.version })}
                    </span>
                  </div>
                  <p className="text-[11px] text-muted">
                    {fmt(t.strategies.heartbeat, {
                      value: formatDateTime(record.last_heartbeat_at),
                    })}
                  </p>
                </button>
              );
            })}
          </CardContent>
        </Card>

        <div className="space-y-4">
          {creating && (
            <Card>
              <CardHeader>
                <div>
                  <CardTitle>{t.strategies.createTitle}</CardTitle>
                  <CardDescription>{t.strategies.createDesc}</CardDescription>
                </div>
              </CardHeader>
              <CardContent>
                <CreateStrategyForm
                  key="new"
                  pending={createStrategy.isPending}
                  onCreate={(name, prompt) =>
                    createStrategy.mutate(
                      { name, source: prompt ? "ai" : "manual" },
                      {
                        onSuccess: (record) => {
                          setCreating(false);
                          setSelectedId(record.id);
                          if (prompt) {
                            generateCode.mutate({ id: record.id, prompt });
                          }
                        },
                      },
                    )
                  }
                />
                {(templates.data ?? []).length > 0 && (
                  <div className="mt-4 space-y-2 border-t border-line pt-4">
                    <div>
                      <p className="text-xs font-medium text-fg">{t.strategies.templatesTitle}</p>
                      <p className="mt-0.5 text-[11px] text-muted">{t.strategies.templatesDesc}</p>
                    </div>
                    <div className="grid gap-2 sm:grid-cols-2">
                      {(templates.data ?? []).map((template) => (
                        <button
                          key={template.key}
                          type="button"
                          disabled={instantiateTemplate.isPending}
                          onClick={() =>
                            instantiateTemplate.mutate(
                              { key: template.key },
                              {
                                onSuccess: (record) => {
                                  setCreating(false);
                                  setSelectedId(record.id);
                                },
                              },
                            )
                          }
                          className="rounded-lg border border-line bg-panel-soft px-3 py-2 text-left transition-colors hover:border-accent/30 disabled:opacity-60"
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-xs font-medium text-fg">{template.name}</span>
                            <span className="shrink-0 text-[11px] text-accent">
                              {instantiateTemplate.isPending
                                ? t.strategies.instantiating
                                : t.strategies.useTemplate}
                            </span>
                          </div>
                          <p className="mt-1 text-[11px] text-muted">{template.description}</p>
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {!creating && !selected && (
            <Card>
              <CardContent className="py-16 text-center text-sm text-muted">
                {t.strategies.empty}
              </CardContent>
            </Card>
          )}

          {!creating && selected && (
            <StrategyDetail
              key={selected.id}
              record={selected}
              backtestDays={backtestDays}
              onBacktestDays={setBacktestDays}
              onToggle={() =>
                updateStrategy.mutate({
                  id: selected.id,
                  status: selected.status === "running" ? "paused" : "running",
                })
              }
              onValidate={() => validateStrategy.mutate(selected.id)}
              onBacktest={() =>
                backtestStrategy.mutate({ id: selected.id, days: backtestDays })
              }
              onRollback={(version) => {
                setConfirmAction({
                  message: t.strategies.rollbackConfirm,
                  label: t.strategies.rollback,
                  run: () => rollbackStrategy.mutate({ id: selected.id, version }),
                });
              }}
              onDelete={() => {
                setConfirmAction({
                  message: t.strategies.deleteConfirm,
                  label: t.strategies.delete,
                  run: () =>
                    deleteStrategy.mutate(selected.id, {
                      onSuccess: () => setSelectedId(null),
                    }),
                });
              }}
              pending={{
                toggle: updateStrategy.isPending,
                validate: validateStrategy.isPending,
                backtest: backtestStrategy.isPending,
                rollback: rollbackStrategy.isPending,
                remove: deleteStrategy.isPending,
              }}
            />
          )}
        </div>
      </div>

      <ConfirmDialog
        open={confirmAction !== null}
        message={confirmAction?.message ?? ""}
        confirmLabel={confirmAction?.label ?? ""}
        cancelLabel={t.strategies.cancel}
        onCancel={() => setConfirmAction(null)}
        onConfirm={() => {
          const action = confirmAction;
          setConfirmAction(null);
          action?.run();
        }}
      />
    </div>
  );
}

function StrategyDetail({
  record,
  backtestDays,
  onBacktestDays,
  onToggle,
  onValidate,
  onBacktest,
  onRollback,
  onDelete,
  pending,
}: {
  record: StrategyRecord;
  backtestDays: number;
  onBacktestDays: (days: number) => void;
  onToggle: () => void;
  onValidate: () => void;
  onBacktest: () => void;
  onRollback: (version: number) => void;
  onDelete: () => void;
  pending: {
    toggle: boolean;
    validate: boolean;
    backtest: boolean;
    rollback: boolean;
    remove: boolean;
  };
}) {
  const { t } = useI18n();
  const { definition, validation, last_backtest: report } = record;
  const running = record.status === "running";
  const errors = validation.issues.filter((issue) => issue.severity === "error");
  const warnings = validation.issues.filter((issue) => issue.severity === "warning");
  const winRate = report && report.trades > 0 ? (report.wins / report.trades) * 100 : null;

  // 版本历史默认收起，只有用户点击后才发起请求。
  const [versionsOpen, setVersionsOpen] = useState(false);
  const versions = useStrategyVersions(versionsOpen ? record.id : null);

  // 与后端 publish_readiness 保持一致：校验通过且已部署沙箱代码即可发布；
  // 回测只作为参考信息，事件驱动策略没有历史公告可回放。
  const publishable = errors.length === 0 && !!record.sandbox;
  // 发布不再改变运行位置，改为看是否已有对外发布记录；保留卡片便于撤回或更新。
  const publications = usePublications(true);
  const published = (publications.data ?? []).some(
    (item) => item.strategy_id === record.id && item.status !== "private",
  );

  return (
    <>
      <Card>
        <CardHeader className="flex-wrap">
          <div className="space-y-1">
            <CardTitle>{definition.name}</CardTitle>
            <div className="flex flex-wrap items-center gap-1">
              <Badge tone={statusTone[record.status]}>{strategyStatusLabel(record.status)}</Badge>
              <SourceBadge source={record.source} />
              <span className="text-[11px] text-muted">
                {fmt(t.strategies.version, { version: record.version })}
              </span>
            </div>
            <CardDescription>
              {definition.universe.venues.join(" / ")} · {definition.universe.symbols.join(" / ")}
            </CardDescription>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" variant={running ? "secondary" : "primary"} disabled={pending.toggle} onClick={onToggle}>
              {running ? <Pause size={13} /> : <Play size={13} />}
              {running ? t.strategies.pause : t.strategies.start}
            </Button>
            <Button size="sm" variant="secondary" disabled={pending.validate} onClick={onValidate}>
              <ShieldCheck size={13} />
              {pending.validate ? t.strategies.validating : t.strategies.validate}
            </Button>
            <Button size="sm" variant="secondary" disabled={pending.backtest} onClick={onBacktest}>
              <FlaskConical size={13} />
              {pending.backtest ? t.strategies.backtesting : t.strategies.backtest}
            </Button>
            <Button size="sm" variant="danger" disabled={pending.remove} onClick={onDelete}>
              <Trash2 size={13} />
              {t.strategies.delete}
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <div className="mb-1.5 flex items-center gap-2">
              <p className="text-xs font-medium text-fg">{t.strategies.validationTitle}</p>
              {errors.length > 0 ? (
                <Badge tone="danger">
                  {fmt(t.strategies.validationErrors, { count: errors.length })}
                </Badge>
              ) : (
                <Badge tone="success">{t.strategies.validationPass}</Badge>
              )}
              {warnings.length > 0 && (
                <Badge tone="warning">
                  {fmt(t.strategies.validationWarnings, { count: warnings.length })}
                </Badge>
              )}
              <span className="text-[11px] text-muted">
                {fmt(t.strategies.validatedAt, {
                  value: formatDateTime(validation.validated_at),
                })}
              </span>
            </div>
            <p className="mb-2 text-[11px] text-muted">{t.strategies.validationDesc}</p>
            {validation.issues.length > 0 && (
              <ul className="space-y-1">
                {validation.issues.map((issue, index) => (
                  <li
                    key={`${issue.path}-${index}`}
                    className="flex items-start gap-2 rounded-md border border-line bg-panel-soft px-2 py-1 text-[11px]"
                  >
                    <Badge tone={issue.severity === "error" ? "danger" : "warning"}>
                      {issue.severity}
                    </Badge>
                    <span className="font-mono text-muted">{issue.path}</span>
                    <span className="text-body">{issue.message}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </CardContent>
      </Card>

      <StrategyCodeCard record={record} running={running} />

      <StrategyScopeCard record={record} running={running} />

      {publishable || published ? (
        <StrategyPublishCard record={record} />
      ) : (
        <Card>
          <CardHeader>
            <div className="flex items-center gap-2">
              <ShieldCheck size={15} className="text-muted" />
              <div>
                <CardTitle>{t.strategies.publishLockedTitle}</CardTitle>
                <CardDescription>{t.strategies.publishLockedDesc}</CardDescription>
              </div>
            </div>
          </CardHeader>
        </Card>
      )}


      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.strategies.backtestTitle}</CardTitle>
            <CardDescription>{t.strategies.backtestDesc}</CardDescription>
          </div>
          <label className="flex items-center gap-2 text-[11px] text-muted">
            {t.strategies.backtestDays}
            <input
              type="number"
              min={1}
              max={365}
              value={backtestDays}
              onChange={(event) => onBacktestDays(Number(event.target.value) || 30)}
              className={cn(inputClass, "w-20")}
            />
          </label>
        </CardHeader>
        <CardContent className="space-y-3">
          {!report ? (
            <p className="text-xs text-muted">{t.strategies.backtestEmpty}</p>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-3 md:grid-cols-4 xl:grid-cols-6">
                <Metric
                  label={t.strategies.report.window}
                  value={`${formatDateTime(report.sample_start)} → ${formatDateTime(report.sample_end)}`}
                />
                <Metric
                  label={t.strategies.report.settlements}
                  value={formatNumber(report.settlements, 0)}
                />
                <Metric label={t.strategies.report.trades} value={formatNumber(report.trades, 0)} />
                <Metric
                  label={t.strategies.report.winRate}
                  value={winRate === null ? "—" : `${winRate.toFixed(0)}%`}
                />
                <Metric
                  label={t.strategies.report.grossFunding}
                  value={formatUsd(report.gross_funding_usd)}
                />
                <Metric label={t.strategies.report.fees} value={formatUsd(report.fees_usd)} />
                <Metric label={t.strategies.report.slippage} value={formatUsd(report.slippage_usd)} />
                <Metric label={t.strategies.report.netPnl} value={formatUsd(report.net_pnl_usd)} />
                <Metric
                  label={t.strategies.report.return}
                  value={formatPercentValue(report.return_pct)}
                />
                <Metric
                  label={t.strategies.report.drawdown}
                  value={formatPercentValue(report.max_drawdown_pct)}
                />
                <Metric
                  label={t.strategies.report.inSample}
                  value={formatUsd(report.in_sample_net_pnl_usd)}
                />
                <Metric
                  label={t.strategies.report.outOfSample}
                  value={formatUsd(report.out_of_sample_net_pnl_usd)}
                />
              </div>
              <div className="space-y-1">
                <p className="text-[11px] text-muted">{t.strategies.report.assumptions}</p>
                <div className="flex flex-wrap gap-1">
                  {report.assumptions.map((assumption) => (
                    <Badge key={assumption} tone="neutral">
                      {t.strategies.assumptions[assumption]}
                    </Badge>
                  ))}
                </div>
              </div>
              <div className="space-y-1">
                <p className="text-[11px] text-muted">{t.strategies.report.curve}</p>
                <EquityCurve report={report} />
              </div>
              <p className="text-[11px] text-warning">{t.strategies.report.overfitting}</p>
            </>
          )}
        </CardContent>
      </Card>

      <StrategyRuntimeCard record={record} />

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.strategies.versionsTitle}</CardTitle>
            <CardDescription>{t.strategies.versionsDesc}</CardDescription>
          </div>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => setVersionsOpen((open) => !open)}
          >
            <History size={14} />
            {versionsOpen ? t.strategies.versionsHide : t.strategies.versionsShow}
          </Button>
        </CardHeader>
        {versionsOpen && (
          <CardContent className="space-y-2">
            {versions.isLoading ? (
              <p className="text-xs text-muted">{t.strategies.versionsLoading}</p>
            ) : !versions.data?.length ? (
              <p className="text-xs text-muted">{t.strategies.versionsEmpty}</p>
            ) : (
              versions.data.map((version) => {
                const current = version.version === record.version;
                return (
                  <div
                    key={version.version}
                    className="flex flex-wrap items-center gap-2 rounded-md border border-line bg-panel-soft px-2 py-1.5 text-[11px]"
                  >
                    <span className="font-medium text-fg">
                      {fmt(t.strategies.version, { version: version.version })}
                    </span>
                    <Badge tone={statusTone[version.status]}>
                      {strategyStatusLabel(version.status)}
                    </Badge>
                    {version.backtest && (
                      <Badge tone="neutral">
                        {formatUsd(version.backtest.net_pnl_usd)}
                      </Badge>
                    )}
                    <span className="text-muted">{formatDateTime(version.saved_at)}</span>
                    <span className="ml-auto flex items-center gap-2">
                      {current ? (
                        <Badge tone="success">{t.strategies.current}</Badge>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={pending.rollback || running}
                          onClick={() => onRollback(version.version)}
                        >
                          {t.strategies.rollback}
                        </Button>
                      )}
                    </span>
                  </div>
                );
              })
            )}
          </CardContent>
        )}
      </Card>
    </>
  );
}
