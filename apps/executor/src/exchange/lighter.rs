use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    make_book, make_depth, parse_decimal, parse_json_decimal, BookSnapshot, Candle, DepthSnapshot,
    FundingRate, FundingSnapshot, FundingVenueClient, MinOrderSpec,
};
use crate::http_client::HttpClient;

/// Lighter 的交易所标识。
pub const VENUE: &str = "lighter";

/// 交易对与 `market_id` 映射的缓存时长，避免每个交易对都重复拉一次全量清单。
const MARKET_TTL: Duration = Duration::from_secs(10 * 60);
/// 历史费率一次最多回补多少条（Lighter 按小时结算）。
const HISTORY_LIMIT: u32 = 100;
/// 两次请求之间的最小间隔，避免触发 Lighter 的限频。
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(150);
/// `/api/v1/funding-rates` 的 `rate` 是 8 小时口径，而 Lighter 按小时结算。
const SNAPSHOT_PERIOD_HOURS: u32 = 8;
/// `/api/v1/fundings` 的 `rate` 以百分比表示，入库统一折算为小数。
const PERCENT_SCALE: u32 = 100;

/// Lighter 的币名（如 `DOGE`）补齐内部统一后缀并转大写。
fn internal_symbol(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    if upper.ends_with("USDT") || upper.ends_with("USDC") {
        Some(upper)
    } else {
        Some(format!("{upper}USDT"))
    }
}

/// Lighter 永续按小时结算，取下个小时整点作为下次结算时间。
fn next_hour(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt((now.timestamp() / 3600 + 1) * 3600, 0)
        .single()
}

/// 从 `orderBookOrders` 解析买一/卖一；订单簿按价格优先排序，首条即最优。
fn book_from_orders(value: &Value, market_id: i64) -> anyhow::Result<BookSnapshot> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 200 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        bail!("Lighter orderBookOrders 返回错误 {code}: {message}");
    }
    let level = |key: &str, label: &str| -> anyhow::Result<(Decimal, Decimal)> {
        let order = value
            .get(key)
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .ok_or_else(|| anyhow!("Lighter 盘口缺少{label}: market_id={market_id}"))?;
        let price = order
            .get("price")
            .ok_or_else(|| anyhow!("Lighter 盘口{label}缺少 price"))?;
        let size = order
            .get("remaining_base_amount")
            .ok_or_else(|| anyhow!("Lighter 盘口{label}缺少 remaining_base_amount"))?;
        Ok((parse_json_decimal(price)?, parse_json_decimal(size)?))
    };
    let (bid, bid_size) = level("bids", "买一")?;
    let (ask, ask_size) = level("asks", "卖一")?;
    make_book(bid, bid_size, ask, ask_size)
}

#[derive(Debug, Deserialize)]
struct BooksEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    order_book_details: Vec<MarketRow>,
}

#[derive(Debug, Deserialize)]
struct MarketRow {
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    market_id: i64,
    #[serde(default)]
    market_type: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    #[serde(default)]
    index_price: Option<String>,
    /// 最小下单数量（基础币）。
    #[serde(default)]
    min_base_amount: Option<String>,
    /// 最小下单名义额（USDT）。
    #[serde(default)]
    min_quote_amount: Option<String>,
    /// 数量精度；下单数量必须是 1e-`size_decimals` 的整数倍。
    #[serde(default)]
    size_decimals: Option<u32>,
    /// 价格精度；下单价格按 1e-`price_decimals` 取整。
    #[serde(default)]
    price_decimals: Option<u32>,
}

impl MarketRow {
    fn is_perp(&self) -> bool {
        self.market_type
            .as_deref()
            .map(|kind| kind.eq_ignore_ascii_case("perp"))
            .unwrap_or(false)
    }

    fn is_tradable(&self) -> bool {
        self.status
            .as_deref()
            .map(|status| status.eq_ignore_ascii_case("active"))
            .unwrap_or(true)
    }

    fn price(&self) -> Option<Decimal> {
        self.mark_price
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .or_else(|| {
                self.index_price
                    .as_deref()
                    .and_then(|value| parse_decimal(value).ok())
            })
    }
}

#[derive(Debug, Deserialize)]
struct RatesEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    funding_rates: Vec<RateRow>,
}

