use std::{
    sync::RwLock,
    time::Duration,
};

use arb_core::OrderOrigin;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError,
    events::{AlertLevel, ExecutorEvent},
    gateway::{self, PairOrderRequest},
    state::AppState,
};

/// 条件单的轮询间隔：过期检查与触发判断都在这个节拍上完成。
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// 条件单生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalStatus {
    /// 等待触发。
    Active,
    /// 已触发并提交订单。
    Triggered,
    /// 超过有效期自动失效。
    Expired,
    /// 用户手动撤销。
    Cancelled,
    /// 触发后下单被拒绝或执行失败。
    Rejected,
}

/// 触发条件的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// 两腿净资金费率（空腿 − 多腿）的年化百分比。
    Funding,
    /// 多腿交易所的中间价。
    Price,
}

/// 触发方向：上穿阈值或下破阈值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerDirection {
    Above,
    Below,
}

/// 一张等待触发的双腿条件单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalOrder {
    pub id: Uuid,
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    pub notional_usd: Decimal,
    pub leverage: Decimal,
    pub trigger_kind: TriggerKind,
    pub trigger_direction: TriggerDirection,
    /// 资金费率触发时为年化百分比，价格触发时为报价。
    pub trigger_value: Decimal,
    /// 有效期；为空表示长期有效。
    pub expires_at: Option<DateTime<Utc>>,
    pub status: ConditionalStatus,
    pub note: Option<String>,
    /// 触发后生成的订单。
    pub triggered_order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建条件单的请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateConditionalRequest {
    pub symbol: String,
    pub long_venue: String,
    pub short_venue: String,
    pub notional_usd: Decimal,
    #[serde(default = "default_leverage")]
    pub leverage: Decimal,
    pub trigger_kind: TriggerKind,
    pub trigger_direction: TriggerDirection,
    pub trigger_value: Decimal,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_leverage() -> Decimal {
    Decimal::ONE
}

/// 条件单内存表；持久化由 `Store` 负责。
#[derive(Default)]
pub struct ConditionalOrders {
    orders: RwLock<Vec<ConditionalOrder>>,
}

impl ConditionalOrders {
    pub fn with_orders(orders: Vec<ConditionalOrder>) -> Self {
        Self {
            orders: RwLock::new(orders),
        }
    }

    /// 全部条件单，按创建时间倒序。
    pub fn list(&self) -> Vec<ConditionalOrder> {
        let mut orders = self.orders.read().expect("条件单锁不可用").clone();
        orders.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        orders
    }

    fn active(&self) -> Vec<ConditionalOrder> {
        self.orders
            .read()
            .expect("条件单锁不可用")
            .iter()
            .filter(|order| order.status == ConditionalStatus::Active)
            .cloned()
            .collect()
    }

    fn insert(&self, order: ConditionalOrder) {
        self.orders.write().expect("条件单锁不可用").push(order);
    }

    fn update(
        &self,
        id: Uuid,
        change: impl FnOnce(&mut ConditionalOrder),
    ) -> Option<ConditionalOrder> {
        let mut orders = self.orders.write().expect("条件单锁不可用");
        let order = orders.iter_mut().find(|order| order.id == id)?;
        change(order);
        order.updated_at = Utc::now();
        Some(order.clone())
    }

    fn find(&self, id: Uuid) -> Option<ConditionalOrder> {
        self.orders
            .read()
            .expect("条件单锁不可用")
            .iter()
            .find(|order| order.id == id)
            .cloned()
    }
}

/// 创建条件单：校验参数后写入内存并落库。
pub async fn create(
    state: &AppState,
    request: CreateConditionalRequest,
) -> Result<ConditionalOrder, ApiError> {
    let symbol = request.symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        return Err(ApiError::BadRequest("交易对不能为空".to_string()));
    }
    if request.long_venue.trim().is_empty() || request.short_venue.trim().is_empty() {
        return Err(ApiError::BadRequest("双腿交易所不能为空".to_string()));
    }
    if request.long_venue == request.short_venue {
        return Err(ApiError::BadRequest("双腿交易所不能相同".to_string()));
    }
    if request.notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("notional_usd 必须大于 0".to_string()));
    }
    if request.leverage <= Decimal::ZERO {
        return Err(ApiError::BadRequest("leverage 必须大于 0".to_string()));
    }
    let now = Utc::now();
    if let Some(expires_at) = request.expires_at {
        if expires_at <= now {
            return Err(ApiError::BadRequest("有效期必须晚于当前时间".to_string()));
        }
    }

    let order = ConditionalOrder {
        id: Uuid::new_v4(),
        symbol,
        long_venue: request.long_venue.trim().to_string(),
        short_venue: request.short_venue.trim().to_string(),
        notional_usd: request.notional_usd,
        leverage: request.leverage,
        trigger_kind: request.trigger_kind,
        trigger_direction: request.trigger_direction,
        trigger_value: request.trigger_value,
        expires_at: request.expires_at,
        status: ConditionalStatus::Active,
        note: None,
        triggered_order_id: None,
        created_at: now,
        updated_at: now,
    };
    state.conditional.insert(order.clone());
    state.store.save_conditional_order(&order).await;
    Ok(order)
}

