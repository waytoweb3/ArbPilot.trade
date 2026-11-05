use arb_core::Position;
use axum::{extract::State, Json};

use crate::state::AppState;

pub async fn positions(State(state): State<AppState>) -> Json<Vec<Position>> {
    let positions = state.positions.read().expect("仓位锁不可用").clone();
    Json(positions)
}
