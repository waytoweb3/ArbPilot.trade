mod accounts;
mod auth;
mod conditional;
mod config;
mod error;
mod events;
mod exchange;
mod funding;
mod gateway;
mod http_client;
mod license;
mod listing;
mod llm;
mod market;
mod news;
mod notifier;
mod onchain;
mod public_mode;
mod realtime;
mod routes;
mod sandbox;
mod secret;
mod selftest;
mod skill;
mod skill_backtest;
mod skill_check;
mod sqlite_store;
mod state;
mod store;
mod strategy_runner;
mod telegram;
mod templates;
mod wasm_backtest;
mod ws;

use anyhow::Context;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,arbpilot_executor=debug".into()),
        )
        .init();

    let config = config::Config::from_env()?;
    // 临时自检入口：逐家交易所下小额单并平仓，不启动 HTTP 服务。
    if std::env::var("ARBPILOT_SELFTEST").is_ok() {
        return selftest::run(config).await;
    }
    if config.api_token == "dev-local-token" {
        tracing::warn!("正在使用默认访问令牌，正式部署前请设置 ARBPILOT_API_TOKEN");
    }

    let store = store::Store::connect_if_configured(
        config.database_url.as_deref(),
        config.local_db.as_deref(),
    )
    .await;
    let state = state::AppState::new(config.clone(), store).await;
    // 官方实例补齐全部模板并发布市场；本地只补齐通用技能模板。
    templates::seed(&state).await;
    // 官方只读实例把内置模板发布到策略市场，供所有用户免费订阅。
    routes::market::seed_official(&state).await;
    // 策略只在「运行中」且执行器未暂停时产生订单意图，并通过统一风控网关下单。
    strategy_runner::spawn(state.clone());
    // 条件单：按固定节拍检查有效期与触发条件，命中后走同一风控网关下单。
    conditional::spawn(state.clone());
    // 上币单腿交易：扫描上币公告推送电报卡片，并按标记价监控止盈止损。
    listing::spawn(state.clone());
    // 技能策略：按 SKILL.md 参数化后的触发条件扫描公告并下单。
    skill::spawn(state.clone());
    let app = routes::build(state);

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .context("绑定执行器端口失败")?;
    tracing::info!(
        addr = %config.bind_addr,
        "ArbPilot 执行器已启动"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("执行器异常退出")?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("收到退出信号，执行器停止接收新请求");
}
