import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useRiskLimits } from "../hooks/useApi";
import { DEFAULT_TOKEN, EXECUTOR_URL, getApiToken, setApiToken } from "../lib/api";

const inputClass =
  "w-full rounded-lg border border-ink-600 bg-ink-850 px-3 py-2 text-sm text-slate-100 outline-none transition-colors focus:border-emerald-400/60";

export default function SettingsPage() {
  const queryClient = useQueryClient();
  const riskLimits = useRiskLimits();
  const [token, setToken] = useState(getApiToken());
  const [saved, setSaved] = useState(false);

  const save = () => {
    setApiToken(token.trim() || DEFAULT_TOKEN);
    setSaved(true);
    queryClient.invalidateQueries();
    window.setTimeout(() => setSaved(false), 2_000);
  };

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-white">设置</h1>
        <p className="mt-1 text-sm text-slate-500">
          凭证由执行器持有。桌面端只保存访问令牌，不保存交易所 API Key。
        </p>
      </header>

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <div>
              <CardTitle>执行器连接</CardTitle>
              <CardDescription>本机或用户自有服务器上的执行器</CardDescription>
            </div>
            <Badge tone="info">本地</Badge>
          </CardHeader>
          <CardContent className="space-y-4">
            <label className="block space-y-1.5">
              <span className="text-xs text-slate-400">执行器地址</span>
              <input className={`${inputClass} font-mono`} value={EXECUTOR_URL} readOnly />
            </label>
            <label className="block space-y-1.5">
              <span className="text-xs text-slate-400">访问令牌</span>
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
                保存令牌
              </Button>
              {saved && <span className="text-xs text-emerald-300">已保存并重新连接</span>}
            </div>
            <p className="text-[11px] leading-relaxed text-slate-500">
              令牌仅用于桌面端访问本机执行器 API。服务器模式建议通过安全隧道或 VPN
              连接，不要将执行器端口直接暴露到公网。
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <CardTitle>风控限额</CardTitle>
              <CardDescription>来自执行器配置，只读</CardDescription>
            </div>
            {riskLimits.isError && <Badge tone="danger">未连接</Badge>}
          </CardHeader>
          <CardContent>
            {riskLimits.data ? (
              <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
                <LimitItem label="单笔名义上限" value={`$${Number(riskLimits.data.max_order_notional_usd).toLocaleString()}`} />
                <LimitItem label="总敞口上限" value={`$${Number(riskLimits.data.max_total_notional_usd).toLocaleString()}`} />
                <LimitItem label="最大杠杆" value={`${Number(riskLimits.data.max_leverage)}x`} />
                <LimitItem label="最大滑点" value={`${Number(riskLimits.data.max_slippage_bps)} bps`} />
                <LimitItem label="行情时效" value={`${riskLimits.data.max_quote_age_secs} 秒`} />
                <LimitItem label="最大腿数" value={String(riskLimits.data.max_legs)} />
                <LimitItem label="允许交易对" value={riskLimits.data.allowed_symbols.join(" / ") || "不限"} />
                <LimitItem label="允许交易所" value={riskLimits.data.allowed_venues.join(" / ") || "不限"} />
              </dl>
            ) : (
              <p className="py-8 text-center text-xs text-slate-500">
                无法读取风控配置，请确认执行器在线。
              </p>
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>安全提示</CardTitle>
            <CardDescription>自托管模式下的默认约定</CardDescription>
          </div>
        </CardHeader>
        <CardContent>
          <ul className="list-disc space-y-1.5 pl-5 text-xs leading-relaxed text-slate-400">
            <li>交易所 API Key 只保存在执行器所在设备的系统密钥库，不进入桌面前端。</li>
            <li>API Key 应关闭提现与资金划转权限，并在交易所侧配置 IP 白名单。</li>
            <li>所有开仓指令都必须通过执行器风控，Telegram 与策略插件无法绕过。</li>
            <li>自动更新只接受签名制品；升级前备份数据库并暂停新开仓。</li>
          </ul>
        </CardContent>
      </Card>
    </div>
  );
}

function LimitItem({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[11px] text-slate-500">{label}</dt>
      <dd className="mt-0.5 font-mono text-xs text-slate-200">{value}</dd>
    </div>
  );
}
