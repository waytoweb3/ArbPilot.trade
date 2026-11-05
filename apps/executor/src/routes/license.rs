use axum::{extract::State, Json};
use serde::Deserialize;

use crate::{error::ApiError, license::LicenseStatus, state::AppState};

/// 当前授权状态；桌面端据此判断是否需要登录或升级套餐。
pub async fn status(State(state): State<AppState>) -> Json<LicenseStatus> {
    Json(state.license.status())
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    token: String,
}

/// 安装账号服务签发的授权令牌；校验通过才会落盘。
pub async fn install(
    State(state): State<AppState>,
    Json(body): Json<InstallRequest>,
) -> Result<Json<LicenseStatus>, ApiError> {
    let token = body.token.trim();
    if token.is_empty() {
        return Err(ApiError::BadRequest("授权令牌不能为空".to_string()));
    }
    state
        .license
        .install(token)
        .map_err(|error| ApiError::BadRequest(format!("授权令牌无效: {error}")))?;
    Ok(Json(state.license.status()))
}

/// 退出登录时清空本地授权。
pub async fn clear(State(state): State<AppState>) -> Json<LicenseStatus> {
    state.license.clear();
    Json(state.license.status())
}
