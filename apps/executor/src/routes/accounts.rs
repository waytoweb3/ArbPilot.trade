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
    accounts::{AccountView, ImportRequest},
    error::ApiError,
    secret::StorageInfo,
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
    /// DEX 签名私钥：Hyperliquid 的 API Wallet 私钥，或 Lighter 的 API Key 私钥。
    /// 留空时该账户只能查询余额，不能下单。
    #[serde(default)]
    pub signer_key: Option<String>,
    /// Lighter 的 API Key 序号（0-254），默认 0。
    #[serde(default)]
    pub key_index: Option<u8>,
    /// Lighter 的数字账户索引，留空时由钱包地址自动解析。
    #[serde(default)]
    pub account_index: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AccountsResponse {
    pub accounts: Vec<AccountView>,
    pub total_equity_usd: Decimal,
    pub observed_at: DateTime<Utc>,
    /// 凭证在本机的存储方式：系统钥匙串或本地密钥文件加密。
    pub storage: StorageInfo,
}

#[derive(Debug, Serialize)]
pub struct ClearResponse {
    /// 被清除的账户数量。
    pub removed: usize,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_hours")]
    pub hours: i64,
}

#[derive(Debug, Deserialize)]
pub struct LeverageQuery {
    pub venue: String,
    pub symbol: String,
}

#[derive(Debug, Serialize)]
pub struct LeverageResponse {
    pub venue: String,
    pub symbol: String,
    /// 交易所在该合约上当前设置的杠杆；无持仓或交易所不提供查询时为 null。
    pub leverage: Option<Decimal>,
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
        storage: state.accounts.storage(),
    })
}

/// 导入 API Key：先调用交易所私有接口验证，成功后才写入本机文件。
pub async fn import(
    State(state): State<AppState>,
    Json(body): Json<ImportAccountRequest>,
) -> Result<(StatusCode, Json<AccountView>), ApiError> {
    let view = state
        .accounts
        .import(ImportRequest {
            venue: body.venue,
            label: body.label,
            api_key: body.api_key.unwrap_or_default(),
            api_secret: body.api_secret.unwrap_or_default(),
            passphrase: body.passphrase,
            wallet_address: body.wallet_address,
            signer_key: body.signer_key,
            key_index: body.key_index,
            account_index: body.account_index,
        })
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// 清除本机保存的全部交易所凭证，不可恢复。
pub async fn clear(State(state): State<AppState>) -> Result<Json<ClearResponse>, ApiError> {
    let removed = state
        .accounts
        .clear()
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(Json(ClearResponse { removed }))
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

/// 查询交易所在该合约上当前设置的杠杆，只读，不会修改账户配置。
pub async fn leverage(
    State(state): State<AppState>,
    Query(query): Query<LeverageQuery>,
) -> Result<Json<LeverageResponse>, ApiError> {
    let venue = query.venue.trim().to_ascii_lowercase();
    let symbol = query.symbol.trim().to_ascii_uppercase();
    if venue.is_empty() || symbol.is_empty() {
        return Err(ApiError::BadRequest("缺少 venue 或 symbol".to_string()));
    }
    let leverage = state
        .exchange
        .current_leverage(&venue, &symbol)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(Json(LeverageResponse {
        venue,
        symbol,
        leverage,
    }))
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
