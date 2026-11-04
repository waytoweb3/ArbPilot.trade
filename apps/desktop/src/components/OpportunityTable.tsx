import { formatBps, formatPercent, formatTime, formatUsd, opportunityKindLabel } from "../lib/format";
import type { Opportunity } from "../lib/types";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";

interface OpportunityTableProps {
  opportunities: Opportunity[];
  selectedId?: string;
  onSelect?: (opportunity: Opportunity) => void;
}

export function OpportunityTable({ opportunities, selectedId, onSelect }: OpportunityTableProps) {
  if (opportunities.length === 0) {
    return (
      <div className="px-4 py-10 text-center text-sm text-slate-500">
        暂无可用机会。执行器会持续扫描价差，机会出现时会显示在这里。
      </div>
    );
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[900px] border-collapse text-sm">
        <thead>
          <tr className="text-left text-[11px] tracking-wide text-slate-500 uppercase">
            <th className="px-4 py-2 font-medium">交易对</th>
            <th className="px-4 py-2 font-medium">类型</th>
            <th className="px-4 py-2 font-medium">多头 / 空头</th>
            <th className="px-4 py-2 font-medium text-right">毛价差</th>
            <th className="px-4 py-2 font-medium text-right">费用</th>
            <th className="px-4 py-2 font-medium text-right">资金费率</th>
            <th className="px-4 py-2 font-medium text-right">净收益</th>
            <th className="px-4 py-2 font-medium text-right">可执行额度</th>
            <th className="px-4 py-2 font-medium text-right">观测时间</th>
          </tr>
        </thead>
        <tbody>
          {opportunities.map((opportunity) => {
            const selected = opportunity.id === selectedId;
            return (
              <tr
                key={opportunity.id}
                onClick={() => onSelect?.(opportunity)}
                className={cn(
                  "border-t border-ink-700/50 transition-colors",
                  onSelect && "cursor-pointer",
                  selected ? "bg-emerald-400/5" : "hover:bg-ink-800/40",
                )}
              >
                <td className="px-4 py-2.5 font-medium text-slate-100">{opportunity.symbol}</td>
                <td className="px-4 py-2.5 text-slate-400">
                  {opportunityKindLabel[opportunity.kind] ?? opportunity.kind}
                </td>
                <td className="px-4 py-2.5 font-mono text-xs">
                  <span className="text-emerald-300">{opportunity.long_venue}</span>
                  <span className="px-1 text-slate-600">/</span>
                  <span className="text-rose-300">{opportunity.short_venue}</span>
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">
                  {formatBps(opportunity.gross_spread_bps)}
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-500">
                  -{formatBps(opportunity.estimated_fee_bps).replace("+", "")}
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-400">
                  {opportunity.funding_rate ? formatPercent(opportunity.funding_rate) : "—"}
                </td>
                <td className="px-4 py-2.5 text-right">
                  <Badge tone={Number(opportunity.net_edge_bps) > 0 ? "success" : "danger"}>
                    {formatBps(opportunity.net_edge_bps)}
                  </Badge>
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-400">
                  {formatUsd(opportunity.max_notional_usd, 0)}
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-500">
                  {formatTime(opportunity.observed_at)}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
