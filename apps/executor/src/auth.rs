use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};

use crate::{error::ApiError, state::AppState};

/// 执行器 API 鉴权。
///
/// 桌面端使用 Authorization: Bearer <token>；WebSocket 无法自定义头，
/// 因此额外允许 ?token= 查询参数。
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    // 官网资金费用页与市场目录直接读取官方只读实例，订阅凭账号令牌校验。
    if state.config.public_mode
        && crate::public_mode::allows_without_api_token(request.method(), request.uri().path())
    {
        return Ok(next.run(request).await);
    }

    let provided = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .map(str::to_owned)
        .or_else(|| query_token(request.uri().query()));

    match provided {
        Some(token)
            if !token.is_empty()
                && constant_time_eq(token.as_bytes(), state.config.api_token.as_bytes()) =>
        {
            Ok(next.run(request).await)
        }
        _ => Err(ApiError::Unauthorized),
    }
}

fn query_token(query: Option<&str>) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "token" && !value.is_empty()).then(|| value.to_string())
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
