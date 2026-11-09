import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../lib/api";
import type {
  CreateOrderRequest,
  CreatePerformanceRequest,
  CreatePublicationRequest,
  ImportAccountRequest,
  SandboxDryRunRequest,
  SettlementStatus,
  StrategyDefinition,
  StrategyStatus,
  UpdatePublicationRequest,
} from "../lib/types";

export function useHealth() {
  return useQuery({
    queryKey: ["health"],
    queryFn: api.health,
    refetchInterval: 5_000,
  });
}

export function useOpportunities() {
  return useQuery({
    queryKey: ["opportunities"],
    queryFn: api.opportunities,
    refetchInterval: 2_000,
  });
}

/** 交割合约基差排行，与资金费率机会同频刷新。 */
export function useBasisOpportunities() {
  return useQuery({
    queryKey: ["opportunities", "basis"],
    queryFn: api.basisOpportunities,
    refetchInterval: 3_000,
  });
}

/** 现货 K 线：执行器按需拉取，桌面端定时刷新。 */
export function useCandles(symbol: string, interval: string, limit: number, venue = "binance-spot") {
  return useQuery({
    queryKey: ["candles", venue, symbol, interval, limit],
    queryFn: () => api.candles(symbol, interval, limit, venue),
    refetchInterval: 30_000,
  });
}

export function usePositions() {
  return useQuery({
    queryKey: ["positions"],
    queryFn: api.positions,
    // 实时价格与浮盈由执行器 WebSocket 推送（positions_updated）写入缓存；
    // 这里只做低频兜底，用于断线或漏事件时重新对齐。
    refetchInterval: 15_000,
  });
}

export function useOrders() {
  return useQuery({
    queryKey: ["orders"],
    queryFn: api.orders,
    refetchInterval: 5_000,
  });
}

/** 一键平仓：成交后立即刷新仓位与订单列表。 */
export function useClosePosition() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      const result = await api.closePosition(id);
      // 风控拒绝会以 422 + 订单体返回，这里转成错误让界面展示拒绝原因。
      if (result.order.status === "rejected") {
        throw new Error(result.order.note ?? "平仓被风控拒绝");
      }
      return result;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["positions"] });
      queryClient.invalidateQueries({ queryKey: ["orders"] });
    },
  });
}

export function useStrategies() {
  return useQuery({
    queryKey: ["strategies"],
    queryFn: api.strategies,
    refetchInterval: 10_000,
  });
}

export function useRiskLimits() {
  return useQuery({
    queryKey: ["risk-limits"],
    queryFn: api.riskLimits,
    staleTime: Number.POSITIVE_INFINITY,
  });
}

/** 修改风控限额，保存后执行器立即按新限额校验后续订单。 */
export function useUpdateRiskLimits() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: api.updateRiskLimits,
    onSuccess: (data) => queryClient.setQueryData(["risk-limits"], data),
  });
}

/** 策略范围可选的交易所与交易对，由执行器数据库配置；配置变更后自动刷新。 */
export function useMarketConfig() {
  return useQuery({
    queryKey: ["market-config"],
    queryFn: api.marketConfig,
    staleTime: 60_000,
    refetchInterval: 60_000,
  });
}

/** 通知配置：新闻推送与资金费率套利提醒的开关、阈值与渠道。 */
export function useNotificationSettings() {
  return useQuery({
    queryKey: ["notification-settings"],
    queryFn: api.notificationSettings,
    staleTime: 30_000,
  });
}

/** 保存通知配置，执行器保存后立即对后续推送生效。 */
export function useUpdateNotificationSettings() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: api.updateNotificationSettings,
    onSuccess: (data) => queryClient.setQueryData(["notification-settings"], data),
  });
}

/** 各交易所资金费率采集器状态：入库条数、最近一次采集结果与各交易对最新费率。 */
export function useFundingStatus() {
  return useQuery({
    queryKey: ["funding-status"],
    queryFn: api.fundingStatus,
    // 缩短刷新间隔，费率变化能在页面上高亮闪烁。
    refetchInterval: 5_000,
  });
}

/** 已入库的资金费率历史，按结算时间倒序。未选定交易对时不会发起请求。 */
export function useFundingHistory(symbol: string | undefined, limit: number, venue: string) {
  return useQuery({
    queryKey: ["funding-history", venue, symbol ?? "all", limit],
    queryFn: () => api.fundingHistory(symbol, limit, venue),
    refetchInterval: 30_000,
    enabled: Boolean(symbol),
  });
}

/** 上币公告：Binance / Upbit / OKX / Bybit / Bitget，桌面端每 30 秒刷新一次。 */
export function useNews(limit = 200) {
  return useQuery({
    queryKey: ["news", limit],
    queryFn: () => api.news(limit),
    refetchInterval: 30_000,
  });
}

/** 电报机器人状态与已绑定账号。 */
export function useTelegramStatus() {
  return useQuery({
    queryKey: ["telegram"],
    queryFn: api.telegramStatus,
    refetchInterval: 15_000,
  });
}

