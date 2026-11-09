use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;

use crate::{error::ApiError, exchange::DepthSnapshot, state::AppState};

#[derive(Debug, Deserialize)]
pub struct DepthQuery {
    symbol: Option<String>,
    limit: Option<u32>,
    /// 交易所标识，默认 Binance 永续；以 `-spot` 结尾走现货。
    venue: Option<String>,
}

/// 多档盘口深度，供交易页展示买卖盘与挂单分布。
pub async fn get(
    State(state): State<AppState>,
    Query(query): Query<DepthQuery>,
) -> Result<Json<DepthSnapshot>, ApiError> {
    let symbol = query.symbol.as_deref().unwrap_or("BTCUSDT");
    let limit = query.limit.unwrap_or(20).clamp(1, 200);
    let venue = query.venue.as_deref().unwrap_or("binance");
    let depth = state.market.depth(venue, symbol, limit).await?;
    Ok(Json(depth))
}
