use axum::{
    extract::{Query, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    error::ApiError,
    exchange::{FundingRate, VENUE},
    funding::FundingStatus,
    state::AppState,
    store::FundingSummary,
};

/// 各交易所的采集器状态：是否启用、最近一次采集结果与各交易对的最新费率。
pub async fn status(State(state): State<AppState>) -> Json<Vec<FundingStatus>> {
    Json(state.funding.status().await)
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    symbol: Option<String>,
    limit: Option<i64>,
    /// 交易所标识，默认 `binance`，可选 `okx`。
    venue: Option<String>,
}

/// 已入库的资金费率历史，按结算时间倒序。
pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<FundingRate>>, ApiError> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1_000);
    let venue = query.venue.as_deref().unwrap_or(VENUE);
    let rates = state
        .store
        .funding_rates(venue, query.symbol.as_deref(), limit)
        .await?;
    Ok(Json(rates))
}

#[derive(Debug, Deserialize)]
pub struct SummaryQuery {
    /// 统计窗口（秒），默认 1 天，最长 90 天。
    secs: Option<i64>,
}

/// 时间窗口内各交易对、各交易所的累计资金费率，供对比表一次性读取。
pub async fn summary(
    State(state): State<AppState>,
    Query(query): Query<SummaryQuery>,
) -> Result<Json<Vec<FundingSummary>>, ApiError> {
    let secs = query.secs.unwrap_or(86_400).clamp(60, 90 * 86_400);
    let since = Utc::now() - chrono::Duration::seconds(secs);
    Ok(Json(state.store.funding_summary(since).await?))
}
