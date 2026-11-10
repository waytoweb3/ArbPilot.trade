import { useEffect, useState } from "react";
import {
  Bot,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Code2,
  FlaskConical,
  History,
  Pause,
  Pencil,
  Play,
  Plus,
  Rocket,
  Save,
  ShieldCheck,
  Sparkles,
  Square,
  Store,
  Trash2,
  X,
} from "lucide-react";

import { PositionsOrdersPanel } from "../components/PositionsOrdersPanel";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { ConfirmDialog } from "../components/ui/confirm-dialog";
import { useAccount } from "../hooks/useAccount";
import {
  useBacktestStrategy,
  useClearStrategyRuns,
  useCompileStrategyCode,
  useCreatePerformance,
  useCreatePublication,
  useCreateStrategy,
  useDeleteStrategy,
  useDeployStrategyCode,
  useGenerateStrategyCode,
  useInstantiateTemplate,
  useListingSettings,
  useMarketAuthors,
  useMarketConfig,
  useOrders,
  usePositions,
  usePublicationPerformance,
  usePublications,
  useRollbackStrategy,
  useSaveStrategyCode,
  useSkillSettings,
  useSkillVersions,
  useStrategies,
  useStrategyCode,
  useStrategyRuns,
  useStrategySkillDiff,
  useStrategyTemplates,
  useStrategyVersions,
  useUpdateListingSettings,
  useUpdateSkillSettings,
  useUpdatePublication,
  useUpdateStrategy,
  useUpdateStrategyExecutionMode,
  useUpdateStrategyScope,
  useUpgradeStrategySkill,
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
  GridShift,
  GridSpacingMode,
  MaEntry,
  MaExit,
  MarketVisibility,
  PerformanceSource,
  PricingModel,
  PublicationStatus,
  RiskLevel,
  SkillAction,
  SkillDirection,
  SkillKind,
  SkillOrderType,
  SkillRule,
  SkillSettings,
  SourceVisibility,
  StrategyExecutionMode,
  StrategyRecord,
  StrategyStatus,
} from "../lib/types";
import { cn } from "../lib/utils";

