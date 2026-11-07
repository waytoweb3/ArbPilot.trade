use axum::{extract::State, Json};

use crate::{onchain::ChainResponse, state::AppState};

/// 链上价格：DEX 池子价格与预言机价格的最近一次快照。
///
/// 只读取内存中的采集结果，不触发链上请求；采集频率由 `ARBPILOT_CHAIN_POLL_SECS` 控制。
pub async fn prices(State(state): State<AppState>) -> Json<ChainResponse> {
    Json(state.chain.snapshot())
}
