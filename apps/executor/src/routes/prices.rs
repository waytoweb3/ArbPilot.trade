use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct PriceStreamQuery {
    symbol: Option<String>,
    /// 交易所标识，默认 Binance 永续。
    venue: Option<String>,
}

/// 实时价格推送：每秒下发一次 WebSocket 盘口中间价。
///
/// 桌面端用它把最新价合并进最后一根 K 线；`ts` 为毫秒时间戳，
/// 前端据此判断 30 秒 K 线是否进入新周期。
pub async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<PriceStreamQuery>,
) -> impl IntoResponse {
    let venue = query
        .venue
        .unwrap_or_else(|| "binance".to_string())
        .trim()
        .to_ascii_lowercase();
    let symbol = query
        .symbol
        .unwrap_or_else(|| "BTCUSDT".to_string())
        .trim()
        .to_ascii_uppercase();
    ws.on_upgrade(move |socket| handle(socket, state, venue, symbol))
}

async fn handle(mut socket: WebSocket, state: AppState, venue: String, symbol: String) {
    // 注册采样：即使前端只看 1m/5m，30 秒 K 线也会随连接持续累积。
    state.market.watch_tick_bars(&venue, &symbol);

    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last: Option<Decimal> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Some((price, at)) = state.market.realtime_quote(&venue, &symbol) else {
                    continue;
                };
                if last == Some(price) {
                    continue;
                }
                last = Some(price);
                let payload = json!({
                    "type": "price_tick",
                    "venue": venue,
                    "symbol": symbol,
                    "price": price.to_string(),
                    "ts": at.timestamp_millis(),
                });
                if socket.send(Message::Text(payload.to_string().into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
}
