use axum::{
    extract::{Path, State},
    Json,
};

use crate::{error::ApiError, state::AppState, telegram::PairingCode, telegram::TelegramStatus};

/// 机器人状态与已绑定账号。
pub async fn status(State(state): State<AppState>) -> Json<TelegramStatus> {
    Json(state.telegram.status())
}

/// 生成一次性配对码，用户在 Telegram 里发送 `/bind <code>` 完成绑定。
pub async fn pairing(State(state): State<AppState>) -> Result<Json<PairingCode>, ApiError> {
    Ok(Json(state.telegram.create_pairing()?))
}

/// 撤销某个 Telegram 账号的绑定。
pub async fn revoke(
    State(state): State<AppState>,
    Path(user_id): Path<i64>,
) -> Result<Json<TelegramStatus>, ApiError> {
    state.telegram.revoke(user_id)?;
    Ok(Json(state.telegram.status()))
}
