use std::{env, net::SocketAddr};

use anyhow::Context;
use arb_core::RiskLimits;
use serde::Serialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingMode {
    /// 模拟盘：不连接真实交易所，只做流程与风控验证。
    Paper,
    /// 真实交易：必须显式打开 ARBPILOT_ALLOW_LIVE_TRADING。
    Live,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub api_token: String,
    pub mode: TradingMode,
    pub allow_live_trading: bool,
    pub database_url: Option<String>,
    pub risk: RiskLimits,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let bind_addr = env::var("ARBPILOT_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
            .parse()
            .context("ARBPILOT_BIND_ADDR 不是合法地址")?;

        let mode = match env::var("ARBPILOT_MODE").unwrap_or_else(|_| "paper".to_string()).as_str()
        {
            "paper" => TradingMode::Paper,
            "live" => TradingMode::Live,
            other => anyhow::bail!("ARBPILOT_MODE 只能是 paper 或 live，当前为 {other}"),
        };

        let allow_live_trading = env::var("ARBPILOT_ALLOW_LIVE_TRADING")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let api_token = env::var("ARBPILOT_API_TOKEN").unwrap_or_else(|_| "dev-local-token".into());

        let mut risk = RiskLimits::default();
        if let Ok(value) = env::var("ARBPILOT_MAX_ORDER_NOTIONAL_USD") {
            risk.max_order_notional_usd = value.parse().context("解析单笔名义上限失败")?;
        }
        if let Ok(value) = env::var("ARBPILOT_MAX_TOTAL_NOTIONAL_USD") {
            risk.max_total_notional_usd = value.parse().context("解析总敞口上限失败")?;
        }
        if let Ok(value) = env::var("ARBPILOT_MAX_LEVERAGE") {
            risk.max_leverage = value.parse().context("解析杠杆上限失败")?;
        }

        Ok(Self {
            bind_addr,
            api_token,
            mode,
            allow_live_trading,
            database_url: env::var("DATABASE_URL").ok().filter(|url| !url.is_empty()),
            risk,
        })
    }
}
