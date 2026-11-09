use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    /// 身份令牌（账号服务签发的授权令牌）无效或已过期。
    UnauthorizedWith(String),
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    /// 套餐未授权：需要登录账号或升级订阅。
    PaymentRequired(String),
    /// 官方托管实例只提供只读公共数据。
    Forbidden(String),
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "缺少或无效的执行器访问令牌".to_string(),
            ),
            Self::UnauthorizedWith(message) => {
                (StatusCode::UNAUTHORIZED, "unauthorized", message.clone())
            }
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "bad_request", message.clone()),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message.clone()),
            Self::Conflict(message) => (StatusCode::CONFLICT, "conflict", message.clone()),
            Self::PaymentRequired(message) => (
                StatusCode::PAYMENT_REQUIRED,
                "license_required",
                message.clone(),
            ),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, "forbidden", message.clone()),
            Self::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                message.clone(),
            ),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.parts().2)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(%message, "执行器内部错误");
        }
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error.to_string())
    }
}
