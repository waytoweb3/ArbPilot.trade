//! 策略市场配置：桌面端只从这里读取可选的交易所与交易对。

use std::collections::BTreeMap;

use axum::{extract::State, Json};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{
    error::ApiError,
    news,
    state::AppState,
    store::{ExchangeVenueConfig, NotificationSettings},
};

#[derive(Debug, Serialize)]
pub struct MarketConfigResponse {
    pub venues: Vec<ExchangeVenueConfig>,
    /// 按交易所分组的交易对；现货与永续共用同一份清单。
    pub symbols: BTreeMap<String, Vec<String>>,
}

pub async fn market(State(state): State<AppState>) -> Json<MarketConfigResponse> {
    let venues = state.store.exchange_venues().await;
    let mut symbols: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in state.store.tradable_symbols().await {
        symbols.entry(row.venue).or_default().push(row.symbol);
    }
    Json(MarketConfigResponse { venues, symbols })
}

/// 消息通知配置：新闻推送与资金费率套利提醒的开关、阈值与渠道。
pub async fn notifications(State(state): State<AppState>) -> Json<NotificationSettings> {
    Json(state.store.notification_settings().await)
}

/// 保存通知配置：保存后立即对后续推送生效，无需重启执行器。
pub async fn update_notifications(
    State(state): State<AppState>,
    Json(mut body): Json<NotificationSettings>,
) -> Result<Json<NotificationSettings>, ApiError> {
    if body.funding_min_annualized_pct < Decimal::ZERO {
        return Err(ApiError::BadRequest("资金费率年化阈值不能为负数".into()));
    }
    if body.funding_cooldown_secs < 0 {
        return Err(ApiError::BadRequest("提醒冷却时间不能为负数".into()));
    }
    body.news_sources = normalize_sources(body.news_sources);
    state.store.save_notification_settings(&body).await;
    tracing::info!(
        news = body.news_enabled,
        funding = body.funding_enabled,
        telegram = body.telegram_enabled,
        desktop = body.desktop_enabled,
        "通知配置已更新"
    );
    Ok(Json(body))
}

/// 只保留执行器支持的新闻来源，去空、去重并统一小写。
fn normalize_sources(values: Vec<String>) -> Vec<String> {
    let mut sources: Vec<String> = Vec::new();
    for value in values {
        let value = value.trim().to_lowercase();
        if value.is_empty() || sources.contains(&value) {
            continue;
        }
        if news::SOURCES.iter().any(|item| *item == value) {
            sources.push(value);
        }
    }
    sources
}
