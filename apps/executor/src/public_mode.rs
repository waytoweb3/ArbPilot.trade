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
        || path == "/prices/stream"
        || path == "/templates"
        || path.starts_with("/templates/")
        || path.starts_with("/market/")
}

/// 官网无需登录即可读取的市场目录（不含 `/api/v1` 前缀）。
///
/// 官方免费策略以此对所有人可见；订阅列表与结算信息仍要求登录。
fn is_market_public_read(path: &str) -> bool {
    path == "/market/publications"
        || path.starts_with("/market/publications/")
        || path == "/market/leaderboard"
        || path == "/market/authors"
}

/// 官网资金费用页无需令牌即可读取的公共路径（不含 `/api/v1` 前缀）。
///
/// 只放行公开市场数据，且仅限只读请求；账户、下单与策略写入仍然必须带令牌。
pub fn allows_anonymous(method: &Method, path: &str) -> bool {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path == "/funding/status" || is_market_public_read(path)
}

/// 官方实例上凭账号身份即可访问、无需共享 API 令牌的市场路径。
///
/// 公开目录允许匿名浏览；订阅与退订需要登录账号，但不该依赖客户端内置的静态令牌。
pub fn allows_without_api_token(method: &Method, path: &str) -> bool {
    if allows_anonymous(method, path) {
        return true;
    }
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    (*method == Method::POST
        && path.starts_with("/market/publications/")
        && path.ends_with("/subscribe"))
        || (*method == Method::DELETE && path.starts_with("/market/subscriptions/"))
        || (*method == Method::GET && path == "/market/subscriptions")
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
    // 免费策略的订阅与退订只写入订阅关系，不涉及密钥、下单或策略运行。
    let subscription_action = (request.method() == Method::POST
        && path.starts_with("/market/publications/")
        && path.ends_with("/subscribe"))
        || (request.method() == Method::DELETE && path.starts_with("/market/subscriptions/"));
    if subscription_action {
        return Ok(next.run(request).await);
    }

    Err(ApiError::Forbidden(
        "官方托管实例仅提供只读公共数据，交易与账户操作请切换到自托管实例".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{allows_anonymous, is_public_read};

    #[test]
    fn allows_anonymous_only_for_public_market_reads() {
        use axum::http::Method;
        assert!(allows_anonymous(
            &Method::GET,
            "/api/v1/funding/status"
        ));
        assert!(allows_anonymous(&Method::GET, "/funding/status"));
        assert!(allows_anonymous(
            &Method::GET,
            "/api/v1/market/publications"
        ));
        assert!(allows_anonymous(
            &Method::GET,
            "/api/v1/market/publications/abc/bundle"
        ));
        assert!(!allows_anonymous(
            &Method::POST,
            "/api/v1/funding/status"
        ));
        assert!(!allows_anonymous(
            &Method::GET,
            "/api/v1/market/subscriptions"
        ));
        assert!(!allows_anonymous(
            &Method::GET,
            "/api/v1/funding/history"
        ));
        assert!(!allows_anonymous(&Method::GET, "/api/v1/accounts"));
    }

    #[test]
    fn allows_market_data_and_blocks_trading_paths() {
        for path in ["/funding/status", "/news", "/chain/prices", "/market/leaderboard", "/events", "/prices/stream", "/templates"] {
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
