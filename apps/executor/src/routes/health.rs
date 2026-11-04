use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{config::TradingMode, state::AppState};

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: String,
    pub mode: TradingMode,
    pub paused: bool,
    pub database_connected: bool,
    pub uptime_secs: u64,
    pub server_time: DateTime<Utc>,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: if state.is_paused() { "degraded" } else { "ok" },
        version: crate::config::VERSION.to_string(),
        mode: state.config.mode,
        paused: state.is_paused(),
        database_connected: state.store.is_connected(),
        uptime_secs: state.uptime_secs(),
        server_time: Utc::now(),
    })
}
