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
mod orderbook;
mod public_mode;
mod realtime;
mod routes;
mod sandbox;
mod secret;
mod selftest;
mod skill;
mod skill_backtest;
mod skill_check;
mod skill_library;
mod skill_turtle;
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
        // 默认令牌只能在回环地址上使用；暴露到公网前必须换成随机令牌。
        if !config.bind_addr.ip().is_loopback() && !config.allow_default_token {
            anyhow::bail!(
                "检测到默认开发令牌且监听 {}（非回环地址）。请设置 ARBPILOT_API_TOKEN=$(openssl rand -hex 32) 后重启；仅本机调试可设 ARBPILOT_ALLOW_DEFAULT_TOKEN=true",
                config.bind_addr
            );
        }
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

    tracing::info!(
        addr = %config.bind_addr,
        "ArbPilot 执行器已启动"
    );

    match (&config.tls_cert_file, &config.tls_key_file) {
        (Some(cert), Some(key)) => {
            // 自托管用户把管理端口暴露到公网时，至少要有 TLS，避免令牌明文传输。
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .with_context(|| {
                    format!("加载 TLS 证书失败: {} / {}", cert.display(), key.display())
                })?;
            tracing::info!("HTTPS 已启用（证书 {}）", cert.display());
            let handle = axum_server::Handle::new();
            let shutdown = handle.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                shutdown.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
            });
            axum_server::bind_rustls(config.bind_addr, tls)
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .context("执行器异常退出")?;
        }
        _ => {
            let listener = TcpListener::bind(config.bind_addr)
                .await
                .context("绑定执行器端口失败")?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("执行器异常退出")?;
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("收到退出信号，执行器停止接收新请求");
}
