use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use arb_core::{RiskLimits, SandboxLimits};
use serde::{Deserialize, Serialize};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 单个链上价格源：一个 DEX 池子，可选配一个预言机做偏离比较。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainFeedConfig {
    /// 唯一标识，用于告警冷却与桌面端展示。
    pub id: String,
    /// 展示用交易对，如 `ETH/USDT`。
    pub symbol: String,
    /// Uniswap V2 风格的池子地址。
    pub pool: String,
    /// 池子里 token0 是否为计价基准币（如 ETH）；否则基准币在 token1。
    #[serde(default = "default_true")]
    pub base_is_token0: bool,
    pub base_decimals: u32,
    pub quote_decimals: u32,
    /// Chainlink 风格聚合器地址；留空表示只记录池子价格，不算偏离。
    #[serde(default)]
    pub oracle: Option<String>,
    #[serde(default = "default_oracle_decimals")]
    pub oracle_decimals: u32,
    #[serde(default)]
    pub oracle_label: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_oracle_decimals() -> u32 {
    8
}

/// 未配置 `ARBPILOT_CHAIN_FEEDS` 时的默认监控对象：
/// Uniswap V2 的 ETH/USDT、ETH/USDC 池子，对 Chainlink ETH/USD。
fn default_chain_feeds() -> Vec<ChainFeedConfig> {
    vec![
        ChainFeedConfig {
            id: "eth-usdt-uniswap-v2".to_string(),
            symbol: "ETH/USDT".to_string(),
            pool: "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852".to_string(),
            base_is_token0: true,
            base_decimals: 18,
            quote_decimals: 6,
            oracle: Some("0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".to_string()),
            oracle_decimals: 8,
            oracle_label: Some("chainlink-eth-usd".to_string()),
        },
        ChainFeedConfig {
            id: "eth-usdc-uniswap-v2".to_string(),
            symbol: "ETH/USDC".to_string(),
            pool: "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".to_string(),
            base_is_token0: false,
            base_decimals: 18,
            quote_decimals: 6,
            oracle: Some("0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".to_string()),
            oracle_decimals: 8,
            oracle_label: Some("chainlink-eth-usd".to_string()),
        },
    ]
}

/// 池子与预言机地址必须是标准的 20 字节十六进制，避免配置错了却只看到空价格。
fn validate_address(value: &str, field: &str) -> anyhow::Result<()> {
    let raw = value.strip_prefix("0x").unwrap_or(value);
    if raw.len() != 40 || !raw.chars().all(|item| item.is_ascii_hexdigit()) {
        anyhow::bail!("{field} 不是合法的 0x 地址: {value}");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub api_token: String,
    pub database_url: Option<String>,
    /// 桌面端随包启动时使用的本地 SQLite 文件；未配置 `DATABASE_URL` 时优先使用它。
    pub local_db: Option<PathBuf>,
    pub risk: RiskLimits,
    pub funding_enabled: bool,
    /// Binance USDT 本位合约公开行情地址，可指向代理或测试桩。
    pub binance_fapi_url: String,
    /// 访问交易所公开行情使用的 HTTP 代理，如 `http://127.0.0.1:7897`。
    pub market_proxy: Option<String>,
    /// Binance 现货公开行情地址，用于生成现货/永续资金费率机会。
    pub binance_spot_url: String,
    /// 是否启用 OKX 资金费率与行情采集。
    pub okx_enabled: bool,
    /// OKX 公开行情地址，可指向代理或测试桩。
    pub okx_base_url: String,
    /// 是否启用 Bybit 资金费率采集。
    pub bybit_enabled: bool,
    /// Bybit 公开行情地址，可指向代理或测试桩。
    pub bybit_base_url: String,
    /// 是否启用 Bitget 资金费率采集。
    pub bitget_enabled: bool,
    /// Bitget 公开行情地址，可指向代理或测试桩。
    pub bitget_base_url: String,
    /// 是否启用 Gate.io 资金费率采集。
    pub gate_enabled: bool,
    /// Gate.io 公开行情地址，可指向代理或测试桩。
    pub gate_base_url: String,
    /// 是否启用 Aster 资金费率采集（与 Binance 合约接口兼容）。
    pub aster_enabled: bool,
    pub aster_base_url: String,
    /// 是否启用 Hyperliquid 资金费率采集。
    pub hyperliquid_enabled: bool,
    pub hyperliquid_base_url: String,
    /// 是否启用 Lighter 资金费率采集。
    pub lighter_enabled: bool,
    pub lighter_base_url: String,
    /// 是否启用 HTX（火币）资金费率采集。
    pub htx_enabled: bool,
    pub htx_base_url: String,
    /// 是否启用 MEXC 资金费率采集。
    pub mexc_enabled: bool,
    pub mexc_base_url: String,
    /// 是否启用 BingX 资金费率采集。
    pub bingx_enabled: bool,
    pub bingx_base_url: String,
    /// Binance 现货 WebSocket 地址，用于 `bookTicker` 推送。
    pub binance_spot_ws_url: String,
    /// Binance USDT 本位永续 WebSocket 地址。
    pub binance_futures_ws_url: String,
    /// OKX 公共 WebSocket 地址（盘口与当期资金费率推送）。
    pub okx_ws_url: String,
    /// Bybit 永续 / 现货 WebSocket 地址。
    pub bybit_ws_url: String,
    pub bybit_spot_ws_url: String,
    /// Bitget 公共 WebSocket 地址。
    pub bitget_ws_url: String,
    /// Gate USDT 本位永续 WebSocket 地址。
    pub gate_ws_url: String,
    /// Aster 合约 WebSocket 地址（Binance 兼容组合流）。
    pub aster_ws_url: String,
    /// HTX USDT 本位永续 WebSocket 地址。
    pub htx_ws_url: String,
    /// MEXC 合约 WebSocket 地址。
    pub mexc_ws_url: String,
    /// BingX 合约 WebSocket 地址。
    pub bingx_ws_url: String,
    /// Hyperliquid WebSocket 地址。
    pub hyperliquid_ws_url: String,
    /// Lighter WebSocket 地址。
    pub lighter_ws_url: String,
    /// 盘口轮询间隔（秒）。
    pub quote_poll_secs: u64,
    /// 手动指定的资金费率交易对；为空时自动发现交易所全部 USDT 本位永续。
    pub funding_symbols: Vec<String>,
    /// 盘口与套利机会使用的交易对（数量不宜过多，REST 兜底需逐币请求）。
    pub market_symbols: Vec<String>,
    /// 资金费率轮询间隔（秒）。
    pub funding_poll_secs: u64,
    /// 是否启用上币新闻采集（Binance / Upbit / OKX / Bybit / Bitget）。
    pub news_enabled: bool,
    /// 上币新闻轮询间隔（秒）。
    pub news_poll_secs: u64,
    /// Binance 新币公告列表接口。
    pub news_binance_url: String,
    /// Upbit 公告接口（trade 分类）。
    pub news_upbit_url: String,
    /// OKX 新币公告接口。
    pub news_okx_url: String,
    /// Bybit 新币公告接口。
    pub news_bybit_url: String,
    /// Bitget 上币公告接口。
    pub news_bitget_url: String,
    /// 是否启用链上价格监听。
    pub chain_enabled: bool,
    /// 链上 JSON-RPC 地址；未配置时监听器保持空闲，不发起请求。
    pub chain_rpc_url: Option<String>,
    /// 链上价格轮询间隔（秒）。
    pub chain_poll_secs: u64,
    /// 池子价格与预言机价格偏离超过该值时告警（bps）。
    pub chain_alert_bps: u32,
    /// 监控的池子与预言机组合。
    pub chain_feeds: Vec<ChainFeedConfig>,
    /// 是否启用 Telegram 机器人；未配置 Token 时即使开启也不会连接。
    pub telegram_enabled: bool,
    /// Telegram Bot Token，只保存在执行器所在机器，不通过 API 回传。
    pub telegram_bot_token: Option<String>,
    /// Telegram Bot API 根地址，默认官方地址，可指向自建反代。
    pub telegram_api_base: String,
    /// Telegram getUpdates 轮询间隔（秒）。
    pub telegram_poll_secs: u64,
    /// 一次性配对码有效期（秒）。
    pub telegram_pairing_secs: u64,
    /// Telegram 绑定关系保存路径，文件权限固定为 600。
    pub telegram_bindings_file: PathBuf,
    /// 交易所 API Key 的本地保存路径，文件权限固定为 600。
    pub accounts_file: PathBuf,
    /// 桌面端编辑后的风控限额保存路径，重启后从这里恢复。
    pub risk_file: PathBuf,
    /// 账户余额定时采集间隔（秒）。
    pub accounts_poll_secs: u64,
    /// 策略运行时评估间隔（秒）。
    pub strategy_poll_secs: u64,
    /// WASM 策略沙箱的默认资源限额。
    pub sandbox: SandboxLimits,
    /// 策略市场平台服务费（bps），用于作者结算。
    pub market_platform_fee_bps: u32,
    /// AI 生成策略代码使用的 OpenAI 兼容接口地址。
    pub llm_base_url: String,
    /// AI 接口密钥；只保存在执行器所在机器。
    pub llm_api_key: Option<String>,
    pub llm_model: String,
    /// AI 请求超时（秒）。
    pub llm_timeout_secs: u64,
    /// 是否强制校验付费授权令牌；默认关闭，便于本地开发与自托管免费使用。
    pub license_required: bool,
    /// 账号服务 Ed25519 公钥：32 字节原始键或 SPKI DER 的 base64。
    pub license_public_key: Option<String>,
    /// 授权过期后的离线宽限小时数。
    pub license_grace_hours: u64,
    /// 授权令牌本地保存路径，文件权限固定为 600。
    pub license_file: PathBuf,
    /// 官方托管只读模式：只放行公共行情与资讯，拒绝账户、下单与策略写入。
    pub public_mode: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let bind_addr = env::var("ARBPILOT_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
            .parse()
            .context("ARBPILOT_BIND_ADDR 不是合法地址")?;

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

        let binance_fapi_url = env::var("ARBPILOT_BINANCE_FAPI_URL")
            .unwrap_or_else(|_| "https://fapi.binance.com".to_string());

        let binance_spot_url = env::var("ARBPILOT_BINANCE_SPOT_URL")
            .unwrap_or_else(|_| "https://api.binance.com".to_string());

        let okx_enabled = env::var("ARBPILOT_OKX_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);

        let okx_base_url =
            env::var("ARBPILOT_OKX_BASE_URL").unwrap_or_else(|_| "https://www.okx.com".to_string());

        let bybit_enabled = env::var("ARBPILOT_BYBIT_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let bybit_base_url = env::var("ARBPILOT_BYBIT_BASE_URL")
            .unwrap_or_else(|_| "https://api.bybit.com".to_string());

        let bitget_enabled = env::var("ARBPILOT_BITGET_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let bitget_base_url = env::var("ARBPILOT_BITGET_BASE_URL")
            .unwrap_or_else(|_| "https://api.bitget.com".to_string());

        let gate_enabled = env::var("ARBPILOT_GATE_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let gate_base_url = env::var("ARBPILOT_GATE_BASE_URL")
            .unwrap_or_else(|_| "https://api.gateio.ws".to_string());

        let aster_enabled = env::var("ARBPILOT_ASTER_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let aster_base_url = env::var("ARBPILOT_ASTER_BASE_URL")
            .unwrap_or_else(|_| "https://fapi.asterdex.com".to_string());

        let hyperliquid_enabled = env::var("ARBPILOT_HYPERLIQUID_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let hyperliquid_base_url = env::var("ARBPILOT_HYPERLIQUID_BASE_URL")
            .unwrap_or_else(|_| "https://api.hyperliquid.xyz".to_string());

        let lighter_enabled = env::var("ARBPILOT_LIGHTER_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let lighter_base_url = env::var("ARBPILOT_LIGHTER_BASE_URL")
            .unwrap_or_else(|_| "https://mainnet.zklighter.elliot.ai".to_string());

        let htx_enabled = env::var("ARBPILOT_HTX_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let htx_base_url = env::var("ARBPILOT_HTX_BASE_URL")
            .unwrap_or_else(|_| "https://api.hbdm.com".to_string());

        let mexc_enabled = env::var("ARBPILOT_MEXC_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let mexc_base_url = env::var("ARBPILOT_MEXC_BASE_URL")
            .unwrap_or_else(|_| "https://contract.mexc.com".to_string());

        let bingx_enabled = env::var("ARBPILOT_BINGX_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let bingx_base_url = env::var("ARBPILOT_BINGX_BASE_URL")
            .unwrap_or_else(|_| "https://open-api.bingx.com".to_string());

        let binance_spot_ws_url = env::var("ARBPILOT_BINANCE_SPOT_WS_URL")
            .unwrap_or_else(|_| "wss://stream.binance.com:9443".to_string());

        let binance_futures_ws_url = env::var("ARBPILOT_BINANCE_FUTURES_WS_URL")
            .unwrap_or_else(|_| "wss://fstream.binance.com".to_string());

        let okx_ws_url = env::var("ARBPILOT_OKX_WS_URL")
            .unwrap_or_else(|_| "wss://ws.okx.com/ws/v5/public".to_string());

        let bybit_ws_url = env::var("ARBPILOT_BYBIT_WS_URL")
            .unwrap_or_else(|_| "wss://stream.bybit.com/v5/public/linear".to_string());
        let bybit_spot_ws_url = env::var("ARBPILOT_BYBIT_SPOT_WS_URL")
            .unwrap_or_else(|_| "wss://stream.bybit.com/v5/public/spot".to_string());
        let bitget_ws_url = env::var("ARBPILOT_BITGET_WS_URL")
            .unwrap_or_else(|_| "wss://ws.bitget.com/v2/ws/public".to_string());
        let gate_ws_url = env::var("ARBPILOT_GATE_WS_URL")
            .unwrap_or_else(|_| "wss://fx-ws.gateio.ws/v4/ws/usdt".to_string());
        let aster_ws_url = env::var("ARBPILOT_ASTER_WS_URL")
            .unwrap_or_else(|_| "wss://fstream.asterdex.com".to_string());
        let htx_ws_url = env::var("ARBPILOT_HTX_WS_URL")
            .unwrap_or_else(|_| "wss://api.hbdm.com/linear-swap-ws".to_string());
        let mexc_ws_url = env::var("ARBPILOT_MEXC_WS_URL")
            .unwrap_or_else(|_| "wss://contract.mexc.com/edge".to_string());
        let bingx_ws_url = env::var("ARBPILOT_BINGX_WS_URL")
            .unwrap_or_else(|_| "wss://open-api-swap.bingx.com/swap-market".to_string());
        let hyperliquid_ws_url = env::var("ARBPILOT_HYPERLIQUID_WS_URL")
            .unwrap_or_else(|_| "wss://api.hyperliquid.xyz/ws".to_string());
        let lighter_ws_url = env::var("ARBPILOT_LIGHTER_WS_URL")
            .unwrap_or_else(|_| "wss://mainnet.zklighter.elliot.ai/stream".to_string());

        let quote_poll_secs = env::var("ARBPILOT_QUOTE_POLL_SECS")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .context("解析 ARBPILOT_QUOTE_POLL_SECS 失败")?;

        // 优先使用专用变量，其次沿用系统里常见的代理变量。
        let market_proxy = [
            "ARBPILOT_MARKET_PROXY",
            "ARBPILOT_BINANCE_PROXY",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
        ]
        .iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty());

        let funding_enabled = env::var("ARBPILOT_FUNDING_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);

        // 留空表示自动发现：Binance 用 exchangeInfo、OKX 用 public/instruments。
        let funding_symbols = env::var("ARBPILOT_FUNDING_SYMBOLS")
            .map(|value| parse_symbols(&value))
            .unwrap_or_default();

        // 盘口与机会必须逐币兜底 REST，默认只保留流动性最好的两个币种；
        // 未配置时回退到手动指定的资金费率列表。
        let market_symbols = env::var("ARBPILOT_MARKET_SYMBOLS")
            .map(|value| parse_symbols(&value))
            .unwrap_or_else(|_| {
                if funding_symbols.is_empty() {
                    parse_symbols("BTCUSDT,ETHUSDT")
                } else {
                    funding_symbols.clone()
                }
            });

        let funding_poll_secs = env::var("ARBPILOT_FUNDING_POLL_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .context("解析 ARBPILOT_FUNDING_POLL_SECS 失败")?;

        let news_enabled = env::var("ARBPILOT_NEWS_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let news_poll_secs = env::var("ARBPILOT_NEWS_POLL_SECS")
            .unwrap_or_else(|_| "120".to_string())
            .parse()
            .context("解析 ARBPILOT_NEWS_POLL_SECS 失败")?;
        let news_binance_url = env::var("ARBPILOT_NEWS_BINANCE_URL").unwrap_or_else(|_| {
            "https://www.binance.com/bapi/composite/v1/public/cms/article/list/query?type=1&pageNo=1&pageSize=20&catalogId=48"
                .to_string()
        });
        let news_upbit_url = env::var("ARBPILOT_NEWS_UPBIT_URL").unwrap_or_else(|_| {
            "https://api-manager.upbit.com/api/v1/announcements?os=web&page=1&per_page=20&category=trade"
                .to_string()
        });
        let news_okx_url = env::var("ARBPILOT_NEWS_OKX_URL").unwrap_or_else(|_| {
            "https://www.okx.com/api/v5/support/announcements?annType=announcements-new-listings&page=1"
                .to_string()
        });
        let news_bybit_url = env::var("ARBPILOT_NEWS_BYBIT_URL").unwrap_or_else(|_| {
            "https://api.bybit.com/v5/announcements/index?locale=en-US&type=new_crypto&limit=20"
                .to_string()
        });
        // Bitget 的路径拼写就是 annoucements，不要改成 announcements。
        let news_bitget_url = env::var("ARBPILOT_NEWS_BITGET_URL").unwrap_or_else(|_| {
            "https://api.bitget.com/api/v2/public/annoucements?language=en_US&annType=coin_listings"
                .to_string()
        });

        // 链上价格监听默认开启，但没有 RPC 地址时不会发出任何请求。
        let chain_enabled = env::var("ARBPILOT_CHAIN_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let chain_rpc_url = env::var("ARBPILOT_CHAIN_RPC_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let chain_poll_secs = env::var("ARBPILOT_CHAIN_POLL_SECS")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .context("解析 ARBPILOT_CHAIN_POLL_SECS 失败")?;
        let chain_alert_bps = env::var("ARBPILOT_CHAIN_ALERT_BPS")
            .unwrap_or_else(|_| "100".to_string())
            .parse()
            .context("解析 ARBPILOT_CHAIN_ALERT_BPS 失败")?;
        let chain_feeds = match env::var("ARBPILOT_CHAIN_FEEDS") {
            Ok(value) if !value.trim().is_empty() => {
                serde_json::from_str::<Vec<ChainFeedConfig>>(&value)
                    .context("解析 ARBPILOT_CHAIN_FEEDS 失败")?
            }
            _ => default_chain_feeds(),
        };
        for feed in &chain_feeds {
            validate_address(&feed.pool, "chain feed pool")?;
            if let Some(oracle) = feed.oracle.as_deref() {
                validate_address(oracle, "chain feed oracle")?;
            }
        }

        let accounts_file = env::var("ARBPILOT_ACCOUNTS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/accounts.json"));

        // 设置页修改的限额优先于环境变量，重启后继续生效。
        let risk_file = env::var("ARBPILOT_RISK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| accounts_file.with_file_name("risk_limits.json"));
        if let Ok(text) = std::fs::read_to_string(&risk_file) {
            match serde_json::from_str::<RiskLimits>(&text) {
                Ok(limits) => risk = limits,
                Err(error) => tracing::warn!(
                    %error,
                    path = %risk_file.display(),
                    "风控限额文件解析失败，沿用环境变量默认值"
                ),
            }
        }

        // Telegram 只负责告警、查询与经确认的下单；API Key 始终留在执行器所在机器。
        let telegram_bot_token = env::var("ARBPILOT_TELEGRAM_BOT_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let telegram_enabled = env::var("ARBPILOT_TELEGRAM_ENABLED")
            .map(|value| !value.eq_ignore_ascii_case("false"))
            .unwrap_or(telegram_bot_token.is_some());
        let telegram_api_base = env::var("ARBPILOT_TELEGRAM_API_BASE")
            .unwrap_or_else(|_| "https://api.telegram.org".to_string());
        let telegram_poll_secs = env::var("ARBPILOT_TELEGRAM_POLL_SECS")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .context("解析 ARBPILOT_TELEGRAM_POLL_SECS 失败")?;
        let telegram_pairing_secs = env::var("ARBPILOT_TELEGRAM_PAIRING_SECS")
            .unwrap_or_else(|_| "600".to_string())
            .parse()
            .context("解析 ARBPILOT_TELEGRAM_PAIRING_SECS 失败")?;
        let telegram_bindings_file = env::var("ARBPILOT_TELEGRAM_BINDINGS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| accounts_file.with_file_name("telegram_bindings.json"));
        let accounts_poll_secs = env::var("ARBPILOT_ACCOUNTS_POLL_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .context("解析 ARBPILOT_ACCOUNTS_POLL_SECS 失败")?;
        let strategy_poll_secs = env::var("ARBPILOT_STRATEGY_POLL_SECS")
            .unwrap_or_else(|_| "15".to_string())
            .parse()
            .context("解析 ARBPILOT_STRATEGY_POLL_SECS 失败")?;

        let mut sandbox = SandboxLimits::default();
        if let Ok(value) = env::var("ARBPILOT_SANDBOX_MAX_FUEL") {
            sandbox.max_fuel = value.parse().context("解析 ARBPILOT_SANDBOX_MAX_FUEL 失败")?;
        }
        if let Ok(value) = env::var("ARBPILOT_SANDBOX_MAX_MEMORY_BYTES") {
            sandbox.max_memory_bytes = value
                .parse()
                .context("解析 ARBPILOT_SANDBOX_MAX_MEMORY_BYTES 失败")?;
        }
        if let Ok(value) = env::var("ARBPILOT_SANDBOX_TIMEOUT_MS") {
            sandbox.timeout_ms = value
                .parse()
                .context("解析 ARBPILOT_SANDBOX_TIMEOUT_MS 失败")?;
        }
        let market_platform_fee_bps = env::var("ARBPILOT_MARKET_FEE_BPS")
            .unwrap_or_else(|_| "2000".to_string())
            .parse()
            .context("解析 ARBPILOT_MARKET_FEE_BPS 失败")?;

        // AI 只负责生成沙箱源码；源码仍需编译、校验并由用户确认后部署。
        let llm_base_url = env::var("ARBPILOT_LLM_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string())
            .trim_end_matches('/')
            .to_string();
        let llm_api_key = env::var("ARBPILOT_LLM_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let llm_model =
            env::var("ARBPILOT_LLM_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        let llm_timeout_secs = env::var("ARBPILOT_LLM_TIMEOUT_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .context("解析 ARBPILOT_LLM_TIMEOUT_SECS 失败")?;

        // 授权校验默认关闭：只有官方托管或需要按套餐限制时才打开。
        let license_required = env::var("ARBPILOT_LICENSE_REQUIRED")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let license_public_key = env::var("ARBPILOT_LICENSE_PUBLIC_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let license_grace_hours = env::var("ARBPILOT_LICENSE_GRACE_HOURS")
            .unwrap_or_else(|_| "72".to_string())
            .parse()
            .context("解析 ARBPILOT_LICENSE_GRACE_HOURS 失败")?;
        let license_file = env::var("ARBPILOT_LICENSE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| accounts_file.with_file_name("license.json"));
        let public_mode = env::var("ARBPILOT_PUBLIC_MODE")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Ok(Self {
            bind_addr,
            api_token,
            database_url: env::var("DATABASE_URL").ok().filter(|url| !url.is_empty()),
            local_db: env::var("ARBPILOT_LOCAL_DB")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            risk,
            funding_enabled,
            binance_fapi_url,
            market_proxy,
            binance_spot_url,
            okx_enabled,
            okx_base_url,
            bybit_enabled,
            bybit_base_url,
            bitget_enabled,
            bitget_base_url,
            gate_enabled,
            gate_base_url,
            aster_enabled,
            aster_base_url,
            hyperliquid_enabled,
            hyperliquid_base_url,
            lighter_enabled,
            lighter_base_url,
            htx_enabled,
            htx_base_url,
            mexc_enabled,
            mexc_base_url,
            bingx_enabled,
            bingx_base_url,
            binance_spot_ws_url,
            binance_futures_ws_url,
            okx_ws_url,
            bybit_ws_url,
            bybit_spot_ws_url,
            bitget_ws_url,
            gate_ws_url,
            aster_ws_url,
            htx_ws_url,
            mexc_ws_url,
            bingx_ws_url,
            hyperliquid_ws_url,
            lighter_ws_url,
            quote_poll_secs,
            funding_symbols,
            market_symbols,
            funding_poll_secs,
            news_enabled,
            news_poll_secs,
            news_binance_url,
            news_upbit_url,
            news_okx_url,
            news_bybit_url,
            news_bitget_url,
            chain_enabled,
            chain_rpc_url,
            chain_poll_secs,
            chain_alert_bps,
            chain_feeds,
            telegram_enabled,
            telegram_bot_token,
            telegram_api_base,
            telegram_poll_secs,
            telegram_pairing_secs,
            telegram_bindings_file,
            accounts_file,
            risk_file,
            accounts_poll_secs,
            strategy_poll_secs,
            sandbox,
            market_platform_fee_bps,
            llm_base_url,
            llm_api_key,
            llm_model,
            llm_timeout_secs,
            license_required,
            license_public_key,
            license_grace_hours,
            license_file,
            public_mode,
        })
    }
}

/// 解析逗号分隔的交易对列表，统一为大写。
fn parse_symbols(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|symbol| symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}
