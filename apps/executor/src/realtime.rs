//! 实时行情：WebSocket 优先，REST 兜底。
//!
//! - Binance / Aster：`<symbol>@bookTicker` 逐笔最优买卖价；
//! - OKX 现货 / 永续：`tickers` 频道提供最优买卖价，`funding-rate` 频道推送当期费率；
//! - Bybit / Bitget / Gate / HTX / MEXC / BingX / Hyperliquid / Lighter：
//!   各自的公共最优档频道，统一折算成名义金额后写入共享表。
//!
//! 每个交易所一条独立线程，断线后自动重连；行情写入共享表，`MarketFeed` /
//! `FundingFeed` 读取时按新鲜度判断是否回退到 REST。

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::bail;
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::{
    config::Config,
    exchange::{
        make_book, millis_to_time, parse_decimal, BookSnapshot, FundingSnapshot,
        GateFundingClient, HtxFundingClient, LighterFundingClient, MexcFundingClient,
        OkxMarketClient, ASTER_VENUE, BINGX_VENUE, BITGET_VENUE, BYBIT_VENUE, GATE_VENUE,
        HTX_VENUE, HYPERLIQUID_VENUE, LIGHTER_VENUE, MEXC_VENUE, OKX_VENUE, VENUE,
    },
    http_client::HttpClient,
    market::{OKX_SPOT_VENUE, SPOT_VENUE},
    ws::{WebSocket, WsEvent},
};

/// 握手超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次读取的阻塞时间，决定心跳检查频率。
const POLL_TIMEOUT: Duration = Duration::from_millis(250);
/// OKX 要求 30 秒内至少发送一次心跳。
const OKX_PING_INTERVAL: Duration = Duration::from_secs(25);
/// Bybit 要求 20 秒内至少发送一次心跳。
const BYBIT_PING_INTERVAL: Duration = Duration::from_secs(20);
/// Bitget 要求 30 秒内至少发送一次心跳。
const BITGET_PING_INTERVAL: Duration = Duration::from_secs(25);
/// Gate 服务端也会主动 ping，这里再做一层客户端心跳。
const GATE_PING_INTERVAL: Duration = Duration::from_secs(20);
/// HTX 要求 5 秒内至少发送一次心跳。
const HTX_PING_INTERVAL: Duration = Duration::from_secs(5);
/// MEXC 心跳间隔。
const MEXC_PING_INTERVAL: Duration = Duration::from_secs(15);
/// BingX 心跳间隔。
const BINGX_PING_INTERVAL: Duration = Duration::from_secs(15);
/// Hyperliquid 心跳间隔。
const HYPERLIQUID_PING_INTERVAL: Duration = Duration::from_secs(30);
/// 断线重连间隔。
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// OKX 单次 subscribe 请求的 args 上限，超过需分批发送。
const OKX_SUBSCRIBE_CHUNK: usize = 100;

/// `BTCUSDT` → `BTC-USDT` / `BTC-USDT-SWAP`。
fn okx_inst_id(symbol: &str, swap: bool) -> String {
    let base = symbol.trim().trim_end_matches("USDT");
    if swap {
        format!("{base}-USDT-SWAP")
    } else {
        format!("{base}-USDT")
    }
}

