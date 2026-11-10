//! 上币公告跟随：桌面端按策略实例读取与修改自动单腿交易设置。

use axum::{
    extract::{Query, State},
    Json,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ApiError, listing::ListingSettings, news::SOURCES, routes::strategies, state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct SettingsQuery {
    pub strategy_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ListingSettingsResponse {
    pub strategy_id: Uuid,
    pub settings: ListingSettings,
    /// 可监听的公告来源。
    pub available_watch_venues: Vec<String>,
    /// 当前具备下单凭证的交易所，供优先级排序。
    pub available_execution_venues: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub strategy_id: Uuid,
    #[serde(flatten)]
    pub settings: ListingSettings,
}

pub async fn settings(
    State(state): State<AppState>,
    Query(query): Query<SettingsQuery>,
) -> Json<ListingSettingsResponse> {
    Json(response(&state, query.strategy_id))
}

/// 保存某个策略实例的规则：保存后立即对后续上币公告生效，无需重启执行器。
pub async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSettingsRequest>,
) -> Result<Json<ListingSettingsResponse>, ApiError> {
    ensure_strategy(&state, body.strategy_id)?;
    let mut settings = body.settings;
    if settings.rules.is_empty() {
        return Err(ApiError::BadRequest("至少保留一条规则".to_string()));
    }

    let watch = watch_venues();
    let execution = available_execution_venues(&state);
    for rule in &mut settings.rules {
        // 关掉的规则只保留配置，允许先停用再慢慢改参数。
        if rule.enabled {
            if rule.notional_usd <= Decimal::ZERO {
                return Err(ApiError::BadRequest("下单金额必须大于 0".to_string()));
            }
            if rule.leverage <= Decimal::ZERO {
                return Err(ApiError::BadRequest("杠杆必须大于 0".to_string()));
            }
            if rule.take_profit_pct < Decimal::ZERO || rule.stop_loss_pct < Decimal::ZERO {
                return Err(ApiError::BadRequest("止盈止损百分比不能为负数".to_string()));
            }
        }
        if rule.id.is_nil() {
            rule.id = Uuid::new_v4();
        }
        rule.name = rule.name.trim().to_string();
        rule.watch_venues = normalize(std::mem::take(&mut rule.watch_venues), &watch);
        rule.execution_venues = normalize(std::mem::take(&mut rule.execution_venues), &execution);
    }

    let saved = state
        .listing
        .update_settings(body.strategy_id, settings)
        .await;
    // 运行中的实例展示当前生效的规则，停止/启动规则后立即可见。
    strategies::refresh_run_note(&state, body.strategy_id).await;
    tracing::info!(
        strategy_id = %body.strategy_id,
        rules = saved.rules.len(),
        "上币公告跟随规则已更新"
    );
    Ok(Json(response(&state, body.strategy_id)))
}

fn ensure_strategy(state: &AppState, strategy_id: Uuid) -> Result<(), ApiError> {
    let exists = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .iter()
        .any(|record| record.id == strategy_id);
    if exists {
        Ok(())
    } else {
        Err(ApiError::NotFound("策略不存在".to_string()))
    }
}

fn response(state: &AppState, strategy_id: Uuid) -> ListingSettingsResponse {
    ListingSettingsResponse {
        strategy_id,
        settings: state.listing.settings(strategy_id),
        available_watch_venues: watch_venues(),
        available_execution_venues: available_execution_venues(state),
    }
}

fn watch_venues() -> Vec<String> {
    SOURCES.iter().map(|source| source.to_string()).collect()
}

/// 当前有下单凭证的交易所，去空、去重并统一小写。
fn available_execution_venues(state: &AppState) -> Vec<String> {
    let mut venues: Vec<String> = state
        .accounts
        .list()
        .into_iter()
        .filter(|account| account.can_trade)
        .map(|account| account.venue.trim().to_lowercase())
        .filter(|venue| !venue.is_empty())
        .collect();
    venues.sort();
    venues.dedup();
    venues
}

/// 只保留可用项，按用户设定的顺序去重；清空表示不限。
fn normalize(values: Vec<String>, available: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values {
        let item = value.trim().to_lowercase();
        if item.is_empty() || result.contains(&item) || !available.contains(&item) {
            continue;
        }
        result.push(item);
    }
    result
}
