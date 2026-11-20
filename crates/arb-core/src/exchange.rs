use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::{Balance, LegStatus, Side, VenueKind};

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error("交易所拒单: {0}")]
    Rejected(String),
    #[error("请求超时: {0}")]
    Timeout(String),
    #[error("触发限频: {0}")]
    RateLimited(String),
    #[error("传输失败: {0}")]
    Transport(String),
    #[error("凭证缺失或无效: {0}")]
    Credentials(String),
    #[error("不支持的操作: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub limit_price: Decimal,
    pub reduce_only: bool,
    /// 幂等标识，交易所不支持时由执行器本地去重。
    pub client_order_id: String,
    /// 该腿应使用的杠杆；`None` 表示沿用交易所当前设置。
    /// 部分交易所直接把杠杆放在下单请求里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leverage: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOrderAck {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOrder {
    pub venue_order_id: String,
    pub status: LegStatus,
    pub filled_quantity: Decimal,
    pub average_price: Option<Decimal>,
    /// 交易所实际收取的手续费；部分交易所异步返回，未返回时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_currency: Option<String>,
}

/// 交易所订单的最终/中间状态：下单回执只代表「已受理」，必须再查一次状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VenueOrderState {
    Open,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    /// 交易所未返回可识别状态（接口不支持或字段缺失）。
    Unknown,
}

/// 订单状态查询结果：成交均价与手续费以交易所回报为准。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOrderStatus {
    pub state: VenueOrderState,
    pub filled_quantity: Decimal,
    pub average_price: Option<Decimal>,
    pub fee: Option<Decimal>,
    pub fee_currency: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub venue: String,
    pub balances: Vec<Balance>,
    pub equity_usd: Decimal,
    pub margin_used_usd: Decimal,
    pub available_margin_usd: Decimal,
    pub observed_at: DateTime<Utc>,
}

/// 各交易所适配器统一实现该 trait。
///
/// 约定：
/// - 不得在日志中输出密钥。
/// - 超时类错误必须由调用方先查询订单状态，再决定是否重试。
#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    fn venue(&self) -> &str;

    fn kind(&self) -> VenueKind;

    async fn account_snapshot(&self) -> Result<AccountSnapshot, ExchangeError>;

    async fn place_order(&self, request: PlaceOrderRequest)
        -> Result<VenueOrderAck, ExchangeError>;

    async fn cancel_order(&self, symbol: &str, venue_order_id: &str) -> Result<(), ExchangeError>;

    async fn order_status(
        &self,
        symbol: &str,
        venue_order_id: &str,
    ) -> Result<VenueOrder, ExchangeError>;
}