#[derive(Debug, Deserialize)]
struct RateRow {
    #[serde(default)]
    market_id: i64,
    #[serde(default)]
    exchange: String,
    /// 可能是整数、浮点或字符串，统一用 `Value` 接收。
    #[serde(default)]
    rate: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct FundingsEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    fundings: Vec<HistoryRow>,
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    /// 结算时间（Unix 秒）。
    #[serde(default)]
    timestamp: i64,
    /// 单期结算法率，单位为百分比。
    #[serde(default)]
    rate: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CandlesEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    c: Vec<CandleRow>,
}

#[derive(Debug, Deserialize)]
struct CandleRow {
    /// 开盘时间（毫秒）。
    #[serde(default)]
    t: i64,
    /// 价格与成交量在响应里是数字，且为 0 时会被省略。
    #[serde(default)]
    o: Option<Value>,
    #[serde(default)]
    h: Option<Value>,
    #[serde(default)]
    l: Option<Value>,
    #[serde(default)]
    c: Option<Value>,
    #[serde(default)]
    v: Option<Value>,
}

/// Lighter 支持的 K 线周期（没有 10m 与 1w，10m 由上层用 5m 聚合）。
fn lighter_resolution(interval: &str) -> anyhow::Result<(&'static str, i64)> {
    Ok(match interval {
        "1m" => ("1m", 60),
        "5m" => ("5m", 300),
        "15m" => ("15m", 900),
        "30m" => ("30m", 1_800),
        "1h" => ("1h", 3_600),
        "4h" => ("4h", 14_400),
        "1d" => ("1d", 86_400),
        other => bail!("Lighter 不支持的 K 线周期: {other}"),
    })
}

/// Lighter 永续公开资金费率客户端。
pub struct LighterFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
    /// `symbol → market_id` 缓存；历史接口只认 `market_id`。
    markets: Mutex<Option<(Instant, Arc<HashMap<String, i64>>)>>,
    /// 上次请求时间，用于进程内节流。
    gate: Mutex<Option<Instant>>,
}

