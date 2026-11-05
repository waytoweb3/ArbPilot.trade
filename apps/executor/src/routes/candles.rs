use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;

use crate::{error::ApiError, exchange::Candle, state::AppState};

#[derive(Debug, Deserialize)]
pub struct CandleQuery {
    symbol: Option<String>,
    interval: Option<String>,
    limit: Option<u32>,
    /// 交易所标识，默认 Binance 现货；OKX 可用 `okx-spot`。
    venue: Option<String>,
}

/// 现货 K 线，供桌面端图表展示。
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<CandleQuery>,
) -> Result<Json<Vec<Candle>>, ApiError> {
    let symbol = query.symbol.as_deref().unwrap_or("BTCUSDT");
    let interval = query.interval.as_deref().unwrap_or("5m");
    let limit = query.limit.unwrap_or(180).clamp(1, 1_000);
    let venue = query.venue.as_deref().unwrap_or("binance-spot");
    let candles = state.market.candles(venue, symbol, interval, limit).await?;
    Ok(Json(candles))
}
