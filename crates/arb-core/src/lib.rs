//! ArbPilot 执行器核心领域层。
//!
//! 该 crate 不包含任何交易所密钥、网络或数据库实现，只描述：
//! - 机会、订单、仓位等领域对象
//! - 风控前置检查
//! - 双腿订单状态机与补偿动作
//! - 版本化策略定义、语义校验与资金费率回测
//! - 交易所适配器 trait（由具体交易所实现）

pub mod exchange;
pub mod indicators;
pub mod market;
pub mod order;
pub mod risk;
pub mod sandbox;
pub mod strategy;
pub mod types;

pub use exchange::{
    AccountSnapshot, ExchangeAdapter, ExchangeError, PlaceOrderRequest, VenueOrder, VenueOrderAck,
};
pub use market::{
    compute_settlement, compute_settlement_from_gross, period_bounds, publish_readiness,
    rank_publications, settlement_period,
    LeaderboardEntry, MarketVisibility, PerformanceSource, PricingModel, PublicationStatus,
    RiskLevel, SettlementStatus, SourceVisibility, StrategyAuthorCard, StrategyChangelogEntry,
    StrategyPerformanceRecord, StrategyPublication, StrategyPricing, StrategySettlement,
    StrategySubscription, SubscriptionStatus,
};
pub use order::Compensation;
pub use risk::{
    RiskContext, RiskDecision, RiskEngine, RiskLimits, RiskViolation, RiskViolationCode,
    VenueRiskLimits,
};
pub use sandbox::{
    SandboxArtifact, SandboxArtifactSource, SandboxBinding, SandboxCandle, SandboxDecision,
    SandboxEvent, SandboxIndicators, SandboxInput, SandboxLimits, SandboxMarketSnapshot,
    SandboxOrderLimits,
    SandboxOutput, SandboxPositionSnapshot, SandboxRunReport, SandboxRunStatus,
    SANDBOX_ABI_VERSION, SANDBOX_CANDLE, SANDBOX_CANDLE_CLOSE, SANDBOX_CANDLE_HIGH,
    SANDBOX_CANDLE_LOW, SANDBOX_CANDLE_OPEN, SANDBOX_CANDLE_OPEN_TIME, SANDBOX_CANDLE_VOLUME,
    SANDBOX_CANDLES, SANDBOX_CANCEL, SANDBOX_CLOSE, SANDBOX_EMIT, SANDBOX_ENTRYPOINT,
    SANDBOX_EVENT, SANDBOX_EVENTS, SANDBOX_EVENT_AGE_SECS, SANDBOX_EVENT_MATCHED,
    SANDBOX_EVENT_PUBLISHED_AT, SANDBOX_EVENT_TEXT, SANDBOX_EVENT_TEXT_KIND,
    SANDBOX_EVENT_TEXT_SOURCE, SANDBOX_EVENT_TEXT_SYMBOL, SANDBOX_EVENT_TEXT_TITLE,
    SANDBOX_FIELD_ASK, SANDBOX_FIELD_BID, SANDBOX_FIELD_FUNDING_RATE,
    SANDBOX_FIELD_INSTRUMENT, SANDBOX_FIELD_LAST, SANDBOX_FIELD_MAX_NOTIONAL_USD, SANDBOX_HOLD,
    SANDBOX_IMPORT_NAMESPACE, SANDBOX_INDICATOR, SANDBOX_INDICATOR_ATR,
    SANDBOX_INDICATOR_CHANNEL_HIGH, SANDBOX_INDICATOR_CHANNEL_LOW, SANDBOX_INDICATOR_EMA_FAST,
    SANDBOX_INDICATOR_EMA_SLOW, SANDBOX_INDICATOR_SMA_FAST, SANDBOX_INDICATOR_SMA_SLOW,
    SANDBOX_LIMIT, SANDBOX_LOG, SANDBOX_MARKET, SANDBOX_MARKET_SYMBOL, SANDBOX_MARKETS,
    SANDBOX_OPEN,
    SANDBOX_OPEN_PAIR, SANDBOX_POSITION_ENTRY_PRICE, SANDBOX_POSITION_HELD_BARS,
    SANDBOX_POSITION_HELD_SECS, SANDBOX_POSITION_NOTIONAL, SANDBOX_POSITION_PRESENT,
    SANDBOX_POSITION_SIDE,
};
pub use strategy::{
    run_backtest, BacktestAssumption, BacktestReport, CostAssumptions, DataRules, EntryRules,
    EquityPoint, ExecutionRules, ExitRules, FundingSample, Instrument, StrategyCode,
    StrategyCodeLanguage, StrategyCodeOrigin, StrategyDefinition, StrategyEvent,
    StrategyEventLevel, StrategyExecutionMode, StrategyKind, StrategyOpenPosition,
    SkillBinding, StrategyPendingOrder, StrategyRecord, StrategyRisk, StrategyRunScope,
    StrategyRuntime, StrategySource, StrategyStatus, StrategyUniverse, StrategyValidation,
    ValidationIssue, ValidationSeverity,
};
pub use types::{
    Balance, LegStatus, Opportunity, OpportunityKind, Order, OrderIntent, OrderLeg, OrderLegIntent,
    OrderOrigin, OrderStatus, Position, PositionSide, Quote, Side, VenueKind,
};
