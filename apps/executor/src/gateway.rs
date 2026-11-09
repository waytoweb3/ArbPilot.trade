use arb_core::{
    LegStatus, Order, OrderIntent, OrderLegIntent, OrderOrigin, OrderStatus, PlaceOrderRequest,
    Position, Quote, RiskContext, RiskDecision, RiskEngine, Side,
};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::{error::ApiError, events::ExecutorEvent, state::AppState};

const MAX_IN_MEMORY_ORDERS: usize = 500;

/// 单腿下单请求。方向由 `side` 决定，价格为空时按当前对手价成交。
#[derive(Debug, Clone)]
pub struct LegOrderRequest {
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    /// 该腿的名义价值（美元）。
    pub notional_usd: Decimal,
    /// 指定成交价；为空表示市价腿，用当前对手价作为保护价。
    pub limit_price: Option<Decimal>,
    /// 指定下单数量（币）；平仓时直接使用持仓数量，避免按价格换算后
    /// 因盘口与标记价的差异少平或多平。
    pub quantity: Option<Decimal>,
}

/// 一次提交的订单：可以是单腿、配对双腿，或其他多腿组合。
#[derive(Debug, Clone)]
pub struct OrderBatchRequest {
    pub legs: Vec<LegOrderRequest>,
    /// 要设置到交易所的杠杆；`None` 表示沿用交易所当前设置，
    /// 风控与仓位记录仍按 1 倍处理。
    pub leverage: Option<Decimal>,
    pub max_slippage_bps: Option<Decimal>,
    pub strategy_id: Option<Uuid>,
    pub origin: OrderOrigin,
    pub idempotency_key: String,
    /// 平仓：反向操作、reduce_only，并减少已有仓位。
    pub closing: bool,
    /// 第一腿成交后，其余腿必须在该时限内完成，超时按失衡处理。
    pub max_unhedged_secs: Option<u64>,
}

/// 配对下单请求。桌面端、Telegram 与内置资金费率策略都走这里。
#[derive(Debug, Clone)]
pub struct PairOrderRequest {
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    /// 每条腿的名义价值（美元）。
    pub notional_usd: Decimal,
    /// 要设置到交易所的杠杆；`None` 表示沿用交易所当前设置。
    pub leverage: Option<Decimal>,
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

fn origin_label(origin: OrderOrigin) -> &'static str {
    match origin {
        OrderOrigin::Desktop => "desktop",
        OrderOrigin::Telegram => "telegram",
        OrderOrigin::Strategy => "strategy",
    }
}

/// 报价是否可直接用于风控：观测时间不能超前，且延迟不超过上限（口径与风控一致）。
fn is_quote_fresh(quote: &Quote, max_age_secs: i64) -> bool {
    let age = Utc::now()
        .signed_duration_since(quote.observed_at)
        .num_seconds();
    (0..=max_age_secs).contains(&age)
}

/// 订单动作统一落库到 `order_events`；数据库不可用时 Store 内部退化为本地日志。
async fn log_event(state: &AppState, order_id: Uuid, event_type: &str, detail: serde_json::Value) {
    state
        .store
        .append_order_event(order_id, event_type, detail)
        .await;
}

/// 尚未生成订单就被拒绝的请求写入 `audit_log`，保证每次下单尝试都有痕迹。
async fn reject_request(
    state: &AppState,
    request: &OrderBatchRequest,
    reason: impl Into<String>,
) -> ApiError {
    let reason = reason.into();
    let symbols = request
        .legs
        .iter()
        .map(|leg| leg.symbol.as_str())
        .collect::<Vec<_>>()
        .join(",");
    state
        .store
        .append_audit_log(
            origin_label(request.origin),
            "order.rejected",
            Some(symbols.as_str()).filter(|value| !value.is_empty()),
            json!({
                "reason": &reason,
                "idempotency_key": &request.idempotency_key,
                "closing": request.closing,
                "leverage": request.leverage,
                "legs": request.legs.iter().map(|leg| json!({
                    "venue": &leg.venue,
                    "symbol": &leg.symbol,
                    "side": leg.side,
                    "notional_usd": leg.notional_usd,
                    "limit_price": leg.limit_price,
                })).collect::<Vec<_>>(),
            }),
        )
        .await;
    ApiError::BadRequest(reason)
}

