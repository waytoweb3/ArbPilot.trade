use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::{
    make_book, make_depth, parse_decimal, parse_json_decimal, BookSnapshot, Candle, DepthSnapshot,
    FundingRate, FundingSnapshot, FundingVenueClient,
};
use crate::http_client::HttpClient;

/// Gate.io 的交易所标识。
pub const VENUE: &str = "gate";

/// Gate 单次最多返回 1000 条历史费率。
const HISTORY_LIMIT: u32 = 1_000;

/// `BTC_USDT` → 内部统一的 `BTCUSDT`；非 USDT 本位合约返回 `None`。
fn internal_symbol(contract: &str) -> Option<String> {
    let upper = contract.trim().to_ascii_uppercase();
    let base = upper.strip_suffix("_USDT")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}USDT"))
}

/// 内部 `BTCUSDT` → Gate 的 `BTC_USDT`。
fn gate_contract(symbol: &str) -> anyhow::Result<String> {
    let base = symbol
        .trim()
        .to_ascii_uppercase()
        .strip_suffix("USDT")
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Gate 只支持 USDT 交易对: {symbol}"))?;
    if base.is_empty() {
        bail!("交易对名称不合法: {symbol}");
    }
    Ok(format!("{base}_USDT"))
}

#[derive(Debug, Deserialize)]
struct ContractRow {
    name: String,
    #[serde(default)]
    funding_rate: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    /// 下次结算时间（Unix 秒）。
    #[serde(default)]
    funding_next_apply: Option<i64>,
    /// 该合约的结算间隔（秒）。
    #[serde(default)]
    funding_interval: Option<i64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    in_delisting: Option<bool>,
    #[serde(default)]
    is_pre_market: Option<bool>,
    /// 一张合约对应的标的数量，用于把盘口张数折算成币。
    #[serde(default)]
    quanto_multiplier: Option<String>,
}

impl ContractRow {
    fn is_tradable(&self) -> bool {
        self.status
            .as_deref()
            .map(|status| status.eq_ignore_ascii_case("trading"))
            .unwrap_or(true)
            && !self.in_delisting.unwrap_or(false)
            && !self.is_pre_market.unwrap_or(false)
    }

