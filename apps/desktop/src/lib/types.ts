/** 与 Rust 端 rust_decimal 序列化保持一致：可能是数字或字符串。 */
export type Decimal = number | string;

export type OpportunityKind =
  | "funding_rate"
  | "perp_perp_funding"
  | "delivery_basis"
  | "cex_cex_spread"
  | "cex_dex_spread";

/** 策略类型：内置资金费率套利，或由沙箱代码定义的自定义策略。 */
export type StrategyKind = "funding_rate" | "custom";

export type VenueKind = "cex" | "dex";

export type Side = "buy" | "sell";

export type OrderOrigin = "desktop" | "telegram" | "strategy";

export type LegStatus =
  | "pending"
  | "submitted"
  | "partially_filled"
  | "filled"
  | "cancelled"
  | "rejected";

export type OrderStatus =
  | "pending_risk"
  | "rejected"
  | "submitting"
  | "partially_filled"
  | "filled"
  | "unwinding"
  | "cancelled"
  | "failed";

export type StrategyStatus = "draft" | "running" | "paused" | "stopped";

/** 策略来源：内置、用户手写或 AI 生成，用于审计与复现。 */
export type StrategySource = "built_in" | "manual" | "ai";

export type Instrument = "spot" | "perp";

export type ValidationSeverity = "error" | "warning";

export type StrategyEventLevel = "info" | "warning" | "error";

export interface StrategyUniverse {
  venues: string[];
  symbols: string[];
  instruments: Instrument[];
}

/** 数据库中配置的交易所：现货腿与永续腿分开维护。 */
export interface MarketVenueConfig {
  venue: string;
  segment: "spot" | "perp";
  display_name: string;
}

/** 策略范围下拉的数据来源，全部由执行器从数据库读取。 */
export interface MarketConfig {
  venues: MarketVenueConfig[];
  /** 按交易所分组的交易对；现货与永续共用同一份清单。 */
  symbols: Record<string, string[]>;
}

/** 消息通知配置：新闻推送与资金费率套利提醒的开关、阈值与渠道。 */
export interface NotificationSettings {
  news_enabled: boolean;
  /** 参与推送的新闻来源；空列表表示全部来源。 */
  news_sources: string[];
  funding_enabled: boolean;
  /** 触发提醒的最小年化资金费率（百分比数值，例如 10 表示 10%）。 */
  funding_min_annualized_pct: Decimal;
  /** 同一交易对再次提醒的最小间隔（秒）。 */
  funding_cooldown_secs: number;
  telegram_enabled: boolean;
  desktop_enabled: boolean;
}

export interface EntryRules {
  /** 资金费率下限（小数，如 0.0003）。 */
  funding_spread_min: Decimal;
  /** 扣费后单期净收益下限（小数）。 */
  net_edge_min: Decimal;
}

export interface ExitRules {
  /** 费率低于该值时平仓（小数）。 */
  spread_below: Decimal;
  max_holding_hours: Decimal;
}

export interface StrategyRisk {
  max_notional_usd: Decimal;
  max_unhedged_seconds: number;
}

export interface ExecutionRules {
  leverage: Decimal;
  max_slippage_bps: Decimal;
  cooldown_secs: number;
}

export interface CostAssumptions {
  fee_bps_per_leg: Decimal;
  slippage_bps: Decimal;
}

/** 沙箱策略的数据窗口：主机据此拉取 K 线并计算指标。 */
export interface DataRules {
  timeframe: string;
  lookback: number;
  fast: number;
  slow: number;
  atr: number;
  channel: number;
  /** 事件来源白名单；为空表示接受全部来源。 */
  event_sources?: string[];
  /** 事件关键词：标题包含任一关键词才会进入沙箱；为空表示不按关键词过滤。 */
  event_keywords?: string[];
  /** 事件窗口（秒）：只注入最近这段时间内发布的公告。 */
  event_window_secs?: number;
}