impl LighterFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            markets: Mutex::new(None),
            gate: Mutex::new(None),
        }
    }

    /// Lighter 的公共接口限流较严，这里按最小间隔串行化请求。
    fn fetch(&self, url: &str) -> anyhow::Result<Value> {
        {
            let mut last = self.gate.lock().expect("Lighter 限流锁不可用");
            if let Some(previous) = *last {
                let elapsed = previous.elapsed();
                if elapsed < MIN_REQUEST_INTERVAL {
                    std::thread::sleep(MIN_REQUEST_INTERVAL - elapsed);
                }
            }
            *last = Some(Instant::now());
        }
        self.http.get_json(url)
    }

    fn books(&self) -> anyhow::Result<Vec<MarketRow>> {
        let url = format!("{}/api/v1/orderBookDetails", self.base_url);
        let envelope: BooksEnvelope = serde_json::from_value(self.fetch(&url)?)
            .context("解析 Lighter orderBookDetails 失败")?;
        if envelope.code != 200 {
            bail!(
                "Lighter orderBookDetails 返回错误 {}: {}",
                envelope.code,
                envelope.message.unwrap_or_default()
            );
        }
        Ok(envelope.order_book_details)
    }

    fn remember(&self, books: &[MarketRow]) -> Arc<HashMap<String, i64>> {
        let mut mapping = HashMap::new();
        for row in books {
            if !row.is_perp() || !row.is_tradable() {
                continue;
            }
            let Some(symbol) = internal_symbol(&row.symbol) else {
                continue;
            };
            mapping.insert(symbol, row.market_id);
        }
        let mapping = Arc::new(mapping);
        *self.markets.lock().expect("Lighter 映射锁不可用") =
            Some((Instant::now(), mapping.clone()));
        mapping
    }

    fn market_ids(&self, force: bool) -> anyhow::Result<Arc<HashMap<String, i64>>> {
        if !force {
            let cached = self.markets.lock().expect("Lighter 映射锁不可用");
            if let Some((at, mapping)) = cached.as_ref() {
                if at.elapsed() < MARKET_TTL {
                    return Ok(mapping.clone());
                }
            }
        }
        Ok(self.remember(&self.books()?))
    }

    /// `symbol → market_id` 映射，供 WebSocket 盘口订阅使用。
    pub fn symbol_market_ids(&self) -> anyhow::Result<HashMap<String, i64>> {
        Ok((*self.market_ids(false)?).clone())
    }

    /// 永续的买一/卖一档；`remaining_base_amount` 单位是币。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let mut market_id = self.market_ids(false)?.get(symbol).copied();
        if market_id.is_none() {
            market_id = self.market_ids(true)?.get(symbol).copied();
        }
        let market_id = market_id.ok_or_else(|| anyhow!("Lighter 未找到交易对: {symbol}"))?;
        let url = format!(
            "{}/api/v1/orderBookOrders?market_id={market_id}&limit=1",
            self.base_url
        );
        book_from_orders(&self.fetch(&url)?, market_id)
    }

    /// 永续的多档盘口；订单簿按价格聚合成档位，数量单位是币。
    pub fn depth(&self, symbol: &str, limit: u32) -> anyhow::Result<DepthSnapshot> {
        let mut market_id = self.market_ids(false)?.get(symbol).copied();
        if market_id.is_none() {
            market_id = self.market_ids(true)?.get(symbol).copied();
        }
        let market_id = market_id.ok_or_else(|| anyhow!("Lighter 未找到交易对: {symbol}"))?;
        let cap = limit.clamp(1, 100);
        let url = format!(
            "{}/api/v1/orderBookOrders?market_id={market_id}&limit={cap}",
            self.base_url
        );
        let value = self.fetch(&url)?;
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(200);
        if code != 200 {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bail!("Lighter orderBookOrders 返回错误 {code}: {message}");
        }
        let side = |key: &str| -> anyhow::Result<Vec<(Decimal, Decimal)>> {
            let rows = value
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("Lighter 盘口缺少 {key}: market_id={market_id}"))?;
            let mut merged: Vec<(Decimal, Decimal)> = Vec::new();
            for row in rows {
                let price = row
                    .get("price")
                    .ok_or_else(|| anyhow!("Lighter 盘口缺少 price"))?;
                let size = row
                    .get("remaining_base_amount")
                    .ok_or_else(|| anyhow!("Lighter 盘口缺少 remaining_base_amount"))?;
                let price = parse_json_decimal(price)?;
                let size = parse_json_decimal(size)?;
                match merged.iter_mut().find(|(level, _)| *level == price) {
                    Some(existing) => existing.1 += size,
                    None => merged.push((price, size)),
                }
            }
            Ok(merged)
        };
        let mut bids = side("bids")?;
        let mut asks = side("asks")?;
        bids.sort_by(|a, b| b.0.cmp(&a.0));
        asks.sort_by(|a, b| a.0.cmp(&b.0));
        bids.truncate(cap as usize);
        asks.truncate(cap as usize);
        make_depth(VENUE, symbol, bids, asks, Decimal::ONE)
    }

    /// 单个永续的最小下单数量与最小名义额。
    pub fn min_order(&self, symbol: &str) -> anyhow::Result<Option<MinOrderSpec>> {
        let Some(row) = self.books()?.into_iter().find(|row| {
            internal_symbol(&row.symbol)
                .map(|value| value.eq_ignore_ascii_case(symbol))
                .unwrap_or(false)
        }) else {
            return Ok(None);
        };
        let mut spec = MinOrderSpec::new(VENUE, symbol);
        spec.min_quantity = row
            .min_base_amount
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO);
        spec.min_notional_usd = row
            .min_quote_amount
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())
            .filter(|value| *value > Decimal::ZERO);
        // 签名时数量与价格都会按精度取整，提前按同一精度对齐，避免取整后名义额
        // 掉到最小下单额以下（如 BTC 5 位精度、10 USDC 门槛）。
        spec.step_size = row
            .size_decimals
            .filter(|decimals| *decimals < 28)
            .map(|decimals| Decimal::new(1, decimals));
        spec.tick_size = row
            .price_decimals
            .filter(|decimals| *decimals < 28)
            .map(|decimals| Decimal::new(1, decimals));
        Ok(Some(spec))
    }

    /// 永续 K 线；`/api/v1/candles` 单次最多返回 500 根。
    pub fn candles(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let (resolution, step) = lighter_resolution(interval)?;
        let mut market_id = self.market_ids(false)?.get(symbol).copied();
        if market_id.is_none() {
            market_id = self.market_ids(true)?.get(symbol).copied();
        }
        let market_id = market_id.ok_or_else(|| anyhow!("Lighter 未找到交易对: {symbol}"))?;

        let limit = limit.clamp(1, 500);
        let end = Utc::now();
        let start = end - chrono::Duration::seconds(step * i64::from(limit));
        let url = format!(
            "{}/api/v1/candles?market_id={market_id}&resolution={resolution}&start_timestamp={}&end_timestamp={}&count_back={limit}",
            self.base_url,
            start.timestamp_millis(),
            end.timestamp_millis()
        );
        let envelope: CandlesEnvelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 Lighter candles 失败")?;
        if envelope.code != 200 {
            bail!(
                "Lighter candles 返回错误 {}: {}",
                envelope.code,
                envelope.message.unwrap_or_default()
            );
        }

        let mut candles = Vec::with_capacity(envelope.c.len());
        for row in envelope.c {
            let Some(open_time) = Utc.timestamp_millis_opt(row.t).single() else {
                continue;
            };
            // 价格为 0 的字段会被省略，这里统一按 0 处理。
            let parse = |value: &Option<Value>| -> anyhow::Result<Decimal> {
                value
                    .as_ref()
                    .map(parse_json_decimal)
                    .transpose()
                    .map(Option::unwrap_or_default)
            };
            candles.push(Candle {
                open_time,
                open: parse(&row.o)?,
                high: parse(&row.h)?,
                low: parse(&row.l)?,
                close: parse(&row.c)?,
                volume: parse(&row.v)?,
            });
        }
        candles.sort_by_key(|candle| candle.open_time);
        if candles.len() > limit as usize {
            candles.drain(..candles.len() - limit as usize);
        }
        Ok(candles)
    }

    fn current_rates(&self) -> anyhow::Result<HashMap<i64, Decimal>> {
        let url = format!("{}/api/v1/funding-rates", self.base_url);
        let envelope: RatesEnvelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 Lighter funding-rates 失败")?;
        if envelope.code != 200 {
            bail!(
                "Lighter funding-rates 返回错误 {}: {}",
                envelope.code,
                envelope.message.unwrap_or_default()
            );
        }
        let period = Decimal::from(SNAPSHOT_PERIOD_HOURS);
        let mut rates = HashMap::new();
        for row in envelope.funding_rates {
            if !row.exchange.eq_ignore_ascii_case("lighter") {
                continue;
            }
            if let Some(rate) = row
                .rate
                .as_ref()
                .and_then(|value| parse_json_decimal(value).ok())
            {
                rates.insert(row.market_id, rate / period);
            }
        }
        Ok(rates)
    }

    /// 一次请求返回全部永续的当期费率、标记价与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let books = self.books()?;
        self.remember(&books);
        let rates = self.current_rates()?;

        let now = Utc::now();
        let next_funding_at = next_hour(now);
        let mut snapshots = Vec::new();
        for row in books {
            if !row.is_perp() || !row.is_tradable() {
                continue;
            }
            let Some(symbol) = internal_symbol(&row.symbol) else {
                continue;
            };
            let Some(funding_rate) = rates.get(&row.market_id).copied() else {
                continue;
            };
            snapshots.push((
                symbol,
                FundingSnapshot {
                    funding_rate,
                    mark_price: row.price(),
                    next_funding_at,
                    observed_at: now,
                },
            ));
        }
        Ok(snapshots)
    }

    /// 当前正在交易的永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols: Vec<String> = self
            .books()?
            .into_iter()
            .filter(|row| row.is_perp() && row.is_tradable())
            .filter_map(|row| internal_symbol(&row.symbol))
            .collect();
        symbols.sort();
        symbols.dedup();
        Ok(symbols)
    }

    /// 历史已结算费率，按结算时间升序返回；`start` 为已入库的最后一条。
    pub fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let mut market_id = self.market_ids(false)?.get(symbol).copied();
        if market_id.is_none() {
            market_id = self.market_ids(true)?.get(symbol).copied();
        }
        let market_id = market_id.ok_or_else(|| anyhow!("Lighter 未找到交易对: {symbol}"))?;

        let now = Utc::now();
        let start = start
            .map(|time| time.timestamp())
            .unwrap_or_else(|| now.timestamp() - 30 * 24 * 3600);
        let url = format!(
            "{}/api/v1/fundings?market_id={market_id}&resolution=1h&start_timestamp={start}&end_timestamp={}&count_back={HISTORY_LIMIT}",
            self.base_url,
            now.timestamp()
        );
        let envelope: FundingsEnvelope =
            serde_json::from_value(self.fetch(&url)?).context("解析 Lighter fundings 失败")?;
        if envelope.code != 200 {
            bail!(
                "Lighter fundings 返回错误 {}: {}",
                envelope.code,
                envelope.message.unwrap_or_default()
            );
        }

        let percent = Decimal::from(PERCENT_SCALE);
        let mut rates = Vec::with_capacity(envelope.fundings.len());
        for row in envelope.fundings {
            let Some(funding_time) = Utc.timestamp_opt(row.timestamp, 0).single() else {
                continue;
            };
            if funding_time.timestamp() <= start {
                continue;
            }
            // `rate` 是百分比（如 0.0012 表示 0.0012%），统一换算成小数比例入库。
            let Some(rate) = row
                .rate
                .as_ref()
                .and_then(|value| parse_json_decimal(value).ok())
            else {
                continue;
            };
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: rate / percent,
                mark_price: None,
            });
        }
        rates.sort_by_key(|row| row.funding_time);
        rates.dedup_by_key(|row| row.funding_time);
        Ok(rates)
    }
}

