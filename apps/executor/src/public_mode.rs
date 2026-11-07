use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};

use crate::{error::ApiError, state::AppState};

/// 官方托管实例允许匿名读取的公共路径（不含 `/api/v1` 前缀）。
fn is_public_read(path: &str) -> bool {
    path == "/events"
        || path == "/config/market"
        || path == "/license"
        || path == "/news"
        || path.starts_with("/news/")
        || path == "/chain/prices"
        || path == "/funding"
        || path.starts_with("/funding/")
        || path == "/opportunities"
        || path.starts_with("/opportunities/")
        || path == "/candles"
        || path == "/templates"
        || path.starts_with("/templates/")
        || path.starts_with("/market/")
}

/// 中间件：托管实例只提供只读公共数据。
///
/// 账户密钥、下单、策略、风控与 Telegram 等写操作全部拒绝，
/// 避免用户把交易所 API Key 交给官方服务器。
pub async fn require_public_reads(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.config.public_mode {
        return Ok(next.run(request).await);
    }

    let path = request.uri().path();
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let readable = matches!(*request.method(), Method::GET | Method::HEAD);
    if readable && is_public_read(path) {
        return Ok(next.run(request).await);
    }

    Err(ApiError::Forbidden(
        "官方托管实例仅提供只读公共数据，交易与账户操作请切换到自托管实例".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::is_public_read;

    #[test]
    fn allows_market_data_and_blocks_trading_paths() {
        for path in ["/funding/status", "/news", "/chain/prices", "/market/leaderboard", "/events", "/templates"] {
            assert!(is_public_read(path), "{path} 应允许只读访问");
        }
        for path in [
            "/accounts",
            "/accounts/history",
            "/orders",
            "/positions",
            "/strategies",
            "/control/pause",
            "/config/notifications",
            "/telegram/pairing",
        ] {
            assert!(!is_public_read(path), "{path} 不应在托管实例放行");
        }
    }
}
