//! 链上价格监听：轮询 DEX 池子储备与预言机价格，计算两者偏离。
//!
//! 这里只做「看清价格」，不产生订单。每轮把同一批 `eth_call` 放进一个
//! JSON-RPC 批量请求，减少公共 RPC 的往返次数；池子按 Uniswap V2 的
//! `getReserves()` 读取，预言机按 Chainlink 的 `latestRoundData()` 读取。

use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{
    config::{ChainFeedConfig, Config},
    events::{AlertLevel, ExecutorEvent},
    http_client::HttpClient,
};

/// 批量请求里块号的固定 id，与各池子调用区分开。
const BLOCK_NUMBER_ID: u64 = 9_999;
/// 同一价格源两次告警的最小间隔。
const ALERT_COOLDOWN_SECS: i64 = 300;

const SELECTOR_GET_RESERVES: &str = "0x0902f1ac";
const SELECTOR_LATEST_ROUND_DATA: &str = "0xfeaf968c";

/// 单个价格源最近一次采集结果。
#[derive(Debug, Clone, Serialize)]
pub struct ChainPrice {
    pub id: String,
    pub symbol: String,
    pub pool: String,
    /// 池子现货价（计价币 / 基准币），如 USDT per ETH。
    pub pool_price: Option<Decimal>,
    /// 预言机价格，口径与 `pool_price` 一致。
    pub oracle_price: Option<Decimal>,
    pub oracle_label: Option<String>,
    pub oracle_updated_at: Option<DateTime<Utc>>,
    /// `pool_price` 相对 `oracle_price` 的偏离，正数表示池子价更高。
    pub deviation_bps: Option<i64>,
    pub block_number: Option<u64>,
    pub observed_at: DateTime<Utc>,
    pub error: Option<String>,
}

/// 链上价格接口的响应。
#[derive(Debug, Clone, Serialize)]
pub struct ChainResponse {
    pub enabled: bool,
    /// 只暴露是否配置了 RPC，不回传地址（可能含 API Key）。
    pub rpc_configured: bool,
    pub poll_secs: u64,
    pub alert_threshold_bps: u32,
    pub updated_at: Option<DateTime<Utc>>,
    pub feeds: Vec<ChainPrice>,
}

struct FeedState {
    config: ChainFeedConfig,
    last: ChainPrice,
}

