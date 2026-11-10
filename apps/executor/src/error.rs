use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::skill_check::ValidationReport;

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
    /// 请求内容未通过六维校验：拒绝保存，并回传完整报告。
    SkillValidation(Box<ValidationReport>),
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
            Self::SkillValidation(report) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "skill_validation_failed",
                validation_message(report),
            ),
            Self::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                message.clone(),
            ),
        }
    }
}

/// 校验失败的展示文案：先给六维结论，再逐条列出阻断项。
fn validation_message(report: &ValidationReport) -> String {
    let mut message = format!(
        "技能参数未通过校验，已拒绝保存（{} 项错误）\n{}",
        report.errors, report.summary
    );
    for issue in report.blockers() {
        message.push_str(&format!(
            "\n· [{}] {}",
            issue.dimension.label(),
            issue.message
        ));
    }
    message
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
        let mut error = json!({ "code": code, "message": message });
        if let Self::SkillValidation(report) = &self {
            error["report"] = json!(report);
        }
        (status, Json(json!({ "error": error }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_check::{Dimension, Issue, Report};

    #[test]
    fn skill_validation_error_returns_six_dimension_report() {
        let mut report = Report::default();
        report.push(Issue::error(
            Dimension::Backtest,
            "backtest.zero_fees",
            "手续费假设为 0",
        ));
        let error = ApiError::SkillValidation(Box::new(report.view()));

        let (status, code, message) = error.parts();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code, "skill_validation_failed");
        assert!(message.contains("回测结果可信：不通过"));
        assert!(message.contains("· [回测结果可信] 手续费假设为 0"));

        let body = match &error {
            ApiError::SkillValidation(report) => json!(report),
            _ => json!(null),
        };
        assert_eq!(body["dimensions"].as_array().map(Vec::len), Some(6));
        assert_eq!(body["passed"], json!(false));
    }
}
