use arb_core::Opportunity;
use axum::{extract::State, Json};

use crate::state::AppState;

pub async fn list(State(state): State<AppState>) -> Json<Vec<Opportunity>> {
    Json(state.market.opportunities())
}

/// 交割合约基差排行：现货多 + 交割空，按年化基差排序。
pub async fn basis(State(state): State<AppState>) -> Json<Vec<Opportunity>> {
    Json(state.market.basis_opportunities())
}