/// 配对入口：翻译成两条腿后复用统一的逐腿提交。
///
/// 策略执行器只能调用网关，不能直接访问交易所或密钥。
pub async fn submit_pair(
    state: &AppState,
    request: PairOrderRequest,
) -> Result<PairOrderOutcome, ApiError> {
    if request.long_venue == request.short_venue {
        state
            .store
            .append_audit_log(
                origin_label(request.origin),
                "order.rejected",
                Some(request.symbol.as_str()),
                json!({
                    "reason": "双腿交易所不能相同",
                    "idempotency_key": &request.idempotency_key,
                    "long_venue": &request.long_venue,
                    "short_venue": &request.short_venue,
                }),
            )
            .await;
        return Err(ApiError::BadRequest("双腿交易所不能相同".to_string()));
    }
    // 开仓：买多头腿、卖空头腿；平仓：反向操作且 reduce_only。
    let (long_side, short_side) = if request.closing {
        (Side::Sell, Side::Buy)
    } else {
        (Side::Buy, Side::Sell)
    };

    submit_legs(
        state,
        OrderBatchRequest {
            legs: vec![
                LegOrderRequest {
                    venue: request.long_venue,
                    symbol: request.symbol.clone(),
                    side: long_side,
                    notional_usd: request.notional_usd,
                    limit_price: None,
                    quantity: None,
                },
                LegOrderRequest {
                    venue: request.short_venue,
                    symbol: request.symbol,
                    side: short_side,
                    notional_usd: request.notional_usd,
                    limit_price: None,
                    quantity: None,
                },
            ],
            leverage: request.leverage,
            max_slippage_bps: request.max_slippage_bps,
            strategy_id: request.strategy_id,
            origin: request.origin,
            idempotency_key: request.idempotency_key,
            closing: request.closing,
            max_unhedged_secs: request.max_unhedged_secs,
        },
    )
    .await
}