impl FundingVenueClient for LighterFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let mut rows = self.premium_index_all()?;
        rows.iter()
            .position(|(item, _)| item == symbol)
            .map(|index| rows.swap_remove(index).1)
            .ok_or_else(|| anyhow!("Lighter 未找到交易对: {symbol}"))
    }

    fn funding_history(
        &self,
        symbol: &str,
        start: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        Self::funding_history(self, symbol, start)
    }

    fn listed_symbols(&self) -> anyhow::Result<Vec<String>> {
        self.perpetual_symbols()
    }

    /// Lighter 的永续合约每小时结算一次。
    fn funding_interval_secs(&self) -> Option<i64> {
        Some(3600)
    }

    fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        Self::premium_index_all(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_market_names() {
        assert_eq!(internal_symbol("DOGE").as_deref(), Some("DOGEUSDT"));
        assert_eq!(internal_symbol("btc").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("ETHUSDC").as_deref(), Some("ETHUSDC"));
        assert_eq!(internal_symbol("  "), None);
    }

    #[test]
    fn rounds_up_to_next_hour() {
        let now = Utc.timestamp_opt(1_700_000_123, 0).unwrap();
        assert_eq!(next_hour(now).unwrap().timestamp(), 1_700_002_800);
    }

    #[test]
    fn parses_history_rows() {
        let value = serde_json::json!({
            "code": 200,
            "fundings": [
                { "timestamp": 1_793_700_000, "value": "0.00000096", "rate": "0.0013" },
                { "timestamp": 1_793_703_600, "rate": "0.0002" }
            ]
        });
        let envelope: FundingsEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(envelope.fundings.len(), 2);
        assert_eq!(
            parse_json_decimal(envelope.fundings[0].rate.as_ref().unwrap()).unwrap(),
            rust_decimal_macros::dec!(0.0013)
        );
    }

    #[test]
    fn builds_book_from_orders() {
        let value = serde_json::json!({
            "code": 200,
            "bids": [{ "price": "75730.2", "remaining_base_amount": "1.2" }],
            "asks": [{ "price": "75732.4", "remaining_base_amount": "0.8" }]
        });
        let book = book_from_orders(&value, 1).unwrap();
        assert_eq!(book.bid, rust_decimal_macros::dec!(75730.2));
        assert_eq!(book.bid_notional_usd, rust_decimal_macros::dec!(90876.24));
        assert_eq!(book.ask_notional_usd, rust_decimal_macros::dec!(60585.92));
    }
}