#[derive(Debug, Clone)]
pub struct BookUpdate {
    pub snapshot: BookSnapshot,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct FundingUpdate {
    pub snapshot: FundingSnapshot,
    pub observed_at: DateTime<Utc>,
}

/// WebSocket 订阅的交易对清单。
///
/// `symbols` 是全局 watchlist（资金费率由 `FundingFeed` 自动发现后写入）；
/// `venue_symbols` 记录持仓涉及的交易对，只向对应交易所订阅，避免各所互相订阅
/// 不存在的合约。订阅线程按版本号变化重建连接。
#[derive(Default)]
pub struct SymbolRegistry {
    symbols: RwLock<Vec<String>>,
    venue_symbols: RwLock<HashMap<String, Vec<String>>>,
    version: AtomicU64,
}

impl SymbolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 替换清单；内容未变化时不增加版本号。
    pub fn set(&self, symbols: Vec<String>) {
        {
            let current = self.symbols.read().expect("订阅清单锁不可用");
            if *current == symbols {
                return;
            }
        }
        *self.symbols.write().expect("订阅清单锁不可用") = symbols;
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录某交易所持仓涉及的交易对；只追加不删除，内容变化时提升版本号。
    pub fn extend_venue(&self, venue: &str, symbols: Vec<String>) {
        if symbols.is_empty() {
            return;
        }
        let mut changed = false;
        {
            let mut venue_symbols = self.venue_symbols.write().expect("订阅清单锁不可用");
            let entry = venue_symbols.entry(venue.to_string()).or_default();
            for symbol in symbols {
                if !symbol.is_empty() && !entry.contains(&symbol) {
                    entry.push(symbol);
                    changed = true;
                }
            }
        }
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> Vec<String> {
        self.symbols.read().expect("订阅清单锁不可用").clone()
    }

    /// 某交易所需要订阅的全部交易对：全局 watchlist + 该交易所持仓。
    fn symbols_for(&self, venue: &str) -> Vec<String> {
        let mut symbols = self.snapshot();
        if let Some(extra) = self
            .venue_symbols
            .read()
            .expect("订阅清单锁不可用")
            .get(venue)
        {
            for symbol in extra {
                if !symbols.contains(symbol) {
                    symbols.push(symbol.clone());
                }
            }
        }
        symbols
    }
}

/// 各交易所推送的最新盘口与当期资金费率。
pub struct RealtimeFeed {
    books: RwLock<HashMap<(String, String), BookUpdate>>,
    funding: RwLock<HashMap<(String, String), FundingUpdate>>,
    funding_symbols: Arc<SymbolRegistry>,
    /// 盘口订阅清单：watchlist + 各交易所的持仓交易对。
    book_symbols: Arc<SymbolRegistry>,
}

impl RealtimeFeed {
    /// 启动各交易所的 WebSocket 订阅线程。
    ///
    /// Binance 始终订阅；其余交易所按各自的启用开关挂载，未启用的不建立连接。
    pub fn start(
        config: &Config,
        http: Arc<HttpClient>,
        okx: Option<Arc<OkxMarketClient>>,
    ) -> Arc<Self> {
        let book_symbols = Arc::new(SymbolRegistry::new());
        book_symbols.set(config.market_symbols.clone());
        let feed = Arc::new(Self {
            books: RwLock::new(HashMap::new()),
            funding: RwLock::new(HashMap::new()),
            funding_symbols: Arc::new(SymbolRegistry::new()),
            book_symbols: book_symbols.clone(),
        });

        let binance_spot = feed.clone();
        let spot_http = http.clone();
        let spot_url = config.binance_spot_ws_url.clone();
        let spot_symbols = book_symbols.clone();
        spawn("binance-spot-ws", move || {
            run_binance_books(binance_spot, spot_http, spot_url, SPOT_VENUE, spot_symbols)
        });

        let binance_futures = feed.clone();
        let futures_http = http.clone();
        let futures_url = config.binance_futures_ws_url.clone();
        let futures_symbols = book_symbols.clone();
        spawn("binance-futures-ws", move || {
            run_binance_books(
                binance_futures,
                futures_http,
                futures_url,
                VENUE,
                futures_symbols,
            )
        });

        if let Some(client) = okx {
            let okx_feed = feed.clone();
            let okx_ws_url = config.okx_ws_url.clone();
            let registry = feed.funding_symbols();
            let okx_http = http.clone();
            let okx_symbols = book_symbols.clone();
            spawn("okx-ws", move || {
                run_okx(okx_feed, okx_http, okx_ws_url, client, okx_symbols, registry)
            });
        }

        if config.aster_enabled {
            let aster_feed = feed.clone();
            let aster_http = http.clone();
            let aster_url = config.aster_ws_url.clone();
            let aster_symbols = book_symbols.clone();
            spawn("aster-ws", move || {
                run_binance_books(
                    aster_feed,
                    aster_http,
                    aster_url,
                    ASTER_VENUE,
                    aster_symbols,
                )
            });
        }

        if config.bybit_enabled {
            let bybit_feed = feed.clone();
            let bybit_http = http.clone();
            let bybit_url = config.bybit_ws_url.clone();
            let bybit_symbols = book_symbols.clone();
            spawn_loop("bybit-ws", move || {
                stream_bybit(
                    &bybit_feed,
                    &bybit_http,
                    &bybit_url,
                    &bybit_symbols,
                    BYBIT_VENUE,
                )
            });

            let spot_feed = feed.clone();
            let spot_http = http.clone();
            let spot_ws_url = config.bybit_spot_ws_url.clone();
            let bybit_spot_symbols = book_symbols.clone();
            spawn_loop("bybit-spot-ws", move || {
                stream_bybit(
                    &spot_feed,
                    &spot_http,
                    &spot_ws_url,
                    &bybit_spot_symbols,
                    crate::market::BYBIT_SPOT_VENUE,
                )
            });
        }

        if config.bitget_enabled {
            let bitget_feed = feed.clone();
            let bitget_http = http.clone();
            let bitget_url = config.bitget_ws_url.clone();
            let bitget_symbols = book_symbols.clone();
            spawn_loop("bitget-ws", move || {
                stream_bitget(
                    &bitget_feed,
                    &bitget_http,
                    &bitget_url,
                    &bitget_symbols,
                )
            });
        }

        if config.gate_enabled {
            let client = Arc::new(GateFundingClient::new(&config.gate_base_url, http.clone()));
            let gate_feed = feed.clone();
            let gate_http = http.clone();
            let gate_url = config.gate_ws_url.clone();
            let gate_symbols = book_symbols.clone();
            spawn_loop("gate-ws", move || {
                stream_gate(&gate_feed, &gate_http, &gate_url, &gate_symbols, &client)
            });
        }

        if config.htx_enabled {
            let client = Arc::new(HtxFundingClient::new(&config.htx_base_url, http.clone()));
            let htx_feed = feed.clone();
            let htx_http = http.clone();
            let htx_url = config.htx_ws_url.clone();
            let htx_symbols = book_symbols.clone();
            spawn_loop("htx-ws", move || {
                stream_htx(&htx_feed, &htx_http, &htx_url, &htx_symbols, &client)
            });
        }

        if config.mexc_enabled {
            let client = Arc::new(MexcFundingClient::new(&config.mexc_base_url, http.clone()));
            let mexc_feed = feed.clone();
            let mexc_http = http.clone();
            let mexc_url = config.mexc_ws_url.clone();
            let mexc_symbols = book_symbols.clone();
            spawn_loop("mexc-ws", move || {
                stream_mexc(&mexc_feed, &mexc_http, &mexc_url, &mexc_symbols, &client)
            });
        }

        if config.bingx_enabled {
            let bingx_feed = feed.clone();
            let bingx_http = http.clone();
            let bingx_url = config.bingx_ws_url.clone();
            let bingx_symbols = book_symbols.clone();
            spawn_loop("bingx-ws", move || {
                stream_bingx(
                    &bingx_feed,
                    &bingx_http,
                    &bingx_url,
                    &bingx_symbols,
                )
            });
        }

        if config.hyperliquid_enabled {
            let hl_feed = feed.clone();
            let hl_http = http.clone();
            let hl_url = config.hyperliquid_ws_url.clone();
            let hl_symbols = book_symbols.clone();
            spawn_loop("hyperliquid-ws", move || {
                stream_hyperliquid(&hl_feed, &hl_http, &hl_url, &hl_symbols)
            });
        }

        if config.lighter_enabled {
            let client = Arc::new(LighterFundingClient::new(
                &config.lighter_base_url,
                http.clone(),
            ));
            let lighter_feed = feed.clone();
            let lighter_http = http.clone();
            let lighter_url = config.lighter_ws_url.clone();
            let lighter_symbols = book_symbols.clone();
            spawn_loop("lighter-ws", move || {
                stream_lighter(
                    &lighter_feed,
                    &lighter_http,
                    &lighter_url,
                    &lighter_symbols,
                    &client,
                )
            });
        }

        feed
    }

    /// 资金费率订阅清单，由 `FundingFeed` 自动发现后写入。
    pub fn funding_symbols(&self) -> Arc<SymbolRegistry> {
        self.funding_symbols.clone()
    }

    /// 盘口订阅清单，持仓同步后把持仓交易对追加进来即可实时收到价格。
    pub fn book_symbols(&self) -> Arc<SymbolRegistry> {
        self.book_symbols.clone()
    }

    /// 读取某交易对的最新推送盘口。
    pub fn book(&self, venue: &str, symbol: &str) -> Option<BookUpdate> {
        self.books
            .read()
            .expect("实时行情锁不可用")
            .get(&(venue.to_string(), symbol.to_string()))
            .cloned()
    }

    pub fn funding(&self, venue: &str, symbol: &str) -> Option<FundingUpdate> {
        self.funding
            .read()
            .expect("实时行情锁不可用")
            .get(&(venue.to_string(), symbol.to_string()))
            .cloned()
    }

    /// 当前所有盘口快照，供 `MarketFeed` 合并进报价表。
    pub fn books(&self) -> HashMap<(String, String), BookUpdate> {
        self.books.read().expect("实时行情锁不可用").clone()
    }

    fn store_book(&self, venue: &str, symbol: &str, snapshot: BookSnapshot, at: DateTime<Utc>) {
        let key = (venue.to_string(), symbol.to_string());
        let first = {
            let mut books = self.books.write().expect("实时行情锁不可用");
            let first = !books.contains_key(&key);
            books.insert(
                key,
                BookUpdate {
                    snapshot,
                    observed_at: at,
                },
            );
            first
        };
        if first {
            tracing::info!(venue, symbol, "已收到 WebSocket 盘口推送");
        }
    }

    fn store_funding(&self, venue: &str, symbol: &str, snapshot: FundingSnapshot) {
        let at = snapshot.observed_at;
        self.funding.write().expect("实时行情锁不可用").insert(
            (venue.to_string(), symbol.to_string()),
            FundingUpdate {
                snapshot,
                observed_at: at,
            },
        );
    }
}

fn spawn(name: &str, task: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new().name(name.to_string()).spawn(task) {
        tracing::error!(%error, task = name, "启动 WebSocket 线程失败");
    }
}

/// 带自动重连的订阅循环；连续失败时每分钟只打一条日志。
fn spawn_loop<F>(name: &'static str, stream: F)
where
    F: Fn() -> anyhow::Result<()> + Send + 'static,
{
    spawn(name, move || {
        let mut last_error: Option<Instant> = None;
        loop {
            match stream() {
                Ok(()) => tracing::warn!(task = name, "WebSocket 已结束，准备重连"),
                Err(error) => {
                    let now = Instant::now();
                    let should_log = last_error
                        .map(|at| now.duration_since(at) >= Duration::from_secs(60))
                        .unwrap_or(true);
                    if should_log {
                        tracing::warn!(task = name, %error, "WebSocket 连接失败，稍后重试");
                        last_error = Some(now);
                    }
                }
            }
            thread::sleep(RECONNECT_DELAY);
        }
    });
}

/// 订阅清单变化时是否需要重新连接。
///
/// 版本号是全局的，但只有本交易所的交易对列表真的变了才需要重连，
/// 其它交易所新增持仓不应打断本连接。
fn needs_reconnect(
    registry: &SymbolRegistry,
    venue: &str,
    symbols: &[String],
    version: &mut u64,
) -> bool {
    let current = registry.version();
    if current == *version {
        return false;
    }
    if registry.symbols_for(venue) == symbols {
        *version = current;
        return false;
    }
    true
}

/// 把文本 / 二进制（可选 gzip）消息还原成 JSON 文本。
fn message_text(event: WsEvent, gzip: bool) -> anyhow::Result<Option<String>> {
    match event {
        WsEvent::Text(text) => Ok(Some(text)),
        WsEvent::Binary(bytes) if gzip => {
            let mut decoder = GzDecoder::new(&bytes[..]);
            let mut text = String::new();
            decoder
                .read_to_string(&mut text)
                .map_err(|error| anyhow::anyhow!("解压 WebSocket 消息失败: {error}"))?;
            Ok(Some(text))
        }
        _ => Ok(None),
    }
}

/// 上线前预取每张合约的乘数，避免盘口推送时逐条查 REST。
fn fetch_multipliers(
    client: &GateFundingClient,
    symbols: &[String],
) -> HashMap<String, Decimal> {
    let mut multipliers = HashMap::new();
    for symbol in symbols {
        match client.symbol_multiplier(symbol) {
            Ok(value) => {
                multipliers.insert(symbol.clone(), value);
            }
            Err(error) => {
                tracing::warn!(venue = "gate", symbol, %error, "获取合约乘数失败，该合约暂用 REST 盘口")
            }
        }
    }
    multipliers
}

fn fetch_contract_sizes(
    client: &HtxFundingClient,
    symbols: &[String],
) -> HashMap<String, Decimal> {
    let mut sizes = HashMap::new();
    for symbol in symbols {
        match client.symbol_contract_size(symbol) {
            Ok(value) => {
                sizes.insert(symbol.clone(), value);
            }
            Err(error) => {
                tracing::warn!(venue = "htx", symbol, %error, "获取合约面值失败，该合约暂用 REST 盘口")
            }
        }
    }
    sizes
}

fn fetch_mexc_sizes(
    client: &MexcFundingClient,
    symbols: &[String],
) -> HashMap<String, Decimal> {
    let mut sizes = HashMap::new();
    for symbol in symbols {
        match client.symbol_contract_size(symbol) {
            Ok(value) => {
                sizes.insert(symbol.clone(), value);
            }
            Err(error) => {
                tracing::warn!(venue = "mexc", symbol, %error, "获取合约面值失败，该合约暂用 REST 盘口")
            }
        }
    }
    sizes
}

/// `BTCUSDT` → `BTC`（Hyperliquid 的币种名）。
fn hyperliquid_coin(symbol: &str) -> Option<String> {
    let base = symbol.trim().trim_end_matches("USDT");
    (!base.is_empty()).then(|| base.to_string())
}

/// `BTCUSDT` → `BTC_USDT`（Gate）。
fn gate_contract(symbol: &str) -> Option<String> {
    let base = symbol.trim().trim_end_matches("USDT");
    (!base.is_empty()).then(|| format!("{base}_USDT"))
}

/// `BTCUSDT` → `BTC-USDT`（HTX / BingX）。
fn dash_contract(symbol: &str) -> Option<String> {
    let base = symbol.trim().trim_end_matches("USDT");
    (!base.is_empty()).then(|| format!("{base}-USDT"))
}

/// `BTCUSDT` → `BTC_USDT`（MEXC）。
fn mexc_contract(symbol: &str) -> Option<String> {
    gate_contract(symbol)
}

/// Bybit 公共盘口：`orderbook.1.<symbol>`。
fn stream_bybit(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
    venue: &str,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(venue);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    let args = symbols
        .iter()
        .map(|symbol| format!("\"orderbook.1.{}\"", symbol.to_ascii_uppercase()))
        .collect::<Vec<_>>()
        .join(",");
    ws.send_text(&format!(r#"{{"op":"subscribe","args":[{args}]}}"#))?;

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        // 本交易所持仓变化时断开重连，用新的交易对清单重新订阅。
        if needs_reconnect(registry, venue, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= BYBIT_PING_INTERVAL {
            ws.send_text(r#"{"op":"ping"}"#)?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "bybit", %error, "忽略无法解析的 Bybit 消息");
                continue;
            }
        };
        if value.get("op").and_then(Value::as_str) == Some("ping") {
            ws.send_text(r#"{"op":"pong"}"#)?;
            continue;
        }
        let Some(topic) = value.get("topic").and_then(Value::as_str) else {
            continue;
        };
        let Some(symbol) = topic.strip_prefix("orderbook.1.") else {
            continue;
        };
        let Some(data) = value.get("data") else {
            continue;
        };
        let level = |side: &str| -> Option<(Decimal, Decimal)> {
            let row = data.pointer(&format!("/{side}/0"))?;
            Some((
                parse_decimal(row.get(0)?.as_str()?).ok()?,
                parse_decimal(row.get(1)?.as_str()?).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level("b"), level("a")) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty, ask, ask_qty) {
            feed.store_book(venue, &symbol.to_ascii_uppercase(), snapshot, Utc::now());
        }
    }
}

/// Bitget 公共盘口：`books1` 频道。
fn stream_bitget(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(BITGET_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    let args = symbols
        .iter()
        .map(|symbol| {
            format!(
                r#"{{"instType":"USDT-FUTURES","channel":"books1","instId":"{}"}}"#,
                symbol.to_ascii_uppercase()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    ws.send_text(&format!(r#"{{"op":"subscribe","args":[{args}]}}"#))?;

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, BITGET_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= BITGET_PING_INTERVAL {
            ws.send_text("ping")?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        if text == "pong" {
            continue;
        }
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "bitget", %error, "忽略无法解析的 Bitget 消息");
                continue;
            }
        };
        let Some(symbol) = value.pointer("/arg/instId").and_then(Value::as_str) else {
            continue;
        };
        let Some(item) = value
            .get("data")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
        else {
            continue;
        };
        let level = |side: &str| -> Option<(Decimal, Decimal)> {
            let row = item.get(side)?.as_array()?.first()?;
            Some((
                parse_decimal(row.get(0)?.as_str()?).ok()?,
                parse_decimal(row.get(1)?.as_str()?).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level("bids"), level("asks")) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty, ask, ask_qty) {
            feed.store_book(BITGET_VENUE, &symbol.to_ascii_uppercase(), snapshot, Utc::now());
        }
    }
}

/// Gate USDT 本位永续盘口：`futures.book_ticker`，数量单位是张。
fn stream_gate(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
    client: &GateFundingClient,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(GATE_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    // 连接建立时预取合约乘数，失败只影响名义金额，不影响价格。
    let multipliers = fetch_multipliers(client, &symbols);
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    let payload = symbols
        .iter()
        .filter_map(|symbol| gate_contract(symbol))
        .collect::<Vec<_>>();
    ws.send_text(
        &json!({
            "time": Utc::now().timestamp(),
            "channel": "futures.book_ticker",
            "event": "subscribe",
            "payload": payload,
        })
        .to_string(),
    )?;

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, GATE_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= GATE_PING_INTERVAL {
            ws.send_text(
                &json!({ "time": Utc::now().timestamp(), "channel": "futures.ping" })
                    .to_string(),
            )?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "gate", %error, "忽略无法解析的 Gate 消息");
                continue;
            }
        };
        let Some(channel) = value.get("channel").and_then(Value::as_str) else {
            continue;
        };
        if channel.ends_with(".ping") {
            ws.send_text(
                &json!({
                    "time": Utc::now().timestamp(),
                    "channel": channel.replace(".ping", ".pong"),
                })
                .to_string(),
            )?;
            continue;
        }
        // 订阅回执的 `event` 是 `subscribe`，行情推送的 `event` 是 `update`。
        if let Some(event) = value.get("event").and_then(Value::as_str) {
            if event != "update" {
                continue;
            }
        }
        let Some(result) = value.get("result") else {
            continue;
        };
        let Some(contract) = result.get("s").and_then(Value::as_str) else {
            continue;
        };
        let symbol = contract.replace('_', "").to_ascii_uppercase();
        let Some(multiplier) = multipliers.get(&symbol).copied() else {
            continue;
        };
        let text_field = |name: &str| -> Option<Decimal> {
            parse_decimal(result.get(name)?.as_str()?).ok()
        };
        // Gate 的数量字段 `B` / `A` 是数字而不是字符串。
        let number_field = |name: &str| -> Option<Decimal> {
            parse_decimal(&result.get(name)?.to_string()).ok()
        };
        let (Some(bid), Some(bid_size), Some(ask), Some(ask_size)) = (
            text_field("b"),
            number_field("B"),
            text_field("a"),
            number_field("A"),
        ) else {
            continue;
        };
        if let Ok(snapshot) = make_book(
            bid,
            bid_size * multiplier,
            ask,
            ask_size * multiplier,
        ) {
            feed.store_book(GATE_VENUE, &symbol, snapshot, Utc::now());
        }
    }
}

/// HTX USDT 本位永续盘口：`market.<contract>.bbo`，数量单位是张，消息为 gzip 压缩。
fn stream_htx(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
    client: &HtxFundingClient,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(HTX_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    // 连接建立时预取合约面值，失败只影响名义金额，不影响价格。
    let sizes = fetch_contract_sizes(client, &symbols);
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    for symbol in &symbols {
        if let Some(contract) = dash_contract(symbol) {
            ws.send_text(
                &json!({ "sub": format!("market.{contract}.bbo"), "id": "arbpilot" })
                    .to_string(),
            )?;
        }
    }

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, HTX_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= HTX_PING_INTERVAL {
            ws.send_text(&json!({ "ping": Utc::now().timestamp_millis() }).to_string())?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, true)? else {
            continue;
        };
        if text == "pong" {
            continue;
        }
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "htx", %error, "忽略无法解析的 HTX 消息");
                continue;
            }
        };
        let Some(channel) = value.get("ch").and_then(Value::as_str) else {
            continue;
        };
        let Some(contract) = channel
            .strip_prefix("market.")
            .and_then(|rest| rest.strip_suffix(".bbo"))
        else {
            continue;
        };
        let symbol = contract.replace('-', "").to_ascii_uppercase();
        let Some(size) = sizes.get(&symbol).copied() else {
            continue;
        };
        let level = |side: &str| -> Option<(Decimal, Decimal)> {
            let row = value.pointer(&format!("/tick/{side}"))?;
            Some((
                parse_decimal(&row.get(0)?.to_string()).ok()?,
                parse_decimal(&row.get(1)?.to_string()).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level("bid"), level("ask")) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty * size, ask, ask_qty * size) {
            feed.store_book(HTX_VENUE, &symbol, snapshot, Utc::now());
        }
    }
}

/// MEXC 合约盘口：`sub.depth.full`，数量单位是张。
fn stream_mexc(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
    client: &MexcFundingClient,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(MEXC_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    // 连接建立时预取合约面值，失败只影响名义金额，不影响价格。
    let sizes = fetch_mexc_sizes(client, &symbols);
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    for symbol in &symbols {
        if let Some(contract) = mexc_contract(symbol) {
            ws.send_text(
                &json!({
                    "method": "sub.depth.full",
                    "param": { "symbol": contract, "limit": 5 },
                })
                .to_string(),
            )?;
        }
    }

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, MEXC_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= MEXC_PING_INTERVAL {
            ws.send_text(&json!({ "method": "ping" }).to_string())?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "mexc", %error, "忽略无法解析的 MEXC 消息");
                continue;
            }
        };
        if value.get("channel").and_then(Value::as_str) != Some("push.depth.full") {
            continue;
        }
        let Some(data) = value.get("data") else {
            continue;
        };
        // MEXC 把 `symbol` 放在消息顶层，部分老版本才放在 data 里。
        let Some(contract) = value
            .get("symbol")
            .and_then(Value::as_str)
            .or_else(|| data.get("symbol").and_then(Value::as_str))
        else {
            continue;
        };
        let symbol = contract.replace('_', "").to_ascii_uppercase();
        let Some(size) = sizes.get(&symbol).copied() else {
            continue;
        };
        let level = |side: &str| -> Option<(Decimal, Decimal)> {
            let row = data.get(side)?.as_array()?.first()?;
            Some((
                parse_decimal(&row.get(0)?.to_string()).ok()?,
                parse_decimal(&row.get(1)?.to_string()).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level("bids"), level("asks")) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty * size, ask, ask_qty * size) {
            feed.store_book(MEXC_VENUE, &symbol, snapshot, Utc::now());
        }
    }
}

/// BingX 合约盘口：`<contract>@bookTicker`，消息为 gzip 压缩。
fn stream_bingx(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(BINGX_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    for symbol in &symbols {
        if let Some(contract) = dash_contract(symbol) {
            ws.send_text(
                &json!({
                    "id": "arbpilot",
                    "reqType": "sub",
                    "dataType": format!("{contract}@bookTicker"),
                })
                .to_string(),
            )?;
        }
    }

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, BINGX_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= BINGX_PING_INTERVAL {
            ws.send_text(&json!({ "ping": Utc::now().timestamp_millis().to_string() }).to_string())?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, true)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "bingx", %error, "忽略无法解析的 BingX 消息");
                continue;
            }
        };
        let Some(data_type) = value.get("dataType").and_then(Value::as_str) else {
            continue;
        };
        if !data_type.ends_with("@bookTicker") {
            continue;
        }
        let Some(data) = value.get("data") else {
            continue;
        };
        let Some(contract) = data.get("s").and_then(Value::as_str) else {
            continue;
        };
        let symbol = contract.replace('-', "").to_ascii_uppercase();
        let field = |name: &str| -> Option<Decimal> {
            parse_decimal(data.get(name)?.as_str()?).ok()
        };
        let (Some(bid), Some(bid_qty), Some(ask), Some(ask_qty)) =
            (field("b"), field("B"), field("a"), field("A"))
        else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty, ask, ask_qty) {
            feed.store_book(BINGX_VENUE, &symbol, snapshot, Utc::now());
        }
    }
}

/// Hyperliquid 盘口：`bbo` 频道直接推送最优买卖档。
fn stream_hyperliquid(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(HYPERLIQUID_VENUE);
    if symbols.is_empty() {
        bail!("没有可订阅的交易对");
    }
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    for symbol in &symbols {
        if let Some(coin) = hyperliquid_coin(symbol) {
            ws.send_text(
                &json!({ "method": "subscribe", "subscription": { "type": "bbo", "coin": coin } })
                    .to_string(),
            )?;
        }
    }

    let mut version = registry.version();
    let mut last_ping = Instant::now();
    loop {
        if needs_reconnect(registry, HYPERLIQUID_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        if last_ping.elapsed() >= HYPERLIQUID_PING_INTERVAL {
            ws.send_text(&json!({ "method": "ping" }).to_string())?;
            last_ping = Instant::now();
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "hyperliquid", %error, "忽略无法解析的 Hyperliquid 消息");
                continue;
            }
        };
        if value.get("channel").and_then(Value::as_str) != Some("bbo") {
            continue;
        }
        let Some(data) = value.get("data") else {
            continue;
        };
        let Some(coin) = data.get("coin").and_then(Value::as_str) else {
            continue;
        };
        let level = |index: usize| -> Option<(Decimal, Decimal)> {
            let row = data.pointer(&format!("/bbo/{index}"))?;
            Some((
                parse_decimal(row.get("px")?.as_str()?).ok()?,
                parse_decimal(row.get("sz")?.as_str()?).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level(0), level(1)) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty, ask, ask_qty) {
            feed.store_book(
                HYPERLIQUID_VENUE,
                &format!("{}USDT", coin.to_ascii_uppercase()),
                snapshot,
                Utc::now(),
            );
        }
    }
}

/// Lighter 盘口：`order_book/{market_id}` 频道。
fn stream_lighter(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    registry: &SymbolRegistry,
    client: &LighterFundingClient,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(LIGHTER_VENUE);
    let markets = match client.symbol_market_ids() {
        Ok(ids) => symbols
            .iter()
            .filter_map(|symbol| ids.get(symbol).map(|id| (symbol.clone(), *id)))
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(%error, "获取 Lighter 交易对映射失败，暂用 REST 盘口");
            Vec::new()
        }
    };
    if markets.is_empty() {
        bail!("没有可订阅的交易对");
    }
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    for (_, market_id) in &markets {
        ws.send_text(
            &json!({ "type": "subscribe", "channel": format!("order_book/{market_id}") })
                .to_string(),
        )?;
    }
    let mut by_id: HashMap<i64, &str> = HashMap::new();
    for (symbol, market_id) in &markets {
        by_id.insert(*market_id, symbol);
    }

    let mut version = registry.version();
    loop {
        if needs_reconnect(registry, LIGHTER_VENUE, &symbols, &mut version) {
            return Ok(());
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(target: "lighter", %error, "忽略无法解析的 Lighter 消息");
                continue;
            }
        };
        let Some(channel) = value.get("channel").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = channel
            .strip_prefix("order_book:")
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        let Some(symbol) = by_id.get(&id).copied() else {
            continue;
        };
        let Some(book) = value.get("order_book") else {
            continue;
        };
        let level = |side: &str| -> Option<(Decimal, Decimal)> {
            let row = book.get(side)?.as_array()?.first()?;
            Some((
                parse_decimal(row.get("price")?.as_str()?).ok()?,
                parse_decimal(row.get("size")?.as_str()?).ok()?,
            ))
        };
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (level("bids"), level("asks")) else {
            continue;
        };
        if let Ok(snapshot) = make_book(bid, bid_qty, ask, ask_qty) {
            feed.store_book(LIGHTER_VENUE, symbol, snapshot, Utc::now());
        }
    }
}


