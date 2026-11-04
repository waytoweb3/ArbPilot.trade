mod control;
mod health;
mod opportunities;
mod orders;
mod portfolio;
mod strategies;
mod ws;

use axum::{
    middleware,
    routing::{get, patch, post},
    Router,
};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::state::AppState;

pub fn build(state: AppState) -> Router {
    let protected = Router::new()
        .route("/opportunities", get(opportunities::list))
        .route("/orders", get(orders::list).post(orders::create))
        .route("/positions", get(portfolio::positions))
        .route("/account", get(portfolio::account))
        .route("/strategies", get(strategies::list))
        .route("/strategies/{id}", patch(strategies::update))
        .route("/risk/limits", get(control::limits))
        .route("/control/pause", post(control::pause))
        .route("/control/resume", post(control::resume))
        .route("/events", get(ws::handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_token,
        ));

    Router::new()
        .route("/api/v1/health", get(health::health))
        .nest("/api/v1", protected)
        // 仅用于本地开发（Vite dev server）。正式部署应收紧允许来源。
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