/** 版本化、可校验的策略定义；回测与实盘共用同一份结构。 */
export interface StrategyDefinition {
  name: string;
  kind: StrategyKind;
  universe: StrategyUniverse;
  entry: EntryRules;
  exit: ExitRules;
  risk: StrategyRisk;
  execution: ExecutionRules;
  /** 沙箱策略的 K 线与指标窗口；内置套利不读取。 */
  data: DataRules;
  costs: CostAssumptions;
  notes: string | null;
}

export interface ValidationIssue {
  path: string;
  severity: ValidationSeverity;
  message: string;
}

export interface StrategyValidation {
  issues: ValidationIssue[];
  validated_at: string | null;
}

export type BacktestAssumption =
  | "zero_basis"
  | "mark_price_fills"
  | "round_trip_fees"
  | "round_trip_slippage"
  | "settled_funding_only"
  | "net_edge_per_settlement"
  | "wasm_bar_replay"
  | "bar_close_fills"
  | "limit_fill_on_touch"
  | "no_event_replay";

export interface BacktestEquityPoint {
  at: string;
  cumulative_pnl_usd: Decimal;
  symbol: string;
}

export interface BacktestReport {
  strategy_name: string;
  definition_version: number;
  generated_at: string;
  sample_start: string | null;
  sample_end: string | null;
  settlements: number;
  trades: number;
  wins: number;
  losses: number;
  gross_funding_usd: Decimal;
  fees_usd: Decimal;
  slippage_usd: Decimal;
  net_pnl_usd: Decimal;
  return_pct: Decimal;
  max_drawdown_pct: Decimal;
  in_sample_net_pnl_usd: Decimal;
  out_of_sample_net_pnl_usd: Decimal;
  assumptions: BacktestAssumption[];
  equity_curve: BacktestEquityPoint[];
}

export interface StrategyEvent {
  at: string;
  level: StrategyEventLevel;
  message: string;
  order_id: string | null;
}

export interface StrategyOpenPosition {
  symbol: string;
  /** 配对套利的现货腿；单腿持仓为空。 */
  long_venue?: string;
  /** 配对套利的永续腿；单腿持仓为空。 */
  short_venue?: string;
  /** 单腿持仓的交易所。 */
  venue?: string | null;
  /** 单腿持仓方向；配对套利为空。 */
  side?: "long" | "short" | null;
  /** 单腿持仓的开仓价。 */
  entry_price?: Decimal | null;
  notional_usd: Decimal;
  opened_at: string;
  entry_net_edge_bps: Decimal;
}

export interface StrategyRuntime {
  last_evaluated_at: string | null;
  last_signal_at: string | null;
  last_order_id: string | null;
  last_error: string | null;
  signals: number;
  orders: number;
  open: StrategyOpenPosition | null;
  events: StrategyEvent[];
  /** 当前运行实例；运行中才有值。 */
  run_id?: string | null;
}

/** 策略运行位置：未发布跑在用户自己的执行器，发布后由平台托管执行器运行。 */
export type StrategyRunScope = "platform" | "self_hosted";

/** 一次运行实例：策略在某个交易所组合上的运行。 */
export interface StrategyRun {
  id: string;
  strategy_id: string;
  strategy_version: number;
  scope: StrategyRunScope;
  status: StrategyStatus;
  long_venue: string;
  short_venue: string;
  symbols: string[];
  started_at: string;
  stopped_at: string | null;
  last_heartbeat_at: string | null;
}

/** 运行事件：信号、下单、错误等，按时间正序返回。 */
export interface StrategyRunEvent {
  run_id: string;
  strategy_id: string;
  level: StrategyEventLevel;
  message: string;
  order_id: string | null;
  created_at: string;
}

export interface StrategyRunsResponse {
  runs: StrategyRun[];
  events: StrategyRunEvent[];
}

export interface StrategyRecord {
  id: string;
  status: StrategyStatus;
  source: StrategySource;
  version: number;
  definition: StrategyDefinition;
  validation: StrategyValidation;
  last_backtest: BacktestReport | null;
  runtime: StrategyRuntime;
  /** 可选 WASM 沙箱绑定；存在时由模块替代内置入场/出场判断。 */
  sandbox?: SandboxBinding | null;
  /** 运行位置，由发布 / 撤回自动切换。 */
  run_scope?: StrategyRunScope;
  last_heartbeat_at: string | null;
  created_at: string;
  updated_at: string;
}