/// Binance 盘口线程：`/stream?streams=` 组合多条 `<symbol>@bookTicker`。
fn run_binance_books(
    feed: Arc<RealtimeFeed>,
    http: Arc<HttpClient>,
    base_url: String,
    venue: &'static str,
    registry: Arc<SymbolRegistry>,
) {
    tracing::info!(venue, "Binance WebSocket 盘口线程已启动");
    let mut last_error: Option<Instant> = None;
    loop {
        // 每次重连都按当前订阅清单重建 URL，新开仓的交易对随之生效。
        let symbols = registry.symbols_for(venue);
        if symbols.is_empty() {
            thread::sleep(RECONNECT_DELAY);
            continue;
        }
        let streams = symbols
            .iter()
            .map(|symbol| format!("{}@bookTicker", symbol.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/stream?streams={streams}",
            base_url.trim_end_matches('/')
        );

        match stream_binance(&feed, &http, &url, venue, &registry) {
            Ok(()) => tracing::warn!(venue, "Binance WebSocket 已结束，准备重连"),
            Err(error) => {
                let now = Instant::now();
                let should_log = last_error
                    .map(|at| now.duration_since(at) >= Duration::from_secs(60))
                    .unwrap_or(true);
                if should_log {
                    tracing::warn!(venue, %error, "Binance WebSocket 连接失败，稍后重试");
                    last_error = Some(now);
                }
            }
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

fn stream_binance(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    venue: &str,
    registry: &SymbolRegistry,
) -> anyhow::Result<()> {
    let symbols = registry.symbols_for(venue);
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    let mut version = registry.version();
    loop {
        if needs_reconnect(registry, venue, &symbols, &mut version) {
            return Ok(());
        }
        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "忽略无法解析的 Binance 消息");
                continue;
            }
        };
        if let Some((symbol, snapshot)) = parse_binance_book(&value) {
            feed.store_book(venue, &symbol, snapshot, Utc::now());
        }
    }
}

/// OKX 公共频道线程：现货/永续 `tickers` + 永续 `funding-rate`。
fn run_okx(
    feed: Arc<RealtimeFeed>,
    http: Arc<HttpClient>,
    url: String,
    client: Arc<OkxMarketClient>,
    books: Arc<SymbolRegistry>,
    funding: Arc<SymbolRegistry>,
) {
    tracing::info!(url = %url, "OKX WebSocket 行情线程已启动");
    let mut last_error: Option<Instant> = None;
    loop {
        let symbols = books.symbols_for(OKX_VENUE);
        if symbols.is_empty() {
            thread::sleep(RECONNECT_DELAY);
            continue;
        }
        // 永续盘口数量单位是「张」，每次连接前拉取面值，失败则跳过该合约的实时盘口。
        let mut ct_values = HashMap::new();
        for symbol in &symbols {
            match client.swap_contract_value(symbol) {
                Ok(value) => {
                    ct_values.insert(symbol.clone(), value);
                }
                Err(error) => {
                    tracing::warn!(symbol, %error, "获取 OKX 合约面值失败，该合约暂用 REST 盘口")
                }
            }
        }

        match stream_okx(
            &feed,
            &http,
            &url,
            &symbols,
            &ct_values,
            &books,
            &funding,
        ) {
            Ok(()) => tracing::warn!("OKX WebSocket 已结束，准备重连"),
            Err(error) => {
                let now = Instant::now();
                let should_log = last_error
                    .map(|at| now.duration_since(at) >= Duration::from_secs(60))
                    .unwrap_or(true);
                if should_log {
                    tracing::warn!(%error, "OKX WebSocket 连接失败，稍后重试");
                    last_error = Some(now);
                }
            }
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

fn stream_okx(
    feed: &RealtimeFeed,
    http: &HttpClient,
    url: &str,
    symbols: &[String],
    ct_values: &HashMap<String, Decimal>,
    books: &SymbolRegistry,
    funding: &SymbolRegistry,
) -> anyhow::Result<()> {
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;

    // 盘口只订阅 watchlist（逐币 REST 兜底成本高），资金费率则覆盖全量永续；
    // 持仓交易对（`symbols` 中 watchlist 之外的部分）只订阅永续盘口。
    let watchlist = books.snapshot();
    let mut book_args = Vec::new();
    for symbol in symbols {
        book_args.push(format!(
            r#"{{"channel":"tickers","instId":"{}"}}"#,
            okx_inst_id(symbol, true)
        ));
    }
    for symbol in &watchlist {
        book_args.push(format!(
            r#"{{"channel":"tickers","instId":"{}"}}"#,
            okx_inst_id(symbol, false)
        ));
    }
    for chunk in book_args.chunks(OKX_SUBSCRIBE_CHUNK) {
        ws.send_text(&format!(
            r#"{{"op":"subscribe","args":[{}]}}"#,
            chunk.join(",")
        ))?;
    }
    if !book_args.is_empty() {
        tracing::info!("OKX 盘口订阅成功");
    }

    let mut subscribed: HashSet<String> = HashSet::new();
    let mut book_version = books.version();
    let mut version = u64::MAX;
    let mut last_ping = Instant::now();
    loop {
        // 持仓交易对变化时断开重连，用新的清单重新订阅盘口。
        if needs_reconnect(books, OKX_VENUE, symbols, &mut book_version) {
            return Ok(());
        }
        if funding.version() != version {
            version = funding.version();
            let pending: Vec<String> = funding
                .snapshot()
                .into_iter()
                .filter(|symbol| !subscribed.contains(symbol))
                .collect();
            let args = pending
                .iter()
                .map(|symbol| {
                    format!(
                        r#"{{"channel":"funding-rate","instId":"{}"}}"#,
                        okx_inst_id(symbol, true)
                    )
                })
                .collect::<Vec<_>>();
            for chunk in args.chunks(OKX_SUBSCRIBE_CHUNK) {
                ws.send_text(&format!(
                    r#"{{"op":"subscribe","args":[{}]}}"#,
                    chunk.join(",")
                ))?;
            }
            if !pending.is_empty() {
                tracing::info!(
                    count = pending.len(),
                    total = pending.len() + subscribed.len(),
                    "OKX 资金费率订阅已扩展"
                );
                subscribed.extend(pending);
            }
        }

        if last_ping.elapsed() >= OKX_PING_INTERVAL {
            // OKX 公共频道的心跳是纯文本 `ping`，JSON 形式会被判为非法请求。
            ws.send_text("ping")?;
            last_ping = Instant::now();
        }

        let Some(event) = ws.poll()? else {
            continue;
        };
        let Some(text) = message_text(event, false)? else {
            continue;
        };
        // 心跳回复是纯文本 `pong`，不是 JSON。
        if text == "pong" {
            continue;
        }
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "忽略无法解析的 OKX 消息");
                continue;
            }
        };
        dispatch_okx(feed, &value, ct_values);
    }
}

fn dispatch_okx(feed: &RealtimeFeed, value: &Value, ct_values: &HashMap<String, Decimal>) {
    if let Some(event) = value.get("event").and_then(Value::as_str) {
        match event {
            "error" => {
                let code = value
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let msg = value.get("msg").and_then(Value::as_str).unwrap_or_default();
                tracing::warn!(code, msg, "OKX WebSocket 返回错误");
            }
            "subscribe" | "pong" | "notice" => {}
            other => tracing::debug!(event = other, "忽略 OKX 事件"),
        }
        return;
    }

    let Some(channel) = value.pointer("/arg/channel").and_then(Value::as_str) else {
        return;
    };
    let Some(inst_id) = value.pointer("/arg/instId").and_then(Value::as_str) else {
        return;
    };
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return;
    };
    let Some(item) = data.first() else {
        return;
    };

    match channel {
        "tickers" => {
            let (venue, symbol) = if inst_id.ends_with("-SWAP") {
                (OKX_VENUE, okx_symbol(inst_id))
            } else {
                (OKX_SPOT_VENUE, okx_symbol(inst_id))
            };
            let Some(symbol) = symbol else { return };
            let ct_val = if venue == OKX_VENUE {
                match ct_values.get(&symbol) {
                    Some(value) => *value,
                    None => return,
                }
            } else {
                Decimal::ONE
            };
            if let Ok(snapshot) = parse_okx_book(item, ct_val) {
                feed.store_book(venue, &symbol, snapshot, Utc::now());
            }
        }
        "funding-rate" => {
            let Some(symbol) = okx_symbol(inst_id) else {
                return;
            };
            match parse_okx_funding(item) {
                Ok(snapshot) => feed.store_funding(OKX_VENUE, &symbol, snapshot),
                Err(error) => tracing::debug!(%error, "忽略无法解析的 OKX 资金费率推送"),
            }
        }
        _ => {}
    }
}

