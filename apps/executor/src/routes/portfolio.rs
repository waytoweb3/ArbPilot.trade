use arb_core::{OrderOrigin, OrderStatus, Position, Side};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    error::ApiError,
    gateway::{self, LegOrderRequest, OrderBatchRequest},
    routes::orders::CreateOrderResponse,
    state::AppState,
};

pub async fn positions(State(state): State<AppState>) -> Json<Vec<Position>> {
    let mut positions = state.positions.read().expect("仓位锁不可用").clone();
    // 读取时再叠加一次最新 WebSocket 盘口，确保响应里的价格与推送一致。
    state.apply_realtime_prices(&mut positions);
    Json(positions)
}

/// 一键平仓：按当前仓位反向提交只减仓的市价单。
///
/// 数量取交易所同步回来的持仓，价格用最新盘口；平仓只降低敞口，
/// 不受开仓限额与暂停状态限制。
pub async fn close(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<CreateOrderResponse>), ApiError> {
    let position = {
        let mut positions = state.positions.write().expect("仓位锁不可用");
        state.apply_realtime_prices(&mut positions);
        positions
            .iter()
            .find(|position| position.id == id)
            .cloned()
    };
    let Some(position) = position else {
        return Err(ApiError::NotFound("仓位不存在或已平仓".to_string()));
    };

    let side = match position.side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    };
    let notional_usd = position.quantity * position.mark_price;
    if notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("仓位数量为 0，无法平仓".to_string()));
    }

    let outcome = gateway::submit_legs(
        &state,
        OrderBatchRequest {
            legs: vec![LegOrderRequest {
                venue: position.venue.clone(),
                symbol: position.symbol.clone(),
                side,
                notional_usd,
                limit_price: None,
                // 用交易所同步回来的持仓数量，避免按名义价值换算后留下零头。
                quantity: Some(position.quantity),
            }],
            leverage: Some(position.leverage),
            max_slippage_bps: None,
            strategy_id: position.strategy_id,
            source: None,
            origin: OrderOrigin::Desktop,
            idempotency_key: format!("close-{}", Uuid::new_v4()),
            closing: true,
            max_unhedged_secs: None,
        },
    )
    .await?;

    let status = match outcome.order.status {
        OrderStatus::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
        OrderStatus::Filled => StatusCode::CREATED,
        _ => StatusCode::OK,
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

/// 整组平仓：把同一 `group_id` 的双腿仓位一次性反向平掉。
///
/// 组内任一腿缺失说明组合已经不平衡，此时拒绝执行，避免平掉单腿留下裸敞口。
pub async fn close_group(
    State(state): State<AppState>,
    Path(group_id): Path<Uuid>,
) -> Result<(StatusCode, Json<CreateOrderResponse>), ApiError> {
    let members = {
        let mut positions = state.positions.write().expect("仓位锁不可用");
        state.apply_realtime_prices(&mut positions);
        positions
            .iter()
            .filter(|position| position.group_id == Some(group_id))
            .cloned()
            .collect::<Vec<_>>()
    };
    if members.is_empty() {
        return Err(ApiError::NotFound("仓位组不存在或已平仓".to_string()));
    }

    let mut legs = Vec::with_capacity(members.len());
    for position in &members {
        let notional_usd = position.quantity * position.mark_price;
        if notional_usd <= Decimal::ZERO {
            return Err(ApiError::BadRequest(format!(
                "{} {} 仓位数量为 0，无法平仓",
                position.venue, position.symbol
            )));
        }
        legs.push(LegOrderRequest {
            venue: position.venue.clone(),
            symbol: position.symbol.clone(),
            side: match position.side {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            },
            notional_usd,
            limit_price: None,
            quantity: Some(position.quantity),
        });
    }

    let outcome = gateway::submit_legs(
        &state,
        OrderBatchRequest {
            legs,
            // 同一批下单共用一个杠杆，取组内最大值以免低估保证金需求。
            leverage: Some(
                members
                    .iter()
                    .map(|position| position.leverage)
                    .max()
                    .unwrap_or(Decimal::ONE),
            ),
            max_slippage_bps: None,
            strategy_id: members.iter().find_map(|position| position.strategy_id),
            source: None,
            origin: OrderOrigin::Desktop,
            idempotency_key: format!("close-group-{}", Uuid::new_v4()),
            closing: true,
            max_unhedged_secs: None,
        },
    )
    .await?;

    let status = match outcome.order.status {
        OrderStatus::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
        OrderStatus::Filled => StatusCode::CREATED,
        _ => StatusCode::OK,
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
