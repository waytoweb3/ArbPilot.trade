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
    /// 同交易所「现货多 + 永续空」吃资金费。
    FundingRate,
    /// 跨交易所「永续多 + 永续空」吃资金费率差。
    PerpPerpFunding,
    /// 同交易所「现货多 + 交割合约空」吃基差到期收敛。
    DeliveryBasis,
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
    /// 资金费率年化收益率（百分比，`10.95` 表示 10.95%/年）。
    ///
    /// 各交易所结算间隔不同（Hyperliquid 1 小时、多数 CEX 8 小时），
    /// 机会之间的比较与排序一律以该字段为准，避免直接比较单期费率。
    pub funding_annualized_pct: Option<Decimal>,
    /// 仅交割基差机会有值：交割合约代码，如 `BTCUSDT_251226`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_contract: Option<String>,
    /// 仅交割基差机会有值：交割合约到期时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<DateTime<Utc>>,
    /// 基差年化收益率（百分比，`10.95` 表示 10.95%/年），仅交割基差机会有值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annualized_basis_pct: Option<Decimal>,
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

/// 持仓方向。`Side` 描述买卖动作，这里描述持有的是多头还是空头。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSide {
    Long,
    Short,
}

impl PositionSide {
    /// 开仓动作：做多买入，做空卖出。
    pub fn open_side(self) -> Side {
        match self {
            Self::Long => Side::Buy,
            Self::Short => Side::Sell,
        }
    }

    /// 平仓动作与开仓相反。
    pub fn close_side(self) -> Side {
        match self {
            Self::Long => Side::Sell,
            Self::Short => Side::Buy,
        }
    }
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
        let legs = intent.legs.iter().cloned().map(OrderLeg::pending).collect();
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
    /// 来自策略的仓位记录来源策略，手动下单为 None。
    #[serde(default)]
    pub strategy_id: Option<Uuid>,
    /// 同一次双腿下单产生的仓位共享同一个分组 ID，用于整组平仓。
    #[serde(default)]
    pub group_id: Option<Uuid>,
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