/// 统一订单入口：幂等检查 → 行情校验 → 风控 → 实盘下单 → 仓位记账。
///
/// 单腿与多腿共用同一条路径，全部经过统一风控后才能到达交易所。
pub async fn submit_legs(
    state: &AppState,
    request: OrderBatchRequest,
) -> Result<PairOrderOutcome, ApiError> {
    if request.legs.is_empty() {
        return Err(reject_request(state, &request, "至少需要一条订单腿").await);
    }
    for leg in &request.legs {
        if leg.notional_usd <= Decimal::ZERO {
            return Err(reject_request(state, &request, "notional_usd 必须大于 0").await);
        }
    }

    let existing_order_id = state
        .idempotency
        .read()
        .expect("幂等锁不可用")
        .get(&request.idempotency_key)
        .copied();
    if let Some(existing_id) = existing_order_id {
        let existing = state
            .orders
            .read()
            .expect("订单锁不可用")
            .iter()
            .find(|order| order.id == existing_id)
            .cloned();
        if let Some(order) = existing {
            log_event(
                state,
                order.id,
                "idempotent_replay",
                json!({
                    "idempotency_key": &request.idempotency_key,
                    "status": order.status,
                }),
            )
            .await;
            return Ok(PairOrderOutcome {
                order,
                decision: None,
                replayed: true,
            });
        }
    }

    let limits = state.risk_limits();
    let mut observed_at = Utc::now();
    let mut legs = Vec::with_capacity(request.legs.len());
    for leg in &request.legs {
        let cached = state.market.quote(&leg.venue, &leg.symbol);
        let quote = match cached {
            // 缓存报价在风控允许的延迟内，直接使用，省掉一次 REST 往返。
            Some(quote) if is_quote_fresh(&quote, limits.max_quote_age_secs) => quote,
            // 轮询缓存可能因采集节奏、WebSocket 只在价格变化时推送，或交易对不在
            // 采集清单内而超过延迟上限；下单前向交易所取一次最新盘口再交给风控。
            cached => match state.market.refresh_quote(&leg.venue, &leg.symbol).await {
                Ok(quote) => quote,
                Err(error) => match cached {
                    Some(quote) => {
                        tracing::warn!(
                            venue = %leg.venue,
                            symbol = %leg.symbol,
                            %error,
                            "刷新盘口失败，沿用缓存报价"
                        );
                        quote
                    }
                    None => {
                        let reason =
                            format!("{} 在 {} 没有可用行情: {error}", leg.symbol, leg.venue);
                        return Err(reject_request(state, &request, reason).await);
                    }
                },
            },
        };
        // 市价腿按对手价成交：买入看卖一，卖出看买一。
        let price = leg.limit_price.unwrap_or(if leg.side == Side::Buy {
            quote.ask
        } else {
            quote.bid
        });
        if price <= Decimal::ZERO {
            let reason = format!("{} 在 {} 的成交价无效", leg.symbol, leg.venue);
            return Err(reject_request(state, &request, reason).await);
        }
        let mut quantity = match leg.quantity {
            Some(quantity) => quantity,
            None => (leg.notional_usd / price).round_dp(6),
        };
        if quantity <= Decimal::ZERO {
            return Err(reject_request(state, &request, "下单数量过小").await);
        }
        // OKX 永续按「张」撮合：数量需向下对齐到 lotSz 的整数倍（BTC-USDT-SWAP
        // 为 0.01 张 = 0.0001 BTC），低于 minSz 时交易所会直接拒单。风控只看名义额，
        // 这里提前拦截并给出可执行的下限提示；对齐后的数量同时写回订单腿，
        // 避免本地记录与实际成交量不一致。
        if !request.closing {
            if let Some(spec) = state.market.swap_contract_spec(&leg.venue, &leg.symbol).await {
                let contracts = ((quantity / spec.ct_val) / spec.lot_sz).floor() * spec.lot_sz;
                if contracts < spec.min_sz {
                    let reason = format!(
                        "{} {} 最小下单 {} 张（每张 {}，约 {} USDT）；当前委托约 {} USDT",
                        leg.venue.to_ascii_uppercase(),
                        leg.symbol,
                        spec.min_sz,
                        spec.ct_val,
                        (spec.min_sz * spec.ct_val * price).round_dp(2),
                        (quantity * price).round_dp(2),
                    );
                    return Err(reject_request(state, &request, reason).await);
                }
                quantity = contracts * spec.ct_val;
            }
        }
        observed_at = observed_at.min(quote.observed_at);
        legs.push(OrderLegIntent {
            venue: leg.venue.clone(),
            symbol: leg.symbol.clone(),
            side: leg.side,
            quantity,
            limit_price: price,
            reduce_only: request.closing,
        });
    }

    let intent = OrderIntent {
        idempotency_key: request.idempotency_key.clone(),
        origin: request.origin,
        strategy_id: request.strategy_id,
        legs,
        leverage: request.leverage.unwrap_or(Decimal::ONE),
        max_slippage_bps: request.max_slippage_bps.unwrap_or(limits.max_slippage_bps),
        quote_observed_at: observed_at,
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
        log_event(
            state,
            order.id,
            "risk_rejected",
            json!({
                "status": order.status,
                "origin": origin_label(order.intent.origin),
                "strategy_id": order.intent.strategy_id,
                "idempotency_key": &order.intent.idempotency_key,
                "note": &order.note,
                "violations": violations,
            }),
        )
        .await;
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
    log_event(
        state,
        order.id,
        "order_submitted",
        json!({
            "status": order.status,
            "origin": origin_label(order.intent.origin),
            "strategy_id": order.intent.strategy_id,
            "idempotency_key": &order.intent.idempotency_key,
            "closing": request.closing,
            "leverage": order.intent.leverage,
            "max_slippage_bps": order.intent.max_slippage_bps,
            "quote_observed_at": order.intent.quote_observed_at,
            "legs": order.legs.iter().map(|leg| json!({
                "venue": &leg.intent.venue,
                "symbol": &leg.intent.symbol,
                "side": leg.intent.side,
                "quantity": leg.intent.quantity,
                "limit_price": leg.intent.limit_price,
                "reduce_only": leg.intent.reduce_only,
            })).collect::<Vec<_>>(),
        }),
    )
    .await;

    let mut failure: Option<String> = None;
    let mut first_fill_at: Option<DateTime<Utc>> = None;
    for index in 0..order.intent.legs.len() {
        let leg = order.intent.legs[index].clone();
        let venue = leg.venue.clone();
        // 开仓前先把杠杆写到交易所；设置失败则不下单，避免按交易所默认杠杆开出
        // 与风控/仓位记录不一致的仓位。平仓只为降低敞口，不做杠杆调整。
        if let (Some(leverage), false) = (request.leverage, request.closing) {
            log_event(
                state,
                order.id,
                "leverage_submitting",
                json!({
                    "index": index,
                    "venue": &venue,
                    "symbol": &leg.symbol,
                    "leverage": leverage,
                }),
            )
            .await;
            if let Err(error) = state.exchange.set_leverage(&venue, &leg.symbol, leverage).await {
                order.legs[index].status = LegStatus::Rejected;
                log_event(
                    state,
                    order.id,
                    "leg_leverage_failed",
                    json!({
                        "index": index,
                        "venue": &venue,
                        "symbol": &leg.symbol,
                        "leverage": leverage,
                        "error": error.to_string(),
                    }),
                )
                .await;
                failure = Some(format!("第 {} 腿设置杠杆失败: {error}", index + 1));
                break;
            }
        }
        let place = PlaceOrderRequest {
            symbol: leg.symbol.clone(),
            side: leg.side,
            quantity: leg.quantity,
            limit_price: leg.limit_price,
            reduce_only: leg.reduce_only,
            client_order_id: format!("{}-{index}", request.idempotency_key),
            leverage: request.leverage,
        };
        // 每条腿都按自己的交易所取凭证并签名，同一笔订单可以跨交易所。
        log_event(
            state,
            order.id,
            "leg_submitting",
            json!({
                "index": index,
                "venue": &venue,
                "symbol": &leg.symbol,
                "side": leg.side,
                "quantity": leg.quantity,
                "limit_price": leg.limit_price,
                "reduce_only": leg.reduce_only,
                "client_order_id": &place.client_order_id,
            }),
        )
        .await;
        let executed = state.exchange.place_order(&venue, place);

        let placed = match (first_fill_at, request.max_unhedged_secs) {
            (Some(filled_at), Some(limit)) => {
                let remaining =
                    limit as i64 - Utc::now().signed_duration_since(filled_at).num_seconds();
                if remaining <= 0 {
                    Err(format!("第 {} 腿超过双腿间隔上限 {limit} 秒", index + 1))
                } else {
                    match tokio::time::timeout(Duration::from_secs(remaining as u64), executed)
                        .await
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
                entry.venue_order_id = Some(ack.venue_order_id.clone());
                entry.filled_quantity = leg.quantity;
                entry.average_price = Some(leg.limit_price);
                first_fill_at.get_or_insert_with(Utc::now);
                log_event(
                    state,
                    order.id,
                    "leg_filled",
                    json!({
                        "index": index,
                        "venue": &venue,
                        "symbol": &leg.symbol,
                        "side": leg.side,
                        "venue_order_id": &ack.venue_order_id,
                        "filled_quantity": leg.quantity,
                        "average_price": leg.limit_price,
                    }),
                )
                .await;
            }
            Err(error) => {
                order.legs[index].status = LegStatus::Rejected;
                log_event(
                    state,
                    order.id,
                    "leg_failed",
                    json!({
                        "index": index,
                        "venue": &venue,
                        "symbol": &leg.symbol,
                        "side": leg.side,
                        "quantity": leg.quantity,
                        "limit_price": leg.limit_price,
                        "error": &error,
                    }),
                )
                .await;
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
            state.set_paused(
                true,
                Some((
                    "双腿失衡，等待人工确认补偿",
                    "legs out of balance; manual compensation required",
                )),
            );
        }
        state.store.save_order(&order).await;
        log_event(
            state,
            order.id,
            if has_filled_leg {
                "order_unwinding"
            } else {
                "order_failed"
            },
            json!({
                "status": order.status,
                "note": &order.note,
                "compensation": format!("{compensation:?}"),
                "paused": has_filled_leg,
                "legs": order.legs.iter().map(|leg| json!({
                    "venue": &leg.intent.venue,
                    "symbol": &leg.intent.symbol,
                    "side": leg.intent.side,
                    "status": leg.status,
                    "venue_order_id": &leg.venue_order_id,
                    "filled_quantity": leg.filled_quantity,
                    "average_price": leg.average_price,
                })).collect::<Vec<_>>(),
            }),
        )
        .await;
        state.emit_order(&order);
        let _ = state.events.send(ExecutorEvent::order_transition(
            &order,
            format!("订单 {} 状态: {:?}", order.id, order.status),
            format!("Order {} status: {:?}", order.id, order.status),
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
                // 双腿订单产生的仓位共享订单 ID，便于整组查看与平仓。
                group_id: (order.intent.legs.len() > 1).then_some(order.id),
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
    log_event(
        state,
        order.id,
        "order_filled",
        json!({
            "status": order.status,
            "closing": request.closing,
            "legs": order.legs.iter().map(|leg| json!({
                "venue": &leg.intent.venue,
                "symbol": &leg.intent.symbol,
                "side": leg.intent.side,
                "venue_order_id": &leg.venue_order_id,
                "filled_quantity": leg.filled_quantity,
                "average_price": leg.average_price,
            })).collect::<Vec<_>>(),
        }),
    )
    .await;
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
