mod auth;
mod config;
mod error;
mod events;
mod exchange;
mod market;
mod routes;
mod state;
mod store;

use anyhow::Context;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::config::TradingMode;

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
    if config.mode == TradingMode::Live && !config.allow_live_trading {
        anyhow::bail!("ARBPILOT_MODE=live 时必须显式设置 ARBPILOT_ALLOW_LIVE_TRADING=true");
    }
    if config.mode == TradingMode::Live {
        tracing::warn!("真实交易模式已开启，但当前构建仅包含模拟交易所适配器");
    }
    if config.api_token == "dev-local-token" {
        tracing::warn!("正在使用默认访问令牌，正式部署前请设置 ARBPILOT_API_TOKEN");
    }

    let store = store::Store::connect_if_configured(config.database_url.as_deref()).await;
    let state = state::AppState::new(config.clone(), store);
    let app = routes::build(state);

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .context("绑定执行器端口失败")?;
    tracing::info!(
        addr = %config.bind_addr,
        mode = ?config.mode,
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
