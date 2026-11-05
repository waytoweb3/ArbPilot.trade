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
