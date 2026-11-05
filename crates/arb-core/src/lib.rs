//! ArbPilot 执行器核心领域层。
//!
//! 该 crate 不包含任何交易所密钥、网络或数据库实现，只描述：
//! - 机会、订单、仓位等领域对象
//! - 风控前置检查
//! - 双腿订单状态机与补偿动作
//! - 版本化策略定义、语义校验与资金费率回测
//! - 交易所适配器 trait（由具体交易所实现）

pub mod exchange;
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
};
pub use sandbox::{
    SandboxArtifact, SandboxArtifactSource, SandboxBinding, SandboxDecision, SandboxInput,
    SandboxLimits, SandboxMarketSnapshot, SandboxOrderLimits, SandboxOutput,
    SandboxPositionSnapshot, SandboxRunReport, SandboxRunStatus, SANDBOX_ABI_VERSION,
    SANDBOX_CLOSE, SANDBOX_EMIT, SANDBOX_ENTRYPOINT, SANDBOX_FIELD_FUNDING_RATE,
    SANDBOX_FIELD_GROSS_SPREAD_BPS, SANDBOX_FIELD_MAX_NOTIONAL_USD, SANDBOX_FIELD_NET_EDGE_BPS,
    SANDBOX_FIELD_PERP_ASK, SANDBOX_FIELD_PERP_BID, SANDBOX_FIELD_SPOT_ASK,
    SANDBOX_FIELD_SPOT_BID, SANDBOX_HOLD, SANDBOX_IMPORT_NAMESPACE, SANDBOX_LOG, SANDBOX_MARKET,
    SANDBOX_MARKETS, SANDBOX_OPEN, SANDBOX_POSITION_HELD_SECS, SANDBOX_POSITION_NOTIONAL,
    SANDBOX_POSITION_PRESENT,
};
pub use strategy::{
    run_backtest, BacktestAssumption, BacktestReport, CostAssumptions, EntryRules, EquityPoint,
    ExecutionRules, ExitRules, FundingSample, Instrument, StrategyCode, StrategyCodeLanguage,
    StrategyCodeOrigin, StrategyDefinition, StrategyEvent, StrategyEventLevel,
    StrategyOpenPosition, StrategyRecord, StrategyRisk, StrategyRunScope, StrategyRuntime,
    StrategySource, StrategyStatus, StrategyUniverse, StrategyValidation, ValidationIssue,
    ValidationSeverity,
};
pub use types::{
    Balance, LegStatus, Opportunity, OpportunityKind, Order, OrderIntent, OrderLeg, OrderLegIntent,
    OrderOrigin, OrderStatus, Position, Quote, Side, VenueKind,
};
