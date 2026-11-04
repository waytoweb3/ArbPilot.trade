use async_trait::async_trait;
use arb_core::{
    AccountSnapshot, Balance, ExchangeAdapter, ExchangeError, PlaceOrderRequest, VenueKind, VenueOrder,
    VenueOrderAck,
};
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

/// 模拟交易场所：用于 Phase 1 验证订单流程与风控，不接触任何真实资金。
pub struct MockExchange;

impl MockExchange {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockExchange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExchangeAdapter for MockExchange {
    fn venue(&self) -> &str {
        "paper"
    }

    fn kind(&self) -> VenueKind {
        VenueKind::Cex
    }

    async fn account_snapshot(&self) -> Result<AccountSnapshot, ExchangeError> {
        Ok(AccountSnapshot {
            venue: "paper".to_string(),
            balances: vec![Balance {
                asset: "USDT".to_string(),
                free: Decimal::from(100_000),
                locked: Decimal::ZERO,
            }],
            equity_usd: Decimal::from(100_000),
            margin_used_usd: Decimal::ZERO,
            available_margin_usd: Decimal::from(100_000),
            observed_at: Utc::now(),
        })
    }

    async fn place_order(&self, request: PlaceOrderRequest) -> Result<VenueOrderAck, ExchangeError> {
        if request.quantity <= Decimal::ZERO {
            return Err(ExchangeError::Rejected("下单数量必须大于 0".to_string()));
        }
        if request.limit_price <= Decimal::ZERO {
            return Err(ExchangeError::Rejected("限价必须大于 0".to_string()));
        }
        Ok(VenueOrderAck {
            venue_order_id: format!("paper-{}", Uuid::new_v4()),
            client_order_id: request.client_order_id,
            accepted_at: Utc::now(),
        })
    }

    async fn cancel_order(
        &self,
        _symbol: &str,
        _venue_order_id: &str,
    ) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn order_status(
        &self,
        symbol: &str,
        venue_order_id: &str,
    ) -> Result<VenueOrder, ExchangeError> {
        Err(ExchangeError::Unsupported(format!(
            "模拟交易所不提供订单查询: {symbol} {venue_order_id}"
        )))
    }
}