/** 生成一次性配对码。 */
export function useCreateTelegramPairing() {
  return useMutation({ mutationFn: api.createTelegramPairing });
}

/** 撤销某个 Telegram 账号的绑定。 */
export function useRevokeTelegramBinding() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (userId: number) => api.revokeTelegramBinding(userId),
    onSuccess: (data) => queryClient.setQueryData(["telegram"], data),
  });
}

/** 时间窗口内全部交易对的累计资金费率，一次请求替代逐币历史查询。 */
export function useFundingSummary(secs: number | null) {
  return useQuery({
    queryKey: ["funding-summary", secs ?? 0],
    queryFn: () => api.fundingSummary(secs ?? 0),
    refetchInterval: 60_000,
    enabled: secs !== null,
  });
}

/** 已导入的交易所账户与当前权益，15 秒轮询一次。 */
export function useAccounts(enabled = true) {
  return useQuery({
    queryKey: ["accounts"],
    queryFn: api.accounts,
    refetchInterval: 15_000,
    enabled,
  });
}

/** 账户权益历史曲线，按所选时间范围聚合。 */
export function useAccountHistory(hours: number, enabled = true) {
  return useQuery({
    queryKey: ["account-history", hours],
    queryFn: () => api.accountHistory(hours),
    refetchInterval: 60_000,
    enabled,
  });
}

export function useImportAccount() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: ImportAccountRequest) => api.importAccount(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["accounts"] });
      queryClient.invalidateQueries({ queryKey: ["account-history"] });
    },
  });
}

export function useRemoveAccount() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.removeAccount(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["accounts"] });
      queryClient.invalidateQueries({ queryKey: ["account-history"] });
    },
  });
}

export function useRefreshAccount() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.refreshAccount(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["accounts"] });
      queryClient.invalidateQueries({ queryKey: ["account-history"] });
    },
  });
}

export function usePauseExecutor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (reason?: string) => api.pause(reason),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["health"] }),
  });
}

export function useResumeExecutor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.resume(),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["health"] }),
  });
}

export function useCreateOrder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ body, idempotencyKey }: { body: CreateOrderRequest; idempotencyKey: string }) =>
      api.createOrder(body, idempotencyKey),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["orders"] });
      queryClient.invalidateQueries({ queryKey: ["positions"] });
      queryClient.invalidateQueries({ queryKey: ["health"] });
    },
  });
}

export function useUpdateStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, status }: { id: string; status: StrategyStatus }) =>
      api.updateStrategy(id, status),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      // 启停会新建或关闭运行实例。
      queryClient.invalidateQueries({ queryKey: ["strategy-runs"] });
    },
  });
}

/** 策略版本历史，用于复现与回滚。 */
export function useStrategyVersions(id: string | null) {
  return useQuery({
    queryKey: ["strategy-versions", id],
    queryFn: () => api.strategyVersions(id ?? ""),
    enabled: Boolean(id),
  });
}

export function useStrategyRuns(id: string | null, enabled = true) {
  return useQuery({
    queryKey: ["strategy-runs", id],
    queryFn: () => api.strategyRuns(id ?? ""),
    enabled: Boolean(id) && enabled,
    refetchInterval: 15_000,
  });
}

/** 数据库中的内置策略模板；启动时由执行器补齐并编译。 */
export function useStrategyTemplates() {
  return useQuery({
    queryKey: ["strategy-templates"],
    queryFn: () => api.strategyTemplates(),
  });
}

/** 用模板创建一条草稿策略：复制定义并绑定模板编译出的沙箱模块。 */
export function useInstantiateTemplate() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ key, name }: { key: string; name?: string }) =>
      api.instantiateTemplate(key, name),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["strategies"] }),
  });
}

export function useCreateStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      definition?: StrategyDefinition;
      name?: string;
      source?: "manual" | "ai";
    }) => api.createStrategy(body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["strategies"] }),
  });
}

export function useUpdateStrategyDefinition() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, definition }: { id: string; definition: StrategyDefinition }) =>
      api.updateStrategyDefinition(id, definition),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
    },
  });
}

/** 修改策略的应用范围（交易所与代币），会产生新版本。 */
export function useUpdateStrategyScope() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      ...body
    }: {
      id: string;
      spot_venue: string;
      perp_venue: string;
      symbols: string[];
      event_sources?: string[];
      event_keywords?: string[];
      event_window_secs?: number;
    }) => api.updateStrategyScope(id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-code"] });
    },
  });
}

export function useValidateStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.validateStrategy(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["strategies"] }),
  });
}

export function useBacktestStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, days }: { id: string; days: number }) =>
      api.backtestStrategy(id, days),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
    },
  });
}

export function useRollbackStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, version }: { id: string; version: number }) =>
      api.rollbackStrategy(id, version),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
    },
  });
}

export function useDeleteStrategy() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.deleteStrategy(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
    },
  });
}

