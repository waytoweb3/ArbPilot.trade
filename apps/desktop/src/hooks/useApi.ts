import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../lib/api";
import type {
  CreateConditionalRequest,
  CreateOrderRequest,
  CreatePerformanceRequest,
  CreatePublicationRequest,
  ImportAccountRequest,
  ListingSettings,
  Position,
  SettlementStatus,
  SkillSettings,
  StrategyDefinition,
  StrategyExecutionMode,
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

/** 永续 K 线：执行器按需拉取，桌面端定时刷新；30 秒周期由实时盘口聚合，刷新更频繁。 */
export function useCandles(symbol: string, interval: string, limit: number, venue = "binance") {
  return useQuery({
    queryKey: ["candles", venue, symbol, interval, limit],
    queryFn: () => api.candles(symbol, interval, limit, venue),
    refetchInterval: interval === "30s" ? 10_000 : 30_000,
    enabled: Boolean(symbol) && Boolean(venue),
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

/** 多档盘口深度：跟随交易页刷新，用于展示买卖盘挂单分布。 */
export function useDepth(venue: string, symbol: string, limit = 12) {
  return useQuery({
    queryKey: ["depth", venue, symbol, limit],
    queryFn: () => api.depth(venue, symbol, limit),
    refetchInterval: 3_000,
    enabled: Boolean(venue) && Boolean(symbol),
  });
}

/** 最小下单限制：交易所与交易对的合约规格变化不频繁，低频刷新即可。 */
export function useMinOrder(venue: string, symbol: string) {
  return useQuery({
    queryKey: ["min-order", venue, symbol],
    queryFn: () => api.minOrder(venue, symbol),
    staleTime: 30_000,
    enabled: Boolean(venue) && Boolean(symbol),
  });
}

/** 条件单列表：等待触发与已结束的记录都在这里。 */
export function useConditionalOrders() {
  return useQuery({
    queryKey: ["conditional-orders"],
    queryFn: api.conditionalOrders,
    refetchInterval: 5_000,
  });
}

export function useCreateConditionalOrder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateConditionalRequest) => api.createConditionalOrder(body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["conditional-orders"] }),
  });
}

export function useCancelConditionalOrder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.cancelConditionalOrder(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["conditional-orders"] }),
  });
}

/** 整组平仓：成交后刷新仓位与订单列表。 */
export function useClosePositionGroup() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (groupId: string) => {
      const result = await api.closePositionGroup(groupId);
      if (result.order.status === "rejected") {
        throw new Error(result.order.note ?? "整组平仓被风控拒绝");
      }
      return result;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["positions"] });
      queryClient.invalidateQueries({ queryKey: ["orders"] });
    },
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

/** 一键平仓：把给定仓位先按组整体平掉，再平掉未分组的单腿仓位。 */
export function useCloseAllPositions() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (positions: Position[]) => {
      const groups = new Set<string>();
      for (const position of positions) {
        if (position.group_id) {
          groups.add(position.group_id);
        }
      }
      for (const groupId of groups) {
        const result = await api.closePositionGroup(groupId);
        if (result.order.status === "rejected") {
          throw new Error(result.order.note ?? "整组平仓被风控拒绝");
        }
      }
      for (const position of positions) {
        if (position.group_id) {
          continue;
        }
        const result = await api.closePosition(position.id);
        if (result.order.status === "rejected") {
          throw new Error(result.order.note ?? "平仓被风控拒绝");
        }
      }
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

/** 上币公告跟随的单腿交易设置：按策略实例读取监听来源、执行交易所、金额与止盈止损。 */
export function useListingSettings(strategyId: string) {
  return useQuery({
    queryKey: ["listing-settings", strategyId],
    queryFn: () => api.listingSettings(strategyId),
    staleTime: 30_000,
  });
}

/** 保存上币设置，执行器保存后立即对后续公告生效。 */
export function useUpdateListingSettings() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (settings: ListingSettings & { strategy_id: string }) =>
      api.updateListingSettings(settings),
    onSuccess: (data) => {
      queryClient.setQueryData(["listing-settings", data.strategy_id], data);
      // 规则变更会刷新运行实例里的规则快照。
      queryClient.invalidateQueries({ queryKey: ["strategy-runs"] });
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
    },
  });
}

/** 技能策略：按策略实例读取 SKILL.md 六章参数。 */
export function useSkillSettings(strategyId: string) {
  return useQuery({
    queryKey: ["skill-settings", strategyId],
    queryFn: () => api.skillSettings(strategyId),
    staleTime: 30_000,
  });
}

/** 保存技能参数，执行器保存后立即对后续公告生效。 */
export function useUpdateSkillSettings() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (settings: SkillSettings & { strategy_id: string }) =>
      api.updateSkillSettings(settings),
    onSuccess: (data) => {
      queryClient.setQueryData(["skill-settings", data.strategy_id], data);
      queryClient.invalidateQueries({ queryKey: ["strategy-runs"] });
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
    },
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

/** 交易所在指定合约上当前设置的杠杆，1 分钟刷新一次。 */
export function useAccountLeverage(venue: string, symbol: string, enabled = true) {
  return useQuery({
    queryKey: ["account-leverage", venue, symbol],
    queryFn: () => api.accountLeverage(venue, symbol),
    refetchInterval: 60_000,
    enabled: enabled && Boolean(venue) && Boolean(symbol),
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

export function useClearAccounts() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.clearAccounts(),
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

/** 切换下单模式：真实下单会提交交易所，模拟下单只记录成交事件。 */
export function useUpdateStrategyExecutionMode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, mode }: { id: string; mode: StrategyExecutionMode }) =>
      api.updateStrategyExecutionMode(id, mode),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategies"] });
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

/** 清空历史运行实例，运行中的实例保留。 */
export function useClearStrategyRuns(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.clearStrategyRuns(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["strategy-runs", id] });
    },
  });
}

/** 数据库中的内置策略模板；启动时由执行器补齐。 */
export function useStrategyTemplates() {
  return useQuery({
    queryKey: ["strategy-templates"],
    queryFn: () => api.strategyTemplates(),
  });
}

/** 用模板创建一条草稿策略：复制定义并绑定技能参数运行时。 */
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