/// 链上价格监听器：定时批量查询，快照保存在内存中供接口读取。
pub struct ChainFeed {
    http: Option<Arc<HttpClient>>,
    rpc_url: Option<String>,
    enabled: bool,
    poll: Duration,
    alert_bps: u32,
    events: broadcast::Sender<ExecutorEvent>,
    feeds: RwLock<Vec<FeedState>>,
    updated_at: RwLock<Option<DateTime<Utc>>>,
    /// 价格源 id → 上次告警时间。
    alerts: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl ChainFeed {
    pub fn start(
        config: &Config,
        http: Option<Arc<HttpClient>>,
        events: broadcast::Sender<ExecutorEvent>,
    ) -> Arc<Self> {
        let feeds = config
            .chain_feeds
            .iter()
            .cloned()
            .map(|item| FeedState {
                last: ChainPrice::pending(&item),
                config: item,
            })
            .collect();
        let feed = Arc::new(Self {
            http,
            rpc_url: config.chain_rpc_url.clone(),
            enabled: config.chain_enabled,
            poll: Duration::from_secs(config.chain_poll_secs.max(1)),
            alert_bps: config.chain_alert_bps,
            events,
            feeds: RwLock::new(feeds),
            updated_at: RwLock::new(None),
            alerts: Mutex::new(HashMap::new()),
        });

        if !feed.enabled {
            return feed;
        }
        if feed.rpc_url.is_none() {
            tracing::info!("未配置 ARBPILOT_CHAIN_RPC_URL，链上价格监听保持空闲");
            return feed;
        }
        if feed.http.is_none() {
            tracing::warn!("缺少共享 HTTP 客户端，链上价格监听不可用");
            return feed;
        }

        let worker = feed.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(worker.poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                worker.sync_once().await;
            }
        });
        feed
    }

    /// 按需查看最近一次快照，不触发新请求。
    pub fn snapshot(&self) -> ChainResponse {
        ChainResponse {
            enabled: self.enabled && self.rpc_url.is_some(),
            rpc_configured: self.rpc_url.is_some(),
            poll_secs: self.poll.as_secs(),
            alert_threshold_bps: self.alert_bps,
            updated_at: *self.updated_at.read().expect("链上时间锁不可用"),
            feeds: self
                .feeds
                .read()
                .expect("链上价格锁不可用")
                .iter()
                .map(|item| item.last.clone())
                .collect(),
        }
    }

    async fn sync_once(&self) {
        let (Some(rpc_url), Some(http)) = (self.rpc_url.clone(), self.http.clone()) else {
            return;
        };
        let targets: Vec<ChainFeedConfig> = self
            .feeds
            .read()
            .expect("链上价格锁不可用")
            .iter()
            .map(|item| item.config.clone())
            .collect();
        let payload = build_batch(&targets);

        let result = tokio::task::spawn_blocking(move || http.post_json(&rpc_url, &payload)).await;

        match result {
            Ok(Ok(payload)) => self.apply_response(&targets, payload),
            Ok(Err(error)) => self.mark_error(&error.to_string()),
            Err(error) => self.mark_error(&error.to_string()),
        }
    }

    fn apply_response(&self, targets: &[ChainFeedConfig], payload: Value) {
        let (responses, rpc_error) = parse_batch(payload);
        let block_number = responses
            .get(&BLOCK_NUMBER_ID)
            .and_then(Value::as_str)
            .and_then(parse_hex_u64);
        let now = Utc::now();

        let mut feeds = self.feeds.write().expect("链上价格锁不可用");
        for (index, target) in targets.iter().enumerate() {
            let reserves = responses.get(&(index as u64 * 2));
            let oracle = responses.get(&(index as u64 * 2 + 1));
            let next = evaluate(
                target,
                reserves,
                oracle,
                block_number,
                rpc_error.as_deref(),
                now,
            );
            self.maybe_alert(&next);
            if let Some(state) = feeds.iter_mut().find(|item| item.config.id == target.id) {
                state.last = next;
            }
        }
        drop(feeds);
        *self.updated_at.write().expect("链上时间锁不可用") = Some(now);
    }

    fn mark_error(&self, error: &str) {
        let now = Utc::now();
        tracing::warn!(error, "链上价格采集失败");
        let mut feeds = self.feeds.write().expect("链上价格锁不可用");
        for state in feeds.iter_mut() {
            state.last = ChainPrice::failed(&state.config, error, now);
        }
        *self.updated_at.write().expect("链上时间锁不可用") = Some(now);
    }

    /// 偏离超过阈值时推送一次告警；同一价格源在冷却时间内不重复告警。
    fn maybe_alert(&self, price: &ChainPrice) {
        let Some(deviation) = price.deviation_bps else {
            return;
        };
        if deviation.unsigned_abs() < self.alert_bps as u64 {
            return;
        }
        let now = Utc::now();
        {
            let mut alerts = self.alerts.lock().expect("链上告警冷却锁不可用");
            if let Some(last) = alerts.get(&price.id) {
                if now.signed_duration_since(*last) < chrono::Duration::seconds(ALERT_COOLDOWN_SECS)
                {
                    return;
                }
            }
            alerts.insert(price.id.clone(), now);
        }

        let pool = price
            .pool_price
            .map(|value| value.round_dp(4).to_string())
            .unwrap_or_else(|| "-".to_string());
        let oracle = price
            .oracle_price
            .map(|value| value.round_dp(4).to_string())
            .unwrap_or_else(|| "-".to_string());
        let message = format!(
            "链上价格偏离 · {} 池子 {pool} / 预言机 {oracle}，偏离 {deviation:+} bps",
            price.symbol
        );
        let message_en = format!(
            "Onchain price deviation · {} pool {pool} / oracle {oracle}, deviation {deviation:+} bps",
            price.symbol
        );
        tracing::warn!(
            feed = %price.id,
            deviation,
            pool,
            oracle,
            "链上价格偏离超过阈值"
        );
        let _ = self.events.send(ExecutorEvent::Alert {
            level: AlertLevel::Warning,
            message,
            message_en: Some(message_en),
            at: now,
        });
    }
}

impl ChainPrice {
    fn pending(config: &ChainFeedConfig) -> Self {
        Self {
            id: config.id.clone(),
            symbol: config.symbol.clone(),
            pool: config.pool.clone(),
            pool_price: None,
            oracle_price: None,
            oracle_label: config.oracle_label.clone(),
            oracle_updated_at: None,
            deviation_bps: None,
            block_number: None,
            observed_at: Utc::now(),
            error: Some("等待首次采集".to_string()),
        }
    }

    fn failed(config: &ChainFeedConfig, error: &str, at: DateTime<Utc>) -> Self {
        let mut price = Self::pending(config);
        price.observed_at = at;
        price.error = Some(error.to_string());
        price
    }
}

