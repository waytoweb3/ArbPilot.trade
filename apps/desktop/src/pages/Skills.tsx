import { Plus, RefreshCw, ShieldCheck, Sparkles } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "../components/ui/card";
import {
  useCreateSkill,
  useCreateSkillVersion,
  useCreateStrategy,
  useMarketAuthors,
  useSkillDetail,
  useSkillDiff,
  useSkillVersions,
  useSkills,
  useValidateSkill,
} from "../hooks/useApi";
import { useI18n } from "../lib/i18n";
import type {
  ParamSpec,
  RuleBacktest,
  RuleDiff,
  SkillValidationIssue,
  SkillValidationReport,
  SkillVersion,
} from "../lib/types";

const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** 把参数值格式化成输入框文本；数字统一走字符串以保留精度。 */
function paramText(value: unknown): string {
  if (value === null || value === undefined) {
    return "";
  }
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** 参数空间的一行编辑态。 */
interface ParamDraft {
  editable: boolean;
  min: string;
  max: string;
  step: string;
}

function reportTone(report: SkillValidationReport) {
  if (!report.passed) {
    return "danger" as const;
  }
  return report.warnings > 0 ? ("warning" as const) : ("success" as const);
}

/** 本次校验跑出的回测快照；公告类与样本不足的规则不会出现。 */
function BacktestList({ backtests }: { backtests: RuleBacktest[] }) {
  const { t } = useI18n();
  if (backtests.length === 0) {
    return null;
  }
  return (
    <div className="rounded-lg border border-line bg-panel px-3 py-2">
      <p className="text-[11px] font-medium text-body">{t.skillsLibrary.backtestTitle}</p>
      <ul className="mt-1 space-y-1">
        {backtests.map((item) => (
          <li key={item.rule_id} className="text-[10px] text-muted">
            <span className="mr-1 text-body">{item.name}</span>
            <span className="font-mono">
              {item.symbol} · {item.trades} 笔 · 胜率 {String(item.win_rate_pct)}% · 净收益{" "}
              {String(item.net_pnl_usd)} USD · 收益率 {String(item.return_pct)}% · 最大回撤{" "}
              {String(item.max_drawdown_pct)}%
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ReportView({
  report,
  backtests,
}: {
  report: SkillValidationReport;
  backtests?: RuleBacktest[];
}) {
  const { t } = useI18n();
  return (
    <div className="space-y-2 rounded-lg border border-line bg-panel-soft/40 p-3">
      <div className="flex items-center gap-2">
        <Badge tone={reportTone(report)}>
          {report.passed ? t.skillsLibrary.passed : t.skillsLibrary.failed}
        </Badge>
        <span className="whitespace-pre-line text-[11px] text-muted">{report.summary}</span>
      </div>
      <div className="grid gap-2 md:grid-cols-2">
        {report.dimensions.map((dimension) => (
          <div
            key={dimension.dimension}
            className="rounded-md border border-line bg-panel px-2.5 py-2"
          >
            <div className="flex items-center justify-between gap-2">
              <span className="text-[11px] font-medium text-body">{dimension.label}</span>
              <span className="font-mono text-[10px] text-subtle">
                {dimension.errors}/{dimension.warnings}
              </span>
            </div>
            {dimension.issues.length > 0 && (
              <ul className="mt-1 space-y-1">
                {dimension.issues.map((issue: SkillValidationIssue, index) => (
                  <li key={`${issue.code}-${index}`} className="text-[10px] text-muted">
                    <span className="mr-1 font-mono text-subtle">{issue.severity}</span>
                    {issue.message}
                  </li>
                ))}
              </ul>
            )}
          </div>
        ))}
      </div>
      {backtests && <BacktestList backtests={backtests} />}
    </div>
  );
}

function DiffView({ diff }: { diff: RuleDiff }) {
  const { t } = useI18n();
  const empty =
    diff.added.length === 0 && diff.removed.length === 0 && diff.changed.length === 0;
  if (empty) {
    return <p className="text-[11px] text-muted">{t.skillsLibrary.noDiff}</p>;
  }
  return (
    <div className="space-y-2 text-[11px]">
      {diff.added.length > 0 && (
        <div>
          <Badge tone="success">{t.skillsLibrary.added}</Badge>
          <span className="ml-2 text-body">
            {diff.added.map((rule) => rule.name).join("、")}
          </span>
        </div>
      )}
      {diff.removed.length > 0 && (
        <div>
          <Badge tone="danger">{t.skillsLibrary.removed}</Badge>
          <span className="ml-2 text-body">
            {diff.removed.map((rule) => rule.name).join("、")}
          </span>
        </div>
      )}
      {diff.changed.map((change) => (
        <div key={change.rule_id} className="rounded-md border border-line px-2 py-1">
          <Badge tone="warning">{t.skillsLibrary.changed}</Badge>
          <span className="ml-2 text-body">{change.name}</span>
          <span className="ml-2 font-mono text-[10px] text-subtle">
            {change.changed_fields.join(", ")}
          </span>
        </div>
      ))}
    </div>
  );
}

function VersionCard({
  version,
  active,
  onSelect,
}: {
  version: SkillVersion;
  active: boolean;
  onSelect: () => void;
}) {
  const { t } = useI18n();
  return (
    <button
      type="button"
      onClick={onSelect}
      className={`w-full rounded-lg border px-3 py-2 text-left transition-colors ${
        active ? "border-accent/50 bg-accent/5" : "border-line hover:bg-hover"
      }`}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-fg">v{version.version}</span>
        <Badge tone={version.passed ? "success" : "danger"}>
          {version.passed ? t.skillsLibrary.passed : t.skillsLibrary.failed}
        </Badge>
      </div>
      <p className="mt-1 truncate text-[10px] text-muted">{version.changelog || "—"}</p>
      <p className="mt-0.5 truncate font-mono text-[10px] text-subtle">
        {t.skillsLibrary.rules} {version.settings.rules.length} · 可改{" "}
        {(version.params_schema?.params ?? []).filter((spec) => spec.editable).length} ·{" "}
        {(version.logic_sha256 || version.settings_sha256).slice(0, 12)}
      </p>
    </button>
  );
}

export default function SkillsPage() {
  const { t } = useI18n();
  const navigate = useNavigate();
  const authors = useMarketAuthors();
  const [authorId, setAuthorId] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedVersion, setSelectedVersion] = useState<number | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [markdown, setMarkdown] = useState("");
  const [changelog, setChangelog] = useState("");
  const [versionMarkdown, setVersionMarkdown] = useState("");
  const [versionChangelog, setVersionChangelog] = useState("");
  const [diffFrom, setDiffFrom] = useState(1);
  const [diffTo, setDiffTo] = useState(1);
  const [notice, setNotice] = useState<string | null>(null);
  // 参数空间编辑态：键为 `rule_key/path`。
  const [paramDrafts, setParamDrafts] = useState<Record<string, ParamDraft>>({});

  const activeAuthorId = authorId || authors.data?.[0]?.author_id || "";
  const skills = useSkills(activeAuthorId || undefined);
  const detail = useSkillDetail(selectedId);
  const versions = useSkillVersions(selectedId);
  const createSkill = useCreateSkill();
  const createVersion = useCreateSkillVersion();
  const createStrategy = useCreateStrategy();
  const validate = useValidateSkill();
  const diff = useSkillDiff(selectedId, diffFrom, diffTo);

  useEffect(() => {
    const list = skills.data;
    if (!list || list.length === 0) {
      setSelectedId(null);
      return;
    }
    if (!selectedId || !list.some((skill) => skill.id === selectedId)) {
      setSelectedId(list[0].id);
    }
  }, [skills.data, selectedId]);

  useEffect(() => {
    if (versions.data && versions.data.length > 0) {
      const latest = versions.data[versions.data.length - 1].version;
      setSelectedVersion((current) =>
        current && versions.data!.some((item) => item.version === current) ? current : latest,
      );
      setDiffFrom((current) => (current <= latest ? current : 1));
      setDiffTo(latest);
    }
  }, [versions.data]);

  const activeVersion = useMemo(
    () => versions.data?.find((version) => version.version === selectedVersion) ?? null,
    [versions.data, selectedVersion],
  );

  // 切换版本时把已保存的参数空间还原成编辑态。
  useEffect(() => {
    setParamDrafts(
      Object.fromEntries(
        (activeVersion?.params_schema?.params ?? []).map((spec) => [
          `${spec.rule_key}/${spec.path}`,
          {
            editable: spec.editable,
            min: paramText(spec.min),
            max: paramText(spec.max),
            step: paramText(spec.step),
          },
        ]),
      ),
    );
  }, [activeVersion]);

  const handleCreate = async () => {
    if (!activeAuthorId) {
      setNotice(t.skillsLibrary.authorRequired);
      return;
    }
    if (!markdown.trim()) {
      setNotice(t.skillsLibrary.markdownRequired);
      return;
    }
    try {
      const created = await createSkill.mutateAsync({
        author_id: activeAuthorId,
        name: name.trim() || "未命名技能",
        description: description.trim(),
        markdown,
        changelog: changelog.trim() || undefined,
      });
      setSelectedId(created.skill.id);
      setSelectedVersion(created.version?.version ?? 1);
      setName("");
      setDescription("");
      setMarkdown("");
      setChangelog("");
      setNotice(null);
    } catch (error) {
      setNotice(errorText(error));
    }
  };

  const handleCreateVersion = async () => {
    if (!selectedId) {
      return;
    }
    if (!versionMarkdown.trim()) {
      setNotice(t.skillsLibrary.markdownRequired);
      return;
    }
    try {
      const created = await createVersion.mutateAsync({
        skillId: selectedId,
        markdown: versionMarkdown,
        changelog: versionChangelog,
      });
      setSelectedVersion(created.version);
      setVersionMarkdown("");
      setVersionChangelog("");
      setNotice(`${t.skillsLibrary.versions} v${created.version}`);
    } catch (error) {
      setNotice(errorText(error));
    }
  };

  const setDraft = (key: string, patch: Partial<ParamDraft>) =>
    setParamDrafts((drafts) => {
      const merged: ParamDraft = drafts[key] ?? {
        editable: false,
        min: "",
        max: "",
        step: "",
      };
      return { ...drafts, [key]: { ...merged, ...patch } };
    });

  /** 以当前版本的规则骨架 + 编辑后的参数空间另存新版本。 */
  const handleSaveParams = async () => {
    if (!selectedId || !activeVersion) {
      return;
    }
    const params: ParamSpec[] = (activeVersion.params_schema?.params ?? []).map((spec) => {
      const draft = paramDrafts[`${spec.rule_key}/${spec.path}`];
      if (!draft) {
        return spec;
      }
      return {
        ...spec,
        editable: draft.editable,
        min: draft.min.trim() ? draft.min.trim() : null,
        max: draft.max.trim() ? draft.max.trim() : null,
        step: draft.step.trim() ? draft.step.trim() : null,
      };
    });
    try {
      const created = await createVersion.mutateAsync({
        skillId: selectedId,
        settings: activeVersion.settings,
        params_schema: { params },
        changelog: versionChangelog.trim() || "调整参数空间",
      });
      setSelectedVersion(created.version);
      setVersionChangelog("");
      setNotice(`参数空间已保存为 v${created.version}`);
    } catch (error) {
      setNotice(errorText(error));
    }
  };

  const handleValidate = async () => {
    if (!selectedId || !selectedVersion) {
      return;
    }
    try {
      await validate.mutateAsync({ skillId: selectedId, version: selectedVersion });
      setNotice(t.skillsLibrary.validated);
    } catch (error) {
      setNotice(errorText(error));
    }
  };

  const handleCreateStrategy = async () => {
    if (!selectedId || !selectedVersion || !detail.data) {
      return;
    }
    try {
      await createStrategy.mutateAsync({
        name: `${detail.data.skill.name} v${selectedVersion}`,
        skill: {
          template_key: "skill",
          skill_id: selectedId,
          skill_version: selectedVersion,
        },
      });
      navigate("/strategies");
    } catch (error) {
      setNotice(errorText(error));
    }
  };

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-lg font-semibold text-fg">{t.skillsLibrary.title}</h1>
        <p className="mt-1 text-xs text-muted">{t.skillsLibrary.desc}</p>
      </div>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.skillsLibrary.author}</CardTitle>
            <CardDescription>{t.skillsLibrary.selectAuthor}</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          {authors.data && authors.data.length > 0 ? (
            <select
              value={activeAuthorId}
              onChange={(event) => setAuthorId(event.target.value)}
              className="h-9 w-full max-w-sm rounded-md border border-line bg-panel px-2 text-xs text-body"
            >
              {authors.data.map((author) => (
                <option key={author.author_id} value={author.author_id}>
                  {author.display_name}
                </option>
              ))}
            </select>
          ) : (
            <p className="text-[11px] text-muted">{t.skillsLibrary.authorsEmpty}</p>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 lg:grid-cols-[1.1fr_1fr]">
        <Card>
          <CardHeader>
            <div>
              <CardTitle>{t.skillsLibrary.createTitle}</CardTitle>
              <CardDescription>{t.skillsLibrary.desc}</CardDescription>
            </div>
            <Sparkles size={15} className="text-accent" />
          </CardHeader>
          <CardContent className="space-y-2">
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder={t.skillsLibrary.namePlaceholder}
              className="h-9 w-full rounded-md border border-line bg-panel px-2 text-xs text-body"
            />
            <input
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder={t.skillsLibrary.description}
              className="h-9 w-full rounded-md border border-line bg-panel px-2 text-xs text-body"
            />
            <textarea
              value={markdown}
              onChange={(event) => setMarkdown(event.target.value)}
              placeholder={t.skillsLibrary.markdownPlaceholder}
              rows={8}
              className="w-full rounded-md border border-line bg-panel px-2 py-2 font-mono text-[11px] text-body"
            />
            <input
              value={changelog}
              onChange={(event) => setChangelog(event.target.value)}
              placeholder={t.skillsLibrary.changelog}
              className="h-9 w-full rounded-md border border-line bg-panel px-2 text-xs text-body"
            />
            <Button size="sm" onClick={handleCreate} disabled={createSkill.isPending}>
              <Plus size={13} />
              {createSkill.isPending ? t.skillsLibrary.creating : t.skillsLibrary.create}
            </Button>
            {createSkill.error && (
              <p className="text-[11px] text-danger">{errorText(createSkill.error)}</p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <CardTitle>{t.skillsLibrary.title}</CardTitle>
              <CardDescription>{t.skillsLibrary.selectSkill}</CardDescription>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            {skills.data && skills.data.length > 0 ? (
              skills.data.map((skill) => (
                <button
                  key={skill.id}
                  type="button"
                  onClick={() => {
                    validate.reset();
                    setSelectedId(skill.id);
                    setSelectedVersion(null);
                  }}
                  className={`w-full rounded-lg border px-3 py-2 text-left transition-colors ${
                    selectedId === skill.id
                      ? "border-accent/50 bg-accent/5"
                      : "border-line hover:bg-hover"
                  }`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-xs font-medium text-fg">{skill.name}</span>
                    <Badge tone="info">v{skill.latest_version}</Badge>
                  </div>
                  <p className="mt-1 truncate text-[10px] text-muted">
                    {skill.description || "—"}
                  </p>
                </button>
              ))
            ) : (
              <p className="text-[11px] text-muted">{t.skillsLibrary.empty}</p>
            )}
            {skills.error && (
              <p className="text-[11px] text-danger">{errorText(skills.error)}</p>
            )}
          </CardContent>
        </Card>
      </div>

      {detail.data && (
        <Card>
          <CardHeader>
            <div>
              <CardTitle>
                {detail.data.skill.name} · {t.skillsLibrary.versions}
              </CardTitle>
              <CardDescription>
                {t.skillsLibrary.sha} {detail.data.version?.settings_sha256.slice(0, 16) ?? "—"}
              </CardDescription>
            </div>
            <Button
              size="sm"
              variant="secondary"
              onClick={handleValidate}
              disabled={validate.isPending || !selectedVersion}
            >
              <ShieldCheck size={13} />
              {t.skillsLibrary.validate}
            </Button>
            <Button
              size="sm"
              onClick={handleCreateStrategy}
              disabled={createStrategy.isPending || !activeVersion?.passed}
            >
              <Plus size={13} />
              {t.skillsLibrary.createStrategy}
            </Button>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="grid gap-2 md:grid-cols-3">
              {(versions.data ?? []).map((version) => (
                <VersionCard
                  key={version.version}
                  version={version}
                  active={selectedVersion === version.version}
                  onSelect={() => {
                    validate.reset();
                    setSelectedVersion(version.version);
                  }}
                />
              ))}
            </div>

            {activeVersion && (
              <ReportView
                report={activeVersion.validation}
                backtests={validate.data?.backtests}
              />
            )}

            {activeVersion && (
              <div className="space-y-2 rounded-lg border border-line bg-panel-soft/40 p-3">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div>
                    <p className="text-[11px] font-medium text-body">参数空间</p>
                    <p className="text-[10px] text-muted">
                      勾选「可改」并设置范围后，买家可在自己的终端里填值生成实例。
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={handleSaveParams}
                    disabled={createVersion.isPending}
                  >
                    保存为新版本
                  </Button>
                </div>
                <div className="max-h-64 space-y-1 overflow-y-auto">
                  {(activeVersion.params_schema?.params ?? []).map((spec) => {
                    const key = `${spec.rule_key}/${spec.path}`;
                    const draft = paramDrafts[key] ?? {
                      editable: spec.editable,
                      min: paramText(spec.min),
                      max: paramText(spec.max),
                      step: paramText(spec.step),
                    };
                    return (
                      <div
                        key={key}
                        className="grid grid-cols-[minmax(0,1fr)_auto_4.5rem_4.5rem_4.5rem] items-center gap-2"
                      >
                        <span className="truncate text-[11px] text-body" title={spec.path}>
                          {spec.label}
                        </span>
                        <label className="flex items-center gap-1 text-[10px] text-muted">
                          <input
                            type="checkbox"
                            checked={draft.editable}
                            onChange={(event) =>
                              setDraft(key, { editable: event.target.checked })
                            }
                          />
                          可改
                        </label>
                        <input
                          className="rounded border border-line bg-panel px-1 font-mono text-[10px]"
                          placeholder="最小"
                          value={draft.min}
                          onChange={(event) => setDraft(key, { min: event.target.value })}
                        />
                        <input
                          className="rounded border border-line bg-panel px-1 font-mono text-[10px]"
                          placeholder="最大"
                          value={draft.max}
                          onChange={(event) => setDraft(key, { max: event.target.value })}
                        />
                        <input
                          className="rounded border border-line bg-panel px-1 font-mono text-[10px]"
                          placeholder="步长"
                          value={draft.step}
                          onChange={(event) => setDraft(key, { step: event.target.value })}
                        />
                      </div>
                    );
                  })}
                  {(activeVersion.params_schema?.params ?? []).length === 0 && (
                    <p className="text-[10px] text-muted">
                      当前版本没有可参数化的字段。
                    </p>
                  )}
                </div>
              </div>
            )}

            <div className="grid gap-2 md:grid-cols-[1fr_1fr_auto]">
              <textarea
                value={versionMarkdown}
                onChange={(event) => setVersionMarkdown(event.target.value)}
                placeholder={t.skillsLibrary.markdownPlaceholder}
                rows={5}
                className="rounded-md border border-line bg-panel px-2 py-2 font-mono text-[11px] text-body"
              />
              <input
                value={versionChangelog}
                onChange={(event) => setVersionChangelog(event.target.value)}
                placeholder={t.skillsLibrary.newVersionChangelog}
                className="h-9 rounded-md border border-line bg-panel px-2 text-xs text-body"
              />
              <Button
                size="sm"
                onClick={handleCreateVersion}
                disabled={createVersion.isPending}
              >
                <Plus size={13} />
                {t.skillsLibrary.createVersion}
              </Button>
            </div>

            <div className="flex flex-wrap items-center gap-2 rounded-lg border border-line bg-panel-soft/40 p-3">
              <span className="text-[11px] font-medium text-body">{t.skillsLibrary.diff}</span>
              <label className="flex items-center gap-1 text-[11px] text-muted">
                {t.skillsLibrary.from}
                <input
                  type="number"
                  min={1}
                  value={diffFrom}
                  onChange={(event) => setDiffFrom(Number(event.target.value))}
                  className="h-7 w-16 rounded border border-line bg-panel px-1 font-mono text-[11px]"
                />
              </label>
              <label className="flex items-center gap-1 text-[11px] text-muted">
                {t.skillsLibrary.to}
                <input
                  type="number"
                  min={1}
                  value={diffTo}
                  onChange={(event) => setDiffTo(Number(event.target.value))}
                  className="h-7 w-16 rounded border border-line bg-panel px-1 font-mono text-[11px]"
                />
              </label>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => diff.refetch()}
                disabled={diff.isFetching}
              >
                <RefreshCw size={12} />
                {t.skillsLibrary.preview}
              </Button>
              {diff.data && <div className="w-full"><DiffView diff={diff.data} /></div>}
              {diff.error && <p className="text-[11px] text-danger">{errorText(diff.error)}</p>}
            </div>

            {notice && <p className="text-[11px] text-muted">{notice}</p>}
            {validate.error && (
              <p className="text-[11px] text-danger">{errorText(validate.error)}</p>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  );
}
