use arb_core::{StrategyRecord, StrategyStatus};
use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

pub async fn list(State(state): State<AppState>) -> Json<Vec<StrategyRecord>> {
    let strategies = state
        .strategies
        .read()
        .expect("策略锁不可用")
        .clone();
    Json(strategies)
}

#[derive(Debug, Deserialize)]
pub struct UpdateStrategyRequest {
    pub status: StrategyStatus,
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStrategyRequest>,
) -> Result<Json<StrategyRecord>, ApiError> {
    let mut strategies = state.strategies.write().expect("策略锁不可用");
    let record = strategies
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("策略 {id} 不存在")))?;

    record.status = body.status;
    record.updated_at = Utc::now();
    record.last_heartbeat_at = Some(Utc::now());
    Ok(Json(record.clone()))
}