/** 数据库中内置的策略模板：实例化只复制定义并绑定同一个沙箱模块。 */
export interface StrategyTemplateView {
  key: string;
  name: string;
  description: string;
  definition: StrategyDefinition;
  artifact_id: string | null;
  updated_at: string;
}

/** 一次版本快照：定义、校验、回测与部署状态。 */
export interface StrategyVersion {
  strategy_id: string;
  version: number;
  status: StrategyStatus;
  source: StrategySource;
  definition: StrategyDefinition;
  validation: StrategyValidation;
  backtest: BacktestReport | null;
  saved_at: string;
}

export type StrategyCodeLanguage = "wat";
export type StrategyCodeOrigin = "ai" | "user" | "template";

/** 策略代码：以 WebAssembly Text 保存，可查看、修改、编译与部署。 */
export interface StrategyCode {
  strategy_id: string;
  language: StrategyCodeLanguage;
  source: string;
  origin: StrategyCodeOrigin;
  prompt: string | null;
  notes: string | null;
  /** 编译产物；源码变化后会被清空。 */
  artifact_id: string | null;
  artifact_sha256: string | null;
  deployed_at: string | null;
  updated_at: string;
}

export interface StrategyCodeView {
  code?: StrategyCode | null;
  /** 尚未生成代码时用于起步的可编译模板。 */
  template: string;
}

export interface CompileStrategyCodeResponse {
  artifact: SandboxArtifact;
  code: StrategyCode;
}

export interface DeployStrategyCodeResponse {
  code: StrategyCode;
  strategy: StrategyRecord;
}

export interface HealthResponse {
  status: "ok" | "degraded";
  version: string;
  paused: boolean;
  database_connected: boolean;
  uptime_secs: number;
  server_time: string;
}

export interface Opportunity {
  id: string;
  kind: OpportunityKind;
  symbol: string;
  long_venue: string;
  short_venue: string;
  gross_spread_bps: Decimal;
  estimated_fee_bps: Decimal;
  estimated_slippage_bps: Decimal;
  net_edge_bps: Decimal;
  max_notional_usd: Decimal;
  funding_rate: Decimal | null;
  /** 年化资金费率收益率（百分比），跨所比较一律用它。 */
  funding_annualized_pct: Decimal | null;
  /** 交割基差机会：交割合约代码，如 `BTCUSDT_251226`。 */
  delivery_contract?: string | null;
  /** 交割基差机会：交割合约到期时间。 */
  expiry?: string | null;
  /** 交割基差年化收益率（百分比，`10.95` 表示 10.95%/年）。 */
  annualized_basis_pct?: Decimal | null;
  observed_at: string;
  expires_at: string;
}

export interface OrderLegIntent {
  venue: string;
  symbol: string;
  side: Side;
  quantity: Decimal;
  limit_price: Decimal;
  reduce_only: boolean;
}

export interface OrderLeg {
  intent: OrderLegIntent;
  status: LegStatus;
  venue_order_id: string | null;
  filled_quantity: Decimal;
  average_price: Decimal | null;
}

export interface OrderIntent {
  idempotency_key: string;
  origin: OrderOrigin;
  strategy_id: string | null;
  legs: OrderLegIntent[];
  leverage: Decimal;
  max_slippage_bps: Decimal;
  quote_observed_at: string;
  created_at: string;
}

export interface Order {
  id: string;
  status: OrderStatus;
  intent: OrderIntent;
  legs: OrderLeg[];
  note: string | null;
  created_at: string;
  updated_at: string;
}

export interface Position {
  id: string;
  /** 来自策略的仓位记录来源策略，手动下单为 null。 */
  strategy_id?: string | null;
  /** 同一次双腿下单产生的仓位共享该分组 ID，为空表示单腿仓位。 */
  group_id?: string | null;
  venue: string;
  symbol: string;
  side: Side;
  quantity: Decimal;
  entry_price: Decimal;
  mark_price: Decimal;
  leverage: Decimal;
  unrealized_pnl: Decimal;
  updated_at: string;
}

