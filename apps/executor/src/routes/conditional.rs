use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    conditional::{self, ConditionalOrder, CreateConditionalRequest},
    error::ApiError,
    state::AppState,
};

/// 全部条件单，按创建时间倒序。
pub async fn list(State(state): State<AppState>) -> Json<Vec<ConditionalOrder>> {
    Json(state.conditional.list())
}

/// 新建条件单：达到阈值后由后台自动提交双腿订单。
pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateConditionalRequest>,
) -> Result<(StatusCode, Json<ConditionalOrder>), ApiError> {
    let order = conditional::create(&state, request).await?;
    Ok((StatusCode::CREATED, Json(order)))
}

/// 撤销仍在等待中的条件单。
pub async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConditionalOrder>, ApiError> {
    let order = conditional::cancel(&state, id).await?;
    Ok(Json(order))
}