// ---------------------------------------------------------------------------
// 策略代码：AI 生成、查看、修改、编译与部署
// ---------------------------------------------------------------------------

export function useStrategyCode(id: string | null) {
  return useQuery({
    queryKey: ["strategy-code", id],
    queryFn: () => api.strategyCode(id ?? ""),
    enabled: Boolean(id),
  });
}

export function useSaveStrategyCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, source }: { id: string; source: string }) =>
      api.saveStrategyCode(id, { source }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategy-code"] });
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
    },
  });
}

export function useGenerateStrategyCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      prompt,
      history,
    }: {
      id: string;
      prompt: string;
      history?: { role: "user" | "assistant"; content: string }[];
    }) => api.generateStrategyCode(id, prompt, history),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategy-code"] });
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
    },
  });
}

export function useCompileStrategyCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.compileStrategyCode(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategy-code"] });
      queryClient.invalidateQueries({ queryKey: ["sandbox"] });
    },
  });
}

export function useDeployStrategyCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.deployStrategyCode(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategy-code"] });
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["sandbox"] });
    },
  });
}

// ---------------------------------------------------------------------------
// 策略市场
// ---------------------------------------------------------------------------

export function useMarketAuthors() {
  return useQuery({
    queryKey: ["market", "authors"],
    queryFn: api.marketAuthors,
    refetchInterval: 30_000,
  });
}

export function useCreateMarketAuthor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      display_name: string;
      bio?: string;
      payout_account?: string;
    }) => api.createMarketAuthor(body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

export function useUpdateMarketAuthor() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      body,
    }: {
      id: string;
      body: { display_name?: string; bio?: string; payout_account?: string };
    }) => api.updateMarketAuthor(id, body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

/** 发布列表；includeHidden 时包含私有草稿与已撤回记录。 */
export function usePublications(includeHidden: boolean) {
  return useQuery({
    queryKey: ["market", "publications", includeHidden],
    queryFn: () => api.publications({ includeHidden }),
    refetchInterval: 30_000,
  });
}

export function usePublicationPerformance(id: string | null) {
  return useQuery({
    queryKey: ["market", "performance", id],
    queryFn: () => api.publicationPerformance(id ?? ""),
    enabled: Boolean(id),
  });
}

export function useCreatePublication() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreatePublicationRequest) => api.createPublication(body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

export function useUpdatePublication() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdatePublicationRequest }) =>
      api.updatePublication(id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["market"] });
      queryClient.invalidateQueries({ queryKey: ["market", "leaderboard"] });
      // 发布 / 撤回会切换策略的运行位置（平台托管 ↔ 自己的执行器）。
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-runs"] });
    },
  });
}

export function useCreatePerformance() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: CreatePerformanceRequest }) =>
      api.createPerformance(id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["market"] });
      queryClient.invalidateQueries({ queryKey: ["market", "leaderboard"] });
    },
  });
}

export function useLeaderboard() {
  return useQuery({
    queryKey: ["market", "leaderboard"],
    queryFn: api.leaderboard,
    refetchInterval: 30_000,
  });
}

export function useSubscriptions(subscriberId: string) {
  return useQuery({
    queryKey: ["market", "subscriptions", subscriberId],
    queryFn: () => api.subscriptions(subscriberId),
    enabled: Boolean(subscriberId),
    // 支付 webhook 回写订阅状态后，页面无需手动刷新即可看到授权生效。
    refetchInterval: 30_000,
  });
}

export function useSubscribe() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      publicationId,
      subscriberId,
    }: {
      publicationId: string;
      subscriberId: string;
    }) => api.subscribe(publicationId, subscriberId),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

export function useCancelSubscription() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.cancelSubscription(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

/** 已订阅策略导入到自己的执行器：导入后立即出现在策略列表里。 */
export function useImportPublication() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      publicationId,
      subscriberId,
    }: {
      publicationId: string;
      subscriberId: string;
    }) => api.importPublication(publicationId, subscriberId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
      queryClient.invalidateQueries({ queryKey: ["strategy-versions"] });
    },
  });
}

export function useSettlements(authorId?: string) {
  return useQuery({
    queryKey: ["market", "settlements", authorId ?? "all"],
    queryFn: () => api.settlements(authorId),
    refetchInterval: 60_000,
  });
}

export function useCreateSettlement() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ authorId, period }: { authorId: string; period?: string }) =>
      api.createSettlement(authorId, period),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

export function useUpdateSettlement() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      status,
      note,
    }: {
      id: string;
      status: SettlementStatus;
      note?: string;
    }) => api.updateSettlement(id, status, note),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["market"] }),
  });
}

// ---------------------------------------------------------------------------
// 沙箱试运行
// ---------------------------------------------------------------------------

export function useSandboxDryRun() {
  return useMutation({
    mutationFn: ({ artifactId, body }: { artifactId: string; body: SandboxDryRunRequest }) =>
      api.sandboxDryRun(artifactId, body),
  });
}
