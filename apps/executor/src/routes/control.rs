use std::collections::BTreeMap;

use arb_core::{RiskLimits, VenueRiskLimits};
use axum::{extract::State, Json};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{error::ApiError, state::AppState};

#[derive(Debug, Deserialize)]
pub struct PauseRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub paused: bool,
    pub reason: Option<String>,
}

pub async fn pause(
    State(state): State<AppState>,
    Json(body): Json<PauseRequest>,
) -> Json<ControlResponse> {
    state.set_paused(true, body.reason.as_deref().map(|reason| (reason, reason)));
    Json(ControlResponse {
        paused: true,
        reason: body.reason,
    })
}

pub async fn resume(State(state): State<AppState>) -> Json<ControlResponse> {
    state.set_paused(false, None);
    Json(ControlResponse {
        paused: false,
        reason: None,
    })
}

pub async fn limits(State(state): State<AppState>) -> Json<RiskLimits> {
    Json(state.risk_limits())
}

/// 运行期修改风控限额：校验后立即生效，并写入本地文件供重启恢复。
pub async fn update_limits(
    State(state): State<AppState>,
    Json(mut body): Json<RiskLimits>,
) -> Result<Json<RiskLimits>, ApiError> {
    if body.max_order_notional_usd <= Decimal::ZERO {
        return Err(ApiError::BadRequest("单笔名义上限必须大于 0".into()));
    }
    if body.max_total_notional_usd < body.max_order_notional_usd {
        return Err(ApiError::BadRequest(
            "总敞口上限不能小于单笔名义上限".into(),
        ));
    }
    if body.max_leverage < Decimal::ONE {
        return Err(ApiError::BadRequest("杠杆上限不能低于 1".into()));
    }
    if body.max_slippage_bps < Decimal::ZERO {
        return Err(ApiError::BadRequest("滑点上限不能为负数".into()));
    }
    if !(1..=3_600).contains(&body.max_quote_age_secs) {
        return Err(ApiError::BadRequest("行情时效需在 1 到 3600 秒之间".into()));
    }
    if !(1..=4).contains(&body.max_legs) {
        return Err(ApiError::BadRequest("最大腿数需在 1 到 4 之间".into()));
    }

    body.allowed_symbols = normalize(body.allowed_symbols, true);
    body.allowed_venues = normalize(body.allowed_venues, false);
    body.venue_overrides = normalize_overrides(body.venue_overrides)?;

    if let Some(parent) = state.config.risk_file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ApiError::Internal(format!("创建风控限额目录失败: {error}")))?;
        }
    }
    let json = serde_json::to_string_pretty(&body)
        .map_err(|error| ApiError::Internal(format!("序列化风控限额失败: {error}")))?;
    std::fs::write(&state.config.risk_file, json)
        .map_err(|error| ApiError::Internal(format!("保存风控限额失败: {error}")))?;

    *state.risk_limits.write().expect("风控限额锁不可用") = body.clone();
    tracing::info!(path = %state.config.risk_file.display(), "风控限额已更新");
    Ok(Json(body))
}

/// 校验交易所覆盖并统一交易所标识与小写/去重。
fn normalize_overrides(
    overrides: BTreeMap<String, VenueRiskLimits>,
) -> Result<BTreeMap<String, VenueRiskLimits>, ApiError> {
    let mut normalized = BTreeMap::new();
    for (venue, mut limits) in overrides {
        let key = venue.trim().to_lowercase();
        if key.is_empty() {
            return Err(ApiError::BadRequest("交易所标识不能为空".into()));
        }
        if let Some(value) = limits.max_order_notional_usd {
            if value <= Decimal::ZERO {
                return Err(ApiError::BadRequest(format!(
                    "{key} 的单笔名义上限必须大于 0"
                )));
            }
        }
        if let Some(value) = limits.max_leverage {
            if value < Decimal::ONE {
                return Err(ApiError::BadRequest(format!("{key} 的杠杆上限不能低于 1")));
            }
        }
        if let Some(value) = limits.max_slippage_bps {
            if value < Decimal::ZERO {
                return Err(ApiError::BadRequest(format!("{key} 的滑点上限不能为负数")));
            }
        }
        if let Some(value) = limits.max_legs {
            if !(1..=4).contains(&value) {
                return Err(ApiError::BadRequest(format!(
                    "{key} 的最大腿数需在 1 到 4 之间"
                )));
            }
        }
        if let Some(symbols) = limits.allowed_symbols.take() {
            limits.allowed_symbols = Some(normalize(symbols, true));
        }
        normalized.insert(key, limits);
    }
    Ok(normalized)
}

/// 去空、去重；交易对统一大写，交易所统一小写。
fn normalize(values: Vec<String>, uppercase: bool) -> Vec<String> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = if uppercase {
            trimmed.to_uppercase()
        } else {
            trimmed.to_lowercase()
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}
