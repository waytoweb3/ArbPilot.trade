use arb_core::RiskLimits;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct PauseRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControlResponse {
    pub paused: bool,
    pub reason: Option<String>,
}

pub async fn pause(
    State(state): State<AppState>,
    Json(body): Json<PauseRequest>,
) -> Json<ControlResponse> {
    state.set_paused(true, body.reason.as_deref());
    Json(ControlResponse {
        paused: true,
        reason: body.reason,
    })
}

pub async fn resume(State(state): State<AppState>) -> Json<ControlResponse> {
    state.set_paused(false, None);
    Json(ControlResponse {
        paused: false,
        reason: None,
    })
}

pub async fn limits(State(state): State<AppState>) -> Json<RiskLimits> {
    Json(state.config.risk.clone())
}
