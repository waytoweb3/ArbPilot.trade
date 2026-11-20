use arb_core::{
    Compensation, LegStatus, Order, OrderIntent, OrderLegIntent, OrderOrigin, OrderStatus,
    PlaceOrderRequest, Position, Quote, RiskContext, RiskDecision, RiskEngine, Side,
    VenueOrderState, VenueOrderStatus,
};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::{error::ApiError, events::ExecutorEvent, state::AppState, store::PositionGroupMember};

/// 内存里保留的订单条数上限；重启时从库里恢复的历史订单同样按此截断。
pub(crate) const MAX_IN_MEMORY_ORDERS: usize = 500;

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
    /// 触发来源说明（例如上币公告跟随的规则名），随仓位记录以便在仓位列表里展示。
    pub source: Option<String>,
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

/// 交易所最小下单限制对应的最小可下单数量：取「最小名义额 ÷ 价格」按数量精
/// 度向上对齐后的值与「最小数量」中的较大者。
///
/// 数量按精度向下对齐后名义额可能略低于交易所门槛（例如 10 USDC 的请求在 5
/// 位精度的 BTC 上只剩 9.74 USDC），提交前用它判断这一腿是否根本无法下单。
fn venue_minimum_quantity(
    minimum_notional: Option<Decimal>,
    minimum_quantity: Option<Decimal>,
    price: Decimal,
    step: Option<Decimal>,
) -> Option<Decimal> {
    let from_notional = minimum_notional
        .filter(|value| *value > Decimal::ZERO)
        .filter(|_| price > Decimal::ZERO)
        .map(|minimum| {
            let step = step
                .filter(|value| *value > Decimal::ZERO)
                .unwrap_or_else(|| Decimal::new(1, 8));
            (minimum / price / step).ceil() * step
        });
    match (
        from_notional,
        minimum_quantity.filter(|value| *value > Decimal::ZERO),
    ) {
        (Some(from_notional), Some(minimum_quantity)) => Some(from_notional.max(minimum_quantity)),
        (from_notional, minimum_quantity) => from_notional.or(minimum_quantity),
    }
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
            source: None,
            origin: request.origin,
            idempotency_key: request.idempotency_key,
            closing: request.closing,
            max_unhedged_secs: request.max_unhedged_secs,
        },
    )
    .await
}

/// 取消一笔未完成订单；已终态时直接返回，保证桌面端与补偿流程可以重复点击。
pub async fn cancel_order_by_id(state: &AppState, order_id: Uuid) -> Result<Order, ApiError> {
    let mut order = find_order(state, order_id)
        .await
        .ok_or_else(|| ApiError::NotFound("订单不存在".to_string()))?;
    if order.status.is_terminal() {
        return Ok(order);
    }

    let mut errors = Vec::new();
    for (index, leg) in order.legs.iter().enumerate() {
        let Some(venue_order_id) = leg.venue_order_id.clone() else {
            continue;
        };
        if matches!(
            leg.status,
            LegStatus::Filled | LegStatus::Cancelled | LegStatus::Rejected
        ) {
            continue;
        }
        let client_order_id = format!("{}-{index}", order.intent.idempotency_key);
        if let Err(error) = state
            .exchange
            .cancel_order(&leg.intent.venue, &leg.intent.symbol, &venue_order_id, &client_order_id)
            .await
        {
            errors.push(format!("{}: {error}", leg.intent.venue));
        }
    }

    let now = Utc::now();
    for leg in &mut order.legs {
        if matches!(leg.status, LegStatus::Pending | LegStatus::Submitted) {
            leg.status = LegStatus::Cancelled;
        }
    }
    order.status = OrderStatus::Cancelled;
    order.updated_at = now;
    order.note = if errors.is_empty() {
        Some("已撤单".to_string())
    } else {
        Some(format!("部分交易所撤单失败: {}", errors.join("; ")))
    };
    state.store.save_order(&order).await;
    log_event(
        state,
        order.id,
        "order_cancelled",
        json!({ "errors": errors }),
    )
    .await;
    if !errors.is_empty() {
        return Err(ApiError::BadRequest(order.note.clone().unwrap_or_default()));
    }
    Ok(order)
}