/** API Key 型 CEX，以及用钱包地址查询、用签名私钥下单的 DEX。 */
export type AccountVenue =
  | "binance"
  | "okx"
  | "bybit"
  | "bitget"
  | "gate"
  | "aster"
  | "hyperliquid"
  | "lighter";

export type AccountKind = "api_key" | "wallet";

/** 导入账户后按美元折算的单个资产余额。 */
export interface AssetBalance {
  asset: string;
  free: Decimal;
  locked: Decimal;
  usd_value: Decimal | null;
}

export interface AccountSnapshotView {
  venue: string;
  label: string;
  equity_usd: Decimal;
  balances: AssetBalance[];
  observed_at: string;
  error?: string | null;
}

export interface TradingAccount {
  id: string;
  venue: AccountVenue | string;
  kind: AccountKind | string;
  label: string;
  api_key_preview: string;
  /** 是否具备下单所需凭证：CEX 的 Key/Secret，或 DEX 的签名私钥。 */
  can_trade: boolean;
  created_at: string;
  snapshot: AccountSnapshotView | null;
}

export interface AccountsResponse {
  accounts: TradingAccount[];
  total_equity_usd: Decimal;
  observed_at: string;
}

export interface EquityPoint {
  account_id: string;
  venue: string;
  equity_usd: Decimal;
  observed_at: string;
}

export interface EquityHistoryResponse {
  hours: number;
  points: EquityPoint[];
}

export interface ImportAccountRequest {
  venue: AccountVenue;
  label?: string;
  api_key?: string;
  api_secret?: string;
  passphrase?: string;
  wallet_address?: string;
  /** DEX 签名私钥；留空则该账户只能查询余额。 */
  signer_key?: string;
  /** Lighter 的 API Key 序号，默认 0。 */
  key_index?: number;
  /** Lighter 的数字账户索引，留空时按钱包地址自动解析。 */
  account_index?: number;
}

export interface VenueRiskLimits {
  max_order_notional_usd?: Decimal;
  max_leverage?: Decimal;
  max_slippage_bps?: Decimal;
  allowed_symbols?: string[];
  max_legs?: number;
}

export interface RiskLimits {
  max_order_notional_usd: Decimal;
  max_total_notional_usd: Decimal;
  max_leverage: Decimal;
  max_slippage_bps: Decimal;
  max_quote_age_secs: number;
  allowed_symbols: string[];
  allowed_venues: string[];
  max_legs: number;
  /** 按交易所覆盖；键为小写交易所标识。 */
  venue_overrides: Record<string, VenueRiskLimits>;
}

export interface RiskViolation {
  code: string;
  message: string;
}

export type RiskDecision =
  | { decision: "approved"; estimated_notional_usd: Decimal }
  | { decision: "rejected"; violations: RiskViolation[] };

export interface CreateOrderRequest {
  symbol: string;
  long_venue: string;
  short_venue: string;
  notional_usd: number;
  leverage?: number;
  max_slippage_bps?: number;
  origin?: OrderOrigin;
}

export interface CreateOrderResponse {
  order: Order;
  decision: RiskDecision | null;
  replayed: boolean;
}

/** 盘口单档：数量为币本位。 */
export interface DepthLevel {
  price: Decimal;
  quantity: Decimal;
}

/** 多档盘口快照，买盘价格由高到低、卖盘由低到高。 */
export interface DepthSnapshot {
  venue: string;
  symbol: string;
  bids: DepthLevel[];
  asks: DepthLevel[];
  observed_at: string;
}

export type ConditionalStatus =
  | "active"
  | "triggered"
  | "expired"
  | "cancelled"
  | "rejected";

/** 触发条件来源：两腿净资金费率年化，或多腿中间价。 */
export type TriggerKind = "funding" | "price";
export type TriggerDirection = "above" | "below";

