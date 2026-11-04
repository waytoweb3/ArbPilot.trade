use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 场所类型：中心化交易所或链上场所。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueKind {
    Cex,
    Dex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityKind {
    FundingRate,
    CexCexSpread,
    CexDexSpread,
}

/// 一次可执行的套利机会估算。所有 bps 均为相对名义价值的基点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub id: Uuid,
    pub kind: OpportunityKind,
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    pub gross_spread_bps: Decimal,
    pub estimated_fee_bps: Decimal,
    pub estimated_slippage_bps: Decimal,
    pub net_edge_bps: Decimal,
    pub max_notional_usd: Decimal,
    /// 仅资金费率机会有值，正数表示多头收资金费。
    pub funding_rate: Option<Decimal>,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Opportunity {
    /// 行情是否仍然新鲜：未过期且观测时间在允许延迟内。
    pub fn is_fresh(&self, now: DateTime<Utc>, max_age: Duration) -> bool {
        now < self.expires_at && now.signed_duration_since(self.observed_at) <= max_age
    }
}

/// 盘口快照，供构造订单价格与估算滑点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub venue: String,
    pub symbol: String,
    pub bid: Decimal,
    pub ask: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

/// 订单来源，用于审计与权限区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderOrigin {
    Desktop,
    Telegram,
    Strategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLegIntent {
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    /// 限价；市价单在提交前也必须由执行器换算成本地保护价。
    pub limit_price: Decimal,
    pub reduce_only: bool,
}

/// 策略/桌面/Telegram 提交的订单意图，必须经过风控后才能变成订单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub idempotency_key: String,
    pub origin: OrderOrigin,
    pub strategy_id: Option<Uuid>,
    pub legs: Vec<OrderLegIntent>,
    pub leverage: Decimal,
    pub max_slippage_bps: Decimal,
    /// 策略看到行情的时间，用于行情时效校验。
    pub quote_observed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl OrderIntent {
    pub fn notional_usd(&self) -> Decimal {
        self.legs
            .iter()
            .map(|leg| leg.quantity * leg.limit_price)
            .sum()
    }

    pub fn symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self.legs.iter().map(|leg| leg.symbol.clone()).collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegStatus {
    Pending,
    Submitted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    PendingRisk,
    Rejected,
    Submitting,
    PartiallyFilled,
    Filled,
    Unwinding,
    Cancelled,
    Failed,
}

impl OrderStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Filled | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLeg {
    pub intent: OrderLegIntent,
    pub status: LegStatus,
    pub venue_order_id: Option<String>,
    pub filled_quantity: Decimal,
    pub average_price: Option<Decimal>,
}

impl OrderLeg {
    pub fn pending(intent: OrderLegIntent) -> Self {
        Self {
            intent,
            status: LegStatus::Pending,
            venue_order_id: None,
            filled_quantity: Decimal::ZERO,
            average_price: None,
        }
    }

    pub fn is_filled(&self) -> bool {
        matches!(self.status, LegStatus::Filled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub status: OrderStatus,
    pub intent: OrderIntent,
    pub legs: Vec<OrderLeg>,
    /// 风控拒绝原因或补偿动作说明。
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    pub fn from_intent(intent: OrderIntent, status: OrderStatus) -> Self {
        let legs = intent
            .legs
            .iter()
            .cloned()
            .map(OrderLeg::pending)
            .collect();
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            status,
            intent,
            legs,
            note: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: Uuid,
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub entry_price: Decimal,
    pub mark_price: Decimal,
    pub leverage: Decimal,
    pub unrealized_pnl: Decimal,
    pub updated_at: DateTime<Utc>,
}

impl Position {
    pub fn notional_usd(&self) -> Decimal {
        self.quantity * self.mark_price
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub asset: String,
    pub free: Decimal,
    pub locked: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyStatus {
    Draft,
    Running,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecord {
    pub id: Uuid,
    pub name: String,
    pub kind: OpportunityKind,
    pub status: StrategyStatus,
    pub venues: Vec<String>,
    pub symbols: Vec<String>,
    /// 最近一次心跳；用于识别策略 Worker 失联。
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}
