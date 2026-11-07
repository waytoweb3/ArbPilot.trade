import { useQueryClient } from "@tanstack/react-query";
import { Check, Copy, RefreshCw, Server, ShieldCheck, Terminal } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useAccount } from "../hooks/useAccount";
import { setApiToken, setExecutorMode, setExecutorUrl } from "../lib/api";
import { isNewerVersion } from "../lib/appUpdate";
import {
  deployServer,
  fetchManifest,
  isDesktopRuntime,
  loadCredentials,
  loadServerForm,
  onDeployLog,
  probeServer,
  saveCredentials,
  saveServerForm,
  type ProbeResult,
  type ReleaseManifest,
  type ServerForm,
  type ServerInput,
} from "../lib/deploy";
import { fmt, useI18n } from "../lib/i18n";

const inputClass =
  "w-full rounded-lg border border-line-strong bg-panel-soft px-3 py-2 text-sm text-fg outline-none transition-colors focus:border-accent/60";

type Busy = "probe" | "check" | "deploy" | null;

function CommandRow({ label, command }: { label: string; command: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    const mark = () => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    };
    try {
      await navigator.clipboard.writeText(command);
      mark();
      return;
    } catch {
      // 窗口未聚焦或旧内核不支持时，回退到隐藏文本框复制。
    }
    const holder = document.createElement("textarea");
    holder.value = command;
    holder.style.position = "fixed";
    holder.style.opacity = "0";
    document.body.appendChild(holder);
    holder.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(holder);
    if (ok) {
      mark();
    }
  };

  return (
    <div className="space-y-1.5">
      <p className="text-xs text-muted">{label}</p>
      <div className="flex items-start gap-2 rounded-lg border border-line bg-panel-soft px-3 py-2">
        <code className="min-w-0 flex-1 font-mono text-[11px] leading-relaxed break-all whitespace-pre-wrap text-fg">
          {command}
        </code>
        <button
          type="button"
          onClick={copy}
          className="flex shrink-0 items-center gap-1 rounded-md border border-line-strong px-2 py-1 text-[11px] text-muted transition-colors hover:text-fg"
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
          {copied ? t.docs.cliCopied : t.docs.cliCopy}
        </button>
      </div>
    </div>
  );
}