/** 等待触发的双腿条件单。 */
export interface ConditionalOrder {
  id: string;
  symbol: string;
  long_venue: string;
  short_venue: string;
  notional_usd: Decimal;
  leverage: Decimal;
  trigger_kind: TriggerKind;
  trigger_direction: TriggerDirection;
  /** 资金费率触发时为年化百分比，价格触发时为报价。 */
  trigger_value: Decimal;
  expires_at: string | null;
  status: ConditionalStatus;
  note: string | null;
  triggered_order_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateConditionalRequest {
  symbol: string;
  long_venue: string;
  short_venue: string;
  notional_usd: number;
  leverage: number;
  trigger_kind: TriggerKind;
  trigger_direction: TriggerDirection;
  trigger_value: number;
  expires_at: string | null;
}

export interface ControlResponse {
  paused: boolean;
  reason: string | null;
}

/** 已结算的一期资金费率，来自执行器 funding_rates 表。 */
export interface FundingRate {
  venue: string;
  symbol: string;
  funding_time: string;
  funding_rate: Decimal;
  mark_price: Decimal | null;
}

export interface FundingSymbolStatus {
  symbol: string;
  last_funding_time: string | null;
  last_funding_rate: Decimal | null;
  /** 该合约的实际结算间隔（秒），由历史结算时间推断。 */
  funding_interval_secs: number;
  annualized_pct: Decimal | null;
  predicted_rate: Decimal | null;
  mark_price: Decimal | null;
  next_funding_at: string | null;
  observed_at: string | null;
}

/** 某个时间窗口内某交易所某交易对的资金费率累计值。 */
export interface FundingSummary {
  venue: string;
  symbol: string;
  /** 窗口内各期费率之和。 */
  rate: Decimal;
  count: number;
}

export interface FundingStatus {
  enabled: boolean;
  venue: string;
  base_url: string;
  poll_secs: number;
  stored_rows: number;
  last_attempt_at: string | null;
  last_success_at: string | null;
  last_error: string | null;
  symbols: FundingSymbolStatus[];
}

/** 一根已完成（或进行中）的 K 线，来自所选交易所的现货市场。 */
export interface Candle {
  open_time: string;
  open: Decimal;
  high: Decimal;
  low: Decimal;
  close: Decimal;
  volume: Decimal;
}

/** 执行器 WebSocket 推送的实时中间价。 */
export interface PriceTick {
  price: number;
  /** 推送时间（毫秒）。 */
  ts: number;
}

export interface ExecutorEvent {
  type: "opportunity_updated" | "order_updated" | "executor_status" | "alert";
  [key: string]: unknown;
}

/** 一条上币公告，来自 Binance / Upbit / Coinbase 的采集结果。 */
export interface NewsItem {
  id: string;
  source: string;
  title: string;
  url: string | null;
  published_at: string;
}

export interface NewsSourceStatus {
  source: string;
  enabled: boolean;
  last_success_at: string | null;
  last_error: string | null;
  count: number;
}

export interface NewsResponse {
  items: NewsItem[];
  sources: NewsSourceStatus[];
}

/** 已绑定的 Telegram 账号。 */
export interface TelegramBinding {
  telegram_user_id: number;
  chat_id: number;
  username: string | null;
  permissions: string[];
  /** 机器人在该会话里使用的语言；旧绑定没有该字段。 */
  lang?: "zh" | "en" | null;
  paired_at: string;
}

/** 执行器电报机器人状态。 */
export interface TelegramStatus {
  enabled: boolean;
  polling: boolean;
  bot_username: string | null;
  last_error: string | null;
  bindings: TelegramBinding[];
}

/** 一次性配对码。 */
export interface TelegramPairingCode {
  code: string;
  expires_at: string;
}

/** 执行器维护的离线授权状态，不含令牌本身。 */
export interface ExecutorLicense {
  /** 执行器是否强制校验授权（ARBPILOT_LICENSE_REQUIRED）。 */
  required: boolean;
  installed: boolean;
  valid: boolean;
  /** 已过期但仍在离线宽限期内。 */
  in_grace: boolean;
  plans: string[];
  status: "active" | "canceling" | "past_due" | "expired" | "none";
  email: string | null;
  subject: string | null;
  expires_at: number | null;
  grace_until: number | null;
  error: string | null;
}

// ---------------------------------------------------------------------------
// 策略分享与付费市场
// ---------------------------------------------------------------------------

export type RiskLevel = "low" | "medium" | "high";

/** 源码可见性：开源可复核，黑盒只公布行为说明。 */
export type SourceVisibility = "open_source" | "black_box";

export type MarketVisibility = "invite_only" | "public";

export type PublicationStatus = "private" | "published" | "withdrawn";

export type PricingModel = "free" | "monthly";

/** 表现记录来源：作者自报与平台可复现记录分开统计。 */
export type PerformanceSource =
  "author_backtest" | "platform_replay" | "authorized_live";

export type SubscriptionStatus =
  "pending_payment" | "trial" | "active" | "cancelled" | "expired";

export type SettlementStatus = "pending" | "paid" | "disputed";

export interface StrategyAuthorCard {
  author_id: string;
  display_name: string;
  bio: string;
  /** 结算收款标识（平台账户 ID），不含私钥或银行账号。 */
  payout_account: string | null;
  joined_at: string;
  updated_at: string;
}

export interface StrategyPricing {
  model: PricingModel;
  /** 月费（美元），免费策略必须为 0。 */
  monthly_price_usd: Decimal;
  trial_days: number;
}

export interface StrategyChangelogEntry {
  version: number;
  at: string;
  summary: string;
}

/** 一条策略发布记录，绑定具体策略版本。 */
export interface StrategyPublication {
  id: string;
  strategy_id: string;
  strategy_version: number;
  author_id: string;
  author_name: string;
  title: string;
  summary: string;
  logic_summary: string;
  risk_level: RiskLevel;
  suggested_max_notional_usd: Decimal;
  supported_venues: string[];
  supported_symbols: string[];
  data_sources: string[];
  runtime: string;
  known_limits: string[];
  source_visibility: SourceVisibility;
  visibility: MarketVisibility;
  pricing: StrategyPricing;
  status: PublicationStatus;
  /** 发布时冻结的沙箱模块哈希；为空表示作者还没重新发布过。 */
  artifact_sha256: string | null;
  /** 发布时冻结的策略定义，订阅者导入时直接使用。 */
  definition: StrategyDefinition | null;
  changelog: StrategyChangelogEntry[];
  created_at: string;
  updated_at: string;
}

/** 订阅可下载的策略交付物：冻结的定义 + 可执行的沙箱模块。 */
export interface PublicationBundle {
  publication_id: string;
  strategy_version: number;
  definition: StrategyDefinition;
  artifact_sha256: string;
  wasm_base64: string;
}

/** 版本化表现记录，必须绑定策略版本、区间与成本假设。 */
export interface StrategyPerformanceRecord {
  id: string;
  publication_id: string;
  strategy_id: string;
  strategy_version: number;
  source: PerformanceSource;
  period_start: string;
  period_end: string;
  trades: number;
  win_rate_pct: Decimal;
  net_pnl_usd: Decimal;
  return_pct: Decimal;
  max_drawdown_pct: Decimal;
  fee_bps_per_leg: Decimal;
  slippage_bps: Decimal;
  sample_size: number;
  /** 是否可由第三方复现；作者自报记录一律为 false。 */
  reproducible: boolean;
  recorded_at: string;
  note: string | null;
}

export interface StrategySubscription {
  id: string;
  publication_id: string;
  publication_title: string;
  strategy_id: string;
  author_id: string;
  subscriber_id: string;
  status: SubscriptionStatus;
  monthly_price_usd: Decimal;
  trial_ends_at: string | null;
  current_period_end: string | null;
  started_at: string;
  cancelled_at: string | null;
  payment_ref: string | null;
  updated_at: string;
}

export interface StrategySettlement {
  id: string;
  author_id: string;
  /** 结算周期，格式 `YYYY-MM`。 */
  period: string;
  subscription_count: number;
  gross_usd: Decimal;
  platform_fee_bps: number;
  platform_fee_usd: Decimal;
  net_usd: Decimal;
  status: SettlementStatus;
  created_at: string;
  updated_at: string;
  note: string | null;
}

/** 表现榜条目，收益只占少量权重。 */
export interface LeaderboardEntry {
  publication_id: string;
  title: string;
  author_name: string;
  strategy_version: number;
  risk_level: RiskLevel;
  composite_score: Decimal;
  return_pct: Decimal;
  max_drawdown_pct: Decimal;
  reproducibility_score: Decimal;
  sample_score: Decimal;
  execution_stability_score: Decimal;
  transparency_score: Decimal;
  verified_records: number;
  total_trades: number;
  sources: PerformanceSource[];
}

// ---------------------------------------------------------------------------
// WASM + Wasmtime 策略沙箱
// ---------------------------------------------------------------------------

export type SandboxArtifactSource = "uploaded" | "built_in";

export type SandboxRunStatus = "ok" | "invalid" | "error" | "timeout";

export interface SandboxLimits {
  max_fuel: number;
  max_memory_bytes: number;
  timeout_ms: number;
  max_output_bytes: number;
  max_decisions: number;
  max_log_lines: number;
  max_artifact_bytes: number;
}

/** 已上传并完成静态校验的 wasm 模块。 */
export interface SandboxArtifact {
  id: string;
  name: string;
  /** 模块内容 sha256，用于绑定版本与复现。 */
  sha256: string;
  size_bytes: number;
  abi_version: number | null;
  source: SandboxArtifactSource;
  notes: string | null;
  created_at: string;
}

/** 策略与沙箱模块的绑定。 */
export interface SandboxBinding {
  artifact_id: string;
  artifact_hash: string;
  abi_version: number;
  limits: SandboxLimits;
  attached_at: string;
}

/** 模块唯一可输出的动作集合，没有撤单、转账或密钥能力。 */
export type SandboxDecision =
  | {
      action: "open";
      symbol: string;
      notional_usd: Decimal;
      reason?: string | null;
    }
  | { action: "close"; symbol: string; reason?: string | null }
  | { action: "hold"; reason?: string | null };

export interface SandboxRunReport {
  artifact_id: string;
  artifact_hash: string;
  abi_version: number;
  status: SandboxRunStatus;
  decisions: SandboxDecision[];
  logs: string[];
  fuel_limit: number;
  fuel_consumed: number;
  max_memory_bytes: number;
  duration_ms: number;
  output_bytes: number;
  error: string | null;
  ran_at: string;
}

export interface SandboxDryRunRequest {
  strategy_id?: string;
  symbol?: string;
}

export interface BindSandboxRequest {
  artifact_id: string;
  limits?: SandboxLimits;
}

export interface CreatePublicationRequest {
  strategy_id: string;
  author_id: string;
  title: string;
  summary: string;
  logic_summary: string;
  risk_level: RiskLevel;
  suggested_max_notional_usd: number;
  supported_venues?: string[];
  supported_symbols?: string[];
  data_sources?: string[];
  runtime?: string;
  known_limits?: string[];
  source_visibility: SourceVisibility;
  visibility: MarketVisibility;
  pricing: StrategyPricing;
}

export interface UpdatePublicationRequest {
  action?: "publish" | "withdraw";
  title?: string;
  summary?: string;
  logic_summary?: string;
  risk_level?: RiskLevel;
  suggested_max_notional_usd?: number;
  known_limits?: string[];
  visibility?: MarketVisibility;
  pricing?: StrategyPricing;
  changelog_summary?: string;
}

export interface CreatePerformanceRequest {
  source: PerformanceSource;
  period_start: string;
  period_end: string;
  trades: number;
  win_rate_pct: number;
  net_pnl_usd: number;
  return_pct: number;
  max_drawdown_pct: number;
  fee_bps_per_leg: number;
  slippage_bps: number;
  sample_size: number;
  reproducible?: boolean;
  note?: string;
}
