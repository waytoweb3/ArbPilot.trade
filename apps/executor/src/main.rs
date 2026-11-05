mod accounts;
mod auth;
mod config;
mod error;
mod events;
mod exchange;
mod funding;
mod gateway;
mod http_client;
mod license;
mod llm;
mod market;
mod news;
mod notifier;
mod realtime;
mod routes;
mod sandbox;
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
    if config.api_token == "dev-local-token" {
        tracing::warn!("正在使用默认访问令牌，正式部署前请设置 ARBPILOT_API_TOKEN");
    }

    let store = store::Store::connect_if_configured(config.database_url.as_deref()).await;
    let state = state::AppState::new(config.clone(), store).await;
    // 内置模板存数据库：补齐缺失模板并把源码编译成沙箱模块。
    templates::seed(&state).await;
    // 策略只在「运行中」且执行器未暂停时产生订单意图，并通过统一风控网关下单。
    strategy_runner::spawn(state.clone());
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