    fn snapshot(&self) -> Option<FundingSnapshot> {
        let funding_rate = self
            .funding_rate
            .as_deref()
            .and_then(|value| parse_decimal(value).ok())?;
        Some(FundingSnapshot {
            funding_rate,
            mark_price: self
                .mark_price
                .as_deref()
                .and_then(|value| parse_decimal(value).ok()),
            next_funding_at: self
                .funding_next_apply
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single()),
            observed_at: Utc::now(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct HistoryRow {
    /// 结算时间（Unix 秒）。
    t: i64,
    /// 该期费率。
    r: String,
}

#[derive(Debug, Deserialize)]
struct OrderBookLevel {
    /// 价格（字符串）。
    p: Value,
    /// 数量（张），Gate 返回的是数字。
    s: Value,
}

#[derive(Debug, Deserialize)]
struct OrderBookRow {
    #[serde(default)]
    bids: Vec<OrderBookLevel>,
    #[serde(default)]
    asks: Vec<OrderBookLevel>,
}

/// Gate.io USDT 本位永续公开资金费率客户端。
pub struct GateFundingClient {
    base_url: String,
    http: Arc<HttpClient>,
}

impl GateFundingClient {
    pub fn new(base_url: &str, http: Arc<HttpClient>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    fn contracts(&self) -> anyhow::Result<Vec<ContractRow>> {
        let url = format!("{}/api/v4/futures/usdt/contracts", self.base_url);
        serde_json::from_value(self.http.get_json(&url)?).context("解析 Gate contracts 失败")
    }

    /// 一次请求返回全部 USDT 本位永续的当期费率、标记价与下次结算时间。
    pub fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        let mut snapshots = Vec::new();
        for row in self.contracts()? {
            let Some(symbol) = internal_symbol(&row.name) else {
                continue;
            };
            if !row.is_tradable() {
                continue;
            }
            if let Some(snapshot) = row.snapshot() {
                snapshots.push((symbol, snapshot));
            }
        }
        Ok(snapshots)
    }

    /// 各合约的官方结算间隔（秒），与合约清单同一次请求返回。
    pub fn funding_intervals(&self) -> anyhow::Result<HashMap<String, i64>> {
        let mut intervals = HashMap::new();
        for row in self.contracts()? {
            if !row.is_tradable() {
                continue;
            }
            let Some(symbol) = internal_symbol(&row.name) else {
                continue;
            };
            let Some(secs) = row.funding_interval.filter(|secs| (60..=86_400).contains(secs))
            else {
                continue;
            };
            intervals.insert(symbol, secs);
        }
        Ok(intervals)
    }

    pub fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        let contract = gate_contract(symbol)?;
        let url = format!("{}/api/v4/futures/usdt/contracts/{contract}", self.base_url);
        let row: ContractRow =
            serde_json::from_value(self.http.get_json(&url)?).context("解析 Gate contract 失败")?;
        row.snapshot()
            .ok_or_else(|| anyhow!("Gate contract 缺少合法的 funding_rate: {contract}"))
    }

    /// USDT 本位永续的买一/卖一档；盘口张数需乘以合约乘数折算成币。
    pub fn book(&self, symbol: &str) -> anyhow::Result<BookSnapshot> {
        let contract = gate_contract(symbol)?;
        let multiplier = self.quanto_multiplier(&contract)?;
        let url = format!(
            "{}/api/v4/futures/usdt/order_book?contract={contract}&limit=1",
            self.base_url
        );
        let row: OrderBookRow = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Gate order_book 失败")?;
        let level = |rows: &[OrderBookLevel], label: &str| -> anyhow::Result<(Decimal, Decimal)> {
            let level = rows
                .first()
                .ok_or_else(|| anyhow!("Gate order_book 缺少{label}: {contract}"))?;
            Ok((
                parse_json_decimal(&level.p)?,
                parse_json_decimal(&level.s)?,
            ))
        };
        let (bid, bid_size) = level(&row.bids, "买一")?;
        let (ask, ask_size) = level(&row.asks, "卖一")?;
        make_book(bid, bid_size * multiplier, ask, ask_size * multiplier)
    }

    /// USDT 本位永续的多档盘口；张数按合约乘数折算成币。
    pub fn depth(&self, symbol: &str, limit: u32) -> anyhow::Result<DepthSnapshot> {
        let contract = gate_contract(symbol)?;
        let multiplier = self.quanto_multiplier(&contract)?;
        let limit = limit.clamp(1, 100);
        let url = format!(
            "{}/api/v4/futures/usdt/order_book?contract={contract}&limit={limit}",
            self.base_url
        );
        let row: OrderBookRow = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Gate order_book 失败")?;
        let levels = |rows: Vec<OrderBookLevel>| -> anyhow::Result<Vec<(Decimal, Decimal)>> {
            rows.into_iter()
                .map(|level| Ok((parse_json_decimal(&level.p)?, parse_json_decimal(&level.s)?)))
                .collect()
        };
        make_depth(
            VENUE,
            symbol,
            levels(row.bids)?,
            levels(row.asks)?,
            multiplier,
        )
    }

    /// USDT 本位永续 K 线。
    ///
    /// Gate v4 的 K 线行是 `{t, o, h, l, c, v}` 对象（`t` 为 Unix 秒），
    /// 与其它交易所的数组行不同，这里单独解析。
    pub fn candles(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let contract = gate_contract(symbol)?;
        let interval = gate_interval(interval)?;
        let url = format!(
            "{}/api/v4/futures/usdt/candlesticks?contract={contract}&interval={interval}&limit={}",
            self.base_url,
            limit.clamp(1, 1_000)
        );
        let value = self.http.get_json(&url)?;
        gate_candles(&value)
    }

    /// 一张合约对应的标的数量，供 WebSocket 盘口把张数折算成币。
    pub fn symbol_multiplier(&self, symbol: &str) -> anyhow::Result<Decimal> {
        let contract = gate_contract(symbol)?;
        self.quanto_multiplier(&contract)
    }

    /// 合约乘数：一张合约对应的标的数量。
    fn quanto_multiplier(&self, contract: &str) -> anyhow::Result<Decimal> {
        let url = format!("{}/api/v4/futures/usdt/contracts/{contract}", self.base_url);
        let row: ContractRow = serde_json::from_value(self.http.get_json(&url)?)
            .with_context(|| format!("解析 Gate contract 失败: {contract}"))?;
        row.quanto_multiplier
            .as_deref()
            .ok_or_else(|| anyhow!("Gate contract 缺少 quanto_multiplier: {contract}"))
            .and_then(parse_decimal)
    }

    /// 当前正在交易的 USDT 本位永续合约清单。
    pub fn perpetual_symbols(&self) -> anyhow::Result<Vec<String>> {
        let mut symbols: Vec<String> = self
            .contracts()?
            .into_iter()
            .filter(ContractRow::is_tradable)
            .filter_map(|row| internal_symbol(&row.name))
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
        let contract = gate_contract(symbol)?;
        let mut url = format!(
            "{}/api/v4/futures/usdt/funding_rate?contract={contract}&limit={HISTORY_LIMIT}",
            self.base_url
        );
        if let Some(start) = start {
            url.push_str(&format!("&from={}", start.timestamp() + 1));
        }

        let rows: Vec<HistoryRow> = serde_json::from_value(self.http.get_json(&url)?)
            .context("解析 Gate funding_rate 失败")?;

        let mut rates = Vec::with_capacity(rows.len());
        for row in rows {
            let funding_time = Utc
                .timestamp_opt(row.t, 0)
                .single()
                .ok_or_else(|| anyhow!("Gate 结算时间越界: {}", row.t))?;
            rates.push(FundingRate {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                funding_time,
                funding_rate: parse_decimal(&row.r)?,
                mark_price: None,
            });
        }
        rates.sort_by_key(|row| row.funding_time);
        Ok(rates)
    }
}

/// Gate v4 的 K 线周期；小时/天/周用小写后缀。
fn gate_interval(interval: &str) -> anyhow::Result<&'static str> {
    Ok(match interval {
        "1m" => "1m",
        "5m" => "5m",
        "15m" => "15m",
        "30m" => "30m",
        "1h" => "1h",
        "4h" => "4h",
        "1d" => "1d",
        "1w" => "1w",
        other => bail!("Gate 不支持的 K 线周期: {other}"),
    })
}

#[derive(Debug, Deserialize)]
struct CandleRow {
    /// 开盘时间（Unix 秒）。
    t: i64,
    o: String,
    c: String,
    h: String,
    l: String,
    #[serde(default)]
    v: Option<Value>,
}

fn gate_candles(value: &Value) -> anyhow::Result<Vec<Candle>> {
    let rows: Vec<CandleRow> =
        serde_json::from_value(value.clone()).context("解析 Gate K 线失败")?;
    let mut candles = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(open_time) = Utc.timestamp_opt(row.t, 0).single() else {
            continue;
        };
        candles.push(Candle {
            open_time,
            open: parse_decimal(&row.o)?,
            high: parse_decimal(&row.h)?,
            low: parse_decimal(&row.l)?,
            close: parse_decimal(&row.c)?,
            volume: row
                .v
                .as_ref()
                .map(parse_json_decimal)
                .transpose()?
                .unwrap_or_default(),
        });
    }
    candles.sort_by_key(|candle| candle.open_time);
    Ok(candles)
}

impl FundingVenueClient for GateFundingClient {
    fn premium_index(&self, symbol: &str) -> anyhow::Result<FundingSnapshot> {
        Self::premium_index(self, symbol)
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

    fn premium_index_all(&self) -> anyhow::Result<Vec<(String, FundingSnapshot)>> {
        Self::premium_index_all(self)
    }

    fn funding_intervals(&self) -> anyhow::Result<HashMap<String, i64>> {
        Self::funding_intervals(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn maps_contract_names() {
        assert_eq!(internal_symbol("BTC_USDT").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("btc_usdt").as_deref(), Some("BTCUSDT"));
        assert_eq!(internal_symbol("BTC_USDC"), None);
        assert_eq!(gate_contract("btcusdt").unwrap(), "BTC_USDT");
        assert!(gate_contract("BTCUSDC").is_err());
    }

    #[test]
    fn parses_object_order_book_levels() {
        // Gate v4 的盘口档位是 `{"p": 价格, "s": 数量}` 对象，不是数组。
        let row: OrderBookRow = serde_json::from_value(serde_json::json!({
            "bids": [{ "p": "75722.5", "s": "12" }],
            "asks": [{ "p": "75723", "s": 8 }],
        }))
        .expect("应能解析对象形式的盘口");
        assert_eq!(row.bids[0].p, "75722.5");
        assert_eq!(row.asks[0].s, 8);
    }

    #[test]
    fn builds_snapshot_from_contract() {
        let row = ContractRow {
            name: "BTC_USDT".to_string(),
            funding_rate: Some("0.000042".to_string()),
            mark_price: Some("78351.95".to_string()),
            funding_next_apply: Some(1_793_808_000),
            funding_interval: Some(8 * 3600),
            status: Some("trading".to_string()),
            in_delisting: Some(false),
            is_pre_market: Some(false),
            quanto_multiplier: Some("0.0001".to_string()),
        };
        let snapshot = row.snapshot().expect("应能解析");
        assert_eq!(snapshot.funding_rate, dec!(0.000042));
        assert!(snapshot.next_funding_at.is_some());
        assert!(row.is_tradable());
    }
}