/// 改单：先撤原单，再按新的价格/数量重新走完整风控与下单流程。
///
/// 只允许单腿订单改单；双腿订单改单会破坏对冲，必须先撤单再重新开仓。
pub async fn replace_order(
    state: &AppState,
    order_id: Uuid,
    limit_price: Decimal,
    quantity: Option<Decimal>,
) -> Result<Order, ApiError> {
    let order = find_order(state, order_id)
        .await
        .ok_or_else(|| ApiError::NotFound("订单不存在".to_string()))?;
    if order.intent.legs.len() != 1 {
        return Err(ApiError::BadRequest(
            "双腿订单不支持改单，请先撤单后重新下单".to_string(),
        ));
    }
    if order.status.is_terminal() {
        return Err(ApiError::BadRequest("订单已结束，无法改单".to_string()));
    }
    if limit_price <= Decimal::ZERO {
        return Err(ApiError::BadRequest("改单价格必须大于 0".to_string()));
    }
    let intent = &order.intent.legs[0];
    let quantity = quantity.unwrap_or(intent.quantity);
    if quantity <= Decimal::ZERO {
        return Err(ApiError::BadRequest("改单数量必须大于 0".to_string()));
    }

    cancel_order_by_id(state, order_id).await?;

    let outcome = submit_legs(
        state,
        OrderBatchRequest {
            legs: vec![LegOrderRequest {
                venue: intent.venue.clone(),
                symbol: intent.symbol.clone(),
                side: intent.side,
                notional_usd: quantity * limit_price,
                limit_price: Some(limit_price),
                quantity: Some(quantity),
            }],
            leverage: Some(order.intent.leverage),
            max_slippage_bps: Some(order.intent.max_slippage_bps),
            strategy_id: order.intent.strategy_id,
            source: Some("改单".to_string()),
            origin: order.intent.origin,
            idempotency_key: format!(
                "{}-r{}",
                order.intent.idempotency_key,
                Utc::now().timestamp_millis()
            ),
            closing: intent.reduce_only,
            max_unhedged_secs: None,
        },
    )
    .await?;
    Ok(outcome.order)
}

/// 取消全部未完成订单，返回成功撤单数量与失败明细。
pub async fn cancel_all_open_orders(state: &AppState) -> (usize, Vec<String>) {
    let mut ids: Vec<Uuid> = state
        .orders
        .read()
        .expect("订单锁不可用")
        .iter()
        .filter(|order| !order.status.is_terminal())
        .map(|order| order.id)
        .collect();
    // 重启后内存快照可能为空，持久化记录里的未完成订单同样要撤。
    for order in state.store.orders().await {
        if !order.status.is_terminal() && !ids.contains(&order.id) {
            ids.push(order.id);
        }
    }
    let mut cancelled = 0;
    let mut errors = Vec::new();
    for id in ids {
        match cancel_order_by_id(state, id).await {
            Ok(_) => cancelled += 1,
            Err(error) => errors.push(format!("{id}: {error}")),
        }
    }
    (cancelled, errors)
}

/// 从内存快照与持久化记录中查找订单。
pub(crate) async fn find_order(state: &AppState, order_id: Uuid) -> Option<Order> {
    if let Some(order) = state
        .orders
        .read()
        .expect("订单锁不可用")
        .iter()
        .find(|order| order.id == order_id)
        .cloned()
    {
        return Some(order);
    }
    state
        .store
        .orders()
        .await
        .into_iter()
        .find(|order| order.id == order_id)
}

