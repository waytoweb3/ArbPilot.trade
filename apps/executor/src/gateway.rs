use arb_core::{
    LegStatus, Order, OrderIntent, OrderLegIntent, OrderOrigin, OrderStatus, PlaceOrderRequest,
    Position, RiskContext, RiskDecision, RiskEngine, Side,
};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{error::ApiError, events::ExecutorEvent, state::AppState};

const MAX_IN_MEMORY_ORDERS: usize = 500;

/// 双腿下单请求。桌面端、Telegram 与策略都必须走这里，统一经过风控。
#[derive(Debug, Clone)]
pub struct PairOrderRequest {
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    /// 每条腿的名义价值（美元）。
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    pub max_slippage_bps: Option<Decimal>,
    pub strategy_id: Option<Uuid>,
    pub origin: OrderOrigin,
    pub idempotency_key: String,
    /// 平仓：反向卖出多头腿、买回空头腿，并减少已有仓位。
    pub closing: bool,
    /// 第一腿成交后，第二腿必须在该时限内完成，超时按双腿失衡处理。
    pub max_unhedged_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PairOrderOutcome {
    pub order: Order,
    pub decision: Option<RiskDecision>,
    /// true 表示命中幂等键，返回的是历史订单。
    pub replayed: bool,
}

/// 统一订单入口：幂等检查 → 行情校验 → 风控 → 实盘下单 → 仓位记账。
///
/// 策略执行器只能调用该函数，不能直接访问交易所或密钥。
pub async fn submit_pair(
    state: &AppState,
    request: PairOrderRequest,
) -> Result<PairOrderOutcome, ApiError> {
    if !state.config.allow_live_trading {
        return Err(ApiError::Conflict(
            "实盘下单未开启：设置 ARBPILOT_ALLOW_LIVE_TRADING=true 后才会向交易所提交订单"
                .to_string(),
        ));
    }
    if request.notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("notional_usd 必须大于 0".to_string()));
    }
    if request.long_venue == request.short_venue {
        return Err(ApiError::BadRequest("双腿交易所不能相同".to_string()));
    }

    if let Some(existing_id) = state
        .idempotency
        .read()
        .expect("幂等锁不可用")
        .get(&request.idempotency_key)
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
            return Ok(PairOrderOutcome {
                order,
                decision: None,
                replayed: true,
            });
        }
    }

    let long_quote = state
        .market
        .quote(&request.long_venue, &request.symbol)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "{} 在 {} 没有可用行情",
                request.symbol, request.long_venue
            ))
        })?;
    let short_quote = state
        .market
        .quote(&request.short_venue, &request.symbol)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "{} 在 {} 没有可用行情",
                request.symbol, request.short_venue
            ))
        })?;

    let long_quantity = (request.notional_usd / long_quote.ask).round_dp(6);
    let short_quantity = (request.notional_usd / short_quote.bid).round_dp(6);
    if long_quantity <= Decimal::ZERO || short_quantity <= Decimal::ZERO {
        return Err(ApiError::BadRequest("下单数量过小".to_string()));
    }

    // 开仓：买多头腿、卖空头腿；平仓：反向操作且 reduce_only。
    let (long_side, short_side) = if request.closing {
        (Side::Sell, Side::Buy)
    } else {
        (Side::Buy, Side::Sell)
    };
    let reduce_only = request.closing;
    let limits = state.risk_limits();

    let intent = OrderIntent {
        idempotency_key: request.idempotency_key.clone(),
        origin: request.origin,
        strategy_id: request.strategy_id,
        legs: vec![
            OrderLegIntent {
                venue: request.long_venue.clone(),
                symbol: request.symbol.clone(),
                side: long_side,
                quantity: long_quantity,
                limit_price: if request.closing {
                    long_quote.bid
                } else {
                    long_quote.ask
                },
                reduce_only,
            },
            OrderLegIntent {
                venue: request.short_venue.clone(),
                symbol: request.symbol.clone(),
                side: short_side,
                quantity: short_quantity,
                limit_price: if request.closing {
                    short_quote.ask
                } else {
                    short_quote.bid
                },
                reduce_only,
            },
        ],
        leverage: request.leverage,
        max_slippage_bps: request
            .max_slippage_bps
            .unwrap_or(limits.max_slippage_bps),
        quote_observed_at: long_quote.observed_at.min(short_quote.observed_at),
        created_at: Utc::now(),
    };

    let context = RiskContext {
        now: Utc::now(),
        limits,
        // 平仓会降低敞口，不应被总敞口上限挡住。
        open_notional_usd: if request.closing {
            Decimal::ZERO
        } else {
            state.open_notional_usd()
        },
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
        remember(state, &request.idempotency_key, &order);
        return Ok(PairOrderOutcome {
            order,
            decision: Some(decision),
            replayed: false,
        });
    }

    let mut order = Order::from_intent(intent.clone(), OrderStatus::Submitting);
    state.store.save_order(&order).await;

    let mut failure: Option<String> = None;
    let mut first_fill_at: Option<DateTime<Utc>> = None;
    for index in 0..order.intent.legs.len() {
        let leg = order.intent.legs[index].clone();
        let venue = leg.venue.clone();
        let place = PlaceOrderRequest {
            symbol: leg.symbol.clone(),
            side: leg.side,
            quantity: leg.quantity,
            limit_price: leg.limit_price,
            reduce_only: leg.reduce_only,
            client_order_id: format!("{}-{index}", request.idempotency_key),
        };
        // 每条腿都按自己的交易所取凭证并签名，同一笔订单可以跨交易所。
        let executed = state.exchange.place_order(&venue, place);

        let placed = match (first_fill_at, request.max_unhedged_secs) {
            (Some(filled_at), Some(limit)) => {
                let remaining =
                    limit as i64 - Utc::now().signed_duration_since(filled_at).num_seconds();
                if remaining <= 0 {
                    Err(format!("第 {} 腿超过双腿间隔上限 {limit} 秒", index + 1))
                } else {
                    match tokio::time::timeout(Duration::from_secs(remaining as u64), executed).await
                    {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(_) => Err(format!("第 {} 腿等待成交超时", index + 1)),
                    }
                }
            }
            _ => executed.await.map_err(|error| error.to_string()),
        };

        match placed {
            Ok(ack) => {
                let entry = &mut order.legs[index];
                entry.status = LegStatus::Filled;
                entry.venue_order_id = Some(ack.venue_order_id);
                entry.filled_quantity = leg.quantity;
                entry.average_price = Some(leg.limit_price);
                first_fill_at.get_or_insert_with(Utc::now);
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
        remember(state, &request.idempotency_key, &order);
        return Ok(PairOrderOutcome {
            order,
            decision: Some(decision),
            replayed: false,
        });
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
            if request.closing {
                reduce_position(
                    &mut positions,
                    &intent,
                    leg.intent.venue.as_str(),
                    &leg.intent.symbol,
                    leg.intent.side,
                    leg.filled_quantity,
                );
                continue;
            }
            positions.push(Position {
                id: Uuid::new_v4(),
                strategy_id: intent.strategy_id,
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
    remember(state, &request.idempotency_key, &order);

    Ok(PairOrderOutcome {
        order,
        decision: Some(decision),
        replayed: false,
    })
}

/// 平仓腿成交后减少对应仓位；同一策略优先，手动平仓匹配任意来源。
fn reduce_position(
    positions: &mut Vec<Position>,
    intent: &OrderIntent,
    venue: &str,
    symbol: &str,
    closing_side: Side,
    quantity: Decimal,
) {
    let mut remaining = quantity;
    let mut index = 0;
    while index < positions.len() && remaining > Decimal::ZERO {
        let position = &positions[index];
        let same_strategy = match intent.strategy_id {
            Some(strategy_id) => position.strategy_id == Some(strategy_id),
            None => true,
        };
        // 平仓方向与持仓方向相反：买回平掉空头，卖出平掉多头。
        if position.venue == venue
            && position.symbol == symbol
            && position.side != closing_side
            && same_strategy
        {
            if position.quantity <= remaining {
                remaining -= position.quantity;
                positions.remove(index);
                continue;
            }
            positions[index].quantity -= remaining;
            remaining = Decimal::ZERO;
            continue;
        }
        index += 1;
    }
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
