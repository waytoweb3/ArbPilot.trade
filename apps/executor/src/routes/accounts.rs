use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    accounts::AccountView,
    error::ApiError,
    state::AppState,
    store::EquityPoint,
};

#[derive(Debug, Deserialize)]
pub struct ImportAccountRequest {
    pub venue: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_secret: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    /// DEX 钱包地址或 Lighter 账户索引；CEX 账户留空。
    #[serde(default)]
    pub wallet_address: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountsResponse {
    pub accounts: Vec<AccountView>,
    pub total_equity_usd: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_hours")]
    pub hours: i64,
}

fn default_hours() -> i64 {
    168
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub hours: i64,
    pub points: Vec<EquityPoint>,
}

/// 账户列表与当前总权益；密钥与 Passphrase 不会出现在响应中。
pub async fn list(State(state): State<AppState>) -> Json<AccountsResponse> {
    let accounts = state.accounts.list();
    let observed_at = accounts
        .iter()
        .filter_map(|account| account.snapshot.as_ref())
        .map(|snapshot| snapshot.observed_at)
        .max()
        .unwrap_or_else(Utc::now);

    Json(AccountsResponse {
        accounts,
        total_equity_usd: state.accounts.total_equity_usd(),
        observed_at,
    })
}

/// 导入 API Key：先调用交易所私有接口验证，成功后才写入本机文件。
pub async fn import(
    State(state): State<AppState>,
    Json(body): Json<ImportAccountRequest>,
) -> Result<(StatusCode, Json<AccountView>), ApiError> {
    let view = state
        .accounts
        .import(
            body.venue,
            body.label,
            body.api_key.unwrap_or_default(),
            body.api_secret.unwrap_or_default(),
            body.passphrase,
            body.wallet_address,
        )
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .accounts
        .remove(id)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 手动刷新单个账户；失败时仍返回 200，错误写在 snapshot.error 里。
pub async fn refresh(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AccountView>, ApiError> {
    let view = state
        .accounts
        .refresh(id)
        .await
        .map_err(|error| ApiError::NotFound(error.to_string()))?;
    Ok(Json(view))
}

/// 权益历史，按时间范围自动选择聚合粒度。
pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let hours = query.hours.clamp(1, 24 * 365);
    let bucket_secs = if hours <= 24 {
        300
    } else if hours <= 24 * 7 {
        3_600
    } else {
        14_400
    };

    let points = state
        .accounts
        .history(hours, bucket_secs)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(Json(HistoryResponse { hours, points }))
}