const statusTone: Record<StrategyStatus, "success" | "warning" | "neutral" | "danger"> = {
  running: "success",
  paused: "warning",
  draft: "neutral",
  stopped: "danger",
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

/** 内置模板标识 → 技能触发方式，与后端 `skill::kind_from_template` 保持一致。 */
const SKILL_KINDS_BY_TEMPLATE_KEY: Record<string, SkillKind> = {
  "turtle-breakout": "breakout",
  "ma-trend": "ma_trend",
  grid: "grid",
  "listing-follow": "event",
  skill: "event",
};

/** 旧记录的模板名称 → 技能触发方式，仅用于升级前未回填模板标识的数据。 */
const SKILL_KINDS_BY_NAME: Array<[string, SkillKind]> = [
  ["海龟突破", "breakout"],
  ["均线趋势", "ma_trend"],
  ["网格", "grid"],
  ["上币公告跟随", "event"],
  ["技能策略（SKILL.md）", "event"],
];

/** 指标类技能可选的 K 线周期，与后端行情支持的周期保持一致。 */
const SKILL_TIMEFRAMES = ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"];

/** 去掉模板实例名上的附加信息：重名自动编号与自定义后缀，与后端 `skill::base_name` 一致。 */
function skillBaseName(name: string): string {
  const head = name.trim().split(" · ")[0].trim();
  return head.replace(/ \d+$/, "");
}

/** 识别策略属于哪种技能触发方式；非内置技能策略返回 null。 */
function skillKind(record: StrategyRecord): SkillKind | null {
  const key = record.skill?.template_key;
  if (key) {
    const mapped = SKILL_KINDS_BY_TEMPLATE_KEY[key];
    if (mapped) {
      return mapped;
    }
  }
  // 技能库版本用通用 `skill` 标识交付，触发方式以冻结规则为准。
  const frozen = record.skill?.settings as SkillSettings | null | undefined;
  const inferred = frozen?.rules?.[0]?.kind;
  if (inferred) {
    return inferred;
  }
  // 兼容未回填模板标识的历史记录。
  const name = skillBaseName(record.definition.name);
  const matched = SKILL_KINDS_BY_NAME.find(([base]) => name === base);
  return matched?.[1] ?? null;
}

/**
 * 上币公告跟随由内置模板驱动：代码、回测报告与版本历史不对外展示，
 * 应用范围改为公告监听、交易所、金额与止盈止损设置。
 */
function isListingFollowStrategy(record: StrategyRecord): boolean {
  if (record.skill?.template_key) {
    return record.skill.template_key === "listing-follow";
  }
  return skillBaseName(record.definition.name) === "上币公告跟随";
}

/**
 * 技能策略把 SKILL.md 的六章内容转成参数：四个内置策略（海龟突破、均线趋势、
 * 网格、上币公告跟随）与技能策略模板都由 Rust 技能运行时执行，代码、回测报告与
 * 版本历史不对外展示，改为六章参数卡片。
 */
function isSkillStrategy(record: StrategyRecord): boolean {
  return skillKind(record) !== null;
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

function StrategyScopeCard({
  record,
  running,
  onToggle,
  pendingToggle,
}: {
  record: StrategyRecord;
  running: boolean;
  /** 启动/暂停统一放在应用范围卡片里，顶部不再单独放按钮。 */
  onToggle: () => void;
  pendingToggle: boolean;
}) {
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
        <Button
          size="sm"
          variant={running ? "secondary" : "primary"}
          disabled={pendingToggle}
          onClick={onToggle}
        >
          {running ? <Pause size={13} /> : <Play size={13} />}
          {running ? t.strategies.pause : t.strategies.start}
        </Button>
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

/** 新闻来源/交易所 ID 的展示名。 */
function venueLabel(venue: string): string {
  const labels: Record<string, string> = {
    binance: "Binance",
    okx: "OKX",
    bybit: "Bybit",
    bitget: "Bitget",
    gate: "Gate",
    upbit: "Upbit",
    aster: "Aster",
    hyperliquid: "Hyperliquid",
    lighter: "Lighter",
  };
  return labels[venue] ?? venue;
}

/** 编辑中的一条规则；`key` 同时作为规则 ID 持久化。 */
interface ListingRuleDraft {
  key: string;
  name: string;
  enabled: boolean;
  executionMode: StrategyExecutionMode;
  watchVenues: string[];
  executionVenues: string[];
  notional: string;
  leverage: string;
  takeProfit: string;
  stopLoss: string;
}

/** 新增规则时的默认值；杠杆沿用上一条规则，避免出现空值。 */
function emptyRuleDraft(leverage: string): ListingRuleDraft {
  return {
    key: crypto.randomUUID(),
    name: "",
    enabled: true,
    executionMode: "real",
    watchVenues: [],
    executionVenues: [],
    notional: "100",
    leverage,
    takeProfit: "20",
    stopLoss: "10",
  };
}

function StrategyListingScopeCard({
  strategyId,
  status,
}: {
  strategyId: string;
  /** 策略整体状态：规则能否触发公告取决于它。 */
  status: StrategyStatus;
}) {
  const { t } = useI18n();
  const settingsQuery = useListingSettings(strategyId);
  const saveSettings = useUpdateListingSettings();
  const updateStrategy = useUpdateStrategy();
  // 正在启停的规则，避免连点。
  const [pendingRule, setPendingRule] = useState<string | null>(null);

  const current = settingsQuery.data?.settings;
  const watchOptions = settingsQuery.data?.available_watch_venues ?? [];
  const executionOptions = settingsQuery.data?.available_execution_venues ?? [];

  const [rules, setRules] = useState<ListingRuleDraft[]>([]);
  const [notice, setNotice] = useState<{ tone: "ok" | "error"; text: string } | null>(null);
  // 已保存的规则默认折叠且只读；点「编辑」才开始改，改完在这条规则上单独保存。
  const [editing, setEditing] = useState<Record<string, boolean>>({});
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  useEffect(() => {
    if (!current) {
      return;
    }
    setRules(
      current.rules.map((rule) => ({
        key: rule.id ?? crypto.randomUUID(),
        name: rule.name,
        enabled: rule.enabled ?? true,
        executionMode: rule.execution_mode ?? "real",
        watchVenues: rule.watch_venues,
        executionVenues: rule.execution_venues,
        notional: String(rule.notional_usd),
        leverage: String(rule.leverage),
        takeProfit: String(rule.take_profit_pct),
        stopLoss: String(rule.stop_loss_pct),
      })),
    );
  }, [current]);

  const patchRule = (key: string, changes: Partial<ListingRuleDraft>) => {
    setRules((previous) =>
      previous.map((rule) => (rule.key === key ? { ...rule, ...changes } : rule)),
    );
  };

  const addRule = () => {
    const draft = emptyRuleDraft(rules[rules.length - 1]?.leverage ?? "");
    setRules((previous) => [...previous, draft]);
    // 新规则直接展开并进入编辑，填完保存即生效。
    setEditing((previous) => ({ ...previous, [draft.key]: true }));
    setCollapsed((previous) => ({ ...previous, [draft.key]: false }));
  };

  const startEdit = (key: string) => {
    setEditing((previous) => ({ ...previous, [key]: true }));
    setCollapsed((previous) => ({ ...previous, [key]: false }));
  };

  /** 取消编辑：已有规则恢复成已保存的值，尚未保存的新规则直接丢弃。 */
  const cancelEdit = (key: string) => {
    const saved = current?.rules.find((rule) => rule.id === key);
    setRules((previous) => {
      if (!saved) {
        return previous.length <= 1 ? previous : previous.filter((rule) => rule.key !== key);
      }
      return previous.map((rule) =>
        rule.key === key
          ? {
              ...rule,
              name: saved.name,
              enabled: saved.enabled ?? true,
              executionMode: saved.execution_mode ?? "real",
              watchVenues: saved.watch_venues,
              executionVenues: saved.execution_venues,
              notional: String(saved.notional_usd),
              leverage: String(saved.leverage),
              takeProfit: String(saved.take_profit_pct),
              stopLoss: String(saved.stop_loss_pct),
            }
          : rule,
      );
    });
    setEditing((previous) => ({ ...previous, [key]: false }));
    setCollapsed((previous) => ({ ...previous, [key]: true }));
  };

  const removeRule = (key: string) => {
    setRules((previous) =>
      previous.length <= 1 ? previous : previous.filter((rule) => rule.key !== key),
    );
  };

  const toggleWatch = (key: string, venue: string) => {
    setRules((previous) =>
      previous.map((rule) => {
        if (rule.key !== key) {
          return rule;
        }
        return {
          ...rule,
          watchVenues: rule.watchVenues.includes(venue)
            ? rule.watchVenues.filter((item) => item !== venue)
            : [...rule.watchVenues, venue],
        };
      }),
    );
  };

  const addExecution = (key: string, venue: string) => {
    setRules((previous) =>
      previous.map((rule) =>
        rule.key !== key || rule.executionVenues.includes(venue)
          ? rule
          : { ...rule, executionVenues: [...rule.executionVenues, venue] },
      ),
    );
  };

  const removeExecution = (key: string, venue: string) => {
    setRules((previous) =>
      previous.map((rule) =>
        rule.key !== key
          ? rule
          : { ...rule, executionVenues: rule.executionVenues.filter((item) => item !== venue) },
      ),
    );
  };

  const moveExecution = (key: string, index: number, delta: number) => {
    setRules((previous) =>
      previous.map((rule) => {
        if (rule.key !== key) {
          return rule;
        }
        const target = index + delta;
        if (target < 0 || target >= rule.executionVenues.length) {
          return rule;
        }
        const next = [...rule.executionVenues];
        [next[index], next[target]] = [next[target], next[index]];
        return { ...rule, executionVenues: next };
      }),
    );
  };

  /** 组装提交体；参数不合法时给出提示并返回 null。 */
  const buildPayload = (source: ListingRuleDraft[] = rules) => {
    if (!source.length) {
      setNotice({ tone: "error", text: t.strategies.listingRuleNeedOne });
      return null;
    }
    const payload = [];
    for (const rule of source) {
      const amount = Number(rule.notional);
      const lev = Number(rule.leverage);
      const tp = Number(rule.takeProfit);
      const sl = Number(rule.stopLoss);
      // 关掉的规则只保留配置，允许参数暂时不合法。
      if (
        rule.enabled &&
        (!Number.isFinite(amount) ||
          amount <= 0 ||
          !Number.isFinite(lev) ||
          lev <= 0 ||
          !Number.isFinite(tp) ||
          tp < 0 ||
          !Number.isFinite(sl) ||
          sl < 0)
      ) {
        setNotice({ tone: "error", text: t.strategies.listingInvalid });
        return null;
      }
      payload.push({
        id: rule.key,
        name: rule.name.trim(),
        enabled: rule.enabled,
        execution_mode: rule.executionMode,
        notional_usd: amount,
        leverage: lev,
        watch_venues: rule.watchVenues,
        execution_venues: rule.executionVenues,
        take_profit_pct: tp,
        stop_loss_pct: sl,
      });
    }
    return payload;
  };

  /** 单条规则保存：保存成功后收起回到只读，其它规则保持原样。 */
  const saveRule = (key: string) => {
    const payload = buildPayload();
    if (!payload) {
      return;
    }
    setNotice(null);
    saveSettings.mutate(
      { strategy_id: strategyId, rules: payload },
      {
        onSuccess: () => {
          setEditing((previous) => ({ ...previous, [key]: false }));
          setCollapsed((previous) => ({ ...previous, [key]: true }));
          setNotice({ tone: "ok", text: t.strategies.listingSaved });
        },
        onError: (error) => setNotice({ tone: "error", text: errorText(error) }),
      },
    );
  };

  /** 单条规则启停：启动会连带把策略切成运行中，停掉最后一条启用规则时把策略暂停。 */
  const toggleRuleRunning = async (rule: ListingRuleDraft, start: boolean) => {
    const next = rules.map((item) =>
      item.key === rule.key ? { ...item, enabled: start } : item,
    );
    const payload = buildPayload(next);
    if (!payload) {
      return;
    }
    const previous = rules;
    setNotice(null);
    setPendingRule(rule.key);
    setRules(next);
    try {
      await saveSettings.mutateAsync({ strategy_id: strategyId, rules: payload });
      if (start) {
        if (status !== "running") {
          await updateStrategy.mutateAsync({ id: strategyId, status: "running" });
        }
      } else if (status === "running" && !next.some((item) => item.enabled)) {
        await updateStrategy.mutateAsync({ id: strategyId, status: "paused" });
      }
    } catch (error) {
      setRules(previous);
      setNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPendingRule(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.strategies.scopeTitle}</CardTitle>
          <CardDescription>{t.strategies.listingScopeDesc}</CardDescription>
        </div>
        <Button size="sm" variant="secondary" onClick={addRule}>
          <Plus size={13} />
          {t.strategies.listingRuleAdd}
        </Button>
      </CardHeader>
      <CardContent className="space-y-3">
        <p className="text-[11px] text-muted">{t.strategies.listingRunningHint}</p>
        <p className="text-[11px] text-muted">{t.strategies.listingRulesHint}</p>
        <div className="flex flex-wrap gap-1">
          <Badge tone="neutral">
            {t.strategies.listingNews} · {t.strategies.listingAuto}
          </Badge>
          <Badge tone="neutral">
            {t.strategies.listingToken} · {t.strategies.listingAuto}
          </Badge>
          <Badge tone="info">{t.strategies.listingSingleLegValue}</Badge>
        </div>

        {settingsQuery.error && (
          <p className="text-[11px] text-danger">{errorText(settingsQuery.error)}</p>
        )}

        {rules.map((rule, index) => {
          const editingRule = editing[rule.key] === true;
          const collapsedRule = !editingRule && collapsed[rule.key] !== false;
          // 规则真正在跑 = 自身启用 且 策略整体在运行。
          const ruleRunning = rule.enabled && status === "running";
          const title =
            rule.name.trim() || fmt(t.strategies.listingRuleTitle, { index: index + 1 });
          const watch = rule.watchVenues.length
            ? rule.watchVenues.map(venueLabel).join(" / ")
            : t.strategies.listingAllSources;
          const execution = rule.executionVenues.length
            ? rule.executionVenues.map(venueLabel).join(" / ")
            : t.strategies.listingAllVenues;
          return (
          <div
            key={rule.key}
            className={cn(
              "rounded-lg border border-line bg-panel-soft/40",
              !rule.enabled && "opacity-60",
            )}
          >
            <div className="flex items-center gap-2 p-2.5">
              <button
                type="button"
                title={
                  collapsedRule
                    ? t.strategies.listingRuleExpand
                    : t.strategies.listingRuleCollapse
                }
                onClick={() =>
                  setCollapsed((previous) => ({ ...previous, [rule.key]: !collapsedRule }))
                }
                className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
              >
                <ChevronDown
                  size={13}
                  className={cn(
                    "shrink-0 text-muted transition-transform",
                    collapsedRule && "-rotate-90",
                  )}
                />
                <span className="shrink-0 text-[11px] font-medium text-main">{title}</span>
                {collapsedRule && (
                  <span className="truncate text-[11px] text-muted">
                    {watch} → {execution} ·{" "}
                    {rule.executionMode === "real"
                      ? t.strategies.executionModeReal
                      : t.strategies.executionModePaper}{" "}
                    · {rule.notional} USD · {rule.leverage}x · TP {rule.takeProfit}% · SL{" "}
                    {rule.stopLoss}%
                  </span>
                )}
              </button>
              <button
                type="button"
                disabled={pendingRule === rule.key}
                onClick={() => toggleRuleRunning(rule, !ruleRunning)}
                className={cn(
                  "flex shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] transition-colors disabled:opacity-50",
                  ruleRunning
                    ? "border-danger/40 bg-danger/10 text-danger"
                    : "border-accent/40 bg-accent/10 text-accent",
                )}
              >
                {ruleRunning ? <Square size={10} /> : <Play size={10} />}
                {ruleRunning
                  ? t.strategies.listingRuleStop
                  : t.strategies.listingRuleStart}
              </button>
              {!editingRule && (
                <button
                  type="button"
                  title={t.strategies.listingRuleEdit}
                  onClick={() => startEdit(rule.key)}
                  className="flex shrink-0 items-center gap-1 text-[11px] text-muted hover:text-accent"
                >
                  <Pencil size={12} />
                  {t.strategies.listingRuleEdit}
                </button>
              )}
              <button
                type="button"
                title={t.strategies.listingRuleRemove}
                disabled={rules.length <= 1}
                onClick={() => removeRule(rule.key)}
                className="text-muted disabled:opacity-30"
              >
                <Trash2 size={13} />
              </button>
            </div>

            {!collapsedRule && (
            <div className="space-y-2 border-t border-line p-3">
            <label className="block space-y-1">
              <span className="text-[11px] text-muted">{t.strategies.listingRuleName}</span>
              <input
                className={inputClass}
                placeholder={t.strategies.listingRuleNamePlaceholder}
                value={rule.name}
                disabled={!editingRule}
                onChange={(event) => patchRule(rule.key, { name: event.target.value })}
              />
            </label>

            <div className="grid gap-2 sm:grid-cols-4">
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.listingNotional}</span>
                <input
                  className={inputClass}
                  type="number"
                  min={0}
                  step="any"
                  value={rule.notional}
                  disabled={!editingRule}
                  onChange={(event) => patchRule(rule.key, { notional: event.target.value })}
                />
              </label>
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.listingLeverage}</span>
                <input
                  className={inputClass}
                  type="number"
                  min={0}
                  step="any"
                  value={rule.leverage}
                  disabled={!editingRule}
                  onChange={(event) => patchRule(rule.key, { leverage: event.target.value })}
                />
              </label>
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.listingTakeProfit}</span>
                <input
                  className={inputClass}
                  type="number"
                  min={0}
                  step="any"
                  value={rule.takeProfit}
                  disabled={!editingRule}
                  onChange={(event) => patchRule(rule.key, { takeProfit: event.target.value })}
                />
              </label>
              <label className="block space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.listingStopLoss}</span>
                <input
                  className={inputClass}
                  type="number"
                  min={0}
                  step="any"
                  value={rule.stopLoss}
                  disabled={!editingRule}
                  onChange={(event) => patchRule(rule.key, { stopLoss: event.target.value })}
                />
              </label>
            </div>

            <div className="space-y-1">
              <span className="text-[11px] text-muted">{t.strategies.listingWatchVenues}</span>
              <div className="flex flex-wrap gap-1.5">
                {watchOptions.map((venue) => {
                  const active = rule.watchVenues.includes(venue);
                  return (
                    <button
                      key={venue}
                      type="button"
                      disabled={!editingRule}
                      onClick={() => toggleWatch(rule.key, venue)}
                      className={cn(
                        "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                        active
                          ? "border-accent/50 bg-accent/15 text-accent"
                          : "border-line bg-panel-soft text-muted hover:border-accent/30",
                      )}
                    >
                      {venueLabel(venue)}
                    </button>
                  );
                })}
              </div>
              <p className="text-[11px] text-muted">{t.strategies.listingWatchHint}</p>
            </div>

            <div className="space-y-1">
              <span className="text-[11px] text-muted">{t.strategies.listingExecutionVenues}</span>
              {executionOptions.length === 0 ? (
                <p className="rounded-md border border-warning/30 bg-warning/10 px-2 py-1 text-[11px] text-warning">
                  {t.strategies.listingVenuesEmpty}
                </p>
              ) : (
                <>
                  {rule.executionVenues.length > 0 && (
                    <ol className="space-y-1">
                      {rule.executionVenues.map((venue, venueIndex) => (
                        <li
                          key={venue}
                          className="flex items-center justify-between rounded-md border border-line bg-panel-soft px-2 py-1"
                        >
                          <span className="text-[11px] text-main">
                            {venueIndex + 1}. {venueLabel(venue)}
                          </span>
                          <span className="flex items-center gap-1 text-muted">
                            <button
                              type="button"
                              title={t.strategies.listingMoveUp}
                              disabled={!editingRule || venueIndex === 0}
                              onClick={() => moveExecution(rule.key, venueIndex, -1)}
                              className="disabled:opacity-30"
                            >
                              <ChevronUp size={12} />
                            </button>
                            <button
                              type="button"
                              title={t.strategies.listingMoveDown}
                              disabled={
                                !editingRule || venueIndex === rule.executionVenues.length - 1
                              }
                              onClick={() => moveExecution(rule.key, venueIndex, 1)}
                              className="disabled:opacity-30"
                            >
                              <ChevronDown size={12} />
                            </button>
                            <button
                              type="button"
                              title={t.strategies.listingRemove}
                              disabled={!editingRule}
                              onClick={() => removeExecution(rule.key, venue)}
                            >
                              <X size={12} />
                            </button>
                          </span>
                        </li>
                      ))}
                    </ol>
                  )}
                  <div className="flex flex-wrap gap-1.5">
                    {executionOptions
                      .filter((venue) => !rule.executionVenues.includes(venue))
                      .map((venue) => (
                        <button
                          key={venue}
                          type="button"
                          disabled={!editingRule}
                          onClick={() => addExecution(rule.key, venue)}
                          className="rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-muted transition-colors hover:border-accent/30 disabled:opacity-40"
                        >
                          + {venueLabel(venue)}
                        </button>
                      ))}
                  </div>
                </>
              )}
              <p className="text-[11px] text-muted">{t.strategies.listingExecutionHint}</p>

              <div className="space-y-1">
                <span className="text-[11px] text-muted">{t.strategies.executionMode}</span>
                <div className="flex flex-wrap gap-1.5">
                  {(["real", "paper"] as const).map((mode) => {
                    const active = rule.executionMode === mode;
                    return (
                      <button
                        key={mode}
                        type="button"
                        disabled={!editingRule}
                        onClick={() => patchRule(rule.key, { executionMode: mode })}
                        className={cn(
                          "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                          active
                            ? "border-accent/50 bg-accent/15 text-accent"
                            : "border-line bg-panel-soft text-muted hover:border-accent/30",
                        )}
                      >
                        {mode === "real"
                          ? t.strategies.executionModeReal
                          : t.strategies.executionModePaper}
                      </button>
                    );
                  })}
                </div>
                <p className="text-[11px] text-muted">{t.strategies.listingExecutionModeHint}</p>
              </div>

              {editingRule && (
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    size="sm"
                    disabled={saveSettings.isPending}
                    onClick={() => saveRule(rule.key)}
                  >
                    <Save size={13} />
                    {saveSettings.isPending
                      ? t.strategies.listingSaving
                      : t.strategies.listingSave}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => cancelEdit(rule.key)}>
                    {t.strategies.listingRuleCancel}
                  </Button>
                </div>
              )}
            </div>
            </div>
            )}
          </div>
          );
        })}
        <p className="text-[11px] text-muted">{t.strategies.listingPctHint}</p>

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

/** 编辑中的一条技能规则；`key` 同时作为规则 ID 持久化。 */
interface SkillRuleDraft {
  key: string;
  name: string;
  enabled: boolean;
  /** 技能用途：这条技能负责识别什么、做什么。 */
  purpose: string;
  /** 触发方式：公告、通道突破、均线趋势或网格。 */
  kind: SkillKind;
  eventSources: string[];
  /** 触发关键词，用逗号或空格分隔。 */
  keywords: string;
  eventWindow: string;
  timeframe: string;
  lookback: string;
  fast: string;
  slow: string;
  /** 参与计算的均线周期，逗号分隔。 */
  maPeriods: string;
  /** 入场条件，只引用上面的均线周期。 */
  entries: MaEntry[];
  /** 出场条件，任一满足即离场。 */
  exits: MaExit[];
  atr: string;
  channel: string;
  atrMultiple: string;
  /** 海龟系统一/系统二开关与通道周期。 */
  system1Enabled: boolean;
  entry1: string;
  exit1: string;
  system2Enabled: boolean;
  entry2: string;
  exit2: string;
  profitFilter: boolean;
  /** 海龟 Unit 仓位与金字塔加仓。 */
  maxUnits: string;
  pyramidStepN: string;
  maxAdds: string;
  maxSymbols: string;
  maxLeverage: string;
  maxHoldHours: string;
  orderType: SkillOrderType;
  direction: SkillDirection;
  executionMode: StrategyExecutionMode;
  executionVenues: string[];
  notional: string;
  leverage: string;
  riskPct: string;
  riskReward: string;
  maxPositions: string;
  cooldownHours: string;
  takeProfit: string;
  stopLoss: string;
  /** 旧版单边档位数，仅用于迁移。 */
  gridLevels: string;
  gridSpacing: string;
  /** 旧版每档占比，仅用于迁移。 */
  gridShare: string;
  gridCount: string;
  gridLower: string;
  gridUpper: string;
  gridMode: GridSpacingMode;
  gridTrigger: string;
  gridShift: GridShift;
  gridCloseOnFail: boolean;
  action: SkillAction;
  notify: boolean;
  maxNotional: string;
  maxDailyOpens: string;
}

/** 按触发方式给出指标与网格默认值，和改造前的内置模板保持一致。 */
function skillKindDefaults(kind: SkillKind): Partial<SkillRuleDraft> {
  switch (kind) {
    case "breakout":
      return {
        timeframe: "1h",
        lookback: "300",
        fast: "20",
        slow: "55",
        atr: "20",
        channel: "20",
        atrMultiple: "2",
        system1Enabled: true,
        entry1: "20",
        exit1: "10",
        system2Enabled: true,
        entry2: "55",
        exit2: "20",
        profitFilter: true,
        maxUnits: "4",
        pyramidStepN: "0.5",
        maxAdds: "3",
        maxSymbols: "3",
        maxLeverage: "3",
        maxHoldHours: "0",
        orderType: "market",
        riskPct: "0.5",
        cooldownHours: "24",
        takeProfit: "0",
        stopLoss: "0",
      };
    case "ma_trend":
      return {
        timeframe: "1h",
        lookback: "300",
        fast: "5",
        slow: "20",
        maPeriods: "5,10,20,50",
        entries: [
          { type: "alignment", periods: [5, 10, 20, 50] },
          { type: "price_above", ma: 20 },
        ],
        exits: [{ type: "break_ma", ma: 20 }, { type: "atr_stop" }],
        atr: "14",
        atrMultiple: "2",
        riskPct: "1",
        riskReward: "2",
        maxPositions: "3",
        cooldownHours: "4",
        takeProfit: "0",
        stopLoss: "0",
      };
    case "grid":
      return {
        timeframe: "5m",
        lookback: "200",
        direction: "both",
        takeProfit: "0",
        stopLoss: "0",
        gridSpacing: "0.5",
        gridCount: "10",
        gridLower: "0",
        gridUpper: "0",
        gridMode: "arithmetic",
        gridTrigger: "0",
        gridShift: "none",
        gridCloseOnFail: false,
      };
    default:
      return {};
  }
}

/** 新增技能规则时的默认值；杠杆沿用上一条规则，避免出现空值。 */
function emptySkillRuleDraft(leverage: string, kind: SkillKind): SkillRuleDraft {
  return {
    key: crypto.randomUUID(),
    name: "",
    enabled: true,
    purpose: "",
    kind,
    eventSources: [],
    keywords: "",
    eventWindow: "3600",
    timeframe: "1h",
    lookback: "200",
    fast: "20",
    slow: "55",
    maPeriods: "",
    entries: [],
    exits: [],
    atr: "20",
    channel: "20",
    atrMultiple: "2",
    system1Enabled: true,
    entry1: "20",
    exit1: "10",
    system2Enabled: true,
    entry2: "55",
    exit2: "20",
    profitFilter: true,
    maxUnits: "4",
    pyramidStepN: "0.5",
    maxAdds: "3",
    maxSymbols: "3",
    maxLeverage: "3",
    maxHoldHours: "0",
    orderType: "market",
    direction: "long",
    executionMode: "real",
    executionVenues: [],
    notional: "100",
    leverage,
    riskPct: "0",
    riskReward: "0",
    maxPositions: "0",
    cooldownHours: "0",
    takeProfit: "20",
    stopLoss: "10",
    gridLevels: "3",
    gridSpacing: "0.5",
    gridShare: "25",
    gridCount: "10",
    gridLower: "0",
    gridUpper: "0",
    gridMode: "arithmetic",
    gridTrigger: "0",
    gridShift: "none",
    gridCloseOnFail: false,
    action: "open_long",
    notify: true,
    maxNotional: "0",
    maxDailyOpens: "0",
    ...skillKindDefaults(kind),
  };
}

/** 已保存的技能规则转成编辑草稿。 */
function skillDraftFrom(rule: SkillRule): SkillRuleDraft {
  const base = emptySkillRuleDraft(String(rule.business.leverage), rule.kind ?? "event");
  // 旧配置没有 grid_count，用单边档位数 × 2 迁移成网格数量。
  const legacyCount = (rule.business.grid_levels ?? 3) * 2;
  const gridCount =
    (rule.business.grid_count ?? 0) >= 2 ? rule.business.grid_count : legacyCount;
  return {
    ...base,
    key: rule.id ?? crypto.randomUUID(),
    name: rule.name,
    enabled: rule.enabled ?? true,
    purpose: rule.purpose,
    kind: rule.kind ?? "event",
    eventSources: rule.trigger.event_sources,
    keywords: rule.trigger.event_keywords.join(", "),
    eventWindow: String(rule.trigger.event_window_secs),
    timeframe: rule.trigger.timeframe ?? "1h",
    lookback: String(rule.trigger.lookback ?? 200),
    fast: String(rule.trigger.fast ?? 20),
    slow: String(rule.trigger.slow ?? 55),
    // 旧配置没有均线集合时，用快慢均线回填，避免界面出现空列表。
    maPeriods: (
      (rule.trigger.ma_periods ?? []).length > 0
        ? rule.trigger.ma_periods
        : [rule.trigger.fast ?? 20, rule.trigger.slow ?? 55]
    ).join(","),
    entries: rule.trigger.entries ?? [],
    exits: rule.trigger.exits ?? [],
    atr: String(rule.trigger.atr ?? 20),
    channel: String(rule.trigger.channel ?? 20),
    atrMultiple: String(rule.trigger.atr_multiple ?? 2),
    system1Enabled: rule.trigger.turtle?.system1_enabled ?? true,
    entry1: String(rule.trigger.turtle?.entry1 ?? rule.trigger.channel ?? 20),
    exit1: String(rule.trigger.turtle?.exit1 ?? 10),
    system2Enabled: rule.trigger.turtle?.system2_enabled ?? true,
    entry2: String(rule.trigger.turtle?.entry2 ?? 55),
    exit2: String(rule.trigger.turtle?.exit2 ?? 20),
    profitFilter: rule.trigger.turtle?.profit_filter ?? true,
    maxUnits: String(rule.business.max_units ?? 4),
    pyramidStepN: String(rule.business.pyramid_step_n ?? 0.5),
    maxAdds: String(rule.business.max_pyramid_adds ?? 3),
    maxSymbols: String(rule.business.max_symbols ?? 3),
    maxLeverage: String(rule.business.max_leverage ?? rule.business.leverage ?? 3),
    maxHoldHours: String(rule.business.max_hold_hours ?? 0),
    orderType: rule.steps.order_type ?? "market",
    direction: rule.steps.direction ?? "both",
    executionMode: rule.steps.execution_mode ?? "real",
    executionVenues: rule.steps.execution_venues,
    notional: String(rule.business.notional_usd),
    leverage: String(rule.business.leverage),
    riskPct: String(rule.cautions.risk_pct ?? 0),
    riskReward: String(rule.business.risk_reward ?? 0),
    maxPositions: String(rule.cautions.max_positions ?? 0),
    cooldownHours: String(rule.cautions.cooldown_hours ?? 0),
    takeProfit: String(rule.business.take_profit_pct),
    stopLoss: String(rule.business.stop_loss_pct),
    gridLevels: String(rule.business.grid_levels ?? 3),
    gridSpacing: String(rule.business.grid_spacing_pct ?? 0.5),
    gridShare: String(rule.business.grid_share_pct ?? 25),
    gridCount: String(gridCount),
    gridLower: String(rule.business.grid_lower_price ?? 0),
    gridUpper: String(rule.business.grid_upper_price ?? 0),
    gridMode: rule.business.grid_spacing_mode ?? "arithmetic",
    gridTrigger: String(rule.business.grid_trigger_price ?? 0),
    gridShift: rule.business.grid_shift ?? "none",
    gridCloseOnFail: rule.business.grid_close_on_fail ?? false,
    action: rule.output.action,
    notify: rule.output.notify,
    maxNotional: String(rule.cautions.max_notional_usd),
    maxDailyOpens: String(rule.cautions.max_daily_opens),
  };
}

/** 关键词：支持中英文逗号、分号与换行分隔，保留 `will list` 这类多词短语。 */
function parseKeywords(value: string): string[] {
  return value
    .split(/[,，;；\n]+/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

/** 均线周期：支持逗号、分号与空格分隔，去重后升序。 */
function parsePeriods(value: string): number[] {
  const periods = value
    .split(/[,，;；\s]+/)
    .map((item) => Number(item))
    .filter((item) => Number.isInteger(item) && item >= 2 && item <= 1_000);
  return Array.from(new Set(periods)).sort((left, right) => left - right);
}

/** 入场/出场规则引用到的均线周期，用于提示「改规则前不要删均线」。 */
function referencedPeriods(rule: SkillRuleDraft): Set<number> {
  const used = new Set<number>();
  for (const entry of rule.entries) {
    if (entry.type === "alignment") {
      entry.periods.forEach((period) => used.add(period));
    } else {
      used.add(entry.ma);
    }
  }
  for (const exit of rule.exits) {
    if (exit.type === "cross") {
      used.add(exit.fast);
      used.add(exit.slow);
    } else if (exit.type === "break_ma") {
      used.add(exit.ma);
    }
  }
  return used;
}

/**
 * 技能策略参数卡片：把 SKILL.md 的六章内容（技能用途、什么时候调用、
 * 执行步骤、业务规则、输出格式、注意事项）逐条转成可配置参数。
 */
function StrategySkillScopeCard({
  strategyId,
  status,
  kind,
  purpose,
  skillId,
  boundVersion,
}: {
  strategyId: string;
  /** 策略整体状态：规则能否触发事件取决于它。 */
  status: StrategyStatus;
  /** 该内置策略的触发方式，决定「什么时候调用」展示哪些参数。 */
  kind: SkillKind;
  /** 这条技能的用途：取模板名（如「上币公告跟随」），而不是写死的说明。 */
  purpose: string;
  /** 绑定的技能库版本；存在时展示手动升级入口。 */
  skillId?: string | null;
  boundVersion?: number | null;
}) {
  const { t } = useI18n();
  const settingsQuery = useSkillSettings(strategyId);
  const saveSettings = useUpdateSkillSettings();
  const updateStrategy = useUpdateStrategy();
  const [pendingRule, setPendingRule] = useState<string | null>(null);

  const current = settingsQuery.data?.settings;
  const sourceOptions = settingsQuery.data?.available_event_sources ?? [];
  const executionOptions = settingsQuery.data?.available_execution_venues ?? [];

  const [rules, setRules] = useState<SkillRuleDraft[]>([]);
  const [notice, setNotice] = useState<{ tone: "ok" | "error"; text: string } | null>(null);
  const [editing, setEditing] = useState<Record<string, boolean>>({});
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [targetVersion, setTargetVersion] = useState(boundVersion ? boundVersion + 1 : 1);
  const targetDiff = useStrategySkillDiff(strategyId, skillId ? targetVersion : 0);
  const upgradeSkill = useUpgradeStrategySkill();

  useEffect(() => {
    setTargetVersion(boundVersion ? boundVersion + 1 : 1);
  }, [boundVersion]);

  useEffect(() => {
    if (!current) {
      return;
    }
    setRules(current.rules.map(skillDraftFrom));
  }, [current]);

  const patchRule = (key: string, changes: Partial<SkillRuleDraft>) => {
    setRules((previous) =>
      previous.map((rule) => (rule.key === key ? { ...rule, ...changes } : rule)),
    );
  };

  /** 「添加均线」输入框的临时值。 */
  const [maInput, setMaInput] = useState<Record<string, string>>({});

  /** 添加一条均线周期；唯一性由 parsePeriods 保证。 */
  const addMaPeriod = (key: string, current: string) => {
    const value = Number(maInput[key]);
    if (!Number.isInteger(value) || value < 2 || value > 1_000) {
      return;
    }
    const next = parsePeriods(`${current},${value}`);
    patchRule(key, { maPeriods: next.join(",") });
    setMaInput((previous) => ({ ...previous, [key]: "" }));
  };

  /** 增删改均线入场/出场条件；`next` 为空表示删除该条。 */
  const updateRuleItem = <T,>(
    key: string,
    field: "entries" | "exits",
    index: number,
    next: T | null,
  ) => {
    const rule = rules.find((item) => item.key === key);
    if (!rule) {
      return;
    }
    const list = [...rule[field]] as unknown as T[];
    if (next === null) {
      list.splice(index, 1);
    } else {
      list[index] = next;
    }
    patchRule(key, { [field]: list } as Partial<SkillRuleDraft>);
  };

  /** 输出格式文案：真实开仓或模拟开仓。 */
  const outputLabel = (rule: SkillRuleDraft) =>
    rule.executionMode === "real"
      ? t.strategies.skillOutputReal
      : t.strategies.skillOutputPaper;

  const addRule = () => {
    const draft = emptySkillRuleDraft(rules[rules.length - 1]?.leverage ?? "", kind);
    // 公告类规则的初始关键词由后端给出，用户可以在此基础上增删。
    if (draft.kind === "event") {
      draft.keywords = (settingsQuery.data?.default_event_keywords ?? []).join(", ");
    }
    setRules((previous) => [...previous, draft]);
    setEditing((previous) => ({ ...previous, [draft.key]: true }));
    setCollapsed((previous) => ({ ...previous, [draft.key]: false }));
  };

  const startEdit = (key: string) => {
    setEditing((previous) => ({ ...previous, [key]: true }));
    setCollapsed((previous) => ({ ...previous, [key]: false }));
  };

  /** 取消编辑：已有规则恢复成已保存的值，尚未保存的新规则直接丢弃。 */
  const cancelEdit = (key: string) => {
    const saved = current?.rules.find((rule) => rule.id === key);
    setRules((previous) => {
      if (!saved) {
        return previous.length <= 1 ? previous : previous.filter((rule) => rule.key !== key);
      }
      return previous.map((rule) =>
        rule.key === key ? { ...skillDraftFrom(saved), key } : rule,
      );
    });
    setEditing((previous) => ({ ...previous, [key]: false }));
    setCollapsed((previous) => ({ ...previous, [key]: true }));
  };

  const removeRule = (key: string) => {
    setRules((previous) =>
      previous.length <= 1 ? previous : previous.filter((rule) => rule.key !== key),
    );
  };

  const toggleSource = (key: string, source: string) => {
    setRules((previous) =>
      previous.map((rule) =>
        rule.key !== key
          ? rule
          : {
              ...rule,
              eventSources: rule.eventSources.includes(source)
                ? rule.eventSources.filter((item) => item !== source)
                : [...rule.eventSources, source],
            },
      ),
    );
  };

  const addExecution = (key: string, venue: string) => {
    setRules((previous) =>
      previous.map((rule) =>
        rule.key !== key || rule.executionVenues.includes(venue)
          ? rule
          : { ...rule, executionVenues: [...rule.executionVenues, venue] },
      ),
    );
  };

  const removeExecution = (key: string, venue: string) => {
    setRules((previous) =>
      previous.map((rule) =>
        rule.key !== key
          ? rule
          : { ...rule, executionVenues: rule.executionVenues.filter((item) => item !== venue) },
      ),
    );
  };

  const moveExecution = (key: string, index: number, delta: number) => {
    setRules((previous) =>
      previous.map((rule) => {
        if (rule.key !== key) {
          return rule;
        }
        const target = index + delta;
        if (target < 0 || target >= rule.executionVenues.length) {
          return rule;
        }
        const next = [...rule.executionVenues];
        [next[index], next[target]] = [next[target], next[index]];
        return { ...rule, executionVenues: next };
      }),
    );
  };

  /** 组装提交体；参数不合法时给出提示并返回 null。 */
  const buildPayload = (source: SkillRuleDraft[] = rules) => {
    if (!source.length) {
      setNotice({ tone: "error", text: t.strategies.skillRuleNeedOne });
      return null;
    }
    const payload: SkillRule[] = [];
    for (const rule of source) {
      const window = Number(rule.eventWindow);
      const amount = Number(rule.notional);
      const lev = Number(rule.leverage);
      const tp = Number(rule.takeProfit);
      const sl = Number(rule.stopLoss);
      const maxNotional = Number(rule.maxNotional);
      const maxDaily = Number(rule.maxDailyOpens);
      const lookback = Number(rule.lookback);
      const fast = Number(rule.fast);
      const slow = Number(rule.slow);
      const atr = Number(rule.atr);
      const channel = Number(rule.channel);
      const atrMultiple = Number(rule.atrMultiple);
      const gridSpacing = Number(rule.gridSpacing);
      const gridCount = Number(rule.gridCount);
      const gridLower = Number(rule.gridLower);
      const gridUpper = Number(rule.gridUpper);
      const gridTrigger = Number(rule.gridTrigger);
      const riskPct = Number(rule.riskPct);
      const riskReward = Number(rule.riskReward);
      const maxPositions = Number(rule.maxPositions);
      const cooldownHours = Number(rule.cooldownHours);
      const entry1 = Number(rule.entry1);
      const exit1 = Number(rule.exit1);
      const entry2 = Number(rule.entry2);
      const exit2 = Number(rule.exit2);
      const maxUnits = Number(rule.maxUnits);
      const pyramidStepN = Number(rule.pyramidStepN);
      const maxAdds = Number(rule.maxAdds);
      const maxSymbols = Number(rule.maxSymbols);
      const maxLeverage = Number(rule.maxLeverage);
      const maxHoldHours = Number(rule.maxHoldHours);
      const turtlePeriod = (value: number) =>
        Number.isInteger(value) && value >= 2 && value <= 1000;
      const turtleValid =
        rule.kind !== "breakout" ||
        ((!rule.system1Enabled || (turtlePeriod(entry1) && turtlePeriod(exit1) && entry1 > exit1)) &&
          (!rule.system2Enabled || (turtlePeriod(entry2) && turtlePeriod(exit2) && entry2 > exit2)) &&
          Number.isInteger(maxUnits) &&
          maxUnits >= 1 &&
          maxUnits <= 20 &&
          Number.isInteger(maxAdds) &&
          maxAdds >= 0 &&
          maxAdds < maxUnits &&
          Number.isFinite(pyramidStepN) &&
          pyramidStepN > 0 &&
          Number.isInteger(maxSymbols) &&
          maxSymbols >= 0 &&
          Number.isFinite(maxLeverage) &&
          maxLeverage > 0 &&
          Number.isFinite(maxHoldHours) &&
          maxHoldHours >= 0);
      const maPeriods = parsePeriods(rule.maPeriods);
      const longestMa = maPeriods.length > 0 ? Math.max(...maPeriods) : 0;
      // 规则只能引用指标层声明过的均线，避免出现「引用了不存在的 MA」。
      const entriesValid = rule.entries.every((entry) =>
        entry.type === "alignment"
          ? entry.periods.length >= 2 && entry.periods.every((period) => maPeriods.includes(period))
          : maPeriods.includes(entry.ma),
      );
      const exitsValid = rule.exits.every((exit) => {
        if (exit.type === "atr_stop") {
          return true;
        }
        if (exit.type === "break_ma") {
          return maPeriods.includes(exit.ma);
        }
        return exit.fast !== exit.slow && maPeriods.includes(exit.fast) && maPeriods.includes(exit.slow);
      });
      const maValid =
        rule.kind !== "ma_trend" ||
        (maPeriods.length >= 2 && longestMa <= lookback && entriesValid && exitsValid);
      const gridRangeSet = gridLower > 0 || gridUpper > 0;
      const gridRangeValid =
        !gridRangeSet || (gridLower > 0 && gridUpper > gridLower);
      // 自动区间时，间距 × 档位数的一半不能把最远买档推到 0 以下。
      const gridAutoValid = gridRangeSet || gridSpacing * gridCount / 2 < 100;
      // 关掉的规则只保留配置，允许参数暂时不合法。
      if (
        rule.enabled &&
        (!Number.isFinite(window) ||
          window < 60 ||
          window > 604800 ||
          !Number.isFinite(amount) ||
          amount <= 0 ||
          !Number.isFinite(lev) ||
          lev <= 0 ||
          !Number.isFinite(riskPct) ||
          riskPct < 0 ||
          riskPct > 100 ||
          !Number.isFinite(riskReward) ||
          riskReward < 0 ||
          !Number.isInteger(maxPositions) ||
          maxPositions < 0 ||
          !Number.isFinite(cooldownHours) ||
          cooldownHours < 0 ||
          !Number.isFinite(tp) ||
          tp < 0 ||
          !Number.isFinite(sl) ||
          sl < 0 ||
          !maValid ||
          !turtleValid ||
          !Number.isFinite(maxNotional) ||
          maxNotional < 0 ||
          !Number.isFinite(maxDaily) ||
          maxDaily < 0 ||
          !Number.isFinite(lookback) ||
          lookback < 3 ||
          !Number.isFinite(fast) ||
          fast < 2 ||
          !Number.isFinite(slow) ||
          slow < 2 ||
          !Number.isFinite(atr) ||
          atr < 2 ||
          !Number.isFinite(channel) ||
          channel < 2 ||
          !Number.isFinite(atrMultiple) ||
          atrMultiple < 0 ||
          !Number.isFinite(gridSpacing) ||
          gridSpacing <= 0 ||
          gridSpacing >= 100 ||
          !Number.isFinite(gridCount) ||
          !Number.isInteger(gridCount) ||
          gridCount < 2 ||
          gridCount > 1000 ||
          !Number.isFinite(gridLower) ||
          gridLower < 0 ||
          !Number.isFinite(gridUpper) ||
          gridUpper < 0 ||
          !gridRangeValid ||
          !gridAutoValid ||
          !Number.isFinite(gridTrigger) ||
          gridTrigger < 0)
      ) {
        setNotice({ tone: "error", text: t.strategies.skillInvalid });
        return null;
      }
      payload.push({
        id: rule.key,
        name: rule.name.trim(),
        enabled: rule.enabled,
        purpose: rule.purpose.trim(),
        kind: rule.kind,
        trigger: {
          event_sources: rule.eventSources,
          event_keywords: parseKeywords(rule.keywords),
          event_window_secs: window,
          timeframe: rule.timeframe,
          lookback,
          fast,
          slow,
          ma_periods: maPeriods,
          entries: rule.entries,
          exits: rule.exits,
          atr,
          channel,
          atr_multiple: atrMultiple,
          turtle: {
            system1_enabled: rule.system1Enabled,
            system2_enabled: rule.system2Enabled,
            entry1,
            exit1,
            entry2,
            exit2,
            profit_filter: rule.profitFilter,
          },
        },
        steps: {
          execution_mode: rule.executionMode,
          execution_venues: rule.executionVenues,
          direction: rule.direction,
          order_type: rule.orderType,
        },
        business: {
          notional_usd: amount,
          leverage: lev,
          risk_reward: riskReward,
          take_profit_pct: tp,
          stop_loss_pct: sl,
          grid_levels: Math.ceil(gridCount / 2),
          grid_spacing_pct: gridSpacing,
          grid_share_pct: 100 / gridCount,
          grid_count: gridCount,
          grid_lower_price: gridLower,
          grid_upper_price: gridUpper,
          grid_spacing_mode: rule.gridMode,
          grid_trigger_price: gridTrigger,
          grid_shift: rule.gridShift,
          grid_close_on_fail: rule.gridCloseOnFail,
          max_units: maxUnits,
          pyramid_step_n: pyramidStepN,
          max_pyramid_adds: maxAdds,
          max_symbols: maxSymbols,
          max_leverage: maxLeverage,
          max_hold_hours: maxHoldHours,
        },
        output: { action: rule.action, notify: rule.notify },
        cautions: {
          max_notional_usd: maxNotional,
          max_daily_opens: maxDaily,
          risk_pct: riskPct,
          max_positions: maxPositions,
          cooldown_hours: cooldownHours,
        },
      });
    }
    return payload;
  };

  const saveRule = (key: string) => {
    const payload = buildPayload();
    if (!payload) {
      return;
    }
    setNotice(null);
    saveSettings.mutate(
      { strategy_id: strategyId, rules: payload },
      {
        onSuccess: () => {
          setEditing((previous) => ({ ...previous, [key]: false }));
          setCollapsed((previous) => ({ ...previous, [key]: true }));
        },
        onError: (error) => setNotice({ tone: "error", text: errorText(error) }),
      },
    );
  };

  /** 单条规则启停：启动会连带把策略切成运行中，停掉最后一条启用规则时把策略暂停。 */
  const toggleRuleRunning = async (rule: SkillRuleDraft, start: boolean) => {
    const next = rules.map((item) =>
      item.key === rule.key ? { ...item, enabled: start } : item,
    );
    const payload = buildPayload(next);
    if (!payload) {
      return;
    }
    const previous = rules;
    setNotice(null);
    setPendingRule(rule.key);
    setRules(next);
    try {
      await saveSettings.mutateAsync({ strategy_id: strategyId, rules: payload });
      if (start) {
        if (status !== "running") {
          await updateStrategy.mutateAsync({ id: strategyId, status: "running" });
        }
      } else if (status === "running" && !next.some((item) => item.enabled)) {
        await updateStrategy.mutateAsync({ id: strategyId, status: "paused" });
      }
    } catch (error) {
      setRules(previous);
      setNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPendingRule(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.strategies.scopeTitle}</CardTitle>
          <CardDescription>{purpose || t.strategies.skillScopeDesc}</CardDescription>
        </div>
        <Button size="sm" variant="secondary" onClick={addRule}>
          <Plus size={13} />
          {t.strategies.skillRuleAdd}
        </Button>
      </CardHeader>
      <CardContent className="space-y-3">
        {settingsQuery.error && (
          <p className="text-[11px] text-danger">{errorText(settingsQuery.error)}</p>
        )}

        {skillId ? (
          <div className="space-y-2 rounded-lg border border-line bg-panel-soft/40 p-3">
            <div className="flex items-center gap-2">
              <span className="text-[11px] font-medium text-body">
                {t.skillsLibrary.upgradeTitle}
              </span>
              <Badge tone="info">
                {t.skillsLibrary.boundVersion} v{boundVersion ?? "—"}
              </Badge>
            </div>
            <p className="text-[10px] text-muted">{t.skillsLibrary.upgradeDesc}</p>
            <div className="flex flex-wrap items-center gap-2">
              <label className="flex items-center gap-1 text-[11px] text-muted">
                {t.skillsLibrary.targetVersion}
                <input
                  type="number"
                  min={1}
                  value={targetVersion}
                  onChange={(event) => setTargetVersion(Math.max(1, Number(event.target.value)))}
                  className="h-7 w-16 rounded border border-line bg-panel px-1 font-mono text-[11px]"
                />
              </label>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => targetDiff.refetch()}
                disabled={targetDiff.isFetching}
              >
                <History size={12} />
                {t.skillsLibrary.preview}
              </Button>
              <Button
                size="sm"
                onClick={async () => {
                  try {
                    await upgradeSkill.mutateAsync({ strategyId, version: targetVersion });
                    await settingsQuery.refetch();
                    setNotice({ tone: "ok", text: t.skillsLibrary.validated });
                  } catch (error) {
                    setNotice({ tone: "error", text: errorText(error) });
                  }
                }}
                disabled={upgradeSkill.isPending}
              >
                {t.skillsLibrary.upgrade}
              </Button>
            </div>
            {targetDiff.data && (
              <p className="text-[10px] text-muted">
                {t.skillsLibrary.added} {targetDiff.data.added.length} ·{" "}
                {t.skillsLibrary.removed} {targetDiff.data.removed.length} ·{" "}
                {t.skillsLibrary.changed} {targetDiff.data.changed.length}
              </p>
            )}
            {targetDiff.error && (
              <p className="text-[10px] text-danger">{errorText(targetDiff.error)}</p>
            )}
          </div>
        ) : null}

        {rules.map((rule, index) => {
          const editingRule = editing[rule.key] === true;
          const collapsedRule = !editingRule && collapsed[rule.key] !== false;
          const ruleRunning = rule.enabled && status === "running";
          const title =
            rule.name.trim() || fmt(t.strategies.skillRuleTitle, { index: index + 1 });
          const sources = rule.eventSources.length
            ? rule.eventSources.map(venueLabel).join(" / ")
            : t.strategies.skillAllSources;
          const execution = rule.executionVenues.length
            ? rule.executionVenues.map(venueLabel).join(" / ")
            : t.strategies.listingAllVenues;
          // 均线周期与规则引用：规则只能引用已声明的均线。
          const maList = parsePeriods(rule.maPeriods);
          const usedPeriods = referencedPeriods(rule);
          // 折叠时按触发方式给出摘要：公告看来源，指标看周期与参数，网格看档位。
          const condition =
            rule.kind === "event"
              ? sources
              : rule.kind === "grid"
                ? `${rule.timeframe} · ${rule.gridLevels} 档 · ${rule.gridSpacing}%`
                : rule.kind === "ma_trend"
                  ? `${rule.timeframe} · ${maList.map((period) => `MA${period}`).join("/")}`
                  : `${rule.timeframe} · 系统一 ${rule.entry1}/${rule.exit1} · 系统二 ${rule.entry2}/${rule.exit2}`;
          const target =
            rule.kind === "grid"
              ? `${rule.notional} USD × ${rule.gridShare}%`
              : rule.kind === "breakout"
                ? `${rule.maxUnits} Units · ${rule.maxLeverage}x`
                : `${rule.notional} USD · ${rule.leverage}x`;
          // 来源版本来自已保存规则；本地新增或编辑中的草稿不显示版本标记。
          const sourceVersion = current?.rules.find((item) => item.id === rule.key)
            ?.source_skill_version;
          return (
            <div
              key={rule.key}
              className={cn(
                "rounded-lg border border-line bg-panel-soft/40",
                !rule.enabled && "opacity-60",
              )}
            >
              <div className="flex items-center gap-2 p-2.5">
                <button
                  type="button"
                  title={
                    collapsedRule
                      ? t.strategies.listingRuleExpand
                      : t.strategies.listingRuleCollapse
                  }
                  onClick={() =>
                    setCollapsed((previous) => ({ ...previous, [rule.key]: !collapsedRule }))
                  }
                  className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                >
                  <ChevronDown
                    size={13}
                    className={cn(
                      "shrink-0 text-muted transition-transform",
                      collapsedRule && "-rotate-90",
                    )}
                  />
                  <span className="shrink-0 text-[11px] font-medium text-main">{title}</span>
                  {sourceVersion ? (
                    <Badge tone="info">v{sourceVersion}</Badge>
                  ) : null}
                  {collapsedRule && (
                    <span className="truncate text-[11px] text-muted">
                      {condition} → {execution} · {outputLabel(rule)} · {target}
                      {rule.kind === "grid"
                        ? ""
                        : ` · TP ${rule.takeProfit}% · SL ${rule.stopLoss}%`}
                    </span>
                  )}
                </button>
                <button
                  type="button"
                  disabled={pendingRule === rule.key}
                  onClick={() => toggleRuleRunning(rule, !ruleRunning)}
                  className={cn(
                    "flex shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] transition-colors disabled:opacity-50",
                    ruleRunning
                      ? "border-danger/40 bg-danger/10 text-danger"
                      : "border-accent/40 bg-accent/10 text-accent",
                  )}
                >
                  {ruleRunning ? <Square size={10} /> : <Play size={10} />}
                  {ruleRunning
                    ? t.strategies.listingRuleStop
                    : t.strategies.listingRuleStart}
                </button>
                {!editingRule && (
                  <button
                    type="button"
                    title={t.strategies.listingRuleEdit}
                    onClick={() => startEdit(rule.key)}
                    className="flex shrink-0 items-center gap-1 text-[11px] text-muted hover:text-accent"
                  >
                    <Pencil size={12} />
                    {t.strategies.listingRuleEdit}
                  </button>
                )}
                <button
                  type="button"
                  title={t.strategies.listingRuleRemove}
                  disabled={rules.length <= 1}
                  onClick={() => removeRule(rule.key)}
                  className="text-muted disabled:opacity-30"
                >
                  <Trash2 size={13} />
                </button>
              </div>

              {!collapsedRule && (
                <div className="space-y-3 border-t border-line p-3">
                  <label className="block space-y-1">
                    <span className="text-[11px] text-muted">{t.strategies.listingRuleName}</span>
                    <input
                      className={inputClass}
                      placeholder={t.strategies.skillRuleNamePlaceholder}
                      value={rule.name}
                      disabled={!editingRule}
                      onChange={(event) => patchRule(rule.key, { name: event.target.value })}
                    />
                  </label>

                  {/* 1. 什么时候调用 */}
                  <div className="space-y-2">
                    <span className="text-[11px] font-medium text-main">
                      {t.strategies.skillTriggerTitle}
                    </span>
                    {rule.kind === "event" ? (
                      <>
                        <div className="space-y-1">
                          <span className="text-[11px] text-muted">
                            {t.strategies.listingWatchVenues}
                          </span>
                          <div className="flex flex-wrap gap-1.5">
                            {sourceOptions.map((source) => {
                              const active = rule.eventSources.includes(source);
                              return (
                                <button
                                  key={source}
                                  type="button"
                                  disabled={!editingRule}
                                  onClick={() => toggleSource(rule.key, source)}
                                  className={cn(
                                    "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                                    active
                                      ? "border-accent/50 bg-accent/15 text-accent"
                                      : "border-line bg-panel-soft text-muted hover:border-accent/30",
                                  )}
                                >
                                  {venueLabel(source)}
                                </button>
                              );
                            })}
                          </div>
                          <p className="text-[11px] text-muted">
                            {t.strategies.skillSourcesHint}
                          </p>
                        </div>
                        <label className="block space-y-1">
                          <span className="text-[11px] text-muted">
                            {t.strategies.skillEventWindow}
                          </span>
                          <input
                            className={inputClass}
                            type="number"
                            min={60}
                            max={604800}
                            step="any"
                            value={rule.eventWindow}
                            disabled={!editingRule}
                            onChange={(event) =>
                              patchRule(rule.key, { eventWindow: event.target.value })
                            }
                          />
                        </label>
                      </>
                    ) : (
                      <>
                        <div className="grid gap-2 sm:grid-cols-2">
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillTimeframe}
                            </span>
                            <select
                              className={inputClass}
                              value={rule.timeframe}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { timeframe: event.target.value })
                              }
                            >
                              {SKILL_TIMEFRAMES.map((timeframe) => (
                                <option key={timeframe} value={timeframe}>
                                  {timeframe}
                                </option>
                              ))}
                            </select>
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillLookback}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={3}
                              step={1}
                              value={rule.lookback}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { lookback: event.target.value })
                              }
                            />
                          </label>
                        </div>
                        {rule.kind === "ma_trend" && (
                          <>
                            {/* 指标层：只声明要算哪些均线，规则层再引用它们。 */}
                            <div className="space-y-1">
                              <span className="text-[11px] text-muted">
                                {t.strategies.skillMaPeriods}
                              </span>
                              <div className="flex flex-wrap gap-1.5">
                                {maList.map((period) => {
                                  const locked = usedPeriods.has(period);
                                  return (
                                    <span
                                      key={period}
                                      title={locked ? t.strategies.skillMaReferenced : undefined}
                                      className="inline-flex items-center gap-1 rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-main"
                                    >
                                      MA{period}
                                      <button
                                        type="button"
                                        disabled={!editingRule || locked}
                                        onClick={() =>
                                          patchRule(rule.key, {
                                            maPeriods: maList
                                              .filter((item) => item !== period)
                                              .join(","),
                                          })
                                        }
                                        className="text-muted disabled:opacity-30"
                                      >
                                        <X size={11} />
                                      </button>
                                    </span>
                                  );
                                })}
                              </div>
                              <div className="flex items-center gap-1.5">
                                <input
                                  className={inputClass}
                                  type="number"
                                  min={2}
                                  max={1000}
                                  step={1}
                                  placeholder={t.strategies.skillMaPlaceholder}
                                  value={maInput[rule.key] ?? ""}
                                  disabled={!editingRule}
                                  onChange={(event) =>
                                    setMaInput((previous) => ({
                                      ...previous,
                                      [rule.key]: event.target.value,
                                    }))
                                  }
                                />
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  disabled={!editingRule}
                                  onClick={() => addMaPeriod(rule.key, rule.maPeriods)}
                                >
                                  <Plus size={12} />
                                  {t.strategies.skillMaAdd}
                                </Button>
                              </div>
                              <p className="text-[11px] text-muted">
                                {t.strategies.skillMaHint}
                              </p>
                            </div>

                            {/* 入场条件：全部满足且方向一致才开仓。 */}
                            <div className="space-y-1.5">
                              <span className="text-[11px] font-medium text-main">
                                {t.strategies.skillEntryTitle}
                              </span>
                              {rule.entries.map((entry, entryIndex) => (
                                <div
                                  key={`entry-${entryIndex}`}
                                  className="flex items-center gap-1.5"
                                >
                                  <span className="w-28 shrink-0 text-[11px] text-main">
                                    {entry.type === "alignment"
                                      ? t.strategies.skillEntryAlignment
                                      : entry.type === "price_above"
                                        ? t.strategies.skillEntryPriceAbove
                                        : t.strategies.skillEntryPullback}
                                  </span>
                                  {entry.type === "alignment" ? (
                                    <input
                                      className={inputClass}
                                      value={entry.periods.join(",")}
                                      disabled={!editingRule}
                                      onChange={(event) =>
                                        updateRuleItem<MaEntry>(
                                          rule.key,
                                          "entries",
                                          entryIndex,
                                          {
                                            type: "alignment",
                                            periods: parsePeriods(event.target.value),
                                          },
                                        )
                                      }
                                    />
                                  ) : (
                                    <select
                                      className={inputClass}
                                      value={entry.ma}
                                      disabled={!editingRule}
                                      onChange={(event) =>
                                        updateRuleItem<MaEntry>(
                                          rule.key,
                                          "entries",
                                          entryIndex,
                                          {
                                            type: entry.type,
                                            ma: Number(event.target.value),
                                          },
                                        )
                                      }
                                    >
                                      {maList.map((period) => (
                                        <option key={period} value={period}>
                                          MA{period}
                                        </option>
                                      ))}
                                    </select>
                                  )}
                                  <button
                                    type="button"
                                    disabled={!editingRule}
                                    onClick={() =>
                                      updateRuleItem<MaEntry>(rule.key, "entries", entryIndex, null)
                                    }
                                    className="text-muted disabled:opacity-30"
                                  >
                                    <X size={12} />
                                  </button>
                                </div>
                              ))}
                              <div className="flex flex-wrap gap-1.5">
                                <button
                                  type="button"
                                  disabled={!editingRule}
                                  onClick={() =>
                                    updateRuleItem<MaEntry>(
                                      rule.key,
                                      "entries",
                                      rule.entries.length,
                                      { type: "alignment", periods: maList },
                                    )
                                  }
                                  className="rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-muted hover:border-accent/30 disabled:opacity-30"
                                >
                                  + {t.strategies.skillEntryAlignment}
                                </button>
                                {(["price_above", "pullback"] as const).map((type) => (
                                  <button
                                    key={type}
                                    type="button"
                                    disabled={!editingRule}
                                    onClick={() =>
                                      updateRuleItem<MaEntry>(
                                        rule.key,
                                        "entries",
                                        rule.entries.length,
                                        {
                                          type,
                                          ma:
                                            maList.find((period) => period >= 20) ??
                                            maList[0] ??
                                            20,
                                        },
                                      )
                                    }
                                    className="rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-muted hover:border-accent/30 disabled:opacity-30"
                                  >
                                    +{" "}
                                    {type === "price_above"
                                      ? t.strategies.skillEntryPriceAbove
                                      : t.strategies.skillEntryPullback}
                                  </button>
                                ))}
                              </div>
                              <p className="text-[11px] text-muted">
                                {t.strategies.skillEntryHint}
                              </p>
                            </div>

                            {/* 出场条件：任一满足即离场。 */}
                            <div className="space-y-1.5">
                              <span className="text-[11px] font-medium text-main">
                                {t.strategies.skillExitTitle}
                              </span>
                              {rule.exits.map((exit, exitIndex) => (
                                <div
                                  key={`exit-${exitIndex}`}
                                  className="flex items-center gap-1.5"
                                >
                                  <span className="w-28 shrink-0 text-[11px] text-main">
                                    {exit.type === "break_ma"
                                      ? t.strategies.skillExitBreakMa
                                      : exit.type === "cross"
                                        ? t.strategies.skillExitCross
                                        : t.strategies.skillExitAtrStop}
                                  </span>
                                  {exit.type === "break_ma" && (
                                    <select
                                      className={inputClass}
                                      value={exit.ma}
                                      disabled={!editingRule}
                                      onChange={(event) =>
                                        updateRuleItem<MaExit>(
                                          rule.key,
                                          "exits",
                                          exitIndex,
                                          { type: "break_ma", ma: Number(event.target.value) },
                                        )
                                      }
                                    >
                                      {maList.map((period) => (
                                        <option key={period} value={period}>
                                          MA{period}
                                        </option>
                                      ))}
                                    </select>
                                  )}
                                  {exit.type === "cross" && (
                                    <div className="flex items-center gap-1">
                                      <select
                                        className={inputClass}
                                        value={exit.fast}
                                        disabled={!editingRule}
                                        onChange={(event) =>
                                          updateRuleItem<MaExit>(
                                            rule.key,
                                            "exits",
                                            exitIndex,
                                            { ...exit, fast: Number(event.target.value) },
                                          )
                                        }
                                      >
                                        {maList.map((period) => (
                                          <option key={period} value={period}>
                                            MA{period}
                                          </option>
                                        ))}
                                      </select>
                                      <select
                                        className={inputClass}
                                        value={exit.slow}
                                        disabled={!editingRule}
                                        onChange={(event) =>
                                          updateRuleItem<MaExit>(
                                            rule.key,
                                            "exits",
                                            exitIndex,
                                            { ...exit, slow: Number(event.target.value) },
                                          )
                                        }
                                      >
                                        {maList.map((period) => (
                                          <option key={period} value={period}>
                                            MA{period}
                                          </option>
                                        ))}
                                      </select>
                                    </div>
                                  )}
                                  {exit.type === "atr_stop" && (
                                    <span className="text-[11px] text-muted">
                                      ATR {rule.atr} × {rule.atrMultiple}
                                    </span>
                                  )}
                                  <button
                                    type="button"
                                    disabled={!editingRule}
                                    onClick={() =>
                                      updateRuleItem<MaExit>(rule.key, "exits", exitIndex, null)
                                    }
                                    className="text-muted disabled:opacity-30"
                                  >
                                    <X size={12} />
                                  </button>
                                </div>
                              ))}
                              <div className="flex flex-wrap gap-1.5">
                                {(["break_ma", "cross", "atr_stop"] as const).map((type) => (
                                  <button
                                    key={type}
                                    type="button"
                                    disabled={!editingRule}
                                    onClick={() => {
                                      const ma =
                                        maList.find((period) => period >= 20) ??
                                        maList[0] ??
                                        20;
                                      const next: MaExit =
                                        type === "break_ma"
                                          ? { type, ma }
                                          : type === "cross"
                                            ? { type, fast: maList[0] ?? 5, slow: maList[maList.length - 1] ?? 20 }
                                            : { type };
                                      updateRuleItem<MaExit>(
                                        rule.key,
                                        "exits",
                                        rule.exits.length,
                                        next,
                                      );
                                    }}
                                    className="rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-muted hover:border-accent/30 disabled:opacity-30"
                                  >
                                    +{" "}
                                    {type === "break_ma"
                                      ? t.strategies.skillExitBreakMa
                                      : type === "cross"
                                        ? t.strategies.skillExitCross
                                        : t.strategies.skillExitAtrStop}
                                  </button>
                                ))}
                              </div>
                              <p className="text-[11px] text-muted">
                                {t.strategies.skillExitHint}
                              </p>
                            </div>

                            {/* ATR 周期与止损倍数由指标/风控共用。 */}
                            <div className="grid gap-2 sm:grid-cols-2">
                              <label className="block space-y-1">
                                <span className="text-[11px] text-muted">
                                  {t.strategies.skillAtr}
                                </span>
                                <input
                                  className={inputClass}
                                  type="number"
                                  min={2}
                                  step={1}
                                  value={rule.atr}
                                  disabled={!editingRule}
                                  onChange={(event) =>
                                    patchRule(rule.key, { atr: event.target.value })
                                  }
                                />
                              </label>
                              <label className="block space-y-1">
                                <span className="text-[11px] text-muted">
                                  {t.strategies.skillAtrMultiple}
                                </span>
                                <input
                                  className={inputClass}
                                  type="number"
                                  min={0}
                                  step="any"
                                  value={rule.atrMultiple}
                                  disabled={!editingRule}
                                  onChange={(event) =>
                                    patchRule(rule.key, { atrMultiple: event.target.value })
                                  }
                                />
                              </label>
                            </div>
                          </>
                        )}
                        {rule.kind === "breakout" && (
                          <div className="space-y-3">
                            <div className="space-y-2 rounded-lg border border-line/60 bg-panel-soft/40 p-2.5">
                              <div className="flex items-center gap-2">
                                <span className="text-[11px] font-medium text-main">
                                  {t.strategies.skillTurtleSystems}
                                </span>
                                <span className="rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent">
                                  {t.strategies.skillTurtleCore}
                                </span>
                              </div>
                              {[
                                {
                                  label: t.strategies.skillTurtleSystem1,
                                  enabled: rule.system1Enabled,
                                  entry: rule.entry1,
                                  exit: rule.exit1,
                                  entryKey: "entry1" as const,
                                  exitKey: "exit1" as const,
                                  enabledKey: "system1Enabled" as const,
                                },
                                {
                                  label: t.strategies.skillTurtleSystem2,
                                  enabled: rule.system2Enabled,
                                  entry: rule.entry2,
                                  exit: rule.exit2,
                                  entryKey: "entry2" as const,
                                  exitKey: "exit2" as const,
                                  enabledKey: "system2Enabled" as const,
                                },
                              ].map((system) => (
                                <div key={system.enabledKey} className="space-y-1.5">
                                  <div className="flex items-center justify-between gap-2">
                                    <span className="text-[11px] text-main">{system.label}</span>
                                    <button
                                      type="button"
                                      disabled={!editingRule}
                                      onClick={() =>
                                        patchRule(rule.key, {
                                          [system.enabledKey]: !system.enabled,
                                        })
                                      }
                                      className={`rounded-full border px-2 py-0.5 text-[10px] disabled:opacity-40 ${
                                        system.enabled
                                          ? "border-accent/40 bg-accent/10 text-accent"
                                          : "border-line bg-panel text-muted"
                                      }`}
                                    >
                                      {system.enabled
                                        ? t.strategies.skillTurtleOn
                                        : t.strategies.skillTurtleOff}
                                    </button>
                                  </div>
                                  <div className="grid gap-2 sm:grid-cols-2">
                                    <label className="block space-y-1">
                                      <span className="text-[10px] text-muted">
                                        {t.strategies.skillTurtleEntry}
                                      </span>
                                      <input
                                        className={inputClass}
                                        type="number"
                                        min={2}
                                        step={1}
                                        value={system.entry}
                                        disabled={!editingRule || !system.enabled}
                                        onChange={(event) =>
                                          patchRule(rule.key, {
                                            [system.entryKey]: event.target.value,
                                          })
                                        }
                                      />
                                    </label>
                                    <label className="block space-y-1">
                                      <span className="text-[10px] text-muted">
                                        {t.strategies.skillTurtleExit}
                                      </span>
                                      <input
                                        className={inputClass}
                                        type="number"
                                        min={2}
                                        step={1}
                                        value={system.exit}
                                        disabled={!editingRule || !system.enabled}
                                        onChange={(event) =>
                                          patchRule(rule.key, {
                                            [system.exitKey]: event.target.value,
                                          })
                                        }
                                      />
                                    </label>
                                  </div>
                                </div>
                              ))}
                              <div className="flex items-center justify-between gap-2 border-t border-line/60 pt-2">
                                <div>
                                  <span className="text-[11px] text-main">
                                    {t.strategies.skillTurtleProfitFilter}
                                  </span>
                                  <p className="mt-0.5 text-[10px] text-muted">
                                    {t.strategies.skillTurtleProfitFilterHint}
                                  </p>
                                </div>
                                <button
                                  type="button"
                                  disabled={!editingRule}
                                  onClick={() =>
                                    patchRule(rule.key, { profitFilter: !rule.profitFilter })
                                  }
                                  className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] disabled:opacity-40 ${
                                    rule.profitFilter
                                      ? "border-accent/40 bg-accent/10 text-accent"
                                      : "border-line bg-panel text-muted"
                                  }`}
                                >
                                  {rule.profitFilter
                                    ? t.strategies.skillTurtleOn
                                    : t.strategies.skillTurtleOff}
                                </button>
                              </div>
                            </div>

                            <div className="space-y-2 rounded-lg border border-line/60 bg-panel-soft/40 p-2.5">
                              <div className="flex items-center gap-2">
                                <span className="text-[11px] font-medium text-main">
                                  {t.strategies.skillTurtleVolatility}
                                </span>
                                <span className="rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent">
                                  {t.strategies.skillTurtleCore}
                                </span>
                              </div>
                              <div className="grid gap-2 sm:grid-cols-2">
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleN}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={2}
                                    step={1}
                                    value={rule.atr}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { atr: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleStopMultiple}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step="any"
                                    value={rule.atrMultiple}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { atrMultiple: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleUnitRisk}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step="any"
                                    value={rule.riskPct}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { riskPct: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleMaxUnits}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={1}
                                    max={20}
                                    step={1}
                                    value={rule.maxUnits}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { maxUnits: event.target.value })
                                    }
                                  />
                                </label>
                              </div>
                            </div>

                            <div className="space-y-2 rounded-lg border border-line/60 bg-panel-soft/40 p-2.5">
                              <div className="flex items-center gap-2">
                                <span className="text-[11px] font-medium text-main">
                                  {t.strategies.skillTurtlePyramid}
                                </span>
                                <span className="rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent">
                                  {t.strategies.skillTurtleAdvanced}
                                </span>
                              </div>
                              <div className="grid gap-2 sm:grid-cols-2">
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleStep}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step="any"
                                    value={rule.pyramidStepN}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { pyramidStepN: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleMaxAdds}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    max={19}
                                    step={1}
                                    value={rule.maxAdds}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { maxAdds: event.target.value })
                                    }
                                  />
                                </label>
                              </div>
                              <p className="text-[10px] text-muted">
                                {t.strategies.skillTurtleAutoAdd} · {rule.maxAdds} /{" "}
                                {t.strategies.skillTurtleMaxAdds}
                              </p>
                              <TurtleSimPanel stepN={rule.pyramidStepN} maxUnits={rule.maxUnits} />
                            </div>

                            <div className="space-y-2 rounded-lg border border-line/60 bg-panel-soft/40 p-2.5">
                              <div className="flex items-center gap-2">
                                <span className="text-[11px] font-medium text-main">
                                  {t.strategies.skillTurtleRisk}
                                </span>
                                <span className="rounded-full bg-warn/10 px-1.5 py-0.5 text-[10px] text-warn">
                                  {t.strategies.skillTurtleRiskTag}
                                </span>
                              </div>
                              <div className="grid gap-2 sm:grid-cols-3">
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleMaxLeverage}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={1}
                                    step="any"
                                    value={rule.maxLeverage}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { maxLeverage: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleMaxSymbols}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step={1}
                                    value={rule.maxSymbols}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { maxSymbols: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillCooldownHours}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step="any"
                                    value={rule.cooldownHours}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { cooldownHours: event.target.value })
                                    }
                                  />
                                </label>
                                <label className="block space-y-1">
                                  <span className="text-[10px] text-muted">
                                    {t.strategies.skillTurtleMaxHold}
                                  </span>
                                  <input
                                    className={inputClass}
                                    type="number"
                                    min={0}
                                    step="any"
                                    value={rule.maxHoldHours}
                                    disabled={!editingRule}
                                    onChange={(event) =>
                                      patchRule(rule.key, { maxHoldHours: event.target.value })
                                    }
                                  />
                                </label>
                              </div>
                            </div>
                          </div>
                        )}
                        <p className="text-[11px] text-muted">
                          {t.strategies.skillIndicatorHint}
                        </p>
                      </>
                    )}
                  </div>

                  {/* 2. 执行步骤 */}
                  <div className="space-y-2">
                    <span className="text-[11px] font-medium text-main">
                      {t.strategies.skillStepsTitle}
                    </span>
                    <span className="block text-[11px] text-muted">
                      {t.strategies.listingExecutionVenues}
                    </span>
                    {executionOptions.length === 0 ? (
                      <p className="rounded-md border border-warning/30 bg-warning/10 px-2 py-1 text-[11px] text-warning">
                        {t.strategies.listingVenuesEmpty}
                      </p>
                    ) : (
                      <>
                        {rule.executionVenues.length > 0 && (
                          <ol className="space-y-1">
                            {rule.executionVenues.map((venue, venueIndex) => (
                              <li
                                key={venue}
                                className="flex items-center justify-between rounded-md border border-line bg-panel-soft px-2 py-1"
                              >
                                <span className="text-[11px] text-main">
                                  {venueIndex + 1}. {venueLabel(venue)}
                                </span>
                                <span className="flex items-center gap-1 text-muted">
                                  <button
                                    type="button"
                                    title={t.strategies.listingMoveUp}
                                    disabled={!editingRule || venueIndex === 0}
                                    onClick={() => moveExecution(rule.key, venueIndex, -1)}
                                    className="disabled:opacity-30"
                                  >
                                    <ChevronUp size={12} />
                                  </button>
                                  <button
                                    type="button"
                                    title={t.strategies.listingMoveDown}
                                    disabled={
                                      !editingRule ||
                                      venueIndex === rule.executionVenues.length - 1
                                    }
                                    onClick={() => moveExecution(rule.key, venueIndex, 1)}
                                    className="disabled:opacity-30"
                                  >
                                    <ChevronDown size={12} />
                                  </button>
                                  <button
                                    type="button"
                                    title={t.strategies.listingRemove}
                                    disabled={!editingRule}
                                    onClick={() => removeExecution(rule.key, venue)}
                                  >
                                    <X size={12} />
                                  </button>
                                </span>
                              </li>
                            ))}
                          </ol>
                        )}
                        <div className="flex flex-wrap gap-1.5">
                          {executionOptions
                            .filter((venue) => !rule.executionVenues.includes(venue))
                            .map((venue) => (
                              <button
                                key={venue}
                                type="button"
                                disabled={!editingRule}
                                onClick={() => addExecution(rule.key, venue)}
                                className="rounded-full border border-line bg-panel-soft px-2 py-0.5 text-[11px] text-muted transition-colors hover:border-accent/30 disabled:opacity-40"
                              >
                                + {venueLabel(venue)}
                              </button>
                            ))}
                        </div>
                      </>
                    )}
                    <p className="text-[11px] text-muted">{t.strategies.listingExecutionHint}</p>

                    <div className="space-y-1">
                      <span className="text-[11px] text-muted">{t.strategies.skillDirection}</span>
                      <div className="flex flex-wrap gap-1.5">
                        {(
                          [
                            ["both", t.strategies.skillDirectionBoth],
                            ["long", t.strategies.skillDirectionLong],
                            ["short", t.strategies.skillDirectionShort],
                          ] as const
                        ).map(([direction, label]) => {
                          const active = rule.direction === direction;
                          return (
                            <button
                              key={direction}
                              type="button"
                              disabled={!editingRule}
                              onClick={() => patchRule(rule.key, { direction })}
                              className={cn(
                                "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                                active
                                  ? "border-accent/50 bg-accent/15 text-accent"
                                  : "border-line bg-panel-soft text-muted hover:border-accent/30",
                              )}
                            >
                              {label}
                            </button>
                          );
                        })}
                      </div>
                      <p className="text-[11px] text-muted">{t.strategies.skillDirectionHint}</p>
                    </div>

                    {rule.kind === "breakout" && (
                      <div className="space-y-1">
                        <span className="text-[11px] text-muted">
                          {t.strategies.skillTurtleOrderType}
                        </span>
                        <div className="flex flex-wrap gap-1.5">
                          {(["market", "limit"] as SkillOrderType[]).map((type) => {
                            const active = rule.orderType === type;
                            return (
                              <button
                                key={type}
                                type="button"
                                disabled={!editingRule}
                                onClick={() => patchRule(rule.key, { orderType: type })}
                                className={cn(
                                  "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                                  active
                                    ? "border-accent/50 bg-accent/15 text-accent"
                                    : "border-line bg-panel-soft text-muted hover:border-accent/30",
                                )}
                              >
                                {type === "market"
                                  ? t.strategies.skillTurtleOrderMarket
                                  : t.strategies.skillTurtleOrderLimit}
                              </button>
                            );
                          })}
                        </div>
                        <p className="text-[11px] text-muted">
                          {t.strategies.skillTurtleOrderHint}
                        </p>
                      </div>
                    )}
                  </div>

                  {/* 3. 业务规则 */}
                  <div className="space-y-2">
                    <span className="text-[11px] font-medium text-main">
                      {t.strategies.skillBusinessTitle}
                    </span>
                    {rule.kind === "breakout" ? (
                      <p className="rounded-md border border-line/60 bg-panel-soft/40 px-2 py-1.5 text-[11px] text-muted">
                        {t.strategies.skillTurtleSizingHint}
                      </p>
                    ) : (
                    <div className="grid gap-2 sm:grid-cols-4">
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {t.strategies.listingNotional}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step="any"
                          value={rule.notional}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { notional: event.target.value })
                          }
                        />
                      </label>
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {t.strategies.listingLeverage}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step="any"
                          value={rule.leverage}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { leverage: event.target.value })
                          }
                        />
                      </label>
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {rule.kind === "grid"
                            ? t.strategies.skillGridTakeProfit
                            : t.strategies.listingTakeProfit}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step="any"
                          value={rule.takeProfit}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { takeProfit: event.target.value })
                          }
                        />
                      </label>
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {rule.kind === "grid"
                            ? t.strategies.skillGridStopLoss
                            : t.strategies.listingStopLoss}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step="any"
                          value={rule.stopLoss}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { stopLoss: event.target.value })
                          }
                        />
                      </label>
                      {rule.kind !== "event" && rule.kind !== "grid" && (
                        <label className="block space-y-1">
                          <span className="text-[11px] text-muted">
                            {t.strategies.skillRiskReward}
                          </span>
                          <input
                            className={inputClass}
                            type="number"
                            min={0}
                            step="any"
                            value={rule.riskReward}
                            disabled={!editingRule}
                            onChange={(event) =>
                              patchRule(rule.key, { riskReward: event.target.value })
                            }
                          />
                        </label>
                      )}
                    </div>
                    )}
                    {rule.kind === "grid" && (
                      <div className="space-y-2">
                        <div className="grid gap-2 sm:grid-cols-4">
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridLower}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step="any"
                              value={rule.gridLower}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { gridLower: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridUpper}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step="any"
                              value={rule.gridUpper}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { gridUpper: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridCount}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={2}
                              max={1000}
                              step={1}
                              value={rule.gridCount}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { gridCount: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridSpacing}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step="any"
                              value={rule.gridSpacing}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { gridSpacing: event.target.value })
                              }
                            />
                          </label>
                        </div>
                        <div className="grid gap-2 sm:grid-cols-3">
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridMode}
                            </span>
                            <select
                              className={inputClass}
                              value={rule.gridMode}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, {
                                  gridMode: event.target.value as GridSpacingMode,
                                })
                              }
                            >
                              <option value="arithmetic">{t.strategies.skillGridModeArithmetic}</option>
                              <option value="geometric">{t.strategies.skillGridModeGeometric}</option>
                            </select>
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridTrigger}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step="any"
                              value={rule.gridTrigger}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { gridTrigger: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillGridShift}
                            </span>
                            <select
                              className={inputClass}
                              value={rule.gridShift}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, {
                                  gridShift: event.target.value as GridShift,
                                })
                              }
                            >
                              <option value="none">{t.strategies.skillGridShiftNone}</option>
                              <option value="up">{t.strategies.skillGridShiftUp}</option>
                              <option value="down">{t.strategies.skillGridShiftDown}</option>
                              <option value="both">{t.strategies.skillGridShiftBoth}</option>
                            </select>
                          </label>
                        </div>
                        <label className="flex items-start gap-2 text-[11px] text-muted">
                          <input
                            type="checkbox"
                            className="mt-0.5 h-4 w-4 shrink-0 cursor-pointer accent-accent"
                            checked={rule.gridCloseOnFail}
                            disabled={!editingRule}
                            onChange={(event) =>
                              patchRule(rule.key, { gridCloseOnFail: event.target.checked })
                            }
                          />
                          <span>{t.strategies.skillGridCloseOnFail}</span>
                        </label>
                        <p className="text-[11px] text-muted">{t.strategies.skillGridHint}</p>
                      </div>
                    )}
                    <p className="text-[11px] text-muted">{t.strategies.skillZeroHint}</p>
                  </div>

                  {/* 4. 输出格式 */}
                  <div className="space-y-2">
                    <span className="text-[11px] font-medium text-main">
                      {t.strategies.skillOutputTitle}
                    </span>
                    <div className="flex flex-wrap gap-1.5">
                      {(["real", "paper"] as const).map((mode) => {
                        const active = rule.executionMode === mode;
                        return (
                          <button
                            key={mode}
                            type="button"
                            disabled={!editingRule}
                            onClick={() => patchRule(rule.key, { executionMode: mode })}
                            className={cn(
                              "rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                              active
                                ? "border-accent/50 bg-accent/15 text-accent"
                                : "border-line bg-panel-soft text-muted hover:border-accent/30",
                            )}
                          >
                            {mode === "real"
                              ? t.strategies.skillOutputReal
                              : t.strategies.skillOutputPaper}
                          </button>
                        );
                      })}
                    </div>
                  </div>

                  {/* 5. 注意事项 */}
                  <div className="space-y-2">
                    <span className="text-[11px] font-medium text-main">
                      {t.strategies.skillCautionsTitle}
                    </span>
                    <div className="grid gap-2 sm:grid-cols-5">
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {t.strategies.skillMaxNotional}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step="any"
                          value={rule.maxNotional}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { maxNotional: event.target.value })
                          }
                        />
                      </label>
                      <label className="block space-y-1">
                        <span className="text-[11px] text-muted">
                          {t.strategies.skillMaxDailyOpens}
                        </span>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          step={1}
                          value={rule.maxDailyOpens}
                          disabled={!editingRule}
                          onChange={(event) =>
                            patchRule(rule.key, { maxDailyOpens: event.target.value })
                          }
                        />
                      </label>
                      {rule.kind !== "event" &&
                        rule.kind !== "grid" &&
                        rule.kind !== "breakout" && (
                        <>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillRiskPct}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              max={100}
                              step="any"
                              value={rule.riskPct}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { riskPct: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillMaxPositions}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step={1}
                              value={rule.maxPositions}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { maxPositions: event.target.value })
                              }
                            />
                          </label>
                          <label className="block space-y-1">
                            <span className="text-[11px] text-muted">
                              {t.strategies.skillCooldownHours}
                            </span>
                            <input
                              className={inputClass}
                              type="number"
                              min={0}
                              step="any"
                              value={rule.cooldownHours}
                              disabled={!editingRule}
                              onChange={(event) =>
                                patchRule(rule.key, { cooldownHours: event.target.value })
                              }
                            />
                          </label>
                        </>
                      )}
                    </div>
                    <p className="text-[11px] text-muted">{t.strategies.skillCautionsHint}</p>
                  </div>

                  {editingRule && (
                    <div className="flex flex-wrap items-center gap-2">
                      <Button
                        size="sm"
                        disabled={saveSettings.isPending}
                        onClick={() => saveRule(rule.key)}
                      >
                        <Save size={13} />
                        {saveSettings.isPending
                          ? t.strategies.listingSaving
                          : t.strategies.listingSave}
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => cancelEdit(rule.key)}>
                        {t.strategies.listingRuleCancel}
                      </Button>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}

        {notice && (
          <p
            className={cn(
              "whitespace-pre-line rounded-md border px-2 py-1 text-[11px]",
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

function StrategyPublishCard({ record }: { record: StrategyRecord }) {
  const { t } = useI18n();
  const copy = t.market;
  const authors = useMarketAuthors();
  const publications = usePublications(true);
  const createPublication = useCreatePublication();
  const updatePublication = useUpdatePublication();
  const createPerformance = useCreatePerformance();
  const skillVersionQuery = useSkillVersions(record.skill?.skill_id ?? null);
  const [skillVersion, setSkillVersion] = useState(
    record.skill?.skill_version ? String(record.skill.skill_version) : "",
  );

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
            {record.skill?.skill_id ? (
              <label className="block text-[11px] text-muted">
                {t.skillsLibrary.versions}
                <select
                  className={inputClass}
                  value={skillVersion}
                  onChange={(event) => setSkillVersion(event.target.value)}
                >
                  <option value="">—</option>
                  {(skillVersionQuery.data ?? []).map((version) => (
                    <option key={version.version} value={version.version}>
                      v{version.version}
                      {version.passed ? "" : ` · ${t.skillsLibrary.failed}`}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
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
                    ...(record.skill?.skill_id && skillVersion
                      ? {
                          skill_id: record.skill.skill_id,
                          skill_version: Number(skillVersion),
                        }
                      : {}),
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

/**
 * 运行实例里的规则行：一条规则一行，只展示条件与当前状态。
 *
 * 运行中的实例读当前规则配置，规则的启停在技能参数卡片里操作；
 * 历史实例保留启动时的规则快照。
 */
function RunRuleList({
  strategyId,
  status,
  note,
  live,
}: {
  strategyId: string;
  status: StrategyStatus;
  /** 启动时的规则快照，历史实例与配置未加载时用它兜底。 */
  note?: string | null;
  /** 实例是否仍在运行。 */
  live: boolean;
}) {
  const { t } = useI18n();
  const settingsQuery = useSkillSettings(strategyId);

  const rules = settingsQuery.data?.settings.rules ?? [];

  const snapshot = (note ?? "")
    .split(/\n|；/)
    .filter((line) => line.trim().length > 0);

  /** 单条规则的触发条件，口径与应用范围卡片里的折叠行保持一致。 */
  const conditionOf = (rule: SkillRule) => {
    const keywords = rule.trigger.event_keywords;
    const keywordLabel =
      keywords.length === 0
        ? "内置关键词"
        : keywords.length === 1
          ? keywords[0]
          : `${keywords[0]} 等 ${keywords.length} 个关键词`;
    if (rule.kind === "grid") {
      const count =
        (rule.business.grid_count ?? 0) >= 2
          ? rule.business.grid_count
          : (rule.business.grid_levels ?? 3) * 2;
      const lower = Number(rule.business.grid_lower_price);
      const upper = Number(rule.business.grid_upper_price);
      const range =
        lower > 0 && upper > lower
          ? `${formatNumber(rule.business.grid_lower_price)}-${formatNumber(rule.business.grid_upper_price)}`
          : "自动区间";
      const mode =
        rule.business.grid_spacing_mode === "geometric" ? "等比" : "等差";
      return `${rule.trigger.timeframe} · ${range} · ${count} 格 · ${mode} · 间距 ${rule.business.grid_spacing_pct}%`;
    }
    return rule.kind === "event"
      ? `${
          rule.trigger.event_sources.length
            ? rule.trigger.event_sources.map(venueLabel).join(" / ")
            : t.strategies.skillAllSources
        }（${keywordLabel}）`
      : rule.kind === "ma_trend"
        ? `${rule.trigger.timeframe} · ${(
            rule.trigger.ma_periods.length > 0
              ? rule.trigger.ma_periods
              : [rule.trigger.fast, rule.trigger.slow]
          )
            .map((period) => `MA${period}`)
            .join("/")}`
        : `${rule.trigger.timeframe} · 通道 ${rule.trigger.channel} · ATR ${rule.trigger.atr_multiple}×`;
  };

  /** 折叠摘要：条件 → 执行交易所 · 输出方式 · 金额/杠杆 · 止盈止损。 */
  const summaryOf = (rule: SkillRule) => {
    const execution = rule.steps.execution_venues.length
      ? rule.steps.execution_venues.map(venueLabel).join(" / ")
      : t.strategies.listingAllVenues;
    const output =
      rule.steps.execution_mode === "real"
        ? t.strategies.skillOutputReal
        : t.strategies.skillOutputPaper;
    const count =
      (rule.business.grid_count ?? 0) >= 2
        ? rule.business.grid_count
        : (rule.business.grid_levels ?? 3) * 2;
    const target =
      rule.kind === "grid"
        ? `${formatNumber(rule.business.notional_usd)} USD ÷ ${count} 格`
        : `${formatNumber(rule.business.notional_usd)} USD · ${formatNumber(rule.business.leverage)}x`;
    const risk =
      rule.kind === "grid"
        ? ` · 整组 TP ${formatNumber(rule.business.take_profit_pct)}% · SL ${formatNumber(rule.business.stop_loss_pct)}%`
        : ` · TP ${formatNumber(rule.business.take_profit_pct)}% · SL ${formatNumber(rule.business.stop_loss_pct)}%`;
    return `${conditionOf(rule)} → ${execution} · ${output} · ${target}${risk}`;
  };

  // 运行实例只列正在跑的规则，停用的规则留在技能参数卡片里管理。
  const runningRules = rules.filter((rule) => rule.enabled && status === "running");

  if (!live || runningRules.length === 0) {
    return snapshot.length === 0 ? null : (
      <div className="flex w-full flex-col gap-0.5 text-body">
        {snapshot.map((line, index) => (
          <span key={`rule-${index}`}>{line}</span>
        ))}
      </div>
    );
  }

  return (
    <div className="w-full space-y-2">
      {runningRules.map((rule, index) => {
        const title =
          rule.name.trim() || fmt(t.strategies.skillRuleTitle, { index: index + 1 });
        return (
          <div
            key={rule.id ?? rule.name}
            className="flex items-center gap-2 rounded-lg border border-line bg-panel-soft/40 p-2.5"
          >
            <span className="shrink-0 text-[11px] font-medium text-main">{title}</span>
            <span className="min-w-0 flex-1 truncate text-[11px] text-muted">
              {summaryOf(rule)}
            </span>
            <span className="flex shrink-0 items-center gap-1.5 text-[11px] text-accent">
              <span className="size-1.5 rounded-full bg-accent" />
              {t.strategies.skillRuleRunning}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/// 运行情况：订单与各条规则的运行状态。
function StrategyRuntimeCard({
  record,
  eventDriven,
}: {
  record: StrategyRecord;
  /** 上币/技能等事件驱动策略用规则快照代替交易对与版本，且不提供执行模式切换。 */
  eventDriven: boolean;
}) {
  const { t } = useI18n();
  const copy = t.strategies;
  const updateMode = useUpdateStrategyExecutionMode();
  const { runtime } = record;
  const runsQuery = useStrategyRuns(record.id);
  const clearRuns = useClearStrategyRuns(record.id);
  const ordersQuery = useOrders();
  const positionsQuery = usePositions();

  // 只看这个策略触发的订单：手动下单与其它策略的互不干扰。
  const strategyOrders = [...(ordersQuery.data ?? [])]
    .filter((order) => order.intent.strategy_id === record.id)
    .sort((left, right) => right.created_at.localeCompare(left.created_at));
  const strategyPositions = (positionsQuery.data ?? []).filter(
    (position) => position.strategy_id === record.id,
  );

  const runs = runsQuery.data?.runs ?? [];
  const finishedRuns = runs.filter((run) => run.status !== "running");
  const hosted = (record.run_scope ?? "self_hosted") === "platform";
  // 徽标即标签页：运行规则与订单二选一显示，避免同时铺开两段列表。
  const [view, setView] = useState<"rules" | "orders">("rules");

  return (
    <Card>
      <CardHeader className="flex-wrap">
        <div>
          <CardTitle>{copy.runtimeTitle}</CardTitle>
          <CardDescription>{copy.runtimeDesc}</CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <button
            type="button"
            aria-pressed={view === "rules"}
            onClick={() => setView("rules")}
            className="cursor-pointer"
          >
            <Badge tone={view === "rules" ? "info" : "neutral"}>
              {hosted ? copy.runScopePlatform : copy.runScopeSelfHosted}
            </Badge>
          </button>
          <button
            type="button"
            aria-pressed={view === "orders"}
            onClick={() => setView("orders")}
            className="cursor-pointer"
          >
            <Badge tone={view === "orders" ? "info" : "neutral"}>
              {fmt(copy.orders, { count: runtime.orders })}
            </Badge>
          </button>
        </div>
      </CardHeader>
      <CardContent className="space-y-3 text-xs text-muted">
        {runtime.last_error && (
          <p className="text-danger">{fmt(copy.lastError, { message: runtime.last_error })}</p>
        )}

        {view === "rules" ? (
          <>
            <div>
              {finishedRuns.length > 0 && (
                <div className="mb-1 flex items-center justify-end">
                  <button
                    type="button"
                    onClick={() => clearRuns.mutate()}
                    disabled={clearRuns.isPending}
                    className="text-[11px] text-muted transition-colors hover:text-danger disabled:opacity-50"
                  >
                    {copy.runsClear}
                  </button>
                </div>
              )}
              {runs.length === 0 ? (
                <p className="text-[11px] text-muted">{copy.runsEmpty}</p>
              ) : (
                <ul className="space-y-1">
                  {runs.slice(0, 5).map((run) => (
                    <li key={run.id} className="text-[11px]">
                      {/* 规则行自带卡片样式，外层不再套边框，避免与应用范围卡片出现双层框。 */}
                      {/* 运行实例只显示规则行与该规则当前状态。 */}
                      <RunRuleList
                        strategyId={record.id}
                        status={record.status}
                        note={run.note}
                        live={run.status === "running"}
                      />
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        ) : (
          <PositionsOrdersPanel positions={strategyPositions} orders={strategyOrders} />
        )}

        {!eventDriven && (
          <div className="space-y-2 border-t border-line pt-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[11px] text-muted">{copy.executionMode}</span>
              {(["real", "paper"] as const).map((mode) => (
                <Button
                  key={mode}
                  size="sm"
                  variant={(record.execution_mode ?? "real") === mode ? "primary" : "secondary"}
                  disabled={updateMode.isPending || record.status === "running"}
                  onClick={() => updateMode.mutate({ id: record.id, mode })}
                >
                  {mode === "real" ? copy.executionModeReal : copy.executionModePaper}
                </Button>
              ))}
              <span className="text-[11px] text-subtle">{copy.executionModeDesc}</span>
            </div>
            {updateMode.isError && (
              <p className="text-[11px] text-danger">{errorText(updateMode.error)}</p>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** 海龟金字塔仓位的本地模拟：只演示加仓价与整组止损。 */
function TurtleSimPanel({ stepN, maxUnits }: { stepN: string; maxUnits: string }) {
  const { t } = useI18n();
  const [entry, setEntry] = useState("100");
  const [n, setN] = useState("5");
  const step = Math.max(0, Number(stepN) || 0);
  const units = Math.max(1, Math.min(20, Math.floor(Number(maxUnits) || 1)));
  const base = Number(entry) || 0;
  const nValue = Number(n) || 0;
  const distance = nValue * step;
  const rows = Array.from({ length: units }, (_, index) => ({
    unit: index + 1,
    price: base + distance * index,
  }));
  const stop = base + distance * (units - 1) - 2 * nValue;
  const format = (value: number) => (Number.isFinite(value) ? value.toFixed(2) : "—");

  return (
    <div className="rounded-md border border-line/60 bg-panel/60 p-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[11px] font-medium text-main">
          {t.strategies.skillTurtleSimTitle}
        </span>
        <span className="text-[10px] text-muted">
          {t.strategies.skillTurtleSimUnit} {units}
        </span>
      </div>
      <div className="mt-2 grid gap-2 sm:grid-cols-2">
        <label className="block space-y-1">
          <span className="text-[10px] text-muted">{t.strategies.skillTurtleSimEntry}</span>
          <input
            className={inputClass}
            type="number"
            step="any"
            value={entry}
            onChange={(event) => setEntry(event.target.value)}
          />
        </label>
        <label className="block space-y-1">
          <span className="text-[10px] text-muted">{t.strategies.skillTurtleSimN}</span>
          <input
            className={inputClass}
            type="number"
            step="any"
            value={n}
            onChange={(event) => setN(event.target.value)}
          />
        </label>
      </div>
      <div className="mt-2 space-y-1">
        {rows.map((row) => (
          <div key={row.unit} className="flex items-center justify-between text-[10px]">
            <span className="text-muted">
              {t.strategies.skillTurtleSimUnit} {row.unit}
            </span>
            <span className="text-main">{format(row.price)}</span>
          </div>
        ))}
      </div>
      <div className="mt-1 flex items-center justify-between border-t border-line/60 pt-1 text-[10px]">
        <span className="text-muted">{t.strategies.skillTurtleSimStop}</span>
        <span className="text-warn">{format(stop)}</span>
      </div>
      <p className="mt-1 text-[10px] leading-relaxed text-muted">
        {t.strategies.skillTurtleSimHint}
      </p>
    </div>
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
  const backtestStrategy = useBacktestStrategy();
  const rollbackStrategy = useRollbackStrategy();
  const deleteStrategy = useDeleteStrategy();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  // 左侧列表可收成窄条，把宽度让给右侧详情；点某条策略时自动收起。
  const [listCollapsed, setListCollapsed] = useState(false);
  // 策略多了以后分页展示，默认每页 10 条。
  const [listPage, setListPage] = useState(0);
  const [backtestDays, setBacktestDays] = useState(30);
  /** 删除/回滚的二次确认；Tauri 的 macOS WebView 不支持 window.confirm。 */
  const [confirmAction, setConfirmAction] = useState<{
    message: string;
    label: string;
    run: () => void;
  } | null>(null);

  const list = strategies.data ?? [];
  const listPageSize = 10;
  const listPages = Math.max(1, Math.ceil(list.length / listPageSize));
  const listPageCurrent = Math.min(listPage, listPages - 1);
  const pagedList = list.slice(
    listPageCurrent * listPageSize,
    (listPageCurrent + 1) * listPageSize,
  );
  const selected = creating ? null : (list.find((record) => record.id === selectedId) ?? list[0] ?? null);

  const actionError = [
    updateStrategy.error,
    createStrategy.error,
    instantiateTemplate.error,
    generateCode.error,
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

  const requestDelete = (record: StrategyRecord) => {
    setConfirmAction({
      message: t.strategies.deleteConfirm,
      label: t.strategies.delete,
      run: () =>
        deleteStrategy.mutate(record.id, {
          onSuccess: () => {
            if (selectedId === record.id) {
              setSelectedId(null);
            }
          },
        }),
    });
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

      <div
        className={cn(
          "grid gap-4",
          listCollapsed
            ? "lg:grid-cols-[56px_minmax(0,1fr)]"
            : "lg:grid-cols-[300px_minmax(0,1fr)]",
        )}
      >
        <Card className="h-fit">
          <CardHeader className={cn(listCollapsed && "justify-center px-2 py-3")}>
            {!listCollapsed && (
              <div>
                <CardTitle>{t.strategies.listTitle}</CardTitle>
                <CardDescription>{t.strategies.listDesc}</CardDescription>
              </div>
            )}
            <button
              type="button"
              title={
                listCollapsed ? t.strategies.listExpand : t.strategies.listCollapse
              }
              onClick={() => setListCollapsed((previous) => !previous)}
              className="text-muted transition-colors hover:text-accent"
            >
              {listCollapsed ? <ChevronRight size={14} /> : <ChevronLeft size={14} />}
            </button>
          </CardHeader>
          <CardContent className={cn("space-y-2", listCollapsed ? "p-1.5" : "p-2")}>
            {list.length === 0 && !listCollapsed && (
              <p className="px-2 py-6 text-center text-xs text-muted">{t.strategies.empty}</p>
            )}
            {listCollapsed &&
              pagedList.map((record) => {
                const active = !creating && selected?.id === record.id;
                return (
                  <button
                    key={record.id}
                    type="button"
                    title={record.definition.name}
                    onClick={() => {
                      setCreating(false);
                      setSelectedId(record.id);
                      setListCollapsed(false);
                    }}
                    className={cn(
                      "mx-auto grid h-9 w-9 place-items-center rounded-md border transition-colors",
                      active
                        ? "border-accent/40 bg-accent/10 text-accent"
                        : "border-line bg-panel-soft text-body hover:border-accent/30",
                    )}
                  >
                    <Bot size={15} />
                  </button>
                );
              })}
            {!listCollapsed &&
              pagedList.map((record) => {
              const active = !creating && selected?.id === record.id;
              return (
                <div
                  key={record.id}
                  className={cn(
                    "flex items-start gap-1 rounded-lg border transition-colors",
                    active
                      ? "border-accent/40 bg-accent/10"
                      : "border-line bg-panel-soft hover:border-accent/30",
                  )}
                >
                  <button
                    type="button"
                    onClick={() => {
                      setCreating(false);
                      setSelectedId(record.id);
                    }}
                    className="min-w-0 flex-1 space-y-2 px-3 py-2 text-left"
                  >
                    <div className="flex items-center gap-2">
                      <span className="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-panel text-body">
                        <Bot size={14} />
                      </span>
                      <span className="min-w-0 flex-1 truncate text-xs font-medium text-fg">
                        {record.definition.name}
                      </span>
                    </div>
                    {!isListingFollowStrategy(record) &&
                      !isSkillStrategy(record) && (
                      <>
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
                      </>
                    )}
                  </button>
                  <button
                    type="button"
                    title={t.strategies.delete}
                    disabled={deleteStrategy.isPending}
                    onClick={() => requestDelete(record)}
                    className="mr-1 mt-2 shrink-0 rounded-md p-1 text-muted transition-colors hover:bg-danger/10 hover:text-danger disabled:opacity-40"
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              );
            })}
            {listCollapsed && listPages > 1 && (
              <div className="flex flex-col items-center gap-0.5 pt-1 text-[10px] text-subtle">
                <button
                  type="button"
                  disabled={listPageCurrent <= 0}
                  onClick={() => setListPage(listPageCurrent - 1)}
                  className="text-muted transition-colors hover:text-accent disabled:opacity-40"
                >
                  <ChevronLeft size={12} />
                </button>
                <span>
                  {listPageCurrent + 1}/{listPages}
                </span>
                <button
                  type="button"
                  disabled={listPageCurrent >= listPages - 1}
                  onClick={() => setListPage(listPageCurrent + 1)}
                  className="text-muted transition-colors hover:text-accent disabled:opacity-40"
                >
                  <ChevronRight size={12} />
                </button>
              </div>
            )}
            {!listCollapsed && list.length > listPageSize && (
              <div className="flex flex-wrap items-center justify-between gap-1 border-t border-line px-1 pt-2 text-[11px] text-subtle">
                <span>
                  {fmt(t.strategies.pageInfo, {
                    page: listPageCurrent + 1,
                    pages: listPages,
                  })}
                </span>
                <div className="flex items-center gap-1">
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={listPageCurrent <= 0}
                    onClick={() => setListPage(listPageCurrent - 1)}
                  >
                    <ChevronLeft size={13} />
                    {t.strategies.prevPage}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={listPageCurrent >= listPages - 1}
                    onClick={() => setListPage(listPageCurrent + 1)}
                  >
                    {t.strategies.nextPage}
                    <ChevronRight size={13} />
                  </Button>
                </div>
              </div>
            )}
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
              listing={isListingFollowStrategy(selected)}
              skill={isSkillStrategy(selected)}
              skillKind={skillKind(selected) ?? "event"}
              backtestDays={backtestDays}
              onBacktestDays={setBacktestDays}
              onToggle={() =>
                updateStrategy.mutate({
                  id: selected.id,
                  status: selected.status === "running" ? "paused" : "running",
                })
              }
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
              pending={{
                toggle: updateStrategy.isPending,
                backtest: backtestStrategy.isPending,
                rollback: rollbackStrategy.isPending,
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
  listing,
  skill,
  skillKind,
  backtestDays,
  onBacktestDays,
  onToggle,
  onBacktest,
  onRollback,
  pending,
}: {
  record: StrategyRecord;
  listing: boolean;
  skill: boolean;
  skillKind: SkillKind;
  backtestDays: number;
  onBacktestDays: (days: number) => void;
  onToggle: () => void;
  onBacktest: () => void;
  onRollback: (version: number) => void;
  pending: {
    toggle: boolean;
    backtest: boolean;
    rollback: boolean;
  };
}) {
  const { t } = useI18n();
  // 事件驱动策略（上币公告跟随、技能策略）不展示代码、版本与执行模式。
  const eventDriven = listing || skill;
  // 技能策略按 SKILL.md 规则重放历史 K 线，可以回测；上币公告缺少历史公告样本，不回测。
  const showBacktest = !listing;
  const { definition, validation, last_backtest: report } = record;
  const running = record.status === "running";
  const errors = validation.issues.filter((issue) => issue.severity === "error");
  const warnings = validation.issues.filter((issue) => issue.severity === "warning");
  const winRate = report && report.trades > 0 ? (report.wins / report.trades) * 100 : null;

  // 版本历史默认收起，只有用户点击后才发起请求。
  const [versionsOpen, setVersionsOpen] = useState(false);
  const versions = useStrategyVersions(versionsOpen ? record.id : null);

  // 与后端 publish_readiness 保持一致：校验通过且已部署沙箱代码或配置技能参数即可发布；
  // 回测只作为参考信息，不是发布前置条件。
  const publishable = errors.length === 0 && (!!record.sandbox || !!record.skill);
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
            {!eventDriven && (
              <div className="flex flex-wrap items-center gap-1">
                <Badge tone={statusTone[record.status]}>{strategyStatusLabel(record.status)}</Badge>
              </div>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {showBacktest && (
              <Button size="sm" variant="secondary" disabled={pending.backtest} onClick={onBacktest}>
                <FlaskConical size={13} />
                {pending.backtest ? t.strategies.backtesting : t.strategies.backtest}
              </Button>
            )}
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
            </div>
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

      {!eventDriven && <StrategyCodeCard record={record} running={running} />}

      {skill ? (
        <StrategySkillScopeCard
          strategyId={record.id}
          status={record.status}
          kind={skillKind}
          purpose={skillBaseName(record.definition.name)}
          skillId={record.skill?.skill_id ?? null}
          boundVersion={record.skill?.skill_version ?? null}
        />
      ) : listing ? (
        <StrategyListingScopeCard strategyId={record.id} status={record.status} />
      ) : (
        <StrategyScopeCard
          record={record}
          running={running}
          onToggle={onToggle}
          pendingToggle={pending.toggle}
        />
      )}

      {record.source_publication_id ? null : publishable || published ? (
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


      {showBacktest && (
      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.strategies.backtestTitle}</CardTitle>
            <CardDescription>
              {skill ? t.strategies.backtestDescSkill : t.strategies.backtestDesc}
            </CardDescription>
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
                  label={skill ? t.strategies.report.bars : t.strategies.report.settlements}
                  value={formatNumber(report.settlements, 0)}
                />
                <Metric label={t.strategies.report.trades} value={formatNumber(report.trades, 0)} />
                <Metric
                  label={t.strategies.report.winRate}
                  value={winRate === null ? "—" : `${winRate.toFixed(0)}%`}
                />
                <Metric
                  label={skill ? t.strategies.report.grossPnl : t.strategies.report.grossFunding}
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
      )}

      <StrategyRuntimeCard record={record} eventDriven={eventDriven} />

      {!eventDriven && (
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
      )}
    </>
  );
}