/// Binance 组合流消息：`{"stream":"btcusdt@bookTicker","data":{...}}`。
fn parse_binance_book(value: &Value) -> Option<(String, BookSnapshot)> {
    let data = value.get("data").unwrap_or(value);
    let symbol = data
        .get("s")
        .and_then(Value::as_str)
        .map(str::to_ascii_uppercase)
        .or_else(|| {
            value
                .get("stream")
                .and_then(Value::as_str)
                .and_then(|stream| stream.split('@').next())
                .map(str::to_ascii_uppercase)
        })?;

    let bid = data.get("b").and_then(Value::as_str)?;
    let bid_qty = data.get("B").and_then(Value::as_str)?;
    let ask = data.get("a").and_then(Value::as_str)?;
    let ask_qty = data.get("A").and_then(Value::as_str)?;

    let snapshot = make_book(
        parse_decimal(bid).ok()?,
        parse_decimal(bid_qty).ok()?,
        parse_decimal(ask).ok()?,
        parse_decimal(ask_qty).ok()?,
    )
    .ok()?;
    Some((symbol, snapshot))
}

fn parse_okx_book(item: &Value, ct_val: Decimal) -> anyhow::Result<BookSnapshot> {
    let field = |name: &str| -> anyhow::Result<Decimal> {
        item.get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("OKX ticker 缺少字段 {name}"))
            .and_then(parse_decimal)
    };
    make_book(
        field("bidPx")?,
        field("bidSz")? * ct_val,
        field("askPx")?,
        field("askSz")? * ct_val,
    )
}