/// 下单回执只代表「交易所已受理」，这里轮询真实状态直到成交/撤单/拒单。
///
/// - 超过 `ARBPILOT_FILL_CONFIRM_SECS` 仍未成交：撤掉挂单并返回 `Open`，让上层按
///   失衡处理，避免「以为成交了、其实还挂着」。
/// - 交易所不支持查单（Lighter 等）：沿用回执即成交，但会把 `leg_unconfirmed`
///   写进事件流，桌面端可见。
async fn confirm_fill(
    state: &AppState,
    venue: &str,
    symbol: &str,
    quantity: Decimal,
    limit_price: Option<Decimal>,
    ack: &arb_core::VenueOrderAck,
    client_order_id: &str,
) -> Result<VenueOrderStatus, String> {
    let timeout = Duration::from_secs(state.config.risk_guard.fill_confirm_secs.max(1));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last: Option<VenueOrderStatus> = None;
    loop {
        match state
            .exchange
            .order_status(venue, symbol, &ack.venue_order_id, client_order_id)
            .await
        {
            Ok(status) => {
                let terminal = matches!(
                    status.state,
                    VenueOrderState::Filled | VenueOrderState::Canceled | VenueOrderState::Rejected
                );
                if terminal {
                    return Ok(status);
                }
                last = Some(status);
            }
            Err(arb_core::ExchangeError::Unsupported(message)) => {
                tracing::warn!(venue, %symbol, %message, "交易所不支持查单，按回执确认成交");
                return Ok(VenueOrderStatus {
                    state: VenueOrderState::Filled,
                    filled_quantity: quantity,
                    average_price: limit_price,
                    fee: None,
                    fee_currency: None,
                });
            }
            Err(error) => return Err(error.to_string()),
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // 超时仍未终态：主动撤单，避免残留挂单在无人值守时成交。
    if let Err(error) = state
        .exchange
        .cancel_order(venue, symbol, &ack.venue_order_id, client_order_id)
        .await
    {
        tracing::warn!(venue, %symbol, %error, "超时撤单失败");
    }
    Ok(last.unwrap_or(VenueOrderStatus {
        state: VenueOrderState::Unknown,
        filled_quantity: Decimal::ZERO,
        average_price: None,
        fee: None,
        fee_currency: None,
    }))
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
                        state.record_reject(&reason);
                        return Err(reject_request(state, &request, reason).await);
                    }
                },
            },
        };
        // 市价腿按对手价成交：买入看卖一，卖出看买一。
        let mut price = leg.limit_price.unwrap_or(if leg.side == Side::Buy {
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
            if let Some(spec) = state
                .market
                .swap_contract_spec(&leg.venue, &leg.symbol)
                .await
            {
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
        // Binance / Aster / Bybit 永续要求数量是步长（stepSize / qtyStep）的整数倍、
        // 价格是价格步长（tickSize）的整数倍，否则会以「精度超限」或「Qty invalid」
        // 拒单；Lighter / Hyperliquid 的签名接口同样按各自精度取整。数量向下对齐到
        // 步长；价格按盘口方向对齐（市价腿买单向上、卖单向下，保证对齐后仍可立即
        // 成交；指定限价则就近对齐）。对齐后的数量与价格写回订单腿。
        if let Some(spec) = state.market.order_step_spec(&leg.venue, &leg.symbol).await {
            if let Some(step) = spec.step_size.filter(|step| *step > Decimal::ZERO) {
                quantity = (quantity / step).floor() * step;
            }
            if let Some(tick) = spec.tick_size.filter(|tick| *tick > Decimal::ZERO) {
                let ticks = if leg.limit_price.is_none() {
                    if leg.side == Side::Buy {
                        (price / tick).ceil()
                    } else {
                        (price / tick).floor()
                    }
                } else {
                    (price / tick).round()
                };
                price = ticks * tick;
            }
            // 数量按精度向下对齐后名义额可能略低于交易所门槛（如 10 USDC 的请求
            // 在 5 位精度的 BTC 上只剩 9.74 USDC）。在提交任何一腿之前就拦下来，
            // 给出满足门槛的最小数量，避免第一腿成交后其余腿被拒导致敞口失衡。
            if !request.closing {
                if let Some(minimum_quantity) = venue_minimum_quantity(
                    spec.min_notional_usd,
                    spec.min_quantity,
                    price,
                    spec.step_size,
                ) {
                    if quantity < minimum_quantity {
                        let reason = format!(
                            "{} {} 单腿名义价值 {} USDC 低于交易所最小下单额，最小可下单 {} ≈ {} USDC；请提高单腿名义价值",
                            leg.venue.to_ascii_uppercase(),
                            leg.symbol,
                            (quantity * price).round_dp(2),
                            minimum_quantity,
                            (minimum_quantity * price).round_dp(2),
                        );
                        return Err(reject_request(state, &request, reason).await);
                    }
                }
            }
        }
        if quantity <= Decimal::ZERO {
            let reason = format!(
                "{} {} 的下单数量 {} 小于交易所最小步长，无法下单",
                leg.venue.to_ascii_uppercase(),
                leg.symbol,
                (leg.notional_usd / price).round_dp(8),
            );
            return Err(reject_request(state, &request, reason).await);
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

    // 现货腿没有杠杆，杠杆只对永续腿生效；整笔都是现货时按 1 倍记录。
    let leverage = if legs
        .iter()
        .all(|leg| crate::market::is_spot_venue(&leg.venue))
    {
        Decimal::ONE
    } else {
        request.leverage.unwrap_or(Decimal::ONE)
    };
    let intent = OrderIntent {
        idempotency_key: request.idempotency_key.clone(),
        origin: request.origin,
        strategy_id: request.strategy_id,
        legs,
        leverage,
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
        // 与风控/仓位记录不一致的仓位。平仓只为降低敞口，不做杠杆调整；现货腿没有杠杆。
        if let (Some(leverage), false, false) = (
            request.leverage,
            request.closing,
            crate::market::is_spot_venue(&venue),
        ) {
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
            if let Err(error) = state
                .exchange
                .set_leverage(&venue, &leg.symbol, leverage)
                .await
            {
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
                failure = Some(format!("第 {} 个交易所设置杠杆失败: {error}", index + 1));
                state.record_reject(&format!("设置杠杆失败: {error}"));
                break;
            }
        }
        let place = PlaceOrderRequest {
            symbol: leg.symbol.clone(),
            side: leg.side,
            quantity: leg.quantity,
            limit_price: leg.limit_price,
            // 现货没有 reduceOnly；平仓就是卖出持仓，订单意图仍按平仓记账。
            reduce_only: leg.reduce_only && !crate::market::is_spot_venue(&venue),
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
        let client_order_id = place.client_order_id.clone();
        let executed = state.exchange.place_order(&venue, place);

        let placed = match (first_fill_at, request.max_unhedged_secs) {
            (Some(filled_at), Some(limit)) => {
                let remaining =
                    limit as i64 - Utc::now().signed_duration_since(filled_at).num_seconds();
                if remaining <= 0 {
                    Err(format!(
                        "第 {} 个交易所超过下单间隔上限 {limit} 秒",
                        index + 1
                    ))
                } else {
                    match tokio::time::timeout(Duration::from_secs(remaining as u64), executed)
                        .await
                    {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(_) => Err(format!("第 {} 个交易所等待成交超时", index + 1)),
                    }
                }
            }
            _ => executed.await.map_err(|error| error.to_string()),
        };

        match placed {
            Ok(ack) => {
                let confirmation = confirm_fill(
                    state,
                    &venue,
                    &leg.symbol,
                    leg.quantity,
                    Some(leg.limit_price),
                    &ack,
                    &client_order_id,
                )
                .await;
                match confirmation {
                    Ok(status) => {
                        let entry = &mut order.legs[index];
                        entry.venue_order_id = Some(ack.venue_order_id.clone());
                        entry.filled_quantity = status.filled_quantity;
                        entry.average_price = status.average_price.or(Some(leg.limit_price));
                        entry.fee = status.fee;
                        entry.fee_currency = status.fee_currency.clone();
                        entry.status = match status.state {
                            VenueOrderState::Filled => LegStatus::Filled,
                            VenueOrderState::PartiallyFilled => LegStatus::PartiallyFilled,
                            VenueOrderState::Canceled => LegStatus::Cancelled,
                            _ => LegStatus::Rejected,
                        };
                        first_fill_at.get_or_insert_with(Utc::now);
                        log_event(
                            state,
                            order.id,
                            "leg_confirmed",
                            json!({
                                "index": index,
                                "venue": &venue,
                                "symbol": &leg.symbol,
                                "side": leg.side,
                                "venue_order_id": &ack.venue_order_id,
                                "state": status.state,
                                "filled_quantity": status.filled_quantity,
                                "average_price": status.average_price,
                                "fee": status.fee,
                                "fee_currency": status.fee_currency,
                            }),
                        )
                        .await;
                        if entry.status == LegStatus::Filled {
                            state.record_success();
                        } else {
                            state.record_reject("订单未完全成交");
                            failure = Some(format!(
                                "第 {} 个交易所订单未完全成交: {:?}",
                                index + 1,
                                status.state
                            ));
                            break;
                        }
                    }
                    Err(error) => {
                        // 查单失败不等于没成交：保留交易所订单号，交给启动对账处理。
                        order.legs[index].venue_order_id = Some(ack.venue_order_id.clone());
                        order.legs[index].status = LegStatus::Submitted;
                        order.legs[index].filled_quantity = leg.quantity;
                        order.legs[index].average_price = Some(leg.limit_price);
                        state.record_reject(&error);
                        log_event(
                            state,
                            order.id,
                            "leg_unconfirmed",
                            json!({
                                "index": index,
                                "venue": &venue,
                                "venue_order_id": &ack.venue_order_id,
                                "error": &error,
                            }),
                        )
                        .await;
                        failure = Some(format!("第 {} 个交易所成交确认失败: {error}", index + 1));
                        break;
                    }
                }
            }
            Err(error) => {
                order.legs[index].status = LegStatus::Rejected;
                state.record_reject(&error);
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
                failure = Some(format!("第 {} 个交易所下单失败: {error}", index + 1));
                break;
            }
        }
    }

    if let Some(message) = failure {
        let has_filled_leg = order.legs.iter().any(|leg| leg.is_filled());
        let compensation = OrderStatus::PartiallyFilled.compensation(&order.legs);
        // 单腿订单（例如策略/上币单腿交易）失败时没有剩余腿可撤，备注只保留真实原因。
        order.note = Some(match compensation {
            Compensation::None => message,
            _ => format!("{message}；补偿动作: {compensation:?}"),
        });
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
                source: request.source.clone(),
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

    // 双腿开仓把成员登记到持久化的仓位组，重启后仍能把交易所持仓还原回同一组。
    if !request.closing && order.intent.legs.len() > 1 {
        let members = order
            .legs
            .iter()
            .filter(|leg| leg.is_filled())
            .map(|leg| PositionGroupMember {
                strategy_id: order.intent.strategy_id,
                source: request.source.clone(),
                venue: leg.intent.venue.clone(),
                symbol: leg.intent.symbol.clone(),
                side: leg.intent.side,
            })
            .collect::<Vec<_>>();
        if !members.is_empty() {
            state.store.save_position_group(order.id, &members).await;
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

#[cfg(test)]
mod tests {
    use super::venue_minimum_quantity;
    use rust_decimal_macros::dec;

    #[test]
    fn minimum_quantity_rounds_up_to_step() {
        // Lighter BTC：最小 10 USDC、数量精度 1e-5，10 USDC 只够 0.00013 → 需向上取到 0.00014。
        let minimum = venue_minimum_quantity(
            Some(dec!(10)),
            Some(dec!(0.00007)),
            dec!(74936.1),
            Some(dec!(0.00001)),
        );
        assert_eq!(minimum, Some(dec!(0.00014)));
    }

    #[test]
    fn minimum_quantity_takes_larger_of_notional_and_quantity() {
        // 最小数量换算出的名义额更大时以数量为准。
        let minimum = venue_minimum_quantity(Some(dec!(10)), Some(dec!(0.001)), dec!(60000), None);
        assert_eq!(minimum, Some(dec!(0.001)));
    }

    #[test]
    fn minimum_quantity_is_none_without_venue_limits() {
        assert_eq!(venue_minimum_quantity(None, None, dec!(60000), None), None);
    }
}
