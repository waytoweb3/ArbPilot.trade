use arb_core::{AccountSnapshot, ExchangeAdapter, Position};
use axum::{extract::State, Json};

use crate::{error::ApiError, state::AppState};

pub async fn positions(State(state): State<AppState>) -> Json<Vec<Position>> {
    let positions = state
        .positions
        .read()
        .expect("仓位锁不可用")
        .clone();
    Json(positions)
}

pub async fn account(State(state): State<AppState>) -> Result<Json<AccountSnapshot>, ApiError> {
    let snapshot = state
        .exchange
        .account_snapshot()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(Json(snapshot))
}
