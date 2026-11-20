mod accounts;
mod candles;
mod chain;
mod conditional;
mod config;
pub(crate) mod control;
mod depth;
mod funding;
mod health;
mod license;
mod listing;
pub(crate) mod market;
mod news;
mod opportunities;
mod orders;
mod portfolio;
mod prices;
mod sandbox;
mod skill;
pub(crate) mod strategies;
mod strategy_code;
mod telegram;
mod templates;
mod ws;

use axum::{
    http::{header, HeaderName, HeaderValue, Method},
    middleware,
    routing::{delete, get, patch, post},
    Router,
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};

use crate::state::AppState;

pub fn build(state: AppState) -> Router {
    // CORS 只放行显式配置的前端来源（桌面端开发服务器与 Tauri WebView），
    // 不再使用通配符，避免任意网页借浏览器发起带令牌的请求。
    let cors_origins: Vec<HeaderValue> = state
        .config
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse::<HeaderValue>().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(cors_origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("idempotency-key"),
        ]);

    let protected = Router::new()
        .route("/opportunities", get(opportunities::list))
        .route("/opportunities/basis", get(opportunities::basis))
        .route("/candles", get(candles::list))
        .route("/depth", get(depth::get))
        .route("/min-order", get(depth::min_order))
        .route("/orders", get(orders::list).post(orders::create))
        .route("/orders/reconcile", post(orders::reconcile))
        .route("/orders/{id}", patch(orders::replace))
        .route("/orders/{id}/cancel", post(orders::cancel))
        .route(
            "/conditional-orders",
            get(conditional::list).post(conditional::create),
        )
        .route("/conditional-orders/{id}/cancel", post(conditional::cancel))
        .route("/positions", get(portfolio::positions))
        .route("/positions/{id}/close", post(portfolio::close))
        .route(
            "/positions/groups/{group_id}/close",
            post(portfolio::close_group),
        )
        .route(
            "/accounts",
            get(accounts::list)
                .post(accounts::import)
                .delete(accounts::clear),
        )
        .route("/accounts/history", get(accounts::history))
        .route("/accounts/leverage", get(accounts::leverage))
        .route("/accounts/{id}", delete(accounts::remove))
        .route("/accounts/{id}/refresh", post(accounts::refresh))
        .route(
            "/strategies",
            get(strategies::list).post(strategies::create),
        )
        .route(
            "/strategies/{id}",
            get(strategies::detail)
                .patch(strategies::update)
                .delete(strategies::remove),
        )
        .route(
            "/strategies/{id}/definition",
            patch(strategies::update_definition),
        )
        .route("/strategies/{id}/scope", patch(strategies::update_scope))
        .route(
            "/strategies/{id}/execution-mode",
            patch(strategies::update_execution_mode),
        )
        .route("/strategies/{id}/validate", post(strategies::validate))
        .route("/strategies/{id}/backtest", post(strategies::backtest))
        .route("/strategies/{id}/versions", get(strategies::versions))
        .route(
            "/strategies/{id}/runs",
            get(strategies::runs).delete(strategies::clear_runs),
        )
        .route("/strategies/{id}/rollback", post(strategies::rollback))
        .route(
            "/strategies/{id}/sandbox",
            post(sandbox::bind).delete(sandbox::unbind),
        )
        .route(
            "/strategies/{id}/code",
            get(strategy_code::get_code).put(strategy_code::save_code),
        )
        .route(
            "/strategies/{id}/code/generate",
            post(strategy_code::generate_code),
        )
        .route(
            "/strategies/{id}/code/compile",
            post(strategy_code::compile_code),
        )
        .route(
            "/strategies/{id}/code/deploy",
            post(strategy_code::deploy_code),
        )
        .route("/templates", get(templates::list))
        .route("/templates/{key}/instantiate", post(templates::instantiate))
        .route("/skills", get(skill::list_skills).post(skill::create_skill))
        .route("/skills/{id}", get(skill::skill_detail))
        .route(
            "/skills/{id}/versions",
            get(skill::skill_versions).post(skill::create_skill_version),
        )
        .route("/skills/{id}/validate", post(skill::validate_skill))
        .route("/skills/{id}/diff", get(skill::diff_skill_versions))
        .route(
            "/strategies/{id}/skill/rules",
            get(skill::strategy_rules).post(skill::create_strategy_rule),
        )
        .route(
            "/strategies/{id}/skill/rules/{rule_id}",
            patch(skill::update_strategy_rule),
        )
        .route(
            "/strategies/{id}/skill/diff",
            get(skill::diff_strategy_skill),
        )
        .route(
            "/strategies/{id}/skill/upgrade",
            post(skill::upgrade_strategy_skill),
        )
        .route(
            "/market/authors",
            get(market::list_authors).post(market::create_author),
        )
        .route("/market/authors/{id}", patch(market::update_author))
        .route(
            "/market/publications",
            get(market::list_publications).post(market::create_publication),
        )
        .route("/market/publications/import", post(market::import_bundle))
        .route(
            "/market/publications/{id}",
            get(market::publication_detail).patch(market::update_publication),
        )
        .route(
            "/market/publications/{id}/performance",
            get(market::list_performance).post(market::create_performance),
        )
        .route(
            "/market/publications/{id}/subscribe",
            post(market::subscribe),
        )
        .route(
            "/market/publications/{id}/bundle",
            get(market::publication_bundle),
        )
        .route(
            "/market/publications/{id}/import",
            post(market::import_publication),
        )
        .route(
            "/market/publications/{id}/instantiate",
            post(market::instantiate_publication),
        )
        .route("/market/subscriptions", get(market::list_subscriptions))
        .route(
            "/market/subscriptions/{id}",
            patch(market::update_subscription).delete(market::cancel_subscription),
        )
        .route("/market/leaderboard", get(market::leaderboard))
        .route(
            "/market/settlements",
            get(market::list_settlements).post(market::create_settlement),
        )
        .route("/market/settlements/{id}", patch(market::update_settlement))
        .route(
            "/sandbox/artifacts",
            get(sandbox::list_artifacts).post(sandbox::upload_artifact),
        )
        .route("/sandbox/artifacts/{id}/dry-run", post(sandbox::dry_run))
        .route("/funding/history", get(funding::history))
        .route("/funding/summary", get(funding::summary))
        .route("/funding/status", get(funding::status))
        .route("/config/market", get(config::market))
        .route(
            "/config/fees",
            get(config::fees).put(config::update_fees),
        )
        .route(
            "/config/telegram",
            get(config::telegram).put(config::update_telegram),
        )
        .route(
            "/config/notifications",
            get(config::notifications).put(config::update_notifications),
        )
        .route(
            "/listing/settings",
            get(listing::settings).put(listing::update_settings),
        )
        .route(
            "/skill/settings",
            get(skill::settings).put(skill::update_settings),
        )
        .route("/skill/test", post(skill::test_event))
        .route("/news", get(news::list))
        .route("/chain/prices", get(chain::prices))
        .route(
            "/license",
            get(license::status)
                .post(license::install)
                .delete(license::clear),
        )
        .route("/telegram", get(telegram::status))
        .route("/telegram/pairing", post(telegram::pairing))
        .route("/telegram/bindings/{user_id}", delete(telegram::revoke))
        .route(
            "/risk/limits",
            get(control::limits).put(control::update_limits),
        )
        .route("/control/pause", post(control::pause))
        .route("/control/resume", post(control::resume))
        .route("/control/panic", post(control::panic))
        .route("/events", get(ws::handler))
        .route("/prices/stream", get(prices::stream))
        // 先鉴权再校验套餐：内层先执行，因此授权中间件放在鉴权层之前声明。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::license::require_plan,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::license::require_login,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::license::attach_identity,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::public_mode::require_public_reads,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_token,
        ));

    Router::new()
        .route("/api/v1/health", get(health::health))
        .nest("/api/v1", protected)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
