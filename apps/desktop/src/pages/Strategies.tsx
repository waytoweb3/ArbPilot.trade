import { Bot, Pause, Play } from "lucide-react";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useStrategies, useUpdateStrategy } from "../hooks/useApi";
import { formatDateTime, opportunityKindLabel, strategyStatusLabel } from "../lib/format";
import type { StrategyStatus } from "../lib/types";

const statusTone: Record<StrategyStatus, "success" | "warning" | "neutral" | "danger"> = {
  running: "success",
  paused: "warning",
  draft: "neutral",
  stopped: "danger",
};

export default function StrategiesPage() {
  const strategies = useStrategies();
  const updateStrategy = useUpdateStrategy();

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-white">策略</h1>
        <p className="mt-1 text-sm text-slate-500">
          策略只输出订单意图，无法直接访问密钥或绕过执行器风控。
        </p>
      </header>

      <div className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-3">
        {(strategies.data ?? []).map((strategy) => {
          const isRunning = strategy.status === "running";
          return (
            <Card key={strategy.id}>
              <CardHeader>
                <div className="flex items-start gap-2">
                  <div className="grid h-8 w-8 place-items-center rounded-lg bg-ink-800 text-slate-300">
                    <Bot size={16} />
                  </div>
                  <div>
                    <CardTitle>{strategy.name}</CardTitle>
                    <CardDescription>
                      {opportunityKindLabel[strategy.kind] ?? strategy.kind}
                    </CardDescription>
                  </div>
                </div>
                <Badge tone={statusTone[strategy.status]}>
                  {strategyStatusLabel[strategy.status]}
                </Badge>
              </CardHeader>
              <CardContent className="space-y-3 text-xs text-slate-400">
                <div className="grid grid-cols-2 gap-2">
                  <span>交易所：{strategy.venues.join(" / ") || "—"}</span>
                  <span>交易对：{strategy.symbols.join(" / ") || "—"}</span>
                  <span className="col-span-2">
                    最近心跳：{formatDateTime(strategy.last_heartbeat_at)}
                  </span>
                </div>
                <Button
                  size="sm"
                  variant={isRunning ? "secondary" : "primary"}
                  disabled={updateStrategy.isPending}
                  onClick={() =>
                    updateStrategy.mutate({
                      id: strategy.id,
                      status: isRunning ? "paused" : "running",
                    })
                  }
                >
                  {isRunning ? <Pause size={13} /> : <Play size={13} />}
                  {isRunning ? "暂停策略" : "启动策略"}
                </Button>
              </CardContent>
            </Card>
          );
        })}
      </div>

      {(strategies.data ?? []).length === 0 && (
        <Card>
          <CardContent className="py-12 text-center text-sm text-slate-500">
            暂无策略。自定义策略沙箱（WASM + Wasmtime）将在 Phase 3 接入。
          </CardContent>
        </Card>
      )}
    </div>
  );
}
