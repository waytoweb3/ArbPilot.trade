use arb_core::Opportunity;
use axum::{extract::State, Json};

use crate::state::AppState;

pub async fn list(State(state): State<AppState>) -> Json<Vec<Opportunity>> {
    Json(state.market.opportunities())
}
