use arb_core::{Order, OrderOrigin, OrderStatus, RiskDecision};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    gateway::{self, PairOrderRequest},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    /// 每条腿的名义价值（美元）。
    pub notional_usd: Decimal,
    /// 要设置到交易所的杠杆；不传则沿用交易所当前设置。
    pub leverage: Option<Decimal>,
    pub max_slippage_bps: Option<Decimal>,
    pub strategy_id: Option<Uuid>,
    pub origin: Option<OrderOrigin>,
}

#[derive(Debug, Serialize)]
pub struct CreateOrderResponse {
    pub order: Order,
    pub decision: Option<RiskDecision>,
    /// true 表示命中幂等键，返回的是历史订单。
    pub replayed: bool,
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<Order>> {
    Json(state.orders.read().expect("订单锁不可用").clone())
}

/// 开仓入口：桌面端、Telegram 与策略最终都调用同一路径，统一经过风控。
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateOrderRequest>,
) -> Result<(StatusCode, Json<CreateOrderResponse>), ApiError> {
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let outcome = gateway::submit_pair(
        &state,
        PairOrderRequest {
            symbol: body.symbol,
            long_venue: body.long_venue,
            short_venue: body.short_venue,
            notional_usd: body.notional_usd,
            leverage: body.leverage,
            max_slippage_bps: body.max_slippage_bps,
            strategy_id: body.strategy_id,
            origin: body.origin.unwrap_or(OrderOrigin::Desktop),
            idempotency_key,
            closing: false,
            max_unhedged_secs: None,
        },
    )
    .await?;

    let status = if outcome.replayed {
        StatusCode::OK
    } else {
        match outcome.order.status {
            OrderStatus::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
            OrderStatus::Filled => StatusCode::CREATED,
            _ => StatusCode::OK,
        }
    };

    Ok((
        status,
        Json(CreateOrderResponse {
            order: outcome.order,
            decision: outcome.decision,
            replayed: outcome.replayed,
        }),
    ))
}
