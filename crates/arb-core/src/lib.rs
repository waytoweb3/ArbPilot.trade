//! ArbPilot 执行器核心领域层。
//!
//! 该 crate 不包含任何交易所密钥、网络或数据库实现，只描述：
//! - 机会、订单、仓位等领域对象
//! - 风控前置检查
//! - 双腿订单状态机与补偿动作
//! - 交易所适配器 trait（由具体交易所实现）

pub mod exchange;
pub mod order;
pub mod risk;
pub mod types;

pub use exchange::{
    AccountSnapshot, ExchangeAdapter, ExchangeError, PlaceOrderRequest, VenueOrder, VenueOrderAck,
};
pub use order::Compensation;
pub use risk::{RiskContext, RiskDecision, RiskEngine, RiskLimits, RiskViolation, RiskViolationCode};
pub use types::{
    Balance, LegStatus, Opportunity, OpportunityKind, Order, OrderIntent, OrderLeg, OrderLegIntent,
    OrderOrigin, OrderStatus, Position, Quote, Side, StrategyRecord, StrategyStatus, VenueKind,
};
