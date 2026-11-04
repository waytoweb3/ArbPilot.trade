import { Activity, Database, Gauge, ShieldCheck } from "lucide-react";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { OpportunityTable } from "../components/OpportunityTable";
import { PriceChart } from "../components/PriceChart";
import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useHealth, useOpportunities, useOrders } from "../hooks/useApi";
import { formatTime, orderStatusLabel, orderStatusTone } from "../lib/format";

export default function OverviewPage() {
  const health = useHealth();
  const opportunities = useOpportunities();
  const orders = useOrders();

  const online = Boolean(health.data) && !health.isError;
  const topOpportunities = (opportunities.data ?? []).slice(0, 5);
  const recentOrders = (orders.data ?? []).slice(0, 5);

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-white">总览</h1>
        <p className="mt-1 text-sm text-slate-500">
          执行器状态、套利机会与最近订单。模拟盘数据仅用于流程验证。
        </p>
      </header>

      <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard
          title="执行器"
          value={online ? "在线" : "离线"}
          hint={online ? `版本 ${health.data?.version}` : "启动 apps/executor 后自动连接"}
          icon={<Activity size={16} />}
          tone={online ? "success" : "danger"}
        />
        <StatCard
          title="运行模式"
          value={health.data?.mode === "live" ? "真实交易" : "模拟盘"}
          hint="真实交易需显式开启并单独构建适配器"
          icon={<Gauge size={16} />}
          tone="info"
        />
        <StatCard
          title="风控状态"
          value={health.data?.paused ? "已暂停开仓" : "正常"}
          hint="暂停期间仍可查看仓位与撤单"
          icon={<ShieldCheck size={16} />}
          tone={health.data?.paused ? "warning" : "success"}
        />
        <StatCard
          title="数据库"
          value={health.data?.database_connected ? "PostgreSQL" : "内存模式"}
          hint={health.data?.database_connected ? "订单已落库" : "未配置 DATABASE_URL"}
          icon={<Database size={16} />}
          tone={health.data?.database_connected ? "success" : "neutral"}
        />
      </section>

      <section className="grid gap-4 xl:grid-cols-[1.4fr_1fr]">
        <Card>
          <CardHeader>
            <div>
              <CardTitle>BTCUSDT 价差走势</CardTitle>
              <CardDescription>模拟 K 线，接入真实行情后替换数据源</CardDescription>
            </div>
            <Badge tone="neutral">5m</Badge>
          </CardHeader>
          <CardContent className="pt-2">
            <PriceChart symbol="BTCUSDT" />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div>
              <CardTitle>最近订单</CardTitle>
              <CardDescription>全部经执行器风控与状态机</CardDescription>
            </div>
            <Link to="/positions" className="text-xs text-emerald-300 hover:text-emerald-200">
              查看全部
            </Link>
          </CardHeader>
          <CardContent className="space-y-3">
            {recentOrders.length === 0 && (
              <p className="py-8 text-center text-xs text-slate-500">
                暂无订单。前往「套利机会」选择一条机会试试模拟开仓。
              </p>
            )}
            {recentOrders.map((order) => (
              <div
                key={order.id}
                className="flex items-center justify-between gap-3 rounded-lg border border-ink-700/60 px-3 py-2"
              >
                <div className="min-w-0">
                  <p className="text-sm text-slate-200">
                    {order.intent.legs[0]?.symbol ?? "—"}
                    <span className="ml-2 text-xs text-slate-500">
                      {order.intent.legs
                        .map((leg) => `${leg.venue}:${leg.side === "buy" ? "多" : "空"}`)
                        .join(" / ")}
                    </span>
                  </p>
                  <p className="mt-0.5 font-mono text-[11px] text-slate-600">
                    {formatTime(order.created_at)} · {order.id.slice(0, 8)}
                  </p>
                </div>
                <Badge tone={orderStatusTone[order.status]}>
                  {orderStatusLabel[order.status]}
                </Badge>
              </div>
            ))}
          </CardContent>
        </Card>
      </section>

      <Card>
        <CardHeader>
          <div>
            <CardTitle>实时套利机会</CardTitle>
            <CardDescription>净收益已扣除手续费与估算滑点</CardDescription>
          </div>
          <Link to="/opportunities" className="text-xs text-emerald-300 hover:text-emerald-200">
            去下单
          </Link>
        </CardHeader>
        <CardContent className="p-0">
          <OpportunityTable opportunities={topOpportunities} />
        </CardContent>
      </Card>
    </div>
  );
}

function StatCard({
  title,
  value,
  hint,
  icon,
  tone,
}: {
  title: string;
  value: string;
  hint: string;
  icon: ReactNode;
  tone: "success" | "warning" | "danger" | "neutral" | "info";
}) {
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between">
        <span className="text-xs text-slate-500">{title}</span>
        <Badge tone={tone}>{icon}</Badge>
      </div>
      <p className="mt-2 text-lg font-semibold text-white">{value}</p>
      <p className="mt-1 text-[11px] text-slate-500">{hint}</p>
    </Card>
  );
}
