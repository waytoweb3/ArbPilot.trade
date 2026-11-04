import { useState } from "react";

import { OpportunityTable } from "../components/OpportunityTable";
import { TradePanel } from "../components/TradePanel";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { useOpportunities } from "../hooks/useApi";
import type { Opportunity } from "../lib/types";
import { WifiOff } from "lucide-react";

export default function OpportunitiesPage() {
  const opportunities = useOpportunities();
  const [selected, setSelected] = useState<Opportunity | null>(null);

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-semibold text-white">套利机会</h1>
        <p className="mt-1 text-sm text-slate-500">
          资金费率与跨所价差扫描结果。点击任意一行填入右侧下单面板。
        </p>
      </header>

      {opportunities.isError && (
        <div className="flex items-center gap-2 rounded-lg border border-rose-400/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-300">
          <WifiOff size={14} />
          无法获取机会列表，请确认执行器已启动且访问令牌正确。
        </div>
      )}

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
        <Card className="min-w-0">
          <CardHeader>
            <div>
              <CardTitle>机会列表</CardTitle>
              <CardDescription>
                共 {opportunities.data?.length ?? 0} 条 · 每 2 秒刷新
              </CardDescription>
            </div>
          </CardHeader>
          <CardContent className="p-0">
            <OpportunityTable
              opportunities={opportunities.data ?? []}
              selectedId={selected?.id}
              onSelect={setSelected}
            />
          </CardContent>
        </Card>

        <TradePanel opportunity={selected} />
      </div>
    </div>
  );
}
