use arb_core::{
    ExchangeAdapter, LegStatus, Order, OrderIntent, OrderLegIntent, OrderOrigin, OrderStatus,
    PlaceOrderRequest, Position, RiskContext, RiskDecision, RiskEngine, Side,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::TradingMode, error::ApiError, events::ExecutorEvent, state::AppState};

const MAX_IN_MEMORY_ORDERS: usize = 500;

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    /// 每条腿的名义价值（美元）。
    pub notional_usd: Decimal,
    #[serde(default = "default_leverage")]
    pub leverage: Decimal,
    pub max_slippage_bps: Option<Decimal>,
    pub strategy_id: Option<Uuid>,
    pub origin: Option<OrderOrigin>,
}

fn default_leverage() -> Decimal {
    Decimal::ONE
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
    if state.config.mode == TradingMode::Live {
        return Err(ApiError::Conflict(
            "真实交易适配器尚未接入，当前仅支持模拟盘".to_string(),
        ));
    }
    if body.notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("notional_usd 必须大于 0".to_string()));
    }
    if body.long_venue == body.short_venue {
        return Err(ApiError::BadRequest("双腿交易所不能相同".to_string()));
    }

    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Some(existing_id) = state
        .idempotency
        .read()
        .expect("幂等锁不可用")
        .get(&idempotency_key)
        .copied()
    {
        let existing = state
            .orders
            .read()
            .expect("订单锁不可用")
            .iter()
            .find(|order| order.id == existing_id)
            .cloned();
        if let Some(order) = existing {
            return Ok((
                StatusCode::OK,
                Json(CreateOrderResponse {
                    order,
                    decision: None,
                    replayed: true,
                }),
            ));
        }
    }

    let long_quote = state
        .market
        .quote(&body.long_venue, &body.symbol)
        .ok_or_else(|| {
            ApiError::BadRequest(format!("{} 在 {} 没有可用行情", body.symbol, body.long_venue))
        })?;
    let short_quote = state
        .market
        .quote(&body.short_venue, &body.symbol)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "{} 在 {} 没有可用行情",
                body.symbol, body.short_venue
            ))
        })?;

    let long_quantity = (body.notional_usd / long_quote.ask).round_dp(6);
    let short_quantity = (body.notional_usd / short_quote.bid).round_dp(6);
    if long_quantity <= Decimal::ZERO || short_quantity <= Decimal::ZERO {
        return Err(ApiError::BadRequest("下单数量过小".to_string()));
    }

    let intent = OrderIntent {
        idempotency_key: idempotency_key.clone(),
        origin: body.origin.unwrap_or(OrderOrigin::Desktop),
        strategy_id: body.strategy_id,
        legs: vec![
            OrderLegIntent {
                venue: body.long_venue.clone(),
                symbol: body.symbol.clone(),
                side: Side::Buy,
                quantity: long_quantity,
                limit_price: long_quote.ask,
                reduce_only: false,
            },
            OrderLegIntent {
                venue: body.short_venue.clone(),
                symbol: body.symbol.clone(),
                side: Side::Sell,
                quantity: short_quantity,
                limit_price: short_quote.bid,
                reduce_only: false,
            },
        ],
        leverage: body.leverage,
        max_slippage_bps: body
            .max_slippage_bps
            .unwrap_or(state.config.risk.max_slippage_bps),
        quote_observed_at: long_quote.observed_at.min(short_quote.observed_at),
        created_at: Utc::now(),
    };

    let context = RiskContext {
        now: Utc::now(),
        limits: state.config.risk.clone(),
        open_notional_usd: state.open_notional_usd(),
        paused: state.is_paused(),
    };
    let decision = RiskEngine::preflight(&intent, &context);

    if let RiskDecision::Rejected { violations } = &decision {
        let mut order = Order::from_intent(intent, OrderStatus::Rejected);
        order.note = Some(
            violations
                .iter()
                .map(|violation| violation.message.clone())
                .collect::<Vec<_>>()
                .join("；"),
        );
        state.store.save_order(&order).await;
        state.emit_order(&order);
        remember(&state, &idempotency_key, &order);
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(CreateOrderResponse {
                order,
                decision: Some(decision),
                replayed: false,
            }),
        ));
    }

    let mut order = Order::from_intent(intent, OrderStatus::Submitting);
    state.store.save_order(&order).await;

    let mut failure: Option<String> = None;
    for index in 0..order.intent.legs.len() {
        let leg = order.intent.legs[index].clone();
        let request = PlaceOrderRequest {
            symbol: leg.symbol.clone(),
            side: leg.side,
            quantity: leg.quantity,
            limit_price: leg.limit_price,
            reduce_only: leg.reduce_only,
            client_order_id: format!("{idempotency_key}-{index}"),
        };

        match state.exchange.place_order(request).await {
            Ok(ack) => {
                let entry = &mut order.legs[index];
                entry.status = LegStatus::Filled;
                entry.venue_order_id = Some(ack.venue_order_id);
                entry.filled_quantity = leg.quantity;
                entry.average_price = Some(leg.limit_price);
            }
            Err(error) => {
                order.legs[index].status = LegStatus::Rejected;
                failure = Some(format!("第 {} 腿下单失败: {error}", index + 1));
                break;
            }
        }
    }

    if let Some(message) = failure {
        let has_filled_leg = order.legs.iter().any(|leg| leg.is_filled());
        let compensation = OrderStatus::PartiallyFilled.compensation(&order.legs);
        order.note = Some(format!("{message}；补偿动作: {compensation:?}"));
        order.status = if has_filled_leg {
            OrderStatus::Unwinding
        } else {
            OrderStatus::Failed
        };
        if has_filled_leg {
            state.set_paused(true, Some("双腿失衡，等待人工确认补偿"));
        }
        state.store.save_order(&order).await;
        state.emit_order(&order);
        let _ = state.events.send(ExecutorEvent::order_transition(
            &order,
            format!("订单 {} 状态: {:?}", order.id, order.status),
        ));
        remember(&state, &idempotency_key, &order);
        return Ok((
            StatusCode::OK,
            Json(CreateOrderResponse {
                order,
                decision: Some(decision),
                replayed: false,
            }),
        ));
    }

    order.status = OrderStatus::Filled;
    order.updated_at = Utc::now();

    {
        let mut positions = state.positions.write().expect("仓位锁不可用");
        for leg in &order.legs {
            if !leg.is_filled() {
                continue;
            }
            let price = leg.intent.limit_price;
            positions.push(Position {
                id: Uuid::new_v4(),
                venue: leg.intent.venue.clone(),
                symbol: leg.intent.symbol.clone(),
                side: leg.intent.side,
                quantity: leg.filled_quantity,
                entry_price: price,
                mark_price: price,
                leverage: order.intent.leverage,
                unrealized_pnl: Decimal::ZERO,
                updated_at: Utc::now(),
            });
        }
    }

    state.store.save_order(&order).await;
    state.emit_order(&order);
    remember(&state, &idempotency_key, &order);

    Ok((
        StatusCode::CREATED,
        Json(CreateOrderResponse {
            order,
            decision: Some(decision),
            replayed: false,
        }),
    ))
}

fn remember(state: &AppState, key: &str, order: &Order) {
    state
        .idempotency
        .write()
        .expect("幂等锁不可用")
        .insert(key.to_string(), order.id);

    let mut orders = state.orders.write().expect("订单锁不可用");
    orders.insert(0, order.clone());
    orders.truncate(MAX_IN_MEMORY_ORDERS);
}