/// 把块号与每个价格源的池子/预言机调用合成一个 JSON-RPC 批量请求。
fn build_batch(targets: &[ChainFeedConfig]) -> Value {
    let mut calls = Vec::with_capacity(targets.len() * 2 + 1);
    calls.push(json!({
        "jsonrpc": "2.0",
        "id": BLOCK_NUMBER_ID,
        "method": "eth_blockNumber",
        "params": [],
    }));
    for (index, target) in targets.iter().enumerate() {
        let base = index as u64 * 2;
        calls.push(json!({
            "jsonrpc": "2.0",
            "id": base,
            "method": "eth_call",
            "params": [{ "to": target.pool, "data": SELECTOR_GET_RESERVES }, "latest"],
        }));
        if let Some(oracle) = target.oracle.as_deref() {
            calls.push(json!({
                "jsonrpc": "2.0",
                "id": base + 1,
                "method": "eth_call",
                "params": [{ "to": oracle, "data": SELECTOR_LATEST_ROUND_DATA }, "latest"],
            }));
        }
    }
    Value::Array(calls)
}

/// 批量响应按 id 归位；任一子请求报错时记录错误信息。
fn parse_batch(payload: Value) -> (HashMap<u64, Value>, Option<String>) {
    let mut responses = HashMap::new();
    let mut error = None;
    let items = match payload {
        Value::Array(items) => items,
        other => {
            let message = rpc_error_message(&other);
            return (responses, Some(message));
        }
    };
    for item in items {
        if let Some(message) = item.get("error") {
            error = Some(rpc_error_message(message));
            continue;
        }
        let (Some(id), Some(result)) = (item.get("id").and_then(Value::as_u64), item.get("result"))
        else {
            continue;
        };
        responses.insert(id, result.clone());
    }
    (responses, error)
}

fn rpc_error_message(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "RPC 返回错误".to_string())
}

fn evaluate(
    target: &ChainFeedConfig,
    reserves: Option<&Value>,
    oracle: Option<&Value>,
    block_number: Option<u64>,
    rpc_error: Option<&str>,
    now: DateTime<Utc>,
) -> ChainPrice {
    let mut price = ChainPrice::pending(target);
    price.observed_at = now;
    price.block_number = block_number;
    price.error = rpc_error.map(str::to_string);

    match reserves.and_then(Value::as_str) {
        Some(raw) => match decode_reserves(raw) {
            Some((reserve0, reserve1)) => match pool_price(target, reserve0, reserve1) {
                Some(value) => price.pool_price = Some(value),
                None => price.error = Some("池子储备为 0，无法计算价格".into()),
            },
            None => price.error = Some("池子返回值无法解析".into()),
        },
        None => {
            if price.error.is_none() {
                price.error = Some("池子调用无返回".into());
            }
        }
    }

    if let Some(oracle) = oracle.and_then(Value::as_str) {
        if let Some((answer, updated_at)) = decode_latest_round(oracle, target.oracle_decimals) {
            price.oracle_price = Some(answer);
            price.oracle_updated_at = updated_at;
        }
    }

    if let (Some(pool), Some(oracle)) = (price.pool_price, price.oracle_price) {
        price.deviation_bps = deviation_bps(pool, oracle);
    }

    if target.oracle.is_none() {
        price.oracle_label = None;
    }

    price
}

/// Uniswap V2 `getReserves()` 返回 `(uint112, uint112, uint32)`。
fn decode_reserves(raw: &str) -> Option<(u128, u128)> {
    let words = decode_words(raw)?;
    if words.len() < 2 {
        return None;
    }
    Some((words[0], words[1]))
}

/// Chainlink `latestRoundData()` 返回 `(uint80, int256, uint256, uint256, uint80)`。
fn decode_latest_round(raw: &str, decimals: u32) -> Option<(Decimal, Option<DateTime<Utc>>)> {
    let words = decode_words(raw)?;
    if words.len() < 4 || words[1] == 0 {
        return None;
    }
    let answer = scaled(words[1], decimals);
    let updated_at = if words[3] == 0 {
        None
    } else {
        DateTime::from_timestamp(words[3] as i64, 0)
    };
    Some((answer, updated_at))
}

/// ABI 返回值按 32 字节一个字切开；这里只取每个字低 16 字节（足够覆盖 uint112/int256 正数）。
fn decode_words(raw: &str) -> Option<Vec<u128>> {
    let hex = raw.strip_prefix("0x").unwrap_or(raw);
    if hex.is_empty() || hex.len() % 64 != 0 {
        return None;
    }
    hex.as_bytes()
        .chunks(64)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk).ok()?;
            u128::from_str_radix(&text[32..], 16).ok()
        })
        .collect()
}

fn pool_price(target: &ChainFeedConfig, reserve0: u128, reserve1: u128) -> Option<Decimal> {
    let (base, quote) = if target.base_is_token0 {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };
    if base == 0 || quote == 0 {
        return None;
    }
    let base_amount = scaled(base, target.base_decimals);
    let quote_amount = scaled(quote, target.quote_decimals);
    quote_amount.checked_div(base_amount)
}