/** 自托管部署文档；嵌入设置页时隐藏自身标题。 */
export default function DocsPage({ embedded = false }: { embedded?: boolean }) {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const desktop = isDesktopRuntime();
  const account = useAccount();
  /** 自托管部署属于 $49/月基础版权益。 */
  const hasDeployPlan = account.data?.entitlement?.plans.includes("arbitrage") ?? false;

  const [form, setForm] = useState<ServerForm>(() => ({
    host: "",
    port: "22",
    username: "root",
    auth: "password",
    installDir: "/opt/arbpilot",
    ...loadServerForm(),
  }));
  const [password, setPassword] = useState("");
  const [privateKey, setPrivateKey] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [credentialsLoaded, setCredentialsLoaded] = useState(false);
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [manifest, setManifest] = useState<ReleaseManifest | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [logs, setLogs] = useState("");
  const [tokenFilled, setTokenFilled] = useState(false);
  const logsRef = useRef("");
  const logBoxRef = useRef<HTMLPreElement>(null);

  const buildServer = useCallback(
    (): ServerInput => ({
      host: form.host.trim(),
      port: Number(form.port) || 22,
      username: form.username.trim(),
      auth: form.auth,
      password: form.auth === "password" ? password : undefined,
      privateKey: form.auth === "key" ? privateKey : undefined,
      passphrase: form.auth === "key" ? passphrase || undefined : undefined,
      installDir: form.installDir.trim() || "/opt/arbpilot",
    }),
    [form, password, privateKey, passphrase],
  );

  const message = (value: unknown) => (value instanceof Error ? value.message : String(value));

  // 进入页面时回填本机钥匙串里保存的凭据，并检查一次最新版本。
  useEffect(() => {
    if (!desktop) {
      return;
    }
    let cancelled = false;
    fetchManifest()
      .then((result) => {
        if (!cancelled) {
          setManifest(result);
        }
      })
      .catch(() => undefined);

    const saved = loadServerForm();
    if (saved.host && saved.username) {
      loadCredentials({
        host: saved.host,
        port: Number(saved.port) || 22,
        username: saved.username,
        auth: saved.auth ?? "password",
        installDir: saved.installDir,
      })
        .then((credentials) => {
          if (cancelled || !credentials) {
            return;
          }
          setPassword(credentials.password ?? "");
          setPrivateKey(credentials.privateKey ?? "");
          setPassphrase(credentials.passphrase ?? "");
          setCredentialsLoaded(true);
        })
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
  }, [desktop]);

  useEffect(() => {
    const box = logBoxRef.current;
    if (box) {
      box.scrollTop = box.scrollHeight;
    }
  }, [logs]);

  const handleProbe = async () => {
    setError(null);
    setBusy("probe");
    try {
      const result = await probeServer(buildServer());
      setProbe(result);
      saveServerForm(form);
      await saveCredentials(buildServer()).catch(() => undefined);
    } catch (caught) {
      setProbe(null);
      setError(message(caught));
    } finally {
      setBusy(null);
    }
  };

  const handleCheck = async () => {
    setError(null);
    setBusy("check");
    try {
      setManifest(await fetchManifest());
    } catch (caught) {
      setError(`${t.docs.manifestError}：${message(caught)}`);
    } finally {
      setBusy(null);
    }
  };

  const handleDeploy = async () => {
    setError(null);
    if (!hasDeployPlan) {
      setError(t.docs.deployRequiresPlan);
      return;
    }
    setBusy("deploy");
    setLogs("");
    logsRef.current = "";
    setTokenFilled(false);
    const server = buildServer();
    try {
      const target = manifest ?? (await fetchManifest());
      setManifest(target);
      saveServerForm(form);
      await saveCredentials(server);

      const unlisten = await onDeployLog((chunk) => {
        logsRef.current += chunk;
        setLogs((current) => current + chunk);
      });
      try {
        const version = await deployServer(server, target);
        setProbe((current) => ({
          version,
          isRoot: current?.isRoot ?? false,
          installDir: server.installDir ?? "/opt/arbpilot",
        }));
        // install.sh 会把生成的访问令牌打印到日志里，部署完成后直接写入客户端连接配置。
        const match = logsRef.current.match(/访问令牌[：:]\s*([0-9a-fA-F]{16,})/);
        if (match) {
          // 刚部署的是用户自己的实例，切到自托管连接，避免把令牌写进托管配置。
          setExecutorMode("self");
          setApiToken(match[1]);
          setExecutorUrl("http://127.0.0.1:8787");
          setTokenFilled(true);
          queryClient.invalidateQueries();
        }
      } finally {
        unlisten();
      }
    } catch (caught) {
      setError(message(caught));
    } finally {
      setBusy(null);
    }
  };

  const serverVersion = probe?.version ?? null;
  const latestVersion = manifest?.version ?? null;
  const upToDate = Boolean(
    serverVersion && latestVersion && !isNewerVersion(latestVersion, serverVersion),
  );
  const deployLabel =
    busy === "deploy"
      ? t.docs.deploying
      : !serverVersion
        ? t.docs.deploy
        : upToDate
          ? t.docs.redeploy
          : fmt(t.docs.update, { version: latestVersion ?? "" });

  return (
    <div className={embedded ? "space-y-4" : "space-y-6"}>
      {!embedded && (
        <header>
          <h1 className="text-xl font-semibold text-fg">{t.docs.title}</h1>
          <p className="mt-1 text-sm text-muted">{t.docs.subtitle}</p>
        </header>
      )}

      {!desktop && (
        <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-[11px] leading-relaxed text-warning">
          <ShieldCheck size={14} className="mt-0.5 shrink-0" />
          {t.docs.desktopOnly}
        </div>
      )}

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div>
              <CardTitle>{t.docs.server}</CardTitle>
              <CardDescription>{t.docs.serverDesc}</CardDescription>
            </div>
            <Badge tone={probe ? "success" : "neutral"}>
              {probe ? t.docs.connected : t.docs.notDeployed}
            </Badge>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-[1fr_100px]">
              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.docs.host}</span>
                <input
                  className={`${inputClass} font-mono`}
                  value={form.host}
                  onChange={(event) => setForm({ ...form, host: event.target.value })}
                  placeholder="1.2.3.4"
                />
                <span className="block text-[11px] text-subtle">{t.docs.hostHint}</span>
              </label>
              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.docs.port}</span>
                <input
                  className={`${inputClass} font-mono`}
                  value={form.port}
                  onChange={(event) => setForm({ ...form, port: event.target.value })}
                  placeholder="22"
                />
              </label>
            </div>

            <label className="block space-y-1.5">
              <span className="text-xs text-muted">{t.docs.username}</span>
              <input
                className={`${inputClass} font-mono`}
                value={form.username}
                onChange={(event) => setForm({ ...form, username: event.target.value })}
                placeholder="root"
              />
            </label>

            <div className="space-y-1.5">
              <span className="text-xs text-muted">{t.docs.auth}</span>
              <div className="flex gap-2">
                {(["password", "key"] as const).map((kind) => (
                  <button
                    key={kind}
                    type="button"
                    onClick={() => setForm({ ...form, auth: kind })}
                    className={`rounded-lg border px-3 py-1.5 text-xs transition-colors ${
                      form.auth === kind
                        ? "border-accent/50 bg-accent/10 text-accent"
                        : "border-line-strong bg-panel-soft text-muted hover:text-body"
                    }`}
                  >
                    {kind === "password" ? t.docs.authPassword : t.docs.authKey}
                  </button>
                ))}
              </div>
            </div>

            {form.auth === "password" ? (
              <label className="block space-y-1.5">
                <span className="text-xs text-muted">{t.docs.password}</span>
                <input
                  className={`${inputClass} font-mono`}
                  type="password"
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                  autoComplete="off"
                />
              </label>
            ) : (
              <>
                <label className="block space-y-1.5">
                  <span className="text-xs text-muted">{t.docs.privateKey}</span>
                  <textarea
                    className={`${inputClass} min-h-24 font-mono text-[11px]`}
                    value={privateKey}
                    onChange={(event) => setPrivateKey(event.target.value)}
                    spellCheck={false}
                  />
                  <span className="block text-[11px] leading-relaxed text-subtle">
                    {t.docs.privateKeyHint}
                  </span>
                </label>
                <label className="block space-y-1.5">
                  <span className="text-xs text-muted">{t.docs.passphrase}</span>
                  <input
                    className={`${inputClass} font-mono`}
                    type="password"
                    value={passphrase}
                    onChange={(event) => setPassphrase(event.target.value)}
                    autoComplete="off"
                  />
                </label>
              </>
            )}

            <label className="block space-y-1.5">
              <span className="text-xs text-muted">{t.docs.installDir}</span>
              <input
                className={`${inputClass} font-mono`}
                value={form.installDir}
                onChange={(event) => setForm({ ...form, installDir: event.target.value })}
                placeholder="/opt/arbpilot"
              />
              <span className="block text-[11px] text-subtle">{t.docs.installDirHint}</span>
            </label>

            <div className="flex items-center gap-3">
              <Button size="sm" onClick={handleProbe} disabled={!desktop || busy !== null}>
                {busy === "probe" ? t.docs.testing : t.docs.test}
              </Button>
              {credentialsLoaded && <span className="text-[11px] text-accent">{t.docs.saved}</span>}
            </div>
            <p className="text-[11px] leading-relaxed text-muted">{t.docs.credentialNote}</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <CardTitle>{t.docs.release}</CardTitle>
              <CardDescription>{t.docs.releaseDesc}</CardDescription>
            </div>
            <Badge tone={upToDate ? "success" : "info"}>
              {upToDate ? t.docs.upToDate : (latestVersion ?? t.docs.unknown)}
            </Badge>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="rounded-lg border border-line bg-panel-soft px-3 py-2">
                <p className="text-[11px] text-muted">{t.docs.serverVersion}</p>
                <p className="mt-1 font-mono text-sm text-fg">
                  {serverVersion ?? t.docs.notDeployed}
                </p>
              </div>
              <div className="rounded-lg border border-line bg-panel-soft px-3 py-2">
                <p className="text-[11px] text-muted">{t.docs.latestVersion}</p>
                <p className="mt-1 font-mono text-sm text-fg">{latestVersion ?? t.docs.unknown}</p>
              </div>
            </div>

            {probe && (
              <div className="flex items-center gap-2 text-[11px] text-muted">
                <Server size={13} />
                {probe.isRoot ? t.docs.rootUser : t.docs.sudoUser}
              </div>
            )}

            <div className="flex flex-wrap items-center gap-2">
              <Button
                size="sm"
                variant="secondary"
                onClick={handleCheck}
                disabled={!desktop || busy !== null}
              >
                <RefreshCw size={13} />
                {busy === "check" ? t.docs.checking : t.docs.check}
              </Button>
              <Button
                size="sm"
                onClick={handleDeploy}
                disabled={!desktop || busy !== null || !hasDeployPlan}
              >
                <Terminal size={13} />
                {deployLabel}
              </Button>
            </div>

            {!hasDeployPlan && (
              <p className="text-[11px] leading-relaxed text-warning">
                {t.docs.deployRequiresPlan}
              </p>
            )}
            {tokenFilled && <p className="text-[11px] text-accent">{t.docs.tokenFilled}</p>}
            {error && <p className="text-[11px] leading-relaxed text-danger">{error}</p>}

            <div className="space-y-1.5">
              <p className="text-xs text-muted">{t.docs.logs}</p>
              <pre
                ref={logBoxRef}
                className="max-h-64 overflow-auto rounded-lg border border-line bg-panel-soft p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-muted"
              >
                {logs || t.docs.logsEmpty}
              </pre>
            </div>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.docs.steps}</CardTitle>
          </div>
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-2">
          {[
            [t.docs.step1Title, t.docs.step1],
            [t.docs.step2Title, t.docs.step2],
            [t.docs.step3Title, t.docs.step3],
            [t.docs.step4Title, t.docs.step4],
          ].map(([title, body], index) => (
            <div key={title} className="flex gap-3">
              <span className="grid h-6 w-6 shrink-0 place-items-center rounded-full bg-accent/15 text-[11px] font-semibold text-accent">
                {index + 1}
              </span>
              <div className="space-y-1">
                <p className="text-xs font-medium text-fg">{title}</p>
                <p className="text-[11px] leading-relaxed text-muted">{body}</p>
              </div>
            </div>
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.docs.cli}</CardTitle>
            <CardDescription>{t.docs.cliDesc}</CardDescription>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <CommandRow
            label={t.docs.cliInstall}
            command="curl -fsSL https://download.arbpilot.trade/executor/install.sh | sudo bash"
          />
          <CommandRow label={t.docs.cliUpdate} command="sudo arbpilot update" />
          <p className="text-[11px] leading-relaxed text-muted">{t.docs.cliNote}</p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>{t.settings.security}</CardTitle>
            <CardDescription>{t.docs.credentialNote}</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          <ul className="list-disc space-y-1.5 pl-5 text-xs leading-relaxed text-muted">
            <li>{t.docs.security1}</li>
            <li>{t.docs.security2}</li>
            <li>{t.docs.security3}</li>
          </ul>
        </CardContent>
      </Card>
    </div>
  );
}
