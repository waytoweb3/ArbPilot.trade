//! 策略市场配置：桌面端只从这里读取可选的交易所与交易对。

use std::collections::BTreeMap;

use axum::{extract::State, Json};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{
    config::FeeConfig,
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

/// 当前生效的手续费档位：taker / maker / 滑点，单位 bps。
pub async fn fees(State(state): State<AppState>) -> Json<FeeConfig> {
    Json(state.fees())
}

/// 修改费率档位：保存到本地文件并立即重算机会，VIP 用户可换成自己的实际费率。
pub async fn update_fees(
    State(state): State<AppState>,
    Json(body): Json<FeeConfig>,
) -> Result<Json<FeeConfig>, ApiError> {
    let fees = state
        .set_fees(body)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    tracing::info!(
        taker_bps = %fees.taker_bps,
        maker_bps = %fees.maker_bps,
        slippage_bps = %fees.slippage_bps,
        "费率档位已更新"
    );
    Ok(Json(fees))
}

#[derive(Debug, Serialize)]
pub struct TelegramConfig {
    /// 是否已经配置 Token（不会回传 Token 本身）。
    pub configured: bool,
    pub enabled: bool,
    pub api_base: String,
    /// 内置执行器下保存后需要重启 sidecar 才能生效。
    pub restart_required: bool,
}

pub async fn telegram(State(state): State<AppState>) -> Json<TelegramConfig> {
    Json(TelegramConfig {
        configured: state.config.telegram_bot_token.is_some(),
        enabled: state.config.telegram_enabled,
        api_base: state.config.telegram_api_base.clone(),
        restart_required: false,
    })
}

#[derive(Debug, Deserialize)]
pub struct UpdateTelegramRequest {
    pub token: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 保存 Telegram Bot Token：写入本地文件（权限 600），重启执行器后生效。
///
/// 内置执行器没有 .env，桌面端必须通过这个入口把 Token 交给 sidecar。
pub async fn update_telegram(
    State(state): State<AppState>,
    Json(body): Json<UpdateTelegramRequest>,
) -> Result<Json<TelegramConfig>, ApiError> {
    let token = body
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let path = &state.config.telegram_file;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ApiError::BadRequest(format!("创建配置目录失败: {error}")))?;
    }
    let mut content = serde_json::json!({
        "token": token,
        "enabled": body.enabled.unwrap_or(state.config.telegram_enabled),
    });
    if let Some(existing) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    {
        if token.is_none() {
            content["token"] = existing["token"].clone();
        }
    }
    std::fs::write(path, content.to_string())
        .map_err(|error| ApiError::BadRequest(format!("保存 Telegram 配置失败: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(path = %path.display(), "Telegram 配置已保存，重启执行器后生效");
    Ok(Json(TelegramConfig {
        configured: content["token"].as_str().is_some(),
        enabled: content["enabled"].as_bool().unwrap_or(false),
        api_base: state.config.telegram_api_base.clone(),
        restart_required: true,
    }))
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