fn parse_okx_funding(item: &Value) -> anyhow::Result<FundingSnapshot> {
    let rate = item
        .get("fundingRate")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("OKX funding-rate 缺少 fundingRate"))?;
    let funding_time = item
        .get("fundingTime")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("OKX funding-rate 缺少 fundingTime"))?;

    Ok(FundingSnapshot {
        funding_rate: parse_decimal(rate)?,
        mark_price: None,
        next_funding_at: funding_time.parse::<i64>().ok().and_then(millis_to_time),
        observed_at: item
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .and_then(millis_to_time)
            .unwrap_or_else(Utc::now),
    })
}

/// `BTC-USDT-SWAP` / `BTC-USDT` → `BTCUSDT`
fn okx_symbol(inst_id: &str) -> Option<String> {
    let base = inst_id.strip_suffix("-SWAP").unwrap_or(inst_id);
    let base = base.strip_suffix("-USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{}USDT", base.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_binance_combined_book_ticker() {
        let value: Value = serde_json::from_str(
            r#"{"stream":"btcusdt@bookTicker","data":{"u":1,"s":"BTCUSDT","b":"78277.00","B":"2.5","a":"78277.10","A":"3.5"}}"#,
        )
        .unwrap();

        let (symbol, snapshot) = parse_binance_book(&value).expect("应解析成功");
        assert_eq!(symbol, "BTCUSDT");
        assert_eq!(snapshot.bid, dec!(78277.00));
        assert_eq!(snapshot.ask, dec!(78277.10));
        assert_eq!(snapshot.bid_notional_usd, dec!(195692.5));
    }

    #[test]
    fn parses_okx_ticker_with_contract_value() {
        let value: Value = serde_json::from_str(
            r#"{"arg":{"channel":"tickers","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","bidPx":"78270","bidSz":"10","askPx":"78270.1","askSz":"20"}]}"#,
        )
        .unwrap();
        let item = &value["data"][0];

        let snapshot = parse_okx_book(item, dec!(0.01)).expect("应解析成功");
        // 10 张 × 0.01 BTC/张 × 78270
        assert_eq!(snapshot.bid_notional_usd, dec!(7827.0));
        assert_eq!(snapshot.ask_notional_usd, dec!(15654.02));
    }

    #[test]
    fn parses_okx_funding_push() {
        let value: Value = serde_json::from_str(
            r#"{"arg":{"channel":"funding-rate","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","fundingRate":"0.0000765027139019","fundingTime":"1793808000000","ts":"1793795400000"}]}"#,
        )
        .unwrap();

        let snapshot = parse_okx_funding(&value["data"][0]).expect("应解析成功");
        assert_eq!(snapshot.funding_rate, dec!(0.0000765027139019));
        assert_eq!(
            snapshot.next_funding_at.unwrap().timestamp_millis(),
            1793808000000
        );
    }

    #[test]
    fn maps_okx_inst_ids_to_internal_symbols() {
        assert_eq!(okx_symbol("BTC-USDT-SWAP").as_deref(), Some("BTCUSDT"));
        assert_eq!(okx_symbol("BTC-USDT").as_deref(), Some("BTCUSDT"));
        assert_eq!(okx_symbol("BTC-USDC"), None);
        assert_eq!(okx_symbol("-USDT"), None);
    }
}