/// 撤销仍处于等待状态的条件单。
pub async fn cancel(state: &AppState, id: Uuid) -> Result<ConditionalOrder, ApiError> {
    let Some(order) = state.conditional.find(id) else {
        return Err(ApiError::NotFound("条件单不存在".to_string()));
    };
    if order.status != ConditionalStatus::Active {
        return Err(ApiError::BadRequest("只有等待中的条件单可以撤销".to_string()));
    }
    let Some(order) = state.conditional.update(id, |order| {
        order.status = ConditionalStatus::Cancelled;
    }) else {
        return Err(ApiError::NotFound("条件单不存在".to_string()));
    };
    state.store.save_conditional_order(&order).await;
    Ok(order)
}

/// 启动条件单监控：过期检查 + 触发判断 + 自动下单。
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            evaluate(&state).await;
        }
    });
}

/// 单轮评估：逐张检查有效期与触发条件。
async fn evaluate(state: &AppState) {
    let pending = state.conditional.active();
    if pending.is_empty() {
        return;
    }

    // 资金费率状态只在需要时拉取一次，避免每张条件单都读一遍缓存。
    let mut funding: Option<Vec<crate::funding::FundingStatus>> = None;
    for order in pending {
        let now = Utc::now();
        if order
            .expires_at
            .is_some_and(|expires_at| expires_at <= now)
        {
            if let Some(expired) = state.conditional.update(order.id, |order| {
                order.status = ConditionalStatus::Expired;
                order.note = Some("已超过有效期，条件单自动失效".to_string());
            }) {
                state.store.save_conditional_order(&expired).await;
            }
            continue;
        }
        // 暂停开仓时条件单继续等待，恢复后按最新行情重新判断。
        if state.is_paused() {
            continue;
        }

        let observed = match order.trigger_kind {
            TriggerKind::Price => observed_price(state, &order),
            TriggerKind::Funding => {
                let statuses = match &funding {
                    Some(statuses) => statuses,
                    None => {
                        funding = Some(state.funding.status().await);
                        funding.as_ref().expect("资金费率状态已写入")
                    }
                };
                observed_funding(statuses, &order)
            }
        };
        let Some(observed) = observed else {
            continue;
        };
        if !exceeded(observed, order.trigger_direction, order.trigger_value) {
            continue;
        }

        // 先落状态再下单，避免同一轮或异常重启后重复触发。
        let Some(triggered) = state.conditional.update(order.id, |order| {
            order.status = ConditionalStatus::Triggered;
            order.note = Some(format!("触发值 {observed}"));
        }) else {
            continue;
        };
        state.store.save_conditional_order(&triggered).await;

        let outcome = gateway::submit_pair(
            state,
            PairOrderRequest {
                symbol: triggered.symbol.clone(),
                long_venue: triggered.long_venue.clone(),
                short_venue: triggered.short_venue.clone(),
                notional_usd: triggered.notional_usd,
                leverage: Some(triggered.leverage),
                max_slippage_bps: None,
                strategy_id: None,
                origin: OrderOrigin::Desktop,
                idempotency_key: format!("conditional-{}", triggered.id),
                closing: false,
                max_unhedged_secs: None,
            },
        )
        .await;

        let (status, note, order_id) = match outcome {
            Ok(outcome) => (
                ConditionalStatus::Triggered,
                outcome.order.note.clone(),
                Some(outcome.order.id),
            ),
            Err(error) => (ConditionalStatus::Rejected, Some(error.to_string()), None),
        };
        if let Some(updated) = state.conditional.update(triggered.id, |order| {
            order.status = status;
            order.note = note.clone();
            order.triggered_order_id = order_id;
        }) {
            state.store.save_conditional_order(&updated).await;
        }
        let message = match status {
            ConditionalStatus::Triggered => format!(
                "条件单已触发: {} {}→{}",
                triggered.symbol, triggered.long_venue, triggered.short_venue
            ),
            ConditionalStatus::Rejected => format!("条件单触发失败: {}", note.unwrap_or_default()),
            _ => String::new(),
        };
        let _ = state.events.send(ExecutorEvent::Alert {
            level: if status == ConditionalStatus::Triggered {
                AlertLevel::Info
            } else {
                AlertLevel::Warning
            },
            message,
            message_en: None,
            at: Utc::now(),
        });
    }
}

/// 价格触发：取多腿交易所的中间价。
fn observed_price(state: &AppState, order: &ConditionalOrder) -> Option<Decimal> {
    let quote = state.market.quote(&order.long_venue, &order.symbol)?;
    Some((quote.bid + quote.ask) / Decimal::from(2))
}

/// 资金费率触发：空腿年化 − 多腿年化。
fn observed_funding(
    statuses: &[crate::funding::FundingStatus],
    order: &ConditionalOrder,
) -> Option<Decimal> {
    let annualized = |venue: &str| {
        statuses
            .iter()
            .find(|status| status.venue == venue)?
            .symbols
            .iter()
            .find(|entry| entry.symbol == order.symbol)?
            .annualized_pct
    };
    Some(annualized(&order.short_venue)? - annualized(&order.long_venue)?)
}

fn exceeded(observed: Decimal, direction: TriggerDirection, threshold: Decimal) -> bool {
    match direction {
        TriggerDirection::Above => observed >= threshold,
        TriggerDirection::Below => observed <= threshold,
    }
}