/// `(pool - oracle) / oracle`，单位 bps。
fn deviation_bps(pool: Decimal, oracle: Decimal) -> Option<i64> {
    if oracle <= Decimal::ZERO {
        return None;
    }
    let ratio = (pool - oracle).checked_div(oracle)?;
    (ratio * Decimal::from(10_000)).round().to_i64()
}

fn scaled(value: u128, decimals: u32) -> Decimal {
    let amount = Decimal::from_str(&value.to_string()).unwrap_or(Decimal::ZERO);
    if decimals == 0 {
        return amount;
    }
    let unit =
        Decimal::from_str(&format!("1{}", "0".repeat(decimals as usize))).unwrap_or(Decimal::ONE);
    amount / unit
}

fn parse_hex_u64(raw: &str) -> Option<u64> {
    u64::from_str_radix(raw.strip_prefix("0x").unwrap_or(raw), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed() -> ChainFeedConfig {
        ChainFeedConfig {
            id: "eth-usdt".into(),
            symbol: "ETH/USDT".into(),
            pool: "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852".into(),
            base_is_token0: true,
            base_decimals: 18,
            quote_decimals: 6,
            oracle: Some("0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".into()),
            oracle_decimals: 8,
            oracle_label: Some("chainlink-eth-usd".into()),
        }
    }

    /// 把 u128 编码成 32 字节字，便于构造 ABI 返回值。
    fn word(value: u128) -> String {
        format!("{value:064x}")
    }

    #[test]
    fn builds_batch_with_block_number_and_calls() {
        let payload = build_batch(&[feed()]);
        let items = payload.as_array().expect("批量请求应为数组");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["method"], "eth_blockNumber");
        assert_eq!(items[1]["params"][0]["data"], SELECTOR_GET_RESERVES);
        assert_eq!(items[2]["params"][0]["data"], SELECTOR_LATEST_ROUND_DATA);
    }

    #[test]
    fn decodes_uniswap_reserves_and_price() {
        // 2000 ETH / 4,560,000 USDT → 2280 USDT per ETH
        let raw = format!(
            "0x{}{}{}",
            word(2_000 * 10u128.pow(18)),
            word(4_560_000 * 10u128.pow(6)),
            word(1_700_000_000)
        );
        let (reserve0, reserve1) = decode_reserves(&raw).expect("应能解码储备");
        let price = pool_price(&feed(), reserve0, reserve1).expect("应能算出价格");
        assert_eq!(price, Decimal::from(2280));
    }

    #[test]
    fn respects_token_order() {
        let mut target = feed();
        target.base_is_token0 = false;
        let price = pool_price(&target, 4_560_000 * 10u128.pow(6), 2_000 * 10u128.pow(18))
            .expect("应能算出价格");
        assert_eq!(price, Decimal::from(2280));
    }

    #[test]
    fn decodes_chainlink_round() {
        let raw = format!(
            "0x{}{}{}{}{}",
            word(1_100_000_000_000_000_000_000),
            word(228_000_000_000),
            word(1_700_000_000),
            word(1_700_000_060),
            word(1_100_000_000_000_000_000_000)
        );
        let (answer, updated_at) = decode_latest_round(&raw, 8).expect("应能解码预言机");
        assert_eq!(answer, Decimal::from(2280));
        assert_eq!(updated_at, DateTime::from_timestamp(1_700_000_060, 0));
    }

    #[test]
    fn computes_deviation_in_bps() {
        let score = deviation_bps(Decimal::from(2650), Decimal::from(2280)).expect("应能算出偏离");
        assert_eq!(score, 1623);
        let negative =
            deviation_bps(Decimal::from(2150), Decimal::from(2280)).expect("应能算出偏离");
        assert_eq!(negative, -570);
    }

    #[test]
    fn marks_feed_error_when_rpc_fails() {
        let price = evaluate(&feed(), None, None, None, Some("rate limited"), Utc::now());
        assert!(price.pool_price.is_none());
        assert_eq!(price.error.as_deref(), Some("rate limited"));
    }

    #[test]
    fn rejects_unparseable_reserves() {
        let price = evaluate(
            &feed(),
            Some(&Value::String("0x1234".into())),
            None,
            Some(1),
            None,
            Utc::now(),
        );
        assert!(price.pool_price.is_none());
        assert_eq!(price.error.as_deref(), Some("池子返回值无法解析"));
    }

    #[test]
    fn rejects_empty_pool() {
        let raw = format!("0x{}{}{}", word(0), word(0), word(0));
        let price = evaluate(
            &feed(),
            Some(&Value::String(raw)),
            None,
            Some(1),
            None,
            Utc::now(),
        );
        assert!(price.pool_price.is_none());
        assert!(price.error.is_some());
    }
}
