//! 实时行情：WebSocket 优先，REST 兜底。
//!
//! - Binance 现货 / USDT 本位永续：`<symbol>@bookTicker` 逐笔最优买卖价；
//! - OKX 现货 / 永续：`tickers` 频道提供最优买卖价，`funding-rate` 频道推送当期费率。
//!
//! 每个交易所一条独立线程，断线后自动重连；行情写入共享表，`MarketFeed` /
//! `FundingFeed` 读取时按新鲜度判断是否回退到 REST。

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::{
    config::Config,
    exchange::{
        make_book, millis_to_time, parse_decimal, BookSnapshot, FundingSnapshot, OkxMarketClient,
        OKX_VENUE, VENUE,
    },
    http_client::HttpClient,
    market::{OKX_SPOT_VENUE, SPOT_VENUE},
    ws::WebSocket,
};

/// 握手超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次读取的阻塞时间，决定心跳检查频率。
const POLL_TIMEOUT: Duration = Duration::from_millis(250);
/// OKX 要求 30 秒内至少发送一次心跳。
const OKX_PING_INTERVAL: Duration = Duration::from_secs(25);
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

/// 资金费率 WebSocket 订阅的交易对清单。
///
/// 交易对由 `FundingFeed` 自动发现后写入；订阅线程按版本号增量补订阅。
#[derive(Default)]
pub struct SymbolRegistry {
    symbols: RwLock<Vec<String>>,
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

    fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> Vec<String> {
        self.symbols.read().expect("订阅清单锁不可用").clone()
    }
}

/// 各交易所推送的最新盘口与当期资金费率。
pub struct RealtimeFeed {
    books: RwLock<HashMap<(String, String), BookUpdate>>,
    funding: RwLock<HashMap<(String, String), FundingUpdate>>,
    funding_symbols: Arc<SymbolRegistry>,
}

impl RealtimeFeed {
    /// 启动各交易所的 WebSocket 订阅线程。
    ///
    /// 未启用 OKX 或未提供客户端时只订阅 Binance。
    pub fn start(
        config: &Config,
        http: Arc<HttpClient>,
        okx: Option<Arc<OkxMarketClient>>,
    ) -> Arc<Self> {
        let feed = Arc::new(Self {
            books: RwLock::new(HashMap::new()),
            funding: RwLock::new(HashMap::new()),
            funding_symbols: Arc::new(SymbolRegistry::new()),
        });
        let symbols = config.market_symbols.clone();

        let binance_spot = feed.clone();
        let spot_http = http.clone();
        let spot_url = config.binance_spot_ws_url.clone();
        let spot_symbols = symbols.clone();
        spawn("binance-spot-ws", move || {
            run_binance_books(binance_spot, spot_http, spot_url, SPOT_VENUE, spot_symbols)
        });

        let binance_futures = feed.clone();
        let futures_http = http.clone();
        let futures_url = config.binance_futures_ws_url.clone();
        let futures_symbols = symbols.clone();
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
            spawn("okx-ws", move || {
                run_okx(okx_feed, http, okx_ws_url, client, symbols, registry)
            });
        }

        feed
    }

    /// 资金费率订阅清单，由 `FundingFeed` 自动发现后写入。
    pub fn funding_symbols(&self) -> Arc<SymbolRegistry> {
        self.funding_symbols.clone()
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

/// Binance 盘口线程：`/stream?streams=` 组合多条 `<symbol>@bookTicker`。
fn run_binance_books(
    feed: Arc<RealtimeFeed>,
    http: Arc<HttpClient>,
    base_url: String,
    venue: &'static str,
    symbols: Vec<String>,
) {
    if symbols.is_empty() {
        return;
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

    tracing::info!(venue, url = %url, "Binance WebSocket 盘口订阅已启动");
    let mut last_error: Option<Instant> = None;
    loop {
        match stream_binance(&feed, &http, &url, venue) {
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
) -> anyhow::Result<()> {
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;
    loop {
        let Some(text) = ws.poll()? else {
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
    symbols: Vec<String>,
    registry: Arc<SymbolRegistry>,
) {
    // 永续盘口数量单位是「张」，先从 REST 拉一次面值，失败则跳过该合约的实时盘口。
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

    tracing::info!(url = %url, symbols = ?symbols, "OKX WebSocket 行情订阅已启动");
    let mut last_error: Option<Instant> = None;
    loop {
        match stream_okx(&feed, &http, &url, &symbols, &ct_values, &registry) {
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
    registry: &SymbolRegistry,
) -> anyhow::Result<()> {
    let mut ws = WebSocket::connect(http, url, HANDSHAKE_TIMEOUT, POLL_TIMEOUT)?;

    // 盘口只订阅 watchlist（逐币 REST 兜底成本高），资金费率则覆盖全量永续。
    let mut book_args = Vec::new();
    for symbol in symbols {
        book_args.push(format!(
            r#"{{"channel":"tickers","instId":"{}"}}"#,
            okx_inst_id(symbol, false)
        ));
        book_args.push(format!(
            r#"{{"channel":"tickers","instId":"{}"}}"#,
            okx_inst_id(symbol, true)
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
    let mut version = u64::MAX;
    let mut last_ping = Instant::now();
    loop {
        if registry.version() != version {
            version = registry.version();
            let pending: Vec<String> = registry
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

        let Some(text) = ws.poll()? else {
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
